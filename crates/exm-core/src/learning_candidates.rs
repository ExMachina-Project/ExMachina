//! learning_candidates.rs —— 学习候选与来源证据（capability-parity 6.4，
//! spec: agent-learning-loop「经验候选与来源证据」）。
//!
//! 任务后沉淀的可复用经验先成为**候选**，不直接写长期技能/记忆：
//! - 候选必须绑定真实证据（工具回执 / 测试结果 / 文件路径 / 会话证据账本）；
//!   「模型自述成功」（无证据引用）默认拒绝进入候选；
//! - 敏感信息（密钥/令牌/私钥/密码形态）与私密数据默认拒绝；
//! - 范围（scope）与触发条件显式声明，重复候选（同 scope+触发词高度重合）被合并拒收；
//! - 人工确认后才可转正（promote 面由 6.5 的版本化写入承接，此处只管候选态）。
//!
//! 候选本身是 jsonl 追加账本（learning_candidates 集合）——状态机：
//! pending →（人工）approved / rejected；approved 的落地走 6.5。

use crate::store::Store;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// 候选证据类型：只有这些是「真实证据」——模型自述不在其列。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateEvidenceKind {
    /// 本会话工具调用回执（reference = 工具调用 callId，记录须 ok=true）
    ToolReceipt,
    /// 本会话测试通过回执（reference = callId；终端命令、测试语义、成功）
    TestResult,
    /// 本会话写工具产出的工作区文件（相对路径，不出界，且本会话有成功写调用指向它）
    ArtifactFile,
    /// 会话证据账本条目（EvidenceKind::Test/Command 级别 ≥B）
    SessionEvidence,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LearningCandidate {
    pub id: String,
    /// 目标载体：skill / memory（转正时决定写到哪）
    pub target: String,
    /// 目标载体标识（技能 id；记忆候选为空）
    #[serde(default)]
    pub target_id: String,
    pub title: String,
    /// 经验正文（技能 instructions / 记忆内容）
    pub content: String,
    /// 适用范围：global / group / agent:<id>
    pub scope: String,
    /// 触发条件（触发词列表；命中即候选注入）
    pub triggers: Vec<String>,
    /// 置信度 0-1（来源证据强度，非模型自评）
    pub confidence: f64,
    /// 来源任务（会话 id + 目标摘要）
    pub source_session: String,
    pub source_goal: String,
    /// 绑定证据（kind + 引用 + 校验状态）
    pub evidence: Vec<BoundEvidence>,
    pub status: String,
    pub created_at: String,
    pub decided_at: Option<String>,
    pub decided_by: Option<String>,
    pub reject_reason: Option<String>,
    /// 转正结果（人工批准后才执行；serde default——存量账本无此字段，按「未转正」读）。
    /// 裁决（approved）与转正（写入技能/记忆）是两件事：转正失败不改变用户的裁决，
    /// 但失败必须留痕，不能让「已批准」看起来像「已生效」。
    #[serde(default)]
    pub promotion: Option<Promotion>,
}

/// 转正（把已批准候选写入技能目录 / 记忆存储）的结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Promotion {
    pub ok: bool,
    /// 写入目标：技能 id 或记忆条目 id
    pub ref_id: String,
    /// 技能：写入后的版本号（撤销时回滚到 version-1）
    pub version: Option<u64>,
    pub at: String,
    /// 失败原因（ok=false 时）
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoundEvidence {
    pub kind: CandidateEvidenceKind,
    /// 引用（工具调用 callId / 工作区相对路径 / 证据条目 id）
    pub reference: String,
    /// 登记时校验结果（"verified" / "unverified"）
    pub verification: String,
}

