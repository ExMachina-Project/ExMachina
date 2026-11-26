//! 纯操作身份与四轴状态：不派发、不存储、不执行授权/预算/取消。
//!
//! 公开 wire 报告 Observation 不含宿主事实。ConnectionOperation/OperationIdentity
//! 不实现 Deserialize；HostChange/HostEvidenceRef 仅 crate 内受信宿主和私有快照可用，
//! 不得直接映射请求正文。证据只有宿主记录引用及 provenance，没有文本/URL/凭据。
//! restore_trusted 只接受受信存储，不是导入/回调 API；结构检查不验证证据真实性、
//! 不防存储回滚，也不恢复执行授权。每次实际动作仍须由后续服务重验完整授权交集。
//! 纯值 CAS 必须另行串行持久化；不实现 3.2 账本、3.8 回调鉴权或任何执行底座。

use super::identity::{ConnectionRef, RegistrationBinding};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};

const SCHEMA_VERSION: u16 = 2;
const MAX_EVENTS: usize = 128;
const MAX_SNAPSHOT_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "u64")]
pub struct OperationRevision(u64);
impl TryFrom<u64> for OperationRevision {
    type Error = anyhow::Error;
    fn try_from(value: u64) -> Result<Self> {
        ensure!(value > 0, "操作 revision 必须大于零");
        Ok(Self(value))
    }
}
impl OperationRevision {
    pub fn get(self) -> u64 {
        self.0
    }
}

/// 仅身份输入，不含状态/证据。必须经 OperationIdentity::new 校验；不能当作授权。
/// 所有 ID 均须由宿主绑定；None 成员表示非成员操作，不是通配符。
/// project_id 是操作上下文；来源 project 为 None 不表示该来源授权所有项目。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OperationAssociation {
    pub request_id: String,
    pub actor_ref: String,
    pub project_id: String,
    pub session_id: String,
    pub root_execution_id: String,
    pub execution_id: String,
    pub member_instance_id: Option<String>,
    pub evidence_session_id: String,
    pub binding: RegistrationBinding,
    pub capability_id: String,
    /// 与 CapabilityDescriptor::revision 一致；构造操作身份及恢复时须非零。
    pub capability_revision: u64,
    /// 有界不透明快照引用，不是 grant，也不恢复执行授权。
    pub authorization_revision: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OperationIdentity(IdentityData);
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct IdentityData {
    operation_id: String,
    association: OperationAssociation,
}
impl OperationIdentity {
    pub fn new(association: OperationAssociation) -> Result<Self> {
        Self::validated(IdentityData {
            operation_id: crate::types::new_id(),
            association,
        })
    }
    fn validated(data: IdentityData) -> Result<Self> {
        let uuid = uuid::Uuid::parse_str(&data.operation_id)?;
        ensure!(
            !uuid.is_nil() && uuid.to_string() == data.operation_id,
            "操作 ID 必须是规范 UUID"
        );
        let a = &data.association;
        for value in [
            &a.request_id,
            &a.actor_ref,
            &a.project_id,
            &a.session_id,
            &a.root_execution_id,
            &a.execution_id,
            &a.evidence_session_id,
            &a.capability_id,
            &a.authorization_revision,
        ]
        .into_iter()
        .chain(a.member_instance_id.iter())
        {
            validate_ref(value)?;
        }
        ensure!(a.capability_revision > 0, "能力 revision 必须大于零");
        if let Some(project) = a.binding.source().scope().project_id() {
            ensure!(project == a.project_id, "来源与操作项目不一致");
        }
        Ok(Self(data))
    }
    pub fn operation_id(&self) -> &str {
        &self.0.operation_id
    }
    pub fn association(&self) -> &OperationAssociation {
        &self.0.association
    }
}

