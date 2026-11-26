//! 连接能力的声明、宿主事实及有界准入预检（不执行适配器）。
//!
//! `CapabilityDescriptor` 是不可信 wire 数据，反序列化不代表校验、验证或授权。
//! 入口使用 `from_json` / `validate`；宿主快照和证据不实现 `Deserialize`，也不从
//! 远端字段恢复。宿主方法只能由已完成相应检查的服务调用，不能直接映射请求正文。
//! `binding_key` 是登记服务提供的不可混域键，须覆盖实例、项目、登记代际、账户、
//! 端点及来源 revision；本模块不复制连接身份类型，也不生成该键。
//!
//! 输入输出使用本模块的闭合、有界 JSON 结构方言，不是通用 JSON Schema：仅支持
//! null/bool/integer/number/string/array/closed-object；不支持 $ref、组合、正则或
//! 任意附加属性。不支持的字段直接拒绝，绝不把未校验的 schema 当成完整验证。
//! schema 深度最多 8、节点最多 128，对象最多 64 个 ASCII 引用格式字段，数组
//! 最多 1024 项；单个载荷最多 16384 个节点。所有字节/时间/并发限额均须非零。
//!
//! 准入结果只说明能力契约满足，仍须由操作服务执行当前来源权限、grant、材料白名单、
//! 参数/目标审批、根预算及设备专用确认等交集检查；不是执行票据或只读免审授权。

use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Write};
use std::sync::Arc;

pub const CAPABILITY_CONTRACT_VERSION: CapabilityVersion = CapabilityVersion { major: 1, minor: 0 };
pub const MAX_CAPABILITY_DESCRIPTOR_BYTES: usize = 64 * 1024;
pub const MAX_CAPABILITY_PAYLOAD_BYTES: u64 = 1024 * 1024;
pub const MAX_CAPABILITY_TIMEOUT_MS: u64 = 10 * 60 * 1000;
pub const MAX_CAPABILITY_CONCURRENCY: u32 = 32;
const MAX_EVENT_BYTES: u64 = 64 * 1024;
const MAX_SCHEMA_DEPTH: usize = 8;
const MAX_SCHEMA_NODES: usize = 128;
const MAX_PAYLOAD_NODES: usize = 16 * 1024;
const MAX_EVIDENCE_LIFETIME_MS: u64 = 24 * 60 * 60 * 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CapabilityVersion {
    pub major: u16,
    pub minor: u16,
}

impl CapabilityVersion {
    pub fn validate(self) -> Result<()> {
        ensure!(
            self == CAPABILITY_CONTRACT_VERSION,
            "unsupported capability version {}.{}; host supports {}.{}",
            self.major,
            self.minor,
            CAPABILITY_CONTRACT_VERSION.major,
            CAPABILITY_CONTRACT_VERSION.minor
        );
        Ok(())
    }
}

/// 能力角色，不是资源身份或成员资格。Resource 仅供目录展示，不可执行。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityRole {
    Resource,
    ToolProvider,
    QuestionAnswerer,
    TaskExecutor,
    Collaborator,
    EventSource,
    Sensor,
    Actuator,
}

impl CapabilityRole {
    /// 仅分类；不代表已验证、获准绑定或已经成为成员。
    pub const fn is_member_role(self) -> bool {
        matches!(self, Self::TaskExecutor | Self::Collaborator)
    }

    fn required_guarantees(self) -> &'static [CapabilityGuarantee] {
        match self {
            Self::TaskExecutor => &[CapabilityGuarantee::TaskLifecycle],
            Self::Collaborator => &[
                CapabilityGuarantee::TaskLifecycle,
                CapabilityGuarantee::BidirectionalCollaboration,
            ],
            _ => &[],
        }
    }
}

/// 新效果字符串降为 Unknown，不会因为旧宿主不识别而落入只读路径。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityEffect {
    Read,
    Write,
    Publish,
    Privacy,
    Physical,
    Charge,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalRequirement {
    /// 仅效果分类允许考虑只读路径；不代表已经获得执行权限或豁免其他审批。
    ReadOnlyCandidate,
    Required,
}

fn approval_requirement(effects: &BTreeSet<CapabilityEffect>) -> ApprovalRequirement {
    if effects.len() == 1 && effects.contains(&CapabilityEffect::Read) {
        ApprovalRequirement::ReadOnlyCandidate
    } else {
        ApprovalRequirement::Required
    }
}

/// 目的地只能是引用，不接收 URL、凭据或任意网络目标。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DataDestination {
    /// 不外发；不因此授予宿主文件或工具权限。
    Local,
    Remote {
        #[serde(rename = "endpointRef")]
        endpoint_ref: String,
        #[serde(rename = "accountRef")]
        account_ref: String,
    },
}

impl DataDestination {
    fn validate(&self) -> Result<()> {
        if let Self::Remote {
            endpoint_ref,
            account_ref,
        } = self
        {
            validate_ref(endpoint_ref, "endpoint reference")?;
            validate_ref(account_ref, "account reference")?;
        }
        Ok(())
    }
}

/// 自有的有界 JSON 结构契约。对象始终闭合；字符串按 UTF-8 字节计数。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PayloadSchema {
    Null,
    Boolean,
    Integer,
    Number,
    String {
        #[serde(rename = "maxBytes")]
        max_bytes: u64,
    },
    Array {
        items: Box<PayloadSchema>,
        #[serde(rename = "maxItems")]
        max_items: u32,
    },
    Object {
        fields: BTreeMap<String, PayloadSchema>,
        required: BTreeSet<String>,
    },
}

impl PayloadSchema {
    pub fn validate(&self) -> Result<()> {
        let mut remaining = MAX_SCHEMA_NODES;
        self.validate_schema(0, &mut remaining)
    }

