//! CLI 与面板共用的动态命令参数、身份与结果适配。

use exm_core::command::{
    CommandActor, CommandActorKind, CommandDirectorySnapshot, CommandEntry, CommandErrorCategory,
    CommandExecutionMode, CommandOutputFormat, CommandRequest, CommandResult,
    COMMAND_PROTOCOL_VERSION,
};
use exm_core::command_service::{CommandService, CommandTransportContext};
use exm_core::types::ToolScope;
use exm_core::Core;
use serde_json::Value;
use std::sync::Arc;

pub(crate) fn is_extension(name: &str) -> bool {
    name.starts_with("plugin.") || name.starts_with("skill.")
}

fn local_context(entry: CommandEntry, session: Option<&str>) -> CommandTransportContext {
    CommandTransportContext {
        entry,
        actor: CommandActor {
            id: "local-console".into(),
            kind: CommandActorKind::User,
            roles: vec!["owner".into()],
        },
        authenticated: true,
        session: session.map(str::to_owned),
        project: None,
        flow: None,
        capabilities: Vec::new(),
    }
}

pub(crate) fn directory(
    core: &Arc<Core>,
    entry: CommandEntry,
    session: Option<&str>,
) -> anyhow::Result<CommandDirectorySnapshot> {
    core.extension_command_directory(&local_context(entry, session), ToolScope::Computer)
        .map_err(|error| anyhow::anyhow!("{}", error.message))
}

/// CLI/TUI canonical directory: static descriptors and the current session's dynamic
/// skill/plugin commands, queried with local-console identity and project scope.
pub(crate) fn command_directory(
    core: &Arc<Core>,
    entry: CommandEntry,
    session_id: Option<&str>,
) -> anyhow::Result<CommandDirectorySnapshot> {
    let mut context = local_context(entry, session_id);
    if entry == CommandEntry::Cli {
        context
            .capabilities
            .extend(crate::cli_command_capabilities());
    }
    let workspace = if let Some(session_id) = session_id {
        let session = core
            .store
            .get_session(session_id)?
            .ok_or_else(|| anyhow::anyhow!("会话不存在: {session_id}"))?;
        match session.workspace.as_deref() {
            Some(path) => exm_core::workspace::pinned(path)?,
            None => core
                .registry()
                .effective_workspace(&core.config().workspace_root)
                .canonicalize()?,
        }
    } else {
        core.registry()
            .effective_workspace(&core.config().workspace_root)
            .canonicalize()?
    };
    context.project = Some(workspace.to_string_lossy().into_owned());

    let registry = crate::cli_command_registry()?;
    let query = context.directory_query(None, false);
    let mut snapshot = registry.query(&query);
    match core.extension_command_directory(&context, ToolScope::Computer) {
        Ok(dynamic) => {
            snapshot.entries.extend(dynamic.entries);
            snapshot.diagnostics.extend(dynamic.diagnostics);
            snapshot
                .entries
                .sort_by(|left, right| left.descriptor.name.cmp(&right.descriptor.name));
        }
        Err(error) => snapshot
            .diagnostics
            .push(exm_core::command::CommandRegistryDiagnostic {
                severity: exm_core::command::CommandDiagnosticSeverity::Warning,
                category: error.category,
                code: "dynamic_command_directory_unavailable".into(),
                command: String::new(),
                alias: None,
                message: "技能/插件命令目录暂不可用".into(),
                replacement: None,
            }),
    }
    Ok(snapshot)
}

