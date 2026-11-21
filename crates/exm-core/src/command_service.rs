//! 统一命令执行门面：入口提供可信 transport context，Core 做目录解析、参数校验、
//! 作用域绑定、授权预检、审计 admission，再调用领域 handler。
//!
//! 该模块不包含模型、会话、配置、队列或审批领域实现。领域 handler 由 Core/网关
//! 注入；这样 CLI、聊天和 HTTP 适配器只能改变输入解析和呈现，不能复制业务逻辑。

use crate::command::{
    CapabilityItem, CapabilityStatus, CommandActor, CommandAuditReceipt, CommandDescriptor,
    CommandDiagnosticSeverity, CommandDirectoryQuery, CommandDirectorySnapshot, CommandEntry,
    CommandError, CommandErrorCategory, CommandEvent, CommandExecutionMode, CommandNextAction,
    CommandRegistry, CommandRegistryDiagnostic, CommandRequest, CommandResult, CommandScope,
    CommandSideEffect, CommandStatus, COMMAND_PROTOCOL_VERSION,
};
use crate::tools::redact_str;
use crate::types::{new_id, now_iso};
use async_trait::async_trait;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::Arc;

/// 由认证/通道适配器构造的可信上下文。客户端请求中的 actor 和 entry 会被覆盖，
/// resource id 只有与上下文绑定目标一致时才会接受。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandTransportContext {
    pub entry: CommandEntry,
    pub actor: CommandActor,
    pub authenticated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flow: Option<String>,
    #[serde(default)]
    pub capabilities: Vec<CapabilityItem>,
}

impl CommandTransportContext {
    pub fn directory_query(
        &self,
        scope: Option<CommandScope>,
        include_deprecated: bool,
    ) -> CommandDirectoryQuery {
        CommandDirectoryQuery {
            protocol_version: COMMAND_PROTOCOL_VERSION,
            entry: self.entry,
            actor: self.actor.clone(),
            scope,
            session: self.session.clone(),
            project: self.project.clone(),
            flow: self.flow.clone(),
            capabilities: self.capabilities.clone(),
            include_deprecated,
        }
    }
}

/// 副作用 handler 只接收已经 canonicalize、绑定可信上下文并通过预检的请求。
#[async_trait]
pub trait CommandHandler: Send + Sync {
    async fn execute(&self, request: &CommandRequest) -> Result<CommandExecution, CommandError>;
}

#[derive(Debug, Clone, PartialEq)]
pub struct CommandExecution {
    pub status: CommandStatus,
    pub data: Value,
    pub events: Vec<CommandEvent>,
    pub next_action: Option<CommandNextAction>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandAuditPhase {
    Rejected,
    Started,
    Completed,
}

impl CommandExecution {
    pub fn succeeded(data: Value) -> Self {
        Self {
            status: CommandStatus::Succeeded,
            data,
            events: Vec::new(),
            next_action: None,
        }
    }

    pub fn accepted(data: Value) -> Self {
        Self {
            status: CommandStatus::Accepted,
            data,
            events: Vec::new(),
            next_action: None,
        }
    }
}

/// 审计 admission 不保存原始 args，只保存字段名，避免命令审计成为凭据或跨会话
/// 正文的泄漏通道。具体领域结果仍由 `CommandResult` 的脱敏序列化输出。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandAuditRecord {
    pub event_id: String,
    pub request_id: String,
    pub command: String,
    pub entry: CommandEntry,
    pub actor_id: String,
    pub scope: CommandScope,
    pub side_effect: CommandSideEffect,
    pub phase: CommandAuditPhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<CommandStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_category: Option<CommandErrorCategory>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flow: Option<String>,
    #[serde(default)]
    pub argument_keys: Vec<String>,
    pub occurred_at: String,
}

#[async_trait]
pub trait CommandAuditSink: Send + Sync {
    async fn record(&self, record: CommandAuditRecord) -> anyhow::Result<()>;
}

/// 供 Core/gateway 注入真实事件存储的最小内存 sink，同时作为离线夹具使用。
#[derive(Debug, Default)]
pub struct MemoryCommandAuditSink {
    records: Mutex<Vec<CommandAuditRecord>>,
}

