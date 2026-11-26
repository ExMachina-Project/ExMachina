//! 跨入口命令协议。该模块只定义并序列化命令契约，不执行领域副作用。
//!
//! CLI/TUI、聊天通道、HTTP 和应用界面都应把入口差异压入 `entry` 与呈现层，
//! 使用相同的请求/结果结构。身份字段是上下文声明，不是授权凭据；服务端必须
//! 从已认证连接重新确认 actor、会话和项目归属。

use crate::tools::{redact_json, redact_str};
use serde::ser::SerializeStruct;
use serde::{Deserialize, Serialize, Serializer};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// 当前命令协议主版本。未知版本必须在任何副作用前以 `version_mismatch` 拒绝。
pub const COMMAND_PROTOCOL_VERSION: u16 = 1;
pub const COMMAND_DESCRIPTOR_VERSION: u16 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandEntry {
    Cli,
    Tui,
    Chat,
    WebUi,
    Http,
    Plugin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandScope {
    Global,
    Project,
    Session,
    Flow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandPermission {
    Read,
    Write,
    Control,
    Approve,
    Configure,
    ExternalEffect,
    Admin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandSideEffect {
    None,
    SessionState,
    ProjectState,
    ExecutionControl,
    Approval,
    ExternalEffect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandOutputFormat {
    Human,
    Json,
}

/// 对外稳定的命令错误类别；具体文案可本地化，类别不得随入口改变。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandErrorCategory {
    Forbidden,
    Unsupported,
    Conflict,
    InvalidArgs,
    VersionMismatch,
    NotFound,
    Unavailable,
    ApprovalRequired,
    RateLimited,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandDescriptor {
    pub protocol_version: u16,
    /// 命令的唯一规范名，例如 `model.use`、`approval.decide`。
    pub name: String,
    /// 仅作为兼容入口映射；规范名始终只有一个 owner。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    pub version: u16,
    pub summary: String,
    pub usage: String,
    /// JSON Schema 对象；目录描述参数，不在此处执行参数或业务逻辑。
    pub argument_schema: Value,
    pub scope: CommandScope,
    #[serde(default)]
    pub permissions: Vec<CommandPermission>,
    pub side_effect: CommandSideEffect,
    #[serde(default)]
    pub entries: Vec<CommandEntry>,
    #[serde(default)]
    pub output_formats: Vec<CommandOutputFormat>,
    #[serde(default)]
    pub error_categories: Vec<CommandErrorCategory>,
    #[serde(default)]
    pub required_capabilities: Vec<String>,
    /// 核心命令 owner 固定为 `core`；扩展命令需使用显式命名空间。
    pub owner: String,
    #[serde(default)]
    pub deprecated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replacement: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandActorKind {
    User,
    Service,
    Agent,
    System,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandActor {
    /// 该值由认证适配器填充，服务端不得信任客户端自行提交的角色/身份声明。
    pub id: String,
    pub kind: CommandActorKind,
    #[serde(default)]
    pub roles: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandExecutionMode {
    Execute,
    Preview,
}

/// 规范命令请求。可选 session/project 是资源定位符，服务端仍须校验归属。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandRequest {
    pub protocol_version: u16,
    pub request_id: String,
    pub command: String,
    pub command_version: u16,
    #[serde(default)]
    pub args: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flow: Option<String>,
    pub actor: CommandActor,
    pub entry: CommandEntry,
    pub mode: CommandExecutionMode,
    pub output_format: CommandOutputFormat,
}

impl CommandRequest {
    pub fn has_supported_protocol(&self) -> bool {
        self.protocol_version == COMMAND_PROTOCOL_VERSION
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandStatus {
    Succeeded,
    Accepted,
    Pending,
    Rejected,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandError {
    pub category: CommandErrorCategory,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandEvent {
    pub kind: String,
    #[serde(default)]
    pub data: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandAuditReceipt {
    pub event_id: String,
    pub occurred_at: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandNextAction {
    pub command: String,
    pub label: String,
    #[serde(default)]
    pub args: Value,
}

/// 结果有自定义序列化：任何入口输出前，所有自由文本和 JSON 载荷都经过
/// Core 已有的确定性密钥脱敏器，避免 handler 忘记处理配置值或错误文案。
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandResult {
    pub protocol_version: u16,
    pub request_id: String,
    pub command: String,
    pub status: CommandStatus,
    pub ok: bool,
    #[serde(default)]
    pub data: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<CommandError>,
    #[serde(default)]
    pub events: Vec<CommandEvent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audit: Option<CommandAuditReceipt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_action: Option<CommandNextAction>,
}

impl CommandResult {
    pub fn success(request: &CommandRequest, status: CommandStatus, data: Value) -> Self {
        Self {
            protocol_version: COMMAND_PROTOCOL_VERSION,
            request_id: request.request_id.clone(),
            command: request.command.clone(),
            status,
            ok: true,
            data,
            error: None,
            events: Vec::new(),
            audit: None,
            next_action: None,
        }
    }

    pub fn failure(
        request: &CommandRequest,
        category: CommandErrorCategory,
        message: impl Into<String>,
        details: Option<Value>,
    ) -> Self {
        let status = match category {
            CommandErrorCategory::Forbidden
            | CommandErrorCategory::Unsupported
            | CommandErrorCategory::Conflict
            | CommandErrorCategory::InvalidArgs
            | CommandErrorCategory::VersionMismatch
            | CommandErrorCategory::NotFound
            | CommandErrorCategory::ApprovalRequired => CommandStatus::Rejected,
            CommandErrorCategory::Unavailable
            | CommandErrorCategory::RateLimited
            | CommandErrorCategory::Internal => CommandStatus::Failed,
        };
        Self {
            protocol_version: COMMAND_PROTOCOL_VERSION,
            request_id: request.request_id.clone(),
            command: request.command.clone(),
            status,
            ok: false,
            data: Value::Null,
            error: Some(CommandError {
                category,
                message: message.into(),
                details,
            }),
            events: Vec::new(),
            audit: None,
            next_action: None,
        }
    }
}

/// 能力不是简单的布尔值：声明存在不代表依赖就绪或当前身份获准调用。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityStatus {
    Available,
    Unavailable,
    Forbidden,
    Unverified,
    Disabled,
    MissingDependency,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilityItem {
    pub name: String,
    pub status: CapabilityStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilitySnapshot {
    pub protocol_version: u16,
    pub snapshot_id: String,
    pub generated_at: String,
    pub entry: CommandEntry,
    pub actor: CommandActor,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flow: Option<String>,
    #[serde(default)]
    pub capabilities: Vec<CapabilityItem>,
    /// 经入口/身份/作用域裁剪后可发现的命令规范名。
    #[serde(default)]
    pub commands: Vec<String>,
}

impl CapabilitySnapshot {
    pub fn has_supported_protocol(&self) -> bool {
        self.protocol_version == COMMAND_PROTOCOL_VERSION
    }
}

/// 目录查询上下文。它只描述裁剪条件，不携带可执行闭包或领域状态句柄，
/// 因此 `CommandRegistry::query` 保证是纯查询，不会产生副作用。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandDirectoryQuery {
    pub protocol_version: u16,
    pub entry: CommandEntry,
    pub actor: CommandActor,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<CommandScope>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flow: Option<String>,
    #[serde(default)]
    pub capabilities: Vec<CapabilityItem>,
    /// 默认不把已弃用命令作为可执行命令展示；开启后会带 deprecated 原因。
    #[serde(default)]
    pub include_deprecated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandAvailabilityReason {
    UnsupportedEntry,
    ForbiddenPermission,
    MissingScope,
    MissingCapability,
    CapabilityUnavailable,
    CapabilityUnverified,
    CapabilityDisabled,
    MissingDependency,
    Deprecated,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandDirectoryEntry {
    pub descriptor: CommandDescriptor,
    pub executable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unavailable_reason: Option<CommandAvailabilityReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replacement: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandDirectorySnapshot {
    pub protocol_version: u16,
    pub entries: Vec<CommandDirectoryEntry>,
    #[serde(default)]
    pub diagnostics: Vec<CommandRegistryDiagnostic>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandDiagnosticSeverity {
    Warning,
    Error,
}

/// 注册诊断与执行期 `CommandResult.error.category` 分开：注册器可以保留一个
/// 已弃用但仍可兼容的命令，同时将迁移提示放进目录诊断，不把它伪装成新命令。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandRegistryDiagnostic {
    pub severity: CommandDiagnosticSeverity,
    pub category: CommandErrorCategory,
    pub code: String,
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replacement: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CommandRegistry {
    protocol_version: u16,
    commands: BTreeMap<String, CommandDescriptor>,
    aliases: BTreeMap<String, String>,
    diagnostics: Vec<CommandRegistryDiagnostic>,
}

impl CommandRegistry {
    pub fn new(protocol_version: u16) -> Self {
        Self {
            protocol_version,
            commands: BTreeMap::new(),
            aliases: BTreeMap::new(),
            diagnostics: Vec::new(),
        }
    }

    #[allow(clippy::result_large_err)]
    pub fn with_builtins() -> Result<Self, CommandRegistryDiagnostic> {
        let mut registry = Self::new(COMMAND_PROTOCOL_VERSION);
        for descriptor in builtin_command_descriptors() {
            registry.register(descriptor)?;
        }
        Ok(registry)
    }

    /// 注册规范命令。任何失败都在写入 map 前返回，避免留下半个命令或半个别名。
    #[allow(clippy::result_large_err)]
    pub fn register(
        &mut self,
        descriptor: CommandDescriptor,
    ) -> Result<(), CommandRegistryDiagnostic> {
        if descriptor.protocol_version != self.protocol_version
            || descriptor.protocol_version != COMMAND_PROTOCOL_VERSION
            || descriptor.version == 0
            || (descriptor.owner == "core" && descriptor.version != COMMAND_DESCRIPTOR_VERSION)
        {
            return Err(registry_diagnostic(
                CommandDiagnosticSeverity::Error,
                CommandErrorCategory::VersionMismatch,
                "unknown_command_version",
                &descriptor.name,
                None,
                "命令协议或描述版本不受当前目录支持",
                descriptor.replacement.clone(),
            ));
        }
        if descriptor.name.trim().is_empty() || descriptor.owner.trim().is_empty() {
            return Err(registry_diagnostic(
                CommandDiagnosticSeverity::Error,
                CommandErrorCategory::InvalidArgs,
                "invalid_descriptor",
                &descriptor.name,
                None,
                "命令规范名和唯一 owner 不能为空",
                descriptor.replacement.clone(),
            ));
        }
        if let Some(prefix) = descriptor
            .owner
            .strip_prefix("plugin:")
            .map(|id| format!("plugin.{id}."))
            .or_else(|| {
                descriptor
                    .owner
                    .strip_prefix("skill:")
                    .map(|id| format!("skill.{id}"))
            })
        {
            let namespaced = |name: &str| {
                if descriptor.owner.starts_with("plugin:") {
                    valid_namespace_part(descriptor.owner.trim_start_matches("plugin:"))
                        && !descriptor.owner.contains("__")
                        && name.strip_prefix(&prefix).is_some_and(valid_namespace_part)
                } else {
                    valid_skill_command_id(descriptor.owner.trim_start_matches("skill:"))
                        && name == prefix
                }
            };
            if !namespaced(&descriptor.name)
                || descriptor.aliases.iter().any(|name| !namespaced(name))
            {
                return Err(registry_diagnostic(
                    CommandDiagnosticSeverity::Error,
                    CommandErrorCategory::Forbidden,
                    "extension_namespace_required",
                    &descriptor.name,
                    None,
                    "扩展命令及别名必须位于自身命名空间，不能覆盖核心命令",
                    None,
                ));
            }
        }
        if self.commands.contains_key(&descriptor.name) {
            return Err(registry_diagnostic(
                CommandDiagnosticSeverity::Error,
                CommandErrorCategory::Conflict,
                "duplicate_command_name",
                &descriptor.name,
                None,
                "规范命令名已经注册，禁止覆盖既有 owner",
                descriptor.replacement.clone(),
            ));
        }
        if self.aliases.contains_key(&descriptor.name) {
            return Err(registry_diagnostic(
                CommandDiagnosticSeverity::Error,
                CommandErrorCategory::Conflict,
                "command_name_alias_conflict",
                &descriptor.name,
                None,
                "规范命令名已被其他命令注册为别名",
                descriptor.replacement.clone(),
            ));
        }

        let mut local_aliases = BTreeSet::new();
        for alias in &descriptor.aliases {
            if alias.trim().is_empty() || alias == &descriptor.name {
                return Err(registry_diagnostic(
                    CommandDiagnosticSeverity::Error,
                    CommandErrorCategory::Conflict,
                    "invalid_command_alias",
                    &descriptor.name,
                    Some(alias.clone()),
                    "别名不能为空且不能等于规范命令名",
                    descriptor.replacement.clone(),
                ));
            }
            if !local_aliases.insert(alias.clone())
                || self.aliases.contains_key(alias)
                || self.commands.contains_key(alias)
            {
                return Err(registry_diagnostic(
                    CommandDiagnosticSeverity::Error,
                    CommandErrorCategory::Conflict,
                    "duplicate_command_alias",
                    &descriptor.name,
                    Some(alias.clone()),
                    "命令别名已经被当前或其他命令占用",
                    descriptor.replacement.clone(),
                ));
            }
        }

        if descriptor.deprecated {
            self.diagnostics.push(registry_diagnostic(
                CommandDiagnosticSeverity::Warning,
                CommandErrorCategory::Unsupported,
                "deprecated_command",
                &descriptor.name,
                None,
                "命令已弃用，目录默认不把它展示为可执行命令",
                descriptor.replacement.clone(),
            ));
        }
        for alias in &descriptor.aliases {
            self.aliases.insert(alias.clone(), descriptor.name.clone());
        }
        self.commands.insert(descriptor.name.clone(), descriptor);
        Ok(())
    }

    /// 注册插件/技能命令。插件永远使用显式命名空间，不能通过传入 `model.use`、
    /// `help` 等名称覆盖 core owner；插件 alias 也必须带同一命名空间。
    #[allow(clippy::too_many_arguments, clippy::result_large_err)]
    pub fn register_plugin_command(
        &mut self,
        plugin_id: &str,
        command: &str,
        version: u16,
        summary: &str,
        usage: &str,
        argument_schema: Value,
        required_capabilities: Vec<String>,
    ) -> Result<String, CommandRegistryDiagnostic> {
        let plugin_id = plugin_id.trim();
        let command = command.trim();
        if !valid_namespace_part(plugin_id)
            || plugin_id.contains("__")
            || !valid_namespace_part(command)
        {
            return Err(registry_diagnostic(
                CommandDiagnosticSeverity::Error,
                CommandErrorCategory::InvalidArgs,
                "invalid_plugin_command_namespace",
                command,
                None,
                "插件命令必须使用合法的 plugin id 和命令名",
                None,
            ));
        }
        if version == 0 {
            return Err(registry_diagnostic(
                CommandDiagnosticSeverity::Error,
                CommandErrorCategory::VersionMismatch,
                "invalid_plugin_command_version",
                command,
                None,
                "插件命令版本必须是正数",
                None,
            ));
        }
        let canonical = format!("plugin.{plugin_id}.{command}");
        let descriptor = CommandDescriptor {
            protocol_version: COMMAND_PROTOCOL_VERSION,
            name: canonical.clone(),
            aliases: Vec::new(),
            version,
            summary: summary.into(),
            usage: usage.into(),
            argument_schema,
            scope: CommandScope::Session,
            permissions: vec![CommandPermission::Write, CommandPermission::ExternalEffect],
            side_effect: CommandSideEffect::ExternalEffect,
            entries: vec![CommandEntry::Cli, CommandEntry::Tui],
            output_formats: vec![CommandOutputFormat::Human, CommandOutputFormat::Json],
            error_categories: vec![
                CommandErrorCategory::Forbidden,
                CommandErrorCategory::Unsupported,
                CommandErrorCategory::InvalidArgs,
                CommandErrorCategory::Unavailable,
                CommandErrorCategory::VersionMismatch,
                CommandErrorCategory::Conflict,
            ],
            required_capabilities,
            owner: format!("plugin:{plugin_id}"),
            deprecated: false,
            replacement: None,
        };
        self.register(descriptor)?;
        Ok(canonical)
    }

    /// 将已装载 PluginManager 的显式命令声明映射为版本化描述，并返回同一查询应
    /// 使用的能力状态。禁用/校验失败插件仍可诊断，但不会被宣称为可执行能力。
    pub fn register_plugin_manager(
        &mut self,
        manager: &crate::plugin::PluginManager,
    ) -> Vec<CapabilityItem> {
        let mut capabilities = Vec::new();
        for entry in manager.list() {
            let capability = format!("plugin.{}.ready", entry.manifest.id);
            let (status, reason) = if let Some(error) = entry.error.as_ref() {
                let missing_dependency = error.contains("需要宿主 ≥")
                    || error.contains("依赖未就绪")
                    || error.contains("required dependency");
                (
                    if missing_dependency {
                        CapabilityStatus::MissingDependency
                    } else {
                        CapabilityStatus::Unavailable
                    },
                    Some(error.clone()),
                )
            } else if !entry.enabled {
                (CapabilityStatus::Disabled, Some("插件已禁用".into()))
            } else if entry.form == "process" && manager.provider_of(&entry.manifest.id).is_none() {
                (
                    CapabilityStatus::MissingDependency,
                    Some("插件进程尚未就绪".into()),
                )
            } else {
                (CapabilityStatus::Available, None)
            };
            capabilities.push(CapabilityItem {
                name: capability.clone(),
                status,
                reason,
            });

            if entry.manifest.commands.is_empty() {
                continue; // 模型工具不会隐式变成面向用户的直接调用入口。
            }
            let provider_specs = manager
                .provider_of(&entry.manifest.id)
                .and_then(|provider| provider.tool_provider())
                .map(|provider| provider.tool_specs())
                .unwrap_or_default();
            for (tool, declaration) in &entry.manifest.commands {
                let full_name = crate::plugin::namespaced(&entry.manifest.id, tool);
                let argument_schema = provider_specs
                    .iter()
                    .find(|candidate| candidate.name == full_name)
                    .or_else(|| {
                        entry
                            .manifest
                            .tools
                            .iter()
                            .find(|candidate| &candidate.name == tool)
                    })
                    .map(|candidate| candidate.param_schema())
                    .unwrap_or_else(
                        || serde_json::json!({"type":"object","additionalProperties":true}),
                    );
                let mut requires = vec![capability.clone()];
                requires.extend(declaration.requires.iter().cloned());
                requires.push(format!(
                    "command.handler.plugin.{}.{}",
                    entry.manifest.id, tool
                ));
                if let Err(diagnostic) = self.register_plugin_command(
                    &entry.manifest.id,
                    tool,
                    declaration.version,
                    &format!("{} 插件工具 `{tool}`", entry.manifest.name),
                    &format!("plugin.{}.{tool} <JSON 参数>", entry.manifest.id),
                    argument_schema,
                    requires,
                ) {
                    self.diagnostics.push(diagnostic);
                }
            }
        }
        capabilities
    }

    /// 技能命令只引用既有技能事实源，不复制正文或授予技能额外工具权限。
    /// 首版命令版本固定为 1；技能包版本不等同于命令协议版本。
    #[allow(clippy::result_large_err)]
    pub fn register_skill_command(
        &mut self,
        skill: &crate::types::SkillDef,
    ) -> Result<String, CommandRegistryDiagnostic> {
        if !valid_skill_command_id(&skill.id) {
            return Err(registry_diagnostic(
                CommandDiagnosticSeverity::Error,
                CommandErrorCategory::InvalidArgs,
                "invalid_skill_command_namespace",
                &skill.id,
                None,
                "技能命令必须使用合法的技能 id",
                None,
            ));
        }
        let name = format!("skill.{}", skill.id);
        self.register(CommandDescriptor {
            protocol_version: COMMAND_PROTOCOL_VERSION,
            name: name.clone(),
            aliases: Vec::new(),
            version: COMMAND_DESCRIPTOR_VERSION,
            summary: format!("使用技能：{}", skill.name),
            usage: format!("{name} {{\"input\":\"任务说明\"}}"),
            argument_schema: object_schema(&[("input", "string")], &[]),
            scope: CommandScope::Session,
            permissions: vec![CommandPermission::Write],
            side_effect: CommandSideEffect::ExecutionControl,
            entries: vec![CommandEntry::Cli, CommandEntry::Tui],
            output_formats: vec![CommandOutputFormat::Human, CommandOutputFormat::Json],
            error_categories: vec![
                CommandErrorCategory::Forbidden,
                CommandErrorCategory::Unavailable,
                CommandErrorCategory::Conflict,
                CommandErrorCategory::VersionMismatch,
                CommandErrorCategory::InvalidArgs,
            ],
            required_capabilities: vec![
                format!("skill.{}.ready", skill.id),
                format!("command.handler.{name}"),
            ],
            owner: format!("skill:{}", skill.id),
            deprecated: false,
            replacement: None,
        })?;
        Ok(name)
    }

    /// Core 装配期只可收窄插件命令，不允许替换 owner、版本、名称或别名。
    #[allow(clippy::result_large_err)]
    pub(crate) fn require_plugin_execution_capability(
        &mut self,
        name: &str,
        capability: String,
    ) -> Result<(), CommandRegistryDiagnostic> {
        let Some(descriptor) = self
            .commands
            .get_mut(name)
            .filter(|item| item.owner.starts_with("plugin:"))
        else {
            return Err(registry_diagnostic(
                CommandDiagnosticSeverity::Error,
                CommandErrorCategory::Forbidden,
                "plugin_command_required",
                name,
                None,
                "只能收窄已注册插件命令的执行条件",
                None,
            ));
        };
        descriptor.required_capabilities.push(capability);
        Ok(())
    }

    /// 在插件安装/启停/卸载后用最新 PluginManager 快照重建动态命令视图；核心命令
    /// 原样保留，已卸载 owner 的命令和别名整体移除，避免陈旧帮助或执行入口。
    pub fn sync_plugin_manager(
        &mut self,
        manager: &crate::plugin::PluginManager,
    ) -> Vec<CapabilityItem> {
        self.commands
            .retain(|_, descriptor| !descriptor.owner.starts_with("plugin:"));
        self.aliases.clear();
        for descriptor in self.commands.values() {
            for alias in &descriptor.aliases {
                self.aliases.insert(alias.clone(), descriptor.name.clone());
            }
        }
        self.register_plugin_manager(manager)
    }

    #[allow(clippy::result_large_err)]
    pub fn with_plugins(
        manager: &crate::plugin::PluginManager,
    ) -> Result<(Self, Vec<CapabilityItem>), CommandRegistryDiagnostic> {
        let mut registry = Self::with_builtins()?;
        let capabilities = registry.register_plugin_manager(manager);
        Ok((registry, capabilities))
    }

    pub fn diagnostics(&self) -> &[CommandRegistryDiagnostic] {
        &self.diagnostics
    }

    pub(crate) fn push_diagnostic(&mut self, diagnostic: CommandRegistryDiagnostic) {
        self.diagnostics.push(diagnostic);
    }

    pub fn resolve(&self, name_or_alias: &str) -> Option<&CommandDescriptor> {
        if let Some(descriptor) = self.commands.get(name_or_alias) {
            return Some(descriptor);
        }
        self.aliases
            .get(name_or_alias)
            .and_then(|name| self.commands.get(name))
    }

    pub fn descriptors(&self) -> impl Iterator<Item = &CommandDescriptor> {
        self.commands.values()
    }

    /// 按入口、身份、作用域和能力快照裁剪目录；本函数不修改注册器，也不调用
    /// Core/Store/网络。未授权项保留描述但明确 `executable=false`，以便帮助说明
    /// 不可用原因，同时避免把它伪装成可执行命令。
    pub fn query(&self, query: &CommandDirectoryQuery) -> CommandDirectorySnapshot {
        if query.protocol_version != self.protocol_version {
            let mut diagnostics = self.diagnostics.clone();
            diagnostics.push(registry_diagnostic(
                CommandDiagnosticSeverity::Error,
                CommandErrorCategory::VersionMismatch,
                "unknown_client_protocol_version",
                "",
                None,
                "客户端命令协议版本与目录版本不匹配；未返回可执行命令",
                None,
            ));
            return CommandDirectorySnapshot {
                protocol_version: self.protocol_version,
                entries: Vec::new(),
                diagnostics,
            };
        }
        let mut entries = Vec::new();
        for descriptor in self.commands.values() {
            if descriptor.deprecated && !query.include_deprecated {
                continue;
            }
            let (executable, reason) = self.availability(descriptor, query);
            entries.push(CommandDirectoryEntry {
                replacement: descriptor.replacement.clone(),
                descriptor: descriptor.clone(),
                executable,
                unavailable_reason: reason,
            });
        }
        CommandDirectorySnapshot {
            protocol_version: self.protocol_version,
            entries,
            diagnostics: self.diagnostics.clone(),
        }
    }

    pub(crate) fn availability(
        &self,
        descriptor: &CommandDescriptor,
        query: &CommandDirectoryQuery,
    ) -> (bool, Option<CommandAvailabilityReason>) {
        if descriptor.deprecated {
            return (false, Some(CommandAvailabilityReason::Deprecated));
        }
        if !descriptor.entries.contains(&query.entry) {
            return (false, Some(CommandAvailabilityReason::UnsupportedEntry));
        }
        if query
            .scope
            .is_some_and(|requested_scope| requested_scope != descriptor.scope)
        {
            return (false, Some(CommandAvailabilityReason::MissingScope));
        }
        if descriptor.scope != CommandScope::Global {
            let scope_present = match descriptor.scope {
                CommandScope::Global => true,
                CommandScope::Project => query.project.as_deref().is_some_and(|v| !v.is_empty()),
                CommandScope::Session => query.session.as_deref().is_some_and(|v| !v.is_empty()),
                CommandScope::Flow => query.flow.as_deref().is_some_and(|v| !v.is_empty()),
            };
            if !scope_present {
                return (false, Some(CommandAvailabilityReason::MissingScope));
            }
        }
        if !permissions_allowed(&descriptor.permissions, &query.actor) {
            return (false, Some(CommandAvailabilityReason::ForbiddenPermission));
        }
        for required in &descriptor.required_capabilities {
            let Some(capability) = query
                .capabilities
                .iter()
                .find(|item| item.name == *required)
            else {
                return (false, Some(CommandAvailabilityReason::MissingCapability));
            };
            if capability.status != CapabilityStatus::Available {
                let reason = match capability.status {
                    CapabilityStatus::Available => unreachable!(),
                    CapabilityStatus::Unavailable => {
                        CommandAvailabilityReason::CapabilityUnavailable
                    }
                    CapabilityStatus::Forbidden => CommandAvailabilityReason::ForbiddenPermission,
                    CapabilityStatus::Unverified => CommandAvailabilityReason::CapabilityUnverified,
                    CapabilityStatus::Disabled => CommandAvailabilityReason::CapabilityDisabled,
                    CapabilityStatus::MissingDependency => {
                        CommandAvailabilityReason::MissingDependency
                    }
                };
                return (false, Some(reason));
            }
        }
        (true, None)
    }
}

pub(crate) fn valid_namespace_part(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 48
        && value
            .chars()
            .next()
            .is_some_and(|ch| ch.is_ascii_alphabetic())
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
}

fn valid_skill_command_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 48
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
}

fn registry_diagnostic(
    severity: CommandDiagnosticSeverity,
    category: CommandErrorCategory,
    code: &str,
    command: &str,
    alias: Option<String>,
    message: &str,
    replacement: Option<String>,
) -> CommandRegistryDiagnostic {
    CommandRegistryDiagnostic {
        severity,
        category,
        code: code.into(),
        command: command.into(),
        alias,
        message: message.into(),
        replacement,
    }
}

pub(crate) fn permissions_allowed(permissions: &[CommandPermission], actor: &CommandActor) -> bool {
    if permissions.is_empty() || actor.kind == CommandActorKind::System {
        return true;
    }
    let roles: BTreeSet<String> = actor
        .roles
        .iter()
        .map(|role| role.to_ascii_lowercase())
        .collect();
    permissions.iter().all(|permission| match permission {
        CommandPermission::Read => !actor.id.trim().is_empty(),
        CommandPermission::Write => {
            roles.contains("owner") || roles.contains("operator") || roles.contains("admin")
        }
        CommandPermission::Control
        | CommandPermission::Approve
        | CommandPermission::Configure
        | CommandPermission::ExternalEffect => {
            roles.contains("owner") || roles.contains("operator") || roles.contains("admin")
        }
        CommandPermission::Admin => roles.contains("admin"),
    })
}

fn all_entries() -> Vec<CommandEntry> {
    vec![CommandEntry::Cli, CommandEntry::Tui]
}

fn object_schema(properties: &[(&str, &str)], required: &[&str]) -> Value {
    let properties = properties
        .iter()
        .map(|(name, kind)| {
            let mut schema = serde_json::json!({"type": kind});
            if *kind == "string" && required.contains(name) {
                schema["minLength"] = serde_json::json!(1);
            }
            ((*name).to_string(), schema)
        })
        .collect::<serde_json::Map<_, _>>();
    serde_json::json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false
    })
}

#[allow(clippy::too_many_arguments)]
fn builtin_descriptor(
    name: &str,
    aliases: &[&str],
    scope: CommandScope,
    permissions: &[CommandPermission],
    side_effect: CommandSideEffect,
    summary: &str,
    usage: &str,
    argument_schema: Value,
) -> CommandDescriptor {
    CommandDescriptor {
        protocol_version: COMMAND_PROTOCOL_VERSION,
        name: name.into(),
        aliases: aliases.iter().map(|alias| (*alias).into()).collect(),
        version: COMMAND_DESCRIPTOR_VERSION,
        summary: summary.into(),
        usage: usage.into(),
        argument_schema,
        scope,
        permissions: permissions.to_vec(),
        side_effect,
        entries: all_entries(),
        output_formats: vec![CommandOutputFormat::Human, CommandOutputFormat::Json],
        error_categories: vec![
            CommandErrorCategory::Forbidden,
            CommandErrorCategory::Unsupported,
            CommandErrorCategory::Conflict,
            CommandErrorCategory::InvalidArgs,
            CommandErrorCategory::VersionMismatch,
            CommandErrorCategory::NotFound,
            CommandErrorCategory::Unavailable,
            CommandErrorCategory::ApprovalRequired,
            CommandErrorCategory::RateLimited,
            CommandErrorCategory::Internal,
        ],
        required_capabilities: Vec::new(),
        owner: "core".into(),
        deprecated: false,
        replacement: None,
    }
}

/// 核心命令目录的静态描述。这里不绑定 handler；handler 在后续统一命令服务中
/// 解析同一规范名，入口适配器不能自行复制领域实现。
pub fn builtin_command_descriptors() -> Vec<CommandDescriptor> {
    use CommandPermission::{Admin, Approve, Configure, Control, Read, Write};
    use CommandScope::{Global, Project, Session};
    use CommandSideEffect::{Approval, ExecutionControl, None, ProjectState, SessionState};

    vec![
        builtin_descriptor(
            "help",
            &["?"],
            Global,
            &[Read],
            None,
            "显示当前入口可用命令和权限提示",
            "help [command]",
            object_schema(&[("command", "string")], &[]),
        ),
        builtin_descriptor(
            "model.list",
            &["model"],
            Global,
            &[Read],
            None,
            "列出模型档案和当前生效模型",
            "model list",
            object_schema(&[], &[]),
        ),
        builtin_descriptor(
            "model.use",
            &[],
            Project,
            &[Configure],
            ProjectState,
            "切换项目或会话使用的模型",
            "model use <profileId>",
            object_schema(&[("profileId", "string")], &["profileId"]),
        ),
        builtin_descriptor(
            "status",
            &[],
            Global,
            &[Read],
            None,
            "显示会话、运行和能力状态",
            "status",
            object_schema(&[], &[]),
        ),
        builtin_descriptor(
            "sessions.new",
            &["new"],
            Global,
            &[Write],
            SessionState,
            "创建会话",
            "sessions new",
            object_schema(&[("title", "string")], &[]),
        ),
        builtin_descriptor(
            "sessions.list",
            &["sessions"],
            Global,
            &[Read],
            None,
            "列出当前身份可见会话",
            "sessions list",
            object_schema(&[], &[]),
        ),
        builtin_descriptor(
            "sessions.resume",
            &["resume"],
            Session,
            &[Write],
            SessionState,
            "恢复指定会话",
            "sessions resume <sessionId>",
            object_schema(&[("sessionId", "string")], &["sessionId"]),
        ),
        builtin_descriptor(
            "stop",
            &["cancel"],
            Session,
            &[Control],
            ExecutionControl,
            "停止当前会话运行",
            "stop",
            object_schema(&[], &[]),
        ),
        builtin_descriptor(
            "steer",
            &[],
            Session,
            &[Write],
            SessionState,
            "在安全检查点引导当前运行",
            "steer <instruction>",
            object_schema(&[("instruction", "string")], &["instruction"]),
        ),
        builtin_descriptor(
            "queue.add",
            &["queue"],
            Session,
            &[Write],
            SessionState,
            "明确加入后续任务队列",
            "queue add <instruction>",
            object_schema(&[("instruction", "string")], &["instruction"]),
        ),
        builtin_descriptor(
            "queue.list",
            &[],
            Session,
            &[Read],
            None,
            "查询后续任务队列",
            "queue list",
            object_schema(&[], &[]),
        ),
        builtin_descriptor(
            "skills.list",
            &["skills", "skill"],
            Project,
            &[Read],
            None,
            "列出当前项目技能",
            "skills list",
            object_schema(&[], &[]),
        ),
        builtin_descriptor(
            "cron.list",
            &["cron"],
            Project,
            &[Read],
            None,
            "列出当前项目自动化任务",
            "cron list",
            object_schema(&[], &[]),
        ),
        builtin_descriptor(
            "approval.list",
            &["approval", "approvals"],
            Session,
            &[Read],
            None,
            "列出当前会话待审批项",
            "approval list",
            object_schema(&[], &[]),
        ),
        builtin_descriptor(
            "approval.decide",
            &["approve", "deny"],
            Session,
            &[Approve],
            Approval,
            "批准或拒绝待审批项",
            "approval decide <approvalId> <decision>",
            object_schema(
                &[("approvalId", "string"), ("decision", "string")],
                &["approvalId", "decision"],
            ),
        ),
        builtin_descriptor(
            "config.show",
            &["config"],
            Project,
            &[Read],
            None,
            "显示脱敏后的项目配置",
            "config show",
            object_schema(&[], &[]),
        ),
        builtin_descriptor(
            "config.update",
            &[],
            Project,
            &[Configure],
            ProjectState,
            "更新项目配置",
            "config update <key> <value>",
            object_schema(&[("key", "string"), ("value", "string")], &["key", "value"]),
        ),
        builtin_descriptor(
            "memory.list",
            &["memory", "mem"],
            Project,
            &[Read],
            None,
            "列出当前项目可见记忆",
            "memory list",
            object_schema(&[], &[]),
        ),
        builtin_descriptor(
            "checkpoint.list",
            &["checkpoint", "checkpoints"],
            Project,
            &[Read],
            None,
            "列出工作区文件检查点",
            "checkpoint list",
            object_schema(&[], &[]),
        ),
        builtin_descriptor(
            "group.list",
            &["group", "groups"],
            Global,
            &[Read],
            None,
            "列出智能体组",
            "group list",
            object_schema(&[], &[]),
        ),
        builtin_descriptor(
            "agent.list",
            &["agent", "agents"],
            Global,
            &[Read],
            None,
            "列出智能体",
            "agent list",
            object_schema(&[], &[]),
        ),
        builtin_descriptor(
            "persona.get",
            &["persona"],
            Global,
            &[Read],
            None,
            "查看智能体人设",
            "persona get <identifier>",
            object_schema(&[("identifier", "string")], &["identifier"]),
        ),
        builtin_descriptor(
            "plan.status",
            &["plan"],
            Session,
            &[Read],
            None,
            "查看当前计划状态（与 goal/spec 关联：方案从规格推导，批准前只读）",
            "plan status",
            object_schema(&[], &[]),
        ),
        builtin_descriptor(
            "plan.show",
            &[],
            Session,
            &[Read],
            None,
            "查看计划条目",
            "plan show",
            object_schema(&[], &[]),
        ),
        builtin_descriptor(
            "plan.approve",
            &[],
            Session,
            &[Approve],
            Approval,
            "批准计划并切换执行",
            "plan approve <revision>",
            object_schema(&[("revision", "integer")], &["revision"]),
        ),
        builtin_descriptor(
            "plan.reject",
            &[],
            Session,
            &[Approve],
            Approval,
            "拒绝计划并保留待修改清单",
            "plan reject <revision>",
            object_schema(&[("revision", "integer")], &["revision"]),
        ),
        builtin_descriptor(
            "spec.show",
            &["spec"],
            Session,
            &[Read],
            None,
            "查看会话内编程规格（自动关联当前 goal；修改不自动批准 plan）",
            "spec show",
            object_schema(&[], &[]),
        ),
        builtin_descriptor(
            "spec.draft",
            &[],
            Session,
            &[Write],
            SessionState,
            "草拟会话内编程规格（关联当前 goal；写入 plan 前需显式批准）",
            "spec draft <objective>",
            object_schema(&[("objective", "string")], &["objective"]),
        ),
        builtin_descriptor(
            "spec.edit",
            &[],
            Session,
            &[Write],
            SessionState,
            "修订会话内编程规格",
            "spec edit <objective> [--revision <revision>]",
            object_schema(
                &[("objective", "string"), ("revision", "integer")],
                &["objective"],
            ),
        ),
        builtin_descriptor(
            "spec.export",
            &[],
            Project,
            &[Write],
            ProjectState,
            "显式导出编程规格到批准项目路径",
            "spec export <path>",
            object_schema(&[("path", "string")], &["path"]),
        ),
        builtin_descriptor(
            "spec.clear",
            &[],
            Session,
            &[Write],
            SessionState,
            "清除当前会话编程规格",
            "spec clear [--revision <revision>]",
            object_schema(&[("revision", "integer")], &[]),
        ),
        builtin_descriptor(
            "goal.status",
            &["goal"],
            Session,
            &[Read],
            None,
            "查看会话持久目标（与 spec 关联显示；complete 需真实验证证据）",
            "goal status",
            object_schema(&[], &[]),
        ),
        builtin_descriptor(
            "goal.start",
            &[],
            Session,
            &[Write],
            SessionState,
            "启动会话持久目标（可被 /spec 引用关联；用 workflow 视览看四者关系）",
            "goal start <objective>",
            object_schema(&[("objective", "string")], &["objective"]),
        ),
        builtin_descriptor(
            "goal.edit",
            &[],
            Session,
            &[Write],
            SessionState,
            "修订会话持久目标",
            "goal edit <objective>",
            object_schema(
                &[("objective", "string"), ("revision", "integer")],
                &["objective", "revision"],
            ),
        ),
        builtin_descriptor(
            "goal.pause",
            &[],
            Session,
            &[Write],
            SessionState,
            "暂停会话持久目标",
            "goal pause",
            object_schema(&[], &[]),
        ),
        builtin_descriptor(
            "goal.resume",
            &[],
            Session,
            &[Write],
            SessionState,
            "恢复会话持久目标",
            "goal resume",
            object_schema(&[], &[]),
        ),
        builtin_descriptor(
            "goal.complete",
            &[],
            Session,
            &[Write],
            SessionState,
            "以真实验证证据完成目标",
            "goal complete <verification>",
            object_schema(&[("verification", "string")], &["verification"]),
        ),
        builtin_descriptor(
            "goal.block",
            &[],
            Session,
            &[Write],
            SessionState,
            "标记目标阻塞",
            "goal block <note>",
            object_schema(&[("note", "string")], &["note"]),
        ),
        builtin_descriptor(
            "goal.clear",
            &[],
            Session,
            &[Write],
            SessionState,
            "清除会话持久目标",
            "goal clear",
            object_schema(&[], &[]),
        ),
        // 上下文来源治理（capability-parity 6.3）：文件 / 记忆 / 检索 / 技能四类来源的
        // 清单、单项详情、单来源刷新与自动注入开关。通道 `/context`、CLI `exm context`、
        // WebUI 共用同一组规范名——各入口只是呈现不同，事实源在 Core。
        builtin_descriptor(
            "context.list",
            &["context"],
            Session,
            &[Read],
            None,
            "列出会话上下文来源（文件/记忆/检索/技能）及状态、禁用位与缺失原因",
            "context list",
            object_schema(&[], &[]),
        ),
        builtin_descriptor(
            "context.detail",
            &[],
            Session,
            &[Read],
            None,
            "查看单个上下文来源的状态、原状态与缺失原因",
            "context detail <key>",
            object_schema(&[("key", "string")], &["key"]),
        ),
        // 刷新只重新观测/重读，不改任何状态（禁用清单不动）——无副作用
        builtin_descriptor(
            "context.refresh",
            &[],
            Session,
            &[Read],
            None,
            "重新读取单个来源（文件重读；记忆/检索/技能重新观测）。失败只影响该来源",
            "context refresh <key>",
            object_schema(&[("key", "string")], &["key"]),
        ),
        builtin_descriptor(
            "context.disable",
            &[],
            Session,
            &[Write],
            SessionState,
            "停用来源的自动注入（模型主动召回与用户显式 /skill 不受影响）",
            "context disable <key>",
            object_schema(&[("key", "string")], &["key"]),
        ),
        builtin_descriptor(
            "context.enable",
            &[],
            Session,
            &[Write],
            SessionState,
            "恢复来源的自动注入",
            "context enable <key>",
            object_schema(&[("key", "string")], &["key"]),
        ),
        builtin_descriptor(
            "admin.diagnostics",
            &[],
            Global,
            &[Admin],
            None,
            "查看命令目录诊断",
            "admin diagnostics",
            object_schema(&[], &[]),
        ),
    ]
}

struct RedactedString<'a>(&'a str);

impl Serialize for RedactedString<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&redact_str(self.0))
    }
}

struct RedactedJson<'a>(&'a Value);

impl Serialize for RedactedJson<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        redact_json(self.0).serialize(serializer)
    }
}

struct CommandErrorRef<'a>(&'a CommandError);

impl Serialize for CommandErrorRef<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut state = serializer.serialize_struct("CommandError", 3)?;
        state.serialize_field("category", &self.0.category)?;
        state.serialize_field("message", &RedactedString(&self.0.message))?;
        if let Some(details) = &self.0.details {
            state.serialize_field("details", &RedactedJson(details))?;
        }
        state.end()
    }
}

struct CommandEventRef<'a>(&'a CommandEvent);

impl Serialize for CommandEventRef<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut state = serializer.serialize_struct("CommandEvent", 2)?;
        state.serialize_field("kind", &RedactedString(&self.0.kind))?;
        state.serialize_field("data", &RedactedJson(&self.0.data))?;
        state.end()
    }
}

