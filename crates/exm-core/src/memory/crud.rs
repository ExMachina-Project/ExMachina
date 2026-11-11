//! memory 域分文件（自 memory.rs 按域拆出，纯搬移零行为变更）。

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
            embedding: None,
        };
        self.save_entry(&entry)?;
        self.db
            .put(HASH_INDEX, &hash, &serde_json::json!(entry.id))?;
        self.index_entry(&entry)?;
        Ok(entry)
    }

    pub(crate) fn save_entry(&self, entry: &MemoryEntry) -> Result<()> {
        self.db.put(ENTRIES, &entry.id, entry)
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

    pub fn forget(&self, id: &str) -> Result<()> {
        // 与 remember 互斥：防止「forget 删哈希 → remember 按同哈希新建 → forget 删旧条目」
        // 交错后留下孤儿哈希/悬空索引
        let _guard = self.write_lock.lock();
        if let Some(e) = self.get_entry(id)? {
            self.unindex_entry(&e)?;
            let hash = content_hash(&e.title, &e.body, e.group_id.as_deref());
            self.db.delete(HASH_INDEX, &hash)?;
        }
        self.db.delete(ENTRIES, id)
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

    /// 余弦相似度（任一为空返回 None）
    pub(crate) fn cosine(a: &[f32], b: &[f32]) -> Option<f64> {
        if a.is_empty() || b.is_empty() {
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
