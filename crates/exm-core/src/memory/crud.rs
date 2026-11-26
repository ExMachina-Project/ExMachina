//! memory 域分文件（自 memory.rs 按域拆出，纯搬移零行为变更）。

use super::render::{age_days, cap_chars, content_hash, decayed_importance};
use super::*;

impl MemoryStore {
    /// `data_dir` 为工作区数据根目录（与 Store 共用，但各自分集合）
    pub fn open(data_dir: impl AsRef<Path>) -> Result<Self> {
        let dir = data_dir.as_ref().to_path_buf();
        Ok(MemoryStore {
            db: FsDb::open(&dir)?,
            dir,
            write_lock: parking_lot::Mutex::new(()),
        })
    }

    pub fn root(&self) -> String {
        self.dir.display().to_string()
    }

    // ---------------- 写入 ----------------

    /// 条目标题/正文长度上限（字符）：记忆条目是检索单元而非文档仓库——
    /// 正文会原文进入 memory.md 渲染、召回注入与工具回灌，超长条目会撑爆
    /// 浅层视图与提示预算。超长内容应落工作区文件，条目正文只留要点与文件引用。
    pub(crate) const TITLE_CAP_CHARS: usize = 200;
    pub(crate) const BODY_CAP_CHARS: usize = 8000;

    /// 记住一条：按内容哈希去重（组维度隔离，同内容跨组各自成条）；重复命中则提升重要性/置信度并刷新时间
    pub fn remember(&self, draft: &MemoryDraft) -> Result<MemoryEntry> {
        // 写路径全程持锁：查哈希与写哈希/写条目必须原子，否则并发同内容去重失效（见 write_lock 注释）
        let _guard = self.write_lock.lock();
        // 长度钳制与空白规整先行：哈希必须对「实际落库的内容」计算——
        // 若先按原文算哈希、落库前才截断，同内容二次写入仍命中，但条目正文与哈希语义脱钩
        let title = cap_chars(draft.title.trim(), Self::TITLE_CAP_CHARS);
        let body = cap_chars(draft.body.trim(), Self::BODY_CAP_CHARS);
        let hash = content_hash(&title, &body, draft.group_id.as_deref());
        let now = now_iso();

        if let Some(existing_id) = self.hash_lookup(&hash)? {
            if let Some(mut entry) = self.get_entry(&existing_id)? {
                entry.importance = (entry.importance + 0.08).min(1.0);
                entry.confidence = entry.confidence.max(draft.confidence.clamp(0.0, 1.0));
                entry.updated_at = now;
                entry.access_count += 1;
                self.save_entry(&entry)?;
                return Ok(entry);
            }
        }

        let entry = MemoryEntry {
            id: new_id(),
            kind: draft.kind,
            scope: draft.scope.clone(),
            session_id: draft.session_id.clone(),
            agent_id: draft.agent_id.clone(),
            group_id: draft.group_id.clone(),
            title,
            body,
            tags: draft.tags.clone(),
            importance: draft.importance.clamp(0.0, 1.0),
            confidence: draft.confidence.clamp(0.0, 1.0),
            pinned: false,
            source_ref: draft.source_ref.clone(),
            created_at: now.clone(),
            updated_at: now,
            last_accessed_at: None,
            access_count: 0,
            version: 1,
            embedding: None,
            model_embeddings: Default::default(),
        };
        self.save_entry(&entry)?;
        self.db
            .put(HASH_INDEX, &hash, &serde_json::json!(entry.id))?;
        self.index_entry(&entry)?;
        // 版本链起点（capability-parity 6.5）：v1 = created。历史写失败不算写入失败——
        // 条目已落盘且可检索，缺一行历史只影响审计完整度，不应让「记住」整体失败。
        let _ = self.append_history(&entry, "created");
        Ok(entry)
    }

    pub(crate) fn save_entry(&self, entry: &MemoryEntry) -> Result<()> {
        self.db.put(ENTRIES, &entry.id, entry)
    }

    // ---------------- 版本链（capability-parity 6.5，spec: agent-learning-loop「受控写入和版本回滚」） ----------------

