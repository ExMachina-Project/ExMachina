//! 显式连接关系与来源编辑委托；不会修改来源配置、设备配对、Agent 绑定或 grant。
//!
//! 单个 Core 对同一 root 持有一个 service，所有调用者共享其 clone/Arc；Mutex 覆盖
//! 关系 CAS、克隆、FsDb 单文档提交及内存替换。写盘失败不发布内存值。此处不是
//! 跨进程事务或掉电持久性协议，存储保证沿用 FsDb；不得并行打开独立写者。
//!
//! 调用方须在整个调用期间提供受锁保护的**当前** RegistrationRegistry，不得用
//! 排队时的旧副本。查询先验证关系端点代际，删除/重登记不会令旧关系重新出现。
//! 此服务是管理/目录内部面，不是 Agent 查询接口：鉴权、关系自身和两端可见性、
//! ACL 后计数/分页由目录层负责，通过关系验证绝不意味着获准执行。

use super::identity::{
    ConnectionRef, ConnectionRegistration, ConnectionRelation, ConnectionRelationKind,
    RegistrationRegistry, RegistrationState, SourceKind, SourceRef,
};
use crate::fsdb::FsDb;
use anyhow::{ensure, Context, Result};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::io::Read;
use std::path::Path;
use std::sync::Arc;

pub const CONNECTION_RELATION_SCHEMA_VERSION: u16 = 1;
const COLLECTION: &str = "connection_relations";
const DOCUMENT_ID: &str = "state";
const MAX_RELATION_DOCUMENT_BYTES: usize = 16 * 1024 * 1024;

/// 仅供原来源服务分派的编辑目标，不是 URL、执行命令、配置正文或授权票据。
///
/// sourceKind 分别委托 Nexus、MCP、插件、设备服务和本地软件登记管理面。它与
/// projection 的来源编辑元数据使用同一来源 ID，不虚构页面参数或新的保存 API。
/// 账户/项目/子资源上下文仍取所在端点的完整 SourceRef，由原服务重验权限。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EditTarget {
    source_kind: SourceKind,
    source_id: String,
}

impl EditTarget {
    fn from_source(source: &SourceRef) -> Self {
        Self {
            source_kind: source.kind(),
            source_id: source.source_id().to_owned(),
        }
    }

    pub fn source_kind(&self) -> SourceKind {
        self.source_kind
    }

    pub fn source_id(&self) -> &str {
        &self.source_id
    }
}

/// 为当前登记返回来源编辑委托；不会选中全局目标或调用原来源的编辑/配对动作。
pub fn source_edit_target(
    registry: &RegistrationRegistry,
    reference: &ConnectionRef,
) -> Result<EditTarget> {
    Ok(EditTarget::from_source(
        current_registration(registry, reference)?.source(),
    ))
}

/// 从当前 registry 生成的展示快照。显示名之外保留完整身份及来源域区分同名对象。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RelationEndpoint {
    reference: ConnectionRef,
    display_name: String,
    source: SourceRef,
    edit_target: EditTarget,
}

impl RelationEndpoint {
    fn from_registration(registration: &ConnectionRegistration) -> Self {
        Self {
            reference: registration.reference().clone(),
            display_name: registration.display_name().to_owned(),
            source: registration.source().clone(),
            edit_target: EditTarget::from_source(registration.source()),
        }
    }

    pub fn reference(&self) -> &ConnectionRef {
        &self.reference
    }

    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    pub fn source(&self) -> &SourceRef {
        &self.source
    }

    pub fn edit_target(&self) -> &EditTarget {
        &self.edit_target
    }
}

/// 不持久复制端点名称或来源配置；每次读取由当前登记重新投影。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionRelationView {
    relation: ConnectionRelation,
    from: RelationEndpoint,
    to: RelationEndpoint,
}

impl ConnectionRelationView {
    fn current(registry: &RegistrationRegistry, relation: &ConnectionRelation) -> Option<Self> {
        registry.validate_relation(relation).ok()?;
        Some(Self {
            relation: relation.clone(),
            from: RelationEndpoint::from_registration(registry.get(relation.from())?),
            to: RelationEndpoint::from_registration(registry.get(relation.to())?),
        })
    }

    pub fn relation(&self) -> &ConnectionRelation {
        &self.relation
    }

    pub fn from(&self) -> &RelationEndpoint {
        &self.from
    }