/// 敏感内容标记（默认拒绝进候选）：常见密钥/令牌形态。
/// 敏感内容判定：按**形态**匹配，不按用词。
///
/// 要挡的是「凭据值」与「个人标识符」被写进长期技能/记忆——而不是「密码」「token」
/// 这些概念本身。一个教模型「不要把密钥写进日志」的安全技能、一篇讲「控制 token
/// 预算」的说明，都是正当内容；按裸词匹配会把它们一律拒掉，且拒绝理由（"敏感信息"）
/// 对用户是误导。所以这里只认结构化的凭据/隐私形态：
///
/// - PEM 私钥块；
/// - 带已知前缀的访问令牌（`sk-…`、`AKIA…`、`ghp_…`、`xox?‑…`、`AIza…`）；
/// - 把值赋给凭据名（`password=hunter2`、`api_key: …`、`SECRET_TOKEN="…"`）——
///   值须是 ≥6 个连续的 ASCII 可见字符（真实凭据几乎都是；中文说明、占位符
///   `<your-token>`、`${ENV}` 之类不算）；
/// - `Authorization: Bearer <值>`、URL 内嵌账号密码；
/// - 个人隐私：邮箱、中国大陆手机号、18 位身份证号、16–19 位连续数字（银行卡形态）。
///
/// 这是启发式，挡不住刻意拆分/编码后的凭据——它是一道默认排除，不是防泄露的边界；
/// 真正的边界仍是人工确认与权限校验。
pub fn is_sensitive_content(text: &str) -> bool {
    // 无需二次判定的形态（Rust regex 不支持环视，数字边界用「非数字或首尾」表达）
    static SHAPES: std::sync::OnceLock<regex::RegexSet> = std::sync::OnceLock::new();
    let shapes = SHAPES.get_or_init(|| {
        regex::RegexSet::new([
            // PEM 私钥块
            r"-----BEGIN [A-Z ]*PRIVATE KEY-----",
            // 已知前缀的访问令牌
            r"\bsk-[A-Za-z0-9_\-]{16,}",
            r"\bAKIA[0-9A-Z]{16}\b",
            r"\bgh[pousr]_[A-Za-z0-9]{20,}",
            r"\bxox[abposr]-[A-Za-z0-9\-]{10,}",
            r"\bAIza[0-9A-Za-z_\-]{30,}",
            // Bearer 头（值须足够长：`Bearer <你的令牌>` 这类占位不算）
            r"(?i)bearer\s+[A-Za-z0-9_\-\.=]{16,}",
            // URL 内嵌账号密码
            r"[a-z][a-z0-9+.\-]*://[^/\s:@]+:[^/\s@]+@",
            // 个人隐私：邮箱 / 中国大陆手机号 / 18 位身份证号 / 16–19 位连续数字（银行卡形态）
            r"[A-Za-z0-9._%+\-]+@[A-Za-z0-9.\-]+\.[A-Za-z]{2,}",
            r"(^|[^\d])1[3-9]\d{9}([^\d]|$)",
            r"(^|[^\d])\d{17}[\dXx]([^\d]|$)",
            // 16–19 位数字（银行卡形态）：连续书写，或严格按 4 位一组、组间单个空格/短横线
            // （6222 0210 0110 2345 678）。不放宽到任意分组——否则「迭代 1500 2000 2500 3000 次」
            // 这类普通句子会被误拒，而本判定守着所有技能写入。
            r"(^|[^\d])(?:\d{4}[ \-]){3,4}\d{1,3}([^\d]|$)",
            // 连续书写的 16–19 位数字。已知误伤面：雪花 id / 分布式 id 也是 16–19 位，形态上
            // 与卡号无法区分——本判定是「默认排除」而非终审，宁可保守（人工确认可放行）。
            r"(^|[^\d])\d{16,19}([^\d]|$)",
        ])
        .expect("敏感内容正则应可编译")
    });
    if shapes.is_match(text) {
        return true;
    }
    // 赋值形态：凭据名 = 值 / 凭据名: 值。捕获「值」，再排除占位符与变量引用。
    static ASSIGN: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let assign = ASSIGN.get_or_init(|| {
        regex::Regex::new(
            r#"(?i)(?:api[_\- ]?key|api[_\- ]?secret|secret|token|passw(?:or)?d|pwd|access[_\- ]?key|secret[_\- ]?key|client[_\- ]?secret|private[_\- ]?key)\s*[:=]\s*["']?([^\s"']{6,})"#,
        )
        .expect("赋值形态正则应可编译")
    });
    assign.captures_iter(text).any(|caps| {
        let value = caps.get(1).map(|m| m.as_str()).unwrap_or_default();
        !is_placeholder_value(value)
    })
}

/// 赋值形态里的「值」是否只是占位符 / 变量引用 / 说明文字——这些不是凭据。
fn is_placeholder_value(value: &str) -> bool {
    let v = value.trim_matches(|c| r#"'"),;."#.contains(c));
    // 变量引用 / 模板：${X}、$X、%X%、{{X}}、<X>
    if v.starts_with("${")
        || v.starts_with('$')
        || v.starts_with('%')
        || v.starts_with("{{")
        || (v.starts_with('<') && v.ends_with('>'))
        || (v.starts_with('[') && v.ends_with(']'))
    {
        return true;
    }
    // 占位词：your-…、xxx…、…、<…>、redacted
    let lower = v.to_lowercase();
    if lower.starts_with("your") || lower.starts_with("xxx") || lower == "..." || lower == "…" {
        return true;
    }
    // 含非 ASCII（中文说明等）：凭据值几乎都是 ASCII，含中文的是说明不是凭据
    !v.is_ascii()
}

impl CandidateEvidenceKind {
    pub fn key(self) -> &'static str {
        match self {
            CandidateEvidenceKind::ToolReceipt => "tool_receipt",
            CandidateEvidenceKind::TestResult => "test_result",
            CandidateEvidenceKind::ArtifactFile => "artifact_file",
            CandidateEvidenceKind::SessionEvidence => "session_evidence",
        }
    }
}

/// 候选草稿（模型/流程提议时的输入）。
pub struct CandidateDraft<'a> {
    /// skill / memory
    pub target: &'a str,
    /// 目标载体上的标识：skill = 技能 id（必填，转正时写到该 id；update 候选据此对应既有技能）；
    /// memory = 留空（转正时由记忆存储分配条目 id）
    pub target_id: Option<&'a str>,
    pub title: &'a str,
    pub content: &'a str,
    pub scope: &'a str,
    pub triggers: &'a [String],
    pub source_session: &'a str,
    pub source_goal: &'a str,
    /// 证据引用（kind + reference 对）
    pub evidence: &'a [(CandidateEvidenceKind, String)],
}

/// 校验结果：拒绝原因（None = 通过）。
pub struct Validation {
    pub ok: bool,
    pub reasons: Vec<String>,
}