struct CommandAuditReceiptRef<'a>(&'a CommandAuditReceipt);

impl Serialize for CommandAuditReceiptRef<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut state = serializer.serialize_struct("CommandAuditReceipt", 2)?;
        state.serialize_field("eventId", &RedactedString(&self.0.event_id))?;
        state.serialize_field("occurredAt", &RedactedString(&self.0.occurred_at))?;
        state.end()
    }
}

struct CommandNextActionRef<'a>(&'a CommandNextAction);

impl Serialize for CommandNextActionRef<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut state = serializer.serialize_struct("CommandNextAction", 3)?;
        state.serialize_field("command", &RedactedString(&self.0.command))?;
        state.serialize_field("label", &RedactedString(&self.0.label))?;
        state.serialize_field("args", &RedactedJson(&self.0.args))?;
        state.end()
    }
}

impl Serialize for CommandResult {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut state = serializer.serialize_struct("CommandResult", 10)?;
        state.serialize_field("protocolVersion", &self.protocol_version)?;
        state.serialize_field("requestId", &RedactedString(&self.request_id))?;
        state.serialize_field("command", &RedactedString(&self.command))?;
        state.serialize_field("status", &self.status)?;
        state.serialize_field("ok", &self.ok)?;
        state.serialize_field("data", &RedactedJson(&self.data))?;
        if let Some(error) = &self.error {
            state.serialize_field("error", &CommandErrorRef(error))?;
        }
        if !self.events.is_empty() {
            let events: Vec<_> = self.events.iter().map(CommandEventRef).collect();
            state.serialize_field("events", &events)?;
        }
        if let Some(audit) = &self.audit {
            state.serialize_field("audit", &CommandAuditReceiptRef(audit))?;
        }
        if let Some(next_action) = &self.next_action {
            state.serialize_field("nextAction", &CommandNextActionRef(next_action))?;
        }
        state.end()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn request(command: &str, entry: CommandEntry, args: Value) -> CommandRequest {
        CommandRequest {
            protocol_version: COMMAND_PROTOCOL_VERSION,
            request_id: "req-fixture-1".into(),
            command: command.into(),
            command_version: 1,
            args,
            session: Some("session-fixture".into()),
            project: Some("project-fixture".into()),
            flow: Some("flow-fixture".into()),
            actor: CommandActor {
                id: "user-fixture".into(),
                kind: CommandActorKind::User,
                roles: vec!["operator".into()],
            },
            entry,
            mode: CommandExecutionMode::Execute,
            output_format: CommandOutputFormat::Json,
        }
    }