    /// 追加一行版本历史（`memory/history/{id}.jsonl`）。调用方持有写锁。
    fn append_history(&self, entry: &MemoryEntry, change: &str) -> Result<()> {
        self.db.append_line(
            HISTORY,
            &entry.id,
            &MemoryHistoryRow {
                version: entry.version_label(),
                title: entry.title.clone(),
                body: entry.body.clone(),
                tags: entry.tags.clone(),
                kind: entry.kind,
                changed_at: entry.updated_at.clone(),
                change: change.to_string(),
            },
        )
    }

    /// 版本历史（旧→新）。条目已被遗忘时历史仍在——遗忘是可审计、可撤销的状态，不是抹除。
    pub fn history(&self, id: &str) -> Result<Vec<MemoryHistoryRow>> {
        self.db.read_lines(HISTORY, id, 0)
    }

    /// 编辑内容的校验（格式/越界）：通过才允许进入版本化写入。返回具体拒绝原因。
    fn validate_content(title: &str, body: &str) -> Result<()> {
        anyhow::ensure!(!title.trim().is_empty(), "title 不能为空");
        anyhow::ensure!(!body.trim().is_empty(), "body 不能为空");
        Ok(())
    }

    /// 版本化内容更新：校验 → 归档当前版本 → 写新版本（version+1）。
    /// 任何校验失败都发生在落盘之前，旧版本保持不变、不留半成品。
    /// `reason` 进历史行（`updated` = 用户/工具编辑；`rolled_back` / `restored` 由回滚与
    /// 撤销遗忘传入），`changed_at` 取当前时间。
    pub(crate) fn update_versioned(
        &self,
        id: &str,
        title: Option<&str>,
        body: Option<&str>,
        tags: Option<Vec<String>>,
        kind: Option<&MemoryKind>,
        reason: &str,
    ) -> Result<Option<MemoryEntry>> {
        let _guard = self.write_lock.lock();
        let Some(mut e) = self.get_entry(id)? else {
            return Ok(None);
        };
        // ---- 1. 校验：全部在改动任何持久状态之前完成 ----
        let new_title = match title {
            Some(t) => cap_chars(t.trim(), Self::TITLE_CAP_CHARS),
            None => e.title.clone(),
        };
        let new_body = match body {
            Some(b) => cap_chars(b.trim(), Self::BODY_CAP_CHARS),
            None => e.body.clone(),
        };
        Self::validate_content(&new_title, &new_body)?;
        let new_kind = match kind {
            Some(k) => *k,
            None => e.kind,
        };
        let new_tags = tags.unwrap_or_else(|| e.tags.clone());
        let old_hash = content_hash(&e.title, &e.body, e.group_id.as_deref());
        let new_hash = content_hash(&new_title, &new_body, e.group_id.as_deref());
        if new_hash != old_hash {
            // 与其它条目撞内容哈希时拒绝：覆盖会让对方失去去重索引，remember 语义随之漂移
            if let Some(other) = self.hash_lookup(&new_hash)? {
                if other != id {
                    anyhow::bail!("已存在相同内容的记忆条目：{other}");
                }
            }
        }
        // ---- 2. 内容与元数据都没变：不升版、不写历史（没有变更就没有变更记录）----
        let content_changed = new_hash != old_hash;
        if !content_changed && new_tags == e.tags && new_kind == e.kind {
            return Ok(Some(e));
        }
        // ---- 3. 写新版本（version+1）并记一行历史 ----
        // 不变量：历史里每个版本号恰好一行，行内存该版本的内容；`change` 说明这一版怎么来的。
        // 旧版本的内容已在它自己那一行里，这里不重复归档。
        let old_version = e.version_label();
        // 先清旧内容的倒排项——unindex_entry 按条目「当前」title/body/tags 取词项，
        // 必须在字段被覆盖之前调用。先覆盖再 unindex 删的是新词项（此刻还不存在），
        // 旧词项会永久留在索引里：编辑过的记忆仍会被旧内容的关键词召回（过期内容回流）。
        self.unindex_entry(&e)?;
        e.title = new_title;
        e.body = new_body;
        e.tags = new_tags;
        e.kind = new_kind;
        e.version = old_version + 1;
        e.updated_at = now_iso();
        if content_changed {
            // 哈希迁移：内容变了去重键跟着变
            e.embedding = None;
            e.model_embeddings.clear();
            self.db.delete(HASH_INDEX, &old_hash)?;
            self.db
                .put(HASH_INDEX, &new_hash, &serde_json::json!(e.id))?;
        }
        self.index_entry(&e)?;
        self.save_entry(&e)?;
        let _ = self.append_history(&e, reason);
        Ok(Some(e))
    }

