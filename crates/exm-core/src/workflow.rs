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
                require_status(&mut state, GoalStatus::Active, "pause")?.status = GoalStatus::Paused
            }
            "resume" => {
                if !matches!(state.status, GoalStatus::Paused | GoalStatus::Blocked) {
                    anyhow::bail!("只有 paused/blocked goal 可以 resume");
                }
                state.status = GoalStatus::Active;
            }
            "complete" => {
                let evidence = required_text(verification, "goal verification")?;
                state.verification.push(evidence);
                state.status = GoalStatus::Completed;
            }
            "block" => {
                state.status = GoalStatus::Blocked;
            }
            "clear" => {
                state.status = GoalStatus::Cleared;
                state.note = Some("goal cleared".into());
            }
            _ => anyhow::bail!(
                "未知 goal 操作：{operation}（start/edit/pause/resume/complete/block/clear）"
            ),
        }
        if let Some(note) = note.filter(|value| !value.trim().is_empty()) {
            state.note = Some(note.trim().to_string());
        }
        if state.objective.trim().is_empty() && state.status != GoalStatus::Cleared {
            anyhow::bail!("goal objective 不能为空");
        }
        state.revision += 1;
        state.updated_at = now;
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
        let state = SpecState {
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
        };
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

    let mut redacted = false;
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
}