    pub fn to(&self) -> &RelationEndpoint {
        &self.to
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RelationRecord {
    relation: ConnectionRelation,
    removed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", try_from = "RelationDocumentData")]
struct RelationDocument {
    schema_version: u16,
    instance_id: Option<String>,
    relations: Vec<RelationRecord>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RelationDocumentData {
    schema_version: u16,
    instance_id: Option<String>,
    relations: Vec<RelationRecord>,
}

impl TryFrom<RelationDocumentData> for RelationDocument {
    type Error = anyhow::Error;

    fn try_from(value: RelationDocumentData) -> Result<Self> {
        let document = Self {
            schema_version: value.schema_version,
            instance_id: value.instance_id,
            relations: value.relations,
        };
        document.validate()?;
        Ok(document)
    }
}

impl RelationDocument {
    fn empty() -> Self {
        Self {
            schema_version: CONNECTION_RELATION_SCHEMA_VERSION,
            instance_id: None,
            relations: Vec::new(),
        }
    }

    fn validate(&self) -> Result<()> {
        ensure!(
            self.schema_version == CONNECTION_RELATION_SCHEMA_VERSION,
            "不支持的连接关系 schemaVersion"
        );
        if let Some(instance) = &self.instance_id {
            // 复用身份域校验，不把文件路径或 URL 当作安装实例。
            RegistrationRegistry::new(instance.clone())?;
        }
        ensure!(
            self.relations.is_empty() || self.instance_id.is_some(),
            "关系文档缺少所属安装实例"
        );
        let mut ids = HashSet::new();
        let mut live_edges = HashSet::new();
        for record in &self.relations {
            let relation = &record.relation;
            ensure!(ids.insert(relation.id()), "关系 ID 重复");
            ensure!(
                self.instance_id.as_deref() == Some(relation.from().instance_id())
                    && self.instance_id.as_deref() == Some(relation.to().instance_id()),
                "关系文档包含其他安装实例的端点"
            );
            if record.removed {
                ensure!(relation.revision() > 1, "删除关系必须递增 revision");
            } else {
                let kind = match relation.kind() {
                    ConnectionRelationKind::EmbeddedIn => 0,
                    ConnectionRelationKind::ProvidedBy => 1,
                };
                ensure!(
                    live_edges.insert((kind, relation.from(), relation.to())),
                    "相同类型和端点的展示关系已存在"
                );
            }
        }
        Ok(())
    }

    fn validate_instance(&self, registry: &RegistrationRegistry) -> Result<()> {
        if let Some(instance) = &self.instance_id {
            ensure!(
                instance == registry.instance_id(),
                "关系文档属于其他安装实例"
            );
        }
        Ok(())
    }
}

struct RelationServiceInner {
    db: FsDb,
    document: Mutex<RelationDocument>,
}

/// clone 共享同一锁和已提交值。Core 须保证同一 root 只有一个独立 service 写者。
#[derive(Clone)]
pub struct ConnectionRelationService {
    inner: Arc<RelationServiceInner>,
}

impl ConnectionRelationService {
    /// 打开既有单文档；缺失才视为空，损坏/未知版本不得静默重置。
    /// 不需要原来源句柄，不启动进程、不探测网络，也不恢复任何执行授权。
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        let db = FsDb::open(root)?;
        let document = db
            .get::<RelationDocument>(COLLECTION, DOCUMENT_ID)?
            .unwrap_or_else(RelationDocument::empty);
        document.validate()?;
        Ok(Self {
            inner: Arc::new(RelationServiceInner {
                db,
                document: Mutex::new(document),
            }),
        })
    }

    /// Read the existing relation document without creating a database, collection directory,
    /// or any file. A missing document is an empty relation set; a present malformed document
    /// is an error and must not be treated as empty.
    pub fn read_current_relations(
        root: impl AsRef<Path>,
        registry: &RegistrationRegistry,
    ) -> Result<Vec<ConnectionRelationView>> {
        let path = root
            .as_ref()
            .join(COLLECTION)
            .join(format!("{DOCUMENT_ID}.json"));
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        let file = match options.open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        anyhow::ensure!(file.metadata()?.is_file(), "关系文档不是普通文件");
        anyhow::ensure!(
            file.metadata()?.len() <= MAX_RELATION_DOCUMENT_BYTES as u64,
            "关系文档超过读取上限"
        );
        let mut bytes = Vec::new();
        file.take(MAX_RELATION_DOCUMENT_BYTES as u64 + 1)
            .read_to_end(&mut bytes)?;
        anyhow::ensure!(
            bytes.len() <= MAX_RELATION_DOCUMENT_BYTES,
            "关系文档超过读取上限"
        );
        let document: RelationDocument = serde_json::from_slice(&bytes)?;
        document.validate_instance(registry)?;
        Ok(document
            .relations
            .iter()
            .filter(|record| !record.removed)
            .filter_map(|record| ConnectionRelationView::current(registry, &record.relation))
            .collect())
    }

    /// 创建用 expected_revision=0 且候选 revision=1；更新必须提交当前 revision+1。
    /// 候选由 identity 的 new/revised 或严格解码产生，版本比较与落盘在同一锁内。
    /// 已删除 ID 不可复活；重新关联须显式建立新的关系 ID。
    pub fn upsert(
        &self,
        registry: &RegistrationRegistry,
        relation: ConnectionRelation,
        expected_revision: u64,
    ) -> Result<ConnectionRelation> {
        let mut document = self.inner.document.lock();
        document.validate_instance(registry)?;
        registry.validate_relation(&relation)?;
        let position = document
            .relations
            .iter()
            .position(|record| record.relation.id() == relation.id());
        if let Some(index) = position {
            let current = &document.relations[index];
            ensure!(!current.removed, "已删除关系不能复活，请显式创建新关系");
            ensure!(
                current.relation.revision() == expected_revision,
                "关系 revision 冲突"
            );
            let revised = current.relation.revised(
                expected_revision,
                relation.kind(),
                relation.from().clone(),
                relation.to().clone(),
            )?;
            ensure!(revised == relation, "更新候选必须恰好递增一个 revision");
        } else {
            ensure!(expected_revision == 0, "关系不存在或创建 revision 冲突");
            ensure!(relation.revision() == 1, "新关系 revision 必须为 1");
        }
        let mut next = document.clone();
        next.instance_id = Some(registry.instance_id().to_owned());
        let record = RelationRecord {
            relation: relation.clone(),
            removed: false,
        };
        if let Some(index) = position {
            next.relations[index] = record;
        } else {
            next.relations.push(record);
        }
        next.validate()?;
        self.inner.db.put(COLLECTION, DOCUMENT_ID, &next)?;
        *document = next;
        Ok(relation)
    }

    /// 只移除展示关系并持久保留递增版本的 tombstone，返回删除后的 revision。
    /// 当前端点失效时拒绝此管理动作；读取仍会隐藏失效关系，失效清理由目录层负责。
    pub fn remove(
        &self,
        registry: &RegistrationRegistry,
        relation_id: &str,
        expected_revision: u64,
    ) -> Result<u64> {
        let mut document = self.inner.document.lock();
        document.validate_instance(registry)?;
        let index = document
            .relations
            .iter()
            .position(|record| record.relation.id() == relation_id)
            .context("关系不存在")?;
        let current = &document.relations[index];
        ensure!(!current.removed, "关系已删除");
        ensure!(
            current.relation.revision() == expected_revision,
            "关系 revision 冲突"
        );
        registry.validate_relation(&current.relation)?;
        let removed = current.relation.revised(
            expected_revision,
            current.relation.kind(),
            current.relation.from().clone(),
            current.relation.to().clone(),
        )?;
        let revision = removed.revision();
        let mut next = document.clone();
        next.relations[index] = RelationRecord {
            relation: removed,
            removed: true,
        };
        next.validate()?;
        self.inner.db.put(COLLECTION, DOCUMENT_ID, &next)?;
        *document = next;
        Ok(revision)
    }

    /// 已删除或任一端点不再是当前代际时不返回关系，不按名称重绑定，也不写盘。
    /// 调用者须在返回客户端前分别检查关系及两端的可见性。
    pub fn get(
        &self,
        registry: &RegistrationRegistry,
        relation_id: &str,
    ) -> Result<Option<ConnectionRelationView>> {
        let document = self.inner.document.lock();
        document.validate_instance(registry)?;
        Ok(document
            .relations
            .iter()
            .find(|record| record.relation.id() == relation_id && !record.removed)
            .and_then(|record| ConnectionRelationView::current(registry, &record.relation)))
    }

    /// 仅供后续目录层做 ACL 后分页，不是可直接暴露的 Agent 全量目录/计数 API。
    pub fn current_relations(
        &self,
        registry: &RegistrationRegistry,
    ) -> Result<Vec<ConnectionRelationView>> {
        let document = self.inner.document.lock();
        document.validate_instance(registry)?;
        Ok(document
            .relations
            .iter()
            .filter(|record| !record.removed)
            .filter_map(|record| ConnectionRelationView::current(registry, &record.relation))
            .collect())
    }
}

fn current_registration<'a>(
    registry: &'a RegistrationRegistry,
    reference: &ConnectionRef,
) -> Result<&'a ConnectionRegistration> {
    ensure!(
        reference.instance_id() == registry.instance_id(),
        "连接属于其他安装实例"
    );
    let registration = registry.get(reference).context("连接登记不存在")?;
    // Registry 的构造/恢复不允许尚未删除的旧代际之后出现新代际。
    ensure!(
        registration.state() != RegistrationState::Deleted,
        "连接登记已失效"
    );
    Ok(registration)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connections::identity::{
        ConnectionKind, RegistrationBinding, SourceLifecycle, SourceScope,
    };
    use serde_json::{json, Value};
    use std::path::PathBuf;
    use std::sync::Barrier;
    use std::thread;
    use uuid::Uuid;

    const INSTANCE: &str = "11111111-1111-4111-8111-111111111111";
    const OTHER_INSTANCE: &str = "22222222-2222-4222-8222-222222222222";

    struct TempRoot(PathBuf);

    impl TempRoot {
        fn new() -> Self {
            #[cfg(unix)]
            let base = PathBuf::from("/tmp/opencode");
            #[cfg(not(unix))]
            let base = std::env::temp_dir();
            let path = base.join(format!("connection-relations-test-{}", Uuid::new_v4()));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn document_path(&self) -> PathBuf {
            self.0.join(COLLECTION).join(format!("{DOCUMENT_ID}.json"))
        }

        fn bytes(&self) -> Vec<u8> {
            std::fs::read(self.document_path()).unwrap()
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            // 仅回收本测试新建的仓库外唯一目录。
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }

    fn source(account: &str, project: &str, subresource: &str) -> SourceRef {
        SourceRef::new(
            SourceKind::Software,
            "same-source-id",
            SourceScope::new(
                INSTANCE,
                "software-owner",
                account,
                Some(project.to_owned()),
            )
            .unwrap(),
            Some(subresource.to_owned()),
            SourceLifecycle::known("source-generation-1").unwrap(),
            "source-revision-1",
        )
        .unwrap()
    }

    fn pair(
        registry: &mut RegistrationRegistry,
        account: &str,
        project: &str,
    ) -> (ConnectionRef, ConnectionRef) {
        let software = registry
            .register(
                ConnectionKind::Software,
                source(account, project, "documents"),
                "同名产品",
            )
            .unwrap();
        let ai = registry
            .register(
                ConnectionKind::ExternalAgent,
                source(account, project, "assistant"),
                "同名产品",
            )
            .unwrap();
        (software, ai)
    }

    fn relation(software: &ConnectionRef, ai: &ConnectionRef) -> ConnectionRelation {
        ConnectionRelation::new(
            ConnectionRelationKind::EmbeddedIn,
            ai.clone(),
            software.clone(),
        )
        .unwrap()
    }

    fn state(service: &ConnectionRelationService) -> RelationDocument {
        service.inner.document.lock().clone()
    }

    #[test]
    fn explicit_relations_reopen_with_distinct_refs_and_source_scopes_despite_equal_names() {
        let root = TempRoot::new();
        let mut registry = RegistrationRegistry::new(INSTANCE).unwrap();
        let (software_a, ai_a) = pair(&mut registry, "account-a", "project-a");
        let (software_b, ai_b) = pair(&mut registry, "account-b", "project-a");
        let (software_c, ai_c) = pair(&mut registry, "account-a", "project-b");
        let service = ConnectionRelationService::open(&root.0).unwrap();
        assert!(service.current_relations(&registry).unwrap().is_empty());
        assert!(!root.document_path().exists());
        let mut relations = Vec::new();
        for (software, ai) in [
            (&software_a, &ai_a),
            (&software_b, &ai_b),
            (&software_c, &ai_c),
        ] {
            let edge = relation(software, ai);
            assert_eq!(service.upsert(&registry, edge.clone(), 0).unwrap(), edge);
            relations.push(edge);
        }
        drop(service);
        let service = ConnectionRelationService::open(&root.0).unwrap();
        assert_eq!(service.current_relations(&registry).unwrap().len(), 3);
        let views: Vec<_> = relations
            .iter()
            .map(|edge| service.get(&registry, edge.id()).unwrap().unwrap())
            .collect();
        for (view, edge) in views.iter().zip(&relations) {
            assert_eq!(view.relation(), edge);
            assert_eq!(view.from().display_name(), view.to().display_name());
            assert_ne!(view.from().reference(), view.to().reference());
            assert_ne!(
                view.from().source().subresource_id(),
                view.to().source().subresource_id()
            );
        }
        assert_ne!(
            views[0].from().source().scope().account_id(),
            views[1].from().source().scope().account_id()
        );
        assert_ne!(
            views[0].to().source().scope().project_id(),
            views[2].to().source().scope().project_id()
        );
    }

    #[test]
    fn upsert_and_remove_require_exact_revisions_and_failed_cas_changes_neither_memory_nor_disk() {
        let root = TempRoot::new();
        let mut registry = RegistrationRegistry::new(INSTANCE).unwrap();
        let (software, ai) = pair(&mut registry, "account-a", "project-a");
        let edge = relation(&software, &ai);
        let service = ConnectionRelationService::open(&root.0).unwrap();
        assert!(service.upsert(&registry, edge.clone(), 1).is_err());
        assert!(!root.document_path().exists());
        service.upsert(&registry, edge.clone(), 0).unwrap();
        let revised = edge
            .revised(
                1,
                ConnectionRelationKind::ProvidedBy,
                ai.clone(),
                software.clone(),
            )
            .unwrap();
        let skipped = revised
            .revised(2, revised.kind(), ai.clone(), software.clone())
            .unwrap();
        let before = state(&service);
        let disk_before = root.bytes();
        for (candidate, expected) in [
            (edge.clone(), 0),
            (edge.clone(), 1),
            (revised.clone(), 0),
            (revised.clone(), 99),
            (skipped, 1),
        ] {
            assert!(service.upsert(&registry, candidate, expected).is_err());
            assert_eq!(state(&service), before);
            assert_eq!(root.bytes(), disk_before);
        }
        assert!(service.remove(&registry, edge.id(), 0).is_err());
        assert!(service.remove(&registry, edge.id(), 2).is_err());
        assert_eq!(state(&service), before);
        assert_eq!(root.bytes(), disk_before);
        assert_eq!(
            service.upsert(&registry, revised.clone(), 1).unwrap(),
            revised
        );
        assert_eq!(
            service
                .get(&registry, edge.id())
                .unwrap()
                .unwrap()
                .relation()
                .revision(),
            2
        );
        drop(service);
        let reopened = ConnectionRelationService::open(&root.0).unwrap();
        assert_eq!(
            reopened
                .get(&registry, edge.id())
                .unwrap()
                .unwrap()
                .relation(),
            &revised
        );
    }

    #[test]
    fn duplicate_live_edges_and_noninitial_create_candidates_are_rejected() {
        let root = TempRoot::new();
        let mut registry = RegistrationRegistry::new(INSTANCE).unwrap();
        let (software, ai) = pair(&mut registry, "account-a", "project-a");
        let edge = relation(&software, &ai);
        let service = ConnectionRelationService::open(&root.0).unwrap();
        let premature = edge
            .revised(1, edge.kind(), ai.clone(), software.clone())
            .unwrap();
        assert!(service.upsert(&registry, premature, 0).is_err());
        assert!(!root.document_path().exists());
        service.upsert(&registry, edge, 0).unwrap();
        let before = state(&service);
        let bytes = root.bytes();
        assert!(service
            .upsert(&registry, relation(&software, &ai), 0)
            .is_err());
        assert_eq!(state(&service), before);
        assert_eq!(root.bytes(), bytes);
    }

    #[test]
    fn concurrent_same_revision_upserts_have_exactly_one_committed_winner() {
        let root = TempRoot::new();
        let mut registry = RegistrationRegistry::new(INSTANCE).unwrap();
        let (software, ai) = pair(&mut registry, "account-a", "project-a");
        let edge = relation(&software, &ai);
        let service = ConnectionRelationService::open(&root.0).unwrap();
        service.upsert(&registry, edge.clone(), 0).unwrap();
        let updated = edge
            .revised(1, ConnectionRelationKind::ProvidedBy, ai, software)
            .unwrap();
        let registry = Arc::new(registry);
        let barrier = Arc::new(Barrier::new(3));
        let workers: Vec<_> = (0..2)
            .map(|_| {
                let service = service.clone();
                let registry = registry.clone();
                let barrier = barrier.clone();
                let updated = updated.clone();
                thread::spawn(move || {
                    barrier.wait();
                    service.upsert(&registry, updated, 1).is_ok()
                })
            })
            .collect();
        barrier.wait();
        let winners = workers
            .into_iter()
            .map(|worker| usize::from(worker.join().unwrap()))
            .sum::<usize>();
        assert_eq!(winners, 1);
        assert_eq!(
            service
                .get(&registry, edge.id())
                .unwrap()
                .unwrap()
                .relation(),
            &updated
        );
        drop(service);
        let reopened = ConnectionRelationService::open(&root.0).unwrap();
        assert_eq!(
            reopened
                .get(&registry, edge.id())
                .unwrap()
                .unwrap()
                .relation(),
            &updated
        );
    }

    #[test]
    fn concurrent_update_and_remove_share_the_same_revision_cas() {
        let root = TempRoot::new();
        let mut registry = RegistrationRegistry::new(INSTANCE).unwrap();
        let (software, ai) = pair(&mut registry, "account-a", "project-a");
        let edge = relation(&software, &ai);
        let service = ConnectionRelationService::open(&root.0).unwrap();
        service.upsert(&registry, edge.clone(), 0).unwrap();
        let revised = edge
            .revised(1, ConnectionRelationKind::ProvidedBy, ai, software)
            .unwrap();
        let registry = Arc::new(registry);
        let barrier = Arc::new(Barrier::new(3));
        let updater = {
            let service = service.clone();
            let registry = registry.clone();
            let barrier = barrier.clone();
            thread::spawn(move || {
                barrier.wait();
                service.upsert(&registry, revised, 1).is_ok()
            })
        };
        let remover = {
            let service = service.clone();
            let registry = registry.clone();
            let barrier = barrier.clone();
            let id = edge.id().to_owned();
            thread::spawn(move || {
                barrier.wait();
                service.remove(&registry, &id, 1).is_ok()
            })
        };
        barrier.wait();
        let updated = updater.join().unwrap();
        let removed = remover.join().unwrap();
        assert_ne!(updated, removed);
        assert_eq!(state(&service).relations[0].relation.revision(), 2);
        assert_eq!(
            service.get(&registry, edge.id()).unwrap().is_some(),
            updated
        );
    }

    #[test]
    fn removing_display_relation_preserves_registry_source_documents_device_and_agent_bindings() {
        let root = TempRoot::new();
        let source_db = FsDb::open(&root.0).unwrap();
        let sentinels = [
            (
                "source_config",
                "software",
                json!({"account":"account-a", "enabled":true}),
            ),
            (
                "device_records",
                "speaker",
                json!({"paired":true, "grants":["device-only"]}),
            ),
            (
                "agent_bindings",
                "member",
                json!({"link":"existing-link", "model":"unchanged"}),
            ),
            (
                "remote_fixture",
                "document",
                json!({"revision":7, "body":"must-not-be-deleted"}),
            ),
        ];
        for (collection, id, value) in &sentinels {
            source_db.put(collection, id, value).unwrap();
        }
        let source_files: Vec<_> = sentinels
            .iter()
            .map(|(collection, id, _)| {
                let path = root.0.join(collection).join(format!("{id}.json"));
                let bytes = std::fs::read(&path).unwrap();
                (path, bytes)
            })
            .collect();
        let mut registry = RegistrationRegistry::new(INSTANCE).unwrap();
        let (software, ai) = pair(&mut registry, "account-a", "project-a");
        registry.set_enabled(&software, true).unwrap();
        // 仅取只读 grant 将来须保存的身份绑定，绝不制造 grant/执行服务。
        let software_binding = registry.binding_for(&software).unwrap();
        let registry_before = registry.snapshot();
        let service = ConnectionRelationService::open(&root.0).unwrap();
        let edge = relation(&software, &ai);
        service.upsert(&registry, edge.clone(), 0).unwrap();
        assert!(registry.binding_for(&ai).is_err());
        let mut crossed = serde_json::to_value(&software_binding).unwrap();
        crossed["connection"] = serde_json::to_value(&ai).unwrap();
        let crossed: RegistrationBinding = serde_json::from_value(crossed).unwrap();
        assert!(registry.validate_binding(&crossed).is_err());
        assert_eq!(service.remove(&registry, edge.id(), 1).unwrap(), 2);
        assert!(service.get(&registry, edge.id()).unwrap().is_none());
        assert_eq!(registry.snapshot(), registry_before);
        assert!(registry.validate_binding(&software_binding).is_ok());
        assert!(registry.binding_for(&ai).is_err());
        for (path, bytes) in source_files {
            assert_eq!(std::fs::read(path).unwrap(), bytes);
        }
    }

    #[test]
    fn removal_tombstone_survives_reopen_and_rejects_replayed_relation_ids() {
        let root = TempRoot::new();
        let mut registry = RegistrationRegistry::new(INSTANCE).unwrap();
        let (software, ai) = pair(&mut registry, "account-a", "project-a");
        let edge = relation(&software, &ai);
        let service = ConnectionRelationService::open(&root.0).unwrap();
        service.upsert(&registry, edge.clone(), 0).unwrap();
        service.remove(&registry, edge.id(), 1).unwrap();
        drop(service);
        let service = ConnectionRelationService::open(&root.0).unwrap();
        let before = state(&service);
        let disk_before = root.bytes();
        assert!(service.get(&registry, edge.id()).unwrap().is_none());
        assert!(service.current_relations(&registry).unwrap().is_empty());
        assert!(service.upsert(&registry, edge.clone(), 0).is_err());
        let replay = edge
            .revised(1, edge.kind(), ai.clone(), software.clone())
            .unwrap();
        let replay = replay
            .revised(2, edge.kind(), ai.clone(), software.clone())
            .unwrap();
        assert!(service.upsert(&registry, replay, 2).is_err());
        assert!(service.remove(&registry, edge.id(), 2).is_err());
        assert_eq!(state(&service), before);
        assert_eq!(root.bytes(), disk_before);
        let new_edge = relation(&software, &ai);
        service.upsert(&registry, new_edge.clone(), 0).unwrap();
        assert_ne!(new_edge.id(), edge.id());
        assert!(service.get(&registry, new_edge.id()).unwrap().is_some());
    }

    #[test]
    fn software_binding_cannot_be_retargeted_to_related_ai_even_if_both_registrations_are_enabled()
    {
        let root = TempRoot::new();
        let mut registry = RegistrationRegistry::new(INSTANCE).unwrap();
        let (software, ai) = pair(&mut registry, "account-a", "project-a");
        registry.set_enabled(&software, true).unwrap();
        registry.set_enabled(&ai, true).unwrap();
        // 软件只读 grant 将来保存的身份字段；这里不伪造能力或实现 grant 服务。
        let software_binding = registry.binding_for(&software).unwrap();
        let ai_binding = registry.binding_for(&ai).unwrap();
        let before = registry.snapshot();
        let service = ConnectionRelationService::open(&root.0).unwrap();
        let edge = relation(&software, &ai);
        service.upsert(&registry, edge.clone(), 0).unwrap();
        assert_ne!(software_binding.connection(), ai_binding.connection());
        assert_ne!(software_binding.source(), ai_binding.source());
        let mut retargeted = serde_json::to_value(&software_binding).unwrap();
        retargeted["connection"] = serde_json::to_value(&ai).unwrap();
        let retargeted: RegistrationBinding = serde_json::from_value(retargeted).unwrap();
        assert!(registry.validate_binding(&retargeted).is_err());
        assert!(registry.validate_binding(&software_binding).is_ok());
        assert!(registry.validate_binding(&ai_binding).is_ok());
        service.remove(&registry, edge.id(), 1).unwrap();
        assert_eq!(registry.snapshot(), before);
        assert!(registry.validate_binding(&retargeted).is_err());
    }

    #[test]
    fn explicit_display_relation_does_not_manage_unresolved_source_lifecycles() {
        let root = TempRoot::new();
        let mut registry = RegistrationRegistry::new(INSTANCE).unwrap();
        let unresolved = |subresource| {
            SourceRef::new(
                SourceKind::Software,
                "legacy-software",
                SourceScope::new(INSTANCE, "legacy-owner", "account-a", None).unwrap(),
                Some(subresource),
                SourceLifecycle::LegacyUnresolved,
                "unversioned",
            )
            .unwrap()
        };
        let software = registry
            .register(
                ConnectionKind::Software,
                unresolved("documents".to_owned()),
                "同名产品",
            )
            .unwrap();
        let ai = registry
            .register(
                ConnectionKind::ExternalAgent,
                unresolved("assistant".to_owned()),
                "同名产品",
            )
            .unwrap();
        let before = registry.snapshot();
        let service = ConnectionRelationService::open(&root.0).unwrap();
        let edge = relation(&software, &ai);
        service.upsert(&registry, edge.clone(), 0).unwrap();
        assert!(service.get(&registry, edge.id()).unwrap().is_some());
        assert!(registry.binding_for(&software).is_err());
        assert!(registry.binding_for(&ai).is_err());
        service.remove(&registry, edge.id(), 1).unwrap();
        assert_eq!(registry.snapshot(), before);
    }

    #[test]
    fn stale_endpoint_never_reappears_after_registration_recreation_or_service_reopen() {
        let root = TempRoot::new();
        let mut registry = RegistrationRegistry::new(INSTANCE).unwrap();
        let (software, ai) = pair(&mut registry, "account-a", "project-a");
        let edge = relation(&software, &ai);
        let service = ConnectionRelationService::open(&root.0).unwrap();
        service.upsert(&registry, edge.clone(), 0).unwrap();
        let bytes = root.bytes();
        registry.delete(&ai).unwrap();
        assert!(service.get(&registry, edge.id()).unwrap().is_none());
        assert!(service.current_relations(&registry).unwrap().is_empty());
        assert!(service.remove(&registry, edge.id(), 1).is_err());
        let stale_update = edge
            .revised(1, edge.kind(), ai.clone(), software.clone())
            .unwrap();
        assert!(service.upsert(&registry, stale_update, 1).is_err());
        let replacement = registry
            .reregister(
                &ai,
                source("account-a", "project-a", "assistant"),
                "同名产品",
            )
            .unwrap();
        assert_eq!(ai.connection_id(), replacement.connection_id());
        assert_ne!(ai.generation(), replacement.generation());
        assert!(service.get(&registry, edge.id()).unwrap().is_none());
        assert!(source_edit_target(&registry, &ai).is_err());
        assert!(source_edit_target(&registry, &replacement).is_ok());
        assert_eq!(root.bytes(), bytes);
        drop(service);
        let service = ConnectionRelationService::open(&root.0).unwrap();
        assert!(service.get(&registry, edge.id()).unwrap().is_none());
        // 只有用户显式提交当前新端点及原关系精确版本，才能建立新的关联。
        let updated = edge
            .revised(1, edge.kind(), replacement.clone(), software)
            .unwrap();
        service.upsert(&registry, updated, 1).unwrap();
        assert_eq!(
            service
                .get(&registry, edge.id())
                .unwrap()
                .unwrap()
                .from()
                .reference(),
            &replacement
        );
        assert!(registry.binding_for(&replacement).is_err());
    }

    #[test]
    fn missing_or_foreign_endpoints_and_cross_instance_store_access_are_rejected() {
        let root = TempRoot::new();
        let mut registry = RegistrationRegistry::new(INSTANCE).unwrap();
        let (software, ai) = pair(&mut registry, "account-a", "project-a");
        let service = ConnectionRelationService::open(&root.0).unwrap();
        let unknown = ConnectionRef::new(INSTANCE, ConnectionKind::ExternalAgent).unwrap();
        assert!(service
            .upsert(&registry, relation(&software, &unknown), 0)
            .is_err());
        assert!(!root.document_path().exists());
        let edge = relation(&software, &ai);
        service.upsert(&registry, edge.clone(), 0).unwrap();
        let foreign = RegistrationRegistry::new(OTHER_INSTANCE).unwrap();
        assert!(service.get(&foreign, edge.id()).is_err());
        assert!(service.current_relations(&foreign).is_err());
        assert!(service.remove(&foreign, edge.id(), 1).is_err());
        assert!(service.upsert(&foreign, edge, 1).is_err());
        assert!(source_edit_target(&foreign, &software).is_err());
    }

    #[test]
    fn fsdb_write_failure_keeps_last_committed_memory_and_disk_for_upsert_and_remove() {
        let root = TempRoot::new();
        let mut registry = RegistrationRegistry::new(INSTANCE).unwrap();
        let (software, ai) = pair(&mut registry, "account-a", "project-a");
        let edge = relation(&software, &ai);
        let service = ConnectionRelationService::open(&root.0).unwrap();
        service.upsert(&registry, edge.clone(), 0).unwrap();
        let before = state(&service);
        let bytes = root.bytes();
        let collection_path = root.0.join(COLLECTION);
        let backup = root.0.join("committed-relations-backup");
        std::fs::rename(&collection_path, &backup).unwrap();
        std::fs::write(&collection_path, b"intentional fixture: not a directory").unwrap();
        let candidate = edge
            .revised(1, ConnectionRelationKind::ProvidedBy, ai, software)
            .unwrap();
        assert!(service.upsert(&registry, candidate, 1).is_err());
        assert!(service.remove(&registry, edge.id(), 1).is_err());
        assert_eq!(state(&service), before);
        assert_eq!(
            service
                .get(&registry, edge.id())
                .unwrap()
                .unwrap()
                .relation(),
            &edge
        );
        assert_eq!(
            std::fs::read(backup.join(format!("{DOCUMENT_ID}.json"))).unwrap(),
            bytes
        );
        std::fs::remove_file(collection_path.clone()).unwrap();
        std::fs::rename(backup, collection_path).unwrap();
        drop(service);
        let reopened = ConnectionRelationService::open(&root.0).unwrap();
        assert_eq!(state(&reopened), before);
        assert_eq!(root.bytes(), bytes);
    }

    #[test]
    fn corrupt_unknown_version_duplicate_or_cross_instance_documents_fail_closed_on_open() {
        let root = TempRoot::new();
        let mut registry = RegistrationRegistry::new(INSTANCE).unwrap();
        let (software, ai) = pair(&mut registry, "account-a", "project-a");
        let service = ConnectionRelationService::open(&root.0).unwrap();
        service
            .upsert(&registry, relation(&software, &ai), 0)
            .unwrap();
        let valid = serde_json::to_value(state(&service)).unwrap();
        drop(service);
        let db = FsDb::open(&root.0).unwrap();
        let mut cases = Vec::new();
        let mut version = valid.clone();
        version["schemaVersion"] = json!(CONNECTION_RELATION_SCHEMA_VERSION + 1);
        cases.push(version);
        let mut duplicate = valid.clone();
        let record = duplicate["relations"][0].clone();
        duplicate["relations"].as_array_mut().unwrap().push(record);
        cases.push(duplicate);
        let mut duplicate_edge = valid.clone();
        let mut record = duplicate_edge["relations"][0].clone();
        record["relation"]["id"] = json!(Uuid::new_v4().to_string());
        duplicate_edge["relations"]
            .as_array_mut()
            .unwrap()
            .push(record);
        cases.push(duplicate_edge);
        let mut foreign = valid.clone();
        foreign["instanceId"] = json!(OTHER_INSTANCE);
        cases.push(foreign);
        let mut missing_instance = valid.clone();
        missing_instance["instanceId"] = Value::Null;
        cases.push(missing_instance);
        let mut invalid_tombstone = valid.clone();
        invalid_tombstone["relations"][0]["removed"] = json!(true);
        cases.push(invalid_tombstone);
        let mut credentials = valid.clone();
        credentials["relations"][0]["credential"] = json!("must-not-be-read");
        cases.push(credentials);
        let mut unknown_field = valid;
        unknown_field["saveConfig"] = json!(true);
        cases.push(unknown_field);
        for invalid in cases {
            db.put(COLLECTION, DOCUMENT_ID, &invalid).unwrap();
            let bytes = root.bytes();
            assert!(ConnectionRelationService::open(&root.0).is_err());
            assert_eq!(root.bytes(), bytes);
        }
        std::fs::write(root.document_path(), b"{not-complete-json").unwrap();
        assert!(ConnectionRelationService::open(&root.0).is_err());
    }

    #[test]
    fn edit_targets_delegate_only_to_typed_source_owners_and_do_not_invent_routes_or_config() {
        let root = TempRoot::new();
        let service = ConnectionRelationService::open(&root.0).unwrap();
        let mut registry = RegistrationRegistry::new(INSTANCE).unwrap();
        for kind in [
            SourceKind::Nexus,
            SourceKind::Mcp,
            SourceKind::Plugin,
            SourceKind::Device,
            SourceKind::Software,
        ] {
            let source = SourceRef::new(
                kind,
                "same-source-id",
                SourceScope::new(INSTANCE, "source-owner", "local-account", None).unwrap(),
                None,
                SourceLifecycle::LegacyUnresolved,
                "revision-1",
            )
            .unwrap();
            let connection_kind = match kind {
                SourceKind::Nexus => ConnectionKind::ExternalAgent,
                SourceKind::Device => ConnectionKind::Device,
                _ => ConnectionKind::Software,
            };
            let reference = registry
                .register(connection_kind, source.clone(), "同名来源")
                .unwrap();
            let before = registry.snapshot();
            let target = source_edit_target(&registry, &reference).unwrap();
            assert_eq!(target.source_kind(), kind);
            assert_eq!(target.source_id(), source.source_id());
            let encoded = serde_json::to_value(target).unwrap();
            assert_eq!(
                encoded
                    .as_object()
                    .unwrap()
                    .keys()
                    .map(String::as_str)
                    .collect::<HashSet<_>>(),
                HashSet::from(["sourceKind", "sourceId"])
            );
            assert_eq!(registry.snapshot(), before);
            assert!(registry.binding_for(&reference).is_err());
        }
        assert!(service.current_relations(&registry).unwrap().is_empty());
        assert!(!root.document_path().exists());
    }

    #[test]
    fn read_only_relation_snapshot_does_not_create_paths_fails_closed_and_hides_stale_edges() {
        let root = TempRoot::new();
        let mut registry = RegistrationRegistry::new(INSTANCE).unwrap();
        let (software, ai) = pair(&mut registry, "account-a", "project-a");
        assert!(
            ConnectionRelationService::read_current_relations(&root.0, &registry)
                .unwrap()
                .is_empty()
        );
        assert!(!root.0.join(COLLECTION).exists());

        std::fs::create_dir_all(root.0.join(COLLECTION)).unwrap();
        std::fs::write(root.document_path(), b"{broken relation store").unwrap();
        let invalid_bytes = root.bytes();
        assert!(ConnectionRelationService::read_current_relations(&root.0, &registry).is_err());
        assert_eq!(root.bytes(), invalid_bytes);

        std::fs::remove_file(root.document_path()).unwrap();
        let service = ConnectionRelationService::open(&root.0).unwrap();
        let edge = relation(&software, &ai);
        service.upsert(&registry, edge, 0).unwrap();
        let committed = root.bytes();
        assert_eq!(
            ConnectionRelationService::read_current_relations(&root.0, &registry)
                .unwrap()
                .len(),
            1
        );
        registry.delete(&ai).unwrap();
        assert!(
            ConnectionRelationService::read_current_relations(&root.0, &registry)
                .unwrap()
                .is_empty()
        );
        assert_eq!(root.bytes(), committed);
    }
}