impl MemoryCommandAuditSink {
    pub fn records(&self) -> Vec<CommandAuditRecord> {
        self.records.lock().clone()
    }
}

#[async_trait]
impl CommandAuditSink for MemoryCommandAuditSink {
    async fn record(&self, record: CommandAuditRecord) -> anyhow::Result<()> {
        self.records.lock().push(record);
        Ok(())
    }
}

/// 适用于只读离线查询或暂未配置持久审计 sink 的测试。生产副作用服务不应使用
/// 该实现，而应注入可失败的持久 sink，以保留 fail-closed 行为。
#[derive(Debug, Default)]
pub struct NoopCommandAuditSink;

#[async_trait]
impl CommandAuditSink for NoopCommandAuditSink {
    async fn record(&self, _record: CommandAuditRecord) -> anyhow::Result<()> {
        Ok(())
    }
}

pub struct CommandService {
    registry: Arc<CommandRegistry>,
    audit: Arc<dyn CommandAuditSink>,
    handlers: BTreeMap<String, Arc<dyn CommandHandler>>,
}

impl CommandService {
    pub fn new(registry: Arc<CommandRegistry>, audit: Arc<dyn CommandAuditSink>) -> Self {
        Self {
            registry,
            audit,
            handlers: BTreeMap::new(),
        }
    }

    pub fn builtin(audit: Arc<dyn CommandAuditSink>) -> anyhow::Result<Self> {
        Ok(Self::new(
            Arc::new(CommandRegistry::with_builtins().map_err(|diagnostic| {
                anyhow::anyhow!("内建命令目录注册失败: {}", diagnostic.code)
            })?),
            audit,
        ))
    }

    pub fn registry(&self) -> &CommandRegistry {
        &self.registry
    }

    /// 将领域 handler 绑定到唯一规范命令 owner；入口调用方不能在 dispatch 时
    /// 临时传入另一个 handler 来替换目录 owner。
    pub fn register_handler(
        &mut self,
        command_name: &str,
        handler: Arc<dyn CommandHandler>,
    ) -> Result<(), CommandRegistryDiagnostic> {
        let Some(descriptor) = self.registry.resolve(command_name) else {
            return Err(CommandRegistryDiagnostic {
                severity: CommandDiagnosticSeverity::Error,
                category: CommandErrorCategory::Unsupported,
                code: "handler_command_not_registered".into(),
                command: command_name.into(),
                alias: None,
                message: "handler 只能绑定已注册的规范命令".into(),
                replacement: None,
            });
        };
        if descriptor.name != command_name {
            return Err(CommandRegistryDiagnostic {
                severity: CommandDiagnosticSeverity::Error,
                category: CommandErrorCategory::Conflict,
                code: "handler_requires_canonical_name".into(),
                command: descriptor.name.clone(),
                alias: Some(command_name.into()),
                message: "handler 必须绑定规范命令名，别名只负责入口解析".into(),
                replacement: Some(descriptor.name.clone()),
            });
        }
        if self.handlers.contains_key(command_name) {
            return Err(CommandRegistryDiagnostic {
                severity: CommandDiagnosticSeverity::Error,
                category: CommandErrorCategory::Conflict,
                code: "duplicate_command_handler".into(),
                command: command_name.into(),
                alias: None,
                message: "规范命令已经绑定 handler，禁止覆盖 owner".into(),
                replacement: None,
            });
        }
        self.handlers.insert(command_name.into(), handler);
        Ok(())
    }

    pub fn directory(
        &self,
        context: &CommandTransportContext,
        scope: Option<CommandScope>,
        include_deprecated: bool,
    ) -> CommandDirectorySnapshot {
        if !context.authenticated {
            return CommandDirectorySnapshot {
                protocol_version: COMMAND_PROTOCOL_VERSION,
                entries: Vec::new(),
                diagnostics: vec![CommandRegistryDiagnostic {
                    severity: CommandDiagnosticSeverity::Error,
                    category: CommandErrorCategory::Forbidden,
                    code: "unauthenticated_context".into(),
                    command: String::new(),
                    alias: None,
                    message: "当前入口身份未认证，不能查询可执行命令目录".into(),
                    replacement: None,
                }],
            };
        }
        let mut query = context.directory_query(scope, include_deprecated);
        query.capabilities.extend(self.handler_capabilities());
        self.registry.query(&query)
    }