/// 精确远端任务/上下文，不以标题、提示词或显示名认领。None 上下文不是续谈授权。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", try_from = "RemoteData")]
pub struct RemoteCorrelation(RemoteData);
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RemoteData {
    operation_id: String,
    execution_id: String,
    connection: ConnectionRef,
    remote_instance_id: String,
    task_id: String,
    context_id: Option<String>,
}
impl TryFrom<RemoteData> for RemoteCorrelation {
    type Error = anyhow::Error;
    fn try_from(v: RemoteData) -> Result<Self> {
        for value in [
            &v.operation_id,
            &v.execution_id,
            &v.remote_instance_id,
            &v.task_id,
        ]
        .into_iter()
        .chain(v.context_id.iter())
        {
            validate_ref(value)?;
        }
        Ok(Self(v))
    }
}
impl RemoteCorrelation {
    pub fn new(
        identity: &OperationIdentity,
        remote_instance_id: String,
        task_id: String,
        context_id: Option<String>,
    ) -> Result<Self> {
        RemoteData {
            operation_id: identity.operation_id().into(),
            execution_id: identity.association().execution_id.clone(),
            connection: identity.association().binding.connection().clone(),
            remote_instance_id,
            task_id,
            context_id,
        }
        .try_into()
    }
    fn validate(&self, identity: &OperationIdentity) -> Result<()> {
        ensure!(
            self.0.operation_id == identity.operation_id()
                && self.0.execution_id == identity.association().execution_id
                && &self.0.connection == identity.association().binding.connection(),
            "远端关联身份不匹配"
        );
        Ok(())
    }
    pub fn task_id(&self) -> &str {
        &self.0.task_id
    }
    pub fn remote_instance_id(&self) -> &str {
        &self.0.remote_instance_id
    }
    pub fn context_id(&self) -> Option<&str> {
        self.0.context_id.as_deref()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionState {
    Prepared,
    AwaitingApproval,
    Queued,
    Accepted,
    Running,
    AwaitingInput,
    ResultReceived,
    Failed,
    Cancelled,
    TimedOut,
    Unknown,
}
impl ExecutionState {
    fn stopped(self) -> bool {
        matches!(
            self,
            Self::Failed | Self::Cancelled | Self::TimedOut | Self::Unknown
        )
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AcceptanceState {
    NotEvaluated,
    Pending,
    Verified,
    Rejected,
    Blocked,
}
impl AcceptanceState {
    fn final_verdict(self) -> bool {
        matches!(self, Self::Verified | Self::Rejected | Self::Blocked)
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectState {
    NotSent,
    NotApplied,
    Applied,
    Partial,
    Unknown,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteCancellation {
    NotRequested,
    Requested,
    Confirmed,
    Unsupported,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CancellationState {
    local_stopped: bool,
    remote: RemoteCancellation,
}
impl CancellationState {
    pub fn local_stopped(self) -> bool {
        self.local_stopped
    }
    pub fn remote(self) -> RemoteCancellation {
        self.remote
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CancellationUpdate {
    LocalStopped,
    RemoteRequested,
    RemoteConfirmed,
    Unsupported,
}

/// wire 只声明报告种类，不能提供宿主 verified/效果/凭据字段；正文留在外部内容存储。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Observation {
    HttpAccepted {},
    Accepted {},
    Running {},
    AwaitingInput {},
    ResultReceived {},
    ProcessExited { code: i32 },
    TextReported {},
    ClaimedDone {},
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceProvenance {
    RemoteReport,
    AdapterReadback,
    HostVerification,
    UserAcceptance,
}

/// 无 Deserialize；只有受信宿主能创建，私有快照解码的是 EvidenceData。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct HostEvidenceRef(EvidenceData);
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EvidenceData {
    record_ref: String,
    provenance: EvidenceProvenance,
    operation_id: String,
    execution_id: String,
    evidence_session_id: String,
}
impl HostEvidenceRef {
    pub(crate) fn new(
        identity: &OperationIdentity,
        record_ref: String,
        provenance: EvidenceProvenance,
    ) -> Result<Self> {
        validate_ref(&record_ref)?;
        Ok(Self(EvidenceData {
            record_ref,
            provenance,
            operation_id: identity.operation_id().into(),
            execution_id: identity.association().execution_id.clone(),
            evidence_session_id: identity.association().evidence_session_id.clone(),
        }))
    }
}
impl EvidenceData {
    fn validate(&self, identity: &OperationIdentity) -> Result<()> {
        validate_ref(&self.record_ref)?;
        ensure!(
            self.operation_id == identity.operation_id()
                && self.execution_id == identity.association().execution_id
                && self.evidence_session_id == identity.association().evidence_session_id,
            "证据跨操作或证据会话"
        );
        Ok(())
    }
    fn host_fact(&self) -> Result<()> {
        ensure!(
            self.provenance != EvidenceProvenance::RemoteReport,
            "远端自述不是宿主核验事实"
        );
        Ok(())
    }
}

/// 无 Deserialize，不能解码请求正文；只接受宿主构造的更改。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct HostChange(ChangeData);
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case", deny_unknown_fields)]
enum ChangeData {
    Bind {
        remote: RemoteCorrelation,
    },
    DispatchStarted {},
    Transition {
        execution: ExecutionState,
    },
    Observe {
        remote: RemoteCorrelation,
        report: Observation,
        evidence: EvidenceData,
    },
    Effect {
        effect: EffectState,
        evidence: EvidenceData,
    },
    Evaluate {
        verdict: AcceptanceState,
        receipt: Option<EvidenceData>,
        required: Vec<EvidenceData>,
    },
    Cancel {
        update: CancellationUpdate,
        evidence: EvidenceData,
    },
}
impl HostChange {
    pub(crate) fn bind(remote: RemoteCorrelation) -> Self {
        Self(ChangeData::Bind { remote })
    }
    /// 受信宿主在调用传输前记录“可能已越过发送边界”；不派发、不授权。
    /// 初次发送可能尚无远端任务 ID；绑定远端和排队本身都不表示发送。
    pub(crate) fn dispatch_started() -> Self {
        Self(ChangeData::DispatchStarted {})
    }
    pub(crate) fn transition(execution: ExecutionState) -> Self {
        Self(ChangeData::Transition { execution })
    }
    pub(crate) fn observe(
        remote: RemoteCorrelation,
        report: Observation,
        evidence: HostEvidenceRef,
    ) -> Self {
        Self(ChangeData::Observe {
            remote,
            report,
            evidence: evidence.0,
        })
    }
    pub(crate) fn effect(effect: EffectState, evidence: HostEvidenceRef) -> Self {
        Self(ChangeData::Effect {
            effect,
            evidence: evidence.0,
        })
    }
    /// required 必须包含宿主确定的全部必要检查，不是由远端选择的证据子集。
    pub(crate) fn evaluate(
        verdict: AcceptanceState,
        receipt: Option<HostEvidenceRef>,
        required: Vec<HostEvidenceRef>,
    ) -> Self {
        Self(ChangeData::Evaluate {
            verdict,
            receipt: receipt.map(|e| e.0),
            required: required.into_iter().map(|e| e.0).collect(),
        })
    }
    pub(crate) fn cancel(update: CancellationUpdate, evidence: HostEvidenceRef) -> Self {
        Self(ChangeData::Cancel {
            update,
            evidence: evidence.0,
        })
    }
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TrustedSnapshot {
    schema_version: u16,
    identity: IdentityData,
    changes: Vec<ChangeData>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionOperation {
    identity: OperationIdentity,
    remote: Option<RemoteCorrelation>,
    revision: OperationRevision,
    execution: ExecutionState,
    acceptance: AcceptanceState,
    effect: EffectState,
    cancellation: CancellationState,
    changes: Vec<ChangeData>,
}
impl ConnectionOperation {
    pub fn prepared(identity: OperationIdentity) -> Self {
        Self {
            identity,
            remote: None,
            revision: OperationRevision(1),
            execution: ExecutionState::Prepared,
            acceptance: AcceptanceState::NotEvaluated,
            effect: EffectState::NotSent,
            cancellation: CancellationState {
                local_stopped: false,
                remote: RemoteCancellation::NotRequested,
            },
            changes: Vec::new(),
        }
    }
    pub fn identity(&self) -> &OperationIdentity {
        &self.identity
    }
    pub fn remote(&self) -> Option<&RemoteCorrelation> {
        self.remote.as_ref()
    }
    pub fn revision(&self) -> OperationRevision {
        self.revision
    }
    pub fn execution(&self) -> ExecutionState {
        self.execution
    }
    pub fn acceptance(&self) -> AcceptanceState {
        self.acceptance
    }
    pub fn effect(&self) -> EffectState {
        self.effect
    }
    pub fn cancellation(&self) -> CancellationState {
        self.cancellation
    }
    /// false 仅表示状态尚未关闭，不表示已有执行权限或执行依赖就绪。
    pub fn admission_closed(&self) -> bool {
        self.execution.stopped()
            || self.execution == ExecutionState::ResultReceived
            || self.acceptance.final_verdict()
    }
    pub fn is_success(&self) -> bool {
        self.execution == ExecutionState::ResultReceived
            && self.acceptance == AcceptanceState::Verified
            && matches!(self.effect, EffectState::Applied | EffectState::NotApplied)
    }
    /// 纯值 CAS：错误时整个值不变；成功只追加记录，不执行/持久化任何动作。
    pub(crate) fn record_host(
        &mut self,
        expected: OperationRevision,
        change: HostChange,
    ) -> Result<()> {
        ensure!(self.revision == expected, "操作 revision 冲突");
        ensure!(self.changes.len() < MAX_EVENTS, "状态记录达到上限");
        let revision = OperationRevision(
            self.revision
                .0
                .checked_add(1)
                .context("操作 revision 耗尽")?,
        );
        let mut next = self.clone();
        next.apply(&change.0)?;
        next.changes.push(change.0);
        next.revision = revision;
        *self = next;
        Ok(())
    }
    fn apply(&mut self, change: &ChangeData) -> Result<()> {
        match change {
            ChangeData::Bind { remote } => {
                ensure!(
                    !self.admission_closed() && self.remote.is_none(),
                    "不能重绑远端任务"
                );
                remote.validate(&self.identity)?;
                self.remote = Some(remote.clone());
            }
            ChangeData::DispatchStarted {} => {
                ensure!(
                    !self.admission_closed()
                        && self.execution == ExecutionState::Queued
                        && self.effect == EffectState::NotSent,
                    "发送边界需要未关闭、已排队且尚未发送的操作"
                );
                // 必须在真实传输调用前记录；一旦可能已发送，超时/取消就不能再叫 NotSent。
                self.effect = EffectState::Unknown;
            }
            ChangeData::Transition { execution } => {
                ensure!(!self.admission_closed(), "终态不能恢复进度");
                use ExecutionState::*;
                ensure!(
                    matches!(
                        (self.execution, *execution),
                        (Prepared, AwaitingApproval | Queued) | (AwaitingApproval, Queued)
                    ) || execution.stopped(),
                    "非法执行状态转换"
                );
                // 执行失联/失败不改变已知效果；发送可能性由发送边界或已关联报告记录。
                self.execution = *execution;
            }
            ChangeData::Observe {
                remote,
                report,
                evidence,
            } => {
                ensure!(!self.admission_closed(), "迟到进度不能复活已关闭操作");
                remote.validate(&self.identity)?;
                ensure!(
                    self.remote.as_ref() == Some(remote),
                    "报告与绑定远端任务不匹配"
                );
                evidence.validate(&self.identity)?;
                use ExecutionState::*;
                ensure!(
                    matches!(self.execution, Queued | Accepted | Running | AwaitingInput),
                    "报告尚无可接收任务"
                );
                self.execution = match report {
                    Observation::HttpAccepted {} | Observation::Accepted {} => {
                        ensure!(self.execution == Queued, "受理报告不能回退进度");
                        Accepted
                    }
                    Observation::Running {} => Running,
                    Observation::AwaitingInput {} => AwaitingInput,
                    Observation::ResultReceived {} | Observation::ProcessExited { code: 0 } => {
                        ResultReceived
                    }
                    Observation::ProcessExited { .. } => Failed,
                    Observation::TextReported {} | Observation::ClaimedDone {} => self.execution,
                };
                if self.effect == EffectState::NotSent {
                    self.effect = EffectState::Unknown;
                }
            }
            ChangeData::Effect { effect, evidence } => {
                evidence.validate(&self.identity)?;
                evidence.host_fact()?;
                ensure!(*effect != EffectState::NotSent, "不能重置为未发送");
                ensure!(
                    self.effect != EffectState::NotSent,
                    "发送边界或远端报告前不能记录效果"
                );
                if !self.execution.stopped() && !self.acceptance.final_verdict() {
                    self.effect = *effect;
                }
            }
            ChangeData::Evaluate {
                verdict,
                receipt,
                required,
            } => {
                ensure!(
                    *verdict != AcceptanceState::NotEvaluated
                        && !required.is_empty()
                        && required.len() <= 16,
                    "验收记录无效"
                );
                for evidence in receipt.iter().chain(required) {
                    evidence.validate(&self.identity)?;
                }
                let supplementary = self.execution.stopped() || self.acceptance.final_verdict();
                if *verdict == AcceptanceState::Verified {
                    receipt
                        .as_ref()
                        .context("verified 必须有回执核验引用")?
                        .host_fact()?;
                    ensure!(
                        required.iter().all(|e| matches!(
                            e.provenance,
                            EvidenceProvenance::HostVerification
                                | EvidenceProvenance::UserAcceptance
                        )),
                        "verified 缺少宿主必需核验"
                    );
                    ensure!(
                        supplementary
                            || (self.execution == ExecutionState::ResultReceived
                                && matches!(
                                    self.effect,
                                    EffectState::Applied | EffectState::NotApplied
                                )),
                        "结果或效果未核实"
                    );
                } else {
                    for evidence in required {
                        evidence.host_fact()?;
                    }
                }
                // 终态后核验只补记录，保留原失败/受阻/未知轴及其证据。
                if !supplementary {
                    self.acceptance = *verdict;
                }
            }
            ChangeData::Cancel { update, evidence } => {
                evidence.validate(&self.identity)?;
                evidence.host_fact()?;
                use CancellationUpdate::*;
                use RemoteCancellation as R;
                match update {
                    LocalStopped => self.cancellation.local_stopped = true,
                    RemoteRequested => {
                        ensure!(
                            self.cancellation.remote == R::NotRequested && self.remote.is_some(),
                            "远端取消请求无有效关联或已记录"
                        );
                        self.cancellation.remote = R::Requested;
                    }
                    RemoteConfirmed => {
                        ensure!(
                            self.cancellation.remote == R::Requested,
                            "没有关联的远端取消请求"
                        );
                        self.cancellation.remote = R::Confirmed;
                    }
                    Unsupported => {
                        ensure!(
                            self.cancellation.remote != R::Confirmed,
                            "已确认停止不能降级"
                        );
                        self.cancellation.remote = R::Unsupported;
                    }
                }
                if !self.admission_closed() {
                    self.execution = ExecutionState::Cancelled;
                }
                // 停止确认不证明回滚、无效果或费用归零，effect 保持原值。
            }
        }
        Ok(())
    }
    pub(crate) fn trusted_snapshot(&self) -> Result<Vec<u8>> {
        let bytes = serde_json::to_vec(&TrustedSnapshot {
            schema_version: SCHEMA_VERSION,
            identity: self.identity.0.clone(),
            changes: self.changes.clone(),
        })?;
        ensure!(bytes.len() <= MAX_SNAPSHOT_BYTES, "操作状态快照过大");
        Ok(bytes)
    }
    pub(crate) fn restore_trusted(current_instance_id: &str, bytes: &[u8]) -> Result<Self> {
        ensure!(bytes.len() <= MAX_SNAPSHOT_BYTES, "操作状态快照过大");
        let snapshot: TrustedSnapshot = serde_json::from_slice(bytes)?;
        ensure!(
            snapshot.schema_version == SCHEMA_VERSION && snapshot.changes.len() <= MAX_EVENTS,
            "操作快照版本或记录数量无效"
        );
        let identity = OperationIdentity::validated(snapshot.identity)?;
        ensure!(
            identity.association().binding.connection().instance_id() == current_instance_id,
            "不能跨实例恢复操作"
        );
        let mut operation = Self::prepared(identity);
        for change in snapshot.changes {
            let expected = operation.revision;
            operation.record_host(expected, HostChange(change))?;
        }
        Ok(operation)
    }
}
fn validate_ref(value: &str) -> Result<()> {
    ensure!(
        !value.is_empty()
            && value.len() <= 256
            && !value.contains("://")
            && !value.starts_with('/')
            && value
                .chars()
                .all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.' | ':' | '/' | '@'))
            && value
                .split('/')
                .all(|part| !matches!(part, "" | "." | "..")),
        "需要有界非秘密记录引用"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connections::identity::{
        ConnectionKind, RegistrationRegistry, SourceKind, SourceLifecycle, SourceRef, SourceScope,
    };
    use serde_json::{json, Value};
    const INSTANCE: &str = "11111111-1111-4111-8111-111111111111";
    const OTHER: &str = "22222222-2222-4222-8222-222222222222";
    fn operation() -> ConnectionOperation {
        let mut registry = RegistrationRegistry::new(INSTANCE).unwrap();
        let source = SourceRef::new(
            SourceKind::Software,
            "source",
            SourceScope::new(INSTANCE, "owner", "account", Some("project".into())).unwrap(),
            Some("assistant".into()),
            SourceLifecycle::known("generation").unwrap(),
            "revision-1",
        )
        .unwrap();
        let reference = registry
            .register(ConnectionKind::ExternalAgent, source, "同名 AI")
            .unwrap();
        registry.set_enabled(&reference, true).unwrap();
        let identity = OperationIdentity::new(OperationAssociation {
            request_id: "request".into(),
            actor_ref: "actor".into(),
            project_id: "project".into(),
            session_id: "session".into(),
            root_execution_id: "root".into(),
            execution_id: "execution".into(),
            member_instance_id: Some("member".into()),
            evidence_session_id: "evidence-session".into(),
            binding: registry.binding_for(&reference).unwrap(),
            capability_id: "capability".into(),
            capability_revision: 1,
            authorization_revision: "auth-rev-1".into(),
        })
        .unwrap();
        ConnectionOperation::prepared(identity)
    }
    fn proof(op: &ConnectionOperation, provenance: EvidenceProvenance) -> HostEvidenceRef {
        HostEvidenceRef::new(op.identity(), "host:record-1".into(), provenance).unwrap()
    }
    fn change(op: &mut ConnectionOperation, change: HostChange) -> Result<()> {
        op.record_host(op.revision(), change)
    }
    fn queued() -> ConnectionOperation {
        let mut op = operation();
        let remote = RemoteCorrelation::new(
            op.identity(),
            "remote-instance".into(),
            "task-1".into(),
            None,
        )
        .unwrap();
        change(&mut op, HostChange::bind(remote)).unwrap();
        change(&mut op, HostChange::transition(ExecutionState::Queued)).unwrap();
        op
    }
    fn report(op: &mut ConnectionOperation, report: Observation) -> Result<()> {
        let update = HostChange::observe(
            op.remote().unwrap().clone(),
            report,
            proof(op, EvidenceProvenance::RemoteReport),
        );
        change(op, update)
    }
    fn effect(op: &mut ConnectionOperation, effect: EffectState) -> Result<()> {
        let update = HostChange::effect(effect, proof(op, EvidenceProvenance::HostVerification));
        change(op, update)
    }
    fn evaluate(op: &mut ConnectionOperation, verdict: AcceptanceState) -> Result<()> {
        let update = HostChange::evaluate(
            verdict,
            Some(proof(op, EvidenceProvenance::AdapterReadback)),
            vec![proof(op, EvidenceProvenance::HostVerification)],
        );
        change(op, update)
    }
    fn cancel(op: &mut ConnectionOperation, update: CancellationUpdate) -> Result<()> {
        let update = HostChange::cancel(update, proof(op, EvidenceProvenance::AdapterReadback));
        change(op, update)
    }
    #[test]
    fn capability_revision_is_a_nonzero_number_and_identity_is_not_authorization() {
        let association = operation().identity().association().clone();
        let mut zero = association.clone();
        zero.capability_revision = 0;
        assert!(OperationIdentity::new(zero).is_err());
        for revision in [1, u64::MAX] {
            let mut valid = association.clone();
            valid.capability_revision = revision;
            let identity = OperationIdentity::new(valid).unwrap();
            assert_eq!(identity.association().capability_revision, revision);
            assert_eq!(
                serde_json::to_value(identity).unwrap()["association"]["capabilityRevision"],
                json!(revision)
            );
        }
        for revision in [json!(0), json!("1"), json!(-1), json!(1.5), Value::Null] {
            let mut wire = serde_json::to_value(&association).unwrap();
            wire["capabilityRevision"] = revision;
            assert!(serde_json::from_value::<OperationAssociation>(wire)
                .ok()
                .and_then(|a| OperationIdentity::new(a).ok())
                .is_none());
        }
        let mut mismatch = association.clone();
        mismatch.project_id = "other-project".into();
        assert!(OperationIdentity::new(mismatch).is_err());

        // 安装级来源仍保留操作的明确项目；仅构造上下文，不求交/授予任何项目权限。
        for project in ["project", "other-project"] {
            let mut wire = serde_json::to_value(&association).unwrap();
            wire["binding"]["source"]["scope"]["projectId"] = Value::Null;
            wire["projectId"] = json!(project);
            wire["memberInstanceId"] = Value::Null;
            let identity = OperationIdentity::new(serde_json::from_value(wire).unwrap()).unwrap();
            assert_eq!(identity.association().project_id, project);
            assert_eq!(
                identity.association().binding.source().scope().project_id(),
                None
            );
            assert_eq!(identity.association().authorization_revision, "auth-rev-1");
        }
    }
    #[test]
    fn transport_reports_do_not_imply_verified_success() {
        for observation in [
            Observation::HttpAccepted {},
            Observation::ProcessExited { code: 0 },
            Observation::TextReported {},
            Observation::ClaimedDone {},
            Observation::ResultReceived {},
        ] {
            let mut op = queued();
            report(&mut op, observation).unwrap();
            assert!(!op.is_success());
            assert_eq!(op.acceptance(), AcceptanceState::NotEvaluated);
            assert_eq!(op.effect(), EffectState::Unknown);
            assert!(evaluate(&mut op, AcceptanceState::Verified).is_err());
        }
    }
    #[test]
    fn dispatch_boundary_distinguishes_not_sent_from_possible_send() {
        let mut unbound = operation();
        let before = unbound.clone();
        assert!(change(&mut unbound, HostChange::dispatch_started()).is_err());
        assert_eq!(unbound, before);
        change(&mut unbound, HostChange::transition(ExecutionState::Queued)).unwrap();
        change(&mut unbound, HostChange::dispatch_started()).unwrap();
        assert!(unbound.remote().is_none());
        assert_eq!(unbound.effect(), EffectState::Unknown);
        let remote = RemoteCorrelation::new(
            unbound.identity(),
            "remote-instance".into(),
            "task-1".into(),
            None,
        )
        .unwrap();
        change(&mut unbound, HostChange::bind(remote)).unwrap();
        assert_eq!(
            ConnectionOperation::restore_trusted(INSTANCE, &unbound.trusted_snapshot().unwrap())
                .unwrap(),
            unbound
        );

        let mut op = operation();
        let remote = RemoteCorrelation::new(
            op.identity(),
            "remote-instance".into(),
            "task-1".into(),
            None,
        )
        .unwrap();
        change(&mut op, HostChange::bind(remote)).unwrap();
        assert_eq!(op.effect(), EffectState::NotSent);
        let before = op.clone();
        assert!(change(&mut op, HostChange::dispatch_started()).is_err());
        assert_eq!(op, before);

        change(&mut op, HostChange::transition(ExecutionState::Queued)).unwrap();
        assert_eq!(op.effect(), EffectState::NotSent);
        change(&mut op, HostChange::dispatch_started()).unwrap();
        assert_eq!(op.effect(), EffectState::Unknown);
        let before = op.clone();
        assert!(change(&mut op, HostChange::dispatch_started()).is_err());
        assert_eq!(op, before);

        for dispatched in [false, true] {
            let mut base = queued();
            assert_eq!(base.effect(), EffectState::NotSent);
            if dispatched {
                change(&mut base, HostChange::dispatch_started()).unwrap();
            }
            let expected = if dispatched {
                EffectState::Unknown
            } else {
                EffectState::NotSent
            };
            for stop in [
                ExecutionState::Failed,
                ExecutionState::TimedOut,
                ExecutionState::Cancelled,
                ExecutionState::Unknown,
            ] {
                let mut op = base.clone();
                change(&mut op, HostChange::transition(stop)).unwrap();
                assert_eq!(op.effect(), expected);
                assert!(op.admission_closed() && !op.is_success());
                let before = op.clone();
                assert!(change(&mut op, HostChange::dispatch_started()).is_err());
                assert!(report(&mut op, Observation::ResultReceived {}).is_err());
                assert_eq!(op, before);
            }
            for update in [
                CancellationUpdate::LocalStopped,
                CancellationUpdate::RemoteRequested,
                CancellationUpdate::Unsupported,
            ] {
                let mut op = base.clone();
                cancel(&mut op, update).unwrap();
                if update == CancellationUpdate::RemoteRequested {
                    cancel(&mut op, CancellationUpdate::RemoteConfirmed).unwrap();
                }
                assert_eq!(op.effect(), expected);
                assert!(op.admission_closed() && !op.is_success());
                assert_eq!(
                    ConnectionOperation::restore_trusted(INSTANCE, &op.trusted_snapshot().unwrap())
                        .unwrap(),
                    op
                );
            }
        }
    }
    #[test]
    fn effects_cannot_make_a_prepared_operation_successful() {
        for mut op in [operation(), queued()] {
            let before = op.clone();
            for state in [
                EffectState::NotSent,
                EffectState::NotApplied,
                EffectState::Applied,
                EffectState::Partial,
                EffectState::Unknown,
            ] {
                assert!(effect(&mut op, state).is_err());
                assert_eq!(op, before);
            }
            assert_eq!(op.effect(), EffectState::NotSent);
            assert!(!op.is_success());
            assert!(evaluate(&mut op, AcceptanceState::Verified).is_err());
            assert_eq!(op, before);
        }
    }
    #[test]
    fn receipt_effect_and_host_evaluation_are_separate_success_prerequisites() {
        let mut op = queued();
        report(&mut op, Observation::HttpAccepted {}).unwrap();
        assert_eq!(op.execution(), ExecutionState::Accepted);
        report(&mut op, Observation::Running {}).unwrap();
        report(&mut op, Observation::ResultReceived {}).unwrap();
        effect(&mut op, EffectState::NotApplied).unwrap();
        evaluate(&mut op, AcceptanceState::Pending).unwrap();
        assert!(!op.is_success());
        evaluate(&mut op, AcceptanceState::Verified).unwrap();
        assert!(op.is_success());
        assert!(op.admission_closed());
        assert_eq!(
            ConnectionOperation::restore_trusted(INSTANCE, &op.trusted_snapshot().unwrap())
                .unwrap(),
            op
        );
    }
    #[test]
    fn terminal_states_and_final_verdicts_never_reopen_or_erase_uncertain_evidence() {
        for execution in [
            ExecutionState::Failed,
            ExecutionState::TimedOut,
            ExecutionState::Cancelled,
            ExecutionState::Unknown,
        ] {
            let mut op = queued();
            report(&mut op, Observation::Running {}).unwrap();
            change(&mut op, HostChange::transition(execution)).unwrap();
            let before = op.clone();
            assert!(report(&mut op, Observation::Running {}).is_err());
            assert_eq!(op, before);
            evaluate(&mut op, AcceptanceState::Verified).unwrap();
            effect(&mut op, EffectState::Applied).unwrap();
            assert_eq!(op.execution(), execution);
            assert_eq!(op.acceptance(), AcceptanceState::NotEvaluated);
            assert_eq!(op.effect(), EffectState::Unknown);
            assert!(op.admission_closed() && !op.is_success());
            assert_eq!(&op.changes[..before.changes.len()], &before.changes);
        }
        for verdict in [AcceptanceState::Rejected, AcceptanceState::Blocked] {
            let mut op = queued();
            report(&mut op, Observation::ResultReceived {}).unwrap();
            evaluate(&mut op, verdict).unwrap();
            evaluate(&mut op, AcceptanceState::Verified).unwrap();
            assert_eq!(op.acceptance(), verdict);
            assert!(!op.is_success());
        }
        for confirmed in [
            EffectState::Applied,
            EffectState::NotApplied,
            EffectState::Partial,
        ] {
            for stop in [
                ExecutionState::Failed,
                ExecutionState::Cancelled,
                ExecutionState::TimedOut,
                ExecutionState::Unknown,
            ] {
                let mut op = queued();
                change(&mut op, HostChange::dispatch_started()).unwrap();
                effect(&mut op, confirmed).unwrap();
                change(&mut op, HostChange::transition(stop)).unwrap();
                assert_eq!(op.effect(), confirmed);
                assert!(op.admission_closed() && !op.is_success());
            }
        }
    }
    #[test]
    fn remote_claims_missing_receipts_and_cross_scoped_evidence_cannot_verify() {
        let mut op = queued();
        report(&mut op, Observation::ResultReceived {}).unwrap();
        effect(&mut op, EffectState::Applied).unwrap();
        let before = op.clone();
        for provenance in [
            EvidenceProvenance::RemoteReport,
            EvidenceProvenance::AdapterReadback,
        ] {
            let invalid = HostChange::evaluate(
                AcceptanceState::Verified,
                Some(proof(&op, EvidenceProvenance::AdapterReadback)),
                vec![proof(&op, provenance)],
            );
            assert!(change(&mut op, invalid).is_err());
        }
        let foreign = proof(&queued(), EvidenceProvenance::HostVerification);
        for receipt in [None, Some(foreign)] {
            let invalid = HostChange::evaluate(
                AcceptanceState::Verified,
                receipt,
                vec![proof(&op, EvidenceProvenance::HostVerification)],
            );
            assert!(change(&mut op, invalid).is_err());
        }
        assert_eq!(op, before);
    }
    #[test]
    fn cancellation_separates_local_stop_remote_request_confirmation_and_unsupported() {
        let mut op = queued();
        report(&mut op, Observation::Running {}).unwrap();
        effect(&mut op, EffectState::Applied).unwrap();
        assert!(cancel(&mut op, CancellationUpdate::RemoteConfirmed).is_err());
        for update in [
            CancellationUpdate::LocalStopped,
            CancellationUpdate::RemoteRequested,
            CancellationUpdate::RemoteConfirmed,
        ] {
            cancel(&mut op, update).unwrap();
            assert!(op.admission_closed() && !op.is_success());
            assert_eq!(op.effect(), EffectState::Applied);
        }
        assert!(op.cancellation().local_stopped());
        assert_eq!(op.cancellation().remote(), RemoteCancellation::Confirmed);
        assert!(report(&mut op, Observation::ResultReceived {}).is_err());
        let mut unsupported = queued();
        report(&mut unsupported, Observation::Running {}).unwrap();
        cancel(&mut unsupported, CancellationUpdate::Unsupported).unwrap();
        assert_eq!(unsupported.effect(), EffectState::Unknown);
        assert_eq!(
            unsupported.cancellation().remote(),
            RemoteCancellation::Unsupported
        );
        assert!(!unsupported.cancellation().local_stopped());
    }
    #[test]
    fn exact_revision_and_remote_identity_cas_reject_regression_without_mutation() {
        let mut op = queued();
        let revision = op.revision();
        report(&mut op, Observation::Running {}).unwrap();
        let before = op.clone();
        assert!(op
            .record_host(revision, HostChange::transition(ExecutionState::Failed))
            .is_err());
        for execution in [
            ExecutionState::Prepared,
            ExecutionState::Queued,
            ExecutionState::Accepted,
            ExecutionState::ResultReceived,
        ] {
            assert!(change(&mut op, HostChange::transition(execution)).is_err());
        }
        let remote = serde_json::to_value(op.remote().unwrap()).unwrap();
        for key in [
            "operationId",
            "executionId",
            "remoteInstanceId",
            "taskId",
            "contextId",
        ] {
            let mut altered = remote.clone();
            altered[key] = json!("other-id");
            let invalid = HostChange::observe(
                serde_json::from_value(altered).unwrap(),
                Observation::Running {},
                proof(&op, EvidenceProvenance::RemoteReport),
            );
            assert!(change(&mut op, invalid).is_err());
        }
        assert!(report(&mut op, Observation::HttpAccepted {}).is_err());
        assert_eq!(op, before);
        assert!(OperationRevision::try_from(0).is_err());

        let mut completed = queued();
        report(&mut completed, Observation::ResultReceived {}).unwrap();
        let before = completed.clone();
        for execution in [
            ExecutionState::Failed,
            ExecutionState::Cancelled,
            ExecutionState::TimedOut,
            ExecutionState::Unknown,
        ] {
            assert!(change(&mut completed, HostChange::transition(execution)).is_err());
        }
        assert_eq!(completed, before);
    }
    #[test]
    fn wire_cannot_restore_host_facts_and_trusted_restore_replays_validated_history() {
        for value in [
            json!({"kind":"verified"}),
            json!({"kind":"running","acceptance":"verified"}),
            json!({"kind":"running","effect":"applied"}),
            json!({"kind":"running","evidence":{}}),
        ] {
            assert!(serde_json::from_value::<Observation>(value).is_err());
        }
        let mut op = queued();
        report(&mut op, Observation::ResultReceived {}).unwrap();
        let snapshot: Value = serde_json::from_slice(&op.trusted_snapshot().unwrap()).unwrap();
        for (pointer, value) in [
            ("/schemaVersion", json!(99)),
            ("/identity/association/projectId", json!("other-project")),
            ("/identity/association/capabilityRevision", json!(0)),
            ("/identity/association/authorizationRevision", json!("")),
        ] {
            let mut changed = snapshot.clone();
            *changed.pointer_mut(pointer).unwrap() = value;
            assert!(ConnectionOperation::restore_trusted(
                INSTANCE,
                &serde_json::to_vec(&changed).unwrap()
            )
            .is_err());
        }
        let mut injected = snapshot.clone();
        injected["acceptance"] = json!("verified");
        assert!(ConnectionOperation::restore_trusted(
            INSTANCE,
            &serde_json::to_vec(&injected).unwrap()
        )
        .is_err());
        assert!(
            ConnectionOperation::restore_trusted(OTHER, &op.trusted_snapshot().unwrap()).is_err()
        );
        let mut regression = snapshot;
        regression["changes"]
            .as_array_mut()
            .unwrap()
            .push(json!({"event":"transition","execution":"running"}));
        assert!(ConnectionOperation::restore_trusted(
            INSTANCE,
            &serde_json::to_vec(&regression).unwrap()
        )
        .is_err());
    }
}