    fn test_descriptor(name: &str, aliases: &[&str]) -> CommandDescriptor {
        builtin_descriptor(
            name,
            aliases,
            CommandScope::Project,
            &[CommandPermission::Write],
            CommandSideEffect::ProjectState,
            "测试命令",
            "test <value>",
            object_schema(&[("value", "string")], &["value"]),
        )
    }

    fn directory_query(entry: CommandEntry, roles: &[&str]) -> CommandDirectoryQuery {
        CommandDirectoryQuery {
            protocol_version: COMMAND_PROTOCOL_VERSION,
            entry,
            actor: CommandActor {
                id: "test-actor".into(),
                kind: CommandActorKind::User,
                roles: roles.iter().map(|role| (*role).into()).collect(),
            },
            scope: Some(CommandScope::Project),
            session: None,
            project: Some("test-project".into()),
            flow: None,
            capabilities: vec![CapabilityItem {
                name: "test.capability".into(),
                status: CapabilityStatus::Available,
                reason: None,
            }],
            include_deprecated: false,
        }
    }

    fn result(request: &CommandRequest, data: Value) -> CommandResult {
        let mut result = CommandResult::success(request, CommandStatus::Succeeded, data);
        result.audit = Some(CommandAuditReceipt {
            event_id: "audit-fixture-1".into(),
            occurred_at: "2026-11-21T00:00:00.000Z".into(),
        });
        result
    }

