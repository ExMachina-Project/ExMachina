//! 本地登记及不透明凭据引用；不解析凭据、不连接来源、不执行动作。
//!
//! 同一进程对同一规范化 data root 共享一个实例；clone 共享 Mutex。修改在锁内
//! clone -> FsDb 单文档提交 -> 发布内存，失败不发布。安装 ID 独立保存，不能从
//! 登记快照采纳另一个安装的身份。损坏、未知版本及缺少一半文档均拒绝打开。
//! 保证沿用 FsDb，不声称跨进程事务、抗受信目录回滚或掉电 fsync；Core 不得让
//! 多个进程同时写同一 data root，也不能把整个受信目录克隆当成新安装的安全导入。
//!
//! 此处是宿主管理内部面，不做主体 ACL 或 grant 服务。身份绑定通过不等于执行
//! 授权；目录不得直接暴露凭据引用或未按 ACL 过滤的 snapshot。跨实例传递只能用
//! refs-only bundle：不含名称、原连接 ID/代际、来源生命周期证据、凭据或运行授权，
//! 导入重新映射当前安装、生成新连接/代际，并固定为 unresolved，等待来源重验。

use super::identity::{
    ConnectionKind, ConnectionRef, ConnectionRegistration, RegistrationBinding,
    RegistrationRegistry, RegistrationSnapshot, RegistrationState, SourceLifecycle, SourceRef,
    SourceScope,
};
use crate::fsdb::FsDb;
use anyhow::{ensure, Context, Result};
use parking_lot::Mutex;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, Weak};
use uuid::Uuid;

pub const CONNECTION_REGISTRATION_STORE_SCHEMA_VERSION: u16 = 1;
pub const CONNECTION_REFERENCE_BUNDLE_SCHEMA_VERSION: u16 = 1;
const COLLECTION: &str = "connection_registrations";
const INSTALLATION_ID: &str = "installation";
const DOCUMENT_ID: &str = "state";
const MAX_REGISTRATIONS: usize = 4096; // 包括 tombstone，不自动裁剪身份历史。
const MAX_DOCUMENT_BYTES: usize = 16 * 1024 * 1024;
const MAX_BUNDLE_BYTES: usize = 8 * 1024 * 1024;
const UNRESOLVED_REVISION: &str = "unresolved";

/// 原来源凭据服务持有的引用，不是秘密或可用令牌。credentialId 必须是该服务的
/// 非秘密 UUID 对象 ID；revision 由原服务提供和核验。没有 URL、环境变量名或 key 字段。
/// SourceRef 精确绑定实例、来源、账户/项目/子资源、生命周期及来源 revision。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", try_from = "CredentialReferenceData")]
pub struct CredentialReference {
    source: SourceRef,
    credential_id: String,
    revision: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CredentialReferenceData {
    source: SourceRef,
    credential_id: String,
    revision: u64,
}

impl TryFrom<CredentialReferenceData> for CredentialReference {
    type Error = anyhow::Error;

    fn try_from(value: CredentialReferenceData) -> Result<Self> {
        Self::new(value.source, value.credential_id, value.revision)
    }
}

impl CredentialReference {
    pub fn new(source: SourceRef, credential_id: impl Into<String>, revision: u64) -> Result<Self> {
        let reference = Self {
            source,
            credential_id: credential_id.into(),
            revision,
        };
        let id = Uuid::parse_str(&reference.credential_id).context("凭据引用 ID 必须为 UUID")?;
        ensure!(
            !id.is_nil() && id.to_string() == reference.credential_id,
            "凭据引用 ID 必须为非空规范 UUID"
        );
        ensure!(reference.revision > 0, "凭据引用 revision 必须大于零");
        ensure!(
            reference.source.lifecycle().is_known(),
            "未知来源生命周期不能绑定凭据引用"
        );
        Ok(reference)
    }

    pub fn source(&self) -> &SourceRef {
        &self.source
    }