    fn handler_capabilities(&self) -> Vec<CapabilityItem> {
        self.handlers
            .keys()
            .map(|name| CapabilityItem {
                name: format!("command.handler.{name}"),
                status: CapabilityStatus::Available,
                reason: None,
            })
            .collect()
    }

    /// 统一执行路径：预检失败时绝不调用 handler；审计 admission 失败时 fail-closed。
    pub async fn dispatch(
        &self,
        request: CommandRequest,
        context: &CommandTransportContext,
    ) -> CommandResult {
        if request.protocol_version != COMMAND_PROTOCOL_VERSION {
            return self
                .reject(
                    &request,
                    context,
                    CommandErrorCategory::VersionMismatch,
                    "命令协议版本不受支持",
                    Some(json!({"supportedVersion": COMMAND_PROTOCOL_VERSION})),
                    CommandScope::Global,
                    CommandSideEffect::None,
                )
                .await;
        }
        if !context.authenticated || context.actor.id.trim().is_empty() {
            return self
                .reject(
                    &request,
                    context,
                    CommandErrorCategory::Forbidden,
                    "当前入口身份未认证",
                    None,
                    CommandScope::Global,
                    CommandSideEffect::None,
                )
                .await;
        }

        let Some(descriptor) = self.registry.resolve(&request.command).cloned() else {
            return self
                .reject(
                    &request,
                    context,
                    CommandErrorCategory::Unsupported,
                    "命令不在当前目录中",
                    None,
                    CommandScope::Global,
                    CommandSideEffect::None,
                )
                .await;
        };
        if descriptor.deprecated {
            return self
                .reject(
                    &request,
                    context,
                    CommandErrorCategory::Unsupported,
                    "命令已弃用",
                    descriptor
                        .replacement
                        .as_ref()
                        .map(|replacement| json!({"replacement": replacement})),
                    descriptor.scope,
                    descriptor.side_effect,
                )
                .await;
        }
        if request.command_version != descriptor.version {
            return self
                .reject(
                    &request,
                    context,
                    CommandErrorCategory::VersionMismatch,
                    "命令描述版本不受支持",
                    Some(json!({"supportedVersion": descriptor.version})),
                    descriptor.scope,
                    descriptor.side_effect,
                )
                .await;
        }

        let mut bound = request.clone();
        bound.command = descriptor.name.clone();
        bound.entry = context.entry;
        bound.actor = context.actor.clone();
        if let Err((category, field)) = bind_target(&mut bound.session, &context.session, "session")
        {
            return self
                .reject(
                    &bound,
                    context,
                    category,
                    "请求的资源不属于当前入口上下文",
                    Some(json!({"field": field})),
                    descriptor.scope,
                    descriptor.side_effect,
                )
                .await;
        }
        if let Err((category, field)) = bind_target(&mut bound.project, &context.project, "project")
        {
            return self
                .reject(
                    &bound,
                    context,
                    category,
                    "请求的资源不属于当前入口上下文",
                    Some(json!({"field": field})),
                    descriptor.scope,
                    descriptor.side_effect,
                )
                .await;
        }
        if let Err((category, field)) = bind_target(&mut bound.flow, &context.flow, "flow") {
            return self
                .reject(
                    &bound,
                    context,
                    category,
                    "请求的资源不属于当前入口上下文",
                    Some(json!({"field": field})),
                    descriptor.scope,
                    descriptor.side_effect,
                )
                .await;
        }

        if let Some(error) = preflight_error(
            &self.registry,
            &descriptor,
            &bound,
            context,
            &self.handler_capabilities(),
        ) {
            return self
                .reject(
                    &bound,
                    context,
                    error.category,
                    &error.message,
                    error.details,
                    descriptor.scope,
                    descriptor.side_effect,
                )
                .await;
        }
        if let Err(error) = validate_arguments(&descriptor, &bound.args) {
            return self
                .reject(
                    &bound,
                    context,
                    CommandErrorCategory::InvalidArgs,
                    &error.message,
                    Some(json!({"field": error.field, "expected": error.expected})),
                    descriptor.scope,
                    descriptor.side_effect,
                )
                .await;
        }
        let Some(handler) = self.handlers.get(&descriptor.name).cloned() else {
            return self
                .reject(
                    &bound,
                    context,
                    CommandErrorCategory::Unsupported,
                    "命令已登记但当前尚未接入执行 handler",
                    None,
                    descriptor.scope,
                    descriptor.side_effect,
                )
                .await;
        };

        let mut audit = self.audit_record(
            &bound,
            context,
            &descriptor,
            CommandAuditPhase::Started,
            None,
            None,
        );
        let started_receipt = audit_receipt(&audit);
        if self.audit.record(audit.clone()).await.is_err() {
            return CommandResult::failure(
                &bound,
                CommandErrorCategory::Unavailable,
                "命令审计暂不可用，未执行副作用",
                None,
            );
        }

        if bound.mode == CommandExecutionMode::Preview
            && descriptor.side_effect != CommandSideEffect::None
        {
            let mut result = CommandResult::success(
                &bound,
                CommandStatus::Pending,
                json!({
                    "preview": true,
                    "command": descriptor.name,
                    "sideEffect": descriptor.side_effect,
                }),
            );
            result.audit = Some(started_receipt);
            return result;
        };

        let execution = match handler.execute(&bound).await {
            Ok(execution) => execution,
            Err(error) => {
                audit.event_id = new_id();
                audit.phase = CommandAuditPhase::Completed;
                audit.status = Some(CommandStatus::Failed);
                audit.error_category = Some(error.category);
                let final_receipt = if self.audit.record(audit.clone()).await.is_ok() {
                    audit_receipt(&audit)
                } else {
                    started_receipt
                };
                let mut result =
                    CommandResult::failure(&bound, error.category, error.message, error.details);
                result.audit = Some(final_receipt);
                return result;
            }
        };
        let mut result = CommandResult::success(&bound, execution.status, execution.data);
        result.events = execution.events;
        result.next_action = execution.next_action;
        audit.event_id = new_id();
        audit.phase = CommandAuditPhase::Completed;
        audit.status = Some(result.status);
        audit.error_category = None;
        result.audit = Some(if self.audit.record(audit.clone()).await.is_ok() {
            audit_receipt(&audit)
        } else {
            result.events.push(CommandEvent {
                kind: "command.audit_completion_pending".into(),
                data: json!({"requestId": bound.request_id}),
            });
            started_receipt
        });
        result
    }

