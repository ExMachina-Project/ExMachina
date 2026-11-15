//! 知识库 —— 跨智能体共享的通用知识层（agent-mainstream-parity-3）
//!
//! 与记忆（memory/）语义分离：记忆是个体/会话私有的经历沉淀（分区隔离），
//! 知识是实例内通用的客观结论（部署约束、平台特性、方法论、踩坑经验），
//! 全部智能体共享同一集合，不按任何维度分区。
//!
//! 存储（与 MemoryStore 同构，纯 Rust 文档存储零 C 依赖）：
//!   `knowledges/{id}.json`          知识条目
//!   `knowledge_hash/{hash}.json`    正文哈希 → 条目 id（去重键）
//!   `knowledge_terms/bucket-XX.json` 倒排索引：term → [(entry_id, weight)]
//!
//! 设计参照 memory/mod.rs；不继承 MemoryStore（记忆的分区/衰减/置顶语义
//! 不适用），只复用 FsDb 与 tokens 分词器。

use crate::fsdb::FsDb;
use crate::types::new_id;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const ENTRIES: &str = "knowledges";
const HASH_INDEX: &str = "knowledge_hash";
const TERMS: &str = "knowledge_terms";

// ---------------------------------------------------------------- 类型

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgeEntry {
    pub id: String,
    pub title: String,
    pub body: String,
    #[serde(default)]
    pub tags: Vec<String>,
    /// 产出来源：智能体标识（manual = 管理面手写）
    pub source_agent: String,
    /// 来源会话引用（可空；管理面写入无会话）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_session: Option<String>,
    /// 检索命中计数（热度信号：重复写入去重提升 +1，检索命中 +1）
    pub hits: u64,
    pub created_at: String,
    pub updated_at: String,
}

/// knowledge_write 的入参草稿
#[derive(Debug, Clone)]
pub struct KnowledgeDraft {
    pub title: String,
    pub body: String,
    pub tags: Vec<String>,
    pub source_agent: String,
    pub source_session: Option<String>,
}

/// 写入结果：新建 / 去重提升（工具回显与 REST 复用）
#[derive(Debug)]
pub enum KnowledgeWriteOutcome {
    Created(KnowledgeEntry),
    /// 命中既有条目（内容哈希相同）：提升 hits 并刷新 updated_at
    Deduped(KnowledgeEntry),
}

impl KnowledgeWriteOutcome {
    pub fn entry(&self) -> &KnowledgeEntry {
        match self {
            KnowledgeWriteOutcome::Created(e) | KnowledgeWriteOutcome::Deduped(e) => e,
        }
    }
    pub fn deduped(&self) -> bool {
        matches!(self, KnowledgeWriteOutcome::Deduped(_))
    }
}

// ---------------------------------------------------------------- Store

pub struct KnowledgeStore {
    db: FsDb,
    dir: PathBuf,
    /// 写入串行锁：与 MemoryStore.write_lock 同理——「查哈希 → 写哈希/条目」
    /// 是跨多文件的读-改-写，并发同内容会双双未命中而各建一条（去重契约破坏）
    write_lock: parking_lot::Mutex<()>,
}

/// 正文长度上限（字符）：知识条目是检索单元而非文档仓库，超长应拆分为
/// 多条更聚焦的知识（与 MemoryStore::BODY_CAP_CHARS 同数量级）
pub const KNOWLEDGE_BODY_CAP_CHARS: usize = 8000;
pub const KNOWLEDGE_TITLE_CAP_CHARS: usize = 200;

fn cap_chars(s: &str, cap: usize) -> String {
    s.chars().take(cap).collect()
}

/// 正文哈希（与 memory::content_hash 同构但无 group 维度——知识全局共享）
fn body_hash(body: &str) -> String {
    let mut h: u64 = 1469598103934665603;
    for b in body.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(1099511628211);
    }
    format!("{h:016x}")
}

impl KnowledgeStore {
    pub fn open(data_dir: impl AsRef<Path>) -> Result<Self> {
        let dir = data_dir.as_ref().to_path_buf();
        Ok(KnowledgeStore {
            db: FsDb::open(&dir)?,
            dir,
            write_lock: parking_lot::Mutex::new(()),
        })
    }

    pub fn root(&self) -> String {
        self.dir.display().to_string()
    }

    // ---------------- 写入 ----------------