    fn validate_schema(&self, depth: usize, remaining: &mut usize) -> Result<()> {
        ensure!(
            depth <= MAX_SCHEMA_DEPTH && *remaining > 0,
            "schema limit exceeded"
        );
        *remaining -= 1;
        match self {
            Self::String { max_bytes } => ensure!(
                (1..=MAX_CAPABILITY_PAYLOAD_BYTES).contains(max_bytes),
                "invalid schema string limit"
            ),
            Self::Array { items, max_items } => {
                ensure!((1..=1024).contains(max_items), "invalid schema array limit");
                items.validate_schema(depth + 1, remaining)?;
            }
            Self::Object { fields, required } => {
                ensure!(fields.len() <= 64, "too many schema fields");
                ensure!(
                    required.iter().all(|name| fields.contains_key(name)),
                    "unknown required field"
                );
                for (name, schema) in fields {
                    validate_ref(name, "schema field")?;
                    schema.validate_schema(depth + 1, remaining)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn validate_payload(&self, value: &Value, max_bytes: u64) -> Result<()> {
        let mut remaining = MAX_PAYLOAD_NODES;
        self.validate_value(value, &mut remaining)?;
        serde_json::to_writer(CappedJsonWriter(max_bytes), value)
            .context("payload byte limit exceeded")?;
        Ok(())
    }

    fn validate_value(&self, value: &Value, remaining: &mut usize) -> Result<()> {
        ensure!(*remaining > 0, "payload node limit exceeded");
        *remaining -= 1;
        let matches = match (self, value) {
            (Self::Null, Value::Null) | (Self::Boolean, Value::Bool(_)) => true,
            (Self::Integer, Value::Number(n)) => n.is_i64() || n.is_u64(),
            (Self::Number, Value::Number(_)) => true,
            (Self::String { max_bytes }, Value::String(s)) => s.len() as u64 <= *max_bytes,
            (Self::Array { items, max_items }, Value::Array(values)) => {
                ensure!(
                    values.len() <= *max_items as usize,
                    "payload array limit exceeded"
                );
                for child in values {
                    items.validate_value(child, remaining)?;
                }
                true
            }
            (Self::Object { fields, required }, Value::Object(values)) => {
                ensure!(
                    required.iter().all(|key| values.contains_key(key)),
                    "missing required payload field"
                );
                for (key, child) in values {
                    let schema = fields.get(key).context("undeclared payload field")?;
                    schema.validate_value(child, remaining)?;
                }
                true
            }
            _ => false,
        };
        ensure!(matches, "payload does not match bounded schema");
        Ok(())
    }
}

/// 序列化计数遇到上限立即失败，不为超大 Value 再分配一份 JSON。
struct CappedJsonWriter(u64);

impl Write for CappedJsonWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() as u64 > self.0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "payload limit exceeded",
            ));
        }
        self.0 -= bytes.len() as u64;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CapabilityLimits {
    pub timeout_ms: u64,
    pub max_input_bytes: u64,
    pub max_output_bytes: u64,
    pub max_concurrency: u32,
}

impl Default for CapabilityLimits {
    fn default() -> Self {
        Self {
            timeout_ms: MAX_CAPABILITY_TIMEOUT_MS,
            max_input_bytes: MAX_CAPABILITY_PAYLOAD_BYTES,
            max_output_bytes: MAX_CAPABILITY_PAYLOAD_BYTES,
            max_concurrency: 1,
        }
    }
}

impl CapabilityLimits {
    pub fn validate(self) -> Result<()> {
        ensure!(
            (1..=MAX_CAPABILITY_TIMEOUT_MS).contains(&self.timeout_ms),
            "invalid timeout limit"
        );
        ensure!(
            (1..=MAX_CAPABILITY_PAYLOAD_BYTES).contains(&self.max_input_bytes),
            "invalid input limit"
        );
        ensure!(
            (1..=MAX_CAPABILITY_PAYLOAD_BYTES).contains(&self.max_output_bytes),
            "invalid output limit"
        );
        ensure!(
            (1..=MAX_CAPABILITY_CONCURRENCY).contains(&self.max_concurrency),
            "invalid concurrency limit"
        );
        Ok(())
    }

    fn fits_within(self, declared: Self) -> bool {
        self.timeout_ms <= declared.timeout_ms
            && self.max_input_bytes <= declared.max_input_bytes
            && self.max_output_bytes <= declared.max_output_bytes
            && self.max_concurrency <= declared.max_concurrency
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdempotencySupport {
    Unsupported,
    RequestKey,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CancellationSupport {
    Unsupported,
    RequestOnly,
    Confirmed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MeteringSupport {
    Unknown,
    UsageReport,
    HardCostLimit,
}

/// 全部只是远端支持声明；Confirmed / HardCostLimit 也不是宿主保证。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CapabilitySupport {
    pub idempotency: IdempotencySupport,
    pub cancellation: CancellationSupport,
    pub metering: MeteringSupport,
    /// 有精确任务 ID 的 start/status/result 生命周期；不是文本派发或 tools/list。
    /// 不因此推导进度、用户问答、取消、本地工具回调或成员调度已经支持。
    pub task_lifecycle: bool,
    /// 通过宿主仲裁的双向任务/消息协议；不是普通工具调用。
    pub bidirectional_collaboration: bool,
}

impl Default for CapabilitySupport {
    fn default() -> Self {
        Self {
            idempotency: IdempotencySupport::Unsupported,
            cancellation: CancellationSupport::Unsupported,
            metering: MeteringSupport::Unknown,
            task_lifecycle: false,
            bidirectional_collaboration: false,
        }
    }
}

/// 宿主核验过的协议/强制能力，不代表本次任务已经设置费用限额、取得幂等回执或停止。
/// 操作级数值、审批和回执仍由操作服务逐项验证。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityGuarantee {
    Idempotency,
    CancellationRequest,
    CancellationConfirmation,
    VerifiedMetering,
    EnforcedCostLimit,
    TaskLifecycle,
    BidirectionalCollaboration,
}

impl CapabilityGuarantee {
    const ALL: [Self; 7] = [
        Self::Idempotency,
        Self::CancellationRequest,
        Self::CancellationConfirmation,
        Self::VerifiedMetering,
        Self::EnforcedCostLimit,
        Self::TaskLifecycle,
        Self::BidirectionalCollaboration,
    ];

    fn is_declared(self, support: &CapabilitySupport) -> bool {
        match self {
            Self::Idempotency => support.idempotency == IdempotencySupport::RequestKey,
            Self::CancellationRequest => support.cancellation != CancellationSupport::Unsupported,
            Self::CancellationConfirmation => {
                support.cancellation == CancellationSupport::Confirmed
            }
            Self::VerifiedMetering => support.metering != MeteringSupport::Unknown,
            Self::EnforcedCostLimit => support.metering == MeteringSupport::HardCostLimit,
            Self::TaskLifecycle => support.task_lifecycle,
            Self::BidirectionalCollaboration => support.bidirectional_collaboration,
        }
    }
}

/// 不可信、可序列化的能力声明。不要向此结构添加宿主授权/验证字段。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CapabilityDescriptor {
    pub version: CapabilityVersion,
    pub id: String,
    pub revision: u64,
    pub role: CapabilityRole,
    pub input_schema: PayloadSchema,
    pub output_schema: PayloadSchema,
    /// 缺失或空集合按 Unknown 处理，而不是 Read。
    #[serde(default)]
    pub effects: BTreeSet<CapabilityEffect>,
    pub data_destinations: BTreeSet<DataDestination>,
    pub limits: CapabilityLimits,
    pub support: CapabilitySupport,
}

impl CapabilityDescriptor {
    /// wire 入口先检查字节上限，再反序列化和严格校验。
    pub fn from_json(bytes: &[u8]) -> Result<Self> {
        ensure!(
            bytes.len() <= MAX_CAPABILITY_DESCRIPTOR_BYTES,
            "descriptor byte limit exceeded"
        );
        let descriptor: Self =
            serde_json::from_slice(bytes).context("invalid capability descriptor")?;
        descriptor.validate()?;
        Ok(descriptor)
    }

    pub fn validate(&self) -> Result<()> {
        self.version.validate()?;
        validate_ref(&self.id, "capability id")?;
        ensure!(self.revision > 0, "capability revision must be nonzero");
        self.input_schema.validate()?;
        self.output_schema.validate()?;
        self.limits.validate()?;
        ensure!(
            (1..=16).contains(&self.data_destinations.len()),
            "invalid destination count"
        );
        for destination in &self.data_destinations {
            destination.validate()?;
        }
        ensure!(
            !self.support.bidirectional_collaboration || self.support.task_lifecycle,
            "collaboration requires task lifecycle"
        );
        for guarantee in self.role.required_guarantees() {
            ensure!(
                guarantee.is_declared(&self.support),
                "role requires an undeclared protocol guarantee"
            );
        }
        if self.role == CapabilityRole::EventSource {
            ensure!(
                self.limits.max_output_bytes <= MAX_EVENT_BYTES,
                "event payload limit exceeded"
            );
        }
        ensure!(
            serde_json::to_vec(self)?.len() <= MAX_CAPABILITY_DESCRIPTOR_BYTES,
            "descriptor byte limit exceeded"
        );
        Ok(())
    }