    async fn reject(
        &self,
        request: &CommandRequest,
        context: &CommandTransportContext,
        category: CommandErrorCategory,
        message: &str,
        details: Option<Value>,
        scope: CommandScope,
        side_effect: CommandSideEffect,
    ) -> CommandResult {
        let mut result = CommandResult::failure(request, category, message, details);
        let mut record = self.audit_record(
            request,
            context,
            &CommandDescriptor {
                protocol_version: COMMAND_PROTOCOL_VERSION,
                name: request.command.clone(),
                aliases: Vec::new(),
                version: request.command_version,
                summary: String::new(),
                usage: String::new(),
                argument_schema: Value::Null,
                scope,
                permissions: Vec::new(),
                side_effect,
                entries: vec![context.entry],
                output_formats: Vec::new(),
                error_categories: Vec::new(),
                required_capabilities: Vec::new(),
                owner: "core".into(),
                deprecated: false,
                replacement: None,
            },
            CommandAuditPhase::Rejected,
            Some(CommandStatus::Rejected),
            Some(category),
        );
        record.argument_keys.clear();
        if self.audit.record(record.clone()).await.is_ok() {
            result.audit = Some(audit_receipt(&record));
        }
        result.events.push(CommandEvent {
            kind: "command.rejected".into(),
            data: json!({"category": category}),
        });
        result
    }