    /// 写入一条知识：正文哈希去重——命中既有条目则提升 hits 刷新时间（Deduped），
    /// 未命中则新建（Created）。标题/正文先钳制再哈希（对实际落库内容算哈希）。
    pub fn write(&self, draft: &KnowledgeDraft) -> Result<KnowledgeWriteOutcome> {
        let _guard = self.write_lock.lock();
        let title = cap_chars(draft.title.trim(), KNOWLEDGE_TITLE_CAP_CHARS);
        let body = cap_chars(draft.body.trim(), KNOWLEDGE_BODY_CAP_CHARS);
        let hash = body_hash(&body);
        let now = now_iso();

        if let Some(existing_id) = self.hash_lookup(&hash)? {
            if let Some(mut entry) = self.get(&existing_id)? {
                entry.hits += 1;
                entry.updated_at = now;
                // 标签并集补充（重复写入常带补充视角的标签）
                for t in &draft.tags {
                    if !entry.tags.contains(t) {
                        entry.tags.push(t.clone());
                    }
                }
                self.save(&entry)?;
                return Ok(KnowledgeWriteOutcome::Deduped(entry));
            }
        }

        let entry = KnowledgeEntry {
            id: new_id(),
            title,
            body,
            tags: draft.tags.clone(),
            source_agent: draft.source_agent.clone(),
            source_session: draft.source_session.clone(),
            hits: 0,
            created_at: now.clone(),
            updated_at: now,
        };
        self.save(&entry)?;
        self.db.put(HASH_INDEX, &hash, &serde_json::json!(entry.id))?;
        self.index_entry(&entry)?;
        Ok(KnowledgeWriteOutcome::Created(entry))
    }

    pub(crate) fn save(&self, entry: &KnowledgeEntry) -> Result<()> {
        self.db.put(ENTRIES, &entry.id, entry)
    }

    pub fn get(&self, id: &str) -> Result<Option<KnowledgeEntry>> {
        self.db.get(ENTRIES, id)
    }

    pub(crate) fn hash_lookup(&self, hash: &str) -> Result<Option<String>> {
        let v: Option<serde_json::Value> = self.db.get(HASH_INDEX, hash)?;
        Ok(v.and_then(|x| x.as_str().map(|s| s.to_string())))
    }

    /// 删除条目（管理面）：条目 + 哈希索引 + 倒排残留（倒排按惰性清理：
    /// 检索时命中不存在条目即跳过，与 MemoryStore.forget 同口径）
    pub fn delete(&self, id: &str) -> Result<bool> {
        let _guard = self.write_lock.lock();
        let Some(entry) = self.get(id)? else {
            return Ok(false);
        };
        self.db.delete(ENTRIES, id)?;
        let hash = body_hash(&entry.body);
        if self.hash_lookup(&hash)?.as_deref() == Some(id) {
            self.db.delete(HASH_INDEX, &hash)?;
        }
        Ok(true)
    }

    // ---------------- 检索 ----------------

    /// 词项倒排检索：命中任一查询词项即候选，按（命中权重和 + hits 热度）合排。
    /// 命中条目累加 hits（热度信号积累）；空查询回落最近条目列表。
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<KnowledgeEntry>> {
        let q_tokens = tokens(query);
        let mut out: Vec<KnowledgeEntry> = Vec::new();
        if q_tokens.is_empty() {
            out = self.recent(limit);
            // 空查询回落不算命中，不计 hits
            return Ok(out);
        }
        let mut score: std::collections::HashMap<String, f64> = Default::default();
        for t in &q_tokens {
            let bucket: std::collections::HashMap<String, Vec<(String, f64)>> =
                self.db.read_bucket_map(TERMS, t)?;
            if let Some(list) = bucket.get(t) {
                for (id, w) in list {
                    *score.entry(id.clone()).or_insert(0.0) += *w;
                }
            }
        }
        if score.is_empty() {
            return Ok(out);
        }
        let mut ranked: Vec<(String, f64)> = score.into_iter().collect();
        ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        let mut hits_touched: Vec<(f64, KnowledgeEntry)> = Vec::new();
        for (id, w) in ranked.iter().take(limit * 3) {
            if let Some(mut e) = self.get(id)? {
                e.hits += 1; // 本次命中计入（先记后收，热度随检索积累）
                self.save(&e)?;
                hits_touched.push((*w, e));
            }
        }
        // 已按权重序取回，直接截断
        out = hits_touched
            .into_iter()
            .take(limit)
            .map(|(_, e)| e)
            .collect();
        Ok(out)
    }

    /// 最近条目（createdAt 倒序）
    pub fn recent(&self, limit: usize) -> Vec<KnowledgeEntry> {
        let mut all: Vec<KnowledgeEntry> = self.db.list::<KnowledgeEntry>(ENTRIES).unwrap_or_default();
        all.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        all.truncate(limit);
        all
    }