/// 本会话的工具回执 = 已落盘消息里的记录 ∪ **本轮内存暂存**里的记录。
///
/// 两者必须合并：`push_tool` 只把回执放进 round_trace 暂存，到轮次收束（消息落盘）
/// 才进 `ChatMessage.tool_calls`；而模型提交候选通常发生在**同一轮内**（刚跑完测试
/// 紧接着沉淀）。只读已落盘消息会让本轮的真实回执全部核验失败——证据门在生产里
/// 形同关闭（单测若只往 store 里直接写消息，发现不了这一点）。
/// 暂存只读快照、不取走，不影响轮次收束。
fn session_tool_receipts(
    store: &Store,
    session_id: &str,
) -> Vec<crate::round_trace::ToolCallRecord> {
    let mut out = Vec::new();
    for msg in store.list_messages(session_id, 0).unwrap_or_default() {
        out.extend(msg.tool_calls);
    }
    // 上限取一个足够覆盖单轮的上界（单轮步数本身有 max_steps 约束，远小于此）
    out.extend(crate::round_trace::snapshot_tool_records(
        session_id, 10_000,
    ));
    out
}

/// 命令行是否具备「测试」语义（测试回执证据的判定依据：不能拿任意成功命令冒充）。
fn looks_like_test_command(cmd: &str) -> bool {
    let c = cmd.trim().to_lowercase();
    [
        "cargo test",
        "cargo nextest",
        "pytest",
        "python -m unittest",
        "go test",
        "npm test",
        "npm run test",
        "pnpm test",
        "yarn test",
        "vitest",
        "jest",
        "mvn test",
        "gradle test",
        "phpunit",
        "rspec",
        "dotnet test",
        "make test",
        "make check",
        "cargo check",
        "cargo clippy",
        "cargo build",
    ]
    .iter()
    .any(|m| c.contains(m))
}

/// 工作区内文件证据归属：只接受相对路径（拒绝绝对路径与 `..`），canonicalize
/// 后必须仍在工作区内且为常规文件。无工作区的会话不接受文件证据（无法判定归属）。
fn artifact_within_workspace(
    workspace: Option<&Path>,
    reference: &str,
) -> Option<std::path::PathBuf> {
    let root = workspace?;
    let rel = Path::new(reference.trim());
    if rel.is_absolute() || reference.trim().is_empty() {
        return None;
    }
    if rel
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return None;
    }
    let root = root.canonicalize().ok()?;
    let full = root.join(rel).canonicalize().ok()?;
    if !full.starts_with(&root) || !full.is_file() {
        return None;
    }
    Some(full)
}

/// 本会话「真实产出的文件」：工作区内、常规文件，**且本会话有一条成功的写类工具调用
/// （filesystem / edit / patch）指向它**。
///
/// 文件证据是四类证据里证明力最弱的一类——它只能说明「有东西被写出来了」，说明不了
/// 任务成功；真正的成功只有测试回执能证明。这里额外要求「由本会话的写工具产出」，
/// 是为了挡住最廉价的冒充：拿仓库里早就存在的文件、或没动过任何文件的会话里的旧文件
/// 充当成果。路径按工作区相对坐标比较（写工具收到的 path 即模型给的相对路径；
/// 绝对路径写入会剥掉工作区前缀后再比）。
fn artifact_written_this_session(
    workspace: Option<&Path>,
    full: &Path,
    receipts: &[crate::round_trace::ToolCallRecord],
) -> bool {
    let Some(root) = workspace.and_then(|p| p.canonicalize().ok()) else {
        return false;
    };
    let writes = ["filesystem", "edit", "patch"];
    receipts.iter().any(|r| {
        if !r.ok || !writes.iter().any(|w| r.tool == *w) {
            return false;
        }
        if r.tool == "filesystem" {
            let op = r.args.get("op").and_then(|v| v.as_str()).unwrap_or("write");
            if op != "write" {
                return false;
            }
        }
        let Some(given) = r.args.get("path").and_then(|v| v.as_str()) else {
            return false;
        };
        // 写工具收到的 path 可能是工作区相对路径，也可能是绝对路径：统一解析到同一
        // 坐标系再比（不依赖进程 cwd）。
        let given_path = Path::new(given.trim());
        let resolved = if given_path.is_absolute() {
            given_path.canonicalize().ok()
        } else {
            root.join(given_path).canonicalize().ok()
        };
        resolved.is_some_and(|p| p == full)
    })
}

/// 校验证据引用是否真实（6.4 红线：只有真实文件/测试/回执支持的经验才可进入候选）。
/// 归属是核验的一部分——证据必须属于**本会话**，别的会话的回执/别处的文件不算数：
/// - tool_receipt：本会话消息里存在该 callId 的工具记录且 ok=true；
/// - test_result：同上，且该记录是终端命令、命令行具备测试语义、执行成功；
/// - artifact_file：会话工作区内的常规文件（相对路径，不出界）；
/// - session_evidence：本会话证据账本存在该条目且级别 ≥B。
pub fn verify_evidence(
    store: &Store,
    session_id: &str,
    workspace: Option<&Path>,
    evidence: &[(CandidateEvidenceKind, String)],
) -> Vec<BoundEvidence> {
    let receipts = session_tool_receipts(store, session_id);
    let session_rows = store.list_evidence(session_id).ok();
    evidence
        .iter()
        .map(|(kind, reference)| {
            let reference = reference.trim();
            let verified = match kind {
                CandidateEvidenceKind::ArtifactFile => {
                    artifact_within_workspace(workspace, reference).is_some_and(|full| {
                        artifact_written_this_session(workspace, &full, &receipts)
                    })
                }
                CandidateEvidenceKind::ToolReceipt => {
                    receipts.iter().any(|r| r.call_id == reference && r.ok)
                }
                CandidateEvidenceKind::TestResult => receipts.iter().any(|r| {
                    r.call_id == reference
                        && r.ok
                        && r.tool.contains("terminal")
                        && r.args
                            .get("command")
                            .or_else(|| r.args.get("cmd"))
                            .and_then(|v| v.as_str())
                            .map(looks_like_test_command)
                            .unwrap_or(false)
                }),
                CandidateEvidenceKind::SessionEvidence => {
                    session_rows.as_ref().is_some_and(|rows| {
                        rows.iter().any(|row| {
                            row.get("id").and_then(|v| v.as_str()) == Some(reference)
                                && matches!(
                                    row.get("level").and_then(|v| v.as_str()),
                                    Some("A") | Some("B")
                                )
                        })
                    })
                }
            };
            BoundEvidence {
                kind: *kind,
                reference: reference.to_string(),
                verification: if verified { "verified" } else { "unverified" }.into(),
            }
        })
        .collect()
}

