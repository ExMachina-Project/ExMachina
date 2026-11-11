//! 记忆系统 —— 自我进化内核（docs/08）
//!
//! 存储（纯 Rust 文档存储，零 C 依赖）：
//!   `memory/entries/{id}.json`        记忆条目
//!   `memory/hash_index/{hash}.json`   内容哈希 → 条目 id（去重键）
//!   `memory/terms/bucket-XX.json`     倒排索引分桶：term → [(entry_id, weight)]
//!   `memory/links/{entryId}.jsonl`    记忆关联（derived_from/contradicts/supersedes）
//!   `memory/agent_stats/{agentId}.json` 个体可靠性统计
//!
//! 基础记忆 `memory.md` 由条目渲染（可随时再生成），是 DB 的人读视图。

use crate::fsdb::FsDb;
use crate::types::*;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const ENTRIES: &str = "memory/entries";
const HASH_INDEX: &str = "memory/hash_index";
const TERMS: &str = "memory/terms";
const LINKS: &str = "memory/links";
const STATS: &str = "memory/agent_stats";

// ---------------------------------------------------------------- 类型

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryKind {
    /// 基础事实（环境、路径、约定）
    Fact,
    /// 决策与取舍
    Decision,
    /// 用户偏好（群体共享：对全体个体生效）
    Preference,
    /// 证据（A/B 级）
    Evidence,
    /// 会话摘要
    Digest,
    /// 教训（失败/受阻原因）
    Lesson,
}