    pub(crate) fn get_entry(&self, id: &str) -> Result<Option<MemoryEntry>> {
        self.get_entry_public(id)
    }

    /// 读取条目（嵌入回填等跨模块路径用）
    pub fn get_entry_public(&self, id: &str) -> Result<Option<MemoryEntry>> {
        self.db.get(ENTRIES, id)
    }

    pub(crate) fn hash_lookup(&self, hash: &str) -> Result<Option<String>> {
        let v: Option<serde_json::Value> = self.db.get(HASH_INDEX, hash)?;
        Ok(v.and_then(|x| x.as_str().map(|s| s.to_string())))
    }

    pub(crate) fn index_entry(&self, entry: &MemoryEntry) -> Result<()> {
        let mut terms: std::collections::HashMap<String, f64> = Default::default();
        let text = format!("{} {} {}", entry.title, entry.body, entry.tags.join(" "));
        for t in tokens(&text) {
            let w = if entry.title.to_lowercase().contains(&t) {
                3.0
            } else {
                1.0
            };
            *terms.entry(t).or_insert(0.0) += w;
        }
        for (term, weight) in terms {
            let id = entry.id.clone();
            self.db
                .with_bucket_map::<Vec<(String, f64)>, _>(TERMS, &term, |term, map| {
                    let list = map.entry(term.to_string()).or_default();
                    if let Some(slot) = list.iter_mut().find(|(eid, _)| *eid == id) {
                        slot.1 = weight;
                    } else {
                        list.push((id.clone(), weight));
                    }
                })?;
        }
        Ok(())
    }

    pub(crate) fn unindex_entry(&self, entry: &MemoryEntry) -> Result<()> {
        let text = format!("{} {} {}", entry.title, entry.body, entry.tags.join(" "));
        let mut seen = std::collections::HashSet::new();
        for t in tokens(&text) {
            if !seen.insert(t.clone()) {
                continue;
            }
            let id = entry.id.clone();
            self.db
                .with_bucket_map::<Vec<(String, f64)>, _>(TERMS, &t, |term, map| {
                    if let Some(list) = map.get_mut(term) {
                        list.retain(|(eid, _)| *eid != id);
                    }
                })?;
        }
        Ok(())
    }

    pub fn pin(&self, id: &str, pinned: bool) -> Result<()> {
        // 读-改-写条目，与 remember 的命中提升同口径串行，避免字段更新互相覆盖
        let _guard = self.write_lock.lock();
        if let Some(mut e) = self.get_entry(id)? {
            e.pinned = pinned;
            e.updated_at = now_iso();
            self.save_entry(&e)?;
        }
        Ok(())
    }

    /// 条目编辑（assistant-parity T3）：title/body/tags/kind 可改。
    /// capability-parity 6.5 起走版本化写入（见 [`Self::update_versioned`]）：校验通过才
    /// 归档旧版本并升版；校验失败旧版本不变。
    pub fn update(
        &self,
        id: &str,
        title: Option<&str>,
        body: Option<&str>,
        tags: Option<Vec<String>>,
        kind: Option<&str>,
    ) -> Result<Option<MemoryEntry>> {
        let kind = match kind {
            Some(k) => Some(
                crate::memory::MemoryKind::parse(k)
                    .ok_or_else(|| anyhow::anyhow!("未知记忆类型：{k}"))?,
            ),
            None => None,
        };
        self.update_versioned(id, title, body, tags, kind.as_ref(), "updated")
    }