    fn audit_record(
        &self,
        request: &CommandRequest,
        context: &CommandTransportContext,
        descriptor: &CommandDescriptor,
        phase: CommandAuditPhase,
        status: Option<CommandStatus>,
        error_category: Option<CommandErrorCategory>,
    ) -> CommandAuditRecord {
        CommandAuditRecord {
            event_id: new_id(),
            request_id: redact_str(&request.request_id),
            command: redact_str(&descriptor.name),
            entry: context.entry,
            actor_id: redact_str(&context.actor.id),
            scope: descriptor.scope,
            side_effect: descriptor.side_effect,
            phase,
            status,
            error_category,
            session: context.session.as_deref().map(redact_str),
            project: context.project.as_deref().map(redact_str),
            flow: context.flow.as_deref().map(redact_str),
            argument_keys: if phase == CommandAuditPhase::Started {
                argument_keys(&request.args)
                    .into_iter()
                    .map(|key| redact_str(&key))
                    .collect()
            } else {
                Vec::new()
            },
            occurred_at: now_iso(),
        }
    }

    /// 人类输出和 JSON 输出都由同一个结果对象派生；人类路径不能绕过结果脱敏。
    pub fn render_human(result: &CommandResult) -> String {
        let value = serde_json::to_value(result).unwrap_or_else(|_| {
            json!({
                "ok": false,
                "status": "failed",
                "error": {"category": "internal", "message": "结果渲染失败"}
            })
        });
        let command = value["command"].as_str().unwrap_or("命令");
        if result.ok {
            let content = value["data"]
                .get("message")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| value["data"].to_string());
            format!("{command}: {content}")
        } else {
            format!(
                "{} [{}]: {}",
                command,
                value["error"]["category"].as_str().unwrap_or("internal"),
                value["error"]["message"].as_str().unwrap_or("命令失败")
            )
        }
    }

    pub fn render_json(result: &CommandResult) -> String {
        serde_json::to_string(result).unwrap_or_else(|_| {
            "{\"ok\":false,\"status\":\"failed\",\"error\":{\"category\":\"internal\",\"message\":\"结果渲染失败\"}}".into()
        })
    }

    pub fn render(result: &CommandResult, format: crate::command::CommandOutputFormat) -> String {
        match format {
            crate::command::CommandOutputFormat::Human => Self::render_human(result),
            crate::command::CommandOutputFormat::Json => Self::render_json(result),
        }
    }
}

fn audit_receipt(record: &CommandAuditRecord) -> CommandAuditReceipt {
    CommandAuditReceipt {
        event_id: record.event_id.clone(),
        occurred_at: record.occurred_at.clone(),
    }
}

fn bind_target(
    request_target: &mut Option<String>,
    context_target: &Option<String>,
    field: &str,
) -> Result<(), (CommandErrorCategory, String)> {
    match (request_target.as_deref(), context_target.as_deref()) {
        (Some(requested), Some(bound)) if requested == bound => Ok(()),
        (Some(_), Some(_)) | (Some(_), None) => {
            Err((CommandErrorCategory::Forbidden, field.into()))
        }
        (None, Some(bound)) => {
            *request_target = Some(bound.into());
            Ok(())
        }
        (None, None) => Ok(()),
    }
}

struct ValidationError {
    field: String,
    expected: String,
    message: String,
}

