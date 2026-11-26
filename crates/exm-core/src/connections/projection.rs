//! 既有来源的同步、只读管理面投影；不是 Agent 查询面，也不是执行/授权接口。
//!
//! 本模块只消费配置引用、PluginManager 本地清单、MCP 已缓存工具名和 DeviceService
//! 快照。目录不持有端点、命令、环境、凭据、工具 schema、来源错误正文或设备 grant，
//! 编辑目标也只是原服务的导航元数据。调用方须先做管理入口鉴权；后续面向 Agent 的
//! 目录服务必须按 `SourceKey` 求交来源 ACL，再计算数量、分页、能力和关系。
//!
//! 当前来源都没有可证明删除/重建的生命周期，故始终 `LegacyUnresolved`。
//! `unversioned` 明确表示来源未提供 revision，不能用于失效协调或授权快照；不拿
//! URL、插件版本、设备时间戳或显示名冒充代际。持久陈旧缓存/revision 协调由目录服务
//! 负责；legacy 投影不登记连接，只有受信 registration_store 中已经存在的登记才会
//! 附带其 ConnectionRef，不生成 grant 或组织成员。

use super::capability::{
    CapabilityAuthorizationState, CapabilityEffect, CapabilityRole, CapabilitySupport,
    VerificationState,
};
use super::identity::{
    ConnectionKind, ConnectionRef, ConnectionRegistration, ConnectionRelationKind,
    RegistrationRegistry, RegistrationState, SourceKind, SourceLifecycle, SourceRef, SourceScope,
};
use crate::config::LinkTarget;
use crate::device::{
    DeviceCandidate, DeviceCapability, DeviceKind, DeviceSnapshot, DeviceStatus, DeviceTransport,
};
use crate::mcp::McpServerConfig;
use crate::plugin::PluginEntry;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, HashMap};

const MAX_PROJECTED_CAPABILITIES: usize = 200;
const MAX_PROJECTED_RELATIONS: usize = 200;

/// 来源槽位，仅供管理面与后续来源 ACL 对照；不是可授权的连接身份。
/// revision 和生命周期不参与槽位比较；授权必须另用身份登记服务的完整代际绑定。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SourceKey {
    pub connection_kind: ConnectionKind,
    pub kind: SourceKind,
    pub source_id: String,
    pub scope: SourceScope,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subresource_id: Option<String>,
}

impl SourceKey {
    pub fn from_ref(connection_kind: ConnectionKind, source: &SourceRef) -> Self {
        Self {
            connection_kind,
            kind: source.kind(),
            source_id: source.source_id().to_owned(),
            scope: source.scope().clone(),
            subresource_id: source.subresource_id().map(str::to_owned),
        }
    }
}

/// 来源域由宿主提供稳定安装 UUID；查询时不能临时生成实例身份。
/// 这里的 account 是本地配置域，不是声称已识别远端账户；来源仍保持未纳管。
#[derive(Debug, Clone)]
pub(crate) struct SourceProjectionContext {
    nexus: SourceScope,
    mcp: SourceScope,
    plugin: SourceScope,
    device: SourceScope,
}