    /// 遗忘一条记忆——留痕可撤销（spec「删除状态在后续会话生效并可审计」）：
    /// 被遗忘时的内容先记入历史（版本号继续 +1，`change=forgotten`），再移除条目、
    /// 倒排与哈希索引。遗忘后条目立即从召回、列表与 memory.md 消失；历史保留，
    /// [`Self::restore`] 可撤销，[`Self::rollback`] 可恢复任一历史版本。
    pub fn forget(&self, id: &str) -> Result<()> {
        // 与 remember 互斥：防止「forget 删哈希 → remember 按同哈希新建 → forget 删旧条目」
        // 交错后留下孤儿哈希/悬空索引
        let _guard = self.write_lock.lock();
        if let Some(mut e) = self.get_entry(id)? {
            self.unindex_entry(&e)?;
            let hash = content_hash(&e.title, &e.body, e.group_id.as_deref());
            self.db.delete(HASH_INDEX, &hash)?;
            e.version = e.version_label() + 1;
            e.updated_at = now_iso();
            let _ = self.append_history(&e, "forgotten");
            // 墓碑：保留条目的归属与统计字段（scope/session/agent/group/source_ref/
            // created_at/importance/confidence/pinned）——历史行只存内容，撤销遗忘时
            // 要靠它把条目恢复成原来的归属，否则恢复出来的记忆会「换了主人」。
            // 墓碑不参与任何读取路径（召回/列表/渲染/统计只读 ENTRIES）。
            let _ = self.db.put(TOMBSTONES, id, &e);
        }
        self.db.delete(ENTRIES, id)
    }

    /// 墓碑是否存在（条目已遗忘且可撤销）。条目仍存在时为 false。
    pub fn is_forgotten(&self, id: &str) -> Result<bool> {
        Ok(self.get_entry(id)?.is_none() && self.db.exists(TOMBSTONES, id)?)
    }

    /// 已遗忘（可撤销）的条目清单：遗忘时间倒序（`updated_at` 为秒级精度，同秒内按 id）。
    /// 墓碑保留条目的归属与统计字段，所以这里读出来就是遗忘当时的完整条目——
    /// 治理面据此提供「撤销遗忘」入口（否则条目从列表消失后用户无处可点）。
    /// 墓碑与条目互斥（forget 删条目、restore 删墓碑），这里再做一次防御性过滤。
    pub fn list_forgotten(&self) -> Result<Vec<MemoryEntry>> {
        let mut rows: Vec<MemoryEntry> = self
            .db
            .list::<MemoryEntry>(TOMBSTONES)?
            .into_iter()
            .filter(|e| self.get_entry(&e.id).ok().flatten().is_none())
            .collect();
        // updated_at 是秒级 ISO 串，同秒内遗忘的多条靠它分不出先后——用 id 作平局裁决，
        // 保证清单顺序确定（同一数据每次读出来一样），不会随底层 map 迭代序漂移。
        rows.sort_by(|a, b| {
            b.updated_at
                .cmp(&a.updated_at)
                .then_with(|| a.id.cmp(&b.id))
        });
        Ok(rows)
    }

    /// 按历史行重建条目：保留 `base` 的全部归属与统计字段（scope/session/agent/group/
    /// source_ref/created_at/importance/confidence/pinned/access_count），内容取历史行，
    /// 版本号 = 历史最大版本 + 1（单调递增，不倒退）。调用方持有写锁。
    /// `base` = 当前条目（回滚）或墓碑（撤销遗忘）。
    fn materialize(
        &self,
        base: &MemoryEntry,
        row: &MemoryHistoryRow,
        change: &str,
    ) -> Result<MemoryEntry> {
        Self::validate_content(&row.title, &row.body)?;
        let max_version = self
            .history(&base.id)?
            .iter()
            .map(|r| r.version)
            .max()
            .unwrap_or(0);
        let mut e = base.clone();
        e.title = row.title.clone();
        e.body = row.body.clone();
        e.tags = row.tags.clone();
        e.kind = row.kind;
        e.version = max_version + 1;
        e.updated_at = now_iso();
        e.embedding = None;
        e.model_embeddings.clear();
        let new_hash = content_hash(&e.title, &e.body, e.group_id.as_deref());
        if let Some(other) = self.hash_lookup(&new_hash)? {
            if other != e.id {
                anyhow::bail!("已存在相同内容的记忆条目：{other}");
            }
        }
        // 旧内容的哈希与倒排项必须清掉（条目当前不存在时哈希已在遗忘时删除，删除不存在的键是 no-op）
        self.unindex_entry(base)?;
        self.db.delete(
            HASH_INDEX,
            &content_hash(&base.title, &base.body, base.group_id.as_deref()),
        )?;
        self.db
            .put(HASH_INDEX, &new_hash, &serde_json::json!(e.id))?;
        self.index_entry(&e)?;
        self.save_entry(&e)?;
        let _ = self.append_history(&e, change);
        Ok(e)
    }

