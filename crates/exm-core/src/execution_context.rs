//! 任务级执行上下文。
//!
//! `Session` 是用户交互和持久化历史的归属；执行上下文则描述一次实际运行。
//! 两者不能再通过一个字符串同时承担预算、trace、审批来源和资源所有者的职责。
//! 本模块先提供可持久化的契约对象，运行时取消令牌和具体进程句柄仍由各执行器持有。

use crate::types::{new_id, ToolScope};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Component, Path};

pub const EXECUTION_CONTEXT_SCHEMA_VERSION: u16 = 1;
const DEFAULT_LEGACY_MAX_STEPS: u32 = 0;
const DEFAULT_MAX_DELEGATION_DEPTH: u8 = 1;

/// 未显式声明子任务工具时只允许的只读工具。
pub const DEFAULT_CHILD_READ_ONLY_TOOLS: &[&str] = &["read", "grep", "glob"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum ExecutionKind {
    #[default]
    Legacy,
    Root,
    Child,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum ContextSource {
    #[default]
    Legacy,
    Current,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceBoundary {
    /// 已批准项目目录；缺省表示没有项目作用域，不能被解释为任意路径。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_root: Option<String>,
    /// 用户级依赖和工具缓存目录，与项目和控制数据分离。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_root: Option<String>,
    /// 网关控制数据目录；Agent 不因知道该路径而获得写权限。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state_root: Option<String>,
}

impl WorkspaceBoundary {
    pub fn project(path: impl Into<String>) -> Self {
        Self {
            project_root: Some(path.into()),
            ..Self::default()
        }
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        for (name, raw) in [
            ("projectRoot", self.project_root.as_deref()),
            ("cacheRoot", self.cache_root.as_deref()),
            ("stateRoot", self.state_root.as_deref()),
        ] {
            let Some(raw) = raw else { continue };
            let path = Path::new(raw);
            anyhow::ensure!(!raw.contains('\0'), "{name} 不能包含 NUL");
            anyhow::ensure!(path.is_absolute(), "{name} 必须是绝对路径");
            anyhow::ensure!(
                !path
                    .components()
                    .any(|component| matches!(component, Component::ParentDir)),
                "{name} 不得包含 .. 路径段"
            );
        }
        if let (Some(project), Some(cache)) = (&self.project_root, &self.cache_root) {
            anyhow::ensure!(project != cache, "projectRoot 与 cacheRoot 必须分离");
        }
        if let (Some(project), Some(state)) = (&self.project_root, &self.state_root) {
            anyhow::ensure!(project != state, "projectRoot 与 stateRoot 必须分离");
        }
        Ok(())
    }

    pub fn is_project_scoped(&self) -> bool {
        self.project_root.is_some()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionAuthorization {
    /// 通道/任务的能力档位；执行时仍须重新校验具体工具是否启用。
    #[serde(default)]
    pub scope: ToolScope,
    /// 内建工具和动态工具的名称交集，不是能力升级凭证。
    #[serde(default)]
    pub allowed_tools: BTreeSet<String>,
    /// 是否允许创建一层子任务。
    #[serde(default)]
    pub allow_delegation: bool,
    /// 是否允许当前执行声明外部副作用动作。
    #[serde(default)]
    pub allow_external_writes: bool,
    #[serde(default = "default_max_delegation_depth")]
    pub max_delegation_depth: u8,
}

fn default_max_delegation_depth() -> u8 {
    DEFAULT_MAX_DELEGATION_DEPTH
}

impl Default for ExecutionAuthorization {
    fn default() -> Self {
        Self {
            scope: ToolScope::Chat,
            allowed_tools: BTreeSet::new(),
            allow_delegation: false,
            allow_external_writes: false,
            max_delegation_depth: DEFAULT_MAX_DELEGATION_DEPTH,
        }
    }
}

impl ExecutionAuthorization {
    pub fn allows(&self, tool: &str) -> bool {
        let tool = tool.trim();
        if tool.is_empty() || !self.scope.allows_tools() {
            return false;
        }
        if tool == "spawn" && !self.allow_delegation {
            return false;
        }
        if tool == "computer" && !self.scope.allows_computer() {
            return false;
        }
        self.allowed_tools.contains(tool)
    }

    fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.max_delegation_depth <= DEFAULT_MAX_DELEGATION_DEPTH,
            "当前仅支持一层子任务委派"
        );
        if !self.scope.allows_tools() {
            anyhow::ensure!(self.allowed_tools.is_empty(), "Chat 作用域不得携带工具授权");
        }
        if !self.scope.allows_computer() {
            anyhow::ensure!(
                !self.allowed_tools.contains("computer"),
                "非 Computer 作用域不得携带 computer 授权"
            );
        }
        anyhow::ensure!(
            self.allowed_tools.iter().all(|tool| {
                !tool.is_empty() && tool.len() <= 128 && !tool.chars().any(char::is_whitespace)
            }),
            "工具授权名称无效"
        );
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CancellationBinding {
    /// 运行时取消状态的索引；不是用户可猜测的 taskId。
    #[serde(default)]
    pub token_id: String,
    /// 子任务是否继承父任务的取消边界。
    #[serde(default)]
    pub inherited: bool,
    #[serde(default)]
    pub explicitly_cancelled: bool,
}

impl Default for CancellationBinding {
    fn default() -> Self {
        Self {
            token_id: String::new(),
            inherited: false,
            explicitly_cancelled: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BudgetBinding {
    /// 根执行标识；子任务不能用新的 executionId 重置此字段。
    #[serde(default)]
    pub root_execution_id: String,
    /// 0 只有在 unlimited=true 时才表示不限；缺省上下文是 0 且不允许执行。
    #[serde(default)]
    pub max_steps: u32,
    #[serde(default)]
    pub consumed_steps: u32,
    #[serde(default)]
    pub unlimited: bool,
}

impl Default for BudgetBinding {
    fn default() -> Self {
        Self {
            root_execution_id: String::new(),
            max_steps: DEFAULT_LEGACY_MAX_STEPS,
            consumed_steps: 0,
            unlimited: false,
        }
    }
}

impl BudgetBinding {
    pub fn remaining(&self) -> Option<u32> {
        if self.unlimited {
            None
        } else {
            Some(self.max_steps.saturating_sub(self.consumed_steps))
        }
    }

    pub fn can_consume(&self, steps: u32) -> bool {
        self.unlimited
            || self
                .consumed_steps
                .checked_add(steps)
                .is_some_and(|total| total <= self.max_steps)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionContext {
    #[serde(default)]
    pub schema_version: u16,
    #[serde(default)]
    pub source: ContextSource,
    #[serde(default)]
    pub kind: ExecutionKind,
    /// 用户会话：交互归属，不是工具 trace 或子任务身份。
    #[serde(default)]
    pub session_id: String,
    /// 根执行：父级预算与授权的归属。
    #[serde(default)]
    pub root_execution_id: String,
    /// 当前执行：并发、资源和局部结果的归属。
    #[serde(default)]
    pub execution_id: String,
    /// 证据会话：工具事件和结果持久化的归属。
    #[serde(default)]
    pub evidence_session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_execution_id: Option<String>,
    #[serde(default)]
    pub delegation_depth: u8,
    #[serde(default)]
    pub workspace: WorkspaceBoundary,
    #[serde(default)]
    pub authorization: ExecutionAuthorization,
    #[serde(default)]
    pub cancellation: CancellationBinding,
    #[serde(default)]
    pub budget: BudgetBinding,
}

impl ExecutionContext {
    /// 创建一次新的根执行。`session_id` 只用于交互归属；所有其他身份均独立生成。
    pub fn root(
        session_id: impl Into<String>,
        workspace: WorkspaceBoundary,
        scope: ToolScope,
        allowed_tools: impl IntoIterator<Item = impl Into<String>>,
        allow_delegation: bool,
        allow_external_writes: bool,
        max_steps: Option<u32>,
    ) -> anyhow::Result<Self> {
        let root_execution_id = format!("exec-{}", new_id());
        let evidence_session_id = format!("evidence-{}", new_id());
        let cancellation_id = format!("cancel-{}", new_id());
        let context = Self {
            schema_version: EXECUTION_CONTEXT_SCHEMA_VERSION,
            source: ContextSource::Current,
            kind: ExecutionKind::Root,
            session_id: session_id.into(),
            root_execution_id: root_execution_id.clone(),
            execution_id: root_execution_id.clone(),
            evidence_session_id,
            parent_execution_id: None,
            delegation_depth: 0,
            workspace,
            authorization: ExecutionAuthorization {
                scope,
                allowed_tools: allowed_tools.into_iter().map(Into::into).collect(),
                allow_delegation,
                allow_external_writes,
                max_delegation_depth: DEFAULT_MAX_DELEGATION_DEPTH,
            },
            cancellation: CancellationBinding {
                token_id: cancellation_id,
                inherited: false,
                explicitly_cancelled: false,
            },
            budget: BudgetBinding {
                root_execution_id,
                max_steps: max_steps.unwrap_or_default(),
                consumed_steps: 0,
                unlimited: max_steps.is_none(),
            },
        };
        context.validate()?;
        Ok(context)
    }

    /// 按父授权与请求工具求交集；空请求只获得只读默认，不继承写权限或委派权限。
    pub fn child(
        &self,
        child_execution_id: impl Into<String>,
        evidence_session_id: impl Into<String>,
        requested_tools: &[String],
    ) -> anyhow::Result<Self> {
        self.ensure_current()?;
        anyhow::ensure!(
            self.authorization.allow_delegation,
            "当前执行未授权创建子任务"
        );
        anyhow::ensure!(
            self.delegation_depth < self.authorization.max_delegation_depth,
            "子任务委派深度已达上限"
        );
        let requested: BTreeSet<String> = if requested_tools.is_empty() {
            DEFAULT_CHILD_READ_ONLY_TOOLS
                .iter()
                .map(|tool| (*tool).to_string())
                .collect()
        } else {
            requested_tools
                .iter()
                .map(|tool| tool.trim().to_string())
                .collect()
        };
        let allowed_tools = self
            .authorization
            .allowed_tools
            .intersection(&requested)
            .filter(|tool| {
                tool.as_str() != "spawn"
                    && (tool.as_str() != "computer" || self.authorization.scope.allows_computer())
            })
            .cloned()
            .collect();
        let execution_id = child_execution_id.into();
        let context = Self {
            schema_version: EXECUTION_CONTEXT_SCHEMA_VERSION,
            source: ContextSource::Current,
            kind: ExecutionKind::Child,
            session_id: self.session_id.clone(),
            root_execution_id: self.root_execution_id.clone(),
            execution_id,
            evidence_session_id: evidence_session_id.into(),
            parent_execution_id: Some(self.execution_id.clone()),
            delegation_depth: self.delegation_depth + 1,
            workspace: self.workspace.clone(),
            authorization: ExecutionAuthorization {
                scope: self.authorization.scope,
                allowed_tools,
                allow_delegation: false,
                allow_external_writes: false,
                max_delegation_depth: self.authorization.max_delegation_depth,
            },
            cancellation: CancellationBinding {
                token_id: self.cancellation.token_id.clone(),
                inherited: true,
                explicitly_cancelled: false,
            },
            budget: self.budget.clone(),
        };
        context.validate()?;
        Ok(context)
    }

    /// 将旧的仅有 session/workspace 记录安全读入为“遗留、不可执行”上下文。
    pub fn from_legacy_value(value: &serde_json::Value) -> anyhow::Result<Self> {
        let session_id = value
            .get("sessionId")
            .or_else(|| value.get("session_id"))
            .and_then(|value| value.as_str())
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| anyhow::anyhow!("遗留执行记录缺少 sessionId"))?;
        let project_root = value
            .get("workspace")
            .or_else(|| value.get("workspaceRoot"))
            .or_else(|| value.get("workspace_root"))
            .and_then(|value| value.as_str())
            .map(str::to_string);
        let root_execution_id = format!("legacy:{session_id}");
        let context = Self {
            schema_version: 0,
            source: ContextSource::Legacy,
            kind: ExecutionKind::Legacy,
            session_id: session_id.to_string(),
            root_execution_id: root_execution_id.clone(),
            execution_id: root_execution_id.clone(),
            evidence_session_id: format!("legacy-evidence:{session_id}"),
            parent_execution_id: None,
            delegation_depth: 0,
            workspace: WorkspaceBoundary {
                project_root,
                ..WorkspaceBoundary::default()
            },
            authorization: ExecutionAuthorization::default(),
            cancellation: CancellationBinding::default(),
            budget: BudgetBinding {
                root_execution_id,
                ..BudgetBinding::default()
            },
        };
        context.validate()?;
        Ok(context)
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.schema_version <= EXECUTION_CONTEXT_SCHEMA_VERSION,
            "未知的执行上下文版本: {}",
            self.schema_version
        );
        for (name, value) in [
            ("sessionId", self.session_id.as_str()),
            ("rootExecutionId", self.root_execution_id.as_str()),
            ("executionId", self.execution_id.as_str()),
            ("evidenceSessionId", self.evidence_session_id.as_str()),
        ] {
            anyhow::ensure!(valid_id(value), "{name} 不能为空或包含控制字符");
        }
        anyhow::ensure!(
            self.budget.root_execution_id == self.root_execution_id,
            "budget.rootExecutionId 必须绑定 rootExecutionId"
        );
        anyhow::ensure!(
            self.budget.unlimited || self.budget.consumed_steps <= self.budget.max_steps,
            "预算已超过上限"
        );
        self.workspace.validate()?;
        self.authorization.validate()?;
        if self.kind == ExecutionKind::Child {
            anyhow::ensure!(
                self.parent_execution_id
                    .as_deref()
                    .is_some_and(|id| id != self.execution_id),
                "Child 执行必须保留不同的 parentExecutionId"
            );
            anyhow::ensure!(self.delegation_depth > 0, "Child 执行深度必须大于 0");
            anyhow::ensure!(self.cancellation.inherited, "Child 必须继承取消边界");
        }
        if self.source == ContextSource::Current {
            anyhow::ensure!(
                self.schema_version == EXECUTION_CONTEXT_SCHEMA_VERSION,
                "当前执行上下文版本不匹配"
            );
            anyhow::ensure!(
                valid_id(&self.cancellation.token_id),
                "当前执行缺少取消令牌归属"
            );
            anyhow::ensure!(
                self.execution_id != self.evidence_session_id,
                "执行身份与证据会话不得复用"
            );
        }
        Ok(())
    }

    pub fn ensure_current(&self) -> anyhow::Result<()> {
        self.validate()?;
        anyhow::ensure!(
            self.source == ContextSource::Current && self.kind != ExecutionKind::Legacy,
            "遗留执行上下文只能用于读取，不能启动新执行"
        );
        Ok(())
    }

    pub fn is_legacy(&self) -> bool {
        self.source == ContextSource::Legacy || self.kind == ExecutionKind::Legacy
    }

    pub fn allows_tool(&self, tool: &str) -> bool {
        !self.is_legacy() && self.authorization.allows(tool)
    }
}

fn valid_id(value: &str) -> bool {
    !value.trim().is_empty()
        && value.len() <= 256
        && !value.chars().any(char::is_control)
        && !value.chars().any(char::is_whitespace)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn root_context() -> ExecutionContext {
        ExecutionContext::root(
            "session-root",
            WorkspaceBoundary {
                project_root: Some("/tmp/opencode/project".into()),
                cache_root: Some("/tmp/opencode/cache".into()),
                state_root: Some("/tmp/opencode/state".into()),
            },
            ToolScope::Tools,
            ["read", "grep", "glob", "edit", "browser", "spawn"],
            true,
            true,
            Some(24),
        )
        .unwrap()
    }

    #[test]
    fn 根与子执行身份和证据身份彼此分离() {
        let root = root_context();
        let child = root
            .child(
                "exec-child",
                "evidence-child",
                &[
                    "edit".into(),
                    "browser".into(),
                    "computer".into(),
                    "spawn".into(),
                ],
            )
            .unwrap();
        assert_eq!(child.root_execution_id, root.root_execution_id);
        assert_ne!(child.execution_id, root.execution_id);
        assert_ne!(child.evidence_session_id, root.evidence_session_id);
        assert_eq!(
            child.parent_execution_id.as_deref(),
            Some(root.execution_id.as_str())
        );
        assert!(child.allows_tool("edit"));
        assert!(child.allows_tool("browser"));
        assert!(!child.allows_tool("computer"), "Tools 档不得恢复 computer");
        assert!(!child.allows_tool("spawn"), "子任务不得递归派生");
        assert_eq!(
            child.budget.root_execution_id,
            root.budget.root_execution_id
        );
        assert_eq!(child.cancellation.token_id, root.cancellation.token_id);
    }

    #[test]
    fn 空工具请求只获得父授权内的只读交集() {
        let root = root_context();
        let child = root.child("exec-read", "evidence-read", &[]).unwrap();
        assert!(child.allows_tool("read"));
        assert!(child.allows_tool("grep"));
        assert!(child.allows_tool("glob"));
        assert!(!child.allows_tool("edit"));
        assert!(!child.authorization.allow_external_writes);
        assert!(!child.authorization.allow_delegation);
    }

    #[test]
    fn 遗留记录回读为不可执行且不扩大权限() {
        let legacy = ExecutionContext::from_legacy_value(&json!({
            "sessionId": "old-session",
            "workspace": "/tmp/opencode/old-project"
        }))
        .unwrap();
        assert!(legacy.is_legacy());
        assert!(legacy.ensure_current().is_err());
        assert!(!legacy.allows_tool("read"));
        assert_eq!(legacy.authorization.scope, ToolScope::Chat);
        assert_eq!(legacy.budget.remaining(), Some(0));
        assert_eq!(
            legacy.workspace.project_root.as_deref(),
            Some("/tmp/opencode/old-project")
        );
    }

    #[test]
    fn 序列化和缺省字段读取不混用身份() {
        let root = root_context();
        let encoded = serde_json::to_value(&root).unwrap();
        let decoded: ExecutionContext = serde_json::from_value(encoded).unwrap();
        decoded.validate().unwrap();
        assert_eq!(decoded, root);

        let missing: ExecutionContext = serde_json::from_value(json!({
            "sessionId": "only-session"
        }))
        .unwrap();
        assert!(missing.validate().is_err());
        assert!(!missing.allows_tool("terminal"));
        assert_eq!(missing.authorization.scope, ToolScope::Chat);
        assert!(!missing.budget.unlimited);
    }
}