impl MemoryKind {
    pub fn key(self) -> &'static str {
        match self {
            MemoryKind::Fact => "fact",
            MemoryKind::Decision => "decision",
            MemoryKind::Preference => "preference",
            MemoryKind::Evidence => "evidence",
            MemoryKind::Digest => "digest",
            MemoryKind::Lesson => "lesson",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "fact" => MemoryKind::Fact,
            "decision" => MemoryKind::Decision,
            "preference" => MemoryKind::Preference,
            "evidence" => MemoryKind::Evidence,
            "digest" => MemoryKind::Digest,
            "lesson" => MemoryKind::Lesson,
            _ => return None,
        })
    }
    fn boost(self) -> f64 {
        match self {
            MemoryKind::Preference => 0.60,
            MemoryKind::Decision => 0.45,
            MemoryKind::Lesson => 0.35,
            MemoryKind::Fact => 0.30,
            MemoryKind::Evidence => 0.15,
            MemoryKind::Digest => 0.05,
        }
    }
    pub fn all() -> [MemoryKind; 6] {
        [
            MemoryKind::Fact,
            MemoryKind::Decision,
            MemoryKind::Preference,
            MemoryKind::Evidence,
            MemoryKind::Digest,
            MemoryKind::Lesson,
        ]
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryEntry {
    pub id: String,
    pub kind: MemoryKind,
    /// global | project | session
    pub scope: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    /// 组归属（docs/09 §6）：None = 全局可见；Some(gid) = 仅该组可见（旧数据视为全局）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_id: Option<String>,
    pub title: String,
    pub body: String,
    #[serde(default)]
    pub tags: Vec<String>,
    pub importance: f64,
    pub confidence: f64,
    pub pinned: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_ref: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_accessed_at: Option<String>,
    #[serde(default)]
    pub access_count: u32,
    /// 语义向量（混合记忆检索；None = 未嵌入，仅参与词项召回）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding: Option<Vec<f32>>,
}

#[derive(Debug, Clone)]
pub struct MemoryDraft {
    pub kind: MemoryKind,
    pub scope: String,
    pub session_id: Option<String>,
    pub agent_id: Option<String>,
    pub group_id: Option<String>,
    pub title: String,
    pub body: String,
    pub tags: Vec<String>,
    pub importance: f64,
    pub confidence: f64,
    pub source_ref: Option<String>,
}

impl MemoryDraft {
    pub fn new(kind: MemoryKind, title: impl Into<String>, body: impl Into<String>) -> Self {
        MemoryDraft {
            kind,
            scope: "project".into(),
            session_id: None,
            agent_id: None,
            group_id: None,
            title: title.into(),
            body: body.into(),
            tags: vec![],
            importance: 0.5,
            confidence: 0.7,
            source_ref: None,
        }
    }
    pub fn importance(mut self, v: f64) -> Self {
        self.importance = v.clamp(0.0, 1.0);
        self
    }
    pub fn tags(mut self, tags: &[&str]) -> Self {
        self.tags = tags.iter().map(|s| s.to_string()).collect();
        self
    }
    pub fn session(mut self, id: &str) -> Self {
        self.session_id = Some(id.to_string());
        self
    }
    pub fn agent(mut self, id: &str) -> Self {
        self.agent_id = Some(id.to_string());
        self
    }
    pub fn group(mut self, gid: &str) -> Self {
        self.group_id = Some(gid.to_string());
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecallHit {
    pub entry: MemoryEntry,
    pub score: f64,
    pub reasons: Vec<String>,
    /// 调试视图（8.4）：本条命中的查询词项明细（词 → 该词权重贡献）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matched_terms: Option<std::collections::BTreeMap<String, f64>>,
    /// 调试视图（8.4）：语义相似分数值（0~1 余弦，无嵌入/无查询向量时 None）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_score: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentStat {
    pub agent_id: String,
    pub runs: u32,
    pub done: u32,
    pub blocked: u32,
    pub failed: u32,
    pub avg_confidence: f64,
    pub updated_at: String,
}

/// 可插拔嵌入器：默认词法实现；后续接入向量模型时替换即可（打分函数是唯一改动点）
pub trait Embedder: Send + Sync {
    fn embed(&self, text: &str) -> Vec<f32>;
    fn similarity(&self, a: &[f32], b: &[f32]) -> f64;
}

pub struct LexicalEmbedder;

impl Embedder for LexicalEmbedder {
    fn embed(&self, text: &str) -> Vec<f32> {
        let mut v = vec![0.0f32; 64];
        for t in tokens(text) {
            let h = t
                .bytes()
                .fold(0u32, |a, b| a.wrapping_mul(31).wrapping_add(b as u32));
            v[(h % 64) as usize] += 1.0;
        }
        v
    }
    fn similarity(&self, a: &[f32], b: &[f32]) -> f64 {
        let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
        let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
        let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
        if na == 0.0 || nb == 0.0 {
            0.0
        } else {
            (dot / (na * nb)) as f64
        }
    }
}

// ---------------------------------------------------------------- 记忆库

pub struct MemoryStore {
    db: FsDb,
    dir: PathBuf,
    /// 写入串行锁：remember 的「查哈希 → 命中提升 / 未命中新建」是跨多文件的读-改-写，
    /// 并发写同内容会双双未命中而各建一条（哈希去重契约被破坏），并发命中提升也会互相覆盖丢增量。
    /// 只串行「写路径」（remember/forget/pin/decay）；检索不持锁——recall 里 access_count
    /// 的并发丢失属可容忍的统计误差，不值得让检索排队。
    write_lock: parking_lot::Mutex<()>,
}

type TermBucket = std::collections::HashMap<String, Vec<(String, f64)>>;

/// 记忆分层过滤（docs/08）：个体 = 归属某智能体的私有记忆；群体 = 无归属共享
#[derive(Clone, Copy)]
enum AgentLayer<'a> {
    /// 群体 + 所有个体（管理视角）
    All,
    /// 仅群体（指挥体规划）
    SharedOnly,
    /// 某个体可见范围：其私有 + 群体
    Agent(&'a str),
}


// ---------------------------------------------------------------- 域分文件

mod crud;
mod recall;
mod render;

use render::*;

#[cfg(test)]
mod tests;