impl SourceProjectionContext {
    pub fn new(instance_id: &str) -> anyhow::Result<Self> {
        let scope = |authority| {
            SourceScope::new(instance_id, authority, "local_configuration", None)
                .map_err(|_| anyhow::anyhow!("连接投影的安装实例标识无效"))
        };
        Ok(Self {
            nexus: scope("core.nexus.links")?,
            mcp: scope("core.mcp.servers")?,
            plugin: scope("core.plugins")?,
            device: scope("core.devices")?,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SourceEditSurface {
    NexusLinks,
    McpServers,
    Plugins,
    DeviceRegistration,
    DevicePairing,
    SoftwareRegistration,
}

/// 显式指向原管理服务，不是新编辑 API，不含可回写配置或全局交互目标。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SourceEditTarget {
    pub surface: SourceEditSurface,
    pub source_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ProjectionAvailability {
    Unknown,
    Disabled,
    Unavailable,
    Unsupported,
}

/// 仅用于决定稳定的未就绪原因，不序列化凭据状态或其他凭据元数据。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SourceCredentialState {
    Unknown,
    Configured,
    Missing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SourceHandshakeState {
    NotApplicable,
    /// 没有握手证据，不是断言对端从未握手；成功的空 tools/list 也可能是此状态。
    NotObserved,
    /// 仅有过去清单证据，不能证明当前会话存活或工具可执行。
    CachedEvidence,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SourceSnapshotFreshness {
    NotProvided,
    /// 来源提供缓存但未提供获取时间/有效期；不能称为最新、在线或已验证。
    Undated,
    Stale,
}

/// 有限稳定原因码；不解析/回显远端错误文本，以免把凭据或端点带入响应和日志。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ProjectionReason {
    LegacyUnresolved,
    LegacyTextOnly,
    SourceDisabled,
    MissingCredentials,
    HandshakeNotObserved,
    SnapshotUndated,
    StaleSnapshot,
    SourceUnavailable,
    UnsupportedSource,
    DeviceExperimentDisabled,
    DeviceUnpaired,
    DeviceDisconnected,
    DeviceBackendNotRegistered,
    DeviceBackendNotObserved,
    DeviceHardwareUnverified,
    InvalidSourceReference,
    InvalidCapabilityName,
    DuplicateSourceId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SourceReadiness {
    pub enabled: bool,
    pub availability: ProjectionAvailability,
    pub handshake: SourceHandshakeState,
    pub freshness: SourceSnapshotFreshness,
    pub reasons: Vec<ProjectionReason>,
}

impl SourceReadiness {
    fn new(enabled: bool) -> Self {
        let mut state = Self {
            enabled,
            availability: ProjectionAvailability::Unknown,
            handshake: SourceHandshakeState::NotApplicable,
            freshness: SourceSnapshotFreshness::NotProvided,
            reasons: vec![ProjectionReason::LegacyUnresolved],
        };
        if !enabled {
            state.block(
                ProjectionAvailability::Disabled,
                ProjectionReason::SourceDisabled,
            );
        }
        state
    }

    fn reason(&mut self, reason: ProjectionReason) {
        if !self.reasons.contains(&reason) {
            self.reasons.push(reason);
        }
    }

    fn block(&mut self, availability: ProjectionAvailability, reason: ProjectionReason) {
        // 保留所有正交原因；来源/实验关闭优先展示，但不抹掉缺凭据或后端错误。
        match (self.availability, availability) {
            (ProjectionAvailability::Disabled, _)
            | (ProjectionAvailability::Unsupported, ProjectionAvailability::Unavailable) => {}
            _ => self.availability = availability,
        }
        self.reason(reason);
    }

    fn cached(&mut self, stale: bool) {
        self.freshness = if stale {
            SourceSnapshotFreshness::Stale
        } else {
            SourceSnapshotFreshness::Undated
        };
        self.reason(if stale {
            ProjectionReason::StaleSnapshot
        } else {
            ProjectionReason::SnapshotUndated
        });
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CapabilityListingOrigin {
    LegacyTextConfiguration,
    LocalManifest,
    CachedToolList,
    DeviceDeclaration,
}

/// 只用于展示的最小摘要，不是可提交给 invoke 的版本化能力契约。
/// 不复制不可信 description/schema；所有授权/真实验证保持未知/未验证。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SourceCapabilitySummary {
    pub name: String,
    pub role: CapabilityRole,
    pub origin: CapabilityListingOrigin,
    pub effects: BTreeSet<CapabilityEffect>,
    pub support: CapabilitySupport,
    pub verification: VerificationState,
    pub authorization: CapabilityAuthorizationState,
}

impl SourceCapabilitySummary {
    fn new(name: String, role: CapabilityRole, origin: CapabilityListingOrigin) -> Self {
        let mut effects = BTreeSet::from([CapabilityEffect::Unknown]);
        match role {
            CapabilityRole::Sensor => {
                effects.insert(CapabilityEffect::Privacy);
            }
            CapabilityRole::Actuator => {
                effects.insert(CapabilityEffect::Physical);
            }
            _ => {}
        }
        Self {
            name,
            role,
            origin,
            effects,
            support: CapabilitySupport::default(),
            verification: VerificationState::Unverified,
            authorization: CapabilityAuthorizationState::Unknown,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DeviceBackendState {
    NotObserved,
    NoAdapterRegistered,
    Unsupported,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DevicePairingState {
    Registered,
    Unpaired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DeviceProjectionState {
    pub kind: DeviceKind,
    pub transport: DeviceTransport,
    pub status: DeviceStatus,
    pub pairing: DevicePairingState,
    pub experiment_enabled: bool,
    pub backend: DeviceBackendState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SourceProjection {
    pub source_ref: SourceRef,
    pub kind: ConnectionKind,
    /// 名称只作展示，附来源种类和完整本地 ID 区分同名对象；身份只看 source_ref。
    pub display_name: String,
    pub source_edit_target: SourceEditTarget,
    pub readiness: SourceReadiness,
    pub capabilities: Vec<SourceCapabilitySummary>,
    pub capabilities_total: usize,
    pub capabilities_truncated: bool,
    pub relations: Vec<AdminRelationProjection>,
    pub relations_total: usize,
    pub relations_truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reference: Option<ConnectionRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub registration_state: Option<RegistrationState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device: Option<DeviceProjectionState>,
}

impl SourceProjection {
    pub fn source_key(&self) -> SourceKey {
        SourceKey::from_ref(self.kind, &self.source_ref)
    }
}

/// 无有效 sourceRef 的来源只输出类别和原因，不回显可能含秘密的非法 ID。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProjectionIssue {
    pub source: SourceKind,
    pub reason: ProjectionReason,
}

/// 内部完整管理快照；不可直接作为 Agent ACL 已过滤的列表返回。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SourceProjectionSnapshot {
    pub records: Vec<SourceProjection>,
    pub issues: Vec<ProjectionIssue>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AdminRelationEndpoint {
    pub reference: ConnectionRef,
    pub display_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AdminRelationProjection {
    pub id: String,
    pub kind: ConnectionRelationKind,
    pub revision: u64,
    pub from: AdminRelationEndpoint,
    pub to: AdminRelationEndpoint,
}

impl SourceProjectionSnapshot {
    fn push(&mut self, mut projection: SourceProjection) {
        projection.capabilities_total = projection
            .capabilities_total
            .max(projection.capabilities.len());
        projection.capabilities_truncated |=
            projection.capabilities_total > MAX_PROJECTED_CAPABILITIES;
        projection.capabilities.truncate(MAX_PROJECTED_CAPABILITIES);
        let key = projection.source_key();
        if let Some(existing) = self
            .records
            .iter_mut()
            .find(|item| item.source_key() == key)
        {
            // 配置中的重复 ID 不成为两个权威对象，也不能因任选一条配置而声称可用。
            existing.readiness.block(
                ProjectionAvailability::Unavailable,
                ProjectionReason::DuplicateSourceId,
            );
        } else {
            self.records.push(projection);
        }
    }

    pub fn extend(&mut self, other: Self) {
        for record in other.records {
            self.push(record);
        }
        self.issues.extend(other.issues);
    }

    fn add_relation(&mut self, relation: AdminRelationProjection) {
        for reference in [&relation.from.reference, &relation.to.reference] {
            if let Some(record) = self
                .records
                .iter_mut()
                .find(|record| record.reference.as_ref() == Some(reference))
            {
                record.relations_total += 1;
                if record.relations.len() < MAX_PROJECTED_RELATIONS {
                    record.relations.push(relation.clone());
                } else {
                    record.relations_truncated = true;
                }
            }
        }
    }
}

fn source_projection(
    scope: &SourceScope,
    source: SourceKind,
    id: &str,
    name: &str,
    kind: ConnectionKind,
    surface: SourceEditSurface,
    enabled: bool,
) -> Result<SourceProjection, ProjectionIssue> {
    let source_ref = SourceRef::new(
        source,
        id,
        scope.clone(),
        None,
        SourceLifecycle::LegacyUnresolved,
        "unversioned",
    )
    .map_err(|_| ProjectionIssue {
        source,
        reason: ProjectionReason::InvalidSourceReference,
    })?;
    let label: String = name
        .chars()
        .filter(|ch| !ch.is_control())
        .take(64)
        .collect();
    let source_label = match source {
        SourceKind::Nexus => "nexus",
        SourceKind::Mcp => "mcp",
        SourceKind::Plugin => "plugin",
        SourceKind::Device => "device",
        SourceKind::Software => "software",
    };
    Ok(SourceProjection {
        source_ref,
        kind,
        display_name: format!(
            "{} · {source_label}:{id}",
            if label.trim().is_empty() {
                source_label
            } else {
                label.trim()
            }
        ),
        source_edit_target: SourceEditTarget {
            surface,
            source_id: id.to_owned(),
        },
        readiness: SourceReadiness::new(enabled),
        capabilities: Vec::new(),
        capabilities_total: 0,
        capabilities_truncated: false,
        relations: Vec::new(),
        relations_total: 0,
        relations_truncated: false,
        reference: None,
        registration_state: None,
        device: None,
    })
}

fn source_edit_surface(source: SourceKind) -> SourceEditSurface {
    match source {
        SourceKind::Nexus => SourceEditSurface::NexusLinks,
        SourceKind::Mcp => SourceEditSurface::McpServers,
        SourceKind::Plugin => SourceEditSurface::Plugins,
        SourceKind::Device => SourceEditSurface::DeviceRegistration,
        SourceKind::Software => SourceEditSurface::SoftwareRegistration,
    }
}

fn source_label(source: SourceKind) -> &'static str {
    match source {
        SourceKind::Nexus => "nexus",
        SourceKind::Mcp => "mcp",
        SourceKind::Plugin => "plugin",
        SourceKind::Device => "device",
        SourceKind::Software => "software",
    }
}

fn registration_display_name(registration: &ConnectionRegistration) -> String {
    let label: String = registration
        .display_name()
        .chars()
        .filter(|ch| !ch.is_control())
        .take(64)
        .collect();
    let source = registration.source();
    format!(
        "{} · {}:{}",
        if label.trim().is_empty() {
            source_label(source.kind())
        } else {
            label.trim()
        },
        source_label(source.kind()),
        source.source_id()
    )
}

fn registration_readiness(registration: &ConnectionRegistration) -> SourceReadiness {
    let state = registration.state();
    let enabled = state == RegistrationState::Enabled;
    let mut readiness = SourceReadiness {
        enabled,
        availability: ProjectionAvailability::Unknown,
        handshake: SourceHandshakeState::NotApplicable,
        freshness: SourceSnapshotFreshness::NotProvided,
        reasons: Vec::new(),
    };
    if state == RegistrationState::Disabled {
        readiness.block(
            ProjectionAvailability::Disabled,
            ProjectionReason::SourceDisabled,
        );
    }
    if state == RegistrationState::LegacyUnresolved {
        readiness.reason(ProjectionReason::LegacyUnresolved);
    }
    readiness
}

fn registration_projection(registration: &ConnectionRegistration) -> SourceProjection {
    let source = registration.source();
    SourceProjection {
        source_ref: source.clone(),
        kind: registration.reference().kind(),
        display_name: registration_display_name(registration),
        source_edit_target: SourceEditTarget {
            surface: source_edit_surface(source.kind()),
            source_id: source.source_id().to_owned(),
        },
        readiness: registration_readiness(registration),
        // 登记不持有能力契约，不能根据名称或类型生成能力。
        capabilities: Vec::new(),
        capabilities_total: 0,
        capabilities_truncated: false,
        relations: Vec::new(),
        relations_total: 0,
        relations_truncated: false,
        reference: Some(registration.reference().clone()),
        registration_state: Some(registration.state()),
        device: None,
    }
}

fn project_registered_instance_records(
    snapshot: &mut SourceProjectionSnapshot,
    registry: &RegistrationRegistry,
) {
    let observed_sources: Vec<_> = snapshot
        .records
        .iter()
        .map(|record| (record.kind, record.source_ref.clone()))
        .collect();
    let mut registered = Vec::new();
    for registration in registry.registrations().iter().filter(|registration| {
        registration.state() != RegistrationState::Deleted
            && registration.reference().instance_id() == registry.instance_id()
            && registration.source().scope().project_id().is_none()
    }) {
        let found = snapshot.records.iter().position(|projection| {
            projection.kind == registration.reference().kind()
                && projection.source_ref.same_object(registration.source())
        });
        let mut projection = if let Some(index) = found {
            let mut source_projection = snapshot.records.remove(index);
            source_projection.source_ref = registration.source().clone();
            source_projection.kind = registration.reference().kind();
            source_projection.display_name = registration_display_name(registration);
            source_projection.source_edit_target = SourceEditTarget {
                surface: source_edit_surface(registration.source().kind()),
                source_id: registration.source().source_id().to_owned(),
            };
            source_projection.readiness.enabled =
                registration.state() == RegistrationState::Enabled;
            if registration.state() == RegistrationState::Disabled {
                source_projection.readiness.block(
                    ProjectionAvailability::Disabled,
                    ProjectionReason::SourceDisabled,
                );
            }
            source_projection.capabilities.clear();
            source_projection.capabilities_total = 0;
            source_projection.capabilities_truncated = false;
            source_projection
        } else {
            registration_projection(registration)
        };
        projection.reference = Some(registration.reference().clone());
        projection.registration_state = Some(registration.state());
        let source_kind_is_snapshotted = matches!(
            registration.source().kind(),
            SourceKind::Nexus | SourceKind::Mcp | SourceKind::Plugin | SourceKind::Device
        );
        if source_kind_is_snapshotted
            && !observed_sources.iter().any(|(kind, source)| {
                *kind == registration.reference().kind()
                    && source.same_object(registration.source())
            })
        {
            projection.readiness.block(
                ProjectionAvailability::Unavailable,
                ProjectionReason::SourceUnavailable,
            );
        }
        // Registry 已校验同一类型的活跃来源唯一性。不同生命周期的 legacy 槽位
        // 不是同一个对象，不能按槽位去重而丢弃已显式登记的完整引用。
        registered.push(projection);
    }
    snapshot.records.extend(registered);
}

pub(crate) fn project_nexus_links(
    scope: &SourceScope,
    links: &[LinkTarget],
) -> SourceProjectionSnapshot {
    let mut snapshot = SourceProjectionSnapshot::default();
    for link in links {
        let mut projection = match source_projection(
            scope,
            SourceKind::Nexus,
            &link.id,
            &link.name,
            ConnectionKind::ExternalAgent,
            SourceEditSurface::NexusLinks,
            link.enabled,
        ) {
            Ok(projection) => projection,
            Err(issue) => {
                snapshot.issues.push(issue);
                continue;
            }
        };
        if matches!(
            link.kind.as_str(),
            "exmachina" | "opencode" | "codex" | "claude" | "custom"
        ) {
            projection
                .readiness
                .reason(ProjectionReason::LegacyTextOnly);
            projection.capabilities.push(SourceCapabilitySummary::new(
                "legacy.text".into(),
                CapabilityRole::QuestionAnswerer,
                CapabilityListingOrigin::LegacyTextConfiguration,
            ));
        } else {
            projection.readiness.block(
                ProjectionAvailability::Unsupported,
                ProjectionReason::UnsupportedSource,
            );
        }
        snapshot.push(projection);
    }
    snapshot
}

/// 从 MCP 管理快照中立即取出最小安全清单；不保留 brief 中的端点/命令/描述。
#[derive(Debug, Clone, Default)]
pub(crate) struct McpCachedInventory {
    pub source_id: String,
    pub tool_names: Vec<String>,
}

impl McpCachedInventory {
    pub fn from_brief(brief: &serde_json::Value) -> Option<Self> {
        Some(Self {
            source_id: brief.get("id")?.as_str()?.to_owned(),
            tool_names: brief
                .get("tools")
                .and_then(|tools| tools.as_array())
                .into_iter()
                .flatten()
                .filter_map(|tool| tool.get("name")?.as_str().map(str::to_owned))
                .collect(),
        })
    }
}

pub(crate) fn project_mcp_servers(
    scope: &SourceScope,
    servers: &[McpServerConfig],
    cached: &[McpCachedInventory],
) -> SourceProjectionSnapshot {
    let mut snapshot = SourceProjectionSnapshot::default();
    let cached: HashMap<&str, &McpCachedInventory> = cached
        .iter()
        .map(|entry| (entry.source_id.as_str(), entry))
        .collect();
    for server in servers {
        let mut projection = match source_projection(
            scope,
            SourceKind::Mcp,
            &server.id,
            &server.id,
            ConnectionKind::Software,
            SourceEditSurface::McpServers,
            server.enabled,
        ) {
            Ok(projection) => projection,
            Err(issue) => {
                snapshot.issues.push(issue);
                continue;
            }
        };
        projection.readiness.handshake = SourceHandshakeState::NotObserved;
        if !matches!(server.transport.as_str(), "stdio" | "http") {
            projection.readiness.block(
                ProjectionAvailability::Unsupported,
                ProjectionReason::UnsupportedSource,
            );
        }
        if let Some(cache) = cached.get(server.id.as_str()) {
            let prefix = format!("mcp:{}:", server.id);
            for name in &cache.tool_names {
                match name
                    .strip_prefix(&prefix)
                    .filter(|name| safe_tool_name(name))
                {
                    Some(_) => add_tool(
                        &mut projection,
                        name.clone(),
                        CapabilityListingOrigin::CachedToolList,
                    ),
                    None => projection
                        .readiness
                        .reason(ProjectionReason::InvalidCapabilityName),
                }
            }
        }
        if projection.capabilities.is_empty() {
            projection
                .readiness
                .reason(ProjectionReason::HandshakeNotObserved);
        } else {
            projection.readiness.handshake = SourceHandshakeState::CachedEvidence;
            projection.readiness.cached(!server.enabled);
        }
        // McpServerConfig 没有必需凭据/鉴权结果字段。空 URL/env 或 http 不等于缺凭据。
        snapshot.push(projection);
    }
    snapshot
}

/// 仅检查来源工具明确声明的 `${credentials.key}`，语法与原工具网关一致。
/// 不解析普通 Authorization 字符串、channel 字段或 process env 来猜凭据要求。
fn plugin_template_credentials(
    entry: &PluginEntry,
    credentials: Option<&BTreeMap<String, String>>,
) -> SourceCredentialState {
    let mut referenced = false;
    for text in entry
        .manifest
        .tools
        .iter()
        .filter(|tool| tool.kind_norm() != "shell")
        .flat_map(|tool| {
            std::iter::once(tool.target.as_str()).chain(tool.headers.values().map(String::as_str))
        })
    {
        let mut rest = text;
        while let Some(index) = rest.find("${credentials.") {
            let after = &rest[index + "${credentials.".len()..];
            let Some(end) = after.find('}') else {
                break;
            };
            let key = &after[..end];
            if !key.is_empty()
                && key
                    .bytes()
                    .all(|ch| ch.is_ascii_alphanumeric() || b"_-".contains(&ch))
            {
                referenced = true;
                // 原解析器按键的存在性取值；空值不被本投影擅自重解释成鉴权失败。
                if !credentials.is_some_and(|values| values.contains_key(key)) {
                    return SourceCredentialState::Missing;
                }
            } else {
                // 与原解析器相同：非法外层变量不吞掉后面可能存在的合法变量。
                rest = after;
                continue;
            }
            rest = &after[end + 1..];
        }
    }
    if referenced {
        SourceCredentialState::Configured
    } else {
        SourceCredentialState::Unknown
    }
}

pub(crate) fn project_plugins(
    scope: &SourceScope,
    entries: &[PluginEntry],
    credentials: &BTreeMap<String, BTreeMap<String, String>>,
) -> SourceProjectionSnapshot {
    let mut snapshot = SourceProjectionSnapshot::default();
    for entry in entries {
        let id = &entry.manifest.id;
        let mut projection = match source_projection(
            scope,
            SourceKind::Plugin,
            id,
            &entry.manifest.name,
            ConnectionKind::Software,
            SourceEditSurface::Plugins,
            entry.enabled,
        ) {
            Ok(projection) => projection,
            Err(issue) => {
                snapshot.issues.push(issue);
                continue;
            }
        };
        if !matches!(
            entry.form.as_str(),
            "builtin" | "virtual" | "declarative" | "process"
        ) {
            projection.readiness.block(
                ProjectionAvailability::Unsupported,
                ProjectionReason::UnsupportedSource,
            );
        }
        if entry.form == "declarative" {
            if plugin_template_credentials(entry, credentials.get(id))
                == SourceCredentialState::Missing
            {
                projection.readiness.block(
                    ProjectionAvailability::Unavailable,
                    ProjectionReason::MissingCredentials,
                );
            }
        }
        if entry.error.is_some() {
            projection.readiness.block(
                ProjectionAvailability::Unavailable,
                ProjectionReason::SourceUnavailable,
            );
        }
        for tool in &entry.manifest.tools {
            if safe_tool_name(&tool.name) {
                add_tool(
                    &mut projection,
                    crate::plugin::namespaced(id, &tool.name),
                    CapabilityListingOrigin::LocalManifest,
                );
            } else {
                projection
                    .readiness
                    .reason(ProjectionReason::InvalidCapabilityName);
            }
        }
        for name in &entry.tools_snapshot {
            let prefix = format!("{id}__");
            let bare = name.strip_prefix(&prefix).unwrap_or(name);
            if safe_tool_name(bare) {
                add_tool(
                    &mut projection,
                    crate::plugin::namespaced(id, bare),
                    CapabilityListingOrigin::CachedToolList,
                );
            } else {
                projection
                    .readiness
                    .reason(ProjectionReason::InvalidCapabilityName);
            }
        }
        if entry.form == "process" {
            projection.readiness.handshake = if entry.tools_snapshot.is_empty() {
                projection
                    .readiness
                    .reason(ProjectionReason::HandshakeNotObserved);
                SourceHandshakeState::NotObserved
            } else {
                SourceHandshakeState::CachedEvidence
            };
        }
        if !entry.tools_snapshot.is_empty() {
            projection
                .readiness
                .cached(entry.error.is_some() || !entry.enabled);
        }
        // 通道、watcher、资源是来源声明，不推导为协作、事件订阅或自主执行协议。
        for (declared, suffix) in [
            (entry.manifest.channel.is_some(), "channel"),
            (entry.manifest.watch.is_some(), "watch"),
            (!entry.manifest.resources.is_empty(), "resources"),
        ] {
            if declared {
                projection.capabilities.push(SourceCapabilitySummary::new(
                    format!("plugin:{id}:{suffix}"),
                    CapabilityRole::Resource,
                    CapabilityListingOrigin::LocalManifest,
                ));
            }
        }
        snapshot.push(projection);
    }
    snapshot
}

fn safe_tool_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name
            .bytes()
            .all(|ch| ch.is_ascii_alphanumeric() || b"_-.".contains(&ch))
}

fn add_tool(projection: &mut SourceProjection, name: String, origin: CapabilityListingOrigin) {
    if !projection
        .capabilities
        .iter()
        .any(|capability| capability.name == name)
    {
        projection.capabilities.push(SourceCapabilitySummary::new(
            name,
            CapabilityRole::ToolProvider,
            origin,
        ));
    }
}

/// 只投影 DeviceService 已有登记和已缓存候选；不会发现、配对、查询适配器状态或 probe。
/// adapters_registered 只证明是否有登记，不能证明特定 transport 后端可用。
pub(crate) fn project_devices(
    scope: &SourceScope,
    experiment_enabled: bool,
    devices: &[DeviceSnapshot],
    candidates: &[DeviceCandidate],
    adapters_registered: bool,
) -> SourceProjectionSnapshot {
    let mut snapshot = SourceProjectionSnapshot::default();
    for device in devices {
        let record = &device.record;
        let mut projection = match source_projection(
            scope,
            SourceKind::Device,
            &record.id,
            &record.name,
            ConnectionKind::Device,
            SourceEditSurface::DeviceRegistration,
            record.enabled,
        ) {
            Ok(projection) => projection,
            Err(issue) => {
                snapshot.issues.push(issue);
                continue;
            }
        };
        let status = device.runtime.status;
        let pairing = if matches!(status, DeviceStatus::Unpaired | DeviceStatus::Discovered) {
            DevicePairingState::Unpaired
        } else {
            DevicePairingState::Registered
        };
        set_device_state(
            &mut projection,
            DeviceProjectionState {
                kind: record.kind,
                transport: record.transport,
                status,
                pairing,
                experiment_enabled,
                backend: device_backend_state(status, adapters_registered),
            },
            &record.capabilities,
        );
        // last_error 正文、requestId、endpoint 和 grants 均不进入新目录。
        if let Some(error) = &device.runtime.last_error {
            let (availability, reason) = match error.category {
                crate::device::DeviceErrorCategory::Unsupported => (
                    ProjectionAvailability::Unsupported,
                    ProjectionReason::UnsupportedSource,
                ),
                _ => (
                    ProjectionAvailability::Unavailable,
                    ProjectionReason::SourceUnavailable,
                ),
            };
            projection.readiness.block(availability, reason);
        }
        snapshot.push(projection);
    }
    for candidate in candidates {
        // 配对完成时旧候选可能仍在独立缓存；登记记录始终为唯一权威，不回退成候选。
        if devices
            .iter()
            .any(|device| device.record.id == candidate.id)
        {
            continue;
        }
        let mut projection = match source_projection(
            scope,
            SourceKind::Device,
            &candidate.id,
            &candidate.name,
            ConnectionKind::Device,
            SourceEditSurface::DevicePairing,
            true,
        ) {
            Ok(projection) => projection,
            Err(issue) => {
                snapshot.issues.push(issue);
                continue;
            }
        };
        set_device_state(
            &mut projection,
            DeviceProjectionState {
                kind: candidate.kind,
                transport: candidate.transport,
                status: DeviceStatus::Unpaired,
                pairing: DevicePairingState::Unpaired,
                experiment_enabled,
                backend: device_backend_state(DeviceStatus::Unpaired, adapters_registered),
            },
            &candidate.capabilities,
        );
        snapshot.push(projection);
    }
    snapshot
}

fn device_backend_state(status: DeviceStatus, adapters_registered: bool) -> DeviceBackendState {
    if !adapters_registered {
        return DeviceBackendState::NoAdapterRegistered;
    }
    match status {
        DeviceStatus::Unsupported => DeviceBackendState::Unsupported,
        DeviceStatus::Unavailable => DeviceBackendState::Unavailable,
        _ => DeviceBackendState::NotObserved,
    }
}

fn set_device_state(
    projection: &mut SourceProjection,
    state: DeviceProjectionState,
    capabilities: &BTreeSet<DeviceCapability>,
) {
    if !state.experiment_enabled {
        projection.readiness.block(
            ProjectionAvailability::Disabled,
            ProjectionReason::DeviceExperimentDisabled,
        );
    }
    match state.backend {
        DeviceBackendState::NoAdapterRegistered => projection.readiness.block(
            ProjectionAvailability::Unsupported,
            ProjectionReason::DeviceBackendNotRegistered,
        ),
        DeviceBackendState::Unsupported => projection.readiness.block(
            ProjectionAvailability::Unsupported,
            ProjectionReason::UnsupportedSource,
        ),
        DeviceBackendState::Unavailable => projection.readiness.block(
            ProjectionAvailability::Unavailable,
            ProjectionReason::SourceUnavailable,
        ),
        DeviceBackendState::NotObserved => projection
            .readiness
            .reason(ProjectionReason::DeviceBackendNotObserved),
    }
    if state.pairing == DevicePairingState::Unpaired {
        projection
            .readiness
            .reason(ProjectionReason::DeviceUnpaired);
    }
    match state.status {
        DeviceStatus::Disconnected | DeviceStatus::Disconnecting => projection
            .readiness
            .reason(ProjectionReason::DeviceDisconnected),
        DeviceStatus::Error => projection.readiness.block(
            ProjectionAvailability::Unavailable,
            ProjectionReason::SourceUnavailable,
        ),
        _ => {}
    }
    projection
        .readiness
        .reason(ProjectionReason::DeviceHardwareUnverified);
    for capability in capabilities {
        let (name, role) = match capability {
            DeviceCapability::AudioOutput => ("device.audio_output", CapabilityRole::Actuator),
            DeviceCapability::ImageCapture => ("device.image_capture", CapabilityRole::Sensor),
        };
        projection.capabilities.push(SourceCapabilitySummary::new(
            name.into(),
            role,
            CapabilityListingOrigin::DeviceDeclaration,
        ));
    }
    projection.device = Some(state);
}

impl crate::Core {
    /// 内部受信管理面的全量来源快照。不得将此方法直接接入 Agent 的列表/计数 API。
    /// 同步读取：不调用 provider、ready、refresh_all、adapter_probes、status 或模型。
    pub(crate) fn connection_source_snapshot(
        &self,
        context: &SourceProjectionContext,
    ) -> SourceProjectionSnapshot {
        let config = self.config();
        let mut snapshot = project_nexus_links(&context.nexus, &config.nexus.links);
        let cached: Vec<_> = self
            .mcp()
            .servers_brief()
            .iter()
            .filter_map(McpCachedInventory::from_brief)
            .collect();
        snapshot.extend(project_mcp_servers(
            &context.mcp,
            &config.mcp_servers,
            &cached,
        ));
        snapshot.extend(project_plugins(
            &context.plugin,
            &self.plugins().list(),
            &config.plugins.credentials,
        ));
        let devices = self.devices();
        snapshot.extend(project_devices(
            &context.device,
            devices.enabled(),
            &devices.snapshots(),
            &devices.candidates(),
            !devices.adapters().is_empty(),
        ));
        snapshot
    }

    /// 受信、已鉴权实例管理员的脱敏只读快照；不是 Agent 目录或主体 ACL 评估。
    /// 只合并 project_id=None 的现有登记，并读取已有关系文档，不创建目录或文件。
    /// 网关须验证非空管理密钥，拒绝客户端指定 actor/project/session/workspace 范围，
    /// 并保持此结果不进入工具/成员入口。
    pub fn admin_connection_source_snapshot(&self) -> anyhow::Result<serde_json::Value> {
        let context = SourceProjectionContext::new(self.connection_registrations.instance_id())?;
        let mut snapshot = self.connection_source_snapshot(&context);
        let data_dir = self.store.root();
        self.connection_registrations.with_registry(|registry| {
            project_registered_instance_records(&mut snapshot, registry);
            for view in
                crate::connections::relations::ConnectionRelationService::read_current_relations(
                    &data_dir, registry,
                )?
                .into_iter()
                .filter(|view| {
                    view.from().source().scope().project_id().is_none()
                        && view.to().source().scope().project_id().is_none()
                })
            {
                let relation = view.relation();
                snapshot.add_relation(AdminRelationProjection {
                    id: relation.id().to_owned(),
                    kind: relation.kind(),
                    revision: relation.revision(),
                    from: AdminRelationEndpoint {
                        reference: view.from().reference().clone(),
                        display_name: view.from().display_name().to_owned(),
                    },
                    to: AdminRelationEndpoint {
                        reference: view.to().reference().clone(),
                        display_name: view.to().display_name().to_owned(),
                    },
                });
            }
            Ok(())
        })?;
        serde_json::to_value(snapshot).map_err(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{CustomTool, ExmConfig, PluginManifest, PluginProcessDecl};
    use crate::device::{DeviceError, DeviceErrorCategory, DeviceRecord, DeviceRuntimeState};
    use serde_json::{json, Value};

    const INSTANCE: &str = "11111111-1111-4111-8111-111111111111";
    const SECRET: &str = "projection-canary-secret-581072";

    fn scope() -> SourceScope {
        SourceScope::new(INSTANCE, "fixture.source", "local_configuration", None).unwrap()
    }

    fn mcp(id: &str, enabled: bool) -> McpServerConfig {
        serde_json::from_value(json!({"id": id, "transport": "http", "enabled": enabled})).unwrap()
    }

    fn plugin(id: &str, form: &str) -> PluginEntry {
        PluginEntry {
            manifest: PluginManifest {
                id: id.into(),
                name: "同名工具来源".into(),
                ..Default::default()
            },
            form: form.into(),
            enabled: true,
            dir: None,
            error: None,
            tools_snapshot: Vec::new(),
        }
    }

    fn device(status: DeviceStatus) -> DeviceSnapshot {
        DeviceSnapshot {
            record: DeviceRecord {
                id: "speaker-1".into(),
                name: "模拟音响".into(),
                kind: DeviceKind::Speaker,
                transport: DeviceTransport::Bluetooth,
                endpoint: Some(SECRET.into()),
                capabilities: BTreeSet::from([DeviceCapability::AudioOutput]),
                grants: Vec::new(),
                enabled: true,
            },
            runtime: DeviceRuntimeState {
                status,
                ..Default::default()
            },
        }
    }

    fn assert_unmanaged(projection: &SourceProjection) {
        assert!(projection.reference.is_none());
        assert!(projection.registration_state.is_none());
        assert_eq!(
            projection.source_ref.lifecycle(),
            &SourceLifecycle::LegacyUnresolved
        );
        assert_eq!(projection.source_ref.revision(), "unversioned");
        for capability in &projection.capabilities {
            assert!(!capability.role.is_member_role());
            assert!(!capability.support.task_lifecycle);
            assert!(!capability.support.bidirectional_collaboration);
            assert_eq!(capability.verification, VerificationState::Unverified);
            assert_eq!(
                capability.authorization,
                CapabilityAuthorizationState::Unknown
            );
            assert!(capability.effects.contains(&CapabilityEffect::Unknown));
        }
    }

    #[test]
    fn nexus_is_one_authority_with_legacy_text_and_explicit_edit_target() {
        let a = LinkTarget {
            id: "a".into(),
            name: "同名".into(),
            kind: "exmachina".into(),
            endpoint: format!("http://127.0.0.1/{SECRET}"),
            api_key: SECRET.into(),
            command: SECRET.into(),
            ..Default::default()
        };
        let b = LinkTarget {
            id: "b".into(),
            name: "同名".into(),
            kind: "custom".into(),
            enabled: false,
            ..a.clone()
        };
        let links = vec![a.clone(), a, b];
        let before = serde_json::to_value(&links).unwrap();
        let snapshot = project_nexus_links(&scope(), &links);
        assert_eq!(snapshot.records.len(), 2);
        assert_eq!(serde_json::to_value(&links).unwrap(), before);
        let a = &snapshot.records[0];
        let b = &snapshot.records[1];
        assert_ne!(a.display_name, b.display_name);
        assert_ne!(a.source_key(), b.source_key());
        assert!(a
            .readiness
            .reasons
            .contains(&ProjectionReason::DuplicateSourceId));
        assert!(a
            .readiness
            .reasons
            .contains(&ProjectionReason::LegacyTextOnly));
        assert_eq!(a.source_edit_target.surface, SourceEditSurface::NexusLinks);
        assert_eq!(a.source_edit_target.source_id, "a");
        assert_eq!(b.readiness.availability, ProjectionAvailability::Disabled);
        for record in &snapshot.records {
            assert_unmanaged(record);
        }
        assert!(!serde_json::to_string(&snapshot).unwrap().contains(SECRET));
    }

    #[test]
    fn source_keys_keep_accounts_projects_and_instances_separate_without_guessing_generations() {
        let links = [LinkTarget {
            id: "same".into(),
            name: "同名".into(),
            ..Default::default()
        }];
        let mut snapshot = SourceProjectionSnapshot::default();
        for (instance, account, project) in [
            (INSTANCE, "a", Some("project-a")),
            (INSTANCE, "b", Some("project-a")),
            (INSTANCE, "a", Some("project-b")),
            (
                "22222222-2222-4222-8222-222222222222",
                "a",
                Some("project-a"),
            ),
        ] {
            let scope =
                SourceScope::new(instance, "source", account, project.map(str::to_owned)).unwrap();
            snapshot.extend(project_nexus_links(&scope, &links));
        }
        assert_eq!(snapshot.records.len(), 4);
        for record in &snapshot.records {
            assert_unmanaged(record);
        }
    }

    #[test]
    fn mcp_queries_only_declared_sources_and_cached_names_not_agent_roles_or_credentials() {
        let mut servers = vec![
            mcp("waiting", true),
            mcp("private", true),
            mcp("disabled", false),
        ];
        servers[1].allowed_agents = vec!["private-agent".into()];
        servers[1].url = Some(format!("https://{SECRET}.invalid"));
        servers[1].env.insert("TOKEN".into(), SECRET.into());
        servers[1].command = Some(SECRET.into());
        let brief = json!({"id": "private", "url": SECRET, "command": SECRET, "tools": [
            {"name": "mcp:private:query", "description": SECRET, "parameters": {"token": SECRET}},
            {"name": "mcp:private:query"}, {"name": "mcp:other:leak"},
        ]});
        let cached = vec![
            McpCachedInventory::from_brief(&brief).unwrap(),
            McpCachedInventory {
                source_id: "removed".into(),
                tool_names: vec!["mcp:removed:query".into()],
            },
        ];
        let snapshot = project_mcp_servers(&scope(), &servers, &cached);
        assert_eq!(snapshot.records.len(), 3);
        assert_eq!(
            snapshot.records[0].readiness.handshake,
            SourceHandshakeState::NotObserved
        );
        assert_eq!(
            snapshot.records[1].capabilities.len(),
            1,
            "管理面保留私有来源，不冒充 Agent ACL"
        );
        assert_eq!(
            snapshot.records[1].readiness.handshake,
            SourceHandshakeState::CachedEvidence
        );
        assert_eq!(
            snapshot.records[1].readiness.freshness,
            SourceSnapshotFreshness::Undated
        );
        assert_eq!(
            snapshot.records[2].readiness.availability,
            ProjectionAvailability::Disabled
        );
        for record in &snapshot.records {
            assert_unmanaged(record);
        }
        let output = serde_json::to_value(&snapshot).unwrap();
        assert!(!output.to_string().contains(SECRET));
        assert!(output["records"][0]["readiness"]
            .get("credentials")
            .is_none());
    }

    #[test]
    fn plugin_disabled_missing_credentials_unhandshaken_and_stale_are_distinct() {
        let mut missing = plugin("missing", "declarative");
        missing.manifest.tools.push(CustomTool {
            name: "query".into(),
            kind: "http".into(),
            target: format!("http://127.0.0.1/{SECRET}"),
            headers: BTreeMap::from([(
                "Authorization".into(),
                "Bearer ${credentials.token}".into(),
            )]),
            description: SECRET.into(),
            parameters: json!({"default": SECRET}),
            ..Default::default()
        });
        let mut anonymous = plugin("anonymous", "declarative");
        anonymous.manifest.tools.push(CustomTool {
            name: "query".into(),
            kind: "http".into(),
            target: "http://127.0.0.1".into(),
            ..Default::default()
        });
        let waiting = plugin("waiting", "process");
        let mut disabled = plugin("disabled", "process");
        disabled.enabled = false;
        let mut stale = plugin("stale", "process");
        stale.error = Some(format!("远端错误 URL/密码: {SECRET}"));
        stale.tools_snapshot = vec!["query".into()];
        stale.manifest.process = Some(PluginProcessDecl {
            command: vec![SECRET.into()],
            env: BTreeMap::from([("TOKEN".into(), SECRET.into())]),
            ..Default::default()
        });
        let entries = [missing.clone(), anonymous, waiting, disabled, stale];
        let snapshot = project_plugins(&scope(), &entries, &BTreeMap::new());
        assert!(snapshot.records[0]
            .readiness
            .reasons
            .contains(&ProjectionReason::MissingCredentials));
        assert_eq!(
            plugin_template_credentials(&entries[1], None),
            SourceCredentialState::Unknown,
            "普通 HTTP 不猜缺凭据"
        );
        assert_eq!(
            snapshot.records[2].readiness.handshake,
            SourceHandshakeState::NotObserved
        );
        assert_eq!(
            snapshot.records[3].readiness.availability,
            ProjectionAvailability::Disabled
        );
        assert_eq!(
            snapshot.records[4].readiness.freshness,
            SourceSnapshotFreshness::Stale
        );
        assert_eq!(snapshot.records[4].capabilities[0].name, "stale__query");
        assert!(snapshot.records[4]
            .readiness
            .reasons
            .contains(&ProjectionReason::SourceUnavailable));
        for record in &snapshot.records {
            assert_unmanaged(record);
        }
        assert!(!serde_json::to_string(&snapshot).unwrap().contains(SECRET));
        let credentials = BTreeMap::from([(
            "missing".into(),
            BTreeMap::from([("token".into(), SECRET.into())]),
        )]);
        let configured = project_plugins(&scope(), &[missing.clone()], &credentials);
        assert_eq!(
            plugin_template_credentials(&missing, credentials.get("missing")),
            SourceCredentialState::Configured
        );
        missing.manifest.version = "999.1.3".into();
        let updated = project_plugins(&scope(), &[missing], &credentials);
        assert_eq!(
            configured.records[0].source_ref, updated.records[0].source_ref,
            "包版本不冒充来源生命周期/revision"
        );
    }

    #[test]
    fn invalid_source_and_remote_errors_never_echo_raw_values() {
        let links = [LinkTarget {
            id: format!("https://invalid/?token={SECRET}"),
            ..Default::default()
        }];
        let snapshot = project_nexus_links(&scope(), &links);
        assert!(snapshot.records.is_empty());
        assert_eq!(
            snapshot.issues,
            vec![ProjectionIssue {
                source: SourceKind::Nexus,
                reason: ProjectionReason::InvalidSourceReference
            }]
        );
        assert!(!format!("{snapshot:?}").contains(SECRET));
    }

    #[test]
    fn credential_state_follows_explicit_source_templates_not_transport_or_channel_fields() {
        let mut entry = plugin("templates", "declarative");
        entry.manifest.tools.push(CustomTool {
            name: "shell".into(),
            kind: "shell".into(),
            target: "${credentials.not_a_shell_credential}".into(),
            ..Default::default()
        });
        assert_eq!(
            plugin_template_credentials(&entry, None),
            SourceCredentialState::Unknown
        );
        entry.manifest.tools.push(CustomTool {
            name: "query".into(),
            target: "http://127.0.0.1/${credentials.bad ${credentials.actual}".into(),
            ..Default::default()
        });
        assert_eq!(
            plugin_template_credentials(&entry, None),
            SourceCredentialState::Missing
        );
        assert_eq!(
            plugin_template_credentials(
                &entry,
                Some(&BTreeMap::from([("actual".into(), String::new())]))
            ),
            SourceCredentialState::Configured,
            "与来源解析器一致：键存在不等于远端已接受，也不猜测空值失效"
        );
    }

    #[test]
    fn device_projection_keeps_feature_backend_pairing_and_hardware_evidence_separate() {
        let disabled = project_devices(
            &scope(),
            false,
            &[device(DeviceStatus::Connected)],
            &[],
            true,
        );
        assert_eq!(
            disabled.records[0].readiness.availability,
            ProjectionAvailability::Disabled
        );
        assert!(disabled.records[0]
            .readiness
            .reasons
            .contains(&ProjectionReason::DeviceExperimentDisabled));
        let no_backend = project_devices(
            &scope(),
            true,
            &[device(DeviceStatus::Disconnected)],
            &[],
            false,
        );
        assert_eq!(
            no_backend.records[0].device.as_ref().unwrap().backend,
            DeviceBackendState::NoAdapterRegistered
        );
        assert_eq!(
            no_backend.records[0].readiness.availability,
            ProjectionAvailability::Unsupported
        );
        for (status, availability) in [
            (
                DeviceStatus::Unsupported,
                ProjectionAvailability::Unsupported,
            ),
            (
                DeviceStatus::Unavailable,
                ProjectionAvailability::Unavailable,
            ),
            (DeviceStatus::Error, ProjectionAvailability::Unavailable),
            (DeviceStatus::Connected, ProjectionAvailability::Unknown),
        ] {
            let mut input = device(status);
            if status == DeviceStatus::Error {
                input.runtime.last_error =
                    Some(DeviceError::new(DeviceErrorCategory::Unavailable, SECRET));
            }
            let snapshot = project_devices(&scope(), true, &[input], &[], true);
            let record = &snapshot.records[0];
            assert_eq!(record.kind, ConnectionKind::Device);
            assert_eq!(record.readiness.availability, availability);
            assert_unmanaged(record);
            assert!(record
                .readiness
                .reasons
                .contains(&ProjectionReason::DeviceHardwareUnverified));
            assert!(!serde_json::to_string(&snapshot).unwrap().contains(SECRET));
        }
        let registered = device(DeviceStatus::Disconnected);
        let candidates = [
            DeviceCandidate {
                id: registered.record.id.clone(),
                name: "旧候选".into(),
                transport: registered.record.transport,
                kind: registered.record.kind,
                endpoint: None,
                capabilities: registered.record.capabilities.clone(),
            },
            DeviceCandidate {
                id: "unpaired".into(),
                name: "未配对".into(),
                transport: DeviceTransport::Bluetooth,
                kind: DeviceKind::Speaker,
                endpoint: Some(SECRET.into()),
                capabilities: BTreeSet::from([DeviceCapability::AudioOutput]),
            },
        ];
        let snapshot = project_devices(&scope(), true, &[registered], &candidates, true);
        assert_eq!(snapshot.records.len(), 2);
        assert_eq!(
            snapshot.records[0].source_edit_target.surface,
            SourceEditSurface::DeviceRegistration
        );
        assert_eq!(
            snapshot.records[1].source_edit_target.surface,
            SourceEditSurface::DevicePairing
        );
        assert_eq!(
            snapshot.records[1].device.as_ref().unwrap().pairing,
            DevicePairingState::Unpaired
        );
        assert!(snapshot.records[1]
            .readiness
            .reasons
            .contains(&ProjectionReason::DeviceUnpaired));
        for record in &snapshot.records {
            assert_unmanaged(record);
        }
    }

    struct IsolatedWorkspace(std::path::PathBuf);

    /// 独立宿主计数器：来源输入不能将其清零；即使 probe 也不允许在目录读取时调用。
    struct CountedDeviceAdapter(std::sync::Arc<std::sync::atomic::AtomicUsize>);

    impl CountedDeviceAdapter {
        fn unexpected(&self) -> DeviceError {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            DeviceError::new(DeviceErrorCategory::Unavailable, "投影不应调用设备后端")
        }
    }

    #[async_trait::async_trait]
    impl crate::device::DeviceAdapter for CountedDeviceAdapter {
        fn id(&self) -> &str {
            "projection-counted-adapter"
        }

        fn transports(&self) -> BTreeSet<DeviceTransport> {
            BTreeSet::from([DeviceTransport::Bluetooth])
        }

        fn capabilities(&self) -> BTreeSet<DeviceCapability> {
            BTreeSet::from([DeviceCapability::AudioOutput])
        }

        fn probe(&self) -> crate::device::DeviceAdapterProbe {
            let _ = self.unexpected();
            crate::device::DeviceAdapterProbe {
                id: self.id().into(),
                api_version: crate::device::DEVICE_ADAPTER_API_VERSION,
                available: false,
                transports: self.transports(),
                capabilities: self.capabilities(),
                status: DeviceStatus::Unavailable,
            }
        }

        async fn discover(
            &self,
            _: &crate::device::DiscoveryRequest,
        ) -> Result<Vec<DeviceCandidate>, DeviceError> {
            Err(self.unexpected())
        }

        async fn pair(&self, _: &DeviceCandidate) -> Result<DeviceRecord, DeviceError> {
            Err(self.unexpected())
        }

        async fn unpair(&self, _: &DeviceRecord) -> Result<(), DeviceError> {
            Err(self.unexpected())
        }

        async fn connect(&self, _: &DeviceRecord) -> Result<DeviceStatus, DeviceError> {
            Err(self.unexpected())
        }

        async fn disconnect(&self, _: &DeviceRecord) -> Result<DeviceStatus, DeviceError> {
            Err(self.unexpected())
        }

        async fn status(&self, _: &DeviceRecord) -> Result<DeviceStatus, DeviceError> {
            Err(self.unexpected())
        }

        async fn audio_play(
            &self,
            _: &crate::device::DeviceRequestContext,
            _: &DeviceRecord,
            _: &[u8],
        ) -> Result<crate::device::DeviceActionReceipt, DeviceError> {
            Err(self.unexpected())
        }

        async fn camera_capture(
            &self,
            _: &crate::device::DeviceRequestContext,
            _: &DeviceRecord,
        ) -> Result<crate::device::DeviceCapture, DeviceError> {
            Err(self.unexpected())
        }

        async fn cancel(&self, _: &str) -> Result<(), DeviceError> {
            Err(self.unexpected())
        }
    }

    impl IsolatedWorkspace {
        fn new() -> Self {
            let parent = if std::path::Path::new("/tmp/opencode").is_dir() {
                std::path::PathBuf::from("/tmp/opencode")
            } else {
                std::env::temp_dir()
            };
            let path = parent.join(format!("exm-source-projection-{}", uuid::Uuid::new_v4()));
            let mut builder = std::fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            builder.create(&path).expect("必须创建独占的仓库外测试目录");
            let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../..")
                .canonicalize()
                .unwrap();
            assert!(!path.canonicalize().unwrap().starts_with(repo));
            Self(path)
        }
    }

    impl Drop for IsolatedWorkspace {
        fn drop(&mut self) {
            if let Err(error) = std::fs::remove_dir_all(&self.0) {
                if !std::thread::panicking() {
                    panic!("投影夹具回收失败：{error}");
                }
            }
        }
    }

    #[test]
    fn real_core_snapshot_does_not_accept_loopback_connect_reload_plugins_or_modify_sources() {
        let workspace = IsolatedWorkspace::new();
        let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let mut cfg = ExmConfig::load(&workspace.0);
        cfg.data_dir = workspace.0.join("data");
        cfg.agents_dir = workspace.0.join("entities");
        cfg.use_mock = true;
        cfg.llm.api_key.clear();
        cfg.llm.api_keys.clear();
        cfg.llm_profiles.clear();
        cfg.memory_enabled = false;
        cfg.experiments.device_connectivity.enabled = false;
        cfg.nexus.links = vec![LinkTarget {
            id: "remote".into(),
            name: "旧连结".into(),
            endpoint: endpoint.clone(),
            api_key: SECRET.into(),
            ..Default::default()
        }];
        cfg.mcp_servers = vec![
            McpServerConfig {
                url: Some(endpoint.clone()),
                ..mcp("remote", true)
            },
            McpServerConfig {
                command: Some("must-not-start-source-projection-process".into()),
                transport: "stdio".into(),
                ..mcp("stdio", true)
            },
            mcp("disabled", false),
        ];
        let plugin_dir = workspace.0.join("plugins/local");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(plugin_dir.join("plugin.json"), json!({"id": "local", "name": "本地清单", "tools": [
            {"name": "query", "kind": "http", "target": endpoint, "headers": {"Authorization": "Bearer ${credentials.token}"}}
        ]}).to_string()).unwrap();
        let mut source_device = device(DeviceStatus::Disconnected).record;
        source_device.endpoint = None;
        cfg.nexus.devices = vec![source_device];
        let core = crate::Core::with_config(cfg).unwrap();
        let device_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        core.devices()
            .register_adapter(std::sync::Arc::new(CountedDeviceAdapter(
                device_calls.clone(),
            )))
            .unwrap();
        let context = SourceProjectionContext::new(INSTANCE).unwrap();
        let config_before = core.config();
        let target_before = (
            core.registry.active_group(),
            core.registry.active_single(),
            core.registry.active_workspace(),
        );
        let agents_before = serde_json::to_value(core.registry.list()).unwrap();
        let devices_before = core.devices().snapshots();
        let audits_before = core.devices().audits();
        let mcp_before = core.mcp().servers_brief();
        // 不做清单重扫：磁盘变化只能在原来源服务显式装载后出现。
        std::fs::write(plugin_dir.join("plugin.json"), "{invalid changed file}").unwrap();
        let first = core.connection_source_snapshot(&context);
        let second = core.connection_source_snapshot(&context);
        let admin_files_before = tree_state(&workspace.0);
        let admin_registry_before = core.connection_registrations.snapshot();
        let admin_first = core.admin_connection_source_snapshot().unwrap();
        let admin_second = core.admin_connection_source_snapshot().unwrap();
        assert_eq!(admin_first, admin_second);
        assert_eq!(tree_state(&workspace.0), admin_files_before);
        assert_eq!(
            core.connection_registrations.snapshot(),
            admin_registry_before
        );
        assert!(!admin_first.to_string().contains(SECRET));
        assert!(admin_first["records"]
            .as_array()
            .unwrap()
            .iter()
            .all(|record| record["readiness"].get("credentials").is_none()));
        assert_eq!(first, second);
        assert!(first.issues.is_empty());
        let by_id = |source: SourceKind, id: &str| {
            first
                .records
                .iter()
                .find(|record| {
                    record.source_ref.kind() == source && record.source_ref.source_id() == id
                })
                .unwrap()
        };
        assert!(by_id(SourceKind::Plugin, "local")
            .readiness
            .reasons
            .contains(&ProjectionReason::MissingCredentials));
        assert_eq!(
            by_id(SourceKind::Mcp, "remote").readiness.handshake,
            SourceHandshakeState::NotObserved
        );
        assert_eq!(
            by_id(SourceKind::Mcp, "disabled").readiness.availability,
            ProjectionAvailability::Disabled
        );
        assert_eq!(
            by_id(SourceKind::Device, "speaker-1")
                .readiness
                .availability,
            ProjectionAvailability::Disabled
        );
        assert!(std::sync::Arc::ptr_eq(&core.config(), &config_before));
        assert_eq!(
            (
                core.registry.active_group(),
                core.registry.active_single(),
                core.registry.active_workspace(),
            ),
            target_before,
            "查看目录不能改全局激活目标"
        );
        assert_eq!(
            serde_json::to_value(core.registry.list()).unwrap(),
            agents_before
        );
        assert_eq!(core.devices().snapshots(), devices_before);
        assert_eq!(core.devices().audits(), audits_before);
        assert_eq!(core.mcp().servers_brief(), mcp_before);
        assert_eq!(
            device_calls.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "快照不能 probe 或调用设备适配器"
        );
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock,
            "查询不应发起任何 loopback 连接"
        );
        let output: Value = serde_json::to_value(&first).unwrap();
        assert!(!output.to_string().contains(SECRET));
        assert!(output["records"]
            .as_array()
            .unwrap()
            .iter()
            .all(|record| record["readiness"].get("credentials").is_none()));
        for record in &first.records {
            assert_unmanaged(record);
        }
    }

    fn tree_state(root: &std::path::Path) -> BTreeMap<std::path::PathBuf, Option<Vec<u8>>> {
        fn visit(
            root: &std::path::Path,
            current: &std::path::Path,
            output: &mut BTreeMap<std::path::PathBuf, Option<Vec<u8>>>,
        ) {
            for entry in std::fs::read_dir(current).unwrap() {
                let entry = entry.unwrap();
                let path = entry.path();
                let relative = path.strip_prefix(root).unwrap().to_path_buf();
                if entry.file_type().unwrap().is_dir() {
                    output.insert(relative, None);
                    visit(root, &path, output);
                } else {
                    output.insert(relative, Some(std::fs::read(path).unwrap()));
                }
            }
        }

        let mut output = BTreeMap::new();
        visit(root, root, &mut output);
        output
    }

    #[test]
    fn admin_snapshot_merges_instance_refs_hides_project_rows_and_reads_relations_without_writes() {
        use crate::connections::identity::{
            ConnectionRelation, ConnectionRelationKind, SourceLifecycle, SourceRef, SourceScope,
        };
        use crate::connections::relations::ConnectionRelationService;

        let workspace = IsolatedWorkspace::new();
        let mut cfg = ExmConfig::load(&workspace.0);
        cfg.data_dir = workspace.0.join("data");
        cfg.agents_dir = workspace.0.join("entities");
        cfg.use_mock = true;
        cfg.llm.api_key.clear();
        cfg.llm.api_keys.clear();
        cfg.llm_profiles.clear();
        cfg.memory_enabled = false;
        cfg.nexus.links = vec![
            LinkTarget {
                id: "legacy-link".into(),
                name: "Legacy source".into(),
                kind: "exmachina".into(),
                ..Default::default()
            },
            LinkTarget {
                id: "unregistered-link".into(),
                name: "Legacy source".into(),
                kind: "exmachina".into(),
                endpoint: format!("https://example.invalid/{SECRET}"),
                api_key: SECRET.into(),
                ..Default::default()
            },
        ];
        let data_dir = cfg.data_dir.clone();
        let core = crate::Core::with_config(cfg).unwrap();
        let instance = core.connection_registrations.instance_id().to_owned();
        let source =
            |kind, id: &str, account: &str, project: Option<&str>, subresource: Option<&str>| {
                SourceRef::new(
                    kind,
                    id,
                    SourceScope::new(
                        instance.as_str(),
                        "local.software-owner",
                        account,
                        project.map(str::to_owned),
                    )
                    .unwrap(),
                    subresource.map(str::to_owned),
                    SourceLifecycle::known("fixture-generation-1").unwrap(),
                    "fixture-revision-1",
                )
                .unwrap()
            };
        let registrations = &core.connection_registrations;
        let software_a = registrations
            .register(
                ConnectionKind::Software,
                source(SourceKind::Software, "same-id", "account-a", None, None),
                "同名连接",
            )
            .unwrap();
        let software_b = registrations
            .register(
                ConnectionKind::Software,
                source(SourceKind::Software, "same-id", "account-b", None, None),
                "同名连接",
            )
            .unwrap();
        let ai_a = registrations
            .register(
                ConnectionKind::ExternalAgent,
                source(
                    SourceKind::Software,
                    "same-id",
                    "account-a",
                    None,
                    Some("assistant"),
                ),
                "同名连接",
            )
            .unwrap();
        let project_software = registrations
            .register(
                ConnectionKind::Software,
                source(
                    SourceKind::Software,
                    "project-only",
                    "account-a",
                    Some("project-secret"),
                    None,
                ),
                "项目连接",
            )
            .unwrap();
        let project_ai = registrations
            .register(
                ConnectionKind::ExternalAgent,
                source(
                    SourceKind::Software,
                    "project-only",
                    "account-a",
                    Some("project-secret"),
                    Some("assistant"),
                ),
                "项目连接",
            )
            .unwrap();
        let legacy_context = SourceProjectionContext::new(&instance).unwrap();
        let legacy_source = SourceRef::new(
            SourceKind::Nexus,
            "legacy-link",
            legacy_context.nexus.clone(),
            None,
            SourceLifecycle::LegacyUnresolved,
            "unversioned",
        )
        .unwrap();
        let legacy_reference = registrations
            .register(
                ConnectionKind::ExternalAgent,
                legacy_source,
                "Registered legacy link",
            )
            .unwrap();
        let missing_source = SourceRef::new(
            SourceKind::Mcp,
            "source-removed",
            SourceScope::new(
                instance.as_str(),
                "core.mcp.servers",
                "local_configuration",
                None,
            )
            .unwrap(),
            None,
            SourceLifecycle::known("fixture-generation-1").unwrap(),
            "fixture-revision-1",
        )
        .unwrap();
        registrations
            .register(ConnectionKind::Software, missing_source, "Removed source")
            .unwrap();

        let files_before_missing_relations = tree_state(&workspace.0);
        let registry_before_missing_relations = core.connection_registrations.snapshot();
        let no_relations_snapshot = core.admin_connection_source_snapshot().unwrap();
        assert_eq!(
            files_before_missing_relations,
            tree_state(&workspace.0),
            "GET with a missing relation document must not create it or its directory"
        );
        assert_eq!(
            registry_before_missing_relations,
            core.connection_registrations.snapshot()
        );
        assert!(no_relations_snapshot.get("relations").is_none());
        assert!(no_relations_snapshot["records"]
            .as_array()
            .unwrap()
            .iter()
            .all(|record| record["relations"].as_array().unwrap().is_empty()
                && record["relationsTotal"] == 0
                && record["relationsTruncated"] == false));

        let relations = ConnectionRelationService::open(&data_dir).unwrap();
        registrations
            .with_registry(|registry| {
                for (kind, from, to) in [
                    (
                        ConnectionRelationKind::EmbeddedIn,
                        ai_a.clone(),
                        software_a.clone(),
                    ),
                    (
                        ConnectionRelationKind::EmbeddedIn,
                        project_ai.clone(),
                        project_software.clone(),
                    ),
                    (
                        ConnectionRelationKind::ProvidedBy,
                        project_ai,
                        software_a.clone(),
                    ),
                    (
                        ConnectionRelationKind::ProvidedBy,
                        ai_a.clone(),
                        project_software,
                    ),
                ] {
                    relations.upsert(registry, ConnectionRelation::new(kind, from, to)?, 0)?;
                }
                Ok(())
            })
            .unwrap();

        let registry_before = core.connection_registrations.snapshot();
        let files_before = tree_state(&workspace.0);
        let output = core.admin_connection_source_snapshot().unwrap();
        let after_first = tree_state(&workspace.0);
        let registry_after = core.connection_registrations.snapshot();
        let second_output = core.admin_connection_source_snapshot().unwrap();
        assert_eq!(output, second_output);
        assert_eq!(
            files_before, after_first,
            "GET must not create or alter files"
        );
        assert_eq!(
            registry_before, registry_after,
            "GET must not modify registrations"
        );
        assert_eq!(files_before, tree_state(&workspace.0));

        let records = output["records"].as_array().unwrap();
        let same_id: Vec<_> = records
            .iter()
            .filter(|record| record.pointer("/sourceRef/sourceId") == Some(&json!("same-id")))
            .collect();
        assert_eq!(
            same_id.len(),
            3,
            "same display/source ID remains split by account, kind and ref"
        );
        assert!(same_id.iter().any(|record| {
            record["reference"]["kind"] == "software"
                && record["reference"]["connectionId"].as_str() == Some(software_a.connection_id())
        }));
        assert!(same_id.iter().any(|record| {
            record["reference"]["kind"] == "software"
                && record["reference"]["connectionId"].as_str() == Some(software_b.connection_id())
        }));
        assert!(same_id.iter().any(|record| {
            record["reference"]["kind"] == "external_agent"
                && record["reference"]["connectionId"].as_str() == Some(ai_a.connection_id())
        }));
        assert!(same_id.iter().all(|record| {
            record["capabilities"] == json!([])
                && record["capabilitiesTotal"] == 0
                && record["capabilitiesTruncated"] == false
                && record["registrationState"] == "disabled"
        }));
        assert!(!records.iter().any(|record| {
            record.pointer("/sourceRef/sourceId") == Some(&json!("project-only"))
        }));
        assert!(!output.to_string().contains("project-secret"));
        let legacy: Vec<_> = records
            .iter()
            .filter(|record| record.pointer("/sourceRef/sourceId") == Some(&json!("legacy-link")))
            .collect();
        assert_eq!(
            legacy.len(),
            1,
            "matching source and explicit ref have one record"
        );
        assert_eq!(
            legacy[0]["reference"],
            serde_json::to_value(&legacy_reference).unwrap()
        );
        assert!(legacy[0]["readiness"]["reasons"]
            .as_array()
            .unwrap()
            .contains(&json!("legacy_text_only")));
        assert!(output.get("relations").is_none());
        let ai_record = same_id
            .iter()
            .find(|record| {
                record["reference"]["connectionId"].as_str() == Some(ai_a.connection_id())
            })
            .unwrap();
        let software_record = same_id
            .iter()
            .find(|record| {
                record["reference"]["connectionId"].as_str() == Some(software_a.connection_id())
            })
            .unwrap();
        for record in [ai_record, software_record] {
            assert_eq!(record["relationsTotal"], 1);
            assert_eq!(record["relationsTruncated"], false);
            let relations = record["relations"].as_array().unwrap();
            assert_eq!(relations.len(), 1);
            assert_eq!(relations[0]["kind"], "embedded_in");
            assert_eq!(relations[0]["revision"], 1);
            assert_eq!(
                relations[0]["from"]["reference"],
                serde_json::to_value(&ai_a).unwrap()
            );
            assert_eq!(
                relations[0]["to"]["reference"],
                serde_json::to_value(&software_a).unwrap()
            );
        }
        let account_b_record = same_id
            .iter()
            .find(|record| {
                record["reference"]["connectionId"].as_str() == Some(software_b.connection_id())
            })
            .unwrap();
        assert_eq!(account_b_record["relationsTotal"], 0);
        assert_eq!(account_b_record["relations"], json!([]));
        let unregistered = records
            .iter()
            .find(|record| {
                record.pointer("/sourceRef/sourceId") == Some(&json!("unregistered-link"))
            })
            .unwrap();
        assert!(unregistered.get("reference").is_none());
        assert!(unregistered.get("registrationState").is_none());
        assert_eq!(
            unregistered.pointer("/sourceRef/lifecycle/state").unwrap(),
            "legacy_unresolved"
        );
        let removed = records
            .iter()
            .find(|record| record.pointer("/sourceRef/sourceId") == Some(&json!("source-removed")))
            .unwrap();
        assert!(removed["readiness"]["reasons"]
            .as_array()
            .unwrap()
            .contains(&json!("source_unavailable")));
        assert!(records
            .iter()
            .all(|record| record["readiness"].get("credentials").is_none()));
        assert!(!output.to_string().contains(SECRET));

        // 来源 owner 的当前本地快照变化必须立即可见；读取不重登、启用或修改来源文件。
        let mut changed_config = (*core.config()).clone();
        changed_config.nexus.links.clear();
        *core.config.write() = std::sync::Arc::new(changed_config);
        let files_before_removed_source = tree_state(&workspace.0);
        let registry_before_removed_source = registrations.snapshot();
        let changed = core.admin_connection_source_snapshot().unwrap();
        let missing_legacy = changed["records"]
            .as_array()
            .unwrap()
            .iter()
            .find(|record| record["reference"] == serde_json::to_value(&legacy_reference).unwrap())
            .unwrap();
        assert_eq!(missing_legacy["readiness"]["availability"], "unavailable");
        assert!(missing_legacy["readiness"]["reasons"]
            .as_array()
            .unwrap()
            .contains(&json!("source_unavailable")));
        assert_eq!(missing_legacy["capabilities"], json!([]));
        assert_eq!(tree_state(&workspace.0), files_before_removed_source);
        assert_eq!(registrations.snapshot(), registry_before_removed_source);

        // 当前 registry 删除端点后，旧关系不再出现，且 GET 不清理/改写关系文档。
        registrations.delete(&ai_a).unwrap();
        let files_before_deleted_endpoint = tree_state(&workspace.0);
        let after_delete = core.admin_connection_source_snapshot().unwrap();
        let software = after_delete["records"]
            .as_array()
            .unwrap()
            .iter()
            .find(|record| record["reference"] == serde_json::to_value(&software_a).unwrap())
            .unwrap();
        assert_eq!(software["relations"], json!([]));
        assert_eq!(software["relationsTotal"], 0);
        assert_eq!(tree_state(&workspace.0), files_before_deleted_endpoint);

        // 文件存在但已损坏时，受信管理读取也返回错误，不能将其伪装为空关系集。
        let relation_path = data_dir.join("connection_relations/state.json");
        std::fs::write(&relation_path, b"{invalid relation store}").unwrap();
        let files_before_bad_store = tree_state(&workspace.0);
        let registry_before_bad_store = registrations.snapshot();
        assert!(core.admin_connection_source_snapshot().is_err());
        assert_eq!(tree_state(&workspace.0), files_before_bad_store);
        assert_eq!(registrations.snapshot(), registry_before_bad_store);
    }

    #[test]
    fn projected_capabilities_are_bounded_with_true_total() {
        let mut entry = plugin("many-tools", "declarative");
        for index in 0..=MAX_PROJECTED_CAPABILITIES {
            entry.manifest.tools.push(CustomTool {
                name: format!("tool-{index}"),
                ..Default::default()
            });
        }
        let snapshot = project_plugins(&scope(), &[entry], &BTreeMap::new());
        let record = &snapshot.records[0];
        assert_eq!(record.capabilities.len(), MAX_PROJECTED_CAPABILITIES);
        assert_eq!(record.capabilities_total, MAX_PROJECTED_CAPABILITIES + 1);
        assert!(record.capabilities_truncated);
        assert!(record
            .capabilities
            .iter()
            .all(|capability| capability.verification == VerificationState::Unverified));
    }

    #[test]
    fn relations_are_attached_to_both_visible_endpoints_with_per_record_bounds() {
        let ai_reference = ConnectionRef::new(INSTANCE, ConnectionKind::ExternalAgent).unwrap();
        let software_reference = ConnectionRef::new(INSTANCE, ConnectionKind::Software).unwrap();
        let mut snapshot = SourceProjectionSnapshot::default();
        for (id, kind, reference) in [
            ("ai", ConnectionKind::ExternalAgent, ai_reference.clone()),
            (
                "software",
                ConnectionKind::Software,
                software_reference.clone(),
            ),
        ] {
            let mut record = source_projection(
                &scope(),
                SourceKind::Software,
                id,
                "同名",
                kind,
                SourceEditSurface::SoftwareRegistration,
                true,
            )
            .unwrap();
            record.reference = Some(reference);
            snapshot.push(record);
        }
        for index in 0..=MAX_PROJECTED_RELATIONS {
            snapshot.add_relation(AdminRelationProjection {
                id: format!("relation-{index}"),
                kind: ConnectionRelationKind::EmbeddedIn,
                revision: 1,
                from: AdminRelationEndpoint {
                    reference: ai_reference.clone(),
                    display_name: "同名 AI".into(),
                },
                to: AdminRelationEndpoint {
                    reference: software_reference.clone(),
                    display_name: "同名软件".into(),
                },
            });
        }
        for record in snapshot.records {
            assert_eq!(record.relations.len(), MAX_PROJECTED_RELATIONS);
            assert_eq!(record.relations_total, MAX_PROJECTED_RELATIONS + 1);
            assert!(record.relations_truncated);
        }
    }
}
