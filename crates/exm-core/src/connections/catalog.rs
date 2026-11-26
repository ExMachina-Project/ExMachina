//! 无 I/O 的只读目录查询；不是来源仓库、执行 grant、Agent 工具或 HTTP API。
//!
//! # 受信调用边界
//! `CatalogScope` 只能由宿主认证上下文生成。来源 owner 必须在每次查询前取得当前、
//! 精确的可见事实，再构建 `HostCatalogAccess`；不能把请求 JSON、UI 管理权限、远端
//! 自述或执行 grant 直接当作这些事实。没有统一来源 ACL 时使用 `deny_all`，不能因
//! “已经登录管理页”放行。构造入口仅向本 crate 开放，所有宿主事实均不实现 Deserialize。
//!
//! 评估绑定 actor / instance / project / session / audience；None 项目、None 会话都
//! 不是通配符。来源 scope 还须与查询的实例、项目精确一致，并在 owner 的可见集合中。
//! Management 评估不能用于 Agent 查询；可见集合只是读取评估结果，不是可执行票据。
//! 本服务从不创建 grant，也不把目录可见性传入能力执行授权。
//!
//! `CatalogEntry` 只接受当前登记及既有 `CapabilitySnapshot`。owner 负责从当前代际
//! 取值、处理来源失效；不能把历史登记、远端 DTO 或导出的能力投影恢复为宿主事实。
//! 本文件不维护来源缓存，不复制可写配置，也不接收凭据、凭据状态、错误正文或自由
//! metadata。输出仅有闭合的安全摘要；管理者同样看不到凭据 metadata、端点、证据
//! binding key、schema 或材料目的地。能力授权始终为 Unknown，实际执行另行求交。
//!
//! # 分页与隐私
//! 所有名称匹配、计数、分页和一跳邻域均在连接、来源、能力自身、关系自身及两端
//! ACL 之后。隐藏与不存在的单对象查询统一为 NotFound。每页默认 50、硬顶 200；
//! 能力独立分页，邻域按边分页，故每页的边和邻居都不超过 limit，不递归展开全图。
//!
//! 游标为随机 UUID，仅在本服务的有界、短期内存表内有效，不含公开快照 hash。
//! 它绑定完整查询（含原始 limit、目标和过滤项）、精确 scope、当前 ACL revision
//! 及全部可见安全摘要。内部双随机 hash 只用于比较快照，不充当公开 MAC/授权。
//! 同一活跃绑定复用游标，隐藏对象增删不会产生新 token 或改变响应。快照不包含
//! 全局目录/source revision，owner 提供的 ACL revision 也必须是该 scope 的权限
//! revision，而非每次来源刷新递增的全局计数；隐藏变化不能靠版本号侧漏。
//! 每次翻页重新应用当前 ACL；游标失效、过期、跨 scope 或被逐出均统一拒绝。

use super::capability::{
    ApprovalRequirement, CapabilityAuthorizationState, CapabilityAvailability, CapabilityEffect,
    CapabilityGuarantee, CapabilityRole, CapabilitySnapshot, DetectionState, VerificationState,
};
use super::identity::{
    ConnectionKind, ConnectionRef, ConnectionRegistration, ConnectionRelation,
    ConnectionRelationKind, RegistrationState, SourceKind, SourceLifecycle, SourceRef, SourceScope,
};
use serde::{Deserialize, Serialize};
use std::collections::hash_map::{DefaultHasher, RandomState};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::hash::{BuildHasher, Hasher};
use std::io::{self, Write};
use uuid::Uuid;

pub const DEFAULT_CATALOG_PAGE_LIMIT: usize = 50;
pub const MAX_CATALOG_PAGE_LIMIT: usize = 200;
pub const MAX_CATALOG_SEARCH_BYTES: usize = 256;
pub const CATALOG_CURSOR_TTL_MS: u64 = 5 * 60 * 1000;
pub const MAX_CATALOG_CURSOR_STATES: usize = 1024;
const MAX_CONTEXT_ID_BYTES: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CatalogError {
    #[error("目录调用上下文无效")]
    InvalidHostContext,
    #[error("目录可见性评估与当前调用范围不匹配")]
    ScopeMismatch,
    #[error("目录搜索条件超限或包含控制字符")]
    InvalidSearch,
    #[error("目录 limit 必须在 1 到 200 之间")]
    InvalidLimit,
    #[error("目录游标无效或已过期，请重新查询")]
    InvalidCursor,
    #[error("目录对象不存在或不可见")]
    NotFound,
    #[error("可见目录快照包含冲突记录")]
    InvalidSnapshot,
}

pub type CatalogResult<T> = Result<T, CatalogError>;

/// 不同消费面必须重新取得各自的来源可见性，不能重用管理面评估。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatalogAudience {
    Management,
    Agent,
}

/// 不可从客户端反序列化；actor 和 scope 来自认证宿主，不来自查询参数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogScope {
    instance_id: String,
    actor_id: String,
    project_id: Option<String>,
    session_id: Option<String>,
    audience: CatalogAudience,
}

impl CatalogScope {
    pub(crate) fn from_authenticated_host(
        instance_id: String,
        actor_id: String,
        project_id: Option<String>,
        session_id: Option<String>,
        audience: CatalogAudience,
    ) -> CatalogResult<Self> {
        let instance =
            Uuid::parse_str(&instance_id).map_err(|_| CatalogError::InvalidHostContext)?;
        if instance.is_nil() || instance.to_string() != instance_id {
            return Err(CatalogError::InvalidHostContext);
        }
        for value in std::iter::once(actor_id.as_str())
            .chain(project_id.as_deref())
            .chain(session_id.as_deref())
        {
            if !valid_context_id(value) {
                return Err(CatalogError::InvalidHostContext);
            }
        }
        Ok(Self {
            instance_id,
            actor_id,
            project_id,
            session_id,
            audience,
        })
    }

    pub fn instance_id(&self) -> &str {
        &self.instance_id
    }

    pub fn actor_id(&self) -> &str {
        &self.actor_id
    }

    pub fn project_id(&self) -> Option<&str> {
        self.project_id.as_deref()
    }

    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    pub fn audience(&self) -> CatalogAudience {
        self.audience
    }
}

fn valid_context_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_CONTEXT_ID_BYTES
        && value
            .chars()
            .all(|ch| ch.is_alphanumeric() || matches!(ch, '-' | '_' | '.' | ':' | '/' | '@'))
        && !value.contains("://")
        && !value.starts_with('/')
        && value
            .split('/')
            .all(|segment| !matches!(segment, "" | "." | ".."))
}

/// 能力名只在完整登记代际内有意义，ACL 还精确绑定能力 revision。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilityId {
    connection: ConnectionRef,
    name: String,
    revision: u64,
}

impl CapabilityId {
    pub(crate) fn for_snapshot(connection: &ConnectionRef, snapshot: &CapabilitySnapshot) -> Self {
        Self {
            connection: connection.clone(),
            name: snapshot.descriptor().id.clone(),
            revision: snapshot.descriptor().revision,
        }
    }

    pub fn connection(&self) -> &ConnectionRef {
        &self.connection
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }
}

/// 关系自身的独立读取事实；关系改版后必须由 owner 重新评估。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RelationId {
    id: String,
    revision: u64,
}

impl RelationId {
    pub(crate) fn for_relation(relation: &ConnectionRelation) -> Self {
        Self {
            id: relation.id().to_owned(),
            revision: relation.revision(),
        }
    }
}

/// 已由受信 owner 计算的精确只读可见集合；不保存执行许可，不实现序列化或 Debug。
///
/// 来源 scope、连接、能力、关系分别显式列入；任何一层缺失都拒绝。不能将管理员
/// 的 sources/refs 复制给 Agent；即使 actor 相同，audience/project/session 也须重验。
pub struct HostCatalogAccess {
    scope: CatalogScope,
    acl_revision: u64,
    sources: HashSet<SourceScope>,
    connections: HashSet<ConnectionRef>,
    capabilities: HashSet<CapabilityId>,
    relations: HashSet<RelationId>,
}

impl HostCatalogAccess {
    /// 当前来源 owner 没有可用 ACL 时的唯一默认值；没有任何隐式管理者例外。
    pub(crate) fn deny_all(scope: CatalogScope, acl_revision: u64) -> CatalogResult<Self> {
        Self::evaluated_by_owner(
            scope,
            acl_revision,
            HashSet::new(),
            HashSet::new(),
            HashSet::new(),
            HashSet::new(),
        )
    }

    /// 只供宿主来源/身份 owner 在完成当前范围的权限判断后调用；不是外部请求 DTO。
    /// ACL revision 必须反映该调用范围的权限变化，不应反映无关隐藏对象增删。
    pub(crate) fn evaluated_by_owner(
        scope: CatalogScope,
        acl_revision: u64,
        sources: HashSet<SourceScope>,
        connections: HashSet<ConnectionRef>,
        capabilities: HashSet<CapabilityId>,
        relations: HashSet<RelationId>,
    ) -> CatalogResult<Self> {
        if acl_revision == 0 {
            return Err(CatalogError::InvalidHostContext);
        }
        Ok(Self {
            scope,
            acl_revision,
            sources,
            connections,
            capabilities,
            relations,
        })
    }

    fn can_read_connection(&self, registration: &ConnectionRegistration) -> bool {
        let source = registration.source();
        registration.reference().instance_id() == self.scope.instance_id()
            && source.scope().instance_id() == self.scope.instance_id()
            && source.scope().project_id() == self.scope.project_id()
            && self.sources.contains(source.scope())
            && self.connections.contains(registration.reference())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CatalogFreshness {
    Unknown,
    Current,
    Stale,
}

/// 闭合的非秘密元数据。没有凭据状态、自由文本错误、端点或任意 metadata map。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogMetadata {
    pub availability: CapabilityAvailability,
    pub freshness: CatalogFreshness,
}

impl Default for CatalogMetadata {
    fn default() -> Self {
        Self {
            availability: CapabilityAvailability::Unknown,
            freshness: CatalogFreshness::Unknown,
        }
    }
}

/// 短期查询输入，不是新的持久化来源或配置副本。只接受 owner 的当前登记/宿主快照。
/// 禁止将来源完整管理投影直接序列化返回，或从远端/导出 DTO 生成宿主验证事实。
#[derive(Clone)]
pub struct CatalogEntry {
    registration: ConnectionRegistration,
    capabilities: Vec<CapabilitySnapshot>,
    metadata: CatalogMetadata,
}

impl CatalogEntry {
    pub(crate) fn new(
        registration: ConnectionRegistration,
        capabilities: Vec<CapabilitySnapshot>,
        metadata: CatalogMetadata,
    ) -> Self {
        Self {
            registration,
            capabilities,
            metadata,
        }
    }