/// 候选准入校验（6.4 全部红线）。返回 (校验, 绑定证据)。
pub fn validate_candidate(
    store: &Store,
    workspace: Option<&Path>,
    draft: &CandidateDraft,
) -> (Validation, Vec<BoundEvidence>) {
    let mut reasons = Vec::new();
    if draft.title.trim().is_empty() || draft.content.trim().is_empty() {
        reasons.push("title/content 不能为空".into());
    }
    if draft.content.chars().count() > 30_000 {
        reasons.push("content 超过 30000 字符上限".into());
    }
    // 敏感内容默认拒（凭据值 / 个人标识符形态——按形态判定，不按用词）
    if is_sensitive_content(draft.content) || is_sensitive_content(draft.title) {
        reasons.push("内容含敏感信息形态（凭据值 / 私钥 / 个人隐私标识）——默认拒绝".into());
    }
    // 目标载体：只认 skill / memory（转正时决定写到哪；未知 target 无处可写）
    match draft.target.trim().to_lowercase().as_str() {
        "skill" => {
            // 技能候选必须指明写到哪个 id——否则转正时无从落位，update 候选也无法对应
            let id = draft.target_id.unwrap_or_default().trim();
            if id.is_empty() {
                reasons.push("技能候选缺少 id（target_id）——转正时无从落位".into());
            } else if id.len() > 64
                || !id
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            {
                reasons.push("技能 id 非法（仅字母/数字/-/_，≤64）".into());
            }
        }
        "memory" => {}
        other => reasons.push(format!("非法 target：{other}（skill / memory）")),
    }
    // 范围合法性
    let scope = draft.scope.trim();
    if scope.is_empty() {
        reasons.push("scope 不能为空（global / group / agent:<id>）".into());
    } else if scope != "global" && scope != "group" && !scope.starts_with("agent:") {
        reasons.push(format!(
            "非法 scope：{scope}（global / group / agent:<id>）"
        ));
    }
    // 触发词：至少一个非空（无触发条件的经验无法被相关性选择命中）
    if draft.triggers.iter().all(|t| t.trim().is_empty()) {
        reasons.push("至少一个非空触发词（无触发条件无法被相关性选择）".into());
    }
    // 证据：至少一条「真实证据」且经核验为 verified
    let bound = verify_evidence(store, draft.source_session, workspace, draft.evidence);
    if draft.evidence.is_empty() {
        reasons.push(
            "缺少真实证据（模型自述成功不构成证据）——需要工具回执/测试结果/文件产物/证据账目之一"
                .into(),
        );
    } else if bound.iter().all(|e| e.verification != "verified") {
        reasons.push("证据未通过核验（引用不存在或结果非成功）——只有真实文件/测试/回执支持的经验才可进入候选".into());
    }
    (
        Validation {
            ok: reasons.is_empty(),
            reasons,
        },
        bound,
    )
}

/// 重复校验：与既有 pending/approved 候选同 scope 且触发词重合度 ≥ 0.6 判重复。
pub fn is_duplicate(existing: &[LearningCandidate], draft: &CandidateDraft) -> Option<String> {
    let new_triggers: std::collections::HashSet<String> = draft
        .triggers
        .iter()
        .map(|t| t.trim().to_lowercase())
        .collect();
    for cand in existing {
        if cand.status == "rejected" {
            continue;
        }
        if cand.scope != draft.scope.trim() {
            continue;
        }
        let old: std::collections::HashSet<String> = cand
            .triggers
            .iter()
            .map(|t| t.trim().to_lowercase())
            .collect();
        if old.is_empty() || new_triggers.is_empty() {
            continue;
        }
        let overlap = old.intersection(&new_triggers).count();
        let ratio = overlap as f64 / old.len().min(new_triggers.len()).max(1) as f64;
        if ratio >= 0.6 {
            return Some(format!(
                "与候选 {}/「{}」触发词重合（{:.0}%）——同域经验应走候选迭代而非重复登记",
                cand.id,
                cand.title,
                ratio * 100.0
            ));
        }
    }
    None
}

/// 候选账本（store 之上的薄封装：追加 + 列出 + 裁决）。
pub struct CandidateBook<'a> {
    pub store: &'a Store,
}

impl<'a> CandidateBook<'a> {
    pub fn new(store: &'a Store) -> Self {
        Self { store }
    }

    /// 列出候选（可按状态过滤）。
    pub fn list(&self, status: Option<&str>) -> Vec<LearningCandidate> {
        self.store
            .list_learning_candidates()
            .into_iter()
            .filter(|c| status.map(|s| c.status == s).unwrap_or(true))
            .collect()
    }