    pub fn validate_input(&self, input: &Value) -> Result<()> {
        self.validate()?;
        self.input_schema
            .validate_payload(input, self.limits.max_input_bytes)
    }

    pub fn validate_output(&self, output: &Value) -> Result<()> {
        self.validate()?;
        self.output_schema
            .validate_payload(output, self.limits.max_output_bytes)
    }

    pub fn decode_input(&self, bytes: &[u8]) -> Result<Value> {
        self.validate()?;
        decode_payload(&self.input_schema, bytes, self.limits.max_input_bytes)
    }

    pub fn decode_output(&self, bytes: &[u8]) -> Result<Value> {
        self.validate()?;
        decode_payload(&self.output_schema, bytes, self.limits.max_output_bytes)
    }
}

fn validate_ref(value: &str, label: &str) -> Result<()> {
    ensure!(
        !value.is_empty()
            && value.len() <= 128
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-.:".contains(&b)),
        "invalid {label}"
    );
    Ok(())
}

fn decode_payload(schema: &PayloadSchema, bytes: &[u8], max_bytes: u64) -> Result<Value> {
    ensure!(
        bytes.len() as u64 <= max_bytes,
        "payload byte limit exceeded"
    );
    let value = serde_json::from_slice(bytes).context("invalid JSON payload")?;
    schema.validate_payload(&value, max_bytes)?;
    Ok(value)
}

/// 由宿主绑定的能力作用域，不是新的身份模型。字段私有且不从 wire 恢复。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilityEvidenceScope {
    binding_key: String,
    capability_id: String,
    capability_revision: u64,
    version: CapabilityVersion,
    // 比较完整声明，防止对端复用同一 revision 偷换 schema/目的地/效果并继承旧证据。
    // 不导出重复正文，也不允许从导出的 scope 恢复宿主事实。
    #[serde(skip_serializing)]
    declaration: Arc<CapabilityDescriptor>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceOrigin {
    HostInspection,
    IsolatedFixture,
    ProductPlatform,
}

/// 证据覆盖的检查项；检测证据不能改作验证、授权或硬费用保证。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", content = "guarantee", rename_all = "snake_case")]
pub enum CapabilityEvidenceClaim {
    Detection,
    Contract,
    Effects,
    Guarantee(CapabilityGuarantee),
    Authorization,
}

/// 宿主记录的有范围、有期限证据。reference 只能引用证据库，不保存远端正文。
/// IsolatedFixture 不代表真实产品/硬件已验证，目录必须原样展示验证来源。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostCapabilityEvidence {
    scope: CapabilityEvidenceScope,
    reference: String,
    origin: EvidenceOrigin,
    claim: CapabilityEvidenceClaim,
    observed_at_ms: u64,
    expires_at_ms: u64,
}

impl HostCapabilityEvidence {
    /// 仅供受信宿主验证服务调用；这不是远端提交验证结果的接口。
    pub(crate) fn new(
        scope: CapabilityEvidenceScope,
        reference: String,
        origin: EvidenceOrigin,
        claim: CapabilityEvidenceClaim,
        observed_at_ms: u64,
        expires_at_ms: u64,
    ) -> Result<Self> {
        validate_ref(&reference, "evidence reference")?;
        ensure!(
            observed_at_ms > 0
                && expires_at_ms > observed_at_ms
                && expires_at_ms - observed_at_ms <= MAX_EVIDENCE_LIFETIME_MS,
            "invalid evidence lifetime"
        );
        Ok(Self {
            scope,
            reference,
            origin,
            claim,
            observed_at_ms,
            expires_at_ms,
        })
    }