    /// 全量条目（管理面导出/清单用）
    pub fn list_all(&self) -> Vec<KnowledgeEntry> {
        self.db.list::<KnowledgeEntry>(ENTRIES).unwrap_or_default()
    }

    // ---------------- 索引 ----------------

    pub(crate) fn index_entry(&self, entry: &KnowledgeEntry) -> Result<()> {
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
}

fn now_iso() -> String {
    crate::types::now_iso()
}

// ---------------------------------------------------------------- 提示词注入

/// 系统提示尾部注入 top-k 知识（knowledge.injectTopK）：
/// - top_k = 0：不注入（零行为变化，默认）
/// - 检索失败静默跳过（不得阻塞会话——注入是增益不是依赖）
/// - 每条截断 inject_max_chars 并标注「知识库参考」与来源
pub fn inject_prompt_section(
    store: &KnowledgeStore,
    query: &str,
    top_k: usize,
    max_chars: usize,
) -> String {
    if top_k == 0 || query.trim().is_empty() {
        return String::new();
    }
    let hits = match store.search(query, top_k) {
        Ok(h) => h,
        Err(_) => return String::new(), // 静默跳过：注入失败不阻塞会话
    };
    if hits.is_empty() {
        return String::new();
    }
    let mut out = String::from("\n\n## 知识库参考（按当前任务检索的共享知识）\n");
    for e in hits {
        let body: String = e.body.chars().take(max_chars).collect();
        let ellipsis = if e.body.chars().count() > max_chars {
            "…（截断）"
        } else {
            ""
        };
        out.push_str(&format!(
            "- [{}] {}（来源: {}）\n  {}{ellipsis}\n",
            e.id, e.title, e.source_agent, body
        ));
    }
    out
}

// ---------------------------------------------------------------- 分词

/// 词项分词：ASCII 词（≥2 字符）+ CJK 双字 bigram。与 memory::render::tokens
/// 同算法——两库检索语义一致；不直接复用因其为 memory 模块私有（刻意不公开，
/// 保持两库解耦，算法漂移时可各自演进）。
pub(crate) fn tokens(text: &str) -> Vec<String> {
    let lower = text.to_lowercase();
    let mut out: Vec<String> = Vec::new();
    let mut ascii = String::new();
    let mut cjk: Vec<char> = Vec::new();

    fn flush_ascii(buf: &mut String, out: &mut Vec<String>) {
        if buf.chars().count() >= 2 {
            out.push(buf.clone());
        }
        buf.clear();
    }
    fn flush_cjk(buf: &mut Vec<char>, out: &mut Vec<String>) {
        if buf.len() == 1 {
            out.push(buf[0].to_string());
        } else {
            for w in buf.windows(2) {
                out.push(format!("{}{}", w[0], w[1]));
            }
        }
        buf.clear();
    }

    for ch in lower.chars() {
        let is_cjk = ('\u{4e00}'..='\u{9fff}').contains(&ch);
        if is_cjk {
            flush_ascii(&mut ascii, &mut out);
            cjk.push(ch);
        } else if ch.is_alphanumeric() || ch == '_' {
            flush_cjk(&mut cjk, &mut out);
            ascii.push(ch);
        } else {
            flush_ascii(&mut ascii, &mut out);
            flush_cjk(&mut cjk, &mut out);
        }
    }
    flush_ascii(&mut ascii, &mut out);
    flush_cjk(&mut cjk, &mut out);
    out.truncate(200);
    out
}

// ---------------------------------------------------------------- 测试

#[cfg(test)]
mod tests {
    use super::*;

    fn test_store() -> KnowledgeStore {
        let dir = std::env::temp_dir().join(format!("exm-knowledge-{}", new_id()));
        KnowledgeStore::open(&dir).expect("打开测试知识库失败")
    }

    fn draft(title: &str, body: &str) -> KnowledgeDraft {
        KnowledgeDraft {
            title: title.into(),
            body: body.into(),
            tags: vec![],
            source_agent: "test-agent".into(),
            source_session: None,
        }
    }