    /// 登记候选（全红线校验）。返回候选或拒绝原因。
    pub fn submit(
        &self,
        workspace: Option<&Path>,
        draft: &CandidateDraft,
    ) -> Result<LearningCandidate, Vec<String>> {
        let (validation, bound) = validate_candidate(self.store, workspace, draft);
        let mut reasons = validation.reasons;
        if reasons.is_empty() {
            if let Some(dup) = is_duplicate(&self.list(None), draft) {
                reasons.push(dup);
            }
        }
        if !reasons.is_empty() {
            return Err(reasons);
        }
        // 置信度：verified 证据占比（来源证据强度，非模型自评）
        let verified = bound
            .iter()
            .filter(|e| e.verification == "verified")
            .count();
        let confidence = if bound.is_empty() {
            0.0
        } else {
            (verified as f64 / bound.len() as f64).min(1.0)
        };
        let candidate = LearningCandidate {
            id: crate::types::new_id(),
            target: draft.target.trim().to_lowercase(),
            target_id: draft.target_id.unwrap_or_default().trim().to_string(),
            title: draft.title.trim().to_string(),
            content: draft.content.trim().to_string(),
            scope: draft.scope.trim().to_string(),
            triggers: draft
                .triggers
                .iter()
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty())
                .collect(),
            confidence,
            source_session: draft.source_session.to_string(),
            source_goal: draft.source_goal.to_string(),
            evidence: bound,
            status: "pending".into(),
            created_at: crate::types::now_iso(),
            decided_at: None,
            decided_by: None,
            reject_reason: None,
            promotion: None,
        };
        self.store
            .append_learning_candidate(&candidate)
            .map_err(|e| vec![format!("候选账本写入失败: {e}")])?;
        Ok(candidate)
    }

    /// 人工裁决：approve / reject（带决定者）。已裁决的候选不可重复裁决。
    /// 记录转正结果（追加一行同 id 记录，读时取最新）。
    pub fn record_promotion(&self, id: &str, promotion: Promotion) -> Result<(), String> {
        let mut all = self.list(None);
        let Some(cand) = all.iter_mut().find(|c| c.id == id) else {
            return Err(format!("候选不存在: {id}"));
        };
        cand.promotion = Some(promotion);
        self.store
            .update_learning_candidate(cand)
            .map_err(|e| format!("候选账本写入失败: {e}"))
    }

    pub fn decide(
        &self,
        id: &str,
        approve: bool,
        decided_by: &str,
        reason: Option<&str>,
    ) -> Result<LearningCandidate, String> {
        let mut all = self.list(None);
        let Some(cand) = all.iter_mut().find(|c| c.id == id) else {
            return Err(format!("候选不存在: {id}"));
        };
        if cand.status != "pending" {
            return Err(format!("候选已裁决（{}）——不可重复裁决", cand.status));
        }
        cand.status = if approve {
            "approved".into()
        } else {
            "rejected".into()
        };
        cand.decided_at = Some(crate::types::now_iso());
        cand.decided_by = Some(decided_by.to_string());
        if !approve {
            cand.reject_reason = reason.map(str::to_string);
        }
        self.store
            .update_learning_candidate(cand)
            .map_err(|e| format!("候选账本写入失败: {e}"))?;
        Ok(cand.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::round_trace::ToolCallRecord;
    use std::path::PathBuf;

    /// 真实夹具：一个带工作区的会话 + 一条落盘了工具回执的消息。
    /// 回执：`call-ok-test`（cargo test 成功）、`call-ok-ls`（ls 成功，非测试）、
    /// `call-fail-test`（cargo test 失败）；工作区里有 `src/lib.rs`。
    struct Fixture {
        _dir: PathBuf,
        store: Store,
        session: String,
        workspace: PathBuf,
    }

    fn receipt(id: &str, tool: &str, command: &str, ok: bool) -> ToolCallRecord {
        ToolCallRecord {
            call_id: id.into(),
            agent_id: "orchestrator".into(),
            tool: tool.into(),
            args: serde_json::json!({ "command": command }),
            ok,
            duration_ms: 5,
            summary: String::new(),
        }
    }

    fn write_receipt(id: &str, path: &str, ok: bool) -> ToolCallRecord {
        ToolCallRecord {
            call_id: id.into(),
            agent_id: "orchestrator".into(),
            tool: "filesystem".into(),
            args: serde_json::json!({ "op": "write", "path": path, "content": "x" }),
            ok,
            duration_ms: 5,
            summary: String::new(),
        }
    }

    fn read_receipt(id: &str, path: &str) -> ToolCallRecord {
        ToolCallRecord {
            call_id: id.into(),
            agent_id: "orchestrator".into(),
            tool: "read".into(),
            args: serde_json::json!({ "path": path }),
            ok: true,
            duration_ms: 5,
            summary: String::new(),
        }
    }

    fn fixture(tag: &str, with_workspace: bool) -> Fixture {
        fixture_with(tag, with_workspace, true)
    }

    fn fixture_with(tag: &str, with_workspace: bool, with_receipts: bool) -> Fixture {
        let dir = std::env::temp_dir().join(format!("exm-lc-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("entities")).unwrap();
        let workspace = dir.join("ws");
        std::fs::create_dir_all(workspace.join("src")).unwrap();
        std::fs::write(workspace.join("src/lib.rs"), "pub fn x() {}\n").unwrap();
        std::fs::create_dir_all(workspace.join("docs")).unwrap();
        std::fs::write(workspace.join("docs/old.md"), "old\n").unwrap();
        let store = Store::open(&dir).unwrap();
        let session = store
            .create_session_in(
                "lc",
                "exmachina",
                with_workspace.then(|| workspace.to_string_lossy().into_owned()),
            )
            .unwrap()
            .id;
        if with_receipts {
            store
                .add_message_full(
                    &session,
                    crate::types::MessageRole::Orchestrator,
                    Some("exmachina"),
                    vec![],
                    None,
                    vec![
                        receipt("call-ok-test", "terminal", "cargo test -p exm-core", true),
                        receipt("call-ok-ls", "terminal", "ls -la", true),
                        receipt("call-fail-test", "terminal", "cargo test", false),
                        write_receipt("call-wrote-lib", "src/lib.rs", true),
                        write_receipt("call-failed-write", "src/other.rs", false),
                        read_receipt("call-read-old", "docs/old.md"),
                    ],
                    vec![],
                )
                .unwrap();
        }
        Fixture {
            _dir: dir,
            store,
            session,
            workspace,
        }
    }

    fn draft<'a>(
        fx: &'a Fixture,
        content: &'a str,
        scope: &'a str,
        triggers: &'a [String],
        evidence: &'a [(CandidateEvidenceKind, String)],
    ) -> CandidateDraft<'a> {
        CandidateDraft {
            target: "skill",
            target_id: Some("release-test"),
            title: "发布前跑全量测试",
            content,
            scope,
            triggers,
            source_session: &fx.session,
            source_goal: "修编译错误",
            evidence,
        }
    }

    #[test]
    fn 模型自述无证据_默认拒() {
        let fx = fixture("no-ev", true);
        let triggers = vec!["发布".to_string()];
        let d = draft(&fx, "发布前跑 cargo test", "global", &triggers, &[]);
        let book = CandidateBook::new(&fx.store);
        let err = book.submit(Some(&fx.workspace), &d).unwrap_err();
        assert!(
            err.iter().any(|r| r.contains("真实证据")),
            "无证据候选被拒：{err:?}"
        );
        assert!(book.list(None).is_empty(), "拒收不落账");
    }

    #[test]
    fn 伪证据引用_核验不过() {
        let fx = fixture("fake-ev", true);
        let triggers = vec!["发布".to_string()];
        // 不存在的 callId
        let ev = vec![(CandidateEvidenceKind::ToolReceipt, "call-nope".into())];
        let d = draft(&fx, "发布前跑 cargo test", "global", &triggers, &ev);
        let book = CandidateBook::new(&fx.store);
        let err = book.submit(Some(&fx.workspace), &d).unwrap_err();
        assert!(
            err.iter().any(|r| r.contains("核验")),
            "伪证据被拒：{err:?}"
        );
        // 失败的测试回执不算测试证据
        let ev = vec![(CandidateEvidenceKind::TestResult, "call-fail-test".into())];
        let d = draft(&fx, "发布前跑 cargo test", "global", &triggers, &ev);
        let err = book.submit(Some(&fx.workspace), &d).unwrap_err();
        assert!(
            err.iter().any(|r| r.contains("核验")),
            "失败回执被拒：{err:?}"
        );
    }

    #[test]
    fn 非测试命令不能冒充测试回执() {
        let fx = fixture("not-test", true);
        let triggers = vec!["发布".to_string()];
        // `ls -la` 成功，但它不是测试——不能作为 test_result 证据
        let ev = vec![(CandidateEvidenceKind::TestResult, "call-ok-ls".into())];
        let d = draft(&fx, "发布前跑 cargo test", "global", &triggers, &ev);
        let book = CandidateBook::new(&fx.store);
        let err = book.submit(Some(&fx.workspace), &d).unwrap_err();
        assert!(err.iter().any(|r| r.contains("核验")), "{err:?}");
        // 但它是真实的工具回执：作为 tool_receipt 可过
        let ev = vec![(CandidateEvidenceKind::ToolReceipt, "call-ok-ls".into())];
        let d = draft(&fx, "发布前跑 cargo test", "global", &triggers, &ev);
        let cand = book.submit(Some(&fx.workspace), &d).unwrap();
        assert_eq!(cand.evidence[0].verification, "verified");
    }

    #[test]
    fn 文件证据_必须在工作区内且不出界() {
        let fx = fixture("artifact", true);
        let triggers = vec!["发布".to_string()];
        let book = CandidateBook::new(&fx.store);
        // 工作区内相对路径：过
        let ev = vec![(CandidateEvidenceKind::ArtifactFile, "src/lib.rs".into())];
        let d = draft(&fx, "发布前跑 cargo test", "global", &triggers, &ev);
        assert!(book.submit(Some(&fx.workspace), &d).is_ok());
        // 绝对路径（即便是工作区内的绝对路径）：拒——证据按工作区相对坐标引用
        let ev = vec![(
            CandidateEvidenceKind::ArtifactFile,
            fx.workspace
                .join("src/lib.rs")
                .to_string_lossy()
                .into_owned(),
        )];
        let d2 = CandidateDraft {
            triggers: &triggers,
            evidence: &ev,
            ..draft(&fx, "发布前跑 cargo test", "group", &triggers, &[])
        };
        let err = book.submit(Some(&fx.workspace), &d2).unwrap_err();
        assert!(
            err.iter().any(|r| r.contains("核验")),
            "绝对路径被拒：{err:?}"
        );
        // `..` 穿越：拒
        let ev = vec![(CandidateEvidenceKind::ArtifactFile, "../entities/x".into())];
        let d3 = CandidateDraft {
            triggers: &triggers,
            evidence: &ev,
            ..draft(&fx, "发布前跑 cargo test", "agent:a", &triggers, &[])
        };
        let err = book.submit(Some(&fx.workspace), &d3).unwrap_err();
        assert!(
            err.iter().any(|r| r.contains("核验")),
            "穿越路径被拒：{err:?}"
        );
    }

    /// 文件证据必须是**本会话写工具产出**的：仓库里早就存在、本会话只读过的文件不算成果。
    #[test]
    fn 文件证据_必须由本会话写工具产出() {
        let fx = fixture("artifact-own", true);
        let triggers = vec!["发布".to_string()];
        let book = CandidateBook::new(&fx.store);
        let submit = |reference: &str, scope: &str| {
            let ev = vec![(CandidateEvidenceKind::ArtifactFile, reference.to_string())];
            let d = CandidateDraft {
                content: "发布前跑 cargo test",
                triggers: &triggers,
                evidence: &ev,
                ..draft(&fx, "占位", scope, &triggers, &[])
            };
            book.submit(Some(&fx.workspace), &d)
        };
        // 早就存在、本会话只读过：拒
        let err = submit("docs/old.md", "global").unwrap_err();
        assert!(
            err.iter().any(|r| r.contains("核验")),
            "旧文件被拒：{err:?}"
        );
        // 写调用失败（文件不存在）：拒
        let err = submit("src/other.rs", "group").unwrap_err();
        assert!(
            err.iter().any(|r| r.contains("核验")),
            "失败写的文件被拒：{err:?}"
        );
        // 本会话成功写过：过
        assert!(submit("src/lib.rs", "agent:a").is_ok());
    }

    #[test]
    fn 无工作区会话_不接受文件证据() {
        let fx = fixture("no-ws", false);
        let triggers = vec!["发布".to_string()];
        // 工作区目录在磁盘上存在，但会话没有绑定它——归属无法判定，不接受
        let ev = vec![(CandidateEvidenceKind::ArtifactFile, "src/lib.rs".into())];
        let d = draft(&fx, "发布前跑 cargo test", "global", &triggers, &ev);
        let err = CandidateBook::new(&fx.store).submit(None, &d).unwrap_err();
        assert!(err.iter().any(|r| r.contains("核验")), "{err:?}");
    }

    #[test]
    fn 回执证据只认本会话() {
        let fx = fixture("own-a", true);
        // other 会话自己没有产生任何工具回执
        let other = fixture_with("own-b", true, false);
        let triggers = vec!["发布".to_string()];
        // fx 会话的 callId 拿去给 other 会话当证据：拒
        let ev = vec![(CandidateEvidenceKind::ToolReceipt, "call-ok-ls".into())];
        let d = CandidateDraft {
            source_session: &other.session,
            evidence: &ev,
            ..draft(&fx, "发布前跑 cargo test", "global", &triggers, &[])
        };
        let err = CandidateBook::new(&other.store)
            .submit(Some(&other.workspace), &d)
            .unwrap_err();
        assert!(
            err.iter().any(|r| r.contains("核验")),
            "跨会话回执被拒：{err:?}"
        );
    }

    #[test]
    fn 真实测试回执_过审进候选且重复拒() {
        let fx = fixture("real-ev", true);
        let triggers = vec!["发布".to_string()];
        let ev = vec![(CandidateEvidenceKind::TestResult, "call-ok-test".into())];
        let d = draft(&fx, "发布前跑 cargo test", "global", &triggers, &ev);
        let book = CandidateBook::new(&fx.store);
        let cand = book.submit(Some(&fx.workspace), &d).unwrap();
        assert_eq!(cand.status, "pending");
        assert_eq!(cand.confidence, 1.0, "全部证据 verified");
        assert_eq!(cand.evidence[0].verification, "verified");
        // 重复登记：同 scope 同触发词 → 拒
        let err = book.submit(Some(&fx.workspace), &d).unwrap_err();
        assert!(
            err.iter().any(|r| r.contains("重合")),
            "重复候选被拒：{err:?}"
        );
    }

    #[test]
    fn 敏感内容_默认拒() {
        let fx = fixture("sens", true);
        let triggers = vec!["部署".to_string()];
        let ev = vec![(CandidateEvidenceKind::ToolReceipt, "call-ok-ls".into())];
        let d = draft(
            &fx,
            "部署时用 API_KEY=sk-1234567890 直连",
            "global",
            &triggers,
            &ev,
        );
        let book = CandidateBook::new(&fx.store);
        let err = book.submit(Some(&fx.workspace), &d).unwrap_err();
        assert!(
            err.iter().any(|r| r.contains("敏感")),
            "敏感内容被拒：{err:?}"
        );
    }

    /// 敏感内容判定表（按形态，不按用词）。每一行都是独立断言，失败时能直接看出
    /// 是哪一类形态判错。
    #[test]
    fn 敏感内容_凭据与隐私形态被拒() {
        let rejected = [
            // 赋值形态（各种分隔与引号）
            ("API_KEY=sk-1234567890", "等号赋值"),
            ("password: hunter2", "冒号赋值"),
            ("export SECRET_TOKEN='abc123def456'", "带引号的赋值"),
            ("db_pwd = p@ssw0rd!", "下划线名 + 空格"),
            ("client-secret: 9f8e7d6c5b4a", "连字符名"),
            // 已知前缀的令牌
            ("key: sk-abcdefghijklmnopqrstuvwxyz", "sk- 前缀"),
            ("AKIAIOSFODNN7EXAMPLE", "AWS access key id"),
            ("ghp_abcdefghijklmnopqrstuvwxyz0123", "GitHub PAT"),
            ("xoxb-123456789012-abcdefghij", "Slack token"),
            // PEM / Bearer / URL 内嵌
            ("-----BEGIN RSA PRIVATE KEY-----\nMIIE", "PEM 私钥块"),
            ("-----BEGIN OPENSSH PRIVATE KEY-----", "OpenSSH 私钥块"),
            (
                "Authorization: Bearer eyJhbGciOiJIUzI1NiIsInR5cCI6",
                "Bearer 头",
            ),
            (
                "git clone https://alice:S3cretPw@github.com/x/y.git",
                "URL 内嵌密码",
            ),
            // 个人隐私
            ("联系 zhang.san@example.com 确认", "邮箱"),
            ("回电 13812345678 即可", "手机号"),
            ("身份证 110101199003074258", "身份证号"),
            ("卡号 6222 0210 0110 2345 678", "银行卡形态（4 位一组）"),
            ("卡号 6222021001102345", "银行卡形态（连续 16 位）"),
        ];
        for (text, why) in rejected {
            assert!(is_sensitive_content(text), "应判为敏感（{why}）：{text}");
        }
    }

    /// 反向判定表：这些是**正当内容**，不得因出现「token / secret / password」等概念被误拒。
    /// 此前按裸词匹配时，下面每一条都会被拒——而该判定守着所有技能写入。
    #[test]
    fn 敏感内容_正当内容不误拒() {
        let allowed = [
            (
                "控制 token 预算：单次请求不超过 8k，超出先压缩上下文。",
                "谈 token 预算",
            ),
            ("用 tokenizer 分词后再统计。", "tokenizer 子串"),
            ("max_tokens 与 max_completion_tokens 二选一。", "参数名"),
            (
                "不要把 secret 写进日志；密钥只从环境变量读取。",
                "安全规范本身",
            ),
            (
                "密码重置流程：先校验邮箱，再发送一次性链接。",
                "密码重置流程",
            ),
            (
                "password reset: verify email first, then send a one-time link.",
                "英文密码重置流程",
            ),
            (
                "api_key 从 `${OPENAI_API_KEY}` 读取，不要硬编码。",
                "变量引用",
            ),
            (
                "在请求头填 Authorization: Bearer <your-token>。",
                "尖括号占位",
            ),
            ("token = 你的访问令牌", "中文说明作值"),
            ("密码至少 8 位，包含字母和数字。", "口令强度要求"),
            ("版本号 1.21.300，构建于 2026-11-23。", "普通数字"),
            ("任务 #12345678 已完成", "8 位数字（非手机号形态）"),
            ("遇到 401 时刷新令牌后重试。", "HTTP 状态码"),
            ("流程见 https://example.com/docs", "普通 URL"),
            ("联系维护者确认发布窗口。", "无任何敏感形态"),
            // 卡号规则放宽（允许空格/短横线分组）后的误伤面：带空格的数字串必须仍安全
            ("迭代 1500 2000 2500 3000 次", "空格分隔的四个数"),
            (
                "时间戳 1700000000 1700000001 1700000002",
                "空格分隔的时间戳",
            ),
            ("2024-01-15 2024-02-20 2024-03-25", "日期列表"),
            ("坐标 116.397 39.908 121.473 31.230", "坐标"),
            ("端口 8080 8443 9000 9090 3000 5000", "端口清单"),
        ];
        for (text, why) in allowed {
            assert!(!is_sensitive_content(text), "不应判为敏感（{why}）：{text}");
        }
    }

    #[test]
    fn 非法范围与空触发_拒() {
        let fx = fixture("scope", true);
        let ev = vec![(CandidateEvidenceKind::ToolReceipt, "call-ok-ls".into())];
        let empty: Vec<String> = vec![];
        let d = draft(&fx, "内容", "everywhere", &empty, &ev);
        let book = CandidateBook::new(&fx.store);
        let err = book.submit(Some(&fx.workspace), &d).unwrap_err();
        assert!(err.iter().any(|r| r.contains("scope")), "{err:?}");
        assert!(err.iter().any(|r| r.contains("触发词")), "{err:?}");
    }

    #[test]
    fn 人工裁决_一次定局() {
        let fx = fixture("decide", true);
        let triggers = vec!["构建".to_string()];
        let ev = vec![(CandidateEvidenceKind::ToolReceipt, "call-ok-ls".into())];
        let d = draft(&fx, "构建走 release 通道", "group", &triggers, &ev);
        let book = CandidateBook::new(&fx.store);
        let cand = book.submit(Some(&fx.workspace), &d).unwrap();
        let rejected = book
            .decide(&cand.id, false, "user-1", Some("太局部，不值得沉淀"))
            .unwrap();
        assert_eq!(rejected.status, "rejected");
        assert_eq!(
            rejected.reject_reason.as_deref(),
            Some("太局部，不值得沉淀")
        );
        let err = book.decide(&cand.id, true, "user-1", None).unwrap_err();
        assert!(err.contains("不可重复"), "{err}");
        // 被拒候选不参与重复判定：同触发词可重新提交
        let cand2 = book.submit(Some(&fx.workspace), &d).unwrap();
        let approved = book.decide(&cand2.id, true, "user-1", None).unwrap();
        assert_eq!(approved.status, "approved");
        assert_eq!(approved.decided_by.as_deref(), Some("user-1"));
    }
}