    /// 回滚到历史版本：该版本的内容经同一校验后成为新版本行（version 继续
    /// 单调递增——回滚不是倒退版本号，是「以旧内容写新版本」，审计链不断）。
    pub fn rollback(&self, id: &str, to_version: u64) -> Result<MemoryEntry> {
        let _guard = self.write_lock.lock();
        let base = self.get_entry(id)?.with_context(|| {
            format!("记忆条目不存在: {id}（已遗忘的条目请用 restore 撤销遗忘）")
        })?;
        let history = self.history(id)?;
        let row = history
            .iter()
            .find(|r| r.is_content_version() && r.version == to_version)
            .with_context(|| format!("历史版本不存在：{id} v{to_version}"))?
            .clone();
        self.materialize(&base, &row, "rolled_back")
    }

    /// 撤销遗忘：条目已不存在且历史末行为遗忘标记时，以被遗忘时的内容重建条目
    /// （版本号接着走，`change=restored`）。条目仍存在或历史里没有遗忘标记时拒绝。
    pub fn restore(&self, id: &str) -> Result<MemoryEntry> {
        let _guard = self.write_lock.lock();
        anyhow::ensure!(
            self.get_entry(id)?.is_none(),
            "记忆条目仍存在，无需撤销遗忘：{id}"
        );
        let history = self.history(id)?;
        let last = history
            .last()
            .with_context(|| format!("记忆条目不存在: {id}"))?;
        anyhow::ensure!(
            last.change == "forgotten",
            "记忆条目 {id} 未被遗忘，无可撤销的遗忘"
        );
        let tomb = self
            .db
            .get::<MemoryEntry>(TOMBSTONES, id)?
            .with_context(|| format!("记忆条目不存在: {id}（无遗忘记录）"))?;
        anyhow::ensure!(
            last.change == "forgotten",
            "记忆条目 {id} 未被遗忘，无可撤销的遗忘"
        );
        // 被遗忘时的内容 = 墓碑条目自身（它的内容就是遗忘那一刻的内容）
        let row = MemoryHistoryRow {
            version: tomb.version_label(),
            title: tomb.title.clone(),
            body: tomb.body.clone(),
            tags: tomb.tags.clone(),
            kind: tomb.kind,
            changed_at: tomb.updated_at.clone(),
            change: "forgotten".into(),
        };
        let e = self.materialize(&tomb, &row, "restored")?;
        self.db.delete(TOMBSTONES, id)?;
        Ok(e)
    }

    pub fn link(&self, from: &str, to: &str, relation: &str) -> Result<()> {
        self.db.append_line(
            LINKS,
            from,
            &serde_json::json!({ "from": from, "to": to, "relation": relation, "createdAt": now_iso() }),
        )
    }

    /// 条目的关联链（作为起点的出链 + 作为终点的入链）；关联持久化于 links 集合
    pub fn links_of(&self, id: &str) -> Result<Vec<serde_json::Value>> {
        let mut out: Vec<serde_json::Value> = self.db.read_lines(LINKS, id, 0)?;
        // 入链：to = id 的关联存于对端的分桶；全量扫描代价可控（关联数据量小）
        let entries: Vec<MemoryEntry> = self.db.list(ENTRIES)?;
        for e in entries {
            if e.id == id {
                continue;
            }
            for l in self.db.read_lines::<serde_json::Value>(LINKS, &e.id, 0)? {
                if l.get("to").and_then(|v| v.as_str()) == Some(id) {
                    out.push(l);
                }
            }
        }
        Ok(out)
    }