    pub fn registration(&self) -> &ConnectionRegistration {
        &self.registration
    }

    pub fn capability_snapshots(&self) -> &[CapabilitySnapshot] {
        &self.capabilities
    }

    fn summary(&self) -> CatalogConnection {
        CatalogConnection {
            reference: self.registration.reference().clone(),
            source: CatalogSource::from_ref(self.registration.source()),
            display_name: self.registration.display_name().to_owned(),
            state: self.registration.state(),
            metadata: self.metadata,
        }
    }
}

/// owner 提供当前登记切片，不得传 registry 的全量历史。没有持久来源缓存或网络句柄。
#[derive(Clone, Copy)]
pub struct CatalogSnapshot<'a> {
    entries: &'a [CatalogEntry],
    relations: &'a [ConnectionRelation],
}

impl<'a> CatalogSnapshot<'a> {
    pub(crate) fn from_current_entries(
        entries: &'a [CatalogEntry],
        relations: &'a [ConnectionRelation],
    ) -> Self {
        Self { entries, relations }
    }
}

/// 安全来源身份；不输出可能随隐藏对象变化的来源全局 revision。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogSource {
    pub kind: SourceKind,
    pub source_id: String,
    pub scope: SourceScope,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subresource_id: Option<String>,
    pub lifecycle: SourceLifecycle,
}