    /// 全局共享语义（回归锚定）：不同 source_agent 写入/检索同一集合——
    /// 知识是实例级通用层，MUST NOT 按智能体或会话分区（与记忆明确分离）。
    #[test]
    fn 知识_跨智能体共享读写() {
        let store = test_store();
        let a = store
            .write(&KnowledgeDraft {
                source_agent: "creative-agent".into(),
                source_session: Some("sess-1".into()),
                ..draft("napcat 图片直发", "QQ 官方机器人 files 上行仅支持 png/jpg，file_data 为 base64")
            })
            .unwrap();
        let _ = store.write(&KnowledgeDraft {
            source_agent: "coding-agent".into(),
            source_session: Some("sess-2".into()),
            ..draft("无关心条目", "完全不同的另一条知识内容，讲的是构建缓存")
        });

        // 另一「智能体」（不同 source）检索：必须命中他人写入的条目
        let hits = store.search("napcat 直发", 5).unwrap();
        assert_eq!(hits.len(), 1, "跨智能体检索应命中");
        assert_eq!(hits[0].id, a.entry().id);
        assert_eq!(hits[0].source_agent, "creative-agent", "来源标注保留");
        assert!(hits[0].hits >= 1, "检索命中应累加热度");
    }

    #[test]
    fn 知识_重复内容去重提升() {
        let store = test_store();
        let first = store.write(&draft("部署约束", "构建环境无 C 编译器，依赖必须纯 Rust")).unwrap();
        assert!(!first.deduped(), "首写应为新建");
        let again = store
            .write(&KnowledgeDraft {
                tags: vec!["build".into()],
                ..draft("部署约束", "构建环境无 C 编译器，依赖必须纯 Rust")
            })
            .unwrap();
        assert!(again.deduped(), "同正文再写应去重提升");
        assert_eq!(again.entry().id, first.entry().id, "条目 id 不变");
        assert!(again.entry().hits >= 1, "去重提升应累加 hits");
        assert!(
            again.entry().tags.contains(&"build".into()),
            "标签并集补充"
        );
        assert_eq!(store.list_all().len(), 1, "库里只有一条");
    }

    #[test]
    fn 知识_正文超限拒绝语义与截断() {
        let store = test_store();
        // 钳制语义：超长正文截到上限（与记忆同口径——哈希对实际落库内容计算）
        let long_body = "学".repeat(KNOWLEDGE_BODY_CAP_CHARS + 100);
        let out = store.write(&draft("超长", &long_body)).unwrap();
        assert_eq!(out.entry().body.chars().count(), KNOWLEDGE_BODY_CAP_CHARS);
    }

    #[test]
    fn 知识_删除与检索清理() {
        let store = test_store();
        let e = store.write(&draft("时区配置", "automation.timezone 接受 IANA 时区名")).unwrap();
        assert!(store.delete(&e.entry().id).unwrap(), "删除应成功");
        assert!(!store.delete(&e.entry().id).unwrap(), "二次删除返回 false");
        assert!(store.get(&e.entry().id).unwrap().is_none());
        assert!(store.search("时区", 5).unwrap().is_empty(), "删除后不应命中");
    }

    // ---------------- 提示词注入（inject_prompt_section） ----------------

    /// 默认零注入（spec 场景）：inject_top_k=0 时无论库中有多少内容都返回空串
    #[test]
    fn 知识注入_默认零注入() {
        let store = test_store();
        store.write(&draft("部署约束", "构建环境无 C 编译器，依赖必须纯 Rust")).unwrap();
        let out = inject_prompt_section(&store, "部署 Rust 依赖", 0, 500);
        assert!(out.is_empty(), "top_k=0 必须零注入（默认零行为变化）");
    }

    /// 开启后注入：命中条目带标注与来源；每条截断到 max_chars 并标注截断
    #[test]
    fn 知识注入_开启后注入与截断() {
        let store = test_store();
        let e = store
            .write(&draft(
                "napcat 约束",
                "QQ 官方机器人 files 上行仅支持 png/jpg 且 ≤20MB",
            ))
            .unwrap();
        let out = inject_prompt_section(&store, "napcat 上行格式", 3, 20);
        assert!(out.contains("知识库参考"), "标注知识库参考：{out}");
        assert!(out.contains(e.entry().id.as_str()), "含条目 id");
        assert!(out.contains("来源: test-agent"), "标注来源");
        assert!(out.contains("…（截断）"), "超限截断标注");
        // 每条正文不超过截断上限（+截断标注）
        let body_line = out
            .lines()
            .find(|l| l.starts_with("  "))
            .unwrap_or_default();
        assert!(body_line.chars().count() <= 20 + "…（截断）".chars().count() + 2);
    }

    /// 空查询不注入；无命中不注入（不产生空节）
    #[test]
    fn 知识注入_空查询与无命中() {
        let store = test_store();
        store.write(&draft("主题A", "内容甲")).unwrap();
        assert!(inject_prompt_section(&store, "  ", 3, 500).is_empty(), "空查询不注入");
        assert!(
            inject_prompt_section(&store, "完全无关的词项", 3, 500).is_empty(),
            "无命中不注入"
        );
    }
}