fn validate_arguments(descriptor: &CommandDescriptor, args: &Value) -> Result<(), ValidationError> {
    let schema = &descriptor.argument_schema;
    let properties = schema.get("properties").and_then(Value::as_object);
    let required = schema
        .get("required")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if args.is_null() && required.is_empty() {
        return Ok(());
    }
    let Some(object) = args.as_object() else {
        return Err(ValidationError {
            field: "args".into(),
            expected: "object".into(),
            message: "命令参数必须是对象".into(),
        });
    };
    for required_field in required.iter().filter_map(Value::as_str) {
        if !object.contains_key(required_field) || object[required_field].is_null() {
            return Err(ValidationError {
                field: required_field.into(),
                expected: "required".into(),
                message: "缺少必需命令参数".into(),
            });
        }
    }
    if schema.get("additionalProperties").and_then(Value::as_bool) == Some(false) {
        for key in object.keys() {
            if properties.is_none_or(|allowed| !allowed.contains_key(key)) {
                return Err(ValidationError {
                    field: key.clone(),
                    expected: "declared_property".into(),
                    message: "命令参数包含未声明字段".into(),
                });
            }
        }
    }
    if let Some(properties) = properties {
        for (field, value) in object {
            let Some(expected) = properties
                .get(field)
                .and_then(|schema| schema.get("type"))
                .and_then(Value::as_str)
            else {
                continue;
            };
            let valid = match expected {
                "string" => value.is_string(),
                "integer" => value.is_i64() || value.is_u64(),
                "number" => value.is_number(),
                "boolean" => value.is_boolean(),
                "object" => value.is_object(),
                "array" => value.is_array(),
                _ => true,
            };
            if !valid {
                return Err(ValidationError {
                    field: field.clone(),
                    expected: expected.into(),
                    message: "命令参数类型不匹配".into(),
                });
            }
            if let Some(minimum) = properties
                .get(field)
                .and_then(|schema| schema.get("minLength"))
                .and_then(Value::as_u64)
            {
                if value
                    .as_str()
                    .is_some_and(|text| text.chars().count() < minimum as usize)
                {
                    return Err(ValidationError {
                        field: field.clone(),
                        expected: format!("minLength:{minimum}"),
                        message: "命令参数不能为空或长度不足".into(),
                    });
                }
            }
            if let Some(allowed) = properties
                .get(field)
                .and_then(|schema| schema.get("enum"))
                .and_then(Value::as_array)
            {
                if !allowed.contains(value) {
                    return Err(ValidationError {
                        field: field.clone(),
                        expected: "enum".into(),
                        message: "命令参数值不受支持".into(),
                    });
                }
            }
        }
    }
    Ok(())
}

struct PreflightError {
    category: CommandErrorCategory,
    message: String,
    details: Option<Value>,
}

fn preflight_error(
    registry: &CommandRegistry,
    descriptor: &CommandDescriptor,
    request: &CommandRequest,
    context: &CommandTransportContext,
    handler_capabilities: &[CapabilityItem],
) -> Option<PreflightError> {
    if !descriptor.entries.contains(&context.entry) {
        return Some(PreflightError {
            category: CommandErrorCategory::Unsupported,
            message: "当前入口不支持该命令".into(),
            details: None,
        });
    }
    if descriptor.deprecated {
        return Some(PreflightError {
            category: CommandErrorCategory::Unsupported,
            message: "命令已弃用".into(),
            details: descriptor
                .replacement
                .as_ref()
                .map(|replacement| json!({"replacement": replacement})),
        });
    }
    let query = CommandDirectoryQuery {
        protocol_version: COMMAND_PROTOCOL_VERSION,
        entry: context.entry,
        actor: request.actor.clone(),
        scope: Some(descriptor.scope),
        session: request.session.clone(),
        project: request.project.clone(),
        flow: request.flow.clone(),
        capabilities: context
            .capabilities
            .iter()
            .chain(handler_capabilities)
            .cloned()
            .collect(),
        include_deprecated: true,
    };
    let (executable, reason) = registry.availability(descriptor, &query);
    if !executable {
        let (category, message) = match reason {
            Some(crate::command::CommandAvailabilityReason::UnsupportedEntry) => {
                (CommandErrorCategory::Unsupported, "当前入口不支持该命令")
            }
            Some(crate::command::CommandAvailabilityReason::ForbiddenPermission) => (
                CommandErrorCategory::Forbidden,
                "当前身份没有执行该命令的权限",
            ),
            Some(crate::command::CommandAvailabilityReason::MissingScope) => {
                (CommandErrorCategory::InvalidArgs, "命令缺少已绑定的作用域")
            }
            Some(crate::command::CommandAvailabilityReason::MissingCapability)
            | Some(crate::command::CommandAvailabilityReason::CapabilityUnavailable)
            | Some(crate::command::CommandAvailabilityReason::CapabilityUnverified)
            | Some(crate::command::CommandAvailabilityReason::CapabilityDisabled)
            | Some(crate::command::CommandAvailabilityReason::MissingDependency) => {
                (CommandErrorCategory::Unavailable, "命令所需能力当前不可用")
            }
            Some(crate::command::CommandAvailabilityReason::Deprecated) => {
                (CommandErrorCategory::Unsupported, "命令已弃用")
            }
            None => unreachable!(),
        };
        return Some(PreflightError {
            category,
            message: message.into(),
            details: None,
        });
    }
    None
}

