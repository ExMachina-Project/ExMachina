//! 连接身份与登记代际；这里只持有引用，不持有来源配置、端点或凭据。
//!
//! 显示名不参与身份。来源身份包含安装实例、来源服务域、账户、项目、子资源及
//! 生命周期；来源 revision 只表示快照版本。生命周期未知的旧记录只能保持
//! `legacy_unresolved`，不能通过启用、反序列化或关系登记获得新路径授权。
//!
//! `RegistrationBinding` 是后续 grant/审批必须绑定的身份快照，**不是 grant**。
//! 使用它时仍须调用当前 registry 的 `validate_binding`，再求交主体、来源、能力、
//! 数据及执行授权。删除/重登记更换随机代际，禁用/重新启用更换登记 revision。
//! 来源账户、端点绑定或生命周期变化必须先删除旧登记，再显式重登记；不能原位改绑。
//!
//! 所有持久类型的解码都会执行与构造相同的结构校验。持久快照只能从受信存储恢复，
//! 不能把客户端 JSON 当作登记权限；恢复还须提供当前安装实例，跨实例快照会被拒绝。

use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

pub const CONNECTION_IDENTITY_SCHEMA_VERSION: u16 = 1;
const MAX_ID_BYTES: usize = 512;
const MAX_DISPLAY_NAME_BYTES: usize = 512;

/// 类型不表示协议能力、成员资格或执行权限。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionKind {
    Software,
    ExternalAgent,
    Device,
}

/// 仍由原服务维护的权威来源；目录不复制它的可写配置。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    Nexus,
    Mcp,
    Plugin,
    Device,
    Software,
}

/// 来源域。`None` 项目是安装级域，不是匹配任意项目的通配符。
///
/// `authority_id` 是来源服务/远端安装的稳定标识，不是其当前 URL；`account_id`
/// 是来源服务确定的账户标识，不是显示名。无远端账户的来源也须提供本地服务的
/// 稳定账户域，不能用空串把未知账户合并。调用方只能传入非秘密引用；标识校验
/// 拒绝 URL/凭据参数，但不能代替来源凭据服务或通用秘密扫描。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", try_from = "SourceScopeData")]
pub struct SourceScope {
    instance_id: String,
    authority_id: String,
    account_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    project_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SourceScopeData {
    instance_id: String,
    authority_id: String,
    account_id: String,
    project_id: Option<String>,
}

impl TryFrom<SourceScopeData> for SourceScope {
    type Error = anyhow::Error;

    fn try_from(value: SourceScopeData) -> Result<Self> {
        Self::new(
            value.instance_id,
            value.authority_id,
            value.account_id,
            value.project_id,
        )
    }
}

impl SourceScope {
    pub fn new(
        instance_id: impl Into<String>,
        authority_id: impl Into<String>,
        account_id: impl Into<String>,
        project_id: Option<String>,
    ) -> Result<Self> {
        let scope = Self {
            instance_id: instance_id.into(),
            authority_id: authority_id.into(),
            account_id: account_id.into(),
            project_id,
        };
        scope.validate()?;
        Ok(scope)
    }

    fn validate(&self) -> Result<()> {
        validate_uuid(&self.instance_id, "instanceId", false)?;
        validate_id(&self.authority_id, "authorityId")?;
        validate_id(&self.account_id, "accountId")?;
        if let Some(project) = &self.project_id {
            validate_id(project, "projectId")?;
        }
        Ok(())
    }

    pub fn instance_id(&self) -> &str {
        &self.instance_id
    }

    pub fn authority_id(&self) -> &str {
        &self.authority_id
    }

    pub fn account_id(&self) -> &str {
        &self.account_id
    }

    pub fn project_id(&self) -> Option<&str> {
        self.project_id.as_deref()
    }
}

/// 来源服务提供的对象生命周期，不得用显示名或当前 URL 推断。
///
/// 显式登记旧来源时，原服务必须先建立可跟踪的代际；仅把 unresolved 改成
/// known 的远端声明不是生命周期证据。本类型不负责验证来源服务的可信身份。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(
    tag = "state",
    rename_all = "snake_case",
    try_from = "SourceLifecycleData"
)]
pub enum SourceLifecycle {
    Known { generation: String },
    LegacyUnresolved,
}

#[derive(Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
enum SourceLifecycleData {
    Known { generation: String },
    LegacyUnresolved {},
}

impl TryFrom<SourceLifecycleData> for SourceLifecycle {
    type Error = anyhow::Error;

    fn try_from(value: SourceLifecycleData) -> Result<Self> {
        match value {
            SourceLifecycleData::Known { generation } => Self::known(generation),
            SourceLifecycleData::LegacyUnresolved {} => Ok(Self::LegacyUnresolved),
        }
    }
}

impl SourceLifecycle {
    pub fn known(generation: impl Into<String>) -> Result<Self> {
        let lifecycle = Self::Known {
            generation: generation.into(),
        };
        lifecycle.validate()?;
        Ok(lifecycle)
    }

    fn validate(&self) -> Result<()> {
        if let Self::Known { generation } = self {
            validate_id(generation, "source lifecycle generation")?;
        }
        Ok(())
    }

    pub fn is_known(&self) -> bool {
        matches!(self, Self::Known { .. }) && self.validate().is_ok()
    }
}

/// 原来源的一次安全快照；不保存适配器私有配置或可重放材料。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", try_from = "SourceRefData")]
pub struct SourceRef {
    kind: SourceKind,
    source_id: String,
    scope: SourceScope,
    #[serde(skip_serializing_if = "Option::is_none")]
    subresource_id: Option<String>,
    lifecycle: SourceLifecycle,
    revision: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SourceRefData {
    kind: SourceKind,
    source_id: String,
    scope: SourceScope,
    subresource_id: Option<String>,
    lifecycle: SourceLifecycle,
    revision: String,
}

impl TryFrom<SourceRefData> for SourceRef {
    type Error = anyhow::Error;

    fn try_from(value: SourceRefData) -> Result<Self> {
        Self::new(
            value.kind,
            value.source_id,
            value.scope,
            value.subresource_id,
            value.lifecycle,
            value.revision,
        )
    }
}

impl SourceRef {
    pub fn new(
        kind: SourceKind,
        source_id: impl Into<String>,
        scope: SourceScope,
        subresource_id: Option<String>,
        lifecycle: SourceLifecycle,
        revision: impl Into<String>,
    ) -> Result<Self> {
        let source = Self {
            kind,
            source_id: source_id.into(),
            scope,
            subresource_id,
            lifecycle,
            revision: revision.into(),
        };
        source.validate()?;
        Ok(source)
    }

    fn validate(&self) -> Result<()> {
        validate_id(&self.source_id, "sourceId")?;
        self.scope.validate()?;
        if let Some(subresource) = &self.subresource_id {
            validate_id(subresource, "subresourceId")?;
        }
        self.lifecycle.validate()?;
        validate_revision(&self.revision)?;
        Ok(())
    }

    /// 身份比较包含生命周期但不包含快照 revision，也从不比较显示名。
    pub fn same_object(&self, other: &Self) -> bool {
        self.slot() == other.slot() && self.lifecycle == other.lifecycle
    }

    fn slot(&self) -> (SourceKind, &str, &SourceScope, Option<&str>) {
        (
            self.kind,
            &self.source_id,
            &self.scope,
            self.subresource_id.as_deref(),
        )
    }

    pub fn kind(&self) -> SourceKind {
        self.kind
    }

    pub fn source_id(&self) -> &str {
        &self.source_id
    }