    #[derive(Clone, Copy)]
    enum TransportFixture {
        MockCli,
        LoopbackChannel,
        Http,
    }

    impl TransportFixture {
        fn request_round_trip(self, request: &CommandRequest) -> CommandRequest {
            let bytes = match self {
                Self::MockCli | Self::Http => serde_json::to_vec(request),
                Self::LoopbackChannel => serde_json::to_vec(&json!({
                    "type": "command.request",
                    "payload": request,
                })),
            }
            .expect("serialize request fixture");

            match self {
                Self::MockCli | Self::Http => serde_json::from_slice(&bytes),
                Self::LoopbackChannel => {
                    let frame: Value =
                        serde_json::from_slice(&bytes).expect("decode loopback frame");
                    serde_json::from_value(frame["payload"].clone())
                }
            }
            .expect("deserialize request fixture")
        }

        fn result_round_trip(self, result: &CommandResult) -> CommandResult {
            let bytes = match self {
                Self::MockCli | Self::Http => serde_json::to_vec(result),
                Self::LoopbackChannel => serde_json::to_vec(&json!({
                    "type": "command.result",
                    "payload": result,
                })),
            }
            .expect("serialize result fixture");

            match self {
                Self::MockCli | Self::Http => serde_json::from_slice(&bytes),
                Self::LoopbackChannel => {
                    let frame: Value =
                        serde_json::from_slice(&bytes).expect("decode loopback frame");
                    serde_json::from_value(frame["payload"].clone())
                }
            }
            .expect("deserialize result fixture")
        }
    }