    fn is_current(&self, scope: &CapabilityEvidenceScope, now_ms: u64) -> bool {
        self.scope == *scope && self.observed_at_ms <= now_ms && now_ms < self.expires_at_ms
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityAvailability {
    Unknown,
    Available,
    Disabled,
    Unavailable,
    Stale,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityAuthorizationState {
    Unknown,
    Authorized,
    Forbidden,
    Expired,
}

/// 当前宿主授权服务的短期评估结果，不是 grant 仓库，也不是持久执行许可。
/// 调用者作用域必须绑定主体、项目、会话、根执行及当前授权 revision 等上下文，
/// 撤权后须换用当前作用域重验；证据未到期不代表旧 grant 仍然有效。
#[derive(Debug, Clone)]
pub struct HostCapabilityAuthorization {
    caller_scope: String,
    authorized: bool,
    evidence: HostCapabilityEvidence,
}

impl HostCapabilityAuthorization {
    /// 仅在宿主完成该能力/当前调用者的权限交集检查后构建；禁止 wire 直通。
    pub(crate) fn evaluated(
        caller_scope: String,
        authorized: bool,
        evidence: HostCapabilityEvidence,
    ) -> Result<Self> {
        validate_ref(&caller_scope, "caller scope")?;
        ensure!(
            evidence.claim == CapabilityEvidenceClaim::Authorization,
            "authorization requires host authorization evidence"
        );
        Ok(Self {
            caller_scope,
            authorized,
            evidence,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DetectionState {
    NotDetected,
    Detected,
    Stale,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationState {
    Unverified,
    Verified,
    Stale,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilityGuaranteeProjection<'a> {
    pub guarantee: CapabilityGuarantee,
    pub declared: bool,
    pub verified: bool,
    pub evidence: Option<&'a HostCapabilityEvidence>,
}

/// 目录 DTO 只输出事实，不实现 Deserialize，也不能回写为宿主状态。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilityProjection<'a> {
    pub descriptor: &'a CapabilityDescriptor,
    pub declared: bool,
    pub detection: DetectionState,
    pub verification: VerificationState,
    pub effects_verification: VerificationState,
    pub availability: CapabilityAvailability,
    pub authorization: CapabilityAuthorizationState,
    pub host_effects: &'a BTreeSet<CapabilityEffect>,
    pub effective_effects: BTreeSet<CapabilityEffect>,
    pub approval: ApprovalRequirement,
    pub detection_evidence: Option<&'a HostCapabilityEvidence>,
    pub verification_evidence: Option<&'a HostCapabilityEvidence>,
    pub effects_evidence: Option<&'a HostCapabilityEvidence>,
    pub guarantees: Vec<CapabilityGuaranteeProjection<'a>>,
}

/// 私有宿主态。构建新快照总是清空宿主事实；不能从远端声明恢复已验证/已授权。
/// 同一快照中的宿主风险只增不减；更换声明须新建并重新检测、验证、授权。
#[derive(Debug, Clone)]
pub struct CapabilitySnapshot {
    descriptor: Arc<CapabilityDescriptor>,
    scope: CapabilityEvidenceScope,
    availability: CapabilityAvailability,
    detection: Option<HostCapabilityEvidence>,
    verification: Option<HostCapabilityEvidence>,
    effects_verification: Option<HostCapabilityEvidence>,
    host_effects: BTreeSet<CapabilityEffect>,
    guarantees: BTreeMap<CapabilityGuarantee, HostCapabilityEvidence>,
}

impl CapabilitySnapshot {
    pub fn from_declaration(binding_key: String, descriptor: CapabilityDescriptor) -> Result<Self> {
        validate_ref(&binding_key, "capability binding key")?;
        descriptor.validate()?;
        let descriptor = Arc::new(descriptor);
        let scope = CapabilityEvidenceScope {
            binding_key,
            capability_id: descriptor.id.clone(),
            capability_revision: descriptor.revision,
            version: descriptor.version,
            declaration: Arc::clone(&descriptor),
        };
        Ok(Self {
            descriptor,
            scope,
            availability: CapabilityAvailability::Unknown,
            detection: None,
            verification: None,
            effects_verification: None,
            host_effects: BTreeSet::new(),
            guarantees: BTreeMap::new(),
        })
    }

    pub fn descriptor(&self) -> &CapabilityDescriptor {
        &self.descriptor
    }

    pub fn evidence_scope(&self) -> CapabilityEvidenceScope {
        self.scope.clone()
    }

    /// 仅设置宿主来源可用性；不隐式完成检测、验证或授权。
    pub(crate) fn set_availability(&mut self, availability: CapabilityAvailability) {
        self.availability = availability;
    }

    fn check_evidence(
        &self,
        evidence: &HostCapabilityEvidence,
        claim: CapabilityEvidenceClaim,
        now_ms: u64,
    ) -> Result<()> {
        ensure!(
            evidence.claim == claim && evidence.is_current(&self.scope, now_ms),
            "evidence claim, scope or lifetime mismatch"
        );
        Ok(())
    }

    pub(crate) fn record_detection(
        &mut self,
        evidence: HostCapabilityEvidence,
        now_ms: u64,
    ) -> Result<()> {
        self.check_evidence(&evidence, CapabilityEvidenceClaim::Detection, now_ms)?;
        self.detection = Some(evidence);
        Ok(())
    }

    /// 仅验证能力契约，不因此验证效果、幂等、取消或计量保证。
    pub(crate) fn record_verification(
        &mut self,
        evidence: HostCapabilityEvidence,
        now_ms: u64,
    ) -> Result<()> {
        self.check_evidence(&evidence, CapabilityEvidenceClaim::Contract, now_ms)?;
        self.verification = Some(evidence);
        Ok(())
    }

    pub(crate) fn record_effect_verification(
        &mut self,
        effects: BTreeSet<CapabilityEffect>,
        evidence: HostCapabilityEvidence,
        now_ms: u64,
    ) -> Result<()> {
        self.check_evidence(&evidence, CapabilityEvidenceClaim::Effects, now_ms)?;
        ensure!(
            !effects.is_empty(),
            "host effect assessment must not be empty"
        );
        self.host_effects.extend(effects);
        self.effects_verification = Some(evidence);
        Ok(())
    }

    /// 每项保证必须分别提供宿主证据；请求取消绝不等于远端已停止。
    pub(crate) fn record_guarantee(
        &mut self,
        guarantee: CapabilityGuarantee,
        evidence: HostCapabilityEvidence,
        now_ms: u64,
    ) -> Result<()> {
        self.check_evidence(
            &evidence,
            CapabilityEvidenceClaim::Guarantee(guarantee),
            now_ms,
        )?;
        ensure!(
            guarantee.is_declared(&self.descriptor.support),
            "guarantee not declared by this capability"
        );
        self.guarantees.insert(guarantee, evidence);
        Ok(())
    }

    pub fn has_verified_guarantee(&self, guarantee: CapabilityGuarantee, now_ms: u64) -> bool {
        self.guarantees
            .get(&guarantee)
            .is_some_and(|e| e.is_current(&self.scope, now_ms))
    }

    pub fn effective_effects(&self, now_ms: u64) -> BTreeSet<CapabilityEffect> {
        let mut effects = self.descriptor.effects.clone();
        if effects.is_empty() {
            effects.insert(CapabilityEffect::Unknown);
        }
        effects.extend(&self.host_effects);
        // 感知/实体执行的语义下限不能被远端 Read 声明抹掉。
        match self.descriptor.role {
            CapabilityRole::Sensor => {
                effects.insert(CapabilityEffect::Privacy);
            }
            CapabilityRole::Actuator => {
                effects.insert(CapabilityEffect::Physical);
            }
            _ => {}
        }
        if ![&self.verification, &self.effects_verification]
            .into_iter()
            .all(|evidence| {
                evidence
                    .as_ref()
                    .is_some_and(|e| e.is_current(&self.scope, now_ms))
            })
        {
            effects.insert(CapabilityEffect::Unknown);
        }
        effects
    }

    fn authorization_state(
        &self,
        authorization: Option<&HostCapabilityAuthorization>,
        caller_scope: &str,
        now_ms: u64,
    ) -> CapabilityAuthorizationState {
        let Some(authorization) = authorization else {
            return CapabilityAuthorizationState::Unknown;
        };
        if authorization.caller_scope != caller_scope || authorization.evidence.scope != self.scope
        {
            return CapabilityAuthorizationState::Forbidden;
        }
        if !authorization.evidence.is_current(&self.scope, now_ms) {
            return CapabilityAuthorizationState::Expired;
        }
        if authorization.authorized {
            CapabilityAuthorizationState::Authorized
        } else {
            CapabilityAuthorizationState::Forbidden
        }
    }

    /// 只读取已有宿主快照；不握手、发现、启动进程或联系远端。
    pub fn projection(
        &self,
        authorization: Option<&HostCapabilityAuthorization>,
        caller_scope: &str,
        now_ms: u64,
    ) -> CapabilityProjection<'_> {
        let verification_state = |evidence: &Option<HostCapabilityEvidence>| match evidence {
            None => VerificationState::Unverified,
            Some(e) if e.is_current(&self.scope, now_ms) => VerificationState::Verified,
            Some(_) => VerificationState::Stale,
        };
        let effective_effects = self.effective_effects(now_ms);
        CapabilityProjection {
            descriptor: &self.descriptor,
            declared: true,
            detection: match &self.detection {
                None => DetectionState::NotDetected,
                Some(e) if e.is_current(&self.scope, now_ms) => DetectionState::Detected,
                Some(_) => DetectionState::Stale,
            },
            verification: verification_state(&self.verification),
            effects_verification: verification_state(&self.effects_verification),
            availability: self.availability,
            authorization: self.authorization_state(authorization, caller_scope, now_ms),
            host_effects: &self.host_effects,
            approval: approval_requirement(&effective_effects),
            effective_effects,
            detection_evidence: self.detection.as_ref(),
            verification_evidence: self.verification.as_ref(),
            effects_evidence: self.effects_verification.as_ref(),
            guarantees: CapabilityGuarantee::ALL
                .into_iter()
                .map(|guarantee| CapabilityGuaranteeProjection {
                    guarantee,
                    declared: guarantee.is_declared(&self.descriptor.support),
                    verified: self.has_verified_guarantee(guarantee, now_ms),
                    evidence: self.guarantees.get(&guarantee),
                })
                .collect(),
        }
    }

    /// 能力层预检，不发送动作，也不替代参数审批、真实 grant/预算/后端准入。
    pub fn check_admission<'a>(
        &'a self,
        request: &CapabilityAdmissionRequest<'_>,
        authorization: &HostCapabilityAuthorization,
        now_ms: u64,
    ) -> Result<CapabilityAdmission<'a>> {
        ensure!(
            self.availability == CapabilityAvailability::Available,
            "capability source not available"
        );
        for evidence in [&self.detection, &self.verification] {
            ensure!(
                evidence
                    .as_ref()
                    .is_some_and(|e| e.is_current(&self.scope, now_ms)),
                "capability not currently detected and verified"
            );
        }
        ensure!(
            self.authorization_state(Some(authorization), request.caller_scope, now_ms)
                == CapabilityAuthorizationState::Authorized,
            "capability not authorized for current caller"
        );
        ensure!(
            request.role != CapabilityRole::Resource && request.role == self.descriptor.role,
            "capability role mismatch or non-executable resource"
        );
        request.limits.validate()?;
        ensure!(
            request.limits.fits_within(self.descriptor.limits),
            "requested limits exceed capability contract"
        );
        ensure!(
            self.descriptor
                .data_destinations
                .contains(request.destination),
            "undeclared data destination"
        );
        self.descriptor
            .input_schema
            .validate_payload(request.input, request.limits.max_input_bytes)?;
        for guarantee in request
            .required_guarantees
            .iter()
            .chain(request.role.required_guarantees())
        {
            ensure!(
                self.has_verified_guarantee(*guarantee, now_ms),
                "required capability guarantee not verified: {guarantee:?}"
            );
        }
        let effective_effects = self.effective_effects(now_ms);
        Ok(CapabilityAdmission {
            descriptor: &self.descriptor,
            limits: request.limits,
            approval: approval_requirement(&effective_effects),
            effective_effects,
        })
    }
}

/// 来自宿主操作服务的预检请求，不是可反序列化的外部执行请求。
#[derive(Debug)]
pub struct CapabilityAdmissionRequest<'a> {
    pub caller_scope: &'a str,
    pub role: CapabilityRole,
    pub destination: &'a DataDestination,
    pub input: &'a Value,
    pub limits: CapabilityLimits,
    pub required_guarantees: &'a BTreeSet<CapabilityGuarantee>,
}

/// 私有字段、防止 wire 伪造的预检结果；持有它仍然不能派发适配器。
#[derive(Debug)]
pub struct CapabilityAdmission<'a> {
    descriptor: &'a CapabilityDescriptor,
    limits: CapabilityLimits,
    effective_effects: BTreeSet<CapabilityEffect>,
    approval: ApprovalRequirement,
}

impl CapabilityAdmission<'_> {
    pub fn approval(&self) -> ApprovalRequirement {
        self.approval
    }

    pub fn effective_effects(&self) -> &BTreeSet<CapabilityEffect> {
        &self.effective_effects
    }

    pub fn limits(&self) -> CapabilityLimits {
        self.limits
    }

    /// 响应须同时满足声明的 schema 和此次收紧后的字节上限。
    pub fn validate_output(&self, output: &Value) -> Result<()> {
        self.descriptor
            .output_schema
            .validate_payload(output, self.limits.max_output_bytes)
    }

    pub fn decode_output(&self, bytes: &[u8]) -> Result<Value> {
        decode_payload(
            &self.descriptor.output_schema,
            bytes,
            self.limits.max_output_bytes,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const NOW: u64 = 1000;
    const EXPIRES: u64 = 2000;

    fn descriptor() -> CapabilityDescriptor {
        CapabilityDescriptor {
            version: CAPABILITY_CONTRACT_VERSION,
            id: "mcp.documents.read".into(),
            revision: 1,
            role: CapabilityRole::ToolProvider,
            input_schema: PayloadSchema::Object {
                fields: BTreeMap::from([(
                    "document".into(),
                    PayloadSchema::String { max_bytes: 32 },
                )]),
                required: BTreeSet::from(["document".into()]),
            },
            output_schema: PayloadSchema::String { max_bytes: 128 },
            effects: BTreeSet::from([CapabilityEffect::Read]),
            data_destinations: BTreeSet::from([DataDestination::Remote {
                endpoint_ref: "document-api".into(),
                account_ref: "account-a".into(),
            }]),
            limits: CapabilityLimits::default(),
            support: CapabilitySupport::default(),
        }
    }

    fn snapshot() -> CapabilitySnapshot {
        CapabilitySnapshot::from_declaration(
            "instance.project.registration-generation-1".into(),
            descriptor(),
        )
        .unwrap()
    }

    fn evidence(
        snapshot: &CapabilitySnapshot,
        reference: &str,
        claim: CapabilityEvidenceClaim,
    ) -> HostCapabilityEvidence {
        HostCapabilityEvidence::new(
            snapshot.evidence_scope(),
            reference.into(),
            EvidenceOrigin::IsolatedFixture,
            claim,
            NOW,
            EXPIRES,
        )
        .unwrap()
    }

    fn make_ready(snapshot: &mut CapabilitySnapshot) -> HostCapabilityAuthorization {
        snapshot.set_availability(CapabilityAvailability::Available);
        snapshot
            .record_detection(
                evidence(
                    snapshot,
                    "fixture-detection",
                    CapabilityEvidenceClaim::Detection,
                ),
                NOW,
            )
            .unwrap();
        snapshot
            .record_verification(
                evidence(
                    snapshot,
                    "fixture-contract",
                    CapabilityEvidenceClaim::Contract,
                ),
                NOW,
            )
            .unwrap();
        HostCapabilityAuthorization::evaluated(
            "actor.project.session.root".into(),
            true,
            evidence(
                snapshot,
                "authorization-evaluation",
                CapabilityEvidenceClaim::Authorization,
            ),
        )
        .unwrap()
    }

    #[test]
    fn version_table_rejects_unknown_versions_at_wire_and_snapshot_boundaries() {
        for (major, minor, accepted) in [
            (1, 0, true),
            (0, 0, false),
            (2, 0, false),
            (u16::MAX, 0, false),
            (1, 1, false),
        ] {
            let mut declaration = descriptor();
            declaration.version = CapabilityVersion { major, minor };
            let wire = serde_json::to_vec(&declaration).unwrap();
            assert_eq!(
                CapabilityDescriptor::from_json(&wire).is_ok(),
                accepted,
                "{major}.{minor}"
            );
            assert_eq!(
                CapabilitySnapshot::from_declaration("binding".into(), declaration).is_ok(),
                accepted
            );
        }
    }

    #[test]
    fn effect_table_never_trusts_remote_read_only_or_future_effects() {
        for effects in [
            json!([]),
            json!(["read"]),
            json!(["future_effect"]),
            json!(["read", "write"]),
            json!(["physical"]),
        ] {
            let mut wire = serde_json::to_value(descriptor()).unwrap();
            wire["effects"] = effects;
            let declaration =
                CapabilityDescriptor::from_json(&serde_json::to_vec(&wire).unwrap()).unwrap();
            let mut snapshot =
                CapabilitySnapshot::from_declaration("binding".into(), declaration).unwrap();
            let projection = snapshot.projection(None, "caller", NOW);
            assert!(projection
                .effective_effects
                .contains(&CapabilityEffect::Unknown));
            assert_eq!(projection.approval, ApprovalRequirement::Required);
            make_ready(&mut snapshot);
            snapshot
                .record_effect_verification(
                    BTreeSet::from([CapabilityEffect::Read]),
                    evidence(&snapshot, "host-effects", CapabilityEvidenceClaim::Effects),
                    NOW,
                )
                .unwrap();
            let expected =
                if snapshot.descriptor().effects == BTreeSet::from([CapabilityEffect::Read]) {
                    ApprovalRequirement::ReadOnlyCandidate
                } else {
                    ApprovalRequirement::Required
                };
            assert_eq!(snapshot.projection(None, "caller", NOW).approval, expected);
        }
        let mut wire = serde_json::to_value(descriptor()).unwrap();
        wire.as_object_mut().unwrap().remove("effects");
        assert!(
            CapabilityDescriptor::from_json(&serde_json::to_vec(&wire).unwrap())
                .unwrap()
                .effects
                .is_empty()
        );
    }

    #[test]
    fn host_risk_floor_survives_remote_read_claim_and_later_host_read_assessment() {
        let mut snapshot = snapshot();
        let authorization = make_ready(&mut snapshot);
        snapshot
            .record_effect_verification(
                BTreeSet::from([CapabilityEffect::Write, CapabilityEffect::Publish]),
                evidence(&snapshot, "host-write", CapabilityEvidenceClaim::Effects),
                NOW,
            )
            .unwrap();
        snapshot
            .record_effect_verification(
                BTreeSet::from([CapabilityEffect::Read]),
                evidence(&snapshot, "later-read", CapabilityEvidenceClaim::Effects),
                NOW,
            )
            .unwrap();
        assert_eq!(
            snapshot.descriptor().effects,
            BTreeSet::from([CapabilityEffect::Read])
        );
        for now in [NOW, EXPIRES] {
            let projection =
                snapshot.projection(Some(&authorization), "actor.project.session.root", now);
            assert!(projection
                .effective_effects
                .contains(&CapabilityEffect::Write));
            assert!(projection
                .effective_effects
                .contains(&CapabilityEffect::Publish));
            assert_eq!(projection.approval, ApprovalRequirement::Required);
        }
    }

    #[test]
    fn fresh_effect_evidence_is_required_for_read_only_candidate() {
        let mut unverified_contract = snapshot();
        unverified_contract
            .record_effect_verification(
                BTreeSet::from([CapabilityEffect::Read]),
                evidence(
                    &unverified_contract,
                    "effects-only",
                    CapabilityEvidenceClaim::Effects,
                ),
                NOW,
            )
            .unwrap();
        assert_eq!(
            unverified_contract.projection(None, "caller", NOW).approval,
            ApprovalRequirement::Required
        );
        let mut snapshot = snapshot();
        let authorization = make_ready(&mut snapshot);
        assert_eq!(
            snapshot
                .projection(Some(&authorization), "actor.project.session.root", NOW)
                .approval,
            ApprovalRequirement::Required
        );
        snapshot
            .record_effect_verification(
                BTreeSet::from([CapabilityEffect::Read]),
                evidence(
                    &snapshot,
                    "host-read-only",
                    CapabilityEvidenceClaim::Effects,
                ),
                NOW,
            )
            .unwrap();
        assert_eq!(
            snapshot.projection(None, "caller", NOW).approval,
            ApprovalRequirement::ReadOnlyCandidate
        );
        assert_eq!(
            snapshot.projection(None, "caller", EXPIRES).approval,
            ApprovalRequirement::Required
        );
        assert_eq!(
            snapshot
                .projection(None, "caller", EXPIRES)
                .effects_verification,
            VerificationState::Stale
        );
    }

    #[test]
    fn mcp_tools_only_role_table_does_not_create_membership_or_task_protocol() {
        for (role, valid, member) in [
            (CapabilityRole::Resource, true, false),
            (CapabilityRole::ToolProvider, true, false),
            (CapabilityRole::QuestionAnswerer, true, false),
            (CapabilityRole::TaskExecutor, false, true),
            (CapabilityRole::Collaborator, false, true),
            (CapabilityRole::Sensor, true, false),
            (CapabilityRole::Actuator, true, false),
        ] {
            let mut declaration = descriptor();
            declaration.role = role;
            assert_eq!(declaration.validate().is_ok(), valid, "{role:?}");
            assert_eq!(role.is_member_role(), member);
        }
        let mut snapshot = snapshot();
        make_ready(&mut snapshot);
        assert!(!snapshot.has_verified_guarantee(CapabilityGuarantee::TaskLifecycle, NOW));
        assert!(snapshot
            .record_guarantee(
                CapabilityGuarantee::BidirectionalCollaboration,
                evidence(
                    &snapshot,
                    "tools-list",
                    CapabilityEvidenceClaim::Guarantee(
                        CapabilityGuarantee::BidirectionalCollaboration
                    )
                ),
                NOW
            )
            .is_err());
    }

    #[test]
    fn wire_cannot_supply_host_facts() {
        for field in [
            "authorized",
            "verified",
            "hostEffects",
            "effectiveEffects",
            "verification",
            "availability",
        ] {
            let mut wire = serde_json::to_value(descriptor()).unwrap();
            wire[field] = json!(true);
            assert!(
                CapabilityDescriptor::from_json(&serde_json::to_vec(&wire).unwrap()).is_err(),
                "{field}"
            );
        }
        let snapshot = snapshot();
        let projection = snapshot.projection(None, "caller", NOW);
        assert!(projection.declared);
        assert_eq!(projection.detection, DetectionState::NotDetected);
        assert_eq!(projection.verification, VerificationState::Unverified);
        assert_eq!(
            projection.authorization,
            CapabilityAuthorizationState::Unknown
        );
        assert_eq!(projection.availability, CapabilityAvailability::Unknown);
    }

    #[test]
    fn schema_and_payload_table_is_closed_bounded_and_validates_both_directions() {
        let declaration = descriptor();
        for (input, accepted) in [
            (json!({"document": "a"}), true),
            (json!({}), false),
            (json!({"document": 1}), false),
            (json!({"document": "a", "rawUrl": "http://other"}), false),
            (json!({"document": "a".repeat(33)}), false),
            (json!("not-object"), false),
        ] {
            assert_eq!(declaration.validate_input(&input).is_ok(), accepted);
        }
        assert!(declaration.validate_output(&json!("result")).is_ok());
        assert!(declaration
            .validate_output(&json!({"result": "text"}))
            .is_err());
        for schema in [
            json!({"kind": "string", "maxBytes": 8, "pattern": "x"}),
            json!({"type": "object"}),
            json!({"kind": "reference", "$ref": "other"}),
        ] {
            assert!(serde_json::from_value::<PayloadSchema>(schema).is_err());
        }
        for schema in [
            PayloadSchema::String { max_bytes: 0 },
            PayloadSchema::Array {
                items: Box::new(PayloadSchema::Null),
                max_items: 0,
            },
            PayloadSchema::Object {
                fields: BTreeMap::new(),
                required: BTreeSet::from(["missing".into()]),
            },
        ] {
            assert!(schema.validate().is_err());
        }
        let mut deep = PayloadSchema::Null;
        for _ in 0..=MAX_SCHEMA_DEPTH {
            deep = PayloadSchema::Array {
                items: Box::new(deep),
                max_items: 1,
            };
        }
        assert!(deep.validate().is_err());
    }

    #[test]
    fn all_supported_schema_shapes_validate_values_and_total_node_budget() {
        for (schema, valid, invalid) in [
            (PayloadSchema::Null, json!(null), json!(false)),
            (PayloadSchema::Boolean, json!(true), json!(1)),
            (PayloadSchema::Integer, json!(u64::MAX), json!(1.5)),
            (PayloadSchema::Number, json!(1.5), json!("1.5")),
            (
                PayloadSchema::String { max_bytes: 2 },
                json!("é"),
                json!("éx"),
            ),
            (
                PayloadSchema::Array {
                    items: Box::new(PayloadSchema::Integer),
                    max_items: 2,
                },
                json!([1, 2]),
                json!([1, "two"]),
            ),
        ] {
            schema.validate().unwrap();
            assert!(schema
                .validate_payload(&valid, MAX_CAPABILITY_PAYLOAD_BYTES)
                .is_ok());
            assert!(schema
                .validate_payload(&invalid, MAX_CAPABILITY_PAYLOAD_BYTES)
                .is_err());
        }
        let schema = PayloadSchema::Array {
            items: Box::new(PayloadSchema::Array {
                items: Box::new(PayloadSchema::Null),
                max_items: 1024,
            }),
            max_items: 17,
        };
        schema.validate().unwrap();
        assert!(schema
            .validate_payload(
                &json!(vec![vec![Value::Null; 1024]; 17]),
                MAX_CAPABILITY_PAYLOAD_BYTES
            )
            .is_err());
    }

    #[test]
    fn hard_limit_table_rejects_zero_and_over_hard_maximum() {
        let original = descriptor();
        for field in [
            "timeoutMs",
            "maxInputBytes",
            "maxOutputBytes",
            "maxConcurrency",
        ] {
            for limit in [0, u64::MAX] {
                let mut wire = serde_json::to_value(&original).unwrap();
                wire["limits"][field] = json!(limit);
                assert!(
                    CapabilityDescriptor::from_json(&serde_json::to_vec(&wire).unwrap()).is_err(),
                    "{field}={limit}"
                );
            }
        }
        let mut small = descriptor();
        small.limits.max_output_bytes = 4;
        assert!(small.validate_output(&json!("ab")).is_ok());
        assert!(small.validate_output(&json!("abc")).is_err());
        assert!(small.decode_output(b"   \"ab\"   ").is_err());
        assert!(
            CapabilityDescriptor::from_json(&vec![b' '; MAX_CAPABILITY_DESCRIPTOR_BYTES + 1])
                .is_err()
        );
    }

    #[test]
    fn destinations_require_explicit_bounded_account_and_endpoint_references() {
        for destination in [
            DataDestination::Remote {
                endpoint_ref: "".into(),
                account_ref: "a".into(),
            },
            DataDestination::Remote {
                endpoint_ref: "https://host/path".into(),
                account_ref: "a".into(),
            },
            DataDestination::Remote {
                endpoint_ref: "endpoint".into(),
                account_ref: "".into(),
            },
        ] {
            let mut declaration = descriptor();
            declaration.data_destinations = BTreeSet::from([destination]);
            assert!(declaration.validate().is_err());
        }
        let mut declaration = descriptor();
        declaration.data_destinations.clear();
        assert!(declaration.validate().is_err());
    }

    #[test]
    fn evidence_scope_time_and_authorization_are_independent() {
        let mut snapshot = snapshot();
        let mut other_descriptor = descriptor();
        other_descriptor.revision += 1;
        let mut changed_without_revision = descriptor();
        changed_without_revision.output_schema = PayloadSchema::Integer;
        for other in [
            CapabilitySnapshot::from_declaration("other-generation".into(), descriptor()).unwrap(),
            CapabilitySnapshot::from_declaration(
                "instance.project.registration-generation-1".into(),
                other_descriptor,
            )
            .unwrap(),
            CapabilitySnapshot::from_declaration(
                "instance.project.registration-generation-1".into(),
                changed_without_revision,
            )
            .unwrap(),
        ] {
            assert!(snapshot
                .record_detection(
                    evidence(&other, "wrong-scope", CapabilityEvidenceClaim::Detection),
                    NOW
                )
                .is_err());
        }
        let valid = evidence(&snapshot, "valid", CapabilityEvidenceClaim::Contract);
        assert!(snapshot
            .record_verification(valid.clone(), NOW - 1)
            .is_err());
        assert!(snapshot.record_verification(valid, EXPIRES).is_err());
        snapshot
            .record_detection(
                evidence(&snapshot, "detected", CapabilityEvidenceClaim::Detection),
                NOW,
            )
            .unwrap();
        assert_eq!(
            snapshot.projection(None, "caller", NOW).verification,
            VerificationState::Unverified
        );
        let authorization = make_ready(&mut snapshot);
        assert_eq!(
            snapshot
                .projection(Some(&authorization), "other-caller", NOW)
                .authorization,
            CapabilityAuthorizationState::Forbidden
        );
        assert_eq!(
            snapshot
                .projection(Some(&authorization), "actor.project.session.root", EXPIRES)
                .authorization,
            CapabilityAuthorizationState::Expired
        );
        let rebuilt =
            CapabilitySnapshot::from_declaration("new-generation".into(), descriptor()).unwrap();
        assert_eq!(
            rebuilt
                .projection(Some(&authorization), "actor.project.session.root", NOW)
                .authorization,
            CapabilityAuthorizationState::Forbidden
        );
        assert!(HostCapabilityEvidence::new(
            snapshot.evidence_scope(),
            "bad-time".into(),
            EvidenceOrigin::HostInspection,
            CapabilityEvidenceClaim::Contract,
            NOW,
            NOW
        )
        .is_err());
    }

    #[test]
    fn guarantees_are_separate_from_claims_and_generic_verification() {
        let mut declaration = descriptor();
        declaration.support = CapabilitySupport {
            idempotency: IdempotencySupport::RequestKey,
            cancellation: CancellationSupport::Confirmed,
            metering: MeteringSupport::HardCostLimit,
            task_lifecycle: false,
            bidirectional_collaboration: false,
        };
        let mut snapshot =
            CapabilitySnapshot::from_declaration("binding".into(), declaration).unwrap();
        make_ready(&mut snapshot);
        for guarantee in CapabilityGuarantee::ALL {
            assert!(!snapshot.has_verified_guarantee(guarantee, NOW));
        }
        snapshot
            .record_guarantee(
                CapabilityGuarantee::CancellationRequest,
                evidence(
                    &snapshot,
                    "cancel-request",
                    CapabilityEvidenceClaim::Guarantee(CapabilityGuarantee::CancellationRequest),
                ),
                NOW,
            )
            .unwrap();
        assert!(snapshot.has_verified_guarantee(CapabilityGuarantee::CancellationRequest, NOW));
        assert!(
            !snapshot.has_verified_guarantee(CapabilityGuarantee::CancellationConfirmation, NOW)
        );
        assert!(!snapshot.has_verified_guarantee(CapabilityGuarantee::Idempotency, NOW));
        assert!(!snapshot.has_verified_guarantee(CapabilityGuarantee::EnforcedCostLimit, NOW));
        assert!(!snapshot.has_verified_guarantee(CapabilityGuarantee::CancellationRequest, EXPIRES));
        let projection = serde_json::to_value(snapshot.projection(None, "caller", NOW)).unwrap();
        assert_eq!(
            projection["detectionEvidence"]["origin"],
            "isolated_fixture"
        );
    }

    #[test]
    fn evidence_claim_cannot_be_repurposed_as_other_host_facts() {
        let mut declaration = descriptor();
        declaration.support.cancellation = CancellationSupport::Confirmed;
        declaration.support.metering = MeteringSupport::HardCostLimit;
        let mut snapshot =
            CapabilitySnapshot::from_declaration("binding".into(), declaration).unwrap();
        let detection = evidence(
            &snapshot,
            "detected-only",
            CapabilityEvidenceClaim::Detection,
        );
        assert!(snapshot
            .record_verification(detection.clone(), NOW)
            .is_err());
        assert!(HostCapabilityAuthorization::evaluated("caller".into(), true, detection).is_err());
        let contract = evidence(
            &snapshot,
            "contract-only",
            CapabilityEvidenceClaim::Contract,
        );
        assert!(snapshot
            .record_effect_verification(BTreeSet::from([CapabilityEffect::Read]), contract, NOW)
            .is_err());
        for (proven, not_proven) in [
            (
                CapabilityGuarantee::CancellationRequest,
                CapabilityGuarantee::CancellationConfirmation,
            ),
            (
                CapabilityGuarantee::VerifiedMetering,
                CapabilityGuarantee::EnforcedCostLimit,
            ),
        ] {
            let proof = evidence(
                &snapshot,
                "limited-guarantee",
                CapabilityEvidenceClaim::Guarantee(proven),
            );
            assert!(snapshot.record_guarantee(not_proven, proof, NOW).is_err());
        }
        snapshot
            .record_guarantee(
                CapabilityGuarantee::EnforcedCostLimit,
                evidence(
                    &snapshot,
                    "hard-limit-oracle",
                    CapabilityEvidenceClaim::Guarantee(CapabilityGuarantee::EnforcedCostLimit),
                ),
                NOW,
            )
            .unwrap();
        assert!(snapshot.has_verified_guarantee(CapabilityGuarantee::EnforcedCostLimit, NOW));
        assert!(HostCapabilityEvidence::new(
            snapshot.evidence_scope(),
            "too-long".into(),
            EvidenceOrigin::HostInspection,
            CapabilityEvidenceClaim::Contract,
            NOW,
            NOW + MAX_EVIDENCE_LIFETIME_MS + 1
        )
        .is_err());
    }

    #[test]
    fn sensing_and_actuation_have_host_semantic_effect_floors() {
        for (role, minimum_effect) in [
            (CapabilityRole::Sensor, CapabilityEffect::Privacy),
            (CapabilityRole::Actuator, CapabilityEffect::Physical),
        ] {
            let mut declaration = descriptor();
            declaration.role = role;
            let mut snapshot =
                CapabilitySnapshot::from_declaration("device-capability".into(), declaration)
                    .unwrap();
            make_ready(&mut snapshot);
            snapshot
                .record_effect_verification(
                    BTreeSet::from([CapabilityEffect::Read]),
                    evidence(&snapshot, "host-read", CapabilityEvidenceClaim::Effects),
                    NOW,
                )
                .unwrap();
            let projection = snapshot.projection(None, "caller", NOW);
            assert!(projection.effective_effects.contains(&minimum_effect));
            assert_eq!(projection.approval, ApprovalRequirement::Required);
            assert!(!role.is_member_role());
        }
    }

    #[test]
    fn admission_checks_fresh_facts_role_destination_limits_input_and_required_guarantees() {
        let mut snapshot = snapshot();
        let input = json!({"document": "a"});
        let destinations = snapshot.descriptor().data_destinations.clone();
        let no_guarantees = BTreeSet::new();
        let mut request = CapabilityAdmissionRequest {
            caller_scope: "actor.project.session.root",
            role: CapabilityRole::ToolProvider,
            destination: destinations.first().unwrap(),
            input: &input,
            limits: CapabilityLimits::default(),
            required_guarantees: &no_guarantees,
        };
        let authorization = HostCapabilityAuthorization::evaluated(
            request.caller_scope.into(),
            true,
            evidence(&snapshot, "auth", CapabilityEvidenceClaim::Authorization),
        )
        .unwrap();
        assert!(snapshot
            .check_admission(&request, &authorization, NOW)
            .is_err());
        let authorization = make_ready(&mut snapshot);
        assert_eq!(
            snapshot
                .check_admission(&request, &authorization, NOW)
                .unwrap()
                .approval(),
            ApprovalRequirement::Required
        );
        let forbidden = HostCapabilityAuthorization::evaluated(
            request.caller_scope.into(),
            false,
            evidence(
                &snapshot,
                "forbidden",
                CapabilityEvidenceClaim::Authorization,
            ),
        )
        .unwrap();
        assert!(snapshot.check_admission(&request, &forbidden, NOW).is_err());
        let invalid_input = json!({"document": 1});
        request.input = &invalid_input;
        assert!(snapshot
            .check_admission(&request, &authorization, NOW)
            .is_err());
        request.input = &input;
        for state in [
            CapabilityAvailability::Disabled,
            CapabilityAvailability::Unavailable,
            CapabilityAvailability::Stale,
            CapabilityAvailability::Unknown,
        ] {
            snapshot.set_availability(state);
            assert!(snapshot
                .check_admission(&request, &authorization, NOW)
                .is_err());
        }
        snapshot.set_availability(CapabilityAvailability::Available);
        assert!(snapshot
            .check_admission(&request, &authorization, EXPIRES)
            .is_err());
        request.role = CapabilityRole::Collaborator;
        assert!(snapshot
            .check_admission(&request, &authorization, NOW)
            .is_err());
        request.role = CapabilityRole::ToolProvider;
        request.caller_scope = "other-caller";
        assert!(snapshot
            .check_admission(&request, &authorization, NOW)
            .is_err());
        request.caller_scope = "actor.project.session.root";
        let other_destination = DataDestination::Remote {
            endpoint_ref: "document-api".into(),
            account_ref: "account-b".into(),
        };
        request.destination = &other_destination;
        assert!(snapshot
            .check_admission(&request, &authorization, NOW)
            .is_err());
        request.destination = destinations.first().unwrap();
        request.limits.max_concurrency = 2;
        assert!(snapshot
            .check_admission(&request, &authorization, NOW)
            .is_err());
        request.limits = CapabilityLimits::default();
        let required = BTreeSet::from([CapabilityGuarantee::EnforcedCostLimit]);
        request.required_guarantees = &required;
        assert!(snapshot
            .check_admission(&request, &authorization, NOW)
            .is_err());
        request.required_guarantees = &no_guarantees;
        request.limits.max_output_bytes = 4;
        let admission = snapshot
            .check_admission(&request, &authorization, NOW)
            .unwrap();
        assert!(admission.validate_output(&json!("ab")).is_ok());
        assert!(admission.decode_output(b"\"abc\"").is_err());
        assert!(admission
            .effective_effects()
            .contains(&CapabilityEffect::Unknown));
        assert_eq!(admission.limits().max_output_bytes, 4);
    }

    #[test]
    fn collaborator_requires_independently_verified_task_and_bidirectional_protocols() {
        let mut declaration = descriptor();
        declaration.role = CapabilityRole::Collaborator;
        declaration.support.task_lifecycle = true;
        declaration.support.bidirectional_collaboration = true;
        let mut snapshot =
            CapabilitySnapshot::from_declaration("external-ai".into(), declaration).unwrap();
        let authorization = make_ready(&mut snapshot);
        let input = json!({"document": "a"});
        let destinations = snapshot.descriptor().data_destinations.clone();
        let required_guarantees = BTreeSet::new();
        let request = CapabilityAdmissionRequest {
            caller_scope: "actor.project.session.root",
            role: CapabilityRole::Collaborator,
            destination: destinations.first().unwrap(),
            input: &input,
            limits: CapabilityLimits::default(),
            required_guarantees: &required_guarantees,
        };
        assert!(snapshot
            .check_admission(&request, &authorization, NOW)
            .is_err());
        snapshot
            .record_guarantee(
                CapabilityGuarantee::TaskLifecycle,
                evidence(
                    &snapshot,
                    "task-correlation",
                    CapabilityEvidenceClaim::Guarantee(CapabilityGuarantee::TaskLifecycle),
                ),
                NOW,
            )
            .unwrap();
        assert!(snapshot
            .check_admission(&request, &authorization, NOW)
            .is_err());
        snapshot
            .record_guarantee(
                CapabilityGuarantee::BidirectionalCollaboration,
                evidence(
                    &snapshot,
                    "bidirectional-protocol",
                    CapabilityEvidenceClaim::Guarantee(
                        CapabilityGuarantee::BidirectionalCollaboration,
                    ),
                ),
                NOW,
            )
            .unwrap();
        assert!(snapshot
            .check_admission(&request, &authorization, NOW)
            .is_ok());
    }
}