fn argument_keys(args: &Value) -> Vec<String> {
    let Some(object) = args.as_object() else {
        return Vec::new();
    };
    object.keys().cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::{
        builtin_command_descriptors, CommandActorKind, CommandDiagnosticSeverity,
        CommandOutputFormat,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct FixtureHandler {
        calls: Arc<AtomicUsize>,
        result: CommandExecution,
    }

    #[async_trait]
    impl CommandHandler for FixtureHandler {
        async fn execute(
            &self,
            request: &CommandRequest,
        ) -> Result<CommandExecution, CommandError> {
            assert_eq!(request.actor.id, "trusted-user");
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.result.clone())
        }
    }

    fn context() -> CommandTransportContext {
        CommandTransportContext {
            entry: CommandEntry::Cli,
            actor: CommandActor {
                id: "trusted-user".into(),
                kind: CommandActorKind::User,
                roles: vec!["owner".into()],
            },
            authenticated: true,
            session: Some("session-1".into()),
            project: Some("project-1".into()),
            flow: None,
            capabilities: Vec::new(),
        }
    }

    fn request(command: &str, args: Value) -> CommandRequest {
        CommandRequest {
            protocol_version: COMMAND_PROTOCOL_VERSION,
            request_id: "request-1".into(),
            command: command.into(),
            command_version: 1,
            args,
            session: None,
            project: None,
            flow: None,
            actor: CommandActor {
                id: "untrusted-client-actor".into(),
                kind: CommandActorKind::Agent,
                roles: vec!["admin".into()],
            },
            entry: CommandEntry::Chat,
            mode: CommandExecutionMode::Execute,
            output_format: CommandOutputFormat::Json,
        }
    }

    #[tokio::test]
    async fn dispatch_overrides_untrusted_actor_and_entry_and_writes_safe_audit() {
        let audit = Arc::new(MemoryCommandAuditSink::default());
        let mut service = CommandService::builtin(audit.clone()).expect("builtins");
        let calls = Arc::new(AtomicUsize::new(0));
        let handler = FixtureHandler {
            calls: calls.clone(),
            result: CommandExecution::succeeded(json!({"profileId":"fast"})),
        };
        service
            .register_handler("config.update", Arc::new(handler))
            .expect("bind config handler");
        let result = service
            .dispatch(
                request(
                    "config.update",
                    json!({"key":"profileId", "value":"secret"}),
                ),
                &context(),
            )
            .await;
        assert!(result.ok);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let records = audit.records();
        assert_eq!(
            records.len(),
            2,
            "admission and completion are both audited"
        );
        assert_eq!(records[0].actor_id, "trusted-user");
        assert_eq!(records[0].entry, CommandEntry::Cli);
        assert_eq!(records[0].session.as_deref(), Some("session-1"));
        assert_eq!(records[0].project.as_deref(), Some("project-1"));
        assert_eq!(records[0].argument_keys, vec!["key", "value"]);
        assert_eq!(records[0].phase, CommandAuditPhase::Started);
        assert_eq!(records[1].phase, CommandAuditPhase::Completed);
        assert!(!serde_json::to_string(&records).unwrap().contains("secret"));
    }

    #[tokio::test]
    async fn invalid_scope_args_permission_and_unknown_commands_never_call_handler() {
        let audit = Arc::new(MemoryCommandAuditSink::default());
        let service = CommandService::builtin(audit.clone()).expect("builtins");
        let calls = Arc::new(AtomicUsize::new(0));

        let mut wrong_scope = request("model.use", json!({"profileId":"fast"}));
        wrong_scope.project = Some("other-project".into());
        let result = service.dispatch(wrong_scope, &context()).await;
        assert_eq!(
            result.error.as_ref().unwrap().category,
            CommandErrorCategory::Forbidden
        );

        let result = service
            .dispatch(request("model.use", json!({})), &context())
            .await;
        assert_eq!(
            result.error.as_ref().unwrap().category,
            CommandErrorCategory::InvalidArgs
        );

        let result = service
            .dispatch(request("does.not.exist", json!({})), &context())
            .await;
        assert_eq!(
            result.error.as_ref().unwrap().category,
            CommandErrorCategory::Unsupported
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert!(audit
            .records()
            .iter()
            .all(|record| record.phase == CommandAuditPhase::Rejected));
    }

    #[tokio::test]
    async fn audit_failure_is_fail_closed_and_human_render_uses_redacted_result() {
        #[derive(Debug)]
        struct FailingAudit;
        #[async_trait]
        impl CommandAuditSink for FailingAudit {
            async fn record(&self, _record: CommandAuditRecord) -> anyhow::Result<()> {
                anyhow::bail!("fixture audit down")
            }
        }

        let mut service = CommandService::builtin(Arc::new(FailingAudit)).expect("builtins");
        let calls = Arc::new(AtomicUsize::new(0));
        let handler = FixtureHandler {
            calls: calls.clone(),
            result: CommandExecution::succeeded(json!({"ok":true})),
        };
        service
            .register_handler("model.use", Arc::new(handler))
            .expect("bind model handler");
        let result = service
            .dispatch(
                request("model.use", json!({"profileId":"fast"})),
                &context(),
            )
            .await;
        assert_eq!(
            result.error.as_ref().unwrap().category,
            CommandErrorCategory::Unavailable
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert!(CommandService::render_human(&result).contains("审计暂不可用"));
    }

    #[tokio::test]
    async fn command_errors_are_stable_for_supported_entries_and_unsupported_elsewhere() {
        let audit = Arc::new(MemoryCommandAuditSink::default());
        let mut service = CommandService::builtin(audit).expect("builtins");
        let calls = Arc::new(AtomicUsize::new(0));
        service
            .register_handler(
                "config.update",
                Arc::new(FixtureHandler {
                    calls: calls.clone(),
                    result: CommandExecution::succeeded(json!({"shouldNotRun":true})),
                }),
            )
            .expect("bind config handler");

        let mut results = Vec::new();
        for entry in [
            CommandEntry::Cli,
            CommandEntry::Tui,
            CommandEntry::Http,
            CommandEntry::Chat,
        ] {
            let mut transport = context();
            transport.entry = entry;
            results.push(
                service
                    .dispatch(
                        request(
                            "config.update",
                            json!({"key":"profile", "value": {"wrong":"type"}}),
                        ),
                        &transport,
                    )
                    .await,
            );
        }

        for result in &results[..2] {
            assert!(!result.ok);
            assert_eq!(result.status, CommandStatus::Rejected);
            assert_eq!(result.request_id, "request-1");
            assert_eq!(
                result.error.as_ref().unwrap().category,
                CommandErrorCategory::InvalidArgs
            );
            assert!(CommandService::render_human(result).contains("[invalid_args]"));
            let json = CommandService::render_json(result);
            assert!(json.contains("\"requestId\":\"request-1\""));
            assert!(json.contains("\"category\":\"invalid_args\""));
            assert!(!json.contains("wrong"), "raw invalid value leaked: {json}");
        }
        for result in &results[2..] {
            assert!(!result.ok);
            assert_eq!(result.status, CommandStatus::Rejected);
            assert_eq!(
                result.error.as_ref().unwrap().category,
                CommandErrorCategory::Unsupported
            );
        }
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn directory_query_is_read_only_and_unauthenticated_is_empty() {
        let service = CommandService::builtin(Arc::new(NoopCommandAuditSink)).expect("builtins");
        let mut unauthenticated = context();
        unauthenticated.authenticated = false;
        let snapshot = service.directory(&unauthenticated, None, false);
        assert!(snapshot.entries.is_empty());
        assert_eq!(
            snapshot.diagnostics[0].severity,
            CommandDiagnosticSeverity::Error
        );
        assert_eq!(
            snapshot.diagnostics[0].category,
            CommandErrorCategory::Forbidden
        );

        let before = service.registry().descriptors().count();
        let available = service.directory(&context(), Some(CommandScope::Project), false);
        assert!(available
            .entries
            .iter()
            .any(|entry| entry.descriptor.name == "model.use" && entry.executable));
        assert_eq!(service.registry().descriptors().count(), before);
        assert_eq!(builtin_command_descriptors().len(), before);
    }
}
