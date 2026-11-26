//! 编程工作流状态：goal/spec 的会话级事实源。
//!
//! goal/spec 默认只写会话事件，跨进程重启可恢复；仅显式 spec_export 写项目文件，
//! 不触碰 OpenSpec 目录。plan/todo 仍由各自事实源管理。

use crate::types::{now_iso, Envelope};
use crate::Core;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpecExportReceipt {
    pub spec_id: String,
    pub revision: u64,
    /// 项目工作区内的相对路径；不返回未规范化的用户输入。
    pub path: String,
    pub redacted: bool,
    pub exported_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalStatus {
    Active,
    Paused,
    Completed,
    Blocked,
    Cleared,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoalState {
    pub id: String,
    pub revision: u64,
    pub objective: String,
    pub status: GoalStatus,
    #[serde(default)]
    pub acceptance: Vec<String>,
    #[serde(default)]
    pub verification: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_budget: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// 操作审计轨迹（operation@revision 简记；旧事件缺字段按空读取）
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub audit: Vec<String>,
    /// 自动继续 admission（capability-parity 4.2）：仅用户明确 start/resume 时
    /// 置 true；pause/block/complete/clear 置 false。未明确授权绝不自动续跑。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub auto_continue: bool,
    /// 自动继续已消耗轮数（有界；跨重启从事件恢复）
    #[serde(default)]
    pub continue_rounds_used: u64,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpecState {
    pub id: String,
    pub revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal_id: Option<String>,
    pub objective: String,
    #[serde(default)]
    pub scope: Vec<String>,
    #[serde(default)]
    pub constraints: Vec<String>,
    #[serde(default)]
    pub acceptance: Vec<String>,
    #[serde(default)]
    pub verification: Vec<String>,
    pub source: String,
    pub updated_at: String,
    /// 内容在进入会话事件前已脱敏；导出时仍须如实保留这一事实。
    #[serde(default)]
    pub redacted: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowRelation {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal: Option<GoalState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec: Option<SpecState>,
    pub plan_mode: bool,
    pub plan_items: usize,
    pub todo_present: bool,
}

impl Core {
    pub fn goal_get(&self, session_id: &str) -> anyhow::Result<Option<GoalState>> {
        latest_payload(self, session_id, "workflow.goal.changed")
    }

    pub fn goal_update(
        &self,
        session_id: &str,
        operation: &str,
        objective: Option<&str>,
        note: Option<&str>,
        verification: Option<&str>,
        expected_revision: Option<u64>,
    ) -> anyhow::Result<GoalState> {
        self.goal_update_scoped(
            session_id,
            operation,
            objective,
            note,
            verification,
            expected_revision,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn goal_continue_step(&self, session_id: &str) -> anyhow::Result<u64> {
        let state = self.goal_update(session_id, "continue_step", None, None, None, None)?;
        Ok(state.continue_rounds_used)
    }

    /// 带预算与审计来源的 goal 更新（capability-parity 4.1：预算/审计字段事实源）。
    /// `token_budget`：start/edit 可设置（None = 不变）；来源标记进 audit 轨迹。
    #[allow(clippy::too_many_arguments)]
    pub fn goal_update_scoped(
        &self,
        session_id: &str,
        operation: &str,
        objective: Option<&str>,
        note: Option<&str>,
        verification: Option<&str>,
        expected_revision: Option<u64>,
        token_budget: Option<u64>,
    ) -> anyhow::Result<GoalState> {
        let current = self.goal_get(session_id)?;
        if let (Some(expected), Some(current)) = (expected_revision, current.as_ref()) {
            if expected != current.revision {
                anyhow::bail!(
                    "goal revision 冲突：当前为 {}，请求为 {}",
                    current.revision,
                    expected
                );
            }
        } else if expected_revision.is_some() && current.is_none() {
            anyhow::bail!("goal 不存在，不能按 revision 更新");
        }
        let now = now_iso();
        let mut state = current.unwrap_or_else(|| GoalState {
            id: format!("goal-{}", &crate::types::new_id()[..8]),
            revision: 0,
            objective: String::new(),
            status: GoalStatus::Cleared,
            acceptance: Vec::new(),
            verification: Vec::new(),
            token_budget: None,
            note: None,
            audit: Vec::new(),
            auto_continue: false,
            continue_rounds_used: 0,
            created_at: now.clone(),
            updated_at: now.clone(),
        });
        let operation = operation.trim().to_ascii_lowercase();
        match operation.as_str() {
            "start" => {
                if !matches!(state.status, GoalStatus::Cleared) && state.revision > 0 {
                    anyhow::bail!("当前会话已有 goal；请使用 edit、pause 或 clear");
                }
                state.objective = required_text(objective, "goal objective")?;
                state.status = GoalStatus::Active;
                // 明确 start = 自动继续授权（4.2 admission）
                state.auto_continue = true;
                state.continue_rounds_used = 0;
            }
            "edit" => {
                if matches!(state.status, GoalStatus::Cleared) {
                    anyhow::bail!("goal 不存在；请先 start");
                }
                if let Some(objective) = objective.filter(|value| !value.trim().is_empty()) {
                    state.objective = objective.trim().to_string();
                }
                if state.status == GoalStatus::Completed {
                    anyhow::bail!("已完成 goal 不可直接 edit，请 clear 后重新 start");
                }
            }
            "pause" => {
                // 幂等（capability-parity 4.1）：已 paused 再 pause 是无操作成功，
                // 不报错不递增语义（重复请求幂等）；其他状态才要求 Active
                if state.status != GoalStatus::Paused {
                    require_status(&mut state, GoalStatus::Active, "pause")?.status =
                        GoalStatus::Paused
                }
                // 暂停即收回自动继续授权（4.2）
                state.auto_continue = false;
            }
            "resume" => {
                // 幂等：已 Active 再 resume 是无操作成功
                if state.status != GoalStatus::Active
                    && !matches!(state.status, GoalStatus::Paused | GoalStatus::Blocked)
                {
                    anyhow::bail!("只有 paused/blocked goal 可以 resume");
                }
                if matches!(state.status, GoalStatus::Paused | GoalStatus::Blocked) {
                    state.status = GoalStatus::Active;
                }
                // 明确 resume = 自动继续授权（4.2 admission）
                state.auto_continue = true;
            }
            "complete" => {
                // 幂等：已 Completed 再 complete 需要新证据（追加验证记录），
                // 无新证据时是无操作成功
                if state.status != GoalStatus::Completed {
                    let evidence = required_text(verification, "goal verification")?;
                    state.verification.push(evidence);
                    state.status = GoalStatus::Completed;
                } else if let Some(evidence) = verification.filter(|value| !value.trim().is_empty())
                {
                    state.verification.push(evidence.trim().to_string());
                }
                state.auto_continue = false;
            }
            "block" => {
                // 幂等：已 Blocked 再 block 是无操作成功
                if state.status != GoalStatus::Blocked {
                    if matches!(state.status, GoalStatus::Cleared) {
                        anyhow::bail!("goal 不存在；请先 start");
                    }
                    state.status = GoalStatus::Blocked;
                }
                state.auto_continue = false;
            }
            "clear" => {
                // 幂等：已 Cleared 再 clear 是无操作成功（不重建空 goal）。
                // clear 重置瞬态字段（预算/验证/note）——事实保留在事件审计轨迹
                state.status = GoalStatus::Cleared;
                state.note = Some("goal cleared".into());
                state.token_budget = None;
                state.verification.clear();
                state.auto_continue = false;
                state.continue_rounds_used = 0;
            }
            // 内部操作（goal 自动继续写回轮数，capability-parity 4.2）：
            // 不在公开命令目录；只累计 + 保留状态
            "continue_step" => {
                if !matches!(state.status, GoalStatus::Active) {
                    anyhow::bail!("只有 active goal 记录继续轮");
                }
                state.continue_rounds_used += 1;
            }
            _ => anyhow::bail!(
                "未知 goal 操作：{operation}（start/edit/pause/resume/complete/block/clear）"
            ),
        }
        // 预算（4.1）：start/edit 可设置；None = 不变；0 视为清除预算
        if let Some(budget) = token_budget {
            if !matches!(operation.as_str(), "start" | "edit") {
                anyhow::bail!("预算只能在 start/edit 时设置（当前操作 {operation}）");
            }
            state.token_budget = (budget > 0).then_some(budget);
        }
        if let Some(note) = note.filter(|value| !value.trim().is_empty()) {
            state.note = Some(note.trim().to_string());
        }
        if state.objective.trim().is_empty() && state.status != GoalStatus::Cleared {
            anyhow::bail!("goal objective 不能为空");
        }
        state.revision += 1;
        state.updated_at = now;
        // 审计轨迹（4.1）：每次操作留 operation@revision 简记（不含正文，防膨胀）
        state.audit.push(format!("{operation}@{}", state.revision));
        const AUDIT_CAP: usize = 50;
        if state.audit.len() > AUDIT_CAP {
            let drain = state.audit.len() - AUDIT_CAP;
            state.audit.drain(..drain);
        }
        append_workflow_event(
            self,
            session_id,
            "workflow.goal.changed",
            json!({"state": state}),
        )?;
        Ok(state)
    }

    pub fn spec_get(&self, session_id: &str) -> anyhow::Result<Option<SpecState>> {
        let events = self.store.list_events(session_id)?;
        for event in events.into_iter().rev() {
            match event
                .get("kind")
                .or_else(|| event.get("type"))
                .and_then(Value::as_str)
            {
                Some("workflow.spec.cleared") => return Ok(None),
                Some("workflow.spec.changed") => {
                    let payload = event.get("payload").cloned().unwrap_or(Value::Null);
                    if let Some(state) = payload.get("state") {
                        return Ok(Some(serde_json::from_value(state.clone())?));
                    }
                }
                _ => {}
            }
        }
        Ok(None)
    }

    pub fn spec_clear(
        &self,
        session_id: &str,
        expected_revision: Option<u64>,
    ) -> anyhow::Result<()> {
        let current = self.spec_get(session_id)?;
        let Some(current) = current else {
            anyhow::ensure!(
                expected_revision.is_none(),
                "spec 不存在，不能按 revision 清除"
            );
            return Ok(());
        };
        if expected_revision.is_some_and(|revision| revision != current.revision) {
            anyhow::bail!("spec revision 冲突：当前为 {}", current.revision);
        }
        append_workflow_event(
            self,
            session_id,
            "workflow.spec.cleared",
            json!({"id": current.id, "revision": current.revision + 1, "updatedAt": now_iso()}),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn spec_update(
        &self,
        session_id: &str,
        objective: &str,
        scope: Vec<String>,
        constraints: Vec<String>,
        acceptance: Vec<String>,
        verification: Vec<String>,
        expected_revision: Option<u64>,
    ) -> anyhow::Result<SpecState> {
        anyhow::ensure!(self.store.get_session(session_id)?.is_some(), "会话不存在");
        let current = self.spec_get(session_id)?;
        if let (Some(expected), Some(current)) = (expected_revision, current.as_ref()) {
            if expected != current.revision {
                anyhow::bail!(
                    "spec revision 冲突：当前为 {}，请求为 {}",
                    current.revision,
                    expected
                );
            }
        } else if expected_revision.is_some() {
            anyhow::bail!("spec 不存在，不能按 revision 更新");
        }
        let mut state = SpecState {
            id: current
                .as_ref()
                .map(|state| state.id.clone())
                .unwrap_or_else(|| format!("spec-{}", &crate::types::new_id()[..8])),
            revision: current
                .as_ref()
                .map(|state| state.revision + 1)
                .unwrap_or(1),
            goal_id: self.goal_get(session_id)?.map(|goal| goal.id),
            objective: required_text(Some(objective), "spec objective")?,
            scope,
            constraints,
            acceptance,
            verification,
            source: "session_command".into(),
            updated_at: now_iso(),
            redacted: false,
        };
        // 会话事件同时是恢复事实源。先净化返回值与持久化状态，再记录脱敏
        // 来源，避免导出把已掩码的内容误报为从未脱敏。
        let original = serde_json::to_value(&state)?;
        let safe = crate::tools::redact_observation(&original);
        if safe != original {
            state = serde_json::from_value(safe)?;
            state.redacted = true;
        }
        append_workflow_event(
            self,
            session_id,
            "workflow.spec.changed",
            json!({"state": state}),
        )?;
        Ok(state)
    }

    /// 显式把当前会话 spec 导出为项目内 Markdown 文件。
    /// 普通 spec 草拟/修订只写会话事件；导出拒绝绝对路径、穿越、OpenSpec
    /// 与内部元数据目录，并在文件写入前校验工作区边界和敏感字段。
    pub fn spec_export(
        &self,
        session_id: &str,
        relative_path: &str,
    ) -> anyhow::Result<SpecExportReceipt> {
        let session = self
            .store
            .get_session(session_id)?
            .ok_or_else(|| anyhow::anyhow!("会话不存在: {session_id}"))?;
        let spec = self
            .spec_get(session_id)?
            .ok_or_else(|| anyhow::anyhow!("当前会话没有可导出的 spec"))?;
        let relative = validate_spec_export_path(relative_path)?;
        check_spec_export_directories(&relative)?;

        let (content, redacted) = render_spec_markdown(&spec)?;
        let tools = self.orchestrator().tools.clone();
        let requested_root = tools.workspace_for_session(session_id);
        let root = requested_root
            .canonicalize()
            .map_err(|e| anyhow::anyhow!("会话项目工作区不可用: {e}"))?;
        if let Some(pinned) = session.workspace.as_deref() {
            let pinned = crate::workspace::pinned(pinned)
                .map_err(|e| anyhow::anyhow!("会话项目工作区不可用: {e}"))?;
            anyhow::ensure!(root == pinned, "当前项目与会话绑定项目不一致");
        }
        let target = root.join(&relative);
        let parent = target
            .parent()
            .ok_or_else(|| anyhow::anyhow!("导出目标缺少父目录"))?;

        // 创建目录之前先解析最近存在的祖先；创建之后再校验规范化父目录，
        // 阻止现有符号链接把导出带出批准项目。
        let mut ancestor = parent;
        while !ancestor.exists() {
            ancestor = ancestor
                .parent()
                .ok_or_else(|| anyhow::anyhow!("导出父目录不可解析"))?;
        }
        let ancestor = ancestor.canonicalize()?;
        anyhow::ensure!(ancestor.starts_with(&root), "导出路径越界");
        check_spec_export_directories(ancestor.strip_prefix(&root)?)?;
        std::fs::create_dir_all(parent)?;
        let canonical_parent = parent.canonicalize()?;
        anyhow::ensure!(canonical_parent.starts_with(&root), "导出路径越界");
        check_spec_export_directories(canonical_parent.strip_prefix(&root)?)?;
        let file_name = target
            .file_name()
            .ok_or_else(|| anyhow::anyhow!("导出文件名无效"))?;
        let target = canonical_parent.join(file_name);
        if let Ok(metadata) = std::fs::symlink_metadata(&target) {
            anyhow::ensure!(
                !metadata.file_type().is_symlink(),
                "拒绝覆盖符号链接导出目标"
            );
            anyhow::ensure!(metadata.is_file(), "导出目标不是普通文件");
            anyhow::ensure!(target.canonicalize()?.starts_with(&root), "导出路径越界");
        }

        // 复用现有写前检查点；检查点失败时中止覆盖，避免 spec 导出破坏旧文件。
        tools.checkpoint_at_root(&root, &target, session_id)?;
        write_spec_export(&target, content.as_bytes())?;

        let receipt = SpecExportReceipt {
            spec_id: spec.id,
            revision: spec.revision,
            path: relative.to_string_lossy().replace('\\', "/"),
            redacted,
            exported_at: now_iso(),
        };
        append_workflow_event(
            self,
            session_id,
            "workflow.spec.exported",
            json!({ "receipt": receipt }),
        )?;
        Ok(receipt)
    }

    pub fn workflow_relation(&self, session_id: &str) -> anyhow::Result<WorkflowRelation> {
        Ok(WorkflowRelation {
            goal: self.goal_get(session_id)?,
            spec: self.spec_get(session_id)?,
            plan_mode: crate::plan::is_plan_mode(session_id),
            plan_items: crate::plan::list_items(session_id).len(),
            todo_present: !crate::todo::list_items(session_id).is_empty(),
        })
    }

    /// 本会话的文件来源根：项目层 = 会话绑定的项目（无绑定则没有项目层——默认根
    /// 不是用户批准的项目，不按项目层读它的 AGENTS.md）；用户层 = ~/.exmachina。
    fn context_roots(
        &self,
        session: &crate::types::Session,
    ) -> (Option<std::path::PathBuf>, Option<std::path::PathBuf>) {
        (
            session
                .workspace
                .as_deref()
                .and_then(|raw| crate::workspace::pinned(raw).ok()),
            crate::context_files::user_context_root(),
        )
    }

    /// 整层缺席的文件来源及其**真实**原因（capability-parity 6.3「缺失原因」）。
    /// 与 [`Self::context_roots`] 同一套判定，但保留「为什么没有根」：
    /// - 项目层：会话未绑定项目 ｜ 已绑定但项目目录不可用（`pinned` 的原话——
    ///   目录被删 / 被符号链接替换等）。两者是不同的事实，不能都写成「未绑定」。
    /// - 用户层：`~/.exmachina` 目录不存在（原因里带上它本来应在的路径）。
    ///
    /// 只是说明，不是来源：不在 [`Core::context_sources`] 的清单里（没有根就没有
    /// 注入，没有可停用 / 可刷新的东西，也不该出现在禁用键集合里）。
    pub fn context_source_gaps(&self, session_id: &str) -> anyhow::Result<Vec<serde_json::Value>> {
        let session = self.session_or_missing(session_id)?;
        let project_absent = match session.workspace.as_deref().map(str::trim) {
            None | Some("") => Some("会话未绑定项目".to_string()),
            Some(raw) => match crate::workspace::pinned(raw) {
                Ok(_) => None,
                Err(e) => Some(format!("已绑定 {raw}，但项目目录不可用：{e}")),
            },
        };
        let user_absent = if crate::context_files::user_context_root().is_some() {
            None
        } else {
            let home = std::env::var("HOME")
                .or_else(|_| std::env::var("USERPROFILE"))
                .unwrap_or_else(|_| "~".into());
            Some(format!("目录 {home}/.exmachina 不存在"))
        };
        Ok(crate::context_files::source_gaps(
            project_absent,
            user_absent,
        ))
    }

    /// 三个运行时来源的一次观测（原因按各自事实源给出，不是说明文字）。
    fn runtime_context_sources(
        &self,
        session: &crate::types::Session,
    ) -> (
        crate::context_files::RuntimeSource,
        crate::context_files::RuntimeSource,
        crate::context_files::RuntimeSource,
    ) {
        use crate::context_files::RuntimeSource;
        // 记忆：以**注入链实际读的那个开关**为准——`Orchestrator::memory_enabled`
        // （构造时从配置拷贝）。不能读 `config`：配置热更新后两者会分叉，清单
        // 按 config 汇报就会与「实际注不注入」不一致。关着时说明「未启用」；
        // 开着但库读不出来时说明读失败——两种都不是「就绪」
        let memory = if !self.orchestrator().memory_enabled {
            RuntimeSource::missing("记忆服务未启用（运行时开关 memory_enabled=false）")
        } else {
            match self.memory_list(None, 1) {
                Ok(_) => RuntimeSource::ready(),
                Err(e) => RuntimeSource::missing(format!("记忆库读取失败：{e}")),
            }
        };
        // 检索：滚动摘要要等历史超过压缩阈值才会产生；最近对话窗口始终在场，
        // 但它是会话历史本身，不算独立来源（与 plan.rs 的闸同口径）
        let retrieval = if session
            .rolling_summary
            .as_deref()
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false)
        {
            RuntimeSource::ready()
        } else {
            RuntimeSource::missing("会话尚无滚动摘要（历史未超过压缩阈值，或尚未发生压缩）")
        };
        // 技能：只读加载，不触发目录迁移、不动使用统计
        let skills = match self.registry().load_skills_readonly() {
            Ok(sk) if !sk.is_empty() => RuntimeSource::ready(),
            Ok(_) => RuntimeSource::missing("技能目录为空（没有可注入的技能）"),
            Err(e) => RuntimeSource::missing(format!("技能目录读取失败：{e}")),
        };
        (memory, retrieval, skills)
    }

    /// 上下文来源清单（capability-parity 6.3，spec: context-files 来源可管理）：
    /// 文件/记忆/检索/技能四类来源的状态、禁用位与缺失原因。只读聚合——
    /// 事实源各自独立（文件发现 context_files、记忆服务、会话摘要、技能目录）。
    pub fn context_sources(&self, session_id: &str) -> anyhow::Result<Vec<serde_json::Value>> {
        let session = self.session_or_missing(session_id)?;
        let (workspace, user_root) = self.context_roots(&session);
        let disabled = self.store.disabled_context_sources(session_id);
        let (memory, retrieval, skills) = self.runtime_context_sources(&session);
        Ok(crate::context_files::source_inventory(
            workspace.as_deref(),
            user_root.as_deref(),
            &disabled,
            &memory,
            &retrieval,
            &skills,
        ))
    }

    fn session_or_missing(&self, session_id: &str) -> anyhow::Result<crate::types::Session> {
        self.store
            .get_session(session_id)?
            .ok_or_else(|| anyhow::anyhow!("会话不存在: {session_id}"))
    }

    fn context_sources_keys(&self, session: &crate::types::Session) -> Vec<String> {
        let (workspace, user_root) = self.context_roots(session);
        crate::context_files::source_keys(workspace.as_deref(), user_root.as_deref())
    }

    /// 上下文来源禁用/启用（capability-parity 6.3）：key = file:<name> /
    /// memory / retrieval / skills；disable=true 加入清单，false 移除。
    /// 返回更新后的清单。持久化 + 事件留痕（context.sources.updated）。
    ///
    /// 未知键报错——不把不存在的「来源」记成已禁用。文件来源的键只在该会话
    /// **有对应的根**时才存在（项目文件要有绑定项目；用户文件要有 ~/.exmachina）；
    /// 没有根就没有注入，也就没有可停用的东西——报错里说明这一点。
    ///
    /// 校验只针对 `disable`。`enable` 不校验键是否已知：清单是持久化的，而
    /// 「哪些键算已知」取决于当下的根与约定文件名集合——这些都会变。不变量是
    /// 清单里的任何一项都必须能被移除，否则它会永远留在那里清不掉。
    pub fn context_source_set(
        &self,
        session_id: &str,
        key: &str,
        disable: bool,
    ) -> anyhow::Result<Vec<String>> {
        let session = self.session_or_missing(session_id)?;
        let key = key.trim();
        let known = self.context_sources_keys(&session);
        if disable && !known.iter().any(|k| k == key) {
            let hint = match crate::context_files::parse_source_key(key) {
                Some((crate::context_files::SourceKind::File, _))
                    if !known.iter().any(|k| k.starts_with("file:")) =>
                {
                    "（文件来源要先有根：会话未绑定项目、且没有 ~/.exmachina，当前没有任何文件来源会注入）"
                }
                Some((crate::context_files::SourceKind::File, _)) => {
                    "（文件来源只认约定文件名：AGENTS.md / .exmachina/context.md / context.md）"
                }
                _ => "",
            };
            anyhow::bail!("未知上下文来源：{key}{hint}（可用：{}）", known.join(" / "));
        }
        if key.is_empty() {
            anyhow::bail!("缺少来源 key（file:<name> / memory / retrieval / skills）");
        }
        let mut disabled = self.store.disabled_context_sources(session_id);
        if disable {
            if !disabled.iter().any(|k| k == key) {
                disabled.push(key.to_string());
            }
        } else {
            disabled.retain(|k| k != key);
        }
        self.store
            .set_disabled_context_sources(session_id, &disabled)?;
        self.emit_session_event(
            session_id,
            "context.sources.updated",
            serde_json::json!({ "key": key, "disabled": disable, "sources": disabled }),
        );
        Ok(disabled)
    }

    /// 单来源刷新（capability-parity 6.3 `/context refresh <key>`）：只重新观测
    /// **这一个**来源并回报。其它来源不受影响——这里既不写任何共享状态，也
    /// 不替别的来源重算；调用方拿到的 `others` 是刷新前其余来源的原样快照。
    ///
    /// 失败是真实的路径，且**只存在于文件来源**：重读被拒（读取失败 / 路径不合法）
    /// 时 `ok=false` 且带 `error`，但**不改**禁用清单、不抛错——「单来源刷新失败
    /// 不会改变其他来源」。记忆/检索/技能没有缓存状态，刷新只是重新观测一次，
    /// 不存在「刷新失败」这种状态。
    ///
    /// 返回 `{key, ok, error, source, others}`：`source` 是该来源刷新后的条目，
    /// `others` 是其余来源在本次刷新前的原样快照——调用方可以直接比对。
    pub fn context_source_refresh(
        &self,
        session_id: &str,
        key: &str,
    ) -> anyhow::Result<serde_json::Value> {
        let session = self.session_or_missing(session_id)?;
        let key = key.trim();
        let before = self.context_sources(session_id)?;
        if !before
            .iter()
            .any(|s| s.get("key").and_then(|v| v.as_str()) == Some(key))
        {
            anyhow::bail!("未知上下文来源：{key}");
        }
        let (workspace, user_root) = self.context_roots(&session);
        let (entry, error) = match crate::context_files::parse_source_key(key) {
            Some((crate::context_files::SourceKind::File, Some(file))) => {
                let fresh = crate::context_files::refresh_file_source(
                    workspace.as_deref(),
                    user_root.as_deref(),
                    &file,
                );
                let disabled = self.store.disabled_context_sources(session_id);
                let is_disabled = disabled.contains(&key.to_string());
                let mut entry =
                    serde_json::to_value(&fresh.status).unwrap_or(serde_json::Value::Null);
                if let serde_json::Value::Object(map) = &mut entry {
                    map.insert("kind".into(), serde_json::json!("file"));
                    map.insert("key".into(), serde_json::json!(key));
                    map.insert("disabled".into(), serde_json::json!(is_disabled));
                    map.insert("baseStatus".into(), serde_json::json!(fresh.status.status));
                    if is_disabled {
                        map.insert("status".into(), serde_json::json!("disabled"));
                    }
                    if fresh.status.status == "missing" && fresh.status.reason.is_none() {
                        map.insert(
                            "reason".into(),
                            serde_json::json!(format!(
                                "未找到 {}（{} 层）",
                                file,
                                crate::context_files::layer_name(fresh.status.layer)
                            )),
                        );
                    }
                }
                (entry, fresh.error)
            }
            // 运行时来源没有可缓存的旧状态，也就没有「刷新失败」可言：刷新 = 重新
            // 观测一次（只读）。失败路径只存在于文件来源（见上）。
            _ => {
                let entry = self
                    .context_sources(session_id)?
                    .into_iter()
                    .find(|s| s.get("key").and_then(|v| v.as_str()) == Some(key))
                    .unwrap_or(serde_json::Value::Null);
                (entry, None)
            }
        };
        let ok = error.is_none();
        let others: Vec<serde_json::Value> = before
            .iter()
            .filter(|s| s.get("key").and_then(|v| v.as_str()) != Some(key))
            .cloned()
            .collect();
        let report = serde_json::json!({
            "key": key,
            "ok": ok,
            "error": error,
            "source": entry,
            "others": others,
        });
        self.emit_session_event(
            session_id,
            "context.source.refreshed",
            serde_json::json!({ "key": key, "ok": ok, "error": error }),
        );
        Ok(report)
    }

    // ---------------- 学习候选治理（capability-parity 6.4） ----------------

    /// 候选清单（learning.list 面）：可按状态过滤。
    pub fn learning_candidates(
        &self,
        status: Option<&str>,
    ) -> anyhow::Result<Vec<crate::learning_candidates::LearningCandidate>> {
        Ok(crate::learning_candidates::CandidateBook::new(&self.store).list(status))
    }

    /// 提交学习候选（红线校验在 CandidateBook::submit——无真实证据/敏感/
    /// 重复一律拒）。成功发 learning.candidate.created 事件。
    pub fn learning_submit(
        &self,
        draft: &crate::learning_candidates::CandidateDraft<'_>,
    ) -> anyhow::Result<crate::learning_candidates::LearningCandidate> {
        let workspace = self
            .store
            .get_session(draft.source_session)?
            .and_then(|s| s.workspace)
            .and_then(|raw| crate::workspace::pinned(&raw).ok());
        let cand = crate::learning_candidates::CandidateBook::new(&self.store)
            .submit(workspace.as_deref(), draft)
            .map_err(|reasons| anyhow::anyhow!("候选被拒：{}", reasons.join("；")))?;
        self.emit_session_event(
            &cand.source_session,
            "learning.candidate.created",
            serde_json::json!({
                "id": cand.id, "target": cand.target, "title": cand.title,
                "scope": cand.scope, "confidence": cand.confidence,
            }),
        );
        Ok(cand)
    }

    /// 人工裁决（learning.decide 面）：approve/reject 一次定局 + 事件。
    pub fn learning_decide(
        &self,
        id: &str,
        approve: bool,
        decided_by: &str,
        reason: Option<&str>,
    ) -> anyhow::Result<crate::learning_candidates::LearningCandidate> {
        let book = crate::learning_candidates::CandidateBook::new(&self.store);
        let cand = book
            .decide(id, approve, decided_by, reason)
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        self.emit_session_event(
            &cand.source_session,
            if approve {
                "learning.candidate.approved"
            } else {
                "learning.candidate.rejected"
            },
            serde_json::json!({ "id": cand.id, "by": decided_by, "reason": reason }),
        );
        if !approve {
            return Ok(cand);
        }
        // 转正：已批准候选写入长期载体。裁决与转正分开——转正失败不回滚用户的裁决，
        // 但失败必须留痕（promotion.ok=false + 事件），不能让「已批准」看起来像「已生效」。
        let promotion = match self.promote_candidate(&cand) {
            Ok(p) => p,
            Err(e) => crate::learning_candidates::Promotion {
                ok: false,
                ref_id: String::new(),
                version: None,
                at: crate::types::now_iso(),
                error: Some(e.to_string()),
            },
        };
        let ok = promotion.ok;
        let ref_id = promotion.ref_id.clone();
        let version = promotion.version;
        let error = promotion.error.clone();
        let promotion_snapshot = promotion.clone();
        book.record_promotion(&cand.id, promotion)
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        self.emit_session_event(
            &cand.source_session,
            if ok {
                "learning.candidate.promoted"
            } else {
                "learning.candidate.promote_failed"
            },
            serde_json::json!({
                "id": cand.id, "target": cand.target, "refId": ref_id,
                "version": version, "by": decided_by, "error": error,
            }),
        );
        let mut cand = cand;
        cand.promotion = Some(promotion_snapshot);
        Ok(cand)
    }

    /// 把已批准候选写入长期载体：
    /// - skill：经 `write_skill_versioned`（同一套格式/大小/敏感/范围校验 + 旧版本归档），
    ///   scope=agent:<id> 落到 `agents`，触发词落到 `triggers`；
    /// - memory：经既有记忆存储（`memory_remember[_global]`，kind=Lesson，confidence=候选
    ///   置信度，source_ref 指向候选 id 以便追溯）。
    ///
    /// 转正前再过一次敏感内容判定——候选提交后、用户批准前，内容理论上可能已被改写。
    fn promote_candidate(
        &self,
        cand: &crate::learning_candidates::LearningCandidate,
    ) -> anyhow::Result<crate::learning_candidates::Promotion> {
        use crate::learning_candidates::Promotion;
        anyhow::ensure!(
            cand.status == "approved",
            "候选未获批准（{}），不得转正",
            cand.status
        );
        anyhow::ensure!(
            !crate::learning_candidates::is_sensitive_content(&cand.content),
            "内容含敏感信息形态，拒绝转正"
        );
        match cand.target.as_str() {
            "skill" => {
                let agents: Vec<String> = cand
                    .scope
                    .strip_prefix("agent:")
                    .map(|a| vec![a.to_string()])
                    .unwrap_or_default();
                let skill = crate::types::SkillDef {
                    id: cand.target_id.clone(),
                    name: cand.title.clone(),
                    description: cand.source_goal.chars().take(200).collect(),
                    triggers: cand.triggers.clone(),
                    instructions: cand.content.clone(),
                    agents,
                    source: "learning".into(),
                    ..Default::default()
                };
                let written = self.skill_write_versioned_in(&skill, &cand.source_session)?;
                Ok(Promotion {
                    ok: true,
                    ref_id: written.id.clone(),
                    version: Some(written.version),
                    at: crate::types::now_iso(),
                    error: None,
                })
            }
            "memory" => {
                let mut draft = crate::memory::MemoryDraft::new(
                    crate::memory::MemoryKind::Lesson,
                    cand.title.clone(),
                    cand.content.clone(),
                );
                draft.confidence = cand.confidence;
                draft.source_ref = Some(format!("learning-candidate:{}", cand.id));
                draft.tags = cand.triggers.clone();
                let entry = if cand.scope == "global" {
                    self.memory_remember_global(draft)?
                } else {
                    self.memory_remember(draft)?
                };
                Ok(Promotion {
                    ok: true,
                    ref_id: entry.id.clone(),
                    // 与技能分支同一口径：回执带实际生效的版本号。同内容去重命中已有条目时
                    // 返回的是那条已有条目，版本如实取它——不假设晋升一定产生 v1。
                    version: Some(entry.version_label()),
                    at: crate::types::now_iso(),
                    error: None,
                })
            }
            other => anyhow::bail!("未知转正目标：{other}"),
        }
    }

    // ---------------- 技能版本治理（capability-parity 6.5） ----------------

    /// 技能版本历史（旧→新，含当前版本）。
    pub fn skill_history(&self, id: &str) -> anyhow::Result<Vec<(u64, String)>> {
        self.registry.skill_history(id)
    }

    /// 技能版本历史（旧→新），每行带该版本的完整定义——治理面据此展示
    /// 「这一版写了什么」并选择回滚目标。
    pub fn skill_history_detailed(
        &self,
        id: &str,
    ) -> anyhow::Result<Vec<crate::registry::SkillHistoryRow>> {
        self.registry.skill_history_detailed(id)
    }

    /// 技能是否处于「已删除、历史保留」状态。
    pub fn skill_is_deleted(&self, id: &str) -> anyhow::Result<bool> {
        self.registry.skill_is_deleted(id)
    }

    /// 已删除（历史保留、可回滚恢复）的技能清单——治理面「回滚恢复」入口的数据源。
    pub fn list_deleted_skills(&self) -> anyhow::Result<Vec<crate::registry::DeletedSkill>> {
        self.registry.list_deleted_skills()
    }

    /// 技能回滚（capability-parity 6.5 用户撤销学习）：历史版本经同一校验
    /// 后成为新版本行（版本号单调递增，审计链不断）。事件留痕。
    pub fn skill_rollback(
        &self,
        id: &str,
        to_version: u64,
        by: &str,
    ) -> anyhow::Result<crate::types::SkillDef> {
        let rolled = self.registry.rollback_skill(id, to_version)?;
        self.emit_session_event(
            Self::GOVERNANCE_EVENTS,
            "skill.rolled_back",
            serde_json::json!({
                "id": rolled.id, "toVersion": to_version,
                "newVersion": rolled.version, "by": by,
            }),
        );
        Ok(rolled)
    }

    /// 技能版本化写入（治理面直写；模型自改进走 skill_manage 工具同一链路）。
    /// 治理事件流的会话键。无会话上下文的治理操作（REST/CLI 直接写入/回滚技能）
    /// 归到这条流——它与 `session.<id>.events` 同构、同信封，可经
    /// [`Core::governance_events`] 回读（审计入口），不是某个真实会话。
    pub const GOVERNANCE_EVENTS: &'static str = "global";

    /// 治理事件回读（最新在前）。
    pub fn governance_events(&self, limit: usize) -> anyhow::Result<Vec<serde_json::Value>> {
        let mut rows = self.store.list_events(Self::GOVERNANCE_EVENTS)?;
        rows.reverse();
        rows.truncate(limit);
        Ok(rows)
    }

    /// 版本化写入（治理面直写）；事件归治理流。
    pub fn skill_write_versioned(
        &self,
        skill: &crate::types::SkillDef,
    ) -> anyhow::Result<crate::types::SkillDef> {
        self.skill_write_versioned_in(skill, Self::GOVERNANCE_EVENTS)
    }

    /// 版本化写入，事件归属到 `session_id`（学习候选转正时用来源会话——用户在那里批准，
    /// 「已生效」的事件应当出现在那里；传 [`Core::GOVERNANCE_EVENTS`] 则归治理流）。
    pub fn skill_write_versioned_in(
        &self,
        skill: &crate::types::SkillDef,
        session_id: &str,
    ) -> anyhow::Result<crate::types::SkillDef> {
        let written = self.registry.write_skill_versioned(skill)?;
        self.emit_session_event(
            session_id,
            "skill.updated",
            serde_json::json!({
                "id": written.id, "version": written.version, "updatedAt": written.updated_at,
            }),
        );
        Ok(written)
    }
}

fn check_spec_export_directories(path: &Path) -> anyhow::Result<()> {
    let forbidden = path.components().any(|component| match component {
        Component::Normal(name) => matches!(
            name.to_string_lossy().to_ascii_lowercase().as_str(),
            "openspec" | ".exmachina" | ".git"
        ),
        _ => false,
    });
    anyhow::ensure!(
        !forbidden,
        "导出路径不能位于 openspec、.exmachina 或 .git 目录"
    );
    Ok(())
}

/// Sibling create-new + rename avoids truncating the previous file on a failed write
/// and does not modify an outside inode through an existing hard link.
fn write_spec_export(target: &Path, content: &[u8]) -> anyhow::Result<()> {
    use std::io::Write;
    let temporary = target.with_file_name(format!(".spec-export-{}.tmp", crate::types::new_id()));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    let result = (|| -> std::io::Result<()> {
        file.write_all(content)?;
        file.sync_all()?;
        drop(file);
        #[cfg(unix)]
        {
            std::fs::rename(&temporary, target)
        }
        #[cfg(not(unix))]
        {
            let backup =
                target.with_file_name(format!(".spec-export-backup-{}", crate::types::new_id()));
            let had_target = match std::fs::symlink_metadata(target) {
                Ok(_) => {
                    std::fs::rename(target, &backup)?;
                    true
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
                Err(error) => return Err(error),
            };
            match std::fs::rename(&temporary, target) {
                Ok(()) => {
                    if had_target {
                        std::fs::remove_file(backup)?;
                    }
                    Ok(())
                }
                Err(error) => {
                    if had_target {
                        let _ = std::fs::rename(&backup, target);
                    }
                    Err(error)
                }
            }
        }
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result.map_err(Into::into)
}

fn validate_spec_export_path(raw: &str) -> anyhow::Result<PathBuf> {
    let raw = raw.trim();
    anyhow::ensure!(!raw.is_empty(), "导出路径不能为空");
    let path = Path::new(raw);
    anyhow::ensure!(!path.is_absolute(), "导出路径必须是项目内相对路径");
    let mut clean = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => clean.push(part),
            Component::CurDir => anyhow::bail!("导出路径不能包含 `.` 路径段"),
            Component::ParentDir => anyhow::bail!("导出路径不能包含 `..` 路径段"),
            Component::RootDir | Component::Prefix(_) => {
                anyhow::bail!("导出路径必须是项目内相对路径")
            }
        }
    }
    anyhow::ensure!(clean.file_name().is_some(), "导出路径必须指定文件名");
    Ok(clean)
}

fn render_spec_markdown(spec: &SpecState) -> anyhow::Result<(String, bool)> {
    static SENSITIVE_ASSIGNMENT: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(
        || {
            regex::Regex::new(
                r#"(?i)(?:password|passwd|token|secret|api[_ -]?key|private[_ -]?key|authorization|cookie)\s*["']?\s*[:=]\s*["']?[^\s,;"'`]+"#,
            )
            .expect("static sensitive field regex")
        },
    );
    fn clean(value: &str, redacted: &mut bool) -> anyhow::Result<String> {
        anyhow::ensure!(
            !SENSITIVE_ASSIGNMENT.is_match(value),
            "spec 含疑似密码、token 或密钥字段，拒绝导出"
        );
        let safe = crate::tools::redact_str(value);
        *redacted |= safe != value;
        Ok(safe)
    }

    let mut redacted = spec.redacted;
    let spec_id = clean(&spec.id, &mut redacted)?;
    let goal_id = clean(spec.goal_id.as_deref().unwrap_or("未关联"), &mut redacted)?;
    let source = clean(&spec.source, &mut redacted)?;
    let updated_at = clean(&spec.updated_at, &mut redacted)?;
    let objective = clean(&spec.objective, &mut redacted)?;
    let mut lines = vec![
        "# 编程规格".to_string(),
        String::new(),
        format!("- Spec ID: `{spec_id}`"),
        format!("- Revision: {}", spec.revision),
        format!("- Goal ID: `{goal_id}`"),
        format!("- Source: `{source}`"),
        format!("- Updated: {updated_at}"),
        String::new(),
        "## Objective".to_string(),
        String::new(),
        objective,
    ];
    for (title, values) in [
        ("Scope", &spec.scope),
        ("Constraints", &spec.constraints),
        ("Acceptance", &spec.acceptance),
        ("Verification", &spec.verification),
    ] {
        lines.push(String::new());
        lines.push(format!("## {title}"));
        lines.push(String::new());
        if values.is_empty() {
            lines.push("- (none)".to_string());
        } else {
            for value in values {
                lines.push(format!("- {}", clean(value, &mut redacted)?));
            }
        }
    }
    let content = format!("{}\n", lines.join("\n"));
    anyhow::ensure!(content.len() <= 1_000_000, "spec 超过导出大小上限（1 MB）");
    Ok((content, redacted))
}

fn required_text(value: Option<&str>, name: &str) -> anyhow::Result<String> {
    let value = value.unwrap_or_default().trim();
    if value.is_empty() {
        anyhow::bail!("{name} 不能为空");
    }
    Ok(value.to_string())
}

fn require_status<'a>(
    state: &'a mut GoalState,
    expected: GoalStatus,
    operation: &str,
) -> anyhow::Result<&'a mut GoalState> {
    if state.status != expected {
        anyhow::bail!(
            "只有 {:?} goal 可以 {operation}，当前为 {:?}",
            expected,
            state.status
        );
    }
    Ok(state)
}

fn append_workflow_event(
    core: &Core,
    session_id: &str,
    kind: &str,
    payload: Value,
) -> anyhow::Result<()> {
    let envelope = Envelope::new(
        format!("session.{session_id}.workflow"),
        "command-service",
        "workflow",
        kind,
        session_id,
        payload,
    );
    core.store.append_event(&envelope)?;
    let _ = core.events.send(crate::types::CoreEvent {
        kind: kind.to_string(),
        session_id: session_id.to_string(),
        payload: envelope.payload.clone(),
    });
    Ok(())
}

fn latest_payload<T: for<'de> Deserialize<'de>>(
    core: &Core,
    session_id: &str,
    kind: &str,
) -> anyhow::Result<Option<T>> {
    let events = core.store.list_events(session_id)?;
    for event in events.into_iter().rev() {
        if event
            .get("kind")
            .or_else(|| event.get("type"))
            .and_then(Value::as_str)
            != Some(kind)
        {
            continue;
        }
        let payload = event.get("payload").cloned().unwrap_or(Value::Null);
        if let Some(state) = payload.get("state") {
            return Ok(Some(serde_json::from_value(state.clone())?));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ExmConfig;
    use std::sync::Arc;

    fn core() -> Arc<Core> {
        let root = std::env::temp_dir().join(format!("exm-workflow-{}", crate::types::new_id()));
        std::fs::create_dir_all(&root).unwrap();
        Arc::new(Core::with_config(ExmConfig::load(&root)).unwrap())
    }

    #[test]
    fn goal_state_machine_persists_and_checks_revision() {
        let core = core();
        let session = core.create_session("workflow").unwrap();
        let goal = core
            .goal_update(&session.id, "start", Some("修复测试"), None, None, None)
            .unwrap();
        assert_eq!(goal.status, GoalStatus::Active);
        assert_eq!(
            core.goal_get(&session.id).unwrap().unwrap().objective,
            "修复测试"
        );
        let paused = core
            .goal_update(&session.id, "pause", None, None, None, Some(goal.revision))
            .unwrap();
        assert_eq!(paused.status, GoalStatus::Paused);
        assert!(core
            .goal_update(&session.id, "resume", None, None, None, Some(goal.revision))
            .is_err());
        assert!(core
            .goal_update(
                &session.id,
                "resume",
                None,
                None,
                None,
                Some(paused.revision)
            )
            .is_ok());
    }

    #[test]
    fn spec_is_session_state_and_links_current_goal() {
        let core = core();
        let session = core.create_session("workflow").unwrap();
        let goal = core
            .goal_update(&session.id, "start", Some("目标"), None, None, None)
            .unwrap();
        let spec = core
            .spec_update(
                &session.id,
                "实现目标",
                vec!["src".into()],
                vec!["不修改生产配置".into()],
                vec!["cargo test".into()],
                vec!["测试通过".into()],
                None,
            )
            .unwrap();
        assert_eq!(spec.goal_id.as_deref(), Some(goal.id.as_str()));
        assert_eq!(core.spec_get(&session.id).unwrap().unwrap().revision, 1);
        assert!(core
            .store
            .list_events(&session.id)
            .unwrap()
            .iter()
            .any(|event| {
                event["kind"] == "workflow.spec.changed" || event["type"] == "workflow.spec.changed"
            }));
    }

    /// capability-parity 4.5：goal/spec/plan/todo 四者并存的关联视图——
    /// 「目标/规格/方案/执行进度」关系一次可见；修改 spec 不自动批准 plan；
    /// goal 完成必须有真实验证证据（空证据拒绝）。
    #[test]
    fn workflow_relation_shows_four_layers_and_keeps_boundaries() {
        let core = core();
        let session = core.create_session("workflow-relation").unwrap();
        let sid = &session.id;

        // 空态：relation 如实返回全空（不虚造）
        let empty = core.workflow_relation(sid).unwrap();
        assert!(empty.goal.is_none());
        assert!(empty.spec.is_none());
        assert!(!empty.plan_mode);
        assert_eq!(empty.plan_items, 0);
        assert!(!empty.todo_present);

        // 三层并存：goal + spec + plan 条目
        let goal = core
            .goal_update(sid, "start", Some("交付解析器"), None, None, None)
            .unwrap();
        let spec = core
            .spec_update(
                sid,
                "实现目标",
                vec!["src".into()],
                vec!["不修改生产配置".into()],
                vec!["cargo test".into()],
                vec!["测试通过".into()],
                None,
            )
            .unwrap();
        crate::plan::set_plan_mode(sid, true);
        crate::plan::push_item(
            sid,
            crate::plan::PlanItem {
                id: "rel-1".into(),
                action: "write".into(),
                tool: "fs_write".into(),
                target: "src/parser.rs".into(),
                args: serde_json::json!({ "path": "src/parser.rs", "content": "x" }),
                created_at: now_iso(),
            },
        );
        crate::todo::write(sid, vec![("步骤一".into(), "pending".into())]);

        let relation = core.workflow_relation(sid).unwrap();
        let g = relation.goal.as_ref().unwrap();
        assert_eq!(g.id, goal.id, "goal 关联：同 id");
        assert_eq!(g.revision, goal.revision, "goal 版本一致");
        let s = relation.spec.as_ref().unwrap();
        assert_eq!(s.id, spec.id, "spec 关联：同 id");
        assert_eq!(
            s.goal_id.as_deref(),
            Some(goal.id.as_str()),
            "spec → goal 关联可见"
        );
        assert!(relation.plan_mode, "plan 模式位在关系中");
        assert_eq!(relation.plan_items, 1, "plan 条目数在关系中");
        assert!(relation.todo_present, "todo 执行进度在关系中");

        // 边界一：修改 spec 不自动批准 plan——清单保留、模式位不动、
        // 无 plan.approved 事件（批准只能显式 approve）
        let _ = core
            .spec_update(
                sid,
                "实现目标（v2：加错误恢复）",
                vec!["src".into()],
                vec![],
                vec![],
                vec![],
                None,
            )
            .unwrap();
        let after = core.workflow_relation(sid).unwrap();
        assert!(after.plan_mode, "spec 修改后仍 plan 模式（不自动批准）");
        assert_eq!(after.plan_items, 1, "清单条目保留（不自动回放）");
        assert!(
            !core.store.list_events(sid).unwrap().iter().any(|event| {
                let kind = event["kind"].as_str().or(event["type"].as_str());
                kind == Some("plan.approved")
            }),
            "无 plan.approved 事件"
        );

        // 边界二：goal 完成必须有真实验证证据——空证据拒绝
        assert!(
            core.goal_update(sid, "complete", None, None, None, None)
                .is_err(),
            "complete 无验证证据必须拒绝"
        );
        let done = core
            .goal_update(
                sid,
                "complete",
                None,
                None,
                Some("cargo test 全绿（证据）"),
                None,
            )
            .unwrap();
        assert_eq!(done.status, GoalStatus::Completed);
        assert_eq!(done.verification.len(), 1, "证据入档");
        let g2 = core.workflow_relation(sid).unwrap().goal.unwrap();
        assert_eq!(g2.status, GoalStatus::Completed, "关联视图反映终态");

        // 清理（静态 plan/todo 状态不跨测试泄漏）
        crate::plan::set_plan_mode(sid, false);
        crate::plan::approve(sid);
        crate::todo::write(sid, vec![]);
    }

    #[test]
    fn spec_export_is_explicit_project_scoped_and_redacts_content() {
        let core = core();
        let session = core.create_session("workflow-export").unwrap();
        let root = core.config().workspace_root.clone();
        let spec = core
            .spec_update(
                &session.id,
                "实现安全导出 sk-abcdefgh12345678",
                vec!["src/".into()],
                vec!["不得写入 openspec".into()],
                vec!["导出文件存在".into()],
                vec!["读取文件并检查内容".into()],
                None,
            )
            .unwrap();
        assert!(spec.redacted);
        assert!(!spec.objective.contains("sk-abcdefgh12345678"));
        let saved = core.spec_get(&session.id).unwrap().unwrap();
        assert!(saved.redacted);
        assert_eq!(saved.objective, spec.objective);
        assert!(!root.join("docs/spec.md").exists());

        let receipt = core.spec_export(&session.id, "docs/spec.md").unwrap();
        assert_eq!(receipt.spec_id, spec.id);
        assert!(receipt.redacted);
        let content = std::fs::read_to_string(root.join("docs/spec.md")).unwrap();
        assert!(content.contains("## Scope"));
        assert!(content.contains("sk-***"));
        assert!(!content.contains("sk-abcdefgh12345678"));
        assert!(core
            .store
            .list_events(&session.id)
            .unwrap()
            .iter()
            .any(|event| {
                event
                    .get("kind")
                    .or_else(|| event.get("type"))
                    .and_then(Value::as_str)
                    == Some("workflow.spec.exported")
            }));

        std::fs::write(root.join("docs/spec.md"), "previous spec").unwrap();
        core.spec_export(&session.id, "docs/spec.md").unwrap();
        assert!(core
            .store
            .drain_checkpoint_refs(&session.id)
            .unwrap()
            .iter()
            .any(|reference| reference.ends_with("docs/spec.md")));

        assert!(core.spec_export(&session.id, "../outside.md").is_err());
        assert!(core.spec_export(&session.id, "openspec/change.md").is_err());
        assert!(core
            .spec_export(&session.id, root.join("absolute.md").to_str().unwrap())
            .is_err());

        #[cfg(unix)]
        {
            let outside =
                std::env::temp_dir().join(format!("exm-spec-outside-{}", crate::types::new_id()));
            std::fs::create_dir_all(&outside).unwrap();
            std::os::unix::fs::symlink(&outside, root.join("linked-outside")).unwrap();
            assert!(core
                .spec_export(&session.id, "linked-outside/leak.md")
                .is_err());
            assert!(!outside.join("leak.md").exists());
            std::fs::create_dir_all(root.join("openspec")).unwrap();
            std::os::unix::fs::symlink(root.join("openspec"), root.join("spec-alias")).unwrap();
            assert!(core
                .spec_export(&session.id, "spec-alias/not-allowed.md")
                .is_err());
            assert!(!root.join("openspec/not-allowed.md").exists());

            std::fs::write(outside.join("original.md"), "outside sentinel").unwrap();
            std::fs::hard_link(outside.join("original.md"), root.join("hardlink.md")).unwrap();
            core.spec_export(&session.id, "hardlink.md").unwrap();
            assert_eq!(
                std::fs::read_to_string(outside.join("original.md")).unwrap(),
                "outside sentinel"
            );
            let _ = std::fs::remove_dir_all(outside);
        }

        core.spec_update(
            &session.id,
            "token=do-not-export",
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Some(receipt.revision),
        )
        .unwrap();
        assert!(core.spec_export(&session.id, "docs/sensitive.md").is_err());
        assert!(!root.join("docs/sensitive.md").exists());
        let current = core.spec_get(&session.id).unwrap().unwrap();
        core.spec_clear(&session.id, Some(current.revision))
            .unwrap();
        assert!(core.spec_get(&session.id).unwrap().is_none());
        assert!(core
            .spec_update(
                &session.id,
                "stale revision",
                vec![],
                vec![],
                vec![],
                vec![],
                Some(current.revision)
            )
            .is_err());
        assert!(core
            .spec_clear(&session.id, Some(current.revision))
            .is_err());
    }

    #[tokio::test]
    async fn spec_export_uses_pinned_project_and_rejects_other_request_workspace() {
        let core = core();
        let root = core.config().workspace_root.clone();
        let project = root.join("pinned-project");
        let other = root.join("other-project");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        let session = core
            .store
            .create_session_in(
                "pinned-spec",
                &core.registry.active_scope(),
                Some(project.to_string_lossy().into_owned()),
            )
            .unwrap();
        assert_eq!(session.workspace.as_deref(), project.to_str());
        core.spec_update(
            &session.id,
            "pinned export",
            vec![],
            vec![],
            vec![],
            vec![],
            None,
        )
        .unwrap();
        let result = crate::workspace::scope(Some(other.clone()), async {
            core.spec_export(&session.id, "wrong.md")
        })
        .await;
        assert!(result.is_err());
        assert!(!other.join("wrong.md").exists());
        core.spec_export(&session.id, "correct.md").unwrap();
        assert!(project.join("correct.md").is_file());
        assert!(!root.join("correct.md").exists());
    }

    /// capability-parity 4.1：重复请求幂等（pause×2/resume×2/block×2/clear×2
    /// 均 Ok 且状态不变）；预算只能在 start/edit 设置；审计轨迹逐操作留痕；
    /// 旧会话（无 goal 事件）读取为 None 不报错。
    #[test]
    fn goal幂等预算审计与旧会话兼容() {
        let core = core();
        let session = core.create_session("goal-41").unwrap();
        let sid = &session.id;

        // 旧会话兼容：全新会话无 goal 事件 → None
        assert!(core.goal_get(sid).unwrap().is_none());

        // start 带预算
        let g = core
            .goal_update_scoped(
                sid,
                "start",
                Some("交付 v2 功能"),
                None,
                None,
                None,
                Some(50_000),
            )
            .unwrap();
        assert_eq!(g.token_budget, Some(50_000));
        assert_eq!(g.status, GoalStatus::Active);
        assert_eq!(g.audit.last().unwrap(), "start@1");

        // 幂等：pause ×2
        let p1 = core
            .goal_update(sid, "pause", None, None, None, None)
            .unwrap();
        assert_eq!(p1.status, GoalStatus::Paused);
        let p2 = core
            .goal_update(sid, "pause", None, None, None, None)
            .unwrap();
        assert_eq!(p2.status, GoalStatus::Paused, "重复 pause 幂等成功");
        assert_eq!(p2.revision, p1.revision + 1, "revision 仍递增（审计可见）");

        // 幂等：resume ×2（Active 再 resume Ok）
        core.goal_update(sid, "resume", None, None, None, None)
            .unwrap();
        let r2 = core
            .goal_update(sid, "resume", None, None, None, None)
            .unwrap();
        assert_eq!(r2.status, GoalStatus::Active, "重复 resume 幂等成功");

        // 预算只在 start/edit：pause 时设置被拒绝
        assert!(core
            .goal_update_scoped(sid, "pause", None, None, None, None, Some(100))
            .is_err());

        // edit 改预算
        let e = core
            .goal_update_scoped(
                sid,
                "edit",
                Some("交付 v2.1 功能"),
                None,
                None,
                None,
                Some(80_000),
            )
            .unwrap();
        assert_eq!(e.token_budget, Some(80_000));
        assert_eq!(e.objective, "交付 v2.1 功能");

        // 幂等：block ×2
        let b1 = core
            .goal_update(sid, "block", None, Some("外部依赖未就绪"), None, None)
            .unwrap();
        assert_eq!(b1.status, GoalStatus::Blocked);
        let b2 = core
            .goal_update(sid, "block", None, None, None, None)
            .unwrap();
        assert_eq!(b2.status, GoalStatus::Blocked, "重复 block 幂等成功");

        // 幂等：complete ×2（第二次无新证据也 Ok）
        core.goal_update(sid, "resume", None, None, None, None)
            .unwrap();
        let c1 = core
            .goal_update(
                sid,
                "complete",
                None,
                None,
                Some("验收：全部测试通过（mock 证据）"),
                None,
            )
            .unwrap();
        assert_eq!(c1.status, GoalStatus::Completed);
        assert_eq!(c1.verification.len(), 1);
        let c2 = core
            .goal_update(sid, "complete", None, None, None, None)
            .unwrap();
        assert_eq!(c2.status, GoalStatus::Completed, "重复 complete 幂等成功");
        assert_eq!(c2.verification.len(), 1, "无新证据不追加");

        // 幂等：clear ×2
        core.goal_update(sid, "clear", None, None, None, None)
            .unwrap();
        let cl2 = core
            .goal_update(sid, "clear", None, None, None, None)
            .unwrap();
        assert_eq!(cl2.status, GoalStatus::Cleared, "重复 clear 幂等成功");

        // 审计轨迹完整可回放（每次操作一条）
        let final_state = core.goal_get(sid).unwrap().unwrap();
        assert!(
            final_state.audit.len() >= 10,
            "审计轨迹：{:?}",
            final_state.audit
        );
        assert!(final_state.audit.iter().all(|a| a.contains('@')));

        // 最多一个 goal：clear 后可重新 start（同 id 槽位复用，预算/验证清零口径
        // 由状态机保证——新 goal 从 Active 起步）
        let g2 = core
            .goal_update(sid, "start", Some("新目标"), None, None, None)
            .unwrap();
        assert_eq!(g2.status, GoalStatus::Active, "clear 后重新 start 生效");
        assert!(g2.revision > cl2.revision, "revision 继续单调递增");
        assert_eq!(g2.token_budget, None, "clear 后预算清空");
    }

    // ---------------- 学习候选转正（capability-parity 6.4 → 6.5 衔接） ----------------

    mod promotion_tests {
        use super::*;
        use crate::learning_candidates::{CandidateDraft, CandidateEvidenceKind};

        /// 来源会话绑定工作区，本轮暂存里有一条成功的写回执——走真实证据路径。
        fn seeded(
            core: &Core,
            target: &str,
            target_id: Option<&str>,
            scope: &str,
            content: &str,
            triggers: &[&str],
        ) -> crate::learning_candidates::LearningCandidate {
            let ws = std::env::temp_dir().join(format!("exm-promo-{}", crate::types::new_id()));
            std::fs::create_dir_all(&ws).unwrap();
            std::fs::write(ws.join("ARTIFACT.md"), "x").unwrap();
            let session = core
                .store
                .create_session_in(
                    "promo",
                    &core.registry.active_group(),
                    Some(ws.to_string_lossy().into_owned()),
                )
                .unwrap();
            crate::round_trace::begin(&session.id);
            crate::round_trace::push_tool(
                &session.id,
                crate::round_trace::ToolCallRecord {
                    call_id: "w1".into(),
                    agent_id: "exmachina".into(),
                    tool: "filesystem".into(),
                    args: serde_json::json!({ "op": "write", "path": "ARTIFACT.md", "content": "x" }),
                    ok: true,
                    duration_ms: 1,
                    summary: String::new(),
                },
            );
            let triggers: Vec<String> = triggers.iter().map(|s| s.to_string()).collect();
            let evidence = vec![(CandidateEvidenceKind::ArtifactFile, "ARTIFACT.md".into())];
            let draft = CandidateDraft {
                target,
                target_id,
                title: "发布前跑全量测试",
                content,
                scope,
                triggers: &triggers,
                source_session: &session.id,
                source_goal: "修测试",
                evidence: &evidence,
            };
            let cand = core.learning_submit(&draft).expect("真实证据候选应过审");
            crate::round_trace::end(&session.id);
            cand
        }

        #[test]
        fn 批准前不得转正_批准后技能真实写入且版本可追溯() {
            let core = core();
            let cand = seeded(
                &core,
                "skill",
                Some("release-test"),
                "global",
                "发布前先跑 cargo test，全绿再合并。",
                &["发布"],
            );
            // 裁决前：状态 pending，技能目录无此技能
            assert_eq!(cand.status, "pending");
            assert!(core
                .skills()
                .unwrap()
                .iter()
                .all(|s| s.id != "release-test"));
            // 批准 → 转正：真实写入、版本 v1、来源标记 learning
            let approved = core
                .learning_decide(&cand.id, true, "user-1", None)
                .unwrap();
            assert_eq!(approved.status, "approved");
            let promo = approved.promotion.as_ref().expect("应记录转正结果");
            assert!(promo.ok, "转正应成功：{:?}", promo.error);
            assert_eq!(promo.ref_id, "release-test");
            assert_eq!(promo.version, Some(1));
            let skill = core
                .skills()
                .unwrap()
                .into_iter()
                .find(|s| s.id == "release-test")
                .expect("技能应已写入");
            assert_eq!(skill.version, 1);
            assert_eq!(skill.source, "learning");
            assert_eq!(skill.instructions, "发布前先跑 cargo test，全绿再合并。");
            assert_eq!(skill.triggers, vec!["发布".to_string()]);
            // 转正事件归属**来源会话**（用户在那里批准，「已生效」应出现在那里），不落伪会话
            let src = &approved.source_session;
            let kinds: Vec<String> = core
                .store
                .list_events(src)
                .unwrap()
                .iter()
                .filter_map(|e| e["payload"]["type"].as_str().map(str::to_string))
                .collect();
            assert!(
                kinds.contains(&"learning.candidate.approved".to_string()),
                "{kinds:?}"
            );
            assert!(
                kinds.contains(&"learning.candidate.promoted".to_string()),
                "{kinds:?}"
            );
            assert!(
                kinds.contains(&"skill.updated".to_string()),
                "skill.updated 应归来源会话：{kinds:?}"
            );
            // 幂等：重复批准被状态机挡住，不会二次写入
            assert!(core
                .learning_decide(&cand.id, true, "user-1", None)
                .is_err());
            assert_eq!(
                core.skills()
                    .unwrap()
                    .iter()
                    .filter(|s| s.id == "release-test")
                    .count(),
                1
            );
        }

        /// spec「用户撤销学习」：转正后再更新一版，回滚到转正版本 → 旧版本在后续读取生效。
        #[test]
        fn 转正后回滚_旧版本生效且审计链完整() {
            let core = core();
            let cand = seeded(
                &core,
                "skill",
                Some("undo-me"),
                "group",
                "第一版：先跑测试。",
                &["撤销"],
            );
            core.learning_decide(&cand.id, true, "user-1", None)
                .unwrap();
            // 之后又更新一版（治理面直写）
            let mut s2 = core
                .skills()
                .unwrap()
                .into_iter()
                .find(|s| s.id == "undo-me")
                .unwrap();
            s2.instructions = "第二版：跳过测试。".into();
            let v2 = core.skill_write_versioned(&s2).unwrap();
            assert_eq!(v2.version, 2);
            // 用户撤销：回滚到转正版本 v1
            let rolled = core.skill_rollback("undo-me", 1, "user-1").unwrap();
            assert_eq!(rolled.version, 3, "回滚以新版本行呈现，版本号不倒退");
            assert_eq!(rolled.instructions, "第一版：先跑测试。");
            // 后续读取生效
            let now = core
                .skills()
                .unwrap()
                .into_iter()
                .find(|s| s.id == "undo-me")
                .unwrap();
            assert_eq!(now.instructions, "第一版：先跑测试。");
            let hist = core.skill_history("undo-me").unwrap();
            assert_eq!(hist.len(), 3, "审计链完整：{hist:?}");
            // 回滚事件在治理流可回读
            let gov = core.governance_events(50).unwrap();
            assert!(
                gov.iter()
                    .any(|e| e["payload"]["type"] == "skill.rolled_back"),
                "治理事件应可回读：{gov:?}"
            );
        }

        #[test]
        fn 记忆候选_转正写入既有记忆存储() {
            let core = core();
            let cand = seeded(
                &core,
                "memory",
                None,
                "global",
                "这个项目的测试必须串行跑，并行会争抢端口。",
                &["测试"],
            );
            let approved = core
                .learning_decide(&cand.id, true, "user-1", None)
                .unwrap();
            let promo = approved.promotion.as_ref().unwrap();
            assert!(promo.ok, "{:?}", promo.error);
            // 写进了既有记忆存储（不另起炉灶），可经同一事实源读回
            let entry = core
                .memory
                .get_entry_public(&promo.ref_id)
                .unwrap()
                .expect("记忆条目应存在");
            assert_eq!(entry.title, "发布前跑全量测试");
            assert_eq!(entry.body, "这个项目的测试必须串行跑，并行会争抢端口。");
            assert_eq!(
                entry.source_ref.as_deref(),
                Some(format!("learning-candidate:{}", cand.id).as_str())
            );
            assert!(matches!(entry.kind, crate::memory::MemoryKind::Lesson));
            // capability-parity 6.5：记忆条目有版本号了，晋升回执与技能分支同一口径——
            // 带实际生效的版本（新条目 = v1），不再是 None。
            assert_eq!(promo.version, Some(1), "晋升回执带实际生效的版本号");
            assert_eq!(
                promo.version,
                Some(entry.version_label()),
                "回执版本与存储里的条目一致"
            );
            // 转正那一版记入版本历史
            let hist = core.memory_history(&entry.id).unwrap();
            assert_eq!(hist.len(), 1);
            assert_eq!((hist[0].version, hist[0].change.as_str()), (1, "created"));
        }

        /// 转正失败不回滚用户的裁决，但失败必须留痕——不能让「已批准」看起来像「已生效」。
        #[test]
        fn 转正失败_裁决保留_失败留痕() {
            let core = core();
            // 内容在提交后、批准前被改写为敏感形态（直接改账本模拟）
            let mut cand = seeded(
                &core,
                "skill",
                Some("leaky"),
                "global",
                "正常内容",
                &["部署"],
            );
            let mut all = core.store.list_learning_candidates();
            if let Some(row) = all.iter_mut().find(|c| c.id == cand.id) {
                row.content = "部署时用 API_KEY=sk-abcdefghijklmnopqr 直连".into();
                cand.content = row.content.clone();
                core.store.update_learning_candidate(row).unwrap();
            }
            let approved = core
                .learning_decide(&cand.id, true, "user-1", None)
                .unwrap();
            assert_eq!(
                approved.status, "approved",
                "裁决是用户的事实，不因转正失败而回滚"
            );
            let promo = approved.promotion.as_ref().unwrap();
            assert!(!promo.ok, "敏感内容不得转正");
            assert!(
                promo.error.as_deref().unwrap_or("").contains("敏感"),
                "{:?}",
                promo.error
            );
            assert!(
                core.skills().unwrap().iter().all(|s| s.id != "leaky"),
                "失败转正不得留下半成品"
            );
            let kinds: Vec<String> = core
                .store
                .list_events(&approved.source_session)
                .unwrap()
                .iter()
                .filter_map(|e| e["payload"]["type"].as_str().map(str::to_string))
                .collect();
            assert!(
                kinds.contains(&"learning.candidate.promote_failed".to_string()),
                "{kinds:?}"
            );
        }

        /// 技能候选必须指明技能 id——否则转正时无从落位；非法字符同样拒。
        #[test]
        fn 技能候选缺id或id非法_提交即拒() {
            let core = core();
            let ws =
                std::env::temp_dir().join(format!("exm-promo-noid-{}", crate::types::new_id()));
            std::fs::create_dir_all(&ws).unwrap();
            std::fs::write(ws.join("A.md"), "x").unwrap();
            let session = core
                .store
                .create_session_in(
                    "promo",
                    &core.registry.active_group(),
                    Some(ws.to_string_lossy().into_owned()),
                )
                .unwrap();
            crate::round_trace::begin(&session.id);
            crate::round_trace::push_tool(
                &session.id,
                crate::round_trace::ToolCallRecord {
                    call_id: "w1".into(),
                    agent_id: "exmachina".into(),
                    tool: "filesystem".into(),
                    args: serde_json::json!({ "op": "write", "path": "A.md", "content": "x" }),
                    ok: true,
                    duration_ms: 1,
                    summary: String::new(),
                },
            );
            let triggers = vec!["x".to_string()];
            let evidence = vec![(CandidateEvidenceKind::ArtifactFile, "A.md".into())];
            let submit = |target_id: Option<&str>| {
                let draft = CandidateDraft {
                    target: "skill",
                    target_id,
                    title: "t",
                    content: "没有 id 的技能候选",
                    scope: "global",
                    triggers: &triggers,
                    source_session: &session.id,
                    source_goal: "g",
                    evidence: &evidence,
                };
                core.learning_submit(&draft).map_err(|e| e.to_string())
            };
            // 证据是真实的——被拒只能是因为 id
            let err = submit(None as Option<&str>).unwrap_err();
            assert!(err.contains("id"), "缺 id 应被拒：{err}");
            let err = submit(Some("bad id/with slash")).unwrap_err();
            assert!(err.contains("非法"), "非法 id 应被拒：{err}");
            crate::round_trace::end(&session.id);
        }
    }

    /// capability-parity 6.3「用户可查看文件、记忆、检索、技能各来源及缺失原因，
    /// 单来源刷新失败不会改变其他来源」——用真实 Core 验证。
    mod context_source_tests {
        use super::*;

        fn core() -> Core {
            let dir = std::env::temp_dir().join(format!("exm-csrc-{}", crate::types::new_id()));
            std::fs::create_dir_all(&dir).unwrap();
            let mut cfg = crate::config::ExmConfig::load(&dir);
            cfg.use_mock = true;
            cfg.memory_enabled = false;
            Core::with_config(cfg).expect("创建 Core 失败")
        }

        /// 会话绑定到一个独立临时工作区（不写真实仓库根的约定文件）
        fn session_in(core: &Core, tag: &str) -> (crate::types::Session, std::path::PathBuf) {
            let ws =
                std::env::temp_dir().join(format!("exm-csrc-ws-{tag}-{}", crate::types::new_id()));
            std::fs::create_dir_all(&ws).unwrap();
            let session = core
                .store
                .create_session_in(
                    tag,
                    &core.registry.active_group(),
                    Some(ws.to_string_lossy().into_owned()),
                )
                .unwrap();
            (session, ws)
        }

        fn entry<'a>(sources: &'a [serde_json::Value], key: &str) -> &'a serde_json::Value {
            sources
                .iter()
                .find(|s| s["key"] == key)
                .unwrap_or_else(|| panic!("清单里没有 {key}：{sources:?}"))
        }

        /// 四类来源都在清单里；未就绪的带**真实**原因，就绪的没有原因字段值。
        #[test]
        fn 清单_四类来源与缺失原因() {
            let core = core();
            let (session, ws) = session_in(&core, "list");
            std::fs::write(ws.join("AGENTS.md"), "项目规则").unwrap();

            let sources = core.context_sources(&session.id).unwrap();
            for kind in ["file", "memory", "retrieval", "skills"] {
                assert!(
                    sources.iter().any(|s| s["kind"] == kind),
                    "清单缺 {kind} 类来源：{sources:?}"
                );
            }
            let agents = entry(&sources, "file:AGENTS.md");
            assert_eq!(agents["status"], "loaded");
            assert_eq!(agents["chars"], 4);
            assert!(agents["reason"].is_null());
            assert_eq!(agents["layer"], "project");

            // 文件不存在 → missing + 说清是哪个文件、哪一层
            let ctx = entry(&sources, "file:.exmachina/context.md");
            assert_eq!(ctx["status"], "missing");
            assert_eq!(ctx["reason"], "未找到 .exmachina/context.md（project 层）");

            // 记忆：闸是 memoryEnabled——关着就是「未启用」，不是笼统的依赖缺失
            let mem = entry(&sources, "memory");
            assert_eq!(mem["status"], "missing_dependency");
            assert_eq!(
                mem["reason"],
                "记忆服务未启用（运行时开关 memory_enabled=false）"
            );

            // 检索：新会话没有滚动摘要
            let retr = entry(&sources, "retrieval");
            assert_eq!(retr["status"], "missing_dependency");
            assert!(retr["reason"].as_str().unwrap().contains("滚动摘要"));

            // 技能：空目录是「目录为空」，不是读失败
            let sk = entry(&sources, "skills");
            assert_eq!(sk["status"], "missing_dependency");
            let r = sk["reason"].as_str().unwrap();
            assert!(r.contains("技能目录为空") || r.contains("读取失败"), "{r}");

            // 转为就绪要靠「注入链实际用的开关」：Orchestrator 构造时拷贝了配置，
            // 所以用一个开了记忆的 Core 验证，而不是改运行时配置
            let core_on = {
                let dir =
                    std::env::temp_dir().join(format!("exm-csrc-on-{}", crate::types::new_id()));
                std::fs::create_dir_all(&dir).unwrap();
                let mut cfg = crate::config::ExmConfig::load(&dir);
                cfg.use_mock = true;
                cfg.memory_enabled = true;
                Core::with_config(cfg).expect("创建 Core 失败")
            };
            let (session_on, ws_on) = session_in(&core_on, "ready");
            core_on
                .memory_remember(
                    crate::memory::MemoryDraft::new(
                        crate::memory::MemoryKind::Lesson,
                        "一条教训",
                        "正文",
                    )
                    .importance(1.0),
                )
                .expect("写入记忆");
            core_on
                .store
                .update_compaction(&session_on.id, "摘要", 0)
                .unwrap();
            let sources = core_on.context_sources(&session_on.id).unwrap();
            assert_eq!(entry(&sources, "memory")["status"], "ready");
            assert_eq!(entry(&sources, "retrieval")["status"], "ready");
            assert!(entry(&sources, "memory")["reason"].is_null());
            let _ = std::fs::remove_dir_all(ws_on);
            let _ = std::fs::remove_dir_all(ws);
        }

        /// 禁用是叠加位：status=disabled，原状态留在 baseStatus；重新启用回到原状态。
        /// 关掉一个来源，其余来源的条目逐字节不变。
        #[test]
        fn 禁用是叠加位_不影响其他来源() {
            let core = core();
            let (session, ws) = session_in(&core, "disable");
            std::fs::write(ws.join("AGENTS.md"), "项目规则").unwrap();
            let before = core.context_sources(&session.id).unwrap();

            core.context_source_set(&session.id, "file:AGENTS.md", true)
                .unwrap();
            let after = core.context_sources(&session.id).unwrap();
            let agents = entry(&after, "file:AGENTS.md");
            assert_eq!(agents["status"], "disabled");
            assert_eq!(agents["baseStatus"], "loaded");
            assert_eq!(agents["disabled"], true);
            // 其他来源逐项不变
            for s in &after {
                if s["key"] == "file:AGENTS.md" {
                    continue;
                }
                let was = entry(&before, s["key"].as_str().unwrap());
                assert_eq!(s, was, "{} 不该受 file:AGENTS.md 禁用影响", s["key"]);
            }

            core.context_source_set(&session.id, "file:AGENTS.md", false)
                .unwrap();
            let restored = core.context_sources(&session.id).unwrap();
            assert_eq!(entry(&restored, "file:AGENTS.md")["status"], "loaded");
            let _ = std::fs::remove_dir_all(ws);
        }

        /// 未知键报错，且**不**被记进禁用清单（否则回一句「已禁用」却什么都没禁）。
        #[test]
        fn 未知键_拒绝且不入清单() {
            let core = core();
            let (session, ws) = session_in(&core, "unknown");
            for bad in [
                "knowledge",
                "files",
                "",
                "file:README.md",
                "file:../etc/passwd",
                "MEMORY",
            ] {
                let err = core
                    .context_source_set(&session.id, bad, true)
                    .expect_err("未知键应被拒");
                assert!(err.to_string().contains("未知上下文来源"), "{bad}: {err}");
            }
            assert!(
                core.store.disabled_context_sources(&session.id).is_empty(),
                "被拒的键不得进入禁用清单"
            );
            // 报错里给出可用键与原因，用户不用猜
            let err = core
                .context_source_set(&session.id, "file:README.md", true)
                .unwrap_err()
                .to_string();
            assert!(err.contains("约定文件名"), "{err}");
            let _ = std::fs::remove_dir_all(ws);
        }

        /// 没有文件根的会话（未绑定项目）没有文件来源可停用——报错要说清原因，
        /// 而不是只列三个运行时来源让人以为文件来源被吞了。
        #[test]
        fn 无文件根_文件键被拒并说明原因() {
            let core = core();
            let session = core.create_session("nofile").unwrap();
            let err = core
                .context_source_set(&session.id, "file:AGENTS.md", true)
                .unwrap_err()
                .to_string();
            assert!(err.contains("未绑定项目"), "{err}");
            // 运行时来源照常可停用
            core.context_source_set(&session.id, "memory", true)
                .unwrap();
        }

        /// enable 不受「键当前是否已知」限制。清单是持久化的，而「哪些键算已知」
        /// 取决于当下的根（项目绑定 / ~/.exmachina）与约定文件名集合——这些都会变。
        /// 不变量是：**清单里的任何一项都必须能被移除**，否则它会永远留在那里，
        /// 且每次注入都真的被跳过，用户却无从解除。用一条未绑定项目的会话验证：
        /// 它没有任何文件来源键，但清单里若有残留的文件键，enable 仍能清掉。
        #[test]
        fn 启用_不受键是否仍已知限制_清单项总能解除() {
            let core = core();
            let session = core.create_session("unblock").unwrap();
            // 未绑定项目：没有任何文件来源键
            let keys: Vec<String> = core
                .context_sources(&session.id)
                .unwrap()
                .iter()
                .map(|s| s["key"].as_str().unwrap().to_string())
                .collect();
            assert!(keys.iter().all(|k| !k.starts_with("file:")), "{keys:?}");
            // 残留项（来自更早的、当时有文件根的状态）
            core.store
                .set_disabled_context_sources(&session.id, &["file:AGENTS.md".to_string()])
                .unwrap();

            core.context_source_set(&session.id, "file:AGENTS.md", false)
                .unwrap();
            assert!(
                core.store.disabled_context_sources(&session.id).is_empty(),
                "残留的禁用项必须能被解除"
            );
        }

        /// 验收句：「单来源刷新失败不会改变其他来源」。
        ///
        /// 失败走真实路径——约定文件位置上放一个**目录**，读取必然失败。
        /// 断言三件事：该来源 ok=false 且带原因；其余来源与刷新前逐项相同；
        /// 禁用清单没有被这次失败改动。
        #[test]
        fn 单来源刷新失败_不改变其他来源() {
            let core = core();
            let (session, ws) = session_in(&core, "refresh");
            std::fs::write(ws.join("AGENTS.md"), "规则").unwrap();
            std::fs::create_dir_all(ws.join(".exmachina")).unwrap();
            std::fs::write(ws.join(".exmachina/context.md"), "子规则").unwrap();
            core.context_source_set(&session.id, "skills", true)
                .unwrap();
            let before = core.context_sources(&session.id).unwrap();

            // 先正常刷新：看到最新内容
            std::fs::write(ws.join("AGENTS.md"), "规则改长了").unwrap();
            let ok = core
                .context_source_refresh(&session.id, "file:AGENTS.md")
                .unwrap();
            assert_eq!(ok["ok"], true);
            assert!(ok["error"].is_null());
            assert_eq!(ok["source"]["chars"], 5);

            // 制造失败：AGENTS.md 变成目录
            std::fs::remove_file(ws.join("AGENTS.md")).unwrap();
            std::fs::create_dir(ws.join("AGENTS.md")).unwrap();
            let failed = core
                .context_source_refresh(&session.id, "file:AGENTS.md")
                .unwrap();
            assert_eq!(failed["ok"], false, "读不出来必须如实报失败：{failed}");
            let err = failed["error"].as_str().expect("失败带原因");
            assert!(err.contains("读取失败"), "{err}");
            assert_eq!(failed["source"]["status"], "rejected");

            // 其余来源：刷新前快照 == 刷新后重读的清单（不只是「没报错」）
            let others: Vec<serde_json::Value> =
                failed["others"].as_array().expect("others 是数组").clone();
            let after = core.context_sources(&session.id).unwrap();
            for o in &others {
                let now = entry(&after, o["key"].as_str().unwrap());
                assert_eq!(o, now, "{} 被别的来源的刷新失败波及", o["key"]);
            }
            assert!(others.iter().all(|o| o["key"] != "file:AGENTS.md"));
            assert_eq!(others.len(), before.len() - 1);

            // 这次失败没有动禁用清单，也没有把该文件标成禁用
            assert_eq!(
                core.store.disabled_context_sources(&session.id),
                vec!["skills".to_string()]
            );
            assert_eq!(entry(&after, "file:AGENTS.md")["status"], "rejected");
            assert_eq!(entry(&after, "file:AGENTS.md")["disabled"], false);
            assert_eq!(entry(&after, "skills")["status"], "disabled");

            // 运行时来源刷新 = 重新观测，不会失败；禁用位如实带出
            let sk = core.context_source_refresh(&session.id, "skills").unwrap();
            assert_eq!(sk["ok"], true);
            assert_eq!(sk["source"]["status"], "disabled");
            assert_eq!(sk["source"]["baseStatus"], "missing_dependency");

            // 未知键的刷新报错，不留痕
            assert!(core.context_source_refresh(&session.id, "nope").is_err());
            let _ = std::fs::remove_dir_all(ws);
        }

        /// 刷新留痕：context.source.refreshed 事件带 key 与成败。
        #[test]
        fn 刷新_事件留痕() {
            let core = core();
            let (session, ws) = session_in(&core, "event");
            std::fs::create_dir(ws.join("AGENTS.md")).unwrap();
            core.context_source_refresh(&session.id, "file:AGENTS.md")
                .unwrap();
            let events = core
                .store
                .list_events(&session.id)
                .unwrap_or_default()
                .into_iter()
                .filter(|e| {
                    e.get("payload")
                        .and_then(|p| p.get("type"))
                        .and_then(|t| t.as_str())
                        == Some("context.source.refreshed")
                })
                .collect::<Vec<_>>();
            assert_eq!(events.len(), 1, "恰好一条刷新事件");
            let body = events[0]
                .get("payload")
                .and_then(|p| p.get("payload"))
                .expect("事件体");
            assert_eq!(body["key"], "file:AGENTS.md");
            assert_eq!(body["ok"], false);
            let _ = std::fs::remove_dir_all(ws);
        }

        /// 整层缺席要说明原因：未绑定项目的会话没有项目层文件来源——清单里
        /// 一个 `file:` 都没有，用户无从知道为什么。缺席说明补上这件事，
        /// 且它是**说明不是来源**：不进清单、不可停用。
        #[test]
        fn 缺席_未绑定项目_说明项目层未加载() {
            let core = core();
            let session = core
                .store
                .create_session("未绑定", &core.registry.active_group())
                .unwrap();
            assert!(session.workspace.is_none(), "夹具前提：会话未绑定项目");

            let gaps = core.context_source_gaps(&session.id).unwrap();
            let project = gaps
                .iter()
                .find(|g| g["layer"] == "project")
                .unwrap_or_else(|| panic!("缺项目层的缺席说明：{gaps:?}"));
            assert_eq!(project["kind"], "file");
            let reason = project["reason"].as_str().unwrap();
            assert!(
                reason.contains("未绑定项目"),
                "原因要说出是未绑定：{reason}"
            );
            assert!(
                reason.contains("AGENTS.md") && reason.contains(".exmachina/context.md"),
                "要列出没加载的是哪些文件：{reason}"
            );

            // 与清单一致：说了项目层缺席，清单里就不能有项目层条目
            let sources = core.context_sources(&session.id).unwrap();
            assert!(
                sources.iter().all(|s| s["layer"] != "project"),
                "项目层缺席却出现在清单里：{sources:?}"
            );
            // 缺席的层没有可停用的东西：禁用它必须被拒，且不留痕
            let err = core
                .context_source_set(&session.id, "file:AGENTS.md", true)
                .expect_err("未绑定的会话没有 file:AGENTS.md 来源，不该能禁用");
            assert!(err.to_string().contains("file:AGENTS.md"), "{err}");
            assert!(core.store.disabled_context_sources(&session.id).is_empty());
        }

        /// 已绑定但项目目录不可用，是**另一种**缺席：不能也写成「未绑定」。
        /// 原因要带上绑定的路径与 `pinned` 的原话（目录已变化 / 不是目录）。
        #[test]
        fn 缺席_项目目录不可用_不说成未绑定() {
            let core = core();
            let (session, ws) = session_in(&core, "gone");
            let raw = ws.to_string_lossy().into_owned();
            std::fs::remove_dir_all(&ws).unwrap();

            let gaps = core.context_source_gaps(&session.id).unwrap();
            let project = gaps
                .iter()
                .find(|g| g["layer"] == "project")
                .unwrap_or_else(|| panic!("缺项目层的缺席说明：{gaps:?}"));
            let reason = project["reason"].as_str().unwrap();
            assert!(
                reason.contains(&raw),
                "原因要带上绑定的项目路径 {raw}：{reason}"
            );
            assert!(reason.contains("项目目录不可用"), "{reason}");
            assert!(
                !reason.contains("未绑定项目"),
                "已绑定的会话不能说成未绑定：{reason}"
            );
            // 清单同样没有项目层条目——两处判定是同一套
            let sources = core.context_sources(&session.id).unwrap();
            assert!(
                sources.iter().all(|s| s["layer"] != "project"),
                "{sources:?}"
            );
        }

        /// 根都在时没有缺席说明；用户层有没有说明取决于 `~/.exmachina` 是否真实
        /// 存在（环境相关）——按实际条件断言，并与清单里有没有用户层条目互相对照。
        #[test]
        fn 缺席_根都在则无说明_且与清单一致() {
            let core = core();
            let (session, ws) = session_in(&core, "present");
            std::fs::write(ws.join("AGENTS.md"), "规则").unwrap();

            let gaps = core.context_source_gaps(&session.id).unwrap();
            assert!(
                gaps.iter().all(|g| g["layer"] != "project"),
                "项目目录在场却仍说项目层缺席：{gaps:?}"
            );
            let user_gap = gaps.iter().any(|g| g["layer"] == "user");
            let sources = core.context_sources(&session.id).unwrap();
            let user_listed = sources.iter().any(|s| s["layer"] == "user");
            assert_ne!(
                user_gap, user_listed,
                "用户层缺席说明与清单必须互补（说明={} 清单有={}）：{gaps:?} / {sources:?}",
                user_gap, user_listed
            );
            if user_gap {
                let reason = gaps.iter().find(|g| g["layer"] == "user").unwrap()["reason"]
                    .as_str()
                    .unwrap();
                assert!(
                    reason.contains(".exmachina") && reason.contains("context.md"),
                    "要说清缺的是哪个目录里的哪个文件：{reason}"
                );
                assert!(reason.contains("不存在"), "{reason}");
            }
            let _ = std::fs::remove_dir_all(ws);
        }

        /// 缺席说明不是来源：不混进清单。断言不依赖本机有没有 `~/.exmachina`
        /// （有的话未绑定会话的清单里会多一个用户层文件来源）——只看性质本身。
        #[test]
        fn 缺席说明_不进清单() {
            let core = core();
            let session = core
                .store
                .create_session("计数", &core.registry.active_group())
                .unwrap();
            let gaps = core.context_source_gaps(&session.id).unwrap();
            assert!(!gaps.is_empty(), "未绑定会话至少缺项目层：{gaps:?}");
            let sources = core.context_sources(&session.id).unwrap();
            // 清单条目都以 key 标识；缺席说明没有 key，一条也不该出现在清单里
            assert!(sources.iter().all(|s| s["key"].is_string()), "{sources:?}");
            // 说了哪一层缺席，清单里就不能有那一层的条目（两处是同一套判定）
            for g in &gaps {
                let layer = g["layer"].as_str().unwrap();
                assert!(
                    sources.iter().all(|s| s["layer"] != layer),
                    "{layer} 层说了缺席，清单里却有它的条目：{sources:?}"
                );
            }
            // 反过来：未绑定会话一定没有项目层来源
            assert!(
                sources.iter().all(|s| s["layer"] != "project"),
                "{sources:?}"
            );
        }
    }
}