impl CatalogSource {
    fn from_ref(source: &SourceRef) -> Self {
        Self {
            kind: source.kind(),
            source_id: source.source_id().to_owned(),
            scope: source.scope().clone(),
            subresource_id: source.subresource_id().map(str::to_owned),
            lifecycle: source.lifecycle().clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogConnection {
    pub reference: ConnectionRef,
    pub source: CatalogSource,
    pub display_name: String,
    pub state: RegistrationState,
    pub metadata: CatalogMetadata,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogGuarantee {
    pub guarantee: CapabilityGuarantee,
    pub declared: bool,
    pub verified: bool,
}

/// 复用宿主能力 projection 的保守事实，但不暴露完整 descriptor/evidence 或授权票据。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogCapability {
    pub id: CapabilityId,
    pub role: CapabilityRole,
    pub declared: bool,
    pub detection: DetectionState,
    pub verification: VerificationState,
    pub effects_verification: VerificationState,
    pub availability: CapabilityAvailability,
    pub authorization: CapabilityAuthorizationState,
    pub effective_effects: BTreeSet<CapabilityEffect>,
    pub approval: ApprovalRequirement,
    pub guarantees: Vec<CatalogGuarantee>,
}

impl CatalogCapability {
    fn from_snapshot(
        connection: &ConnectionRef,
        snapshot: &CapabilitySnapshot,
        now_ms: u64,
    ) -> Self {
        // 目录读取评估绝不转换成 HostCapabilityAuthorization。
        let projection = snapshot.projection(None, "catalog.visibility.only", now_ms);
        Self {
            id: CapabilityId::for_snapshot(connection, snapshot),
            role: projection.descriptor.role,
            declared: projection.declared,
            detection: projection.detection,
            verification: projection.verification,
            effects_verification: projection.effects_verification,
            availability: projection.availability,
            authorization: projection.authorization,
            effective_effects: projection.effective_effects,
            approval: projection.approval,
            guarantees: projection
                .guarantees
                .into_iter()
                .map(|guarantee| CatalogGuarantee {
                    guarantee: guarantee.guarantee,
                    declared: guarantee.declared,
                    verified: guarantee.verified,
                })
                .collect(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CatalogQuery {
    #[serde(default)]
    pub search: String,
    pub kind: Option<ConnectionKind>,
    pub source_kind: Option<SourceKind>,
    pub source_scope: Option<SourceScope>,
}

impl CatalogQuery {
    fn matches(&self, connection: &CatalogConnection, search: &str) -> bool {
        self.kind
            .is_none_or(|kind| connection.reference.kind() == kind)
            && self
                .source_kind
                .is_none_or(|kind| connection.source.kind == kind)
            && self
                .source_scope
                .as_ref()
                .is_none_or(|scope| connection.source.scope == *scope)
            && (search.is_empty()
                || connection.display_name.to_lowercase().contains(search)
                || connection.source.source_id.to_lowercase().contains(search))
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CapabilityQuery {
    #[serde(default)]
    pub search: String,
    pub role: Option<CapabilityRole>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RelationQuery {
    pub endpoint: Option<ConnectionRef>,
    pub kind: Option<ConnectionRelationKind>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NeighborhoodQuery {
    #[serde(default)]
    pub neighbors: CatalogQuery,
    pub relation_kind: Option<ConnectionRelationKind>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CatalogPageRequest {
    pub limit: Option<usize>,
    pub cursor: Option<String>,
}

impl CatalogPageRequest {
    fn checked_limit(&self) -> CatalogResult<usize> {
        let limit = self.limit.unwrap_or(DEFAULT_CATALOG_PAGE_LIMIT);
        if !(1..=MAX_CATALOG_PAGE_LIMIT).contains(&limit) {
            return Err(CatalogError::InvalidLimit);
        }
        if let Some(cursor) = &self.cursor {
            if cursor.len() != 36
                || Uuid::parse_str(cursor).is_err()
                || cursor.bytes().any(|byte| byte.is_ascii_uppercase())
            {
                return Err(CatalogError::InvalidCursor);
            }
        }
        Ok(limit)
    }
}

/// total 只统计 ACL 后符合查询的记录，truncated 表示仍有后续页。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogPage<T> {
    pub items: Vec<T>,
    pub total: usize,
    pub limit: usize,
    pub truncated: bool,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogCounts {
    pub connections: usize,
    pub capabilities: usize,
    pub relations: usize,
}

/// 按边分页；neighbors 只包含当前页边对应的去重邻居，不附加隐藏/未授权度数。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogNeighborhood {
    pub center: ConnectionRef,
    pub neighbors: Vec<CatalogConnection>,
    pub total_neighbors: usize,
    pub relations: CatalogPage<ConnectionRelation>,
}

#[derive(Serialize)]
struct VisibleCatalog {
    connections: Vec<CatalogConnection>,
    capabilities: Vec<CatalogCapability>,
    relations: Vec<ConnectionRelation>,
}

impl VisibleCatalog {
    fn evaluate(
        scope: &CatalogScope,
        access: &HostCatalogAccess,
        snapshot: CatalogSnapshot<'_>,
        now_ms: u64,
    ) -> CatalogResult<Self> {
        if *scope != access.scope {
            return Err(CatalogError::ScopeMismatch);
        }
        let mut visible = Self {
            connections: Vec::new(),
            capabilities: Vec::new(),
            relations: Vec::new(),
        };
        let mut references = HashSet::new();
        let mut logical_references = HashSet::new();
        let mut capability_ids = HashSet::new();
        for entry in snapshot.entries {
            if !access.can_read_connection(entry.registration()) {
                continue;
            }
            let reference = entry.registration.reference();
            // 只校验已可见记录，隐藏坏记录不能改变查询状态或泄露存在性。
            if !references.insert(reference.clone())
                || !logical_references.insert((
                    reference.instance_id().to_owned(),
                    reference.connection_id().to_owned(),
                ))
            {
                return Err(CatalogError::InvalidSnapshot);
            }
            visible.connections.push(entry.summary());
            for capability in &entry.capabilities {
                let id = CapabilityId::for_snapshot(reference, capability);
                if !access.capabilities.contains(&id) {
                    continue;
                }
                if !capability_ids.insert((reference.clone(), id.name().to_owned())) {
                    return Err(CatalogError::InvalidSnapshot);
                }
                visible.capabilities.push(CatalogCapability::from_snapshot(
                    reference, capability, now_ms,
                ));
            }
        }
        let mut relation_ids = HashSet::new();
        for relation in snapshot.relations {
            if !access
                .relations
                .contains(&RelationId::for_relation(relation))
                || !references.contains(relation.from())
                || !references.contains(relation.to())
            {
                continue;
            }
            if !relation_ids.insert(relation.id()) {
                return Err(CatalogError::InvalidSnapshot);
            }
            visible.relations.push(relation.clone());
        }
        visible.connections.sort_by(|a, b| {
            a.display_name
                .cmp(&b.display_name)
                .then_with(|| reference_key(&a.reference).cmp(&reference_key(&b.reference)))
        });
        visible.capabilities.sort_by(|a, b| {
            reference_key(a.id.connection())
                .cmp(&reference_key(b.id.connection()))
                .then_with(|| a.id.name().cmp(b.id.name()))
                .then_with(|| a.id.revision().cmp(&b.id.revision()))
        });
        visible.relations.sort_by(|a, b| a.id().cmp(b.id()));
        Ok(visible)
    }

    fn connection(&self, reference: &ConnectionRef) -> CatalogResult<&CatalogConnection> {
        self.connections
            .iter()
            .find(|connection| connection.reference == *reference)
            .ok_or(CatalogError::NotFound)
    }
}

fn reference_key(reference: &ConnectionRef) -> (&str, &str, &str, u8) {
    (
        reference.instance_id(),
        reference.connection_id(),
        reference.generation(),
        match reference.kind() {
            ConnectionKind::Software => 0,
            ConnectionKind::ExternalAgent => 1,
            ConnectionKind::Device => 2,
        },
    )
}

fn checked_search(search: &str) -> CatalogResult<String> {
    if search.len() > MAX_CATALOG_SEARCH_BYTES || search.chars().any(char::is_control) {
        return Err(CatalogError::InvalidSearch);
    }
    // 有界字面量匹配，不接受正则、glob 或任意表达式。
    Ok(search.trim().to_lowercase())
}

#[derive(Clone, PartialEq, Eq)]
enum CursorQuery {
    Connections(CatalogQuery),
    Capabilities(ConnectionRef, CapabilityQuery),
    Relations(RelationQuery),
    Neighborhood(ConnectionRef, NeighborhoodQuery),
}

#[derive(Clone, PartialEq, Eq)]
struct CursorBinding {
    scope: CatalogScope,
    acl_revision: u64,
    visible_fingerprint: [u64; 2],
    query: CursorQuery,
    // None 与显式 50 也分别绑定，不留下未参与绑定的请求字段。
    requested_limit: Option<usize>,
}

struct CursorState {
    binding: CursorBinding,
    next_offset: usize,
    issued_at_ms: u64,
    expires_at_ms: u64,
    last_used: u64,
}

/// 只持有分页游标的有界短期状态，不保存目录、来源配置、grant、凭据或操作记录。
/// 宿主应复用同一实例；重建服务会明确使旧游标失效，而不是猜测 offset 后继续。
/// 所有方法的 now_ms 均须取宿主可信时间，不能直通客户端时间戳。
pub struct CatalogService {
    first_hash_state: RandomState,
    second_hash_state: RandomState,
    cursors: HashMap<String, CursorState>,
    sequence: u64,
}

impl Default for CatalogService {
    fn default() -> Self {
        Self::new()
    }
}

impl CatalogService {
    pub fn new() -> Self {
        Self {
            first_hash_state: RandomState::new(),
            second_hash_state: RandomState::new(),
            cursors: HashMap::new(),
            sequence: 0,
        }
    }

    pub fn connections(
        &mut self,
        scope: &CatalogScope,
        access: &HostCatalogAccess,
        snapshot: CatalogSnapshot<'_>,
        query: &CatalogQuery,
        page: &CatalogPageRequest,
        now_ms: u64,
    ) -> CatalogResult<CatalogPage<CatalogConnection>> {
        let limit = page.checked_limit()?;
        let search = checked_search(&query.search)?;
        let visible = VisibleCatalog::evaluate(scope, access, snapshot, now_ms)?;
        let binding = self.binding(
            scope,
            access,
            &visible,
            CursorQuery::Connections(query.clone()),
            page.limit,
        )?;
        let items = visible
            .connections
            .iter()
            .filter(|connection| query.matches(connection, &search))
            .collect();
        self.paginate(items, binding, page, limit, now_ms)
    }

    pub fn connection(
        &self,
        scope: &CatalogScope,
        access: &HostCatalogAccess,
        snapshot: CatalogSnapshot<'_>,
        reference: &ConnectionRef,
        now_ms: u64,
    ) -> CatalogResult<CatalogConnection> {
        VisibleCatalog::evaluate(scope, access, snapshot, now_ms)?
            .connection(reference)
            .cloned()
    }

    /// 能力自身 ACL 在名称/角色过滤和计数前执行，连接可见不意味着能力可见。
    #[allow(clippy::too_many_arguments)] // 显式保留受信 scope/评估/快照与不可信 query 的边界。
    pub fn capabilities(
        &mut self,
        scope: &CatalogScope,
        access: &HostCatalogAccess,
        snapshot: CatalogSnapshot<'_>,
        reference: &ConnectionRef,
        query: &CapabilityQuery,
        page: &CatalogPageRequest,
        now_ms: u64,
    ) -> CatalogResult<CatalogPage<CatalogCapability>> {
        let limit = page.checked_limit()?;
        let search = checked_search(&query.search)?;
        let visible = VisibleCatalog::evaluate(scope, access, snapshot, now_ms)?;
        visible.connection(reference)?;
        let binding = self.binding(
            scope,
            access,
            &visible,
            CursorQuery::Capabilities(reference.clone(), query.clone()),
            page.limit,
        )?;
        let items = visible
            .capabilities
            .iter()
            .filter(|capability| {
                capability.id.connection() == reference
                    && query.role.is_none_or(|role| capability.role == role)
                    && (search.is_empty() || capability.id.name().to_lowercase().contains(&search))
            })
            .collect();
        self.paginate(items, binding, page, limit, now_ms)
    }

    pub fn relations(
        &mut self,
        scope: &CatalogScope,
        access: &HostCatalogAccess,
        snapshot: CatalogSnapshot<'_>,
        query: &RelationQuery,
        page: &CatalogPageRequest,
        now_ms: u64,
    ) -> CatalogResult<CatalogPage<ConnectionRelation>> {
        let limit = page.checked_limit()?;
        let visible = VisibleCatalog::evaluate(scope, access, snapshot, now_ms)?;
        if let Some(endpoint) = &query.endpoint {
            visible.connection(endpoint)?;
        }
        let binding = self.binding(
            scope,
            access,
            &visible,
            CursorQuery::Relations(query.clone()),
            page.limit,
        )?;
        let items = visible
            .relations
            .iter()
            .filter(|relation| {
                query.kind.is_none_or(|kind| relation.kind() == kind)
                    && query.endpoint.as_ref().is_none_or(|endpoint| {
                        relation.from() == endpoint || relation.to() == endpoint
                    })
            })
            .collect();
        self.paginate(items, binding, page, limit, now_ms)
    }

    /// 只有一跳，按可见边分页；邻居和边的展开上限均为 page.limit。
    #[allow(clippy::too_many_arguments)] // 与能力查询相同的宿主/查询边界，另有精确中心 ref。
    pub fn neighborhood(
        &mut self,
        scope: &CatalogScope,
        access: &HostCatalogAccess,
        snapshot: CatalogSnapshot<'_>,
        center: &ConnectionRef,
        query: &NeighborhoodQuery,
        page: &CatalogPageRequest,
        now_ms: u64,
    ) -> CatalogResult<CatalogNeighborhood> {
        let limit = page.checked_limit()?;
        let search = checked_search(&query.neighbors.search)?;
        let visible = VisibleCatalog::evaluate(scope, access, snapshot, now_ms)?;
        visible.connection(center)?;
        let by_reference: HashMap<_, _> = visible
            .connections
            .iter()
            .map(|connection| (&connection.reference, connection))
            .collect();
        let opposite = |relation: &ConnectionRelation| {
            if relation.from() == center {
                Some(relation.to().clone())
            } else if relation.to() == center {
                Some(relation.from().clone())
            } else {
                None
            }
        };
        let mut total_neighbors = HashSet::new();
        let items = visible
            .relations
            .iter()
            .filter(|relation| {
                if !query
                    .relation_kind
                    .is_none_or(|kind| relation.kind() == kind)
                {
                    return false;
                }
                opposite(relation).is_some_and(|reference| {
                    by_reference.get(&reference).is_some_and(|neighbor| {
                        if query.neighbors.matches(neighbor, &search) {
                            total_neighbors.insert(reference);
                            true
                        } else {
                            false
                        }
                    })
                })
            })
            .collect();
        let binding = self.binding(
            scope,
            access,
            &visible,
            CursorQuery::Neighborhood(center.clone(), query.clone()),
            page.limit,
        )?;
        let relations = self.paginate(items, binding, page, limit, now_ms)?;
        let returned_neighbors: HashSet<_> = relations.items.iter().filter_map(opposite).collect();
        drop(by_reference);
        let neighbors = visible
            .connections
            .into_iter()
            .filter(|connection| returned_neighbors.contains(&connection.reference))
            .collect();
        Ok(CatalogNeighborhood {
            center: center.clone(),
            neighbors,
            total_neighbors: total_neighbors.len(),
            relations,
        })
    }

    /// 连接查询命中的子目录计数；关系必须两端都在该子目录，能力仍分别检查自身 ACL。
    pub fn counts(
        &self,
        scope: &CatalogScope,
        access: &HostCatalogAccess,
        snapshot: CatalogSnapshot<'_>,
        query: &CatalogQuery,
        now_ms: u64,
    ) -> CatalogResult<CatalogCounts> {
        let search = checked_search(&query.search)?;
        let visible = VisibleCatalog::evaluate(scope, access, snapshot, now_ms)?;
        let references: HashSet<_> = visible
            .connections
            .iter()
            .filter(|connection| query.matches(connection, &search))
            .map(|connection| &connection.reference)
            .collect();
        Ok(CatalogCounts {
            connections: references.len(),
            capabilities: visible
                .capabilities
                .iter()
                .filter(|capability| references.contains(capability.id.connection()))
                .count(),
            relations: visible
                .relations
                .iter()
                .filter(|relation| {
                    references.contains(relation.from()) && references.contains(relation.to())
                })
                .count(),
        })
    }

    fn binding(
        &self,
        scope: &CatalogScope,
        access: &HostCatalogAccess,
        visible: &VisibleCatalog,
        query: CursorQuery,
        requested_limit: Option<usize>,
    ) -> CatalogResult<CursorBinding> {
        let mut writer = FingerprintWriter {
            first: self.first_hash_state.build_hasher(),
            second: self.second_hash_state.build_hasher(),
        };
        serde_json::to_writer(&mut writer, visible).map_err(|_| CatalogError::InvalidSnapshot)?;
        Ok(CursorBinding {
            scope: scope.clone(),
            acl_revision: access.acl_revision,
            visible_fingerprint: [writer.first.finish(), writer.second.finish()],
            query,
            requested_limit,
        })
    }

    fn paginate<T: Clone>(
        &mut self,
        items: Vec<&T>,
        binding: CursorBinding,
        page: &CatalogPageRequest,
        limit: usize,
        now_ms: u64,
    ) -> CatalogResult<CatalogPage<T>> {
        self.cursors
            .retain(|_, state| state.issued_at_ms <= now_ms && now_ms < state.expires_at_ms);
        self.sequence = self.sequence.saturating_add(1);
        let start = if let Some(cursor) = &page.cursor {
            let state = self
                .cursors
                .get_mut(cursor)
                .ok_or(CatalogError::InvalidCursor)?;
            if state.binding != binding || state.next_offset >= items.len() {
                return Err(CatalogError::InvalidCursor);
            }
            state.last_used = self.sequence;
            state.next_offset
        } else {
            0
        };
        let total = items.len();
        let end = start.saturating_add(limit).min(total);
        let next_cursor = if end < total {
            Some(self.cursor_for(binding, end, now_ms)?)
        } else {
            None
        };
        Ok(CatalogPage {
            items: items[start..end]
                .iter()
                .map(|item| (*item).clone())
                .collect(),
            total,
            limit,
            truncated: end < total,
            next_cursor,
        })
    }

    fn cursor_for(
        &mut self,
        binding: CursorBinding,
        next_offset: usize,
        now_ms: u64,
    ) -> CatalogResult<String> {
        if let Some((token, state)) = self
            .cursors
            .iter_mut()
            .find(|(_, state)| state.binding == binding && state.next_offset == next_offset)
        {
            state.last_used = self.sequence;
            return Ok(token.clone());
        }
        // 溢出不能变成永不过期的游标。
        let expires_at_ms = now_ms
            .checked_add(CATALOG_CURSOR_TTL_MS)
            .ok_or(CatalogError::InvalidHostContext)?;
        if self.cursors.len() >= MAX_CATALOG_CURSOR_STATES {
            if let Some(oldest) = self
                .cursors
                .iter()
                .min_by_key(|(_, state)| state.last_used)
                .map(|(token, _)| token.clone())
            {
                self.cursors.remove(&oldest);
            }
        }
        let token = loop {
            let token = Uuid::new_v4().to_string();
            if !self.cursors.contains_key(&token) {
                break token;
            }
        };
        self.cursors.insert(
            token.clone(),
            CursorState {
                binding,
                next_offset,
                issued_at_ms: now_ms,
                expires_at_ms,
                last_used: self.sequence,
            },
        );
        Ok(token)
    }
}

/// 流式比较安全摘要，不为 hash 再复制整份大目录，也不公开 hash 或 hash key。
struct FingerprintWriter {
    first: DefaultHasher,
    second: DefaultHasher,
}

impl Write for FingerprintWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.first.write(bytes);
        self.second.write(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connections::capability::{
        CapabilityDescriptor, CapabilityEvidenceClaim, CapabilityLimits, CapabilitySupport,
        DataDestination, EvidenceOrigin, HostCapabilityEvidence, PayloadSchema,
        CAPABILITY_CONTRACT_VERSION,
    };
    use crate::connections::identity::RegistrationRegistry;
    use serde_json::json;

    const INSTANCE_A: &str = "11111111-1111-4111-8111-111111111111";
    const INSTANCE_B: &str = "22222222-2222-4222-8222-222222222222";
    const NOW: u64 = 1000;

    fn scope() -> CatalogScope {
        CatalogScope::from_authenticated_host(
            INSTANCE_A.into(),
            "user:alice".into(),
            Some("project-a".into()),
            Some("session-a".into()),
            CatalogAudience::Management,
        )
        .unwrap()
    }

    fn source_scope(account: &str) -> SourceScope {
        SourceScope::new(
            INSTANCE_A,
            "fixture.owner",
            account,
            Some("project-a".into()),
        )
        .unwrap()
    }

    fn descriptor(name: &str) -> CapabilityDescriptor {
        CapabilityDescriptor {
            version: CAPABILITY_CONTRACT_VERSION,
            id: name.into(),
            revision: 1,
            role: CapabilityRole::ToolProvider,
            input_schema: PayloadSchema::Null,
            output_schema: PayloadSchema::Null,
            effects: BTreeSet::from([CapabilityEffect::Read]),
            data_destinations: BTreeSet::from([DataDestination::Local]),
            limits: CapabilityLimits::default(),
            support: CapabilitySupport::default(),
        }
    }

    fn capability(name: &str) -> CapabilitySnapshot {
        CapabilitySnapshot::from_declaration("fixture.binding".into(), descriptor(name)).unwrap()
    }

    fn entry_in(
        index: usize,
        kind: ConnectionKind,
        source_scope: SourceScope,
        name: &str,
    ) -> CatalogEntry {
        let reference = ConnectionRef::new(source_scope.instance_id(), kind).unwrap();
        let source = SourceRef::new(
            if kind == ConnectionKind::Device {
                SourceKind::Device
            } else {
                SourceKind::Software
            },
            format!("remote-{index}"),
            source_scope,
            None,
            SourceLifecycle::known("source-generation-1").unwrap(),
            "source-revision-1",
        )
        .unwrap();
        CatalogEntry::new(
            ConnectionRegistration::new(reference, source, name).unwrap(),
            Vec::new(),
            CatalogMetadata::default(),
        )
    }

    fn entry(index: usize) -> CatalogEntry {
        entry_in(
            index,
            ConnectionKind::Software,
            source_scope("account-a"),
            &format!("visible-{index:04}"),
        )
    }

    fn reference(entry: &CatalogEntry) -> &ConnectionRef {
        entry.registration().reference()
    }

    fn relation(agent: &CatalogEntry, software: &CatalogEntry) -> ConnectionRelation {
        ConnectionRelation::new(
            ConnectionRelationKind::EmbeddedIn,
            reference(agent).clone(),
            reference(software).clone(),
        )
        .unwrap()
    }

    fn allow(
        scope: &CatalogScope,
        entries: &[CatalogEntry],
        relations: &[ConnectionRelation],
    ) -> HostCatalogAccess {
        allow_at_revision(scope, entries, relations, 1)
    }

    fn allow_at_revision(
        scope: &CatalogScope,
        entries: &[CatalogEntry],
        relations: &[ConnectionRelation],
        revision: u64,
    ) -> HostCatalogAccess {
        HostCatalogAccess::evaluated_by_owner(
            scope.clone(),
            revision,
            entries
                .iter()
                .map(|entry| entry.registration.source().scope().clone())
                .collect(),
            entries
                .iter()
                .map(|entry| reference(entry).clone())
                .collect(),
            entries
                .iter()
                .flat_map(|entry| {
                    entry
                        .capabilities
                        .iter()
                        .map(|capability| CapabilityId::for_snapshot(reference(entry), capability))
                })
                .collect(),
            relations.iter().map(RelationId::for_relation).collect(),
        )
        .unwrap()
    }

    fn snapshot<'a>(
        entries: &'a [CatalogEntry],
        relations: &'a [ConnectionRelation],
    ) -> CatalogSnapshot<'a> {
        CatalogSnapshot::from_current_entries(entries, relations)
    }

    fn page(limit: usize) -> CatalogPageRequest {
        CatalogPageRequest {
            limit: Some(limit),
            cursor: None,
        }
    }

    fn next<T>(response: &CatalogPage<T>, request: &CatalogPageRequest) -> CatalogPageRequest {
        CatalogPageRequest {
            limit: request.limit,
            cursor: Some(response.next_cursor.clone().expect("必须明确分页")),
        }
    }

    #[test]
    fn no_owner_acl_defaults_to_deny_even_for_management() {
        let scope = scope();
        let mut entries = vec![entry(0)];
        entries[0].capabilities.push(capability("documents.read"));
        let access = HostCatalogAccess::deny_all(scope.clone(), 1).unwrap();
        let mut service = CatalogService::new();
        let view = snapshot(&entries, &[]);
        let response = service
            .connections(
                &scope,
                &access,
                view,
                &CatalogQuery::default(),
                &CatalogPageRequest::default(),
                NOW,
            )
            .unwrap();
        assert!(response.items.is_empty());
        assert_eq!(response.total, 0);
        assert!(!response.truncated);
        assert_eq!(response.next_cursor, None);
        assert_eq!(
            service
                .counts(&scope, &access, view, &CatalogQuery::default(), NOW)
                .unwrap(),
            CatalogCounts {
                connections: 0,
                capabilities: 0,
                relations: 0,
            }
        );
        assert_eq!(
            service.connection(&scope, &access, view, reference(&entries[0]), NOW),
            Err(CatalogError::NotFound)
        );
        assert_eq!(
            service.capabilities(
                &scope,
                &access,
                view,
                reference(&entries[0]),
                &CapabilityQuery::default(),
                &page(1),
                NOW,
            ),
            Err(CatalogError::NotFound)
        );
        assert_eq!(
            service.neighborhood(
                &scope,
                &access,
                view,
                reference(&entries[0]),
                &NeighborhoodQuery::default(),
                &page(1),
                NOW,
            ),
            Err(CatalogError::NotFound)
        );
        assert!(HostCatalogAccess::deny_all(scope, 0).is_err());
    }

    #[test]
    fn caller_cannot_supply_visibility_facts_in_query_json() {
        for field in [
            "actorId",
            "projectId",
            "sessionId",
            "audience",
            "sources",
            "visibleConnections",
            "authorized",
            "credentials",
        ] {
            let mut query = json!({});
            query[field] = json!(true);
            assert!(serde_json::from_value::<CatalogQuery>(query).is_err());
        }
        for instance in ["not-an-instance", "00000000-0000-0000-0000-000000000000"] {
            assert!(CatalogScope::from_authenticated_host(
                instance.into(),
                "actor".into(),
                None,
                None,
                CatalogAudience::Management,
            )
            .is_err());
        }
        for actor in [
            "",
            "https://account",
            "actor?key=secret",
            "../actor",
            "a\nb",
        ] {
            assert!(CatalogScope::from_authenticated_host(
                INSTANCE_A.into(),
                actor.into(),
                None,
                None,
                CatalogAudience::Management,
            )
            .is_err());
        }
    }

    #[test]
    fn source_and_reference_visibility_are_an_intersection_not_an_or() {
        let scope = scope();
        let entries = vec![entry(0)];
        let wrong_sources = [
            source_scope("account-b"),
            SourceScope::new(
                INSTANCE_A,
                "other.authority",
                "account-a",
                Some("project-a".into()),
            )
            .unwrap(),
            SourceScope::new(INSTANCE_A, "fixture.owner", "account-a", None).unwrap(),
            SourceScope::new(
                INSTANCE_A,
                "fixture.owner",
                "account-a",
                Some("project-b".into()),
            )
            .unwrap(),
            SourceScope::new(
                INSTANCE_B,
                "fixture.owner",
                "account-a",
                Some("project-a".into()),
            )
            .unwrap(),
        ];
        let mut service = CatalogService::new();
        for source_allowed in [false, true] {
            for reference_allowed in [false, true] {
                let mut access = HostCatalogAccess::deny_all(scope.clone(), 1).unwrap();
                if source_allowed {
                    access.sources.insert(source_scope("account-a"));
                }
                if reference_allowed {
                    access.connections.insert(reference(&entries[0]).clone());
                }
                let response = service
                    .connections(
                        &scope,
                        &access,
                        snapshot(&entries, &[]),
                        &CatalogQuery::default(),
                        &page(1),
                        NOW,
                    )
                    .unwrap();
                assert_eq!(
                    response.total,
                    usize::from(source_allowed && reference_allowed)
                );
            }
        }
        for wrong_source in wrong_sources {
            let mut access = allow(&scope, &entries, &[]);
            access.sources = HashSet::from([wrong_source]);
            assert_eq!(
                service
                    .counts(
                        &scope,
                        &access,
                        snapshot(&entries, &[]),
                        &CatalogQuery::default(),
                        NOW,
                    )
                    .unwrap()
                    .connections,
                0
            );
        }
    }

    #[test]
    fn exact_actor_instance_project_session_and_audience_must_match_the_evaluation() {
        let owner = scope();
        let entries = vec![entry(0)];
        let access = allow(&owner, &entries, &[]);
        let mut variants = Vec::new();
        let mut actor = owner.clone();
        actor.actor_id = "user:bob".into();
        variants.push(actor);
        let mut instance = owner.clone();
        instance.instance_id = INSTANCE_B.into();
        variants.push(instance);
        let mut project = owner.clone();
        project.project_id = Some("project-b".into());
        variants.push(project);
        let mut no_project = owner.clone();
        no_project.project_id = None;
        variants.push(no_project);
        let mut session = owner.clone();
        session.session_id = Some("session-b".into());
        variants.push(session);
        let mut no_session = owner.clone();
        no_session.session_id = None;
        variants.push(no_session);
        let mut agent = owner.clone();
        agent.audience = CatalogAudience::Agent;
        variants.push(agent);
        let mut service = CatalogService::new();
        for caller in variants {
            assert_eq!(
                service.connections(
                    &caller,
                    &access,
                    snapshot(&entries, &[]),
                    &CatalogQuery::default(),
                    &page(1),
                    NOW,
                ),
                Err(CatalogError::ScopeMismatch)
            );
        }
    }

    #[test]
    fn broad_owner_sets_still_cannot_cross_instance_or_project_source_scope() {
        let scope = scope();
        let entries = vec![
            entry(0),
            entry_in(
                1,
                ConnectionKind::Software,
                SourceScope::new(
                    INSTANCE_B,
                    "fixture.owner",
                    "account-a",
                    Some("project-a".into()),
                )
                .unwrap(),
                "same-name",
            ),
            entry_in(
                2,
                ConnectionKind::Software,
                SourceScope::new(
                    INSTANCE_A,
                    "fixture.owner",
                    "account-a",
                    Some("project-b".into()),
                )
                .unwrap(),
                "same-name",
            ),
            entry_in(
                3,
                ConnectionKind::Software,
                SourceScope::new(INSTANCE_A, "fixture.owner", "account-a", None).unwrap(),
                "installation-is-not-a-wildcard",
            ),
        ];
        let access = allow(&scope, &entries, &[]);
        let mut service = CatalogService::new();
        let response = service
            .connections(
                &scope,
                &access,
                snapshot(&entries, &[]),
                &CatalogQuery::default(),
                &page(200),
                NOW,
            )
            .unwrap();
        assert_eq!(response.total, 1);
        assert_eq!(response.items[0].reference, *reference(&entries[0]));
        let mut installation = scope.clone();
        installation.project_id = None;
        let installation_access = allow(&installation, &entries, &[]);
        let response = service
            .connections(
                &installation,
                &installation_access,
                snapshot(&entries, &[]),
                &CatalogQuery::default(),
                &page(200),
                NOW,
            )
            .unwrap();
        assert_eq!(response.total, 1);
        assert_eq!(response.items[0].reference, *reference(&entries[3]));
    }

    #[test]
    fn same_names_and_remote_ids_across_accounts_do_not_merge_or_share_capability_acl() {
        let scope = scope();
        let mut entries = vec![
            entry_in(
                0,
                ConnectionKind::Software,
                source_scope("account-a"),
                "同名软件",
            ),
            entry_in(
                0,
                ConnectionKind::Software,
                source_scope("account-b"),
                "同名软件",
            ),
        ];
        for entry in &mut entries {
            entry.capabilities.push(capability("documents.read"));
        }
        let mut access = allow(&scope, &entries, &[]);
        access.capabilities = HashSet::from([CapabilityId::for_snapshot(
            reference(&entries[0]),
            &entries[0].capabilities[0],
        )]);
        let mut service = CatalogService::new();
        let response = service
            .connections(
                &scope,
                &access,
                snapshot(&entries, &[]),
                &CatalogQuery::default(),
                &page(200),
                NOW,
            )
            .unwrap();
        assert_eq!(response.total, 2);
        assert_ne!(response.items[0].reference, response.items[1].reference);
        assert_ne!(
            response.items[0].source.scope,
            response.items[1].source.scope
        );
        assert_eq!(
            response.items[0].source.source_id,
            response.items[1].source.source_id
        );
        for (index, expected) in [(0, 1), (1, 0)] {
            assert_eq!(
                service
                    .capabilities(
                        &scope,
                        &access,
                        snapshot(&entries, &[]),
                        reference(&entries[index]),
                        &CapabilityQuery::default(),
                        &page(200),
                        NOW,
                    )
                    .unwrap()
                    .total,
                expected
            );
        }
    }

    #[test]
    fn acl_precedes_search_counts_and_paging_even_when_hidden_names_sort_first() {
        let scope = scope();
        let mut entries = vec![entry(0), entry(1)];
        let access = allow(&scope, &entries, &[]);
        for index in 2..302 {
            entries.push(entry_in(
                index,
                ConnectionKind::Software,
                source_scope("account-a"),
                "aaa-private-needle",
            ));
        }
        let mut service = CatalogService::new();
        let view = snapshot(&entries, &[]);
        let first = service
            .connections(
                &scope,
                &access,
                view,
                &CatalogQuery::default(),
                &page(1),
                NOW,
            )
            .unwrap();
        assert_eq!(first.total, 2);
        assert_eq!(first.items.len(), 1);
        assert_eq!(first.items[0].display_name, "visible-0000");
        assert!(first.truncated);
        let query = CatalogQuery {
            search: "private-needle".into(),
            ..Default::default()
        };
        let response = service
            .connections(&scope, &access, view, &query, &page(1), NOW)
            .unwrap();
        assert_eq!(response.total, 0);
        assert!(response.items.is_empty());
        assert_eq!(response.next_cursor, None);
        assert_eq!(
            service.counts(&scope, &access, view, &query, NOW).unwrap(),
            CatalogCounts {
                connections: 0,
                capabilities: 0,
                relations: 0,
            }
        );
        let second = service
            .connections(
                &scope,
                &access,
                view,
                &CatalogQuery::default(),
                &next(&first, &page(1)),
                NOW,
            )
            .unwrap();
        assert_eq!(second.total, 2);
        assert_eq!(second.items[0].display_name, "visible-0001");
        assert!(!second.truncated);
    }

    #[test]
    fn capability_acl_precedes_capability_search_count_and_expansion() {
        let scope = scope();
        let mut entries = vec![entry(0)];
        entries[0].capabilities = (0..205)
            .map(|index| capability(&format!("tool.{index:03}")))
            .chain([capability("private.capability.needle")])
            .collect();
        let mut access = allow(&scope, &entries, &[]);
        access
            .capabilities
            .retain(|id| id.name() != "private.capability.needle");
        let mut service = CatalogService::new();
        let view = snapshot(&entries, &[]);
        let first = service
            .capabilities(
                &scope,
                &access,
                view,
                reference(&entries[0]),
                &CapabilityQuery::default(),
                &CatalogPageRequest::default(),
                NOW,
            )
            .unwrap();
        assert_eq!(first.total, 205);
        assert_eq!(first.items.len(), DEFAULT_CATALOG_PAGE_LIMIT);
        assert!(first.truncated);
        assert!(first.next_cursor.is_some());
        let hidden_search = CapabilityQuery {
            search: "private.capability".into(),
            role: None,
        };
        let hidden = service
            .capabilities(
                &scope,
                &access,
                view,
                reference(&entries[0]),
                &hidden_search,
                &page(200),
                NOW,
            )
            .unwrap();
        assert_eq!(hidden.total, 0);
        assert!(hidden.items.is_empty());
        assert_eq!(hidden.next_cursor, None);
        assert_eq!(
            service
                .counts(&scope, &access, view, &CatalogQuery::default(), NOW)
                .unwrap()
                .capabilities,
            205
        );
        let max = service
            .capabilities(
                &scope,
                &access,
                view,
                reference(&entries[0]),
                &CapabilityQuery::default(),
                &page(200),
                NOW,
            )
            .unwrap();
        assert_eq!(max.items.len(), 200);
        let tail = service
            .capabilities(
                &scope,
                &access,
                view,
                reference(&entries[0]),
                &CapabilityQuery::default(),
                &next(&max, &page(200)),
                NOW,
            )
            .unwrap();
        assert_eq!(tail.items.len(), 5);
        assert!(!tail.truncated);
        assert_eq!(tail.next_cursor, None);
    }

    #[test]
    fn relation_requires_its_own_acl_both_references_and_both_sources() {
        let scope = scope();
        let entries = vec![
            entry_in(
                0,
                ConnectionKind::Software,
                source_scope("software-account"),
                "软件",
            ),
            entry_in(
                1,
                ConnectionKind::ExternalAgent,
                source_scope("ai-account"),
                "私有 AI",
            ),
        ];
        let relations = vec![relation(&entries[1], &entries[0])];
        let mut service = CatalogService::new();
        // 软件 ref、AI ref、软件来源、AI 来源、关系自身：任一缺失都不可见。
        for bits in 0u8..32 {
            let mut access = HostCatalogAccess::deny_all(scope.clone(), 1).unwrap();
            for (index, entry) in entries.iter().enumerate() {
                if bits & (1 << index) != 0 {
                    access.connections.insert(reference(entry).clone());
                }
                if bits & (1 << (index + 2)) != 0 {
                    access
                        .sources
                        .insert(entry.registration.source().scope().clone());
                }
            }
            if bits & 16 != 0 {
                access
                    .relations
                    .insert(RelationId::for_relation(&relations[0]));
            }
            let view = snapshot(&entries, &relations);
            let response = service
                .relations(
                    &scope,
                    &access,
                    view,
                    &RelationQuery::default(),
                    &page(1),
                    NOW,
                )
                .unwrap();
            let expected = usize::from(bits == 31);
            assert_eq!(response.total, expected, "bits={bits:05b}");
            assert_eq!(response.items.len(), expected);
            assert_eq!(response.next_cursor, None);
            assert_eq!(
                service
                    .counts(&scope, &access, view, &CatalogQuery::default(), NOW)
                    .unwrap()
                    .relations,
                expected
            );
            if bits & 5 == 5 {
                let neighborhood = service
                    .neighborhood(
                        &scope,
                        &access,
                        view,
                        reference(&entries[0]),
                        &NeighborhoodQuery::default(),
                        &page(1),
                        NOW,
                    )
                    .unwrap();
                assert_eq!(neighborhood.total_neighbors, expected);
                assert_eq!(neighborhood.relations.total, expected);
            }
        }
    }

    #[test]
    fn hidden_relation_existence_is_indistinguishable_from_no_relation() {
        let scope = scope();
        let entries = vec![
            entry(0),
            entry_in(
                1,
                ConnectionKind::ExternalAgent,
                source_scope("account-a"),
                "AI",
            ),
        ];
        let relations = vec![relation(&entries[1], &entries[0])];
        let access = allow(&scope, &entries, &[]);
        let mut service = CatalogService::new();
        let mut list_responses = Vec::new();
        let mut relation_responses = Vec::new();
        let mut neighborhood_responses = Vec::new();
        for source_relations in [&[][..], relations.as_slice(), relations.as_slice(), &[][..]] {
            let view = snapshot(&entries, source_relations);
            list_responses.push(
                service
                    .connections(
                        &scope,
                        &access,
                        view,
                        &CatalogQuery::default(),
                        &page(1),
                        NOW,
                    )
                    .unwrap(),
            );
            relation_responses.push(
                service
                    .relations(
                        &scope,
                        &access,
                        view,
                        &RelationQuery::default(),
                        &page(1),
                        NOW,
                    )
                    .unwrap(),
            );
            neighborhood_responses.push(
                service
                    .neighborhood(
                        &scope,
                        &access,
                        view,
                        reference(&entries[0]),
                        &NeighborhoodQuery::default(),
                        &page(1),
                        NOW,
                    )
                    .unwrap(),
            );
        }
        for index in 1..4 {
            assert_eq!(list_responses[0], list_responses[index]);
            assert_eq!(relation_responses[0], relation_responses[index]);
            assert_eq!(neighborhood_responses[0], neighborhood_responses[index]);
        }
        assert_eq!(relation_responses[0].total, 0);
        assert_eq!(neighborhood_responses[0].total_neighbors, 0);
        // 持有私有对象 ref 与持有随机不存在 ref 的结果相同。
        let hidden_access = allow(&scope, &entries[..1], &relations);
        for endpoint in [
            reference(&entries[1]).clone(),
            ConnectionRef::new(INSTANCE_A, ConnectionKind::ExternalAgent).unwrap(),
        ] {
            assert_eq!(
                service.relations(
                    &scope,
                    &hidden_access,
                    snapshot(&entries, &relations),
                    &RelationQuery {
                        endpoint: Some(endpoint),
                        kind: None
                    },
                    &page(1),
                    NOW,
                ),
                Err(CatalogError::NotFound)
            );
        }
    }

    #[test]
    fn default_and_max_limits_are_explicit_and_all_connection_pages_are_complete() {
        let scope = scope();
        let entries: Vec<_> = (0..251).map(entry).collect();
        let access = allow(&scope, &entries, &[]);
        let mut service = CatalogService::new();
        let view = snapshot(&entries, &[]);
        let mut request = CatalogPageRequest::default();
        let mut returned = HashSet::new();
        loop {
            let response = service
                .connections(
                    &scope,
                    &access,
                    view,
                    &CatalogQuery::default(),
                    &request,
                    NOW,
                )
                .unwrap();
            assert_eq!(response.limit, 50);
            assert_eq!(response.total, 251);
            assert!(response.items.len() <= 50);
            for item in &response.items {
                assert!(returned.insert(item.reference.clone()));
            }
            if response.truncated {
                request = next(&response, &request);
            } else {
                assert_eq!(response.next_cursor, None);
                break;
            }
        }
        assert_eq!(returned.len(), 251);
        assert_eq!(
            service
                .connections(
                    &scope,
                    &access,
                    view,
                    &CatalogQuery::default(),
                    &page(200),
                    NOW
                )
                .unwrap()
                .items
                .len(),
            200
        );
        for limit in [0, 201, usize::MAX] {
            assert_eq!(
                service.connections(
                    &scope,
                    &access,
                    view,
                    &CatalogQuery::default(),
                    &page(limit),
                    NOW
                ),
                Err(CatalogError::InvalidLimit)
            );
            assert_eq!(
                service.capabilities(
                    &scope,
                    &access,
                    view,
                    reference(&entries[0]),
                    &CapabilityQuery::default(),
                    &page(limit),
                    NOW,
                ),
                Err(CatalogError::InvalidLimit)
            );
            assert_eq!(
                service.relations(
                    &scope,
                    &access,
                    view,
                    &RelationQuery::default(),
                    &page(limit),
                    NOW
                ),
                Err(CatalogError::InvalidLimit)
            );
            assert_eq!(
                service.neighborhood(
                    &scope,
                    &access,
                    view,
                    reference(&entries[0]),
                    &NeighborhoodQuery::default(),
                    &page(limit),
                    NOW,
                ),
                Err(CatalogError::InvalidLimit)
            );
        }
    }

    #[test]
    fn cursor_binds_every_connection_query_field_and_the_raw_limit() {
        let scope = scope();
        let entries: Vec<_> = (0..51).map(entry).collect();
        let access = allow(&scope, &entries, &[]);
        let mut service = CatalogService::new();
        let view = snapshot(&entries, &[]);
        let request = CatalogPageRequest::default();
        let first = service
            .connections(
                &scope,
                &access,
                view,
                &CatalogQuery::default(),
                &request,
                NOW,
            )
            .unwrap();
        let next = next(&first, &request);
        for query in [
            CatalogQuery {
                search: " ".into(),
                ..Default::default()
            },
            CatalogQuery {
                kind: Some(ConnectionKind::Software),
                ..Default::default()
            },
            CatalogQuery {
                source_kind: Some(SourceKind::Software),
                ..Default::default()
            },
            CatalogQuery {
                source_scope: Some(source_scope("account-a")),
                ..Default::default()
            },
        ] {
            // 所有查询都命中相同 51 个对象，但不允许借等价结果忽略完整 query 绑定。
            assert_eq!(
                service.connections(&scope, &access, view, &query, &next, NOW),
                Err(CatalogError::InvalidCursor)
            );
        }
        let mut explicit_default = next.clone();
        explicit_default.limit = Some(50);
        assert_eq!(
            service.connections(
                &scope,
                &access,
                view,
                &CatalogQuery::default(),
                &explicit_default,
                NOW,
            ),
            Err(CatalogError::InvalidCursor)
        );
        assert_eq!(
            service
                .connections(&scope, &access, view, &CatalogQuery::default(), &next, NOW)
                .unwrap()
                .items
                .len(),
            1
        );
    }

    #[test]
    fn cursor_cannot_cross_valid_actor_project_session_instance_or_audience_evaluations() {
        let owner = scope();
        let entries = vec![entry(0), entry(1)];
        let access = allow(&owner, &entries, &[]);
        let mut service = CatalogService::new();
        let view = snapshot(&entries, &[]);
        let first = service
            .connections(
                &owner,
                &access,
                view,
                &CatalogQuery::default(),
                &page(1),
                NOW,
            )
            .unwrap();
        let next = next(&first, &page(1));
        for field in 0..5 {
            let mut caller = owner.clone();
            match field {
                0 => caller.actor_id = "user:bob".into(),
                1 => caller.project_id = Some("project-b".into()),
                2 => caller.session_id = Some("session-b".into()),
                3 => caller.instance_id = INSTANCE_B.into(),
                _ => caller.audience = CatalogAudience::Agent,
            }
            let other_access = allow(&caller, &entries, &[]);
            assert_eq!(
                service.connections(
                    &caller,
                    &other_access,
                    view,
                    &CatalogQuery::default(),
                    &next,
                    NOW
                ),
                Err(CatalogError::InvalidCursor)
            );
        }
    }

    #[test]
    fn cursor_expiry_restart_forgery_and_acl_revision_changes_fail_closed() {
        let scope = scope();
        let entries = vec![entry(0), entry(1)];
        let access = allow(&scope, &entries, &[]);
        let mut service = CatalogService::new();
        let view = snapshot(&entries, &[]);
        let first = service
            .connections(
                &scope,
                &access,
                view,
                &CatalogQuery::default(),
                &page(1),
                NOW,
            )
            .unwrap();
        let next = next(&first, &page(1));
        let revised = allow_at_revision(&scope, &entries, &[], 2);
        assert_eq!(
            service.connections(&scope, &revised, view, &CatalogQuery::default(), &next, NOW),
            Err(CatalogError::InvalidCursor)
        );
        assert_eq!(
            CatalogService::new().connections(
                &scope,
                &access,
                view,
                &CatalogQuery::default(),
                &next,
                NOW,
            ),
            Err(CatalogError::InvalidCursor)
        );
        for token in ["private-token-canary".into(), Uuid::new_v4().to_string()] {
            let forged = CatalogPageRequest {
                limit: Some(1),
                cursor: Some(token),
            };
            let error = service
                .connections(
                    &scope,
                    &access,
                    view,
                    &CatalogQuery::default(),
                    &forged,
                    NOW,
                )
                .unwrap_err();
            assert_eq!(error, CatalogError::InvalidCursor);
            assert!(!error.to_string().contains("canary"));
        }
        assert!(service
            .connections(
                &scope,
                &access,
                view,
                &CatalogQuery::default(),
                &next,
                NOW + CATALOG_CURSOR_TTL_MS - 1,
            )
            .is_ok());
        assert_eq!(
            service.connections(
                &scope,
                &access,
                view,
                &CatalogQuery::default(),
                &next,
                NOW + CATALOG_CURSOR_TTL_MS,
            ),
            Err(CatalogError::InvalidCursor)
        );
    }

    #[test]
    fn visible_snapshot_change_or_revocation_invalidates_an_existing_cursor() {
        let scope = scope();
        let entries = vec![entry(0), entry(1), entry(2)];
        let access = allow(&scope, &entries, &[]);
        let mut service = CatalogService::new();
        let first = service
            .connections(
                &scope,
                &access,
                snapshot(&entries[..2], &[]),
                &CatalogQuery::default(),
                &page(1),
                NOW,
            )
            .unwrap();
        let next = next(&first, &page(1));
        assert_eq!(
            service.connections(
                &scope,
                &access,
                snapshot(&entries, &[]),
                &CatalogQuery::default(),
                &next,
                NOW,
            ),
            Err(CatalogError::InvalidCursor)
        );
        let revoked = allow(&scope, &entries[..1], &[]);
        assert_eq!(
            service.connections(
                &scope,
                &revoked,
                snapshot(&entries, &[]),
                &CatalogQuery::default(),
                &next,
                NOW,
            ),
            Err(CatalogError::InvalidCursor)
        );
        let mut stale = entries[..2].to_vec();
        stale[0].metadata.freshness = CatalogFreshness::Stale;
        assert_eq!(
            service.connections(
                &scope,
                &access,
                snapshot(&stale, &[]),
                &CatalogQuery::default(),
                &next,
                NOW,
            ),
            Err(CatalogError::InvalidCursor)
        );
    }

    #[test]
    fn hidden_same_scope_mutations_and_aggregate_source_revision_do_not_change_responses_or_cursors(
    ) {
        let scope = scope();
        let mut entries = vec![entry(0), entry(1), entry(2)];
        entries[0]
            .capabilities
            .push(capability("visible.capability"));
        let access = allow(&scope, &entries, &[]);
        let mut service = CatalogService::new();
        let first = service
            .connections(
                &scope,
                &access,
                snapshot(&entries, &[]),
                &CatalogQuery::default(),
                &page(1),
                NOW,
            )
            .unwrap();
        let next = next(&first, &page(1));
        let expected_tail = service
            .connections(
                &scope,
                &access,
                snapshot(&entries, &[]),
                &CatalogQuery::default(),
                &next,
                NOW,
            )
            .unwrap();
        let mut changed = entries.clone();
        changed[0]
            .capabilities
            .push(capability("hidden.capability.canary"));
        let hidden_agent = entry_in(
            3,
            ConnectionKind::ExternalAgent,
            source_scope("account-a"),
            "hidden-agent-canary",
        );
        let relations = vec![relation(&hidden_agent, &changed[0])];
        changed.push(hidden_agent.clone());
        // 连隐藏坏记录的冲突都不能影响查询；与可见数据的输入顺序同样无关。
        changed.push(hidden_agent);
        changed.reverse();
        for entry in &mut changed {
            let mut registration = serde_json::to_value(&entry.registration).unwrap();
            registration["source"]["revision"] =
                json!("global-source-revision-bumped-for-hidden-object");
            // 权威登记同步该全局来源revision时也会递增登记revision。
            // 这同样不能经目录摘要或游标泄露无关隐藏对象的变化。
            registration["revision"] = json!(entry.registration.revision() + 1);
            entry.registration = serde_json::from_value(registration).unwrap();
        }
        for (current_entries, current_relations) in [
            (changed.as_slice(), relations.as_slice()),
            (entries.as_slice(), &[][..]),
            (changed.as_slice(), &[][..]),
        ] {
            let view = snapshot(current_entries, current_relations);
            let response = service
                .connections(
                    &scope,
                    &access,
                    view,
                    &CatalogQuery::default(),
                    &page(1),
                    NOW,
                )
                .unwrap();
            assert_eq!(response, first);
            let tail = service
                .connections(&scope, &access, view, &CatalogQuery::default(), &next, NOW)
                .unwrap();
            assert_eq!(tail, expected_tail);
            let encoded = serde_json::to_string(&response).unwrap();
            assert!(!encoded.contains("hidden"));
            assert!(!encoded.contains("global-source-revision"));
            assert_eq!(
                service
                    .counts(&scope, &access, view, &CatalogQuery::default(), NOW)
                    .unwrap(),
                CatalogCounts {
                    connections: 3,
                    capabilities: 1,
                    relations: 0
                }
            );
        }
    }

    #[test]
    fn bounded_literal_search_handles_long_unicode_names_without_regex_or_unbounded_input() {
        let scope = scope();
        let name = format!("{}Z", "长".repeat(170));
        assert_eq!(name.len(), 511);
        let entries = vec![entry_in(
            0,
            ConnectionKind::Software,
            source_scope("account-a"),
            &name,
        )];
        let access = allow(&scope, &entries, &[]);
        let mut service = CatalogService::new();
        let view = snapshot(&entries, &[]);
        for (search, total) in [("z".into(), 1), ("长".repeat(85), 1), (".*".into(), 0)] {
            let query = CatalogQuery {
                search,
                ..Default::default()
            };
            let response = service
                .connections(&scope, &access, view, &query, &page(1), NOW)
                .unwrap();
            assert_eq!(response.total, total);
        }
        for search in [
            "x".repeat(257),
            "长".repeat(86),
            "name\n".into(),
            "\0".into(),
        ] {
            let query = CatalogQuery {
                search: search.clone(),
                ..Default::default()
            };
            assert_eq!(
                service.connections(&scope, &access, view, &query, &page(1), NOW),
                Err(CatalogError::InvalidSearch)
            );
            assert_eq!(
                service.capabilities(
                    &scope,
                    &access,
                    view,
                    reference(&entries[0]),
                    &CapabilityQuery { search, role: None },
                    &page(1),
                    NOW,
                ),
                Err(CatalogError::InvalidSearch)
            );
        }
    }

    #[test]
    fn one_hop_neighborhood_limits_both_edges_and_neighbors_and_never_expands_hidden_nodes() {
        let scope = scope();
        let mut entries = vec![entry(0), entry(1)];
        for index in 2..243 {
            entries.push(entry_in(
                index,
                ConnectionKind::ExternalAgent,
                source_scope("account-a"),
                &format!("AI-{index:04}"),
            ));
        }
        let mut relations: Vec<_> = entries[2..]
            .iter()
            .map(|agent| relation(agent, &entries[0]))
            .collect();
        // 第二条同邻居边不能导致无界内嵌关系；第二跳软件不得被展开。
        relations.push(relation(&entries[2], &entries[0]));
        relations.push(relation(&entries[2], &entries[1]));
        let mut access = allow(&scope, &entries, &relations);
        for index in 243..263 {
            let hidden = entry_in(
                index,
                ConnectionKind::ExternalAgent,
                source_scope("account-a"),
                "private AI",
            );
            let edge = relation(&hidden, &entries[0]);
            access.relations.insert(RelationId::for_relation(&edge));
            entries.push(hidden);
            relations.push(edge);
        }
        let mut service = CatalogService::new();
        let view = snapshot(&entries, &relations);
        let default = service
            .neighborhood(
                &scope,
                &access,
                view,
                reference(&entries[0]),
                &NeighborhoodQuery::default(),
                &CatalogPageRequest::default(),
                NOW,
            )
            .unwrap();
        assert_eq!(default.relations.items.len(), 50);
        assert!(default.neighbors.len() <= 50);
        assert_eq!(default.total_neighbors, 241);
        assert_eq!(default.relations.total, 242);
        assert!(default.relations.truncated);
        let mut request = page(200);
        let mut neighbors = HashSet::new();
        let mut edges = HashSet::new();
        loop {
            let response = service
                .neighborhood(
                    &scope,
                    &access,
                    view,
                    reference(&entries[0]),
                    &NeighborhoodQuery::default(),
                    &request,
                    NOW,
                )
                .unwrap();
            assert!(response.neighbors.len() <= 200);
            assert!(response.relations.items.len() <= 200);
            assert_eq!(response.total_neighbors, 241);
            assert_eq!(response.relations.total, 242);
            for neighbor in &response.neighbors {
                assert_eq!(neighbor.reference.kind(), ConnectionKind::ExternalAgent);
                assert!(!neighbor.display_name.contains("private"));
                neighbors.insert(neighbor.reference.clone());
            }
            for edge in &response.relations.items {
                assert!(edges.insert(edge.id().to_owned()));
            }
            if !response.relations.truncated {
                assert_eq!(response.relations.next_cursor, None);
                break;
            }
            request = next(&response.relations, &request);
        }
        assert_eq!(neighbors.len(), 241);
        assert_eq!(edges.len(), 242);
        assert!(!neighbors.contains(reference(&entries[1])));
        let filtered = service
            .neighborhood(
                &scope,
                &access,
                view,
                reference(&entries[0]),
                &NeighborhoodQuery {
                    neighbors: CatalogQuery {
                        kind: Some(ConnectionKind::Device),
                        ..Default::default()
                    },
                    relation_kind: None,
                },
                &page(200),
                NOW,
            )
            .unwrap();
        assert_eq!(filtered.total_neighbors, 0);
        assert_eq!(filtered.relations.total, 0);
        assert!(filtered.neighbors.is_empty());
    }

    #[test]
    fn capability_relation_and_neighborhood_cursors_bind_targets_filters_and_query_family() {
        let scope = scope();
        let mut entries = vec![entry(0), entry(1)];
        for entry in &mut entries {
            entry.capabilities = vec![capability("tool.a"), capability("tool.b")];
        }
        for index in 2..5 {
            entries.push(entry_in(
                index,
                ConnectionKind::ExternalAgent,
                source_scope("account-a"),
                "AI",
            ));
        }
        let relations: Vec<_> = entries[2..]
            .iter()
            .map(|agent| relation(agent, &entries[0]))
            .collect();
        let access = allow(&scope, &entries, &relations);
        let mut service = CatalogService::new();
        let view = snapshot(&entries, &relations);
        let caps = service
            .capabilities(
                &scope,
                &access,
                view,
                reference(&entries[0]),
                &CapabilityQuery::default(),
                &page(1),
                NOW,
            )
            .unwrap();
        let cap_next = next(&caps, &page(1));
        assert_eq!(
            service.capabilities(
                &scope,
                &access,
                view,
                reference(&entries[1]),
                &CapabilityQuery::default(),
                &cap_next,
                NOW,
            ),
            Err(CatalogError::InvalidCursor)
        );
        for query in [
            CapabilityQuery {
                search: " ".into(),
                role: None,
            },
            CapabilityQuery {
                search: String::new(),
                role: Some(CapabilityRole::ToolProvider),
            },
        ] {
            assert_eq!(
                service.capabilities(
                    &scope,
                    &access,
                    view,
                    reference(&entries[0]),
                    &query,
                    &cap_next,
                    NOW,
                ),
                Err(CatalogError::InvalidCursor)
            );
        }
        let edge_page = service
            .relations(
                &scope,
                &access,
                view,
                &RelationQuery::default(),
                &page(1),
                NOW,
            )
            .unwrap();
        let edge_next = next(&edge_page, &page(1));
        for query in [
            RelationQuery {
                endpoint: Some(reference(&entries[0]).clone()),
                kind: None,
            },
            RelationQuery {
                endpoint: None,
                kind: Some(ConnectionRelationKind::EmbeddedIn),
            },
        ] {
            assert_eq!(
                service.relations(&scope, &access, view, &query, &edge_next, NOW),
                Err(CatalogError::InvalidCursor)
            );
        }
        assert_eq!(
            service.connections(
                &scope,
                &access,
                view,
                &CatalogQuery::default(),
                &edge_next,
                NOW
            ),
            Err(CatalogError::InvalidCursor)
        );
        assert_eq!(
            service.neighborhood(
                &scope,
                &access,
                view,
                reference(&entries[0]),
                &NeighborhoodQuery::default(),
                &edge_next,
                NOW,
            ),
            Err(CatalogError::InvalidCursor)
        );
        let neighborhood = service
            .neighborhood(
                &scope,
                &access,
                view,
                reference(&entries[0]),
                &NeighborhoodQuery::default(),
                &page(1),
                NOW,
            )
            .unwrap();
        let neighbor_next = next(&neighborhood.relations, &page(1));
        for query in [
            NeighborhoodQuery {
                neighbors: CatalogQuery {
                    search: " ".into(),
                    ..Default::default()
                },
                relation_kind: None,
            },
            NeighborhoodQuery {
                neighbors: CatalogQuery::default(),
                relation_kind: Some(ConnectionRelationKind::EmbeddedIn),
            },
        ] {
            assert_eq!(
                service.neighborhood(
                    &scope,
                    &access,
                    view,
                    reference(&entries[0]),
                    &query,
                    &neighbor_next,
                    NOW,
                ),
                Err(CatalogError::InvalidCursor)
            );
        }
        assert_eq!(
            service.neighborhood(
                &scope,
                &access,
                view,
                reference(&entries[2]),
                &NeighborhoodQuery::default(),
                &neighbor_next,
                NOW,
            ),
            Err(CatalogError::InvalidCursor)
        );
    }

    #[test]
    fn read_visibility_with_empty_execution_grants_never_becomes_capability_authorization() {
        let scope = scope();
        let mut registry = RegistrationRegistry::new(INSTANCE_A).unwrap();
        let source = entry(0).registration.source().clone();
        let target = registry
            .register(ConnectionKind::Software, source, "软件")
            .unwrap();
        registry.set_enabled(&target, true).unwrap();
        let mut ready = capability("documents.read");
        ready.set_availability(CapabilityAvailability::Available);
        for claim in [
            CapabilityEvidenceClaim::Detection,
            CapabilityEvidenceClaim::Contract,
        ] {
            let evidence = HostCapabilityEvidence::new(
                ready.evidence_scope(),
                "fixture-evidence".into(),
                EvidenceOrigin::IsolatedFixture,
                claim,
                NOW,
                NOW + CATALOG_CURSOR_TTL_MS,
            )
            .unwrap();
            match claim {
                CapabilityEvidenceClaim::Detection => {
                    ready.record_detection(evidence, NOW).unwrap()
                }
                _ => ready.record_verification(evidence, NOW).unwrap(),
            }
        }
        let entries = vec![CatalogEntry::new(
            registry.get(&target).unwrap().clone(),
            vec![ready],
            CatalogMetadata::default(),
        )];
        // 没有执行 grant 或 HostCapabilityAuthorization；只有独立的目录读取评估。
        let access = allow(&scope, &entries, &[]);
        let mut service = CatalogService::new();
        let response = service
            .capabilities(
                &scope,
                &access,
                snapshot(&entries, &[]),
                &target,
                &CapabilityQuery::default(),
                &page(1),
                NOW,
            )
            .unwrap();
        assert!(entries[0].registration.is_grant_eligible());
        assert_eq!(response.total, 1);
        assert_eq!(
            response.items[0].availability,
            CapabilityAvailability::Available
        );
        assert_eq!(response.items[0].verification, VerificationState::Verified);
        assert_eq!(
            response.items[0].authorization,
            CapabilityAuthorizationState::Unknown
        );
        let mut agent = scope.clone();
        agent.audience = CatalogAudience::Agent;
        assert_eq!(
            service.capabilities(
                &agent,
                &access,
                snapshot(&entries, &[]),
                &target,
                &CapabilityQuery::default(),
                &page(1),
                NOW,
            ),
            Err(CatalogError::ScopeMismatch)
        );
        let agent_access = HostCatalogAccess::deny_all(agent.clone(), 1).unwrap();
        assert_eq!(
            service.capabilities(
                &agent,
                &agent_access,
                snapshot(&entries, &[]),
                &target,
                &CapabilityQuery::default(),
                &page(1),
                NOW,
            ),
            Err(CatalogError::NotFound)
        );
    }

    #[test]
    fn capability_revision_and_registration_generation_do_not_inherit_read_acl() {
        let scope = scope();
        let mut entries = vec![entry(0)];
        entries[0].capabilities.push(capability("tool.read"));
        let access = allow(&scope, &entries, &[]);
        let mut revised = descriptor("tool.read");
        revised.revision = 2;
        entries[0].capabilities =
            vec![CapabilitySnapshot::from_declaration("new.binding".into(), revised).unwrap()];
        let mut service = CatalogService::new();
        assert_eq!(
            service
                .capabilities(
                    &scope,
                    &access,
                    snapshot(&entries, &[]),
                    reference(&entries[0]),
                    &CapabilityQuery::default(),
                    &page(1),
                    NOW,
                )
                .unwrap()
                .total,
            0
        );
        let mut changed = serde_json::to_value(&entries[0].registration).unwrap();
        changed["reference"]["generation"] = json!(Uuid::new_v4().to_string());
        entries[0].registration = serde_json::from_value(changed).unwrap();
        assert_eq!(
            service.connection(
                &scope,
                &access,
                snapshot(&entries, &[]),
                reference(&entries[0]),
                NOW
            ),
            Err(CatalogError::NotFound)
        );
    }

    #[test]
    fn even_management_output_omits_credential_metadata_and_host_evidence_material() {
        const CANARY: &str = "private-material-canary-92184";
        let scope = scope();
        let mut entries = vec![entry(0)];
        let mut declaration = descriptor("documents.read");
        declaration.data_destinations = BTreeSet::from([DataDestination::Remote {
            endpoint_ref: CANARY.into(),
            account_ref: CANARY.into(),
        }]);
        let mut safe = CapabilitySnapshot::from_declaration(CANARY.into(), declaration).unwrap();
        let evidence = HostCapabilityEvidence::new(
            safe.evidence_scope(),
            CANARY.into(),
            EvidenceOrigin::IsolatedFixture,
            CapabilityEvidenceClaim::Contract,
            NOW,
            NOW + CATALOG_CURSOR_TTL_MS,
        )
        .unwrap();
        safe.record_verification(evidence, NOW).unwrap();
        entries[0].capabilities.push(safe);
        let access = allow(&scope, &entries, &[]);
        let mut service = CatalogService::new();
        let response = service
            .capabilities(
                &scope,
                &access,
                snapshot(&entries, &[]),
                reference(&entries[0]),
                &CapabilityQuery::default(),
                &page(1),
                NOW,
            )
            .unwrap();
        let encoded = serde_json::to_string(&response).unwrap();
        for forbidden in [
            CANARY,
            "credentials",
            "bindingKey",
            "endpointRef",
            "accountRef",
            "evidence",
            "Schema",
        ] {
            assert!(!encoded.contains(forbidden), "不应输出 {forbidden}");
        }
        assert_eq!(response.items[0].verification, VerificationState::Verified);
        assert_eq!(
            response.items[0].authorization,
            CapabilityAuthorizationState::Unknown
        );
        let mut remote = serde_json::to_value(descriptor("tool.read")).unwrap();
        remote["verified"] = json!(true);
        assert!(CapabilityDescriptor::from_json(&serde_json::to_vec(&remote).unwrap()).is_err());
        let untrusted = CapabilitySnapshot::from_declaration(
            "new.remote.declaration".into(),
            descriptor("tool.read"),
        )
        .unwrap();
        assert_eq!(
            untrusted
                .projection(None, "catalog.visibility.only", NOW)
                .verification,
            VerificationState::Unverified
        );
    }

    #[test]
    fn visible_duplicates_fail_but_hidden_duplicates_do_not_change_response_status() {
        let scope = scope();
        let visible = entry(0);
        let hidden = entry(1);
        let mut entries = vec![visible.clone(), hidden.clone(), hidden];
        let access = allow(&scope, &entries[..1], &[]);
        let mut service = CatalogService::new();
        assert_eq!(
            service
                .connections(
                    &scope,
                    &access,
                    snapshot(&entries, &[]),
                    &CatalogQuery::default(),
                    &page(1),
                    NOW,
                )
                .unwrap()
                .total,
            1
        );
        entries.push(visible);
        assert_eq!(
            service.connections(
                &scope,
                &access,
                snapshot(&entries, &[]),
                &CatalogQuery::default(),
                &page(1),
                NOW,
            ),
            Err(CatalogError::InvalidSnapshot)
        );
    }

    #[test]
    fn cursor_state_is_bounded_and_eviction_never_falls_back_to_a_client_offset() {
        let owner = scope();
        let entries = vec![entry(0), entry(1)];
        let access = allow(&owner, &entries, &[]);
        let mut service = CatalogService::new();
        let view = snapshot(&entries, &[]);
        let first = service
            .connections(
                &owner,
                &access,
                view,
                &CatalogQuery::default(),
                &page(1),
                NOW,
            )
            .unwrap();
        let first_next = next(&first, &page(1));
        for index in 0..MAX_CATALOG_CURSOR_STATES {
            let mut caller = owner.clone();
            caller.actor_id = format!("user:{index}");
            let other_access = allow(&caller, &entries, &[]);
            service
                .connections(
                    &caller,
                    &other_access,
                    view,
                    &CatalogQuery::default(),
                    &page(1),
                    NOW,
                )
                .unwrap();
            assert!(service.cursors.len() <= MAX_CATALOG_CURSOR_STATES);
        }
        assert_eq!(service.cursors.len(), MAX_CATALOG_CURSOR_STATES);
        assert_eq!(
            service.connections(
                &owner,
                &access,
                view,
                &CatalogQuery::default(),
                &first_next,
                NOW
            ),
            Err(CatalogError::InvalidCursor)
        );
    }
}