    pub fn scope(&self) -> &SourceScope {
        &self.scope
    }

    pub fn subresource_id(&self) -> Option<&str> {
        self.subresource_id.as_deref()
    }

    pub fn lifecycle(&self) -> &SourceLifecycle {
        &self.lifecycle
    }

    pub fn revision(&self) -> &str {
        &self.revision
    }
}

/// 本地连接引用。只有完整四元组相等才是同一次登记，不能只比较 connectionId。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", try_from = "ConnectionRefData")]
pub struct ConnectionRef {
    instance_id: String,
    connection_id: String,
    generation: String,
    kind: ConnectionKind,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ConnectionRefData {
    instance_id: String,
    connection_id: String,
    generation: String,
    kind: ConnectionKind,
}

impl TryFrom<ConnectionRefData> for ConnectionRef {
    type Error = anyhow::Error;

    fn try_from(value: ConnectionRefData) -> Result<Self> {
        let reference = Self {
            instance_id: value.instance_id,
            connection_id: value.connection_id,
            generation: value.generation,
            kind: value.kind,
        };
        reference.validate()?;
        Ok(reference)
    }
}

impl ConnectionRef {
    pub fn new(instance_id: impl Into<String>, kind: ConnectionKind) -> Result<Self> {
        Self::fresh(instance_id.into(), Uuid::new_v4().to_string(), kind)
    }

    fn fresh(instance_id: String, connection_id: String, kind: ConnectionKind) -> Result<Self> {
        let reference = Self {
            instance_id,
            connection_id,
            generation: Uuid::new_v4().to_string(),
            kind,
        };
        reference.validate()?;
        Ok(reference)
    }

    fn validate(&self) -> Result<()> {
        validate_uuid(&self.instance_id, "instanceId", false)?;
        validate_uuid(&self.connection_id, "connectionId", false)?;
        validate_uuid(&self.generation, "generation", true)?;
        Ok(())
    }

    pub fn instance_id(&self) -> &str {
        &self.instance_id
    }

    pub fn connection_id(&self) -> &str {
        &self.connection_id
    }

    pub fn generation(&self) -> &str {
        &self.generation
    }