    /// 通道使用事件帧包装，其余两个夹具直接使用 JSON body/输出；三者最终都落到
    /// 同一协议结构。领域 handler 不在此任务中模拟。
    fn round_trip<T>(value: &T) -> (Vec<u8>, T)
    where
        T: Serialize + for<'de> Deserialize<'de>,
    {
        let bytes = serde_json::to_vec(value).expect("serialize fixture");
        let decoded = serde_json::from_slice(&bytes).expect("deserialize fixture");
        (bytes, decoded)
    }

    #[test]
    fn 核心命令请求在入口夹具中往返且结果语义一致() {
        let transports = [
            ("mock CLI", TransportFixture::MockCli, CommandEntry::Cli),
            (
                "loopback channel",
                TransportFixture::LoopbackChannel,
                CommandEntry::Chat,
            ),
            ("HTTP fixture", TransportFixture::Http, CommandEntry::Http),
        ];
        let cases = [
            ("model.use", json!({"profileId":"fast"})),
            (
                "approval.decide",
                json!({"approvalId":"approval-fixture","decision":"approve"}),
            ),
            ("queue.add", json!({"text":"修复隔离项目中的测试"})),
            (
                "plan.approve",
                json!({"planId":"plan-fixture","revision":2}),
            ),
        ];

        for (command, args) in cases {
            let mut canonical_result: Option<CommandResult> = None;
            for (transport_name, fixture, entry) in transports {
                let request = request(command, entry, args.clone());
                let decoded_request = fixture.request_round_trip(&request);
                assert_eq!(
                    decoded_request, request,
                    "{transport_name} request round trip"
                );

                let response = result(
                    &request,
                    json!({"state":"succeeded","resourceId":"fixture-resource"}),
                );
                let decoded_result = fixture.result_round_trip(&response);
                assert_eq!(
                    decoded_result, response,
                    "{transport_name} result round trip"
                );
                if let Some(canonical) = &canonical_result {
                    assert_eq!(
                        &decoded_result, canonical,
                        "{transport_name} returned different command semantics"
                    );
                } else {
                    canonical_result = Some(decoded_result);
                }
            }
        }
    }