    /// 全量重建倒排索引（索引可随时重建，docs/08 §6）
    pub fn reindex(&self) -> Result<usize> {
        self.db.clear_buckets(TERMS)?;
        let entries: Vec<MemoryEntry> = self.db.list(ENTRIES)?;
        for e in &entries {
            self.index_entry(e)?;
        }
        Ok(entries.len())
    }

    /// 全量条目读取（组改 id 的引用清扫用，group-id-migration）
    pub fn entries_public(&self) -> Result<Vec<MemoryEntry>> {
        self.db.list(ENTRIES)
    }

    /// 记忆条目组归属改写（组改 id 的引用清扫）：只改 group_id 并重建倒排索引，
    /// 不动内容/时间戳/统计。写路径持锁（与 remember/decay 同口径）。
    pub fn retag_group(&self, id: &str, new_group: &str) -> Result<()> {
        let _guard = self.write_lock.lock();
        let mut e = self
            .get_entry_public(id)?
            .with_context(|| format!("记忆条目不存在: {id}"))?;
        e.group_id = Some(new_group.to_string());
        self.save_entry(&e)?;
        self.index_entry(&e)?;
        Ok(())
    }

    /// 时间衰减（固定项豁免），下限 floor。
    /// 衰减数学收敛在 [`decay_factor`]/[`decayed_importance`] 两个纯函数里，边界行为有单测锁定。
    pub fn decay(&self, half_life_days: f64, floor: f64) -> Result<usize> {
        // 整轮持写锁：衰减是「读全部条目 → 逐条改写」的维护操作，
        // 与 remember 的命中提升并发时会互相覆盖 importance
        let _guard = self.write_lock.lock();
        let entries: Vec<MemoryEntry> = self.db.list(ENTRIES)?;
        let mut changed = 0;
        for mut e in entries {
            if e.pinned {
                continue;
            }
            let next =
                decayed_importance(e.importance, age_days(&e.updated_at), half_life_days, floor);
            if (next - e.importance).abs() > 1e-6 {
                e.importance = next;
                self.save_entry(&e)?;
                changed += 1;
            }
        }
        Ok(changed)
    }

    // ---------------- 检索 ----------------

    /// 回填条目语义向量（写入路径异步补嵌）
    pub fn set_embedding(&self, id: &str, embedding: Vec<f32>) -> Result<()> {
        let Some(mut e) = self.db.get::<MemoryEntry>(ENTRIES, id)? else {
            return Ok(());
        };
        if e.embedding.is_none() {
            e.embedding = Some(embedding);
            let _ = self.save_entry(&e);
        }
        Ok(())
    }

    pub fn set_model_embedding(&self, id: &str, model: &str, vector: Vec<f32>) -> Result<()> {
        let _guard = self.write_lock.lock();
        if let Some(mut entry) = self.db.get::<MemoryEntry>(ENTRIES, id)? {
            entry.model_embeddings.insert(model.to_string(), vector);
            self.save_entry(&entry)?;
        }
        Ok(())
    }

    /// 余弦相似度（任一为空返回 None）
    pub(crate) fn cosine(a: &[f32], b: &[f32]) -> Option<f64> {
        if a.is_empty() || b.is_empty() || a.len() != b.len() {
            return None;
        }
        let n = a.len().min(b.len());
        let (mut dot, mut na, mut nb) = (0f64, 0f64, 0f64);
        for i in 0..n {
            dot += a[i] as f64 * b[i] as f64;
            na += (a[i] as f64).powi(2);
            nb += (b[i] as f64).powi(2);
        }
        if na == 0.0 || nb == 0.0 {
            return None;
        }
        Some(dot / (na.sqrt() * nb.sqrt()))
    }
}