pub(crate) fn render_directory_markdown(snapshot: &CommandDirectorySnapshot) -> String {
    let mut output = String::from(
        "# Command entry matrix\n\nGenerated from `CommandRegistry::with_builtins()` and the active Clap command tree using `exm command catalog --markdown`. Availability is a local CLI directory preflight without a session; it does not claim successful handler execution.\n\n| 命令 | 用法 | 作用域 | 权限 | 副作用 | 声明入口 | 目录判定 |\n| --- | --- | --- | --- | --- | --- | --- |\n",
    );
    for entry in &snapshot.entries {
        let descriptor = &entry.descriptor;
        let permissions = descriptor
            .permissions
            .iter()
            .map(|permission| format!("{permission:?}").to_ascii_lowercase())
            .collect::<Vec<_>>()
            .join(", ");
        let entries = descriptor
            .entries
            .iter()
            .map(|entry| format!("{entry:?}").to_ascii_lowercase())
            .collect::<Vec<_>>()
            .join(", ");
        let status = if entry.executable {
            "目录允许".to_string()
        } else {
            format!(
                "目录拒绝（{}）",
                unavailable_reason(entry.unavailable_reason)
            )
        };
        output.push_str(&format!(
            "| `{}` | `{}` | `{}` | {} | `{}` | {} | {} |\n",
            descriptor.name,
            descriptor.usage,
            format!("{:?}", descriptor.scope).to_ascii_lowercase(),
            if permissions.is_empty() {
                "none"
            } else {
                &permissions
            },
            format!("{:?}", descriptor.side_effect).to_ascii_lowercase(),
            entries,
            status,
        ));
    }
    output
}

fn unavailable_reason(
    reason: Option<exm_core::command::CommandAvailabilityReason>,
) -> &'static str {
    use exm_core::command::CommandAvailabilityReason as Reason;
    match reason {
        Some(Reason::UnsupportedEntry) => "入口不支持",
        Some(Reason::ForbiddenPermission) => "权限不足",
        Some(Reason::MissingScope) => "缺少所需作用域",
        Some(Reason::MissingCapability) => "缺少所需能力",
        Some(Reason::CapabilityUnavailable) => "能力不可用",
        Some(Reason::CapabilityUnverified) => "能力未验证",
        Some(Reason::CapabilityDisabled) => "能力已禁用",
        Some(Reason::MissingDependency) => "缺少依赖",
        Some(Reason::Deprecated) => "已弃用",
        None => "未知",
    }
}

pub(crate) fn render_directory_text(snapshot: &CommandDirectorySnapshot) -> String {
    snapshot
        .entries
        .iter()
        .map(|entry| {
            let state = if entry.executable {
                "目录允许".to_string()
            } else {
                format!("目录拒绝：{:?}", entry.unavailable_reason)
            };
            format!(
                "{:<26} {:<42} {}\n  {}",
                entry.descriptor.name, entry.descriptor.usage, state, entry.descriptor.summary
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub(crate) async fn execute(
    core: &Arc<Core>,
    entry: CommandEntry,
    session: Option<&str>,
    name: &str,
    version: u16,
    raw_args: &str,
    format: CommandOutputFormat,
) -> CommandResult {
    let context = local_context(entry, session);
    let mut request = CommandRequest {
        protocol_version: COMMAND_PROTOCOL_VERSION,
        request_id: exm_core::types::new_id(),
        command: name.into(),
        command_version: version,
        args: Value::Null,
        session: context.session.clone(),
        project: None,
        flow: None,
        actor: context.actor.clone(),
        entry,
        mode: CommandExecutionMode::Execute,
        output_format: format,
    };
    if !raw_args.trim().is_empty() {
        match serde_json::from_str(raw_args) {
            Ok(args) => request.args = args,
            Err(_) => {
                return CommandResult::failure(
                    &request,
                    CommandErrorCategory::InvalidArgs,
                    "扩展命令参数必须是合法的 JSON 对象",
                    None,
                )
            }
        }
    }
    core.execute_extension_command(request, &context, ToolScope::Computer)
        .await
}

pub(crate) fn render(result: &CommandResult, format: CommandOutputFormat) -> String {
    CommandService::render(result, format)
}

/// 调用失败时结果已打印，不要让顶层再次输出第二个 JSON 对象。
#[derive(Debug)]
pub(crate) struct ReportedCommandFailure;

impl std::fmt::Display for ReportedCommandFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("扩展命令执行失败")
    }
}

impl std::error::Error for ReportedCommandFailure {}