    #[test]
    fn result_wire_deterministically_redacts_sensitive_fields_everywhere() {
        let request = request("config.update", CommandEntry::Http, json!({}));
        let mut result = CommandResult::failure(
            &request,
            CommandErrorCategory::InvalidArgs,
            "invalid token Bearer superSecretValue123456",
            Some(json!({"api_key":"sk-proj-privateValue1234567890","field":"api_key"})),
        );
        result.data = json!({"nested":{"clientSecret":"actual-client-secret","ok":true}});
        result.events.push(CommandEvent {
            kind: "config.updated".into(),
            data: json!({"token":"event-secret","message":"Bearer eventToken123456789"}),
        });
        result.next_action = Some(CommandNextAction {
            command: "config.show".into(),
            label: "查看设置".into(),
            args: json!({"password":"dont-return-this"}),
        });

        let wire = serde_json::to_string(&result).expect("serialize result");
        for secret in [
            "superSecretValue123456",
            "privateValue1234567890",
            "actual-client-secret",
            "event-secret",
            "eventToken123456789",
            "dont-return-this",
        ] {
            assert!(
                !wire.contains(secret),
                "serialized result leaked {secret}: {wire}"
            );
        }
        let decoded: CommandResult = serde_json::from_str(&wire).expect("deserialize result");
        assert_eq!(
            decoded.error.as_ref().unwrap().category,
            CommandErrorCategory::InvalidArgs
        );
        assert_eq!(
            decoded.error.as_ref().unwrap().details.as_ref().unwrap()["api_key"],
            "***"
        );
        assert_eq!(decoded.data["nested"]["clientSecret"], "***");
        assert_eq!(decoded.events[0].data["token"], "***");
        assert_eq!(
            decoded.next_action.as_ref().unwrap().args["password"],
            "***"
        );
    }

    #[test]
    fn 协议版本不匹配可被确定性识别() {
        let mut request = request("model.use", CommandEntry::Cli, json!({"profileId":"fast"}));
        assert!(request.has_supported_protocol());
        request.protocol_version = COMMAND_PROTOCOL_VERSION + 1;
        assert!(!request.has_supported_protocol());

        let snapshot = CapabilitySnapshot {
            protocol_version: COMMAND_PROTOCOL_VERSION,
            snapshot_id: "caps-fixture".into(),
            generated_at: "2026-11-21T00:00:00.000Z".into(),
            entry: CommandEntry::Cli,
            actor: request.actor,
            session: request.session,
            project: request.project,
            flow: request.flow,
            capabilities: vec![CapabilityItem {
                name: "model.use".into(),
                status: CapabilityStatus::Available,
                reason: None,
            }],
            commands: vec!["model.use".into()],
        };
        let (_, decoded): (Vec<u8>, CapabilitySnapshot) = round_trip(&snapshot);
        assert!(decoded.has_supported_protocol());
        assert_eq!(decoded, snapshot);
    }