    pub fn credential_id(&self) -> &str {
        &self.credential_id
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Installation {
    schema_version: u16,
    instance_id: String,
}

impl Installation {
    fn validate(&self) -> Result<()> {
        ensure!(
            self.schema_version == CONNECTION_REGISTRATION_STORE_SCHEMA_VERSION,
            "不支持的安装身份 schemaVersion"
        );
        RegistrationRegistry::new(self.instance_id.clone())?;
        Ok(())
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CredentialBinding {
    connection: ConnectionRef,
    credential: CredentialReference,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RegistrationDocument {
    schema_version: u16,
    snapshot: RegistrationSnapshot,
    credential_bindings: Vec<CredentialBinding>,
}

#[derive(Clone)]
struct StoreState {
    registry: RegistrationRegistry,
    credentials: Vec<CredentialBinding>,
}

impl StoreState {
    fn from_document(instance: &str, document: RegistrationDocument) -> Result<Self> {
        ensure!(
            document.schema_version == CONNECTION_REGISTRATION_STORE_SCHEMA_VERSION,
            "不支持的登记存储 schemaVersion"
        );
        let state = Self {
            registry: RegistrationRegistry::from_snapshot(instance, document.snapshot)?,
            credentials: document.credential_bindings,
        };
        state.validate()?;
        Ok(state)
    }

    fn validate(&self) -> Result<()> {
        ensure!(
            self.registry.registrations().len() <= MAX_REGISTRATIONS
                && self.credentials.len() <= MAX_REGISTRATIONS,
            "登记或凭据引用超过存储上限"
        );
        let mut seen = HashSet::new();
        for binding in &self.credentials {
            ensure!(seen.insert(&binding.connection), "凭据绑定重复");
            let registration = current_registration(&self.registry, &binding.connection)?;
            ensure!(
                registration.source() == binding.credential.source(),
                "凭据引用与登记来源不一致"
            );
        }
        Ok(())
    }

    fn document(&self) -> RegistrationDocument {
        RegistrationDocument {
            schema_version: CONNECTION_REGISTRATION_STORE_SCHEMA_VERSION,
            snapshot: self.registry.snapshot(),
            credential_bindings: self.credentials.clone(),
        }
    }
}

struct StoreInner {
    db: FsDb,
    instance_id: String,
    state: Mutex<StoreState>,
}

static OPEN_STORES: OnceLock<Mutex<HashMap<PathBuf, Weak<StoreInner>>>> = OnceLock::new();

/// 宿主内部服务。独立 open 同一规范化路径及 clone 都共享同一锁和已提交状态。
#[derive(Clone)]
pub struct ConnectionRegistrationStore {
    inner: Arc<StoreInner>,
}

impl ConnectionRegistrationStore {
    /// 仅在两个文档均不存在时创建随机安装 UUID；不会扫描/迁移旧来源或恢复授权。
    /// data_root 来自受信 Core 配置，不能来自导入包、来源 ID 或 Agent 参数。
    pub fn open(data_root: impl AsRef<Path>) -> Result<Self> {
        let canonical_root = FsDb::open(data_root)?.root().canonicalize()?;
        let mut stores = OPEN_STORES.get_or_init(Mutex::default).lock();
        stores.retain(|_, store| store.strong_count() > 0);
        if let Some(inner) = stores.get(&canonical_root).and_then(Weak::upgrade) {
            return Ok(Self { inner });
        }
        let db = FsDb::open(&canonical_root)?;
        let installation = read_document::<Installation>(&db, INSTALLATION_ID, 4096)?;
        let document = read_document::<RegistrationDocument>(&db, DOCUMENT_ID, MAX_DOCUMENT_BYTES)?;
        let (installation, state) = match (installation, document) {
            (None, None) => {
                let installation = Installation {
                    schema_version: CONNECTION_REGISTRATION_STORE_SCHEMA_VERSION,
                    instance_id: Uuid::new_v4().to_string(),
                };
                let state = StoreState {
                    registry: RegistrationRegistry::new(installation.instance_id.clone())?,
                    credentials: Vec::new(),
                };
                // 任一步失败都返回错误；半次初始化不会在下次打开时冒充空登记。
                db.put(COLLECTION, INSTALLATION_ID, &installation)?;
                db.put(COLLECTION, DOCUMENT_ID, &state.document())?;
                (installation, state)
            }
            (Some(installation), Some(document)) => {
                installation.validate()?;
                let state = StoreState::from_document(&installation.instance_id, document)?;
                (installation, state)
            }
            _ => anyhow::bail!("登记存储不完整，不能重置或采纳快照的安装身份"),
        };
        let inner = Arc::new(StoreInner {
            db,
            instance_id: installation.instance_id,
            state: Mutex::new(state),
        });
        stores.insert(canonical_root, Arc::downgrade(&inner));
        Ok(Self { inner })
    }

    pub fn instance_id(&self) -> &str {
        &self.inner.instance_id
    }

    /// 供关系/目录等宿主服务在持锁期间检查当前登记，不能在闭包内重入本服务。
    /// 返回旧副本供稍后执行不能替代执行前重新校验。
    pub fn with_registry<T>(
        &self,
        read: impl FnOnce(&RegistrationRegistry) -> Result<T>,
    ) -> Result<T> {
        read(&self.inner.state.lock().registry)
    }

    /// 受信本地身份快照，不是跨实例导出格式。本服务不提供快照写回/restore 接口。
    pub fn snapshot(&self) -> RegistrationSnapshot {
        self.inner.state.lock().registry.snapshot()
    }

    pub fn get(&self, reference: &ConnectionRef) -> Option<ConnectionRegistration> {
        self.inner.state.lock().registry.get(reference).cloned()
    }

    pub fn register(
        &self,
        kind: ConnectionKind,
        source: SourceRef,
        display_name: impl Into<String>,
    ) -> Result<ConnectionRef> {
        let display_name = display_name.into();
        self.update(|next| next.registry.register(kind, source, display_name))
    }

    pub fn reregister(
        &self,
        deleted: &ConnectionRef,
        source: SourceRef,
        display_name: impl Into<String>,
    ) -> Result<ConnectionRef> {
        let display_name = display_name.into();
        self.update(|next| next.registry.reregister(deleted, source, display_name))
    }

    pub fn set_enabled(&self, reference: &ConnectionRef, enabled: bool) -> Result<()> {
        self.update(|next| next.registry.set_enabled(reference, enabled))
    }

    pub fn delete(&self, reference: &ConnectionRef) -> Result<()> {
        self.update(|next| {
            next.registry.delete(reference)?;
            next.credentials
                .retain(|binding| binding.connection != *reference);
            Ok(())
        })
    }

    /// 身份检查不是 grant 发放，来源/主体/能力/凭据的准入仍由各自 owner 完成。
    pub fn binding_for(&self, reference: &ConnectionRef) -> Result<RegistrationBinding> {
        self.with_registry(|registry| registry.binding_for(reference))
    }

    pub fn validate_binding(
        &self,
        binding: &RegistrationBinding,
    ) -> Result<ConnectionRegistration> {
        self.with_registry(|registry| registry.validate_binding(binding).cloned())
    }

    /// 引用增删/替换时强制禁用当前登记，旧身份授权绑定随 revision 失效。
    /// 相同引用是幂等的；不根据 ID 继承另一个登记/代际的凭据，也不访问凭据服务。
    pub fn set_credential(
        &self,
        reference: &ConnectionRef,
        credential: Option<CredentialReference>,
    ) -> Result<()> {
        self.update(|next| {
            let registration = current_registration(&next.registry, reference)?;
            if let Some(credential) = &credential {
                ensure!(
                    credential.source() == registration.source(),
                    "凭据引用与登记来源不一致"
                );
            }
            let current = next
                .credentials
                .iter()
                .find(|binding| binding.connection == *reference);
            if current.map(|binding| &binding.credential) == credential.as_ref() {
                return Ok(());
            }
            next.registry.set_enabled(reference, false)?;
            next.credentials
                .retain(|binding| binding.connection != *reference);
            if let Some(credential) = credential {
                next.credentials.push(CredentialBinding {
                    connection: reference.clone(),
                    credential,
                });
            }
            Ok(())
        })
    }

    /// 仅宿主凭据服务可消费此引用；不能加入目录/模型/普通事件响应。
    pub fn credential_reference(
        &self,
        reference: &ConnectionRef,
    ) -> Result<Option<CredentialReference>> {
        let state = self.inner.state.lock();
        current_registration(&state.registry, reference)?;
        Ok(state
            .credentials
            .iter()
            .find(|binding| binding.connection == *reference)
            .map(|binding| binding.credential.clone()))
    }

    /// 导出只保留非秘密来源引用；不导出名称、连接 ID、凭据、启用态或来源证明。
    pub fn export_refs(&self) -> Result<RegistrationReferenceBundle> {
        let state = self.inner.state.lock();
        let references = state
            .registry
            .registrations()
            .iter()
            .filter(|registration| registration.state() != RegistrationState::Deleted)
            .map(|registration| {
                Ok(RegistrationReferenceEntry {
                    kind: registration.reference().kind(),
                    source: unresolved_source(
                        registration.source(),
                        self.instance_id(),
                        UNRESOLVED_REVISION,
                    )?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let bundle = RegistrationReferenceBundle {
            schema_version: CONNECTION_REFERENCE_BUNDLE_SCHEMA_VERSION,
            references,
        };
        bundle.validate()?;
        ensure!(
            serde_json::to_vec(&bundle)?.len() <= MAX_BUNDLE_BYTES,
            "引用导出包超过字节上限"
        );
        Ok(bundle)
    }

    /// 原子整批导入。来源标识只是待重验线索；不接受 resolver、凭据或执行回调。
    /// 全部生成新 ID/代际并保持 unresolved，不能通过 set_enabled 获得 grant 资格。
    pub fn import_refs(&self, bundle: RegistrationReferenceBundle) -> Result<Vec<ConnectionRef>> {
        bundle.validate()?;
        ensure!(
            serde_json::to_vec(&bundle)?.len() <= MAX_BUNDLE_BYTES,
            "引用导入包超过字节上限"
        );
        self.update(|next| {
            ensure!(
                next.registry
                    .registrations()
                    .len()
                    .saturating_add(bundle.references.len())
                    <= MAX_REGISTRATIONS,
                "导入后登记超过上限"
            );
            bundle
                .references
                .into_iter()
                .map(|entry| {
                    let source = unresolved_source(
                        &entry.source,
                        self.instance_id(),
                        &format!("import-{}", Uuid::new_v4()),
                    )?;
                    let name = source.source_id().to_owned();
                    next.registry.register(entry.kind, source, name)
                })
                .collect()
        })
    }

    pub fn import_refs_json(&self, bytes: &[u8]) -> Result<Vec<ConnectionRef>> {
        self.import_refs(RegistrationReferenceBundle::from_json(bytes)?)
    }

    fn update<T>(&self, change: impl FnOnce(&mut StoreState) -> Result<T>) -> Result<T> {
        let mut state = self.inner.state.lock();
        let mut next = state.clone();
        let result = change(&mut next)?;
        next.validate()?;
        let document = next.document();
        ensure!(
            serde_json::to_vec_pretty(&document)?.len() <= MAX_DOCUMENT_BYTES,
            "登记文档超过字节上限"
        );
        self.inner.db.put(COLLECTION, DOCUMENT_ID, &document)?;
        *state = next;
        Ok(result)
    }
}

/// 与本地快照故意不兼容的 refs-only 交换格式；所有层级拒绝未知字段。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", try_from = "RegistrationReferenceBundleData")]
pub struct RegistrationReferenceBundle {
    schema_version: u16,
    references: Vec<RegistrationReferenceEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RegistrationReferenceEntry {
    kind: ConnectionKind,
    source: SourceRef,
}

impl RegistrationReferenceEntry {
    pub fn kind(&self) -> ConnectionKind {
        self.kind
    }

    pub fn source(&self) -> &SourceRef {
        &self.source
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RegistrationReferenceBundleData {
    schema_version: u16,
    references: Vec<RegistrationReferenceEntry>,
}

impl TryFrom<RegistrationReferenceBundleData> for RegistrationReferenceBundle {
    type Error = anyhow::Error;

    fn try_from(value: RegistrationReferenceBundleData) -> Result<Self> {
        let bundle = Self {
            schema_version: value.schema_version,
            references: value.references,
        };
        bundle.validate()?;
        Ok(bundle)
    }
}

impl RegistrationReferenceBundle {
    /// 不受信输入须使用此入口，先限制实际字节，再解码及限制条目数。
    pub fn from_json(bytes: &[u8]) -> Result<Self> {
        ensure!(bytes.len() <= MAX_BUNDLE_BYTES, "引用导入包超过字节上限");
        // 错误不回显不受信字段/值，避免把伪装成引用的秘密带入普通诊断。
        serde_json::from_slice(bytes).map_err(|_| anyhow::anyhow!("引用导入包无效或版本不支持"))
    }

    pub fn references(&self) -> &[RegistrationReferenceEntry] {
        &self.references
    }

    fn validate(&self) -> Result<()> {
        ensure!(
            self.schema_version == CONNECTION_REFERENCE_BUNDLE_SCHEMA_VERSION,
            "不支持的引用包 schemaVersion"
        );
        ensure!(
            self.references.len() <= MAX_REGISTRATIONS,
            "引用包条目超过上限"
        );
        let mut seen = HashSet::new();
        for entry in &self.references {
            ensure!(
                matches!(entry.source.lifecycle(), SourceLifecycle::LegacyUnresolved)
                    && entry.source.revision() == UNRESOLVED_REVISION,
                "引用包不能携带来源生命周期或就绪证据"
            );
            ensure!(seen.insert((entry.kind, &entry.source)), "引用包来源重复");
        }
        Ok(())
    }
}

fn unresolved_source(source: &SourceRef, instance: &str, revision: &str) -> Result<SourceRef> {
    let scope = source.scope();
    SourceRef::new(
        source.kind(),
        source.source_id(),
        SourceScope::new(
            instance,
            scope.authority_id(),
            scope.account_id(),
            scope.project_id().map(str::to_owned),
        )?,
        source.subresource_id().map(str::to_owned),
        SourceLifecycle::LegacyUnresolved,
        revision,
    )
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
    ensure!(
        registration.state() != RegistrationState::Deleted,
        "连接登记已失效"
    );
    Ok(registration)
}

// FsDb 暂无有界单文档读取接口；沿用它的固定文档布局和 put 提交，不用 list 的坏文档跳过语义。
fn read_document<T: DeserializeOwned>(db: &FsDb, id: &str, limit: usize) -> Result<Option<T>> {
    let path = db.root().join(COLLECTION).join(format!("{id}.json"));
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(not(unix))]
    if std::fs::symlink_metadata(&path).is_ok_and(|metadata| !metadata.is_file()) {
        anyhow::bail!("登记文档不是普通文件");
    }
    let file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file() && metadata.len() <= limit as u64,
        "登记文档类型或大小无效"
    );
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= limit, "登记文档超过读取上限");
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|_| anyhow::anyhow!("登记文档无效、损坏或版本不支持"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use std::sync::Barrier;
    use std::thread;

    struct TempRoot(PathBuf);

    impl TempRoot {
        fn new() -> Self {
            #[cfg(unix)]
            let base = PathBuf::from("/tmp/opencode");
            #[cfg(not(unix))]
            let base = std::env::temp_dir();
            let path = base.join(format!("connection-registration-test-{}", Uuid::new_v4()));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn path(&self, id: &str) -> PathBuf {
            self.0.join(COLLECTION).join(format!("{id}.json"))
        }

        fn bytes(&self) -> Vec<u8> {
            std::fs::read(self.path(DOCUMENT_ID)).unwrap()
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            // 只回收本测试创建的仓库外唯一目录，不吞掉夹具清理失败。
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }

    fn source(store: &ConnectionRegistrationStore, account: &str) -> SourceRef {
        SourceRef::new(
            super::super::identity::SourceKind::Software,
            "same-remote-id",
            SourceScope::new(
                store.instance_id(),
                "source-owner",
                account,
                Some("project-a".into()),
            )
            .unwrap(),
            Some("documents".into()),
            SourceLifecycle::known("source-generation-1").unwrap(),
            "source-revision-1",
        )
        .unwrap()
    }

    fn credential(source: &SourceRef) -> CredentialReference {
        CredentialReference::new(source.clone(), Uuid::new_v4().to_string(), 1).unwrap()
    }

    fn register(store: &ConnectionRegistrationStore, account: &str) -> ConnectionRef {
        store
            .register(
                ConnectionKind::Software,
                source(store, account),
                "private-display-name",
            )
            .unwrap()
    }

    #[test]
    fn reopen_preserves_installation_references_states_and_opaque_credentials() {
        let root = TempRoot::new();
        let store = ConnectionRegistrationStore::open(&root.0).unwrap();
        let instance = store.instance_id().to_owned();
        assert_eq!(
            Uuid::parse_str(&instance).unwrap().get_version(),
            Some(uuid::Version::Random)
        );
        let reference = register(&store, "account-a");
        assert_eq!(
            store.get(&reference).unwrap().state(),
            RegistrationState::Disabled
        );
        assert!(store.binding_for(&reference).is_err());
        let secret_ref = credential(&source(&store, "account-a"));
        store
            .set_credential(&reference, Some(secret_ref.clone()))
            .unwrap();
        store.set_enabled(&reference, true).unwrap();
        let binding = store.binding_for(&reference).unwrap();
        let disabled = register(&store, "account-b");
        let snapshot = store.snapshot();
        drop(store);
        let reopened = ConnectionRegistrationStore::open(&root.0).unwrap();
        assert_eq!(reopened.instance_id(), instance);
        assert_eq!(reopened.snapshot(), snapshot);
        assert!(reopened.validate_binding(&binding).is_ok());
        assert!(reopened.binding_for(&disabled).is_err());
        assert_eq!(
            reopened.credential_reference(&reference).unwrap(),
            Some(secret_ref)
        );
    }

    #[test]
    fn corrupted_unknown_or_incomplete_storage_is_not_treated_as_empty() {
        let root = TempRoot::new();
        drop(ConnectionRegistrationStore::open(&root.0).unwrap());
        let original = root.bytes();
        let original_installation = std::fs::read(root.path(INSTALLATION_ID)).unwrap();
        for invalid in [b"{".to_vec(), Vec::new(), b"null".to_vec()] {
            std::fs::write(root.path(DOCUMENT_ID), &invalid).unwrap();
            assert!(ConnectionRegistrationStore::open(&root.0).is_err());
            assert_eq!(root.bytes(), invalid);
        }
        let document: Value = serde_json::from_slice(&original).unwrap();
        for pointer in ["/schemaVersion", "/snapshot/schemaVersion"] {
            let mut invalid = document.clone();
            *invalid.pointer_mut(pointer).unwrap() = json!(99);
            std::fs::write(root.path(DOCUMENT_ID), invalid.to_string()).unwrap();
            assert!(ConnectionRegistrationStore::open(&root.0).is_err());
        }
        std::fs::write(root.path(DOCUMENT_ID), &original).unwrap();
        let mut installation: Value = serde_json::from_slice(&original_installation).unwrap();
        installation["schemaVersion"] = json!(99);
        std::fs::write(root.path(INSTALLATION_ID), installation.to_string()).unwrap();
        assert!(ConnectionRegistrationStore::open(&root.0).is_err());
        installation["schemaVersion"] = json!(1);
        installation["instanceId"] = json!("https://not-an-installation.invalid");
        std::fs::write(root.path(INSTALLATION_ID), installation.to_string()).unwrap();
        assert!(ConnectionRegistrationStore::open(&root.0).is_err());
        std::fs::write(root.path(INSTALLATION_ID), &original_installation).unwrap();
        std::fs::remove_file(root.path(DOCUMENT_ID)).unwrap();
        assert!(ConnectionRegistrationStore::open(&root.0).is_err());
        assert!(!root.path(DOCUMENT_ID).exists());
        std::fs::write(root.path(DOCUMENT_ID), &original).unwrap();
        std::fs::remove_file(root.path(INSTALLATION_ID)).unwrap();
        assert!(ConnectionRegistrationStore::open(&root.0).is_err());
        assert!(!root.path(INSTALLATION_ID).exists());
    }

    #[test]
    fn foreign_local_snapshot_never_replaces_current_installation_identity() {
        let a = TempRoot::new();
        let b = TempRoot::new();
        let source_store = ConnectionRegistrationStore::open(&a.0).unwrap();
        let reference = register(&source_store, "account-a");
        source_store.set_enabled(&reference, true).unwrap();
        let target = ConnectionRegistrationStore::open(&b.0).unwrap();
        let installation = std::fs::read(b.path(INSTALLATION_ID)).unwrap();
        drop(target);
        std::fs::write(b.path(DOCUMENT_ID), a.bytes()).unwrap();
        assert!(ConnectionRegistrationStore::open(&b.0).is_err());
        assert_eq!(
            std::fs::read(b.path(INSTALLATION_ID)).unwrap(),
            installation
        );
        assert_eq!(b.bytes(), a.bytes());
    }

    #[test]
    fn failed_commit_does_not_publish_registry_or_credential_changes() {
        let root = TempRoot::new();
        let store = ConnectionRegistrationStore::open(&root.0).unwrap();
        let reference = register(&store, "account-a");
        let original_credential = credential(&source(&store, "account-a"));
        store
            .set_credential(&reference, Some(original_credential.clone()))
            .unwrap();
        store.set_enabled(&reference, true).unwrap();
        let binding = store.binding_for(&reference).unwrap();
        let snapshot = store.snapshot();
        let before = root.bytes();
        let saved = root.0.join("saved-state.json");
        std::fs::rename(root.path(DOCUMENT_ID), &saved).unwrap();
        std::fs::create_dir(root.path(DOCUMENT_ID)).unwrap(); // rename 的确定性故障屏障。
        assert!(store
            .set_credential(&reference, Some(credential(&source(&store, "account-a"))))
            .is_err());
        assert!(store.delete(&reference).is_err());
        assert!(store
            .register(ConnectionKind::Software, source(&store, "account-b"), "new")
            .is_err());
        assert_eq!(store.snapshot(), snapshot);
        assert!(store.validate_binding(&binding).is_ok());
        assert_eq!(
            store.credential_reference(&reference).unwrap(),
            Some(original_credential)
        );
        std::fs::remove_dir(root.path(DOCUMENT_ID)).unwrap();
        std::fs::rename(saved, root.path(DOCUMENT_ID)).unwrap();
        assert_eq!(root.bytes(), before);
        drop(store);
        assert_eq!(
            ConnectionRegistrationStore::open(&root.0)
                .unwrap()
                .snapshot(),
            snapshot
        );
    }

    #[test]
    fn credentials_require_exact_source_binding_and_non_secret_object_ids() {
        let root = TempRoot::new();
        let store = ConnectionRegistrationStore::open(&root.0).unwrap();
        let reference = register(&store, "account-a");
        let original_source = source(&store, "account-a");
        let before = root.bytes();
        for (pointer, replacement) in [
            ("/kind", json!("plugin")),
            ("/sourceId", json!("other-source")),
            ("/scope/instanceId", json!(Uuid::new_v4().to_string())),
            ("/scope/authorityId", json!("other-owner")),
            ("/scope/accountId", json!("other-account")),
            ("/scope/projectId", json!("other-project")),
            ("/subresourceId", json!("assistant")),
            ("/lifecycle/generation", json!("next-generation")),
            ("/revision", json!("next-revision")),
        ] {
            let mut changed = serde_json::to_value(&original_source).unwrap();
            *changed.pointer_mut(pointer).unwrap() = replacement;
            let changed = credential(&serde_json::from_value(changed).unwrap());
            assert!(
                store.set_credential(&reference, Some(changed)).is_err(),
                "{pointer}"
            );
        }
        for invalid in [
            "https://secret.invalid/key",
            "env:API_KEY",
            "sk-canary-secret",
            "../../key",
            "",
        ] {
            assert!(CredentialReference::new(original_source.clone(), invalid, 1).is_err());
        }
        assert!(
            CredentialReference::new(original_source.clone(), Uuid::nil().to_string(), 1).is_err()
        );
        assert!(
            CredentialReference::new(original_source.clone(), Uuid::new_v4().to_string(), 0)
                .is_err()
        );
        let valid = credential(&original_source);
        for field in ["url", "env", "apiKey", "activeTicket"] {
            let mut invalid = serde_json::to_value(&valid).unwrap();
            invalid[field] = json!("credential-canary");
            assert!(serde_json::from_value::<CredentialReference>(invalid).is_err());
        }
        assert_eq!(root.bytes(), before);
        assert_eq!(store.credential_reference(&reference).unwrap(), None);
    }

    #[test]
    fn credential_changes_and_delete_reregister_do_not_inherit_old_bindings() {
        let root = TempRoot::new();
        let store = ConnectionRegistrationStore::open(&root.0).unwrap();
        let original = register(&store, "account-a");
        let source = source(&store, "account-a");
        let credential = credential(&source);
        store
            .set_credential(&original, Some(credential.clone()))
            .unwrap();
        store.set_enabled(&original, true).unwrap();
        let old_grant_target = store.binding_for(&original).unwrap();
        store
            .set_credential(&original, Some(credential.clone()))
            .unwrap();
        assert!(store.validate_binding(&old_grant_target).is_ok());
        store.set_credential(&original, None).unwrap();
        assert!(store.validate_binding(&old_grant_target).is_err());
        assert_eq!(
            store.get(&original).unwrap().state(),
            RegistrationState::Disabled
        );
        store.set_credential(&original, Some(credential)).unwrap();
        store.set_enabled(&original, true).unwrap();
        assert!(store.validate_binding(&old_grant_target).is_err());
        let before_delete = store.binding_for(&original).unwrap();
        store.delete(&original).unwrap();
        let replacement = store.reregister(&original, source, "same-name").unwrap();
        assert_eq!(original.connection_id(), replacement.connection_id());
        assert_ne!(original.generation(), replacement.generation());
        assert!(store.credential_reference(&original).is_err());
        assert_eq!(store.credential_reference(&replacement).unwrap(), None);
        assert_eq!(
            store.get(&replacement).unwrap().state(),
            RegistrationState::Disabled
        );
        store.set_enabled(&replacement, true).unwrap();
        assert!(store.validate_binding(&before_delete).is_err());
        drop(store);
        let reopened = ConnectionRegistrationStore::open(&root.0).unwrap();
        assert_eq!(reopened.credential_reference(&replacement).unwrap(), None);
        assert!(reopened.validate_binding(&before_delete).is_err());
    }

    #[test]
    fn safe_export_import_has_fresh_current_instance_refs_and_no_usable_authority() {
        let a = TempRoot::new();
        let b = TempRoot::new();
        let source_store = ConnectionRegistrationStore::open(&a.0).unwrap();
        let old_ref = register(&source_store, "account-a");
        let secret_ref = credential(&source(&source_store, "account-a"));
        source_store
            .set_credential(&old_ref, Some(secret_ref.clone()))
            .unwrap();
        source_store.set_enabled(&old_ref, true).unwrap();
        let old_grant_target = source_store.binding_for(&old_ref).unwrap();
        let deleted = register(&source_store, "deleted");
        source_store.delete(&deleted).unwrap();
        let bundle = source_store.export_refs().unwrap();
        assert_eq!(bundle.references().len(), 1);
        let bytes = serde_json::to_vec(&bundle).unwrap();
        let text = std::str::from_utf8(&bytes).unwrap();
        for forbidden in [
            old_ref.connection_id(),
            old_ref.generation(),
            secret_ref.credential_id(),
            "private-display-name",
            "source-generation-1",
            "source-revision-1",
            "credential",
            "grant",
            "activeTicket",
            "callback",
            "cameraConfirmation",
            "enabled",
            "url",
            "env",
            "apiKey",
        ] {
            assert!(!text.contains(forbidden), "{forbidden}");
        }
        let target = ConnectionRegistrationStore::open(&b.0).unwrap();
        let new_refs = target.import_refs_json(&bytes).unwrap();
        let new_ref = &new_refs[0];
        assert_ne!(old_ref.instance_id(), new_ref.instance_id());
        assert_eq!(new_ref.instance_id(), target.instance_id());
        assert_ne!(old_ref.connection_id(), new_ref.connection_id());
        assert_ne!(old_ref.generation(), new_ref.generation());
        let imported = target.get(new_ref).unwrap();
        assert_eq!(imported.state(), RegistrationState::LegacyUnresolved);
        assert_eq!(
            imported.source().scope().instance_id(),
            target.instance_id()
        );
        assert_eq!(imported.source().scope().account_id(), "account-a");
        assert_eq!(target.credential_reference(new_ref).unwrap(), None);
        assert!(
            CredentialReference::new(imported.source().clone(), Uuid::new_v4().to_string(), 1)
                .is_err()
        );
        assert!(target.set_credential(new_ref, Some(secret_ref)).is_err());
        assert!(target.binding_for(new_ref).is_err());
        assert!(target.set_enabled(new_ref, true).is_err());
        assert!(target.validate_binding(&old_grant_target).is_err());
        drop(target);
        let reopened = ConnectionRegistrationStore::open(&b.0).unwrap();
        assert_eq!(reopened.get(new_ref).unwrap(), imported);
        assert!(reopened.validate_binding(&old_grant_target).is_err());
    }

    #[test]
    fn imports_reject_unknown_authority_fields_at_every_level_without_writing() {
        let root = TempRoot::new();
        let store = ConnectionRegistrationStore::open(&root.0).unwrap();
        register(&store, "account-a");
        let bundle = serde_json::to_value(store.export_refs().unwrap()).unwrap();
        let before = root.bytes();
        let snapshot = store.snapshot();
        for field in [
            "grant",
            "grants",
            "activeTicket",
            "activeLease",
            "callback",
            "cameraConfirmation",
            "credentialReference",
        ] {
            for pointer in [
                "",
                "/references/0",
                "/references/0/source",
                "/references/0/source/scope",
                "/references/0/source/lifecycle",
            ] {
                let mut invalid = bundle.clone();
                invalid
                    .pointer_mut(pointer)
                    .unwrap()
                    .as_object_mut()
                    .unwrap()
                    .insert(field.into(), json!("authority-secret-canary"));
                let error = store
                    .import_refs_json(&serde_json::to_vec(&invalid).unwrap())
                    .unwrap_err();
                assert!(!error.to_string().contains("authority-secret-canary"));
            }
        }
        let mut invalid = bundle.clone();
        invalid["schemaVersion"] = json!(99);
        assert!(store
            .import_refs_json(&serde_json::to_vec(&invalid).unwrap())
            .is_err());
        invalid = bundle;
        invalid["references"][0]["source"]["lifecycle"] =
            json!({"state":"known", "generation":"forged-proof"});
        assert!(store
            .import_refs_json(&serde_json::to_vec(&invalid).unwrap())
            .is_err());
        assert!(store
            .import_refs_json(&serde_json::to_vec(&snapshot).unwrap())
            .is_err());
        assert_eq!(store.snapshot(), snapshot);
        assert_eq!(root.bytes(), before);
    }

    #[test]
    fn invalid_persisted_credential_binding_and_authority_fields_fail_closed() {
        let root = TempRoot::new();
        let store = ConnectionRegistrationStore::open(&root.0).unwrap();
        let reference = register(&store, "account-a");
        store
            .set_credential(&reference, Some(credential(&source(&store, "account-a"))))
            .unwrap();
        let original: Value = serde_json::from_slice(&root.bytes()).unwrap();
        drop(store);
        for pointer in [
            "/credentialBindings/0/credential/source/scope/accountId",
            "/credentialBindings/0/connection/generation",
        ] {
            let mut invalid = original.clone();
            *invalid.pointer_mut(pointer).unwrap() = if pointer.ends_with("generation") {
                json!(Uuid::new_v4().to_string())
            } else {
                json!("other-account")
            };
            std::fs::write(root.path(DOCUMENT_ID), invalid.to_string()).unwrap();
            assert!(ConnectionRegistrationStore::open(&root.0).is_err());
        }
        for field in ["grants", "activeTicket", "callback", "cameraConfirmation"] {
            let mut invalid = original.clone();
            invalid[field] = json!("must-not-restore");
            std::fs::write(root.path(DOCUMENT_ID), invalid.to_string()).unwrap();
            assert!(ConnectionRegistrationStore::open(&root.0).is_err());
        }
    }

    #[test]
    fn import_conflict_is_atomic_and_existing_source_files_are_unchanged() {
        let a = TempRoot::new();
        let b = TempRoot::new();
        let source_store = ConnectionRegistrationStore::open(&a.0).unwrap();
        register(&source_store, "new-account"); // 会先在 clone 中成功，第二条才冲突。
        register(&source_store, "existing-account");
        let target = ConnectionRegistrationStore::open(&b.0).unwrap();
        register(&target, "existing-account");
        let sentinel = b.0.join("source-config.json");
        std::fs::write(&sentinel, b"existing-source-config-canary").unwrap();
        let before = b.bytes();
        let snapshot = target.snapshot();
        assert!(target
            .import_refs(source_store.export_refs().unwrap())
            .is_err());
        assert_eq!(target.snapshot(), snapshot);
        assert_eq!(b.bytes(), before);
        assert_eq!(
            std::fs::read(sentinel).unwrap(),
            b"existing-source-config-canary"
        );
    }

    #[test]
    fn actual_bytes_and_entry_counts_are_bounded_and_non_regular_files_are_rejected() {
        let root = TempRoot::new();
        let store = ConnectionRegistrationStore::open(&root.0).unwrap();
        register(&store, "account-a");
        let mut bundle = store.export_refs().unwrap();
        bundle.references = vec![bundle.references[0].clone(); MAX_REGISTRATIONS + 1];
        assert!(store.import_refs(bundle).is_err());
        assert!(store
            .import_refs_json(&vec![b' '; MAX_BUNDLE_BYTES + 1])
            .is_err());
        drop(store);
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(root.path(DOCUMENT_ID))
            .unwrap();
        file.set_len(MAX_DOCUMENT_BYTES as u64 + 1).unwrap();
        drop(file);
        assert!(ConnectionRegistrationStore::open(&root.0).is_err());
        std::fs::remove_file(root.path(DOCUMENT_ID)).unwrap();
        std::fs::create_dir(root.path(DOCUMENT_ID)).unwrap();
        assert!(ConnectionRegistrationStore::open(&root.0).is_err());
    }

    #[test]
    fn clones_and_independent_opens_share_serialized_single_process_commits() {
        let root = TempRoot::new();
        let store = ConnectionRegistrationStore::open(&root.0).unwrap();
        let reopened = ConnectionRegistrationStore::open(root.0.join(".")).unwrap();
        assert!(Arc::ptr_eq(&store.inner, &reopened.inner));
        let barrier = Arc::new(Barrier::new(2));
        let handles: Vec<_> = [store.clone(), reopened]
            .into_iter()
            .map(|writer| {
                let barrier = barrier.clone();
                thread::spawn(move || {
                    barrier.wait();
                    writer.register(
                        ConnectionKind::Software,
                        source(&writer, "account-a"),
                        "same-name",
                    )
                })
            })
            .collect();
        let results: Vec<_> = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect();
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(store.snapshot().registrations().len(), 1);
        let snapshot = store.snapshot();
        drop(store);
        assert_eq!(
            ConnectionRegistrationStore::open(&root.0)
                .unwrap()
                .snapshot(),
            snapshot
        );
    }
}