    pub fn kind(&self) -> ConnectionKind {
        self.kind
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RegistrationState {
    LegacyUnresolved,
    Disabled,
    Enabled,
    Deleted,
}

/// 身份登记，不包含 grant。已知生命周期的新登记也默认 disabled。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", try_from = "ConnectionRegistrationData")]
pub struct ConnectionRegistration {
    reference: ConnectionRef,
    source: SourceRef,
    display_name: String,
    state: RegistrationState,
    revision: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ConnectionRegistrationData {
    reference: ConnectionRef,
    source: SourceRef,
    display_name: String,
    state: RegistrationState,
    revision: u64,
}

impl TryFrom<ConnectionRegistrationData> for ConnectionRegistration {
    type Error = anyhow::Error;

    fn try_from(value: ConnectionRegistrationData) -> Result<Self> {
        let registration = Self {
            reference: value.reference,
            source: value.source,
            display_name: value.display_name,
            state: value.state,
            revision: value.revision,
        };
        registration.validate()?;
        Ok(registration)
    }
}

impl ConnectionRegistration {
    pub fn new(
        reference: ConnectionRef,
        source: SourceRef,
        display_name: impl Into<String>,
    ) -> Result<Self> {
        let state = if source.lifecycle.is_known() {
            RegistrationState::Disabled
        } else {
            RegistrationState::LegacyUnresolved
        };
        let registration = Self {
            reference,
            source,
            display_name: display_name.into(),
            state,
            revision: 1,
        };
        registration.validate()?;
        Ok(registration)
    }

    fn validate(&self) -> Result<()> {
        validate_target_source(&self.reference, &self.source)?;
        validate_display_name(&self.display_name)?;
        ensure!(self.revision > 0, "登记 revision 必须大于零");
        match (&self.source.lifecycle, self.state) {
            (_, RegistrationState::Deleted)
            | (SourceLifecycle::LegacyUnresolved, RegistrationState::LegacyUnresolved)
            | (SourceLifecycle::Known { .. }, RegistrationState::Disabled)
            | (SourceLifecycle::Known { .. }, RegistrationState::Enabled) => Ok(()),
            _ => anyhow::bail!("登记状态与来源生命周期不一致"),
        }
    }

    pub fn reference(&self) -> &ConnectionRef {
        &self.reference
    }

    pub fn source(&self) -> &SourceRef {
        &self.source
    }

    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    pub fn state(&self) -> RegistrationState {
        self.state
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// 仅是身份层的必要条件，不代表调用者已有任何执行权限。
    pub fn is_grant_eligible(&self) -> bool {
        self.state == RegistrationState::Enabled && self.source.lifecycle.is_known()
    }
}

/// 后续授权记录须保存的完整目标绑定；不能将此快照本身当作授权票据。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", try_from = "RegistrationBindingData")]
pub struct RegistrationBinding {
    connection: ConnectionRef,
    source: SourceRef,
    registration_revision: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RegistrationBindingData {
    connection: ConnectionRef,
    source: SourceRef,
    registration_revision: u64,
}

impl TryFrom<RegistrationBindingData> for RegistrationBinding {
    type Error = anyhow::Error;

    fn try_from(value: RegistrationBindingData) -> Result<Self> {
        let binding = Self {
            connection: value.connection,
            source: value.source,
            registration_revision: value.registration_revision,
        };
        binding.validate()?;
        Ok(binding)
    }
}

impl RegistrationBinding {
    fn validate(&self) -> Result<()> {
        validate_target_source(&self.connection, &self.source)?;
        ensure!(self.source.lifecycle.is_known(), "未纳管来源不能绑定新授权");
        ensure!(self.registration_revision > 0, "登记 revision 必须大于零");
        Ok(())
    }

    pub fn connection(&self) -> &ConnectionRef {
        &self.connection
    }

    pub fn source(&self) -> &SourceRef {
        &self.source
    }

    pub fn registration_revision(&self) -> u64 {
        self.registration_revision
    }
}

/// 持久化边界，保留已删除的登记代际供审计。不是管理 API 的新增登记请求体。
///
/// `registrations` 按登记顺序保存；同一逻辑 ID 的旧代际必须已删除且位于新代际
/// 之前。持久层不得按 connectionId 覆盖 tombstone 或重排同一 ID 的历史。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", try_from = "RegistrationSnapshotData")]
pub struct RegistrationSnapshot {
    schema_version: u16,
    instance_id: String,
    registrations: Vec<ConnectionRegistration>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RegistrationSnapshotData {
    schema_version: u16,
    instance_id: String,
    registrations: Vec<ConnectionRegistration>,
}

impl TryFrom<RegistrationSnapshotData> for RegistrationSnapshot {
    type Error = anyhow::Error;

    fn try_from(value: RegistrationSnapshotData) -> Result<Self> {
        let snapshot = Self {
            schema_version: value.schema_version,
            instance_id: value.instance_id,
            registrations: value.registrations,
        };
        snapshot.validate()?;
        Ok(snapshot)
    }
}

impl RegistrationSnapshot {
    fn validate(&self) -> Result<()> {
        ensure!(
            self.schema_version == CONNECTION_IDENTITY_SCHEMA_VERSION,
            "不支持的连接身份 schemaVersion"
        );
        validate_uuid(&self.instance_id, "instanceId", false)?;
        let mut generations = HashSet::new();
        let mut latest: HashMap<&str, &ConnectionRegistration> = HashMap::new();
        let mut live_sources = HashSet::new();
        for registration in &self.registrations {
            registration.validate()?;
            let reference = registration.reference();
            ensure!(
                reference.instance_id == self.instance_id,
                "快照包含其他实例的登记"
            );
            ensure!(generations.insert(reference.generation()), "登记代际重复");
            if let Some(previous) = latest.insert(reference.connection_id(), registration) {
                ensure!(
                    previous.state == RegistrationState::Deleted,
                    "旧登记未删除就出现新代际"
                );
                ensure!(
                    previous.reference.kind == reference.kind,
                    "同一连接 ID 不得改变资源类型"
                );
            }
            if registration.state != RegistrationState::Deleted {
                ensure!(
                    live_sources.insert((reference.kind, registration.source.slot())),
                    "同一类型的来源对象已登记"
                );
            }
        }
        Ok(())
    }

    pub fn instance_id(&self) -> &str {
        &self.instance_id
    }

    pub fn schema_version(&self) -> u16 {
        self.schema_version
    }

    pub fn registrations(&self) -> &[ConnectionRegistration] {
        &self.registrations
    }
}

/// 单实例、无 I/O 的登记状态机。调用方负责受信持久化及串行提交，不提供 grant 服务。
#[derive(Debug, Clone)]
pub struct RegistrationRegistry {
    instance_id: String,
    registrations: Vec<ConnectionRegistration>,
}

impl RegistrationRegistry {
    pub fn new(instance_id: impl Into<String>) -> Result<Self> {
        let instance_id = instance_id.into();
        validate_uuid(&instance_id, "instanceId", false)?;
        Ok(Self {
            instance_id,
            registrations: Vec::new(),
        })
    }

    /// 只恢复当前实例的受信快照；跨实例导入须重新解析来源、登记及授权。
    /// 原状态及 revision 原样保留，不自动启用 disabled/unresolved/deleted 记录。
    /// 本模块不防止受信存储回滚，持久层仍须原子提交并防止旧快照覆盖新状态。
    pub fn from_snapshot(
        current_instance_id: impl Into<String>,
        snapshot: RegistrationSnapshot,
    ) -> Result<Self> {
        let current_instance_id = current_instance_id.into();
        validate_uuid(&current_instance_id, "instanceId", false)?;
        snapshot.validate()?;
        ensure!(
            snapshot.instance_id == current_instance_id,
            "不能在其他安装实例恢复登记绑定"
        );
        Ok(Self {
            instance_id: current_instance_id,
            registrations: snapshot.registrations,
        })
    }

    pub fn snapshot(&self) -> RegistrationSnapshot {
        RegistrationSnapshot {
            schema_version: CONNECTION_IDENTITY_SCHEMA_VERSION,
            instance_id: self.instance_id.clone(),
            registrations: self.registrations.clone(),
        }
    }

    pub fn instance_id(&self) -> &str {
        &self.instance_id
    }

    /// 包含历史 tombstone；目录展示须另做可见性过滤与有界投影。
    pub fn registrations(&self) -> &[ConnectionRegistration] {
        &self.registrations
    }

    /// 精确读取任一历史代际，不表示该登记当前仍有效。
    pub fn get(&self, reference: &ConnectionRef) -> Option<&ConnectionRegistration> {
        self.registrations
            .iter()
            .find(|registration| registration.reference == *reference)
    }

    pub fn register(
        &mut self,
        kind: ConnectionKind,
        source: SourceRef,
        display_name: impl Into<String>,
    ) -> Result<ConnectionRef> {
        let reference = ConnectionRef::new(self.instance_id.clone(), kind)?;
        self.insert_fresh(reference, source, display_name.into())
    }

    /// 只有最新代际已删除才可复用逻辑 ID；随机登记代际永不复用，默认也不启用。
    pub fn reregister(
        &mut self,
        deleted: &ConnectionRef,
        source: SourceRef,
        display_name: impl Into<String>,
    ) -> Result<ConnectionRef> {
        let index = self.current_index(deleted)?;
        ensure!(
            self.registrations[index].state == RegistrationState::Deleted,
            "须先删除旧登记"
        );
        let reference = ConnectionRef::fresh(
            self.instance_id.clone(),
            deleted.connection_id.clone(),
            deleted.kind,
        )?;
        self.insert_fresh(reference, source, display_name.into())
    }

    fn insert_fresh(
        &mut self,
        reference: ConnectionRef,
        source: SourceRef,
        display_name: String,
    ) -> Result<ConnectionRef> {
        let registration = ConnectionRegistration::new(reference.clone(), source, display_name)?;
        ensure!(
            !self.registrations.iter().any(|existing| {
                existing.state != RegistrationState::Deleted
                    && existing.reference.kind == reference.kind
                    && existing.source.slot() == registration.source.slot()
            }),
            "同一类型的来源对象已登记；生命周期变化须先使旧登记失效"
        );
        ensure!(
            !self.registrations.iter().any(|existing| {
                existing.reference.generation == reference.generation
                    || (existing.reference.connection_id == reference.connection_id
                        && (existing.state != RegistrationState::Deleted
                            || existing.reference.kind != reference.kind))
            }),
            "连接 ID 或登记代际冲突"
        );
        self.registrations.push(registration);
        Ok(reference)
    }

    pub fn set_enabled(&mut self, reference: &ConnectionRef, enabled: bool) -> Result<()> {
        let index = self.current_index(reference)?;
        let registration = &mut self.registrations[index];
        ensure!(
            registration.state != RegistrationState::Deleted,
            "已删除登记不能重新启用"
        );
        ensure!(
            registration.source.lifecycle.is_known(),
            "来源生命周期未知，必须保持未纳管"
        );
        let state = if enabled {
            RegistrationState::Enabled
        } else {
            RegistrationState::Disabled
        };
        if registration.state != state {
            // 撤销在计数耗尽时仍必须成功；耗尽后再启用则拒绝，旧绑定不能复活。
            registration.revision = if enabled {
                registration
                    .revision
                    .checked_add(1)
                    .context("登记 revision 已耗尽")?
            } else {
                registration.revision.saturating_add(1)
            };
            registration.state = state;
        }
        Ok(())
    }

    /// 来源owner确认同一对象的新快照版本；旧授权绑定随登记revision失效。
    /// 生命周期或账户/项目/端点身份域变化不得原位改绑，必须显式重新登记。
    pub fn observe_source_revision(
        &mut self,
        reference: &ConnectionRef,
        expected_registration_revision: u64,
        source: SourceRef,
    ) -> Result<()> {
        let index = self.current_index(reference)?;
        let registration = &mut self.registrations[index];
        ensure!(
            registration.state != RegistrationState::Deleted,
            "登记已删除"
        );
        ensure!(
            registration.revision == expected_registration_revision,
            "登记revision冲突"
        );
        ensure!(
            registration.source.same_object(&source),
            "来源对象变化须重新登记"
        );
        source.validate()?;
        if registration.source != source {
            let revision = registration
                .revision
                .checked_add(1)
                .context("登记revision已耗尽")?;
            registration.source = source;
            registration.revision = revision;
        }
        Ok(())
    }

    /// 只删除本领域登记，保留 tombstone；不会修改来源配置、设备配对或远端内容。
    pub fn delete(&mut self, reference: &ConnectionRef) -> Result<()> {
        let index = self.current_index(reference)?;
        let registration = &mut self.registrations[index];
        if registration.state != RegistrationState::Deleted {
            registration.revision = registration.revision.saturating_add(1);
            registration.state = RegistrationState::Deleted;
        }
        Ok(())
    }

    /// 只生成身份绑定，不批准任何能力。授权服务必须另外执行完整准入校验。
    pub fn binding_for(&self, reference: &ConnectionRef) -> Result<RegistrationBinding> {
        let registration = &self.registrations[self.current_index(reference)?];
        ensure!(registration.is_grant_eligible(), "登记未启用或来源尚未纳管");
        Ok(RegistrationBinding {
            connection: registration.reference.clone(),
            source: registration.source.clone(),
            registration_revision: registration.revision,
        })
    }

    /// 每次发放/使用 grant 前检查身份仍是当前代际和版本；通过不等于已获授权。
    pub fn validate_binding(
        &self,
        binding: &RegistrationBinding,
    ) -> Result<&ConnectionRegistration> {
        binding.validate()?;
        let registration = &self.registrations[self.current_index(&binding.connection)?];
        ensure!(registration.is_grant_eligible(), "登记未启用或来源尚未纳管");
        ensure!(
            registration.revision == binding.registration_revision
                && registration.source == binding.source,
            "登记或来源版本变化，旧授权绑定失效"
        );
        Ok(registration)
    }

    fn current_index(&self, reference: &ConnectionRef) -> Result<usize> {
        reference.validate()?;
        ensure!(
            reference.instance_id == self.instance_id,
            "连接引用属于其他安装实例"
        );
        let index = self
            .registrations
            .iter()
            .rposition(|registration| {
                registration.reference.connection_id == reference.connection_id
            })
            .context("连接登记不存在")?;
        ensure!(
            self.registrations[index].reference == *reference,
            "连接登记已被新代际替代"
        );
        Ok(index)
    }

    /// 只检查类型、实例及当前代际；查询者仍须分别具备关系与两端的可见权限。
    pub fn validate_relation(&self, relation: &ConnectionRelation) -> Result<()> {
        relation.validate()?;
        for reference in [&relation.from, &relation.to] {
            let registration = &self.registrations[self.current_index(reference)?];
            ensure!(
                registration.state != RegistrationState::Deleted,
                "关系端点已删除"
            );
        }
        // 允许展示 disabled/unresolved 端点；关系不改变就绪状态或授权。
        Ok(())
    }
}

/// 方向均为 AI → 提供它的软件或设备；没有权限、凭据或组织成员继承含义。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionRelationKind {
    EmbeddedIn,
    ProvidedBy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", try_from = "ConnectionRelationData")]
pub struct ConnectionRelation {
    id: String,
    kind: ConnectionRelationKind,
    from: ConnectionRef,
    to: ConnectionRef,
    revision: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ConnectionRelationData {
    id: String,
    kind: ConnectionRelationKind,
    from: ConnectionRef,
    to: ConnectionRef,
    revision: u64,
}

impl TryFrom<ConnectionRelationData> for ConnectionRelation {
    type Error = anyhow::Error;

    fn try_from(value: ConnectionRelationData) -> Result<Self> {
        let relation = Self {
            id: value.id,
            kind: value.kind,
            from: value.from,
            to: value.to,
            revision: value.revision,
        };
        relation.validate()?;
        Ok(relation)
    }
}

impl ConnectionRelation {
    pub fn new(
        kind: ConnectionRelationKind,
        from: ConnectionRef,
        to: ConnectionRef,
    ) -> Result<Self> {
        let relation = Self {
            id: Uuid::new_v4().to_string(),
            kind,
            from,
            to,
            revision: 1,
        };
        relation.validate()?;
        Ok(relation)
    }

    /// 纯值级 CAS；调用方仍须原子持久化，并通过 registry 检查端点当前有效。
    pub fn revised(
        &self,
        expected_revision: u64,
        kind: ConnectionRelationKind,
        from: ConnectionRef,
        to: ConnectionRef,
    ) -> Result<Self> {
        ensure!(self.revision == expected_revision, "关系 revision 冲突");
        let relation = Self {
            id: self.id.clone(),
            kind,
            from,
            to,
            revision: self
                .revision
                .checked_add(1)
                .context("关系 revision 已耗尽")?,
        };
        relation.validate()?;
        Ok(relation)
    }

    fn validate(&self) -> Result<()> {
        validate_uuid(&self.id, "relation id", false)?;
        self.from.validate()?;
        self.to.validate()?;
        ensure!(self.revision > 0, "关系 revision 必须大于零");
        ensure!(
            self.from.instance_id == self.to.instance_id,
            "关系端点必须属于同一安装实例"
        );
        ensure!(
            self.from.connection_id != self.to.connection_id,
            "关系不能指向同一逻辑连接或其其他代际"
        );
        ensure!(
            self.from.kind == ConnectionKind::ExternalAgent
                && matches!(
                    self.to.kind,
                    ConnectionKind::Software | ConnectionKind::Device
                ),
            "内置/提供关系必须引用独立 AI 及软件或设备"
        );
        Ok(())
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn kind(&self) -> ConnectionRelationKind {
        self.kind
    }

    pub fn from(&self) -> &ConnectionRef {
        &self.from
    }

    pub fn to(&self) -> &ConnectionRef {
        &self.to
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }
}

fn validate_target_source(reference: &ConnectionRef, source: &SourceRef) -> Result<()> {
    reference.validate()?;
    source.validate()?;
    ensure!(
        reference.instance_id == source.scope.instance_id,
        "连接与来源必须属于同一安装实例"
    );
    let compatible = match (source.kind, reference.kind) {
        (SourceKind::Device, ConnectionKind::Device) => true,
        (SourceKind::Device, ConnectionKind::ExternalAgent) => source.subresource_id.is_some(),
        (SourceKind::Nexus, ConnectionKind::ExternalAgent) => true,
        (
            SourceKind::Mcp | SourceKind::Plugin | SourceKind::Software,
            ConnectionKind::Software | ConnectionKind::ExternalAgent,
        ) => true,
        _ => false,
    };
    ensure!(
        compatible,
        "连接类型与来源类型不兼容；设备 AI 必须引用独立子资源"
    );
    Ok(())
}

fn validate_uuid(value: &str, field: &str, random_generation: bool) -> Result<()> {
    let parsed = Uuid::parse_str(value).with_context(|| format!("{field} 必须是规范 UUID"))?;
    ensure!(
        !parsed.is_nil() && parsed.to_string() == value,
        "{field} 必须是非空规范 UUID"
    );
    if random_generation {
        ensure!(
            parsed.get_version() == Some(uuid::Version::Random)
                && parsed.get_variant() == uuid::Variant::RFC4122,
            "登记 generation 必须是随机 UUID v4"
        );
    }
    Ok(())
}

fn validate_id(value: &str, field: &str) -> Result<()> {
    ensure!(
        !value.is_empty() && value.len() <= MAX_ID_BYTES,
        "{field} 长度无效"
    );
    ensure!(
        value.chars().all(|character| character.is_alphanumeric()
            || matches!(character, '-' | '_' | '.' | ':' | '/' | '@'))
            && !value.contains("://")
            && !value.starts_with('/')
            && value
                .split('/')
                .all(|segment| !matches!(segment, "" | "." | "..")),
        "{field} 必须是不含 URL、路径跳转、空白或凭据参数的不透明标识"
    );
    Ok(())
}

fn validate_revision(value: &str) -> Result<()> {
    ensure!(
        !value.is_empty()
            && value.len() <= MAX_ID_BYTES
            && !value
                .chars()
                .any(|character| character.is_control() || character.is_whitespace()),
        "来源 revision 无效"
    );
    Ok(())
}

fn validate_display_name(value: &str) -> Result<()> {
    ensure!(
        !value.is_empty()
            && value.len() <= MAX_DISPLAY_NAME_BYTES
            && value.trim() == value
            && !value.chars().any(char::is_control),
        "显示名必须非空、有界且不含控制字符或首尾空白"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::de::DeserializeOwned;
    use serde_json::{json, Value};

    const INSTANCE_A: &str = "11111111-1111-4111-8111-111111111111";
    const INSTANCE_B: &str = "22222222-2222-4222-8222-222222222222";

    fn source(
        instance: &str,
        authority: &str,
        account: &str,
        project: Option<&str>,
        subresource: Option<&str>,
        lifecycle: SourceLifecycle,
    ) -> SourceRef {
        SourceRef::new(
            SourceKind::Software,
            "same-remote-id",
            SourceScope::new(instance, authority, account, project.map(str::to_owned)).unwrap(),
            subresource.map(str::to_owned),
            lifecycle,
            "revision-1",
        )
        .unwrap()
    }

    fn known_source(account: &str, project: Option<&str>, subresource: Option<&str>) -> SourceRef {
        source(
            INSTANCE_A,
            "server-1",
            account,
            project,
            subresource,
            SourceLifecycle::known("object-generation-1").unwrap(),
        )
    }

    fn round_trip<T>(value: &T) -> T
    where
        T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug,
    {
        let decoded = serde_json::from_slice(&serde_json::to_vec(value).unwrap()).unwrap();
        assert_eq!(*value, decoded);
        decoded
    }

    fn reject<T: DeserializeOwned>(value: Value) {
        assert!(serde_json::from_value::<T>(value).is_err());
    }

    #[test]
    fn same_name_and_remote_id_do_not_merge_accounts_projects_authorities_or_subresources() {
        let mut registry = RegistrationRegistry::new(INSTANCE_A).unwrap();
        let sources = [
            known_source("account-a", Some("project-a"), None),
            known_source("account-b", Some("project-a"), None),
            known_source("account-a", Some("project-b"), None),
            known_source("account-a", None, None),
            source(
                INSTANCE_A,
                "server-2",
                "account-a",
                Some("project-a"),
                None,
                SourceLifecycle::known("object-generation-1").unwrap(),
            ),
            known_source("account-a", Some("project-a"), Some("assistant")),
        ];
        let mut refs = HashSet::new();
        let mut all_references = Vec::new();
        for source in sources {
            let reference = registry
                .register(ConnectionKind::Software, source.clone(), "同名软件")
                .unwrap();
            assert!(refs.insert(reference.clone()));
            assert_eq!(registry.get(&reference).unwrap().source(), &source);
            assert_eq!(registry.get(&reference).unwrap().display_name(), "同名软件");
            registry.set_enabled(&reference, true).unwrap();
            all_references.push(reference);
        }
        assert_eq!(refs.len(), 6);
        let first_binding = registry.binding_for(&all_references[0]).unwrap();
        for other in &all_references[1..] {
            let mut crossed = serde_json::to_value(&first_binding).unwrap();
            crossed["connection"] = serde_json::to_value(other).unwrap();
            let crossed: RegistrationBinding = serde_json::from_value(crossed).unwrap();
            assert!(registry.validate_binding(&crossed).is_err());
        }
        let snapshot = round_trip(&registry.snapshot());
        let restored = RegistrationRegistry::from_snapshot(INSTANCE_A, snapshot).unwrap();
        assert_eq!(restored.registrations(), registry.registrations());
        assert!(registry
            .register(
                ConnectionKind::Software,
                known_source("account-a", Some("project-a"), None),
                "另一个显示名"
            )
            .is_err());
    }

    #[test]
    fn same_source_on_different_installations_has_distinct_references_and_cannot_cross_bind() {
        let mut a = RegistrationRegistry::new(INSTANCE_A).unwrap();
        let mut b = RegistrationRegistry::new(INSTANCE_B).unwrap();
        let source_a = known_source("account-a", Some("project-a"), None);
        let source_b = source(
            INSTANCE_B,
            "server-1",
            "account-a",
            Some("project-a"),
            None,
            SourceLifecycle::known("object-generation-1").unwrap(),
        );
        assert!(!source_a.same_object(&source_b));
        let ref_a = a
            .register(ConnectionKind::Software, source_a.clone(), "同名软件")
            .unwrap();
        let ref_b = b
            .register(ConnectionKind::Software, source_b, "同名软件")
            .unwrap();
        assert_ne!(ref_a, ref_b);
        a.set_enabled(&ref_a, true).unwrap();
        b.set_enabled(&ref_b, true).unwrap();
        let binding = round_trip(&a.binding_for(&ref_a).unwrap());
        assert!(a.validate_binding(&binding).is_ok());
        assert!(b.validate_binding(&binding).is_err());
        assert!(b
            .register(ConnectionKind::Software, source_a, "其他实例来源")
            .is_err());
        let mut same_ids = serde_json::to_value(&ref_a).unwrap();
        same_ids["instanceId"] = json!(INSTANCE_B);
        let same_ids: ConnectionRef = serde_json::from_value(same_ids).unwrap();
        assert_ne!(ref_a, same_ids);
        assert!(a.binding_for(&same_ids).is_err());
    }

    #[test]
    fn connection_type_is_part_of_identity_not_inferred_from_source_transport() {
        let mut registry = RegistrationRegistry::new(INSTANCE_A).unwrap();
        let source = known_source("account-a", Some("project-a"), None);
        let software = registry
            .register(ConnectionKind::Software, source.clone(), "同名对象")
            .unwrap();
        let agent = registry
            .register(ConnectionKind::ExternalAgent, source.clone(), "同名对象")
            .unwrap();
        assert_ne!(software, agent);
        assert!(registry
            .register(ConnectionKind::Device, source, "不是物理设备")
            .is_err());
        assert!(registry.binding_for(&software).is_err());
        assert!(registry.binding_for(&agent).is_err());
    }

    #[test]
    fn delete_and_reregister_never_inherit_old_grant_or_approval_binding() {
        let mut registry = RegistrationRegistry::new(INSTANCE_A).unwrap();
        let source = known_source("account-a", Some("project-a"), None);
        let original = registry
            .register(ConnectionKind::Software, source.clone(), "同名软件")
            .unwrap();
        assert_eq!(
            registry.get(&original).unwrap().state(),
            RegistrationState::Disabled
        );
        registry.set_enabled(&original, true).unwrap();
        // 只模拟 grant 保存的身份字段，不实现或制造执行授权。
        let old_grant_target = round_trip(&registry.binding_for(&original).unwrap());
        assert!(registry.validate_binding(&old_grant_target).is_ok());
        assert!(registry
            .reregister(&original, source.clone(), "同名软件")
            .is_err());
        registry.delete(&original).unwrap();
        registry.delete(&original).unwrap();
        assert!(registry.validate_binding(&old_grant_target).is_err());
        assert!(registry.set_enabled(&original, true).is_err());
        let replacement = registry
            .reregister(&original, source.clone(), "同名软件")
            .unwrap();
        assert_eq!(original.connection_id(), replacement.connection_id());
        assert_ne!(original.generation(), replacement.generation());
        assert_eq!(
            registry.get(&original).unwrap().state(),
            RegistrationState::Deleted
        );
        assert_eq!(
            registry.get(&replacement).unwrap().state(),
            RegistrationState::Disabled
        );
        assert!(registry.binding_for(&replacement).is_err());
        registry.set_enabled(&replacement, true).unwrap();
        assert!(registry.validate_binding(&old_grant_target).is_err());
        assert!(registry
            .validate_binding(&registry.binding_for(&replacement).unwrap())
            .is_ok());
        assert!(registry.delete(&original).is_err());
        assert!(registry
            .reregister(&original, source, "过期管理请求")
            .is_err());
        let restored =
            RegistrationRegistry::from_snapshot(INSTANCE_A, round_trip(&registry.snapshot()))
                .unwrap();
        assert!(restored.validate_binding(&old_grant_target).is_err());
        assert!(restored.binding_for(&replacement).is_ok());
    }

    #[test]
    fn disabling_and_reenabling_invalidates_previously_captured_bindings() {
        let mut registry = RegistrationRegistry::new(INSTANCE_A).unwrap();
        let reference = registry
            .register(
                ConnectionKind::Software,
                known_source("account-a", None, None),
                "软件",
            )
            .unwrap();
        registry.set_enabled(&reference, true).unwrap();
        let old = registry.binding_for(&reference).unwrap();
        registry.set_enabled(&reference, true).unwrap();
        assert!(registry.validate_binding(&old).is_ok());
        registry.set_enabled(&reference, false).unwrap();
        assert!(registry.validate_binding(&old).is_err());
        registry.set_enabled(&reference, true).unwrap();
        assert!(registry.validate_binding(&old).is_err());
        assert_ne!(
            old.registration_revision(),
            registry
                .binding_for(&reference)
                .unwrap()
                .registration_revision()
        );
    }

    #[test]
    fn snapshot_restore_never_reactivates_disabled_unresolved_or_deleted_registrations() {
        let mut registry = RegistrationRegistry::new(INSTANCE_A).unwrap();
        let disabled = registry
            .register(
                ConnectionKind::Software,
                known_source("account-a", None, None),
                "已禁用软件",
            )
            .unwrap();
        registry.set_enabled(&disabled, true).unwrap();
        let old_binding = registry.binding_for(&disabled).unwrap();
        registry.set_enabled(&disabled, false).unwrap();
        let unresolved = registry
            .register(
                ConnectionKind::Software,
                source(
                    INSTANCE_A,
                    "server-1",
                    "account-b",
                    None,
                    None,
                    SourceLifecycle::LegacyUnresolved,
                ),
                "未纳管软件",
            )
            .unwrap();
        let deleted = registry
            .register(
                ConnectionKind::Software,
                known_source("account-c", None, None),
                "已删除软件",
            )
            .unwrap();
        registry.set_enabled(&deleted, true).unwrap();
        let deleted_binding = registry.binding_for(&deleted).unwrap();
        registry.delete(&deleted).unwrap();

        let snapshot = round_trip(&registry.snapshot());
        assert_eq!(
            snapshot.schema_version(),
            CONNECTION_IDENTITY_SCHEMA_VERSION
        );
        let mut restored = RegistrationRegistry::from_snapshot(INSTANCE_A, snapshot).unwrap();
        assert_eq!(registry.registrations(), restored.registrations());
        for reference in [&disabled, &unresolved, &deleted] {
            assert!(restored.binding_for(reference).is_err());
        }
        assert!(restored.validate_binding(&old_binding).is_err());
        assert!(restored.validate_binding(&deleted_binding).is_err());
        assert!(restored.set_enabled(&unresolved, true).is_err());
        assert!(restored.set_enabled(&deleted, true).is_err());
        restored.set_enabled(&disabled, true).unwrap();
        assert!(restored.binding_for(&disabled).is_ok());
        assert!(restored.validate_binding(&old_binding).is_err());
    }

    #[test]
    fn revision_or_lifecycle_changes_do_not_allow_a_duplicate_live_source_registration() {
        let mut registry = RegistrationRegistry::new(INSTANCE_A).unwrap();
        let original_source = known_source("account-a", Some("project-a"), None);
        let reference = registry
            .register(ConnectionKind::Software, original_source.clone(), "软件")
            .unwrap();
        for (pointer, changed) in [
            ("/revision", "revision-2"),
            ("/lifecycle/generation", "object-generation-2"),
        ] {
            let mut changed_source = serde_json::to_value(&original_source).unwrap();
            *changed_source.pointer_mut(pointer).unwrap() = json!(changed);
            let changed_source: SourceRef = serde_json::from_value(changed_source).unwrap();
            let before = registry.snapshot();
            assert!(registry
                .register(ConnectionKind::Software, changed_source.clone(), "改名软件")
                .is_err());
            assert_eq!(before, registry.snapshot());
            // 持久快照也不能用新来源版本/生命周期绕过重复来源检查。
            let mut duplicate = serde_json::to_value(&before).unwrap();
            let mut record = duplicate["registrations"][0].clone();
            record["source"] = serde_json::to_value(changed_source).unwrap();
            record["reference"]["connectionId"] = json!(Uuid::new_v4().to_string());
            record["reference"]["generation"] = json!(Uuid::new_v4().to_string());
            duplicate["registrations"]
                .as_array_mut()
                .unwrap()
                .push(record);
            reject::<RegistrationSnapshot>(duplicate);
        }
        registry.set_enabled(&reference, true).unwrap();
        let old_binding = registry.binding_for(&reference).unwrap();
        registry.delete(&reference).unwrap();
        let mut next_source = serde_json::to_value(original_source).unwrap();
        next_source["lifecycle"]["generation"] = json!("object-generation-2");
        let replacement = registry
            .reregister(
                &reference,
                serde_json::from_value(next_source).unwrap(),
                "软件",
            )
            .unwrap();
        registry.set_enabled(&replacement, true).unwrap();
        assert!(registry.validate_binding(&old_binding).is_err());
        assert!(registry.binding_for(&replacement).is_ok());
    }

    #[test]
    fn unknown_lifecycle_stays_unmanaged_until_deleted_and_explicitly_reenrolled() {
        let mut registry = RegistrationRegistry::new(INSTANCE_A).unwrap();
        let legacy_source = source(
            INSTANCE_A,
            "server-1",
            "account-a",
            Some("project-a"),
            None,
            SourceLifecycle::LegacyUnresolved,
        );
        let legacy = registry
            .register(ConnectionKind::Software, legacy_source, "旧来源")
            .unwrap();
        let record = round_trip(registry.get(&legacy).unwrap());
        assert_eq!(record.state(), RegistrationState::LegacyUnresolved);
        assert!(!record.is_grant_eligible());
        assert!(registry.set_enabled(&legacy, true).is_err());
        assert!(registry.set_enabled(&legacy, false).is_err());
        assert!(registry.binding_for(&legacy).is_err());
        let known = known_source("account-a", Some("project-a"), None);
        assert!(registry
            .register(ConnectionKind::Software, known.clone(), "不能自动升级")
            .is_err());
        let mut forged = serde_json::to_value(&record).unwrap();
        forged["state"] = json!("enabled");
        reject::<ConnectionRegistration>(forged);
        let mut forged = serde_json::to_value(&record).unwrap();
        forged["state"] = json!("disabled");
        reject::<ConnectionRegistration>(forged);
        let binding =
            json!({"connection": legacy, "source": record.source(), "registrationRevision": 1});
        reject::<RegistrationBinding>(binding);
        registry.delete(&legacy).unwrap();
        let enrolled = registry.reregister(&legacy, known, "旧来源").unwrap();
        assert_ne!(legacy.generation(), enrolled.generation());
        assert!(registry.binding_for(&enrolled).is_err());
        registry.set_enabled(&enrolled, true).unwrap();
        assert!(registry.binding_for(&enrolled).is_ok());
    }

    #[test]
    fn source_revision_is_not_identity_but_lifecycle_and_all_scopes_are() {
        let original = known_source("account-a", Some("project-a"), Some("assistant"));
        let mut changed = serde_json::to_value(&original).unwrap();
        changed["revision"] = json!("revision-2");
        let changed: SourceRef = serde_json::from_value(changed).unwrap();
        assert!(original.same_object(&changed));
        assert_ne!(original, changed);
        let mut lifecycle = serde_json::to_value(&original).unwrap();
        lifecycle["lifecycle"]["generation"] = json!("object-generation-2");
        assert!(!original.same_object(&serde_json::from_value(lifecycle).unwrap()));
        for pointer in [
            "/scope/accountId",
            "/scope/projectId",
            "/scope/authorityId",
            "/sourceId",
            "/subresourceId",
        ] {
            let mut changed = serde_json::to_value(&original).unwrap();
            *changed.pointer_mut(pointer).unwrap() = json!("different-id");
            assert!(!original.same_object(&serde_json::from_value(changed).unwrap()));
        }
    }

    #[test]
    fn software_ai_relation_is_reference_only_and_requires_current_typed_endpoints() {
        let mut registry = RegistrationRegistry::new(INSTANCE_A).unwrap();
        let software = registry
            .register(
                ConnectionKind::Software,
                known_source("account-a", Some("project-a"), Some("documents")),
                "同名产品",
            )
            .unwrap();
        let ai = registry
            .register(
                ConnectionKind::ExternalAgent,
                known_source("account-a", Some("project-a"), Some("assistant")),
                "同名产品",
            )
            .unwrap();
        registry.set_enabled(&software, true).unwrap();
        let software_binding = registry.binding_for(&software).unwrap();
        let relation = ConnectionRelation::new(
            ConnectionRelationKind::EmbeddedIn,
            ai.clone(),
            software.clone(),
        )
        .unwrap();
        let relation = round_trip(&relation);
        assert!(registry.validate_relation(&relation).is_ok());
        assert!(registry.binding_for(&ai).is_err());
        assert!(registry.validate_binding(&software_binding).is_ok());
        let revised = relation
            .revised(
                1,
                ConnectionRelationKind::ProvidedBy,
                ai.clone(),
                software.clone(),
            )
            .unwrap();
        assert_eq!(revised.id(), relation.id());
        assert_eq!(revised.revision(), 2);
        assert!(relation
            .revised(0, relation.kind(), ai.clone(), software.clone())
            .is_err());
        assert!(ConnectionRelation::new(
            ConnectionRelationKind::EmbeddedIn,
            software.clone(),
            ai.clone()
        )
        .is_err());
        assert!(ConnectionRelation::new(
            ConnectionRelationKind::EmbeddedIn,
            ai.clone(),
            ai.clone()
        )
        .is_err());
        let mut other_instance = serde_json::to_value(&software).unwrap();
        other_instance["instanceId"] = json!(INSTANCE_B);
        assert!(ConnectionRelation::new(
            ConnectionRelationKind::EmbeddedIn,
            ai.clone(),
            serde_json::from_value(other_instance).unwrap()
        )
        .is_err());
        // 删除展示关系（丢弃纯值）不改变任何连接登记或授权绑定。
        drop(revised);
        assert!(registry.validate_binding(&software_binding).is_ok());
        assert!(registry.binding_for(&ai).is_err());
        registry.delete(&ai).unwrap();
        assert!(registry.validate_relation(&relation).is_err());
        let replacement = registry
            .reregister(
                &ai,
                known_source("account-a", Some("project-a"), Some("assistant")),
                "同名产品",
            )
            .unwrap();
        assert!(registry.validate_relation(&relation).is_err());
        let new_relation =
            ConnectionRelation::new(ConnectionRelationKind::EmbeddedIn, replacement, software)
                .unwrap();
        assert!(registry.validate_relation(&new_relation).is_ok());
    }

    #[test]
    fn device_ai_requires_an_independent_subresource_and_does_not_change_device_type() {
        let mut raw = serde_json::to_value(known_source("device-service", None, None)).unwrap();
        raw["kind"] = json!("device");
        let device_source: SourceRef = serde_json::from_value(raw.clone()).unwrap();
        let mut registry = RegistrationRegistry::new(INSTANCE_A).unwrap();
        let device = registry
            .register(ConnectionKind::Device, device_source.clone(), "设备")
            .unwrap();
        assert!(registry
            .register(ConnectionKind::Software, device_source.clone(), "不是软件")
            .is_err());
        assert!(registry
            .register(ConnectionKind::ExternalAgent, device_source, "不是 AI")
            .is_err());
        raw["subresourceId"] = json!("embedded-ai");
        let ai = registry
            .register(
                ConnectionKind::ExternalAgent,
                serde_json::from_value(raw).unwrap(),
                "设备 AI",
            )
            .unwrap();
        let relation =
            ConnectionRelation::new(ConnectionRelationKind::EmbeddedIn, ai, device.clone())
                .unwrap();
        assert!(registry.validate_relation(&relation).is_ok());
        assert_eq!(
            registry.get(&device).unwrap().reference().kind(),
            ConnectionKind::Device
        );
        assert!(registry.binding_for(&device).is_err());
    }

    #[test]
    fn deserialization_cannot_bypass_reference_scope_lifecycle_or_state_validation() {
        let source = known_source("account-a", Some("project-a"), Some("assistant"));
        let reference = ConnectionRef::new(INSTANCE_A, ConnectionKind::ExternalAgent).unwrap();
        let record =
            ConnectionRegistration::new(reference.clone(), source.clone(), "软件 AI").unwrap();
        round_trip(&reference);
        round_trip(&source);
        round_trip(&record);
        for (pointer, invalid) in [
            ("/instanceId", json!("https://server.invalid")),
            ("/connectionId", json!(Uuid::nil().to_string())),
            ("/generation", json!("00000000-0000-1000-8000-000000000001")),
            ("/generation", json!(Uuid::nil().to_string())),
            ("/kind", json!("autonomous_device")),
        ] {
            let mut value = serde_json::to_value(&reference).unwrap();
            *value.pointer_mut(pointer).unwrap() = invalid;
            reject::<ConnectionRef>(value);
        }
        for (pointer, invalid) in [
            ("/scope/instanceId", json!("")),
            ("/scope/accountId", json!("")),
            (
                "/scope/authorityId",
                json!("https://user:secret@server.invalid"),
            ),
            ("/scope/projectId", json!(" ")),
            ("/subresourceId", json!("")),
            ("/sourceId", json!("secret?token=canary")),
            ("/lifecycle/generation", json!("")),
            ("/revision", json!("")),
        ] {
            let mut value = serde_json::to_value(&source).unwrap();
            *value.pointer_mut(pointer).unwrap() = invalid;
            reject::<SourceRef>(value);
        }
        for (pointer, invalid) in [
            ("/revision", json!(0)),
            ("/state", json!("legacy_unresolved")),
            ("/state", json!("authorized")),
            ("/displayName", json!("")),
            ("/source/scope/instanceId", json!(INSTANCE_B)),
            ("/reference/kind", json!("device")),
        ] {
            let mut value = serde_json::to_value(&record).unwrap();
            *value.pointer_mut(pointer).unwrap() = invalid;
            reject::<ConnectionRegistration>(value);
        }
        let mut value = serde_json::to_value(&record).unwrap();
        value["credential"] = json!("must-not-be-accepted");
        reject::<ConnectionRegistration>(value);
        let mut value = serde_json::to_value(&source).unwrap();
        value["scope"]["endpoint"] = json!("https://server.invalid");
        reject::<SourceRef>(value);
        reject::<SourceLifecycle>(json!({"state": "legacy_unresolved", "generation": "forged"}));
        // 手工构造 enum 也不能绕过 SourceRef 的深层校验。
        assert!(SourceRef::new(
            SourceKind::Software,
            "source",
            source.scope().clone(),
            None,
            SourceLifecycle::Known {
                generation: String::new()
            },
            "revision-1"
        )
        .is_err());
    }

    #[test]
    fn binding_decode_and_current_validation_reject_unresolved_or_changed_scopes_and_revisions() {
        let mut registry = RegistrationRegistry::new(INSTANCE_A).unwrap();
        let reference = registry
            .register(
                ConnectionKind::Software,
                known_source("account-a", Some("project-a"), Some("documents")),
                "软件",
            )
            .unwrap();
        registry.set_enabled(&reference, true).unwrap();
        let binding = registry.binding_for(&reference).unwrap();
        for pointer in [
            "/source/scope/accountId",
            "/source/scope/projectId",
            "/source/scope/authorityId",
            "/source/subresourceId",
            "/source/lifecycle/generation",
            "/source/revision",
        ] {
            let mut changed = serde_json::to_value(&binding).unwrap();
            *changed.pointer_mut(pointer).unwrap() = json!("different-value");
            let changed: RegistrationBinding = serde_json::from_value(changed).unwrap();
            assert!(registry.validate_binding(&changed).is_err());
        }
        let mut changed = serde_json::to_value(&binding).unwrap();
        changed["registrationRevision"] = json!(binding.registration_revision() + 1);
        assert!(registry
            .validate_binding(&serde_json::from_value(changed).unwrap())
            .is_err());
        let mut changed = serde_json::to_value(&binding).unwrap();
        changed["registrationRevision"] = json!(0);
        reject::<RegistrationBinding>(changed);
        let mut changed = serde_json::to_value(&binding).unwrap();
        changed["source"]["lifecycle"] = json!({"state": "legacy_unresolved"});
        reject::<RegistrationBinding>(changed);
    }

    #[test]
    fn snapshot_decode_rejects_duplicate_live_objects_and_generation_or_instance_forgery() {
        let mut registry = RegistrationRegistry::new(INSTANCE_A).unwrap();
        let reference = registry
            .register(
                ConnectionKind::Software,
                known_source("account-a", None, None),
                "软件",
            )
            .unwrap();
        let snapshot = serde_json::to_value(registry.snapshot()).unwrap();
        let mut unsupported = snapshot.clone();
        unsupported["schemaVersion"] = json!(CONNECTION_IDENTITY_SCHEMA_VERSION + 1);
        reject::<RegistrationSnapshot>(unsupported);
        let mut duplicate = snapshot.clone();
        let record = duplicate["registrations"][0].clone();
        duplicate["registrations"]
            .as_array_mut()
            .unwrap()
            .push(record);
        reject::<RegistrationSnapshot>(duplicate);
        let mut duplicate_source = snapshot.clone();
        let mut record = duplicate_source["registrations"][0].clone();
        record["reference"]["connectionId"] = json!(Uuid::new_v4().to_string());
        record["reference"]["generation"] = json!(Uuid::new_v4().to_string());
        duplicate_source["registrations"]
            .as_array_mut()
            .unwrap()
            .push(record);
        reject::<RegistrationSnapshot>(duplicate_source);
        let mut mixed_instance = snapshot.clone();
        mixed_instance["instanceId"] = json!(INSTANCE_B);
        reject::<RegistrationSnapshot>(mixed_instance);
        let mut type_change = snapshot.clone();
        type_change["registrations"][0]["state"] = json!("deleted");
        let mut record = snapshot["registrations"][0].clone();
        record["reference"]["kind"] = json!("external_agent");
        record["reference"]["generation"] = json!(Uuid::new_v4().to_string());
        type_change["registrations"]
            .as_array_mut()
            .unwrap()
            .push(record);
        reject::<RegistrationSnapshot>(type_change);
        assert!(RegistrationRegistry::from_snapshot(INSTANCE_B, registry.snapshot()).is_err());
        assert!(registry.binding_for(&reference).is_err());
    }

    #[test]
    fn relation_decode_rejects_wrong_types_instances_revisions_and_authority_fields() {
        let ai = ConnectionRef::new(INSTANCE_A, ConnectionKind::ExternalAgent).unwrap();
        let software = ConnectionRef::new(INSTANCE_A, ConnectionKind::Software).unwrap();
        let relation =
            ConnectionRelation::new(ConnectionRelationKind::EmbeddedIn, ai, software).unwrap();
        for (pointer, invalid) in [
            ("/from/kind", json!("device")),
            ("/to/kind", json!("external_agent")),
            ("/to/instanceId", json!(INSTANCE_B)),
            ("/revision", json!(0)),
            ("/kind", json!("inherits_grants")),
        ] {
            let mut value = serde_json::to_value(&relation).unwrap();
            *value.pointer_mut(pointer).unwrap() = invalid;
            reject::<ConnectionRelation>(value);
        }
        let mut value = serde_json::to_value(&relation).unwrap();
        value["grants"] = json!(["forged-grant"]);
        reject::<ConnectionRelation>(value);
    }

    #[test]
    fn revision_exhaustion_cannot_prevent_revocation_or_revive_an_old_binding() {
        let mut registry = RegistrationRegistry::new(INSTANCE_A).unwrap();
        let reference = registry
            .register(
                ConnectionKind::Software,
                known_source("account-a", None, None),
                "软件",
            )
            .unwrap();
        registry.set_enabled(&reference, true).unwrap();
        let mut value = serde_json::to_value(registry.snapshot()).unwrap();
        value["registrations"][0]["revision"] = json!(u64::MAX);
        let mut registry =
            RegistrationRegistry::from_snapshot(INSTANCE_A, serde_json::from_value(value).unwrap())
                .unwrap();
        let old = registry.binding_for(&reference).unwrap();
        registry.set_enabled(&reference, false).unwrap();
        assert!(registry.validate_binding(&old).is_err());
        assert!(registry.set_enabled(&reference, true).is_err());
        registry.delete(&reference).unwrap();
        assert!(registry.validate_binding(&old).is_err());
    }

    #[test]
    fn serialized_identity_contains_only_references_and_no_configuration_or_credentials() {
        let mut registry = RegistrationRegistry::new(INSTANCE_A).unwrap();
        registry
            .register(
                ConnectionKind::Software,
                known_source("account-a", Some("project-a"), Some("documents")),
                "软件",
            )
            .unwrap();
        let value = serde_json::to_value(registry.snapshot()).unwrap();
        let record = value["registrations"][0].as_object().unwrap();
        assert_eq!(
            record.keys().map(String::as_str).collect::<HashSet<_>>(),
            HashSet::from(["reference", "source", "displayName", "state", "revision"])
        );
        let source = record["source"].as_object().unwrap();
        assert_eq!(
            source.keys().map(String::as_str).collect::<HashSet<_>>(),
            HashSet::from([
                "kind",
                "sourceId",
                "scope",
                "subresourceId",
                "lifecycle",
                "revision"
            ])
        );
        let encoded = serde_json::to_string(&value).unwrap();
        for forbidden in [
            "endpoint",
            "password",
            "credential",
            "accessToken",
            "command",
            "grants",
        ] {
            assert!(!encoded.contains(forbidden));
        }
    }
}