    #[test]
    fn registry_registration_reports_name_alias_version_and_deprecation_diagnostics() {
        let mut registry = CommandRegistry::new(COMMAND_PROTOCOL_VERSION);
        let first = test_descriptor("test.first", &["first"]);
        registry
            .register(first.clone())
            .expect("register first command");

        let duplicate_name = registry.register(first);
        assert_eq!(duplicate_name.unwrap_err().code, "duplicate_command_name");
        assert_eq!(
            registry.descriptors().count(),
            1,
            "duplicate must not mutate catalog"
        );

        let duplicate_alias = registry.register(test_descriptor("test.second", &["first"]));
        assert_eq!(duplicate_alias.unwrap_err().code, "duplicate_command_alias");
        assert!(
            registry.resolve("test.second").is_none(),
            "failed registration is atomic"
        );

        let mut duplicate_local_alias = test_descriptor("test.third", &["third"]);
        duplicate_local_alias.aliases = vec!["same".into(), "same".into()];
        assert_eq!(
            registry.register(duplicate_local_alias).unwrap_err().code,
            "duplicate_command_alias"
        );

        let mut unknown_version = test_descriptor("test.future", &[]);
        unknown_version.version = COMMAND_DESCRIPTOR_VERSION + 1;
        assert_eq!(
            registry.register(unknown_version).unwrap_err().code,
            "unknown_command_version"
        );

        let mut deprecated = test_descriptor("test.legacy", &["legacy"]);
        deprecated.deprecated = true;
        deprecated.replacement = Some("test.first".into());
        registry
            .register(deprecated)
            .expect("deprecated alias may remain registered");
        assert!(registry
            .diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.code == "deprecated_command"));
        assert_eq!(registry.resolve("legacy").unwrap().name, "test.legacy");

        let name_alias_conflict = registry.register(test_descriptor("first", &[]));
        assert_eq!(
            name_alias_conflict.unwrap_err().code,
            "command_name_alias_conflict"
        );

        let default_query = registry.query(&directory_query(CommandEntry::Cli, &["owner"]));
        assert!(!default_query
            .entries
            .iter()
            .any(|entry| entry.descriptor.name == "test.legacy"));
        let mut include_deprecated = directory_query(CommandEntry::Cli, &["owner"]);
        include_deprecated.include_deprecated = true;
        let deprecated_entry = registry
            .query(&include_deprecated)
            .entries
            .into_iter()
            .find(|entry| entry.descriptor.name == "test.legacy")
            .expect("deprecated command is inspectable on request");
        assert!(!deprecated_entry.executable);
        assert_eq!(
            deprecated_entry.unavailable_reason,
            Some(CommandAvailabilityReason::Deprecated)
        );
        assert_eq!(deprecated_entry.replacement.as_deref(), Some("test.first"));
    }

    #[test]
    fn registry_query_is_read_only_and_filters_entry_identity_scope_and_capability() {
        let mut registry = CommandRegistry::new(COMMAND_PROTOCOL_VERSION);
        let mut descriptor = test_descriptor("test.workspace.write", &["workspace-write"]);
        descriptor.entries = vec![CommandEntry::Cli];
        descriptor.required_capabilities = vec!["test.capability".into()];
        registry.register(descriptor).expect("register descriptor");
        let before: Vec<_> = registry.descriptors().cloned().collect();

        let owner_query = directory_query(CommandEntry::Cli, &["owner"]);
        let available = registry.query(&owner_query);
        assert_eq!(available.entries.len(), 1);
        assert!(available.entries[0].executable);

        let chat = registry.query(&directory_query(CommandEntry::Chat, &["owner"]));
        assert_eq!(
            chat.entries[0].unavailable_reason,
            Some(CommandAvailabilityReason::UnsupportedEntry)
        );

        let forbidden = registry.query(&directory_query(CommandEntry::Cli, &["reader"]));
        assert_eq!(
            forbidden.entries[0].unavailable_reason,
            Some(CommandAvailabilityReason::ForbiddenPermission)
        );

        let mut missing_project = owner_query.clone();
        missing_project.project = None;
        assert_eq!(
            registry.query(&missing_project).entries[0].unavailable_reason,
            Some(CommandAvailabilityReason::MissingScope)
        );

        let mut dependency_missing = owner_query.clone();
        dependency_missing.capabilities[0].status = CapabilityStatus::MissingDependency;
        assert_eq!(
            registry.query(&dependency_missing).entries[0].unavailable_reason,
            Some(CommandAvailabilityReason::MissingDependency)
        );

        let mut unknown_client = owner_query;
        unknown_client.protocol_version = COMMAND_PROTOCOL_VERSION + 1;
        let mismatch = registry.query(&unknown_client);
        assert!(mismatch.entries.is_empty());
        assert_eq!(
            mismatch.diagnostics.last().unwrap().code,
            "unknown_client_protocol_version"
        );
        assert_eq!(
            registry.descriptors().cloned().collect::<Vec<_>>(),
            before,
            "directory queries must not mutate registry state"
        );
    }

    #[test]
    fn builtin_directory_has_unique_aliases_and_common_command_families() {
        let registry = CommandRegistry::with_builtins().expect("built-in registry is valid");
        for command in [
            "help",
            "model.list",
            "model.use",
            "status",
            "sessions.new",
            "sessions.list",
            "sessions.resume",
            "stop",
            "steer",
            "queue.add",
            "skills.list",
            "cron.list",
            "approval.list",
            "approval.decide",
            "config.show",
            "config.update",
            "plan.status",
            "plan.approve",
            "spec.show",
            "spec.draft",
            "spec.edit",
            "spec.export",
            "spec.clear",
            "goal.status",
            "goal.start",
            "goal.clear",
            "context.list",
            "context.detail",
            "context.refresh",
            "context.disable",
            "context.enable",
        ] {
            assert!(registry.resolve(command).is_some(), "missing {command}");
        }
        assert_eq!(registry.resolve("model").unwrap().name, "model.list");
        assert_eq!(registry.resolve("queue").unwrap().name, "queue.add");
        assert_eq!(registry.resolve("spec").unwrap().name, "spec.show");
        assert_eq!(registry.resolve("context").unwrap().name, "context.list");
    }

    #[test]
    fn plugin_commands_are_namespaced_and_capability_state_controls_visibility() {
        let mut registry = CommandRegistry::with_builtins().expect("builtins");
        let name = registry
            .register_plugin_command(
                "weather-kit",
                "forecast",
                3,
                "查询天气插件预报",
                "weather forecast <city>",
                serde_json::json!({"type":"object","properties":{"city":{"type":"string"}}}),
                vec!["plugin.weather-kit.ready".into()],
            )
            .unwrap();
        assert_eq!(name, "plugin.weather-kit.forecast");
        assert_eq!(registry.resolve(&name).unwrap().owner, "plugin:weather-kit");

        assert_eq!(
            registry
                .register_plugin_command(
                    "sample",
                    "help",
                    1,
                    "覆盖核心",
                    "help",
                    serde_json::json!({"type":"object"}),
                    Vec::new(),
                )
                .unwrap(),
            "plugin.sample.help"
        );
        assert_eq!(registry.resolve("help").unwrap().owner, "core");
        let mut conflicting = registry.resolve(&name).unwrap().clone();
        conflicting.name = "plugin.weather-kit.other".into();
        conflicting.aliases = vec!["help".into()];
        assert_eq!(
            registry.register(conflicting).unwrap_err().code,
            "extension_namespace_required"
        );

        assert!(registry
            .register_plugin_command(
                "bad id",
                "forecast",
                1,
                "bad",
                "bad",
                serde_json::json!({}),
                Vec::new()
            )
            .is_err());
        assert!(registry
            .register_plugin_command(
                "weather-kit",
                "forecast",
                0,
                "bad",
                "bad",
                serde_json::json!({}),
                Vec::new()
            )
            .is_err());

        let mut query = directory_query(CommandEntry::Cli, &["owner"]);
        query.session = Some("session-fixture".into());
        query.scope = Some(CommandScope::Session);
        query.capabilities.push(CapabilityItem {
            name: "plugin.weather-kit.ready".into(),
            status: CapabilityStatus::Disabled,
            reason: Some("plugin disabled".into()),
        });
        query.capabilities.push(CapabilityItem {
            name: "command.handler.plugin.weather-kit.forecast".into(),
            status: CapabilityStatus::Available,
            reason: None,
        });
        let disabled = registry
            .query(&query)
            .entries
            .into_iter()
            .find(|entry| entry.descriptor.name == name)
            .unwrap();
        assert!(!disabled.executable);
        assert_eq!(
            disabled.unavailable_reason,
            Some(CommandAvailabilityReason::CapabilityDisabled)
        );
        query
            .capabilities
            .iter_mut()
            .find(|item| item.name == "plugin.weather-kit.ready")
            .unwrap()
            .status = CapabilityStatus::MissingDependency;
        let missing = registry
            .query(&query)
            .entries
            .into_iter()
            .find(|entry| entry.descriptor.name == name)
            .unwrap();
        assert_eq!(
            missing.unavailable_reason,
            Some(CommandAvailabilityReason::MissingDependency)
        );
    }

    #[test]
    fn plugin_manager_state_drives_dynamic_command_catalog_and_disabled_visibility() {
        let root =
            std::env::temp_dir().join(format!("exm-command-plugin-{}", crate::types::new_id()));
        let plugin_dir = root.join("weather-kit");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(
            plugin_dir.join("plugin.json"),
            serde_json::to_vec(&serde_json::json!({
                "id": "weather-kit",
                "name": "Weather Kit",
                "version": "1.2.0",
                "commands": { "forecast": { "version": 2 } },
                "tools": [{
                    "name": "forecast",
                    "description": "Forecast weather",
                    "parameters": {"type":"object","properties":{"city":{"type":"string"}}},
                    "target": "https://example.invalid/forecast"
                }]
            }))
            .unwrap(),
        )
        .unwrap();

        let enabled_manager = crate::plugin::PluginManager::new(&root);
        let (loaded, rejected) = enabled_manager.load_all(&BTreeMap::new());
        assert_eq!((loaded, rejected), (1, 0));
        let mut enabled_registry = CommandRegistry::with_builtins().unwrap();
        let enabled = enabled_registry.register_plugin_manager(&enabled_manager);
        assert_eq!(
            enabled_registry
                .resolve("plugin.weather-kit.forecast")
                .unwrap()
                .owner,
            "plugin:weather-kit"
        );
        let mut query = directory_query(CommandEntry::Cli, &["owner"]);
        query.session = Some("session-fixture".into());
        query.scope = Some(CommandScope::Session);
        query.capabilities = enabled;
        let visible = enabled_registry
            .query(&query)
            .entries
            .into_iter()
            .find(|entry| entry.descriptor.name == "plugin.weather-kit.forecast")
            .unwrap();
        assert!(
            !visible.executable,
            "plugin manifest alone does not install a command executor"
        );
        query.capabilities.push(CapabilityItem {
            name: "command.handler.plugin.weather-kit.forecast".into(),
            status: CapabilityStatus::Available,
            reason: None,
        });
        let with_handler = enabled_registry
            .query(&query)
            .entries
            .into_iter()
            .find(|entry| entry.descriptor.name == "plugin.weather-kit.forecast")
            .unwrap();
        assert!(with_handler.executable);

        let disabled_manager = crate::plugin::PluginManager::new(&root);
        let mut overrides = BTreeMap::new();
        overrides.insert("weather-kit".to_string(), false);
        disabled_manager.load_all(&overrides);
        let mut disabled_registry = CommandRegistry::with_builtins().unwrap();
        let disabled = disabled_registry.register_plugin_manager(&disabled_manager);
        query.capabilities = disabled;
        query.capabilities.push(CapabilityItem {
            name: "command.handler.plugin.weather-kit.forecast".into(),
            status: CapabilityStatus::Available,
            reason: None,
        });
        let unavailable = disabled_registry
            .query(&query)
            .entries
            .into_iter()
            .find(|entry| entry.descriptor.name == "plugin.weather-kit.forecast")
            .unwrap();
        assert!(!unavailable.executable);
        assert_eq!(
            unavailable.unavailable_reason,
            Some(CommandAvailabilityReason::CapabilityDisabled)
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn 拒绝类别稳定且不携带跨会话正文() {
        let request = request(
            "queue.edit",
            CommandEntry::Http,
            json!({
                "queueId": "queue-owned-by-other-session",
                "text": "other-session-private-instruction"
            }),
        );
        let categories = [
            (CommandErrorCategory::Forbidden, "forbidden"),
            (CommandErrorCategory::Unsupported, "unsupported"),
            (CommandErrorCategory::Conflict, "conflict"),
            (CommandErrorCategory::InvalidArgs, "invalid_args"),
            (CommandErrorCategory::VersionMismatch, "version_mismatch"),
        ];

        for (category, wire_category) in categories {
            let response = CommandResult::failure(
                &request,
                category,
                "请求不能在当前上下文执行",
                Some(json!({"resource":"queue-owned-by-other-session","revision":3})),
            );
            let wire = serde_json::to_value(&response).expect("serialize rejection");
            assert_eq!(wire["error"]["category"], wire_category);
            assert_eq!(wire["requestId"], "req-fixture-1");
            assert!(!wire
                .to_string()
                .contains("other-session-private-instruction"));
            assert!(!wire.to_string().contains("session-fixture"));
            assert!(!wire.to_string().contains("project-fixture"));
            assert!(!wire.to_string().contains("user-fixture"));
        }
    }

    /// capability-parity 3.6：loopback 事件夹具覆盖五类场景——
    /// 立即接收、不等待、不打断、明确入队、真实回执。
    /// 通道帧形态（{type, payload} 包装）往返后语义必须与规范回执一致：
    /// 回执只表达「已接收/steering/queued」，绝不出现 Succeeded 冒充完成。
    #[test]
    fn loopback夹具覆盖steering与队列五类场景回执() {
        let fixture = TransportFixture::LoopbackChannel;
        let entry = CommandEntry::Chat;

        // ① 立即接收 + ② 不等待：steer 运行中回执 = Accepted（非 Succeeded），
        //    interrupted=false（未打断），不携带最终答案字段
        let steer_req = request("steer", entry, json!({"instruction":"优先处理登录缺陷"}));
        let decoded = fixture.request_round_trip(&steer_req);
        assert_eq!(decoded.command, "steer");
        assert_eq!(decoded.args["instruction"], "优先处理登录缺陷");
        let mut steer_receipt = CommandResult::success(
            &decoded,
            CommandStatus::Accepted,
            json!({
                "status": "steering_accepted",
                "running": true,
                "interrupted": false,
            }),
        );
        steer_receipt.events.push(CommandEvent {
            kind: "steering.accepted".into(),
            data: json!({"source": "chat", "text": "优先处理登录缺陷"}),
        });
        let wire = serde_json::to_value(fixture.result_round_trip(&steer_receipt)).unwrap();
        assert_eq!(wire["status"], "accepted", "立即接收=Accepted: {wire}");
        assert_eq!(wire["data"]["status"], "steering_accepted");
        assert_eq!(wire["data"]["interrupted"], false, "不打断");
        assert!(
            wire.get("data").unwrap()["answer"].is_null(),
            "无最终答案字段"
        );
        assert_eq!(
            wire["events"][0]["kind"], "steering.accepted",
            "真实事件回执"
        );

        // ③ 不打断：stop 是独立命令——普通 steer 请求绝不携带取消语义
        assert!(
            !serde_json::to_string(&decoded).unwrap().contains("stop"),
            "steer 请求不得混入停止语义"
        );

        // ④ 明确入队：queue.add 回执 = Accepted + pending 状态 + 序号，
        //    不写成任务已执行完成
        let queue_req = request("queue.add", entry, json!({"instruction":"之后导出报告"}));
        let decoded_q = fixture.request_round_trip(&queue_req);
        assert_eq!(decoded_q.command, "queue.add");
        let mut queue_receipt = CommandResult::success(
            &decoded_q,
            CommandStatus::Accepted,
            json!({
                "status": "queued",
                "seq": 3,
                "pending": true,
            }),
        );
        queue_receipt.events.push(CommandEvent {
            kind: "task_queue.updated".into(),
            data: json!({"action": "add", "queued": 3}),
        });
        let wire_q = serde_json::to_value(fixture.result_round_trip(&queue_receipt)).unwrap();
        assert_eq!(wire_q["status"], "accepted", "入队=Accepted: {wire_q}");
        assert_eq!(wire_q["data"]["status"], "queued");
        assert_eq!(wire_q["data"]["pending"], true, "pending 不冒充完成");
        assert_eq!(wire_q["data"]["seq"], 3, "队列序号在回执里");
        assert_eq!(wire_q["events"][0]["kind"], "task_queue.updated");

        // ⑤ 真实回执：stop 命令的收束回执如实反映停止结果——
        //    Accepted + stoppedTools 计数，不伪造成功完成
        let stop_req = request("stop", entry, json!({}));
        let decoded_s = fixture.request_round_trip(&stop_req);
        assert_eq!(decoded_s.command, "stop");
        let stop_receipt = CommandResult::success(
            &decoded_s,
            CommandStatus::Accepted,
            json!({"status": "accepted", "stoppedTools": 2}),
        );
        let wire_s = serde_json::to_value(fixture.result_round_trip(&stop_receipt)).unwrap();
        assert_eq!(wire_s["status"], "accepted");
        assert_eq!(wire_s["data"]["stoppedTools"], 2, "停止回执带真实计数");

        // 五类场景共用契约：回执 requestId 与请求一一对应（可审计链）
        for (w, r) in [
            (&wire, &steer_req),
            (&wire_q, &queue_req),
            (&wire_s, &stop_req),
        ] {
            assert_eq!(w["requestId"], r.request_id);
        }
    }
}
