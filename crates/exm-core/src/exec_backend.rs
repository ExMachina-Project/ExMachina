//! exec_backend.rs —— capability-parity 7.1–7.4：执行后端能力声明、授权交集选择、
//! 生命周期监督与隔离失败语义。
//!
//! 设计边界：
//! - 本模块只做**选择与协议**，不把某个运行时实现写死；真实启动器实现
//!   [`BackendLauncher`]，由本机受管/隔离容器/远端适配器提供。
//! - 选择是任务授权、项目边界、执行身份、工具效果与后端能力的**交集**；
//!   任何一项不满足都在启动前拒绝，并把每个候选的拒绝原因记入 `rejected`。
//! - 隔离后端不可用时绝不静默回落到宿主执行：`isolationRequired` 请求没有
//!   就绪的隔离后端时返回 `NoReadyBackend`，宿主候选的拒绝原因如实记录。
//! - 超时/断连/回收失败进入 `TimedOut`/`Unknown`，监督器**不自动重放**任何
//!   操作；写操作是否可重试只能由用户/上层在核验后决定。

use crate::execution_context::ExecutionContext;
use crate::types::new_id;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Component, Path};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const EXEC_BACKEND_SCHEMA_VERSION: u16 = 1;

mod remote;
pub use remote::RemoteBackendLauncher;

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn current_platform() -> String {
    std::env::consts::OS.to_string()
}

// ---------------------------------------------------------------- 能力声明

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BackendKind {
    LocalManaged,
    Bubblewrap,
    Container,
    Remote,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum NetworkMode {
    #[default]
    None,
    Loopback,
    Allowlisted,
    Unrestricted,
}

/// 网络能力/需求：`Allowlisted` 时 `hosts` 是允许/请求的主机集合。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct NetworkRequirement {
    #[serde(default)]
    pub mode: NetworkMode,
    #[serde(default)]
    pub hosts: BTreeSet<String>,
}

impl NetworkRequirement {
    pub fn none() -> Self {
        Self::default()
    }

    pub fn loopback() -> Self {
        Self {
            mode: NetworkMode::Loopback,
            hosts: BTreeSet::new(),
        }
    }

    pub fn allowlisted(hosts: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            mode: NetworkMode::Allowlisted,
            hosts: hosts.into_iter().map(Into::into).collect(),
        }
    }

    pub fn unrestricted() -> Self {
        Self {
            mode: NetworkMode::Unrestricted,
            hosts: BTreeSet::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum BrowserCapability {
    Unsupported,
    /// 依赖存在但未验证（缺版本探测/缺真实夹具）——不得标记为就绪。
    #[default]
    Unverified,
    Verified,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum PersistenceCapability {
    #[default]
    Ephemeral,
    ProjectWorkspace,
    NamedVolume,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MountCapability {
    /// 是否支持显式挂载。false = 只能在既定 cwd/根内执行，不接受挂载参数。
    pub supported: bool,
    /// true = 只允许挂载当前项目根内的路径。
    pub project_only: bool,
    /// 是否允许挂载 Docker/容器运行时 socket（默认必须为 false）。
    pub docker_socket: bool,
    /// 是否允许读写挂载（false 时只允许 `readOnly=true`）。
    pub writable: bool,
}

impl Default for MountCapability {
    fn default() -> Self {
        Self {
            supported: false,
            project_only: true,
            docker_socket: false,
            writable: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct IdentityCapability {
    pub uid_gid: bool,
    /// 声明支持特权容器/root（选择层仍默认拒绝提权请求）。
    pub privileged: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceCapability {
    /// None = 后端不声明硬上限（不是「无限授权」）。
    pub memory_mb: Option<u64>,
    pub processes: Option<u32>,
    pub max_timeout_ms: u64,
}

impl Default for ResourceCapability {
    fn default() -> Self {
        Self {
            memory_mb: None,
            processes: None,
            max_timeout_ms: 86_400_000,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackendCapabilities {
    pub platforms: BTreeSet<String>,
    pub tools: BTreeSet<String>,
    pub network: NetworkRequirement,
    /// 能否实际强制执行请求的网络边界；宿主直连不能冒充断网/白名单沙箱。
    pub network_enforced: bool,
    pub mounts: MountCapability,
    pub identity: IdentityCapability,
    pub resources: ResourceCapability,
    pub persistence: PersistenceCapability,
    pub cancellation: bool,
    pub browser: BrowserCapability,
    /// 是否提供独立于宿主文件系统/进程的隔离边界。
    pub isolated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum BackendReadiness {
    Ready,
    MissingDependency { dependency: String },
    Unverified { detail: String },
    Disabled { detail: String },
}

impl BackendReadiness {
    pub fn is_ready(&self) -> bool {
        matches!(self, Self::Ready)
    }

    pub fn detail(&self) -> String {
        match self {
            Self::Ready => "ready".into(),
            Self::MissingDependency { dependency } => format!("缺少依赖：{dependency}"),
            Self::Unverified { detail } => format!("未验证：{detail}"),
            Self::Disabled { detail } => format!("已停用：{detail}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionBackendDescriptor {
    pub schema_version: u16,
    pub id: String,
    pub kind: BackendKind,
    pub display_name: String,
    pub capabilities: BackendCapabilities,
    pub readiness: BackendReadiness,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackendSelfCheck {
    pub schema_version: u16,
    pub backend: ExecutionBackendDescriptor,
    pub dependency_available: Option<bool>,
    pub ready: bool,
    pub detail: String,
}

impl ExecutionBackendDescriptor {
    pub fn ready(&self) -> bool {
        self.readiness.is_ready()
    }
}

fn backend(
    id: &str,
    kind: BackendKind,
    display_name: &str,
    capabilities: BackendCapabilities,
    readiness: BackendReadiness,
) -> ExecutionBackendDescriptor {
    ExecutionBackendDescriptor {
        schema_version: EXEC_BACKEND_SCHEMA_VERSION,
        id: id.into(),
        kind,
        display_name: display_name.into(),
        capabilities,
        readiness,
    }
}

fn tools(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|name| (*name).to_string()).collect()
}

fn platform_set() -> BTreeSet<String> {
    [current_platform()].into_iter().collect()
}

#[derive(Debug, Clone, Default)]
pub struct ExecutionBackendRegistry {
    backends: BTreeMap<String, ExecutionBackendDescriptor>,
}

impl ExecutionBackendRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// 注册后端；id 只接受 `[A-Za-z0-9_-]`，重复 id 拒绝（不会后者覆盖前者）。
    pub fn register(&mut self, descriptor: ExecutionBackendDescriptor) -> Result<(), String> {
        if descriptor.schema_version != EXEC_BACKEND_SCHEMA_VERSION
            || descriptor.capabilities.platforms.is_empty()
            || descriptor.capabilities.tools.is_empty()
            || descriptor.capabilities.resources.max_timeout_ms == 0
        {
            return Err("后端版本或能力声明不完整".into());
        }
        if descriptor.id.is_empty()
            || !descriptor
                .id
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
        {
            return Err(format!("后端 id 非法：{}", descriptor.id));
        }
        if self.backends.contains_key(&descriptor.id) {
            return Err(format!("后端 id 重复：{}", descriptor.id));
        }
        self.backends.insert(descriptor.id.clone(), descriptor);
        Ok(())
    }

    pub fn get(&self, id: &str) -> Option<&ExecutionBackendDescriptor> {
        self.backends.get(id)
    }

    pub fn list(&self) -> Vec<ExecutionBackendDescriptor> {
        self.backends.values().cloned().collect()
    }

    /// 内置后端目录（只声明能力与就绪状态，不启动任何作业）。
    ///
    /// - `local-managed`：宿主受管执行，非隔离；浏览器依赖独立未验证。
    /// - `bubblewrap`：文件/进程隔离；`bwrap` 不可用即未就绪。
    /// - `container`：隔离容器适配器；运行时未探测到即未就绪，网络默认关闭。
    /// - `remote`：远端工作者；未注册工作者即未就绪。
    pub fn builtin(bwrap_ready: bool, container_ready: bool, remote_ready: bool) -> Self {
        let mut registry = Self::new();
        registry
            .register(backend(
                "local-managed",
                BackendKind::LocalManaged,
                "本机受管执行",
                BackendCapabilities {
                    platforms: platform_set(),
                    tools: tools(&[
                        "terminal",
                        "read",
                        "filesystem",
                        "edit",
                        "patch",
                        "grep",
                        "glob",
                        "git",
                    ]),
                    network: NetworkRequirement::unrestricted(),
                    network_enforced: false,
                    mounts: MountCapability::default(),
                    identity: IdentityCapability {
                        uid_gid: cfg!(unix),
                        privileged: false,
                    },
                    resources: ResourceCapability::default(),
                    persistence: PersistenceCapability::ProjectWorkspace,
                    cancellation: true,
                    browser: BrowserCapability::Unverified,
                    isolated: false,
                },
                BackendReadiness::Ready,
            ))
            .expect("内置后端");
        registry
            .register(backend(
                "bubblewrap",
                BackendKind::Bubblewrap,
                "bubblewrap 隔离执行",
                BackendCapabilities {
                    platforms: ["linux".to_string()].into_iter().collect(),
                    tools: tools(&[
                        "terminal",
                        "read",
                        "filesystem",
                        "edit",
                        "patch",
                        "grep",
                        "glob",
                        "git",
                    ]),
                    network: NetworkRequirement::none(),
                    network_enforced: true,
                    mounts: MountCapability {
                        supported: false,
                        ..MountCapability::default()
                    },
                    identity: IdentityCapability {
                        uid_gid: cfg!(unix),
                        privileged: false,
                    },
                    resources: ResourceCapability::default(),
                    persistence: PersistenceCapability::ProjectWorkspace,
                    cancellation: true,
                    browser: BrowserCapability::Unverified,
                    isolated: true,
                },
                if bwrap_ready {
                    BackendReadiness::Ready
                } else {
                    BackendReadiness::MissingDependency {
                        dependency: "bwrap".into(),
                    }
                },
            ))
            .expect("内置后端");
        registry
            .register(backend(
                "container",
                BackendKind::Container,
                "隔离容器执行",
                BackendCapabilities {
                    platforms: platform_set(),
                    tools: tools(&[
                        "terminal",
                        "read",
                        "filesystem",
                        "edit",
                        "patch",
                        "grep",
                        "glob",
                        "git",
                        "browser",
                    ]),
                    // 容器默认不开放网络；需要网络的运维注册显式 Allowlisted 后端。
                    network: NetworkRequirement::none(),
                    network_enforced: true,
                    mounts: MountCapability {
                        supported: true,
                        project_only: true,
                        docker_socket: false,
                        writable: true,
                    },
                    identity: IdentityCapability {
                        uid_gid: true,
                        privileged: false,
                    },
                    resources: ResourceCapability {
                        memory_mb: Some(4096),
                        processes: Some(512),
                        max_timeout_ms: 3_600_000,
                    },
                    persistence: PersistenceCapability::ProjectWorkspace,
                    cancellation: true,
                    browser: BrowserCapability::Unverified,
                    isolated: true,
                },
                if container_ready {
                    BackendReadiness::Ready
                } else {
                    BackendReadiness::MissingDependency {
                        dependency: "container-runtime".into(),
                    }
                },
            ))
            .expect("内置后端");
        registry
            .register(backend(
                "remote",
                BackendKind::Remote,
                "远端工作者",
                BackendCapabilities {
                    platforms: platform_set(),
                    tools: tools(&[
                        "terminal",
                        "read",
                        "filesystem",
                        "edit",
                        "patch",
                        "grep",
                        "glob",
                        "git",
                    ]),
                    network: NetworkRequirement::none(),
                    network_enforced: true,
                    mounts: MountCapability::default(),
                    identity: IdentityCapability {
                        uid_gid: false,
                        privileged: false,
                    },
                    resources: ResourceCapability {
                        memory_mb: None,
                        processes: None,
                        max_timeout_ms: 3_600_000,
                    },
                    persistence: PersistenceCapability::Ephemeral,
                    cancellation: true,
                    browser: BrowserCapability::Unverified,
                    isolated: true,
                },
                if remote_ready {
                    BackendReadiness::Ready
                } else {
                    BackendReadiness::Unverified {
                        detail: "没有已注册的远程工作者".into(),
                    }
                },
            ))
            .expect("内置后端");
        registry
    }
}

// ---------------------------------------------------------------- 选择请求

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MountRequest {
    /// 宿主路径（绝对路径；必须位于项目根内；禁止 .. 段）。
    pub host_path: String,
    #[serde(default)]
    pub read_only: bool,
    #[serde(default)]
    pub purpose: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct IdentityRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gid: Option<u32>,
    /// 特权容器/宿主 root 请求——选择层始终拒绝，不因后端声明支持而放行。
    #[serde(default)]
    pub elevated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ResourceRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_mb: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub processes: Option<u32>,
    #[serde(default)]
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackendSelectionRequest {
    /// 只用于与服务端可信上下文对账，不从这里授予任何权限。
    pub execution: ExecutionContext,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(default)]
    pub required_tools: BTreeSet<String>,
    #[serde(default)]
    pub browser: bool,
    #[serde(default)]
    pub network: NetworkRequirement,
    #[serde(default)]
    pub mounts: Vec<MountRequest>,
    #[serde(default)]
    pub identity: IdentityRequest,
    #[serde(default)]
    pub resources: ResourceRequest,
    #[serde(default)]
    pub persistence: PersistenceCapability,
    /// 是否需要可取消（长任务/远端作业）。
    #[serde(default)]
    pub cancellation: bool,
    /// 本任务是否声明外部副作用（发布/发送/写第三方）。
    #[serde(default)]
    pub external_effects: bool,
    /// true = 只接受隔离后端；没有就绪的隔离后端即失败，绝不回落到宿主。
    #[serde(default)]
    pub isolation_required: bool,
    /// 候选优先顺序（只影响顺序，不豁免任何能力/授权检查）。
    #[serde(default)]
    pub preferred: Vec<String>,
    /// 用户明确选择某后端时只尝试它；失败不静默使用其他后端。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required_backend: Option<String>,
}

/// 由服务端从真实任务/工具授权构造，不可从请求 JSON 反序列化。
/// 执行身份、网络批准及效果判断属于调用网关，而不是 Agent 自报参数。
#[derive(Debug, Clone)]
pub struct BackendAuthorization {
    execution: ExecutionContext,
    project_id: Option<String>,
    identity: IdentityRequest,
    network: NetworkRequirement,
    external_effects: bool,
}

impl BackendAuthorization {
    pub fn new(
        execution: ExecutionContext,
        project_id: Option<String>,
        identity: IdentityRequest,
    ) -> Result<Self, BackendSelectionError> {
        execution.ensure_current().map_err(|error| {
            selection_error(BackendRejectCode::InvalidExecution, error.to_string())
        })?;
        Ok(Self {
            execution,
            project_id,
            identity,
            network: NetworkRequirement::none(),
            external_effects: false,
        })
    }

    /// 只由用户/任务授权层调用，不接受模型自行提供的批准清单。
    pub fn with_network(mut self, approved: NetworkRequirement) -> Self {
        self.network = approved;
        self
    }

    /// 由既有工具效果判定层设置；请求不能把真实外部写降为只读。
    pub fn with_external_effects(mut self, external: bool) -> Self {
        self.external_effects = external;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendRejectCode {
    InvalidExecution,
    InvalidRequest,
    AuthorizationMismatch,
    ProjectScopeRequired,
    OutsideProjectMount,
    DockerSocketMount,
    UnapprovedNetwork,
    PrivilegedIdentity,
    ToolNotAuthorized,
    ExternalWritesNotAuthorized,
    NoReadyBackend,
    IsolationRequired,
    BackendNotReady,
    PlatformUnsupported,
    ToolUnsupported,
    BrowserUnverified,
    NetworkUnsupported,
    NetworkHostsNotAllowed,
    MountsUnsupported,
    IdentityUnsupported,
    ResourceLimit,
    PersistenceUnsupported,
    CancellationUnsupported,
}

impl BackendRejectCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InvalidExecution => "invalid_execution",
            Self::InvalidRequest => "invalid_request",
            Self::AuthorizationMismatch => "authorization_mismatch",
            Self::ProjectScopeRequired => "project_scope_required",
            Self::OutsideProjectMount => "outside_project_mount",
            Self::DockerSocketMount => "docker_socket_mount",
            Self::UnapprovedNetwork => "unapproved_network",
            Self::PrivilegedIdentity => "privileged_identity",
            Self::ToolNotAuthorized => "tool_not_authorized",
            Self::ExternalWritesNotAuthorized => "external_writes_not_authorized",
            Self::NoReadyBackend => "no_ready_backend",
            Self::IsolationRequired => "isolation_required",
            Self::BackendNotReady => "backend_not_ready",
            Self::PlatformUnsupported => "platform_unsupported",
            Self::ToolUnsupported => "tool_unsupported",
            Self::BrowserUnverified => "browser_unverified",
            Self::NetworkUnsupported => "network_unsupported",
            Self::NetworkHostsNotAllowed => "network_hosts_not_allowed",
            Self::MountsUnsupported => "mounts_unsupported",
            Self::IdentityUnsupported => "identity_unsupported",
            Self::ResourceLimit => "resource_limit",
            Self::PersistenceUnsupported => "persistence_unsupported",
            Self::CancellationUnsupported => "cancellation_unsupported",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RejectedBackend {
    pub backend_id: String,
    pub code: BackendRejectCode,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackendSelectionError {
    pub code: BackendRejectCode,
    pub detail: String,
    #[serde(default)]
    pub rejected: Vec<RejectedBackend>,
}

impl fmt::Display for BackendSelectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code.as_str(), self.detail)
    }
}

impl std::error::Error for BackendSelectionError {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackendExecutionBinding {
    pub root_execution_id: String,
    pub execution_id: String,
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackendSelection {
    pub schema_version: u16,
    pub backend: ExecutionBackendDescriptor,
    pub binding: BackendExecutionBinding,
    pub network: NetworkRequirement,
    pub mounts: Vec<MountRequest>,
    pub identity: IdentityRequest,
    pub resources: ResourceRequest,
    pub browser: bool,
    pub isolation_required: bool,
    #[serde(default)]
    pub rejected: Vec<RejectedBackend>,
}

fn selection_error(code: BackendRejectCode, detail: impl Into<String>) -> BackendSelectionError {
    BackendSelectionError {
        code,
        detail: detail.into(),
        rejected: Vec::new(),
    }
}

fn is_docker_socket(path: &str) -> bool {
    path.to_ascii_lowercase().ends_with("docker.sock")
}

fn path_within_project(path: &Path, project: &Path) -> bool {
    if !path.starts_with(project) {
        return false;
    }
    // 在任何启动/挂载前重查：词法前缀不能阻止符号链接逃逸。
    let (Ok(real_path), Ok(real_project)) = (path.canonicalize(), project.canonicalize()) else {
        return false;
    };
    if !real_path.starts_with(real_project) {
        return false;
    }
    let mut current = std::path::PathBuf::new();
    for component in path.components() {
        current.push(component);
        if std::fs::symlink_metadata(&current).is_ok_and(|meta| meta.file_type().is_symlink()) {
            return false;
        }
    }
    true
}

fn validate_request(
    authority: &BackendAuthorization,
    request: &BackendSelectionRequest,
) -> Result<(), BackendSelectionError> {
    request
        .execution
        .ensure_current()
        .map_err(|error| selection_error(BackendRejectCode::InvalidExecution, error.to_string()))?;
    if request.execution != authority.execution || request.project_id != authority.project_id {
        return Err(selection_error(
            BackendRejectCode::AuthorizationMismatch,
            "请求与服务端任务的执行/会话/项目/授权绑定不一致",
        ));
    }
    if request.execution.cancellation.explicitly_cancelled
        || !request.execution.budget.can_consume(1)
    {
        return Err(selection_error(
            BackendRejectCode::InvalidExecution,
            "执行已取消或预算耗尽",
        ));
    }
    if request.resources.timeout_ms == 0 || request.required_tools.is_empty() {
        return Err(selection_error(
            BackendRejectCode::InvalidRequest,
            "工具与有界超时不能为空",
        ));
    }
    if request.resources.memory_mb == Some(0) || request.resources.processes == Some(0) {
        return Err(selection_error(
            BackendRejectCode::InvalidRequest,
            "资源限制不能以 0 冒充无界授权",
        ));
    }
    if request.execution.session_id.trim().is_empty()
        || request.execution.root_execution_id.trim().is_empty()
        || request.execution.execution_id.trim().is_empty()
    {
        return Err(selection_error(
            BackendRejectCode::InvalidExecution,
            "执行上下文缺少 session/rootExecution/execution 绑定",
        ));
    }

    if request.identity.uid.is_some() != request.identity.gid.is_some() {
        return Err(selection_error(
            BackendRejectCode::InvalidRequest,
            "uid/gid 必须同时提供或同时缺省",
        ));
    }
    if request.identity.elevated || request.identity.uid == Some(0) {
        return Err(selection_error(
            BackendRejectCode::PrivilegedIdentity,
            "执行后端选择拒绝特权/root 身份请求",
        ));
    }
    if request.identity.elevated
        || (request.identity.uid.is_some() && request.identity != authority.identity)
    {
        return Err(selection_error(
            BackendRejectCode::PrivilegedIdentity,
            "请求身份与任务批准的执行身份不同",
        ));
    }

    for mount in &request.mounts {
        let raw = mount.host_path.trim();
        if raw.is_empty()
            || raw.contains('\0')
            || !Path::new(raw).is_absolute()
            || Path::new(raw)
                .components()
                .any(|component| matches!(component, Component::ParentDir))
        {
            return Err(selection_error(
                BackendRejectCode::InvalidRequest,
                format!("挂载路径必须是干净的绝对路径：{}", mount.host_path),
            ));
        }
        if is_docker_socket(raw) {
            return Err(selection_error(
                BackendRejectCode::DockerSocketMount,
                format!("拒绝挂载容器运行时 socket：{raw}"),
            ));
        }
        let Some(project) = request.execution.workspace.project_root.as_deref() else {
            return Err(selection_error(
                BackendRejectCode::ProjectScopeRequired,
                format!("挂载 {raw} 需要已绑定项目根"),
            ));
        };
        if !path_within_project(Path::new(raw), Path::new(project)) {
            return Err(selection_error(
                BackendRejectCode::OutsideProjectMount,
                format!("挂载 {raw} 位于项目根 {project} 之外"),
            ));
        }
    }

    if request.network.mode == NetworkMode::Allowlisted && request.network.hosts.is_empty() {
        return Err(selection_error(
            BackendRejectCode::InvalidRequest,
            "Allowlisted 网络请求必须列出主机",
        ));
    }
    if let Err((_, detail)) = network_compatible(&authority.network, &request.network) {
        return Err(selection_error(
            BackendRejectCode::UnapprovedNetwork,
            detail,
        ));
    }

    if (request.external_effects || authority.external_effects)
        && !authority.execution.authorization.allow_external_writes
    {
        return Err(selection_error(
            BackendRejectCode::ExternalWritesNotAuthorized,
            "任务声明外部副作用，但执行授权未允许外部写",
        ));
    }

    for tool in &request.required_tools {
        if !request.execution.authorization.allows(tool) {
            return Err(selection_error(
                BackendRejectCode::ToolNotAuthorized,
                format!("工具未获任务授权：{tool}"),
            ));
        }
    }
    Ok(())
}

fn network_compatible(
    backend: &NetworkRequirement,
    requested: &NetworkRequirement,
) -> Result<(), (BackendRejectCode, String)> {
    match requested.mode {
        NetworkMode::None => Ok(()),
        NetworkMode::Loopback => match backend.mode {
            NetworkMode::None => Err((
                BackendRejectCode::NetworkUnsupported,
                "后端未声明 loopback 网络".into(),
            )),
            _ => Ok(()),
        },
        NetworkMode::Allowlisted => match backend.mode {
            NetworkMode::Unrestricted => Ok(()),
            NetworkMode::Allowlisted => {
                let missing: Vec<&str> = requested
                    .hosts
                    .difference(&backend.hosts)
                    .map(String::as_str)
                    .collect();
                if missing.is_empty() {
                    Ok(())
                } else {
                    Err((
                        BackendRejectCode::NetworkHostsNotAllowed,
                        format!("后端网络清单不含：{}", missing.join(",")),
                    ))
                }
            }
            NetworkMode::None | NetworkMode::Loopback => Err((
                BackendRejectCode::NetworkUnsupported,
                "后端网络能力低于任务要求".into(),
            )),
        },
        NetworkMode::Unrestricted => {
            if backend.mode == NetworkMode::Unrestricted {
                Ok(())
            } else {
                Err((
                    BackendRejectCode::NetworkUnsupported,
                    "后端不允许无限制网络".into(),
                ))
            }
        }
    }
}

fn candidate_rejection(
    descriptor: &ExecutionBackendDescriptor,
    authority: &BackendAuthorization,
    request: &BackendSelectionRequest,
) -> Option<(BackendRejectCode, String)> {
    if !descriptor.readiness.is_ready() {
        return Some((
            BackendRejectCode::BackendNotReady,
            descriptor.readiness.detail(),
        ));
    }
    if request.isolation_required && !descriptor.capabilities.isolated {
        return Some((
            BackendRejectCode::IsolationRequired,
            "任务要求隔离后端；宿主执行不得作为回退".into(),
        ));
    }
    if !descriptor
        .capabilities
        .platforms
        .contains(&current_platform())
    {
        return Some((
            BackendRejectCode::PlatformUnsupported,
            format!("后端不支持平台 {platform}", platform = current_platform()),
        ));
    }
    let missing_tools: Vec<&str> = request
        .required_tools
        .difference(&descriptor.capabilities.tools)
        .map(String::as_str)
        .collect();
    if !missing_tools.is_empty() {
        return Some((
            BackendRejectCode::ToolUnsupported,
            format!("后端缺少工具：{}", missing_tools.join(",")),
        ));
    }
    if request.browser && descriptor.capabilities.browser != BrowserCapability::Verified {
        return Some((
            BackendRejectCode::BrowserUnverified,
            "浏览器依赖未验证，拒绝标记为就绪".into(),
        ));
    }
    if !descriptor.capabilities.network_enforced
        && request.network.mode != NetworkMode::Unrestricted
    {
        return Some((
            BackendRejectCode::NetworkUnsupported,
            "后端不能强制任务要求的网络边界".into(),
        ));
    }
    if let Err((code, detail)) =
        network_compatible(&descriptor.capabilities.network, &request.network)
    {
        return Some((code, detail));
    }
    if !request.mounts.is_empty() {
        if !descriptor.capabilities.mounts.supported {
            return Some((
                BackendRejectCode::MountsUnsupported,
                "后端不接受显式挂载".into(),
            ));
        }
        if request.mounts.iter().any(|mount| !mount.read_only)
            && !descriptor.capabilities.mounts.writable
        {
            return Some((
                BackendRejectCode::MountsUnsupported,
                "后端只允许只读挂载".into(),
            ));
        }
    }
    if authority.identity.uid.is_some() && !descriptor.capabilities.identity.uid_gid {
        return Some((
            BackendRejectCode::IdentityUnsupported,
            "后端不支持指定 uid/gid".into(),
        ));
    }
    if let Some(memory) = request.resources.memory_mb {
        match descriptor.capabilities.resources.memory_mb {
            Some(max) if memory <= max => {}
            _ => {
                return Some((
                    BackendRejectCode::ResourceLimit,
                    format!("后端不能保证内存上限 {memory}MB"),
                ));
            }
        }
    }
    if let Some(processes) = request.resources.processes {
        if descriptor
            .capabilities
            .resources
            .processes
            .is_none_or(|max| processes > max)
        {
            return Some((
                BackendRejectCode::ResourceLimit,
                "后端不能保证请求的进程数上限".into(),
            ));
        }
    }
    if request.resources.timeout_ms > descriptor.capabilities.resources.max_timeout_ms {
        return Some((
            BackendRejectCode::ResourceLimit,
            format!(
                "超时 {}ms 超过后端上限 {}ms",
                request.resources.timeout_ms, descriptor.capabilities.resources.max_timeout_ms
            ),
        ));
    }
    match request.persistence {
        PersistenceCapability::Ephemeral => {}
        PersistenceCapability::ProjectWorkspace => {
            if descriptor.capabilities.persistence == PersistenceCapability::Ephemeral {
                return Some((
                    BackendRejectCode::PersistenceUnsupported,
                    "后端只提供临时持久化，不能满足项目工作区要求".into(),
                ));
            }
        }
        PersistenceCapability::NamedVolume => {
            if descriptor.capabilities.persistence != PersistenceCapability::NamedVolume {
                return Some((
                    BackendRejectCode::PersistenceUnsupported,
                    "后端不提供命名卷持久化".into(),
                ));
            }
        }
    }
    if request.cancellation && !descriptor.capabilities.cancellation {
        return Some((
            BackendRejectCode::CancellationUnsupported,
            "后端不支持有界取消".into(),
        ));
    }
    None
}

impl ExecutionBackendRegistry {
    /// 能力/授权交集选择。每个候选的失败原因都保留；选择只影响顺序，不豁免检查。
    pub fn select(
        &self,
        authority: &BackendAuthorization,
        request: &BackendSelectionRequest,
    ) -> Result<BackendSelection, BackendSelectionError> {
        validate_request(authority, request)?;

        let mut order: Vec<String> = Vec::new();
        for id in &request.preferred {
            if self.backends.contains_key(id) && !order.contains(id) {
                order.push(id.clone());
            }
        }
        let default_order: &[&str] = if request.isolation_required {
            &["bubblewrap", "container", "remote", "local-managed"]
        } else {
            &["local-managed", "bubblewrap", "container", "remote"]
        };
        for id in default_order {
            if self.backends.contains_key(*id) && !order.iter().any(|seen| seen == id) {
                order.push((*id).to_string());
            }
        }
        for id in self.backends.keys() {
            if !order.iter().any(|seen| seen == id) {
                order.push(id.clone());
            }
        }

        let mut rejected = Vec::new();
        let mut chosen: Option<ExecutionBackendDescriptor> = None;
        for id in order {
            if request
                .required_backend
                .as_ref()
                .is_some_and(|required| required != &id)
            {
                continue;
            }
            let Some(descriptor) = self.backends.get(&id) else {
                continue;
            };
            match candidate_rejection(descriptor, authority, request) {
                Some((code, detail)) => rejected.push(RejectedBackend {
                    backend_id: id,
                    code,
                    detail,
                }),
                None => {
                    if chosen.is_none() {
                        chosen = Some(descriptor.clone());
                    }
                }
            }
        }

        let Some(backend) = chosen else {
            return Err(BackendSelectionError {
                code: BackendRejectCode::NoReadyBackend,
                detail: if request.isolation_required {
                    "没有就绪且满足约束的隔离后端；拒绝回落到宿主执行".into()
                } else {
                    "没有满足任务约束的执行后端".into()
                },
                rejected,
            });
        };
        Ok(BackendSelection {
            schema_version: EXEC_BACKEND_SCHEMA_VERSION,
            backend,
            binding: BackendExecutionBinding {
                root_execution_id: request.execution.root_execution_id.clone(),
                execution_id: request.execution.execution_id.clone(),
                session_id: request.execution.session_id.clone(),
                project_id: request.project_id.clone(),
            },
            network: request.network.clone(),
            mounts: request.mounts.clone(),
            identity: authority.identity.clone(),
            resources: request.resources.clone(),
            browser: request.browser,
            isolation_required: request.isolation_required,
            rejected,
        })
    }
}

/// 严格沙箱 + 配置要求 bwrap 但探测不到时，必须拒绝宿主回退。
/// 独立成纯函数便于测试策略本身，不依赖 CI 是否安装 bwrap。
pub fn strict_isolation_refusal(
    strict: bool,
    use_bwrap: bool,
    bwrap_ready: bool,
) -> Option<&'static str> {
    if strict && use_bwrap && !bwrap_ready {
        Some("严格沙箱模式要求隔离后端，但 bwrap 不可用；拒绝静默回落到宿主执行")
    } else {
        None
    }
}

// ---------------------------------------------------------------- 生命周期

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum JobOperation {
    Read,
    Write,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum JobState {
    Starting,
    Running,
    Succeeded,
    Failed,
    TimedOut,
    Cancelled,
    /// 远端断连/回收失败等无法确认的终态；必须人工/查询核验，禁止自动重放。
    Unknown,
}

impl JobState {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::TimedOut | Self::Cancelled | Self::Unknown
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobEndReason {
    Completed,
    ExitFailure,
    DeadlineExceeded,
    CancelRequested,
    CancelFailed,
    BackendDisconnected,
    ReapFailed,
    StartFailed,
    ProtocolMismatch,
    InvalidEvidence,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobSpec {
    pub execution: ExecutionContext,
    pub project_id: Option<String>,
    pub backend_id: String,
    pub operation: JobOperation,
    /// 真实工具调用/任务动作 ID；同执行内重复提交只查询原作业，不再次启动。
    pub operation_id: String,
    /// 启动参数（由具体启动器解释；监督器不拼接 shell）。
    pub command: Vec<String>,
    pub timeout_ms: u64,
    /// 期望/已知产物引用（绝对路径或后端内引用；终态时与回执合并）。
    pub artifacts: Vec<String>,
    /// 已脱敏的作业摘要，用于审计与快照。
    pub summary: String,
}

/// 已经走完可信授权与后端选择的启动描述；私有字段防止调用者跳过选择直接启动。
#[derive(Debug, Clone)]
pub struct PreparedJob {
    spec: JobSpec,
    selection: BackendSelection,
    authority: BackendAuthorization,
    request: BackendSelectionRequest,
}

impl PreparedJob {
    pub fn spec(&self) -> &JobSpec {
        &self.spec
    }
    pub fn selection(&self) -> &BackendSelection {
        &self.selection
    }
}

impl ExecutionBackendRegistry {
    pub fn prepare_job(
        &self,
        authority: &BackendAuthorization,
        request: &BackendSelectionRequest,
        spec: JobSpec,
    ) -> Result<PreparedJob, BackendSelectionError> {
        let selection = self.select(authority, request)?;
        if spec.execution != authority.execution
            || spec.project_id != authority.project_id
            || spec.backend_id != selection.backend.id
        {
            return Err(selection_error(
                BackendRejectCode::AuthorizationMismatch,
                "启动描述与已选择的任务/后端不一致",
            ));
        }
        if spec.operation_id.is_empty()
            || spec.operation_id.len() > 128
            || !spec
                .operation_id
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
            || !request.required_tools.contains("terminal")
            || spec.command.is_empty()
            || spec.command[0].is_empty()
            || spec.command.iter().any(|arg| arg.contains('\0'))
            || spec.command.iter().map(String::len).sum::<usize>() > 65_536
            || spec.timeout_ms == 0
            || spec.timeout_ms > request.resources.timeout_ms
        {
            return Err(selection_error(
                BackendRejectCode::InvalidRequest,
                "命令启动必须有 terminal 授权、有效 argv 及任务范围内的超时",
            ));
        }
        Ok(PreparedJob {
            spec,
            selection,
            authority: authority.clone(),
            request: request.clone(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct JobEvidence {
    pub backend_id: String,
    pub root_execution_id: String,
    pub execution_id: String,
    pub evidence_session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_execution_id: Option<String>,
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_root: Option<String>,
    #[serde(default)]
    pub artifacts: Vec<String>,
    #[serde(default)]
    pub logs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobSnapshot {
    pub schema_version: u16,
    pub job_id: String,
    pub operation_id: String,
    pub state: JobState,
    pub operation: JobOperation,
    pub started_at_ms: u64,
    pub deadline_ms: u64,
    pub cancel_requested: bool,
    /// 每次都从 1 开始；监督器从不自动重放，超时/断连后仍为 1。
    pub attempts: u32,
    pub reaped: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_reason: Option<JobEndReason>,
    pub evidence: JobEvidence,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recoverable_action: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendJobHandle {
    pub job_id: String,
    pub backend_id: String,
    /// 启动器私有引用（容器 id / 远端作业号）；不向用户暴露为可猜测句柄。
    pub reference: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum BackendJobPoll {
    Running,
    Exited {
        code: i32,
        artifacts: Vec<String>,
        logs: Vec<String>,
    },
    Disconnected {
        detail: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobOutcome {
    /// 回收成功不等于命令成功；没有可核验退出码时保持 None/unknown。
    pub code: Option<i32>,
    pub artifacts: Vec<String>,
    pub logs: Vec<String>,
}

/// 真实后端适配器实现：容器运行时、远端工作者或本机受管进程。
/// 方法不得阻塞线程；启动使用 job_id 作幂等键，future 被丢弃后仍须允许按此键取消/查询。
#[async_trait::async_trait]
pub trait BackendLauncher: Send + Sync {
    fn backend_id(&self) -> &str;
    async fn start(&self, job: &PreparedJob, job_id: &str) -> Result<BackendJobHandle, String>;
    async fn poll(&self, handle: &BackendJobHandle) -> BackendJobPoll;
    async fn cancel(&self, handle: &BackendJobHandle) -> Result<(), String>;
    async fn reap(&self, handle: &BackendJobHandle) -> Result<JobOutcome, String>;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobStartError {
    pub code: String,
    pub backend_id: String,
    pub job_id: Option<String>,
    pub detail: String,
    pub recoverable_action: String,
}

impl fmt::Display for JobStartError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.detail)
    }
}

impl std::error::Error for JobStartError {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobLifecycleError {
    pub code: String,
    pub detail: String,
}

impl fmt::Display for JobLifecycleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.detail)
    }
}

impl std::error::Error for JobLifecycleError {}

struct JobRecord {
    snapshot: JobSnapshot,
    handle: BackendJobHandle,
    cancellation_id: String,
    deadline: tokio::time::Instant,
    finished: Arc<tokio::sync::Notify>,
    spec: JobSpec,
}

type JobObserver = Arc<dyn Fn(&JobSnapshot) + Send + Sync>;

struct SupervisorInner {
    launcher: Arc<dyn BackendLauncher>,
    jobs: std::sync::Mutex<BTreeMap<String, Arc<tokio::sync::Mutex<JobRecord>>>>,
    admissions: std::sync::Mutex<BTreeMap<(String, String), String>>,
    start_timeout: Duration,
    control_timeout: Duration,
    observer: Option<JobObserver>,
}

#[derive(Clone)]
pub struct ExecJobSupervisor {
    inner: Arc<SupervisorInner>,
}

impl ExecJobSupervisor {
    pub fn new(launcher: Arc<dyn BackendLauncher>) -> Self {
        Self::with_timeouts(launcher, Duration::from_secs(5), Duration::from_secs(3))
    }

    pub fn with_timeouts(
        launcher: Arc<dyn BackendLauncher>,
        start: Duration,
        control: Duration,
    ) -> Self {
        Self {
            inner: Arc::new(SupervisorInner {
                launcher,
                jobs: std::sync::Mutex::new(BTreeMap::new()),
                admissions: std::sync::Mutex::new(BTreeMap::new()),
                start_timeout: start.max(Duration::from_millis(1)),
                control_timeout: control.max(Duration::from_millis(1)),
                observer: None,
            }),
        }
    }

    pub fn with_observer(mut self, observer: JobObserver) -> Self {
        Arc::get_mut(&mut self.inner)
            .expect("应在共享监督器前设置观察者")
            .observer = Some(observer);
        self
    }

    fn record(
        &self,
        job_id: &str,
    ) -> Result<Arc<tokio::sync::Mutex<JobRecord>>, JobLifecycleError> {
        self.inner
            .jobs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(job_id)
            .cloned()
            .ok_or_else(|| JobLifecycleError {
                code: "job_not_found".into(),
                detail: "作业不存在或不属于此监督器".into(),
            })
    }

    fn observe(&self, record: &JobRecord) {
        if record.snapshot.reaped
            || (record.snapshot.state.is_terminal() && record.snapshot.state != JobState::Unknown)
        {
            record.finished.notify_one();
        }
        if let Some(observer) = &self.inner.observer {
            observer(&record.snapshot);
        }
    }

    pub async fn start(&self, prepared: PreparedJob) -> Result<JobSnapshot, JobStartError> {
        let spec = &prepared.spec;
        // 选择后路径或批准上下文可能失效，在产生副作用前再次核验。
        if let Err(error) = validate_request(&prepared.authority, &prepared.request) {
            return Err(JobStartError {
                code: error.code.as_str().into(),
                backend_id: spec.backend_id.clone(),
                job_id: None,
                detail: error.detail,
                recoverable_action: "重新核对任务授权与项目边界".into(),
            });
        }
        if self.inner.launcher.backend_id() != spec.backend_id {
            return Err(JobStartError {
                code: "backend_mismatch".into(),
                backend_id: spec.backend_id.clone(),
                job_id: None,
                detail: "运行适配器与明确选择的后端不一致".into(),
                recoverable_action: "注册对应后端，不能用宿主适配器替代".into(),
            });
        }
        let job_id = format!("job-{}", new_id());
        let started = now_ms();
        let deadline = tokio::time::Instant::now() + Duration::from_millis(spec.timeout_ms);
        let snapshot = JobSnapshot {
            schema_version: EXEC_BACKEND_SCHEMA_VERSION,
            job_id: job_id.clone(),
            operation_id: spec.operation_id.clone(),
            state: JobState::Starting,
            operation: spec.operation,
            started_at_ms: started,
            deadline_ms: started.saturating_add(spec.timeout_ms),
            cancel_requested: false,
            attempts: 1,
            reaped: false,
            end_reason: None,
            evidence: JobEvidence {
                backend_id: spec.backend_id.clone(),
                root_execution_id: spec.execution.root_execution_id.clone(),
                execution_id: spec.execution.execution_id.clone(),
                evidence_session_id: spec.execution.evidence_session_id.clone(),
                parent_execution_id: spec.execution.parent_execution_id.clone(),
                session_id: spec.execution.session_id.clone(),
                project_id: spec.project_id.clone(),
                project_root: spec.execution.workspace.project_root.clone(),
                // 期望文件名不是产物证据；仅在真实回执后登记。
                artifacts: Vec::new(),
                logs: Vec::new(),
            },
            summary: crate::tools::redact_str(&spec.summary)
                .chars()
                .take(1000)
                .collect(),
            error: None,
            recoverable_action: None,
        };
        let finished = Arc::new(tokio::sync::Notify::new());
        let record = Arc::new(tokio::sync::Mutex::new(JobRecord {
            snapshot,
            handle: BackendJobHandle {
                job_id: job_id.clone(),
                backend_id: spec.backend_id.clone(),
                reference: job_id.clone(),
            },
            cancellation_id: spec.execution.cancellation.token_id.clone(),
            deadline,
            finished: finished.clone(),
            spec: spec.clone(),
        }));
        let previous = {
            let mut admissions = self
                .inner
                .admissions
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let key = (
                spec.execution.execution_id.clone(),
                spec.operation_id.clone(),
            );
            if let Some(previous) = admissions.get(&key) {
                Some(previous.clone())
            } else {
                admissions.insert(key, job_id.clone());
                self.inner
                    .jobs
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(job_id.clone(), record.clone());
                None
            }
        };
        if let Some(previous) = previous {
            let existing = self.record(&previous).expect("admission 与记录同时建立");
            let existing = existing.lock().await;
            if existing.spec != *spec {
                return Err(JobStartError {
                    code: "idempotency_conflict".into(),
                    backend_id: spec.backend_id.clone(),
                    job_id: Some(previous),
                    detail: "相同动作 ID 不能用于不同启动描述".into(),
                    recoverable_action: "查询原作业；不得更换参数重放未知写动作".into(),
                });
            }
            return Ok(existing.snapshot.clone());
        }
        // 启动 future 被调用方丢弃时，此定时器仍能按稳定作业 ID 请求取消。
        let weak = Arc::downgrade(&self.inner);
        let deadline_job_id = job_id.clone();
        tokio::spawn(async move {
            tokio::select! {
                _ = finished.notified() => {},
                _ = tokio::time::sleep_until(deadline) => {
                    if let Some(inner) = weak.upgrade() {
                        let _ = ExecJobSupervisor { inner }.poll_at(&deadline_job_id, u64::MAX).await;
                    }
                }
            }
        });
        let mut entry = record.lock().await;
        self.observe(&entry);
        let bound = self
            .inner
            .start_timeout
            .min(Duration::from_millis(spec.timeout_ms));
        let result =
            tokio::time::timeout(bound, self.inner.launcher.start(&prepared, &job_id)).await;
        let failure = match result {
            Ok(Ok(handle)) if handle.job_id == job_id && handle.backend_id == spec.backend_id => {
                entry.handle = handle;
                entry.snapshot.state = JobState::Running;
                None
            }
            Ok(Ok(_)) => Some((
                JobEndReason::ProtocolMismatch,
                "后端返回了不匹配的作业/后端身份".into(),
            )),
            Ok(Err(detail)) => Some((JobEndReason::StartFailed, detail)),
            Err(_) => Some((
                JobEndReason::DeadlineExceeded,
                "启动超过有界时限，真实远端状态待核验".into(),
            )),
        };
        if let Some((reason, detail)) = failure {
            mark_unknown(&mut entry.snapshot, reason, &detail);
            entry.snapshot.cancel_requested = true;
            let _ = tokio::time::timeout(
                self.inner.control_timeout,
                self.inner.launcher.cancel(&entry.handle),
            )
            .await;
            self.observe(&entry);
            return Err(JobStartError {
                code: "backend_start_unknown".into(),
                backend_id: spec.backend_id.clone(),
                job_id: Some(job_id),
                detail: crate::tools::redact_str(&detail),
                recoverable_action: "按作业 ID 查询/回收部分初始化资源，禁止自动重放或改用宿主"
                    .into(),
            });
        }
        self.observe(&entry);
        let result = entry.snapshot.clone();
        Ok(result)
    }

    pub async fn snapshot(&self, job_id: &str) -> Option<JobSnapshot> {
        Some(self.record(job_id).ok()?.lock().await.snapshot.clone())
    }

    pub async fn list(&self) -> Vec<JobSnapshot> {
        let records: Vec<_> = self
            .inner
            .jobs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .cloned()
            .collect();
        let mut snapshots = Vec::new();
        for record in records {
            snapshots.push(record.lock().await.snapshot.clone());
        }
        snapshots
    }

    pub async fn poll(&self, job_id: &str) -> Result<JobSnapshot, JobLifecycleError> {
        self.poll_at(job_id, now_ms()).await
    }

    pub async fn poll_at(
        &self,
        job_id: &str,
        at_ms: u64,
    ) -> Result<JobSnapshot, JobLifecycleError> {
        let record = self.record(job_id)?;
        let mut entry = record.lock().await;
        if entry.snapshot.reaped {
            return Ok(entry.snapshot.clone());
        }
        if entry.snapshot.state.is_terminal() && entry.snapshot.state != JobState::Unknown {
            return Ok(entry.snapshot.clone());
        }
        if at_ms >= entry.snapshot.deadline_ms || tokio::time::Instant::now() >= entry.deadline {
            self.cancel_record(&mut entry, true).await;
            return Ok(entry.snapshot.clone());
        }
        let remaining = entry
            .deadline
            .saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(
            self.inner.control_timeout.min(remaining),
            self.inner.launcher.poll(&entry.handle),
        )
        .await
        {
            Ok(BackendJobPoll::Running) => {}
            Ok(BackendJobPoll::Exited {
                code,
                artifacts,
                logs,
            }) => {
                mark_exited(&mut entry.snapshot, code);
                merge_evidence(&mut entry.snapshot, artifacts, logs);
            }
            Ok(BackendJobPoll::Disconnected { detail }) => {
                mark_unknown(
                    &mut entry.snapshot,
                    JobEndReason::BackendDisconnected,
                    &format!("后端断连：{detail}"),
                );
            }
            Err(_) => mark_unknown(
                &mut entry.snapshot,
                JobEndReason::BackendDisconnected,
                "状态查询超时",
            ),
        }
        self.observe(&entry);
        Ok(entry.snapshot.clone())
    }

    async fn cancel_record(&self, entry: &mut JobRecord, timed_out: bool) {
        let was_unknown = entry.snapshot.state == JobState::Unknown;
        entry.snapshot.cancel_requested = true;
        match tokio::time::timeout(
            self.inner.control_timeout,
            self.inner.launcher.cancel(&entry.handle),
        )
        .await
        {
            Ok(Ok(())) if !was_unknown => {
                entry.snapshot.state = if timed_out {
                    JobState::TimedOut
                } else {
                    JobState::Cancelled
                };
                entry.snapshot.end_reason = Some(if timed_out {
                    JobEndReason::DeadlineExceeded
                } else {
                    JobEndReason::CancelRequested
                });
                entry.snapshot.recoverable_action =
                    Some("核验后端作业已停止；写操作不得自动重放".into());
            }
            Ok(Ok(())) => {} // 取消确认只说明停止，不能把未知的写结果改成已取消。
            Ok(Err(detail)) => {
                mark_unknown(&mut entry.snapshot, JobEndReason::CancelFailed, &detail)
            }
            Err(_) => mark_unknown(
                &mut entry.snapshot,
                JobEndReason::CancelFailed,
                "取消请求超时",
            ),
        }
        self.observe(entry);
    }

    pub async fn cancel(&self, job_id: &str) -> Result<JobSnapshot, JobLifecycleError> {
        let record = self.record(job_id)?;
        let mut entry = record.lock().await;
        if !entry.snapshot.state.is_terminal() || entry.snapshot.state == JobState::Unknown {
            self.cancel_record(&mut entry, false).await;
        }
        Ok(entry.snapshot.clone())
    }

    /// 父/子执行共用取消绑定，只影响同一令牌归属的作业。
    pub async fn cancel_binding(&self, token_id: &str) -> Vec<JobSnapshot> {
        let records: Vec<_> = self
            .inner
            .jobs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .cloned()
            .collect();
        let mut results = Vec::new();
        for record in records {
            let mut entry = record.lock().await;
            if entry.cancellation_id == token_id && !entry.snapshot.state.is_terminal() {
                self.cancel_record(&mut entry, false).await;
                results.push(entry.snapshot.clone());
            }
        }
        results
    }

    /// 回收（释放容器/远端引用）。失败进入 `Unknown` + `ReapFailed`，不谎报成功。
    pub async fn reap(&self, job_id: &str) -> Result<JobSnapshot, JobLifecycleError> {
        let record = self.record(job_id)?;
        let mut entry = record.lock().await;
        if entry.snapshot.reaped {
            return Ok(entry.snapshot.clone());
        }
        if !entry.snapshot.state.is_terminal() {
            self.cancel_record(&mut entry, false).await;
        }
        match tokio::time::timeout(
            self.inner.control_timeout,
            self.inner.launcher.reap(&entry.handle),
        )
        .await
        {
            Ok(Ok(outcome)) => {
                entry.snapshot.reaped = true;
                if entry.snapshot.state == JobState::Unknown
                    && entry.snapshot.end_reason != Some(JobEndReason::InvalidEvidence)
                {
                    if let Some(code) = outcome.code {
                        mark_exited(&mut entry.snapshot, code);
                    }
                }
                merge_evidence(&mut entry.snapshot, outcome.artifacts, outcome.logs);
            }
            Ok(Err(detail)) => mark_unknown(
                &mut entry.snapshot,
                JobEndReason::ReapFailed,
                &format!("回收失败：{detail}"),
            ),
            Err(_) => mark_unknown(
                &mut entry.snapshot,
                JobEndReason::ReapFailed,
                "回收超时，需要人工清理",
            ),
        }
        self.observe(&entry);
        Ok(entry.snapshot.clone())
    }
}

fn mark_unknown(snapshot: &mut JobSnapshot, reason: JobEndReason, detail: &str) {
    snapshot.state = JobState::Unknown;
    snapshot.end_reason = Some(reason);
    snapshot.error = Some(
        crate::tools::redact_str(detail)
            .chars()
            .take(2000)
            .collect(),
    );
    snapshot.recoverable_action =
        Some("人工清理或查询后端真实终态并核验产物；禁止自动重放写操作".into());
}

fn mark_exited(snapshot: &mut JobSnapshot, code: i32) {
    snapshot.state = if code == 0 {
        JobState::Succeeded
    } else {
        JobState::Failed
    };
    snapshot.end_reason = Some(if code == 0 {
        JobEndReason::Completed
    } else {
        JobEndReason::ExitFailure
    });
    snapshot.error = (code != 0).then(|| format!("后端退出码 {code}"));
    snapshot.recoverable_action = None;
}

fn merge_evidence(snapshot: &mut JobSnapshot, artifacts: Vec<String>, logs: Vec<String>) {
    for artifact in artifacts {
        let valid = snapshot
            .evidence
            .project_root
            .as_ref()
            .is_some_and(|project| {
                path_within_project(Path::new(&artifact), Path::new(project))
                    && Path::new(&artifact).is_file()
            });
        if !valid {
            mark_unknown(
                snapshot,
                JobEndReason::InvalidEvidence,
                "后端产物不是项目内的真实常规文件",
            );
        } else if snapshot.evidence.artifacts.len() < 128
            && !snapshot.evidence.artifacts.contains(&artifact)
        {
            snapshot.evidence.artifacts.push(artifact);
        }
    }
    let mut remaining =
        65_536usize.saturating_sub(snapshot.evidence.logs.iter().map(String::len).sum());
    for log in logs {
        if remaining == 0 {
            break;
        }
        let log: String = crate::tools::redact_str(&log)
            .chars()
            .take(remaining / 4)
            .collect();
        remaining = remaining.saturating_sub(log.len());
        if !snapshot.evidence.logs.contains(&log) {
            snapshot.evidence.logs.push(log);
        }
    }
}

impl crate::Core {
    /// 当前配置/探测下的执行后端目录（只读，不启动作业）。
    pub fn execution_backends(&self) -> ExecutionBackendRegistry {
        // 二进制存在不等于有经过验证的运行适配器；后续由适配器探测后注册就绪实例。
        let mut registry = ExecutionBackendRegistry::builtin(false, false, false);
        for id in ["bubblewrap", "container", "remote"] {
            if let Some(backend) = registry.backends.get_mut(id) {
                backend.readiness = BackendReadiness::Unverified {
                    detail: "尚未注册并验证此后端的运行适配器".into(),
                };
            }
        }
        registry
    }

    /// 无副作用的自检：只执行固定二进制的 --version，不访问 Docker daemon/远端。
    /// 二进制存在只作为依赖证据，绝不据此把未注册的适配器升级成 ready。
    pub async fn check_execution_backend(
        &self,
        id: &str,
    ) -> Result<BackendSelfCheck, BackendSelectionError> {
        let backend =
            self.execution_backends().get(id).cloned().ok_or_else(|| {
                selection_error(BackendRejectCode::InvalidRequest, "执行后端不存在")
            })?;
        let program = match id {
            "bubblewrap" => Some("bwrap"),
            "container" => Some("docker"),
            _ => None,
        };
        let dependency_available = if let Some(program) = program {
            let mut command = tokio::process::Command::new(program);
            command
                .arg("--version")
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .kill_on_drop(true)
                .env_clear();
            for key in crate::tools::ENV_ALLOW {
                if let Some(value) = std::env::var_os(key) {
                    command.env(key, value);
                }
            }
            Some(
                matches!(tokio::time::timeout(Duration::from_secs(1), command.status()).await, Ok(Ok(status)) if status.success()),
            )
        } else if id == "local-managed" {
            Some(true)
        } else {
            None
        };
        let ready = backend.ready() && dependency_available != Some(false);
        let detail = if ready {
            "既有受管本机路径；不声明文件/网络隔离或已验证浏览器".into()
        } else {
            format!(
                "{}；--version 只证明二进制可调用，不证明后端/浏览器已通过隔离验收",
                backend.readiness.detail()
            )
        };
        Ok(BackendSelfCheck {
            schema_version: EXEC_BACKEND_SCHEMA_VERSION,
            backend,
            dependency_available,
            ready,
            detail,
        })
    }

    /// 按任务授权/项目边界/工具效果/后端能力交集选择后端。
    pub fn select_exec_backend(
        &self,
        authority: &BackendAuthorization,
        request: &BackendSelectionRequest,
    ) -> Result<BackendSelection, BackendSelectionError> {
        let result = self.execution_backends().select(authority, request);
        let (kind, payload) = match &result {
            Ok(selection) => (
                "execution.backend.selected",
                serde_json::to_value(selection),
            ),
            Err(error) => ("execution.backend.rejected", serde_json::to_value(error)),
        };
        if let Ok(payload) = payload {
            self.emit_session_event(&authority.execution.session_id, kind, payload);
        }
        result
    }

    /// 终态/日志/产物快照进入既有事件事实源，不另建作业持久化存储。
    pub fn execution_supervisor(
        self: &Arc<Self>,
        launcher: Arc<dyn BackendLauncher>,
    ) -> ExecJobSupervisor {
        let core = Arc::downgrade(self);
        ExecJobSupervisor::new(launcher).with_observer(Arc::new(move |snapshot| {
            if let (Some(core), Ok(payload)) = (core.upgrade(), serde_json::to_value(snapshot)) {
                core.emit_session_event(
                    &snapshot.evidence.session_id,
                    "execution.backend.changed",
                    payload,
                );
            }
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution_context::WorkspaceBoundary;
    use crate::types::ToolScope;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Temp(std::path::PathBuf);

    impl Temp {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("exm-backend-{}", new_id()));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn context(allowed: &[&str], external: bool, project: Option<&str>) -> ExecutionContext {
        let workspace = match project {
            Some(path) => WorkspaceBoundary::project(path),
            None => WorkspaceBoundary::default(),
        };
        ExecutionContext::root(
            "sess-exec",
            workspace,
            ToolScope::Computer,
            allowed.iter().copied(),
            false,
            external,
            Some(100),
        )
        .expect("执行上下文")
    }

    fn request(execution: ExecutionContext) -> BackendSelectionRequest {
        BackendSelectionRequest {
            execution,
            project_id: Some("proj-1".into()),
            required_tools: tools(&["terminal"]),
            browser: false,
            network: NetworkRequirement::none(),
            mounts: Vec::new(),
            identity: IdentityRequest::default(),
            resources: ResourceRequest {
                memory_mb: None,
                processes: None,
                timeout_ms: 60_000,
            },
            persistence: PersistenceCapability::ProjectWorkspace,
            cancellation: false,
            external_effects: false,
            isolation_required: false,
            preferred: Vec::new(),
            required_backend: None,
        }
    }

    fn authority(req: &BackendSelectionRequest) -> BackendAuthorization {
        BackendAuthorization::new(
            req.execution.clone(),
            req.project_id.clone(),
            IdentityRequest::default(),
        )
        .expect("服务端授权")
    }

    fn select(
        registry: &ExecutionBackendRegistry,
        req: &BackendSelectionRequest,
    ) -> Result<BackendSelection, BackendSelectionError> {
        registry.select(&authority(req), req)
    }

    #[test]
    fn 选择_缺少已验证浏览器时拒绝就绪且不回落到宿主() {
        let registry = ExecutionBackendRegistry::builtin(false, true, false);
        let mut req = request(context(&["browser"], false, None));
        req.required_tools = tools(&["browser"]);
        req.browser = true;
        req.persistence = PersistenceCapability::Ephemeral;
        let error = select(&registry, &req).expect_err("浏览器未验证必须拒绝");
        assert_eq!(error.code, BackendRejectCode::NoReadyBackend, "{error}");
        let container = error
            .rejected
            .iter()
            .find(|item| item.backend_id == "container")
            .expect("容器候选应被评估");
        assert_eq!(container.code, BackendRejectCode::BrowserUnverified);
        assert!(
            error
                .rejected
                .iter()
                .all(|item| item.backend_id != "local-managed"
                    || item.code == BackendRejectCode::ToolUnsupported),
            "宿主缺少 browser 工具时也必须如实拒绝：{error:?}"
        );
    }

    #[test]
    fn 选择_项目外挂载与docker_socket在选择前拒绝并记录原因() {
        let registry = ExecutionBackendRegistry::builtin(true, true, false);
        let mut outside = request(context(&["terminal"], false, Some("/tmp/proj-exec")));
        outside.mounts = vec![MountRequest {
            host_path: "/etc".into(),
            read_only: true,
            purpose: "越权读取".into(),
        }];
        let error = select(&registry, &outside).expect_err("项目外挂载必须拒绝");
        assert_eq!(error.code, BackendRejectCode::OutsideProjectMount);
        assert!(error.detail.contains("/etc"), "{error}");

        let mut socket = request(context(&["terminal"], false, Some("/var/run")));
        socket.mounts = vec![MountRequest {
            host_path: "/var/run/docker.sock".into(),
            read_only: true,
            purpose: "docker control".into(),
        }];
        let error = select(&registry, &socket).expect_err("docker socket 必须拒绝");
        assert_eq!(error.code, BackendRejectCode::DockerSocketMount);
        assert!(error.detail.contains("docker.sock"), "{error}");

        let mut traversal = request(context(&["terminal"], false, Some("/tmp/proj-exec")));
        traversal.mounts = vec![MountRequest {
            host_path: "/tmp/proj-exec/../escape".into(),
            read_only: true,
            purpose: "..".into(),
        }];
        assert_eq!(
            select(&registry, &traversal).unwrap_err().code,
            BackendRejectCode::InvalidRequest
        );
    }

    #[test]
    fn 选择_未批准网络与提权身份拒绝() {
        let registry = ExecutionBackendRegistry::builtin(true, true, false);
        let mut req = request(context(&["terminal"], false, None));
        req.network = NetworkRequirement::allowlisted(["evil.example"]);
        let approved =
            authority(&req).with_network(NetworkRequirement::allowlisted(["good.example"]));
        let error = registry
            .select(&approved, &req)
            .expect_err("未批准网络必须拒绝");
        assert_eq!(error.code, BackendRejectCode::UnapprovedNetwork);
        assert!(error.detail.contains("evil.example"), "{error}");

        let mut elevated = request(context(&["terminal"], false, None));
        elevated.identity.elevated = true;
        let error = select(&registry, &elevated).expect_err("提权必须拒绝");
        assert_eq!(error.code, BackendRejectCode::PrivilegedIdentity);

        let mut root = request(context(&["terminal"], false, None));
        root.identity.uid = Some(0);
        root.identity.gid = Some(0);
        let error = select(&registry, &root).expect_err("root 必须拒绝");
        assert_eq!(error.code, BackendRejectCode::PrivilegedIdentity);
    }

    #[test]
    fn 选择_工具与外部写必须是授权的子集() {
        let registry = ExecutionBackendRegistry::builtin(true, true, false);
        let mut tool = request(context(&["terminal"], false, None));
        tool.required_tools = tools(&["git"]);
        let error = select(&registry, &tool).expect_err("未授权工具必须拒绝");
        assert_eq!(error.code, BackendRejectCode::ToolNotAuthorized);
        assert!(error.detail.contains("git"), "{error}");

        let mut external = request(context(&["terminal"], false, None));
        external.external_effects = true;
        let error = select(&registry, &external).expect_err("外部写未授权必须拒绝");
        assert_eq!(error.code, BackendRejectCode::ExternalWritesNotAuthorized);
    }

    #[test]
    fn 选择_隔离必需时绝不使用宿主执行() {
        let req = {
            let mut req = request(context(&["terminal"], false, None));
            req.isolation_required = true;
            req
        };
        let no_isolated = ExecutionBackendRegistry::builtin(false, false, false);
        let error = select(&no_isolated, &req).expect_err("没有隔离后端时必须失败");
        assert_eq!(error.code, BackendRejectCode::NoReadyBackend);
        let local = error
            .rejected
            .iter()
            .find(|item| item.backend_id == "local-managed")
            .expect("宿主候选必须被记录拒绝");
        assert_eq!(local.code, BackendRejectCode::IsolationRequired);

        let with_bwrap = ExecutionBackendRegistry::builtin(true, false, false);
        let selected = select(&with_bwrap, &req).expect("bwrap 就绪时应选中隔离后端");
        assert_eq!(selected.backend.id, "bubblewrap");
        assert!(selected.backend.capabilities.isolated);
    }

    #[test]
    fn 选择_偏好不能升级为宿主回退() {
        let registry = ExecutionBackendRegistry::builtin(true, false, false);
        let mut req = request(context(&["terminal"], false, None));
        req.isolation_required = true;
        req.preferred = vec!["local-managed".into()];
        let selected = select(&registry, &req).expect("应回落到安全的隔离后端");
        assert_eq!(selected.backend.id, "bubblewrap");
        assert!(selected
            .rejected
            .iter()
            .any(|item| item.backend_id == "local-managed"
                && item.code == BackendRejectCode::IsolationRequired));
    }

    #[test]
    fn 选择_可信授权不采用请求自行扩权的上下文() {
        let registry = ExecutionBackendRegistry::builtin(true, true, false);
        let mut req = request(context(&["terminal"], false, None));
        let approved = authority(&req);
        for field in [
            "project",
            "session",
            "root",
            "execution",
            "tools",
            "external",
        ] {
            let mut changed = req.clone();
            match field {
                "project" => changed.project_id = Some("another-project".into()),
                "session" => changed.execution.session_id = "another-session".into(),
                "root" => {
                    changed.execution.root_execution_id = "another-root".into();
                    changed.execution.budget.root_execution_id = "another-root".into();
                }
                "execution" => changed.execution.execution_id = "another-execution".into(),
                "tools" => {
                    changed
                        .execution
                        .authorization
                        .allowed_tools
                        .insert("git".into());
                }
                _ => changed.execution.authorization.allow_external_writes = true,
            }
            assert_eq!(
                registry.select(&approved, &changed).unwrap_err().code,
                BackendRejectCode::AuthorizationMismatch,
                "{field}"
            );
        }
        req.network = NetworkRequirement::unrestricted();
        assert_eq!(
            registry.select(&approved, &req).unwrap_err().code,
            BackendRejectCode::UnapprovedNetwork
        );
        req.network = NetworkRequirement::loopback();
        assert_eq!(
            registry.select(&approved, &req).unwrap_err().code,
            BackendRejectCode::UnapprovedNetwork
        );
        req.network = NetworkRequirement::none();
        req.identity = IdentityRequest {
            uid: Some(1001),
            gid: Some(1001),
            elevated: false,
        };
        assert_eq!(
            registry.select(&approved, &req).unwrap_err().code,
            BackendRejectCode::PrivilegedIdentity
        );
        req.identity = IdentityRequest::default();
        // 实际工具效果来自网关；请求自称非外部写不能放行。
        assert_eq!(
            registry
                .select(&approved.with_external_effects(true), &req)
                .unwrap_err()
                .code,
            BackendRejectCode::ExternalWritesNotAuthorized
        );
    }

    #[test]
    fn 选择_指定后端失败不偷偷改用其他可用后端() {
        let registry = ExecutionBackendRegistry::builtin(true, false, false);
        let mut req = request(context(&["terminal"], false, None));
        req.required_backend = Some("container".into());
        let error = select(&registry, &req).unwrap_err();
        assert_eq!(error.code, BackendRejectCode::NoReadyBackend);
        assert_eq!(error.rejected.len(), 1);
        assert_eq!(error.rejected[0].backend_id, "container");
        req.required_backend = Some("not-registered".into());
        assert_eq!(
            select(&registry, &req).unwrap_err().code,
            BackendRejectCode::NoReadyBackend
        );
    }

    #[test]
    fn 选择_无法强制网络或资源上限的后端不得冒充满足() {
        let registry = ExecutionBackendRegistry::builtin(true, false, false);
        let mut req = request(context(&["terminal"], false, None));
        req.required_backend = Some("local-managed".into());
        let error = select(&registry, &req).unwrap_err();
        assert_eq!(
            error.rejected[0].code,
            BackendRejectCode::NetworkUnsupported
        );
        req.required_backend = Some("bubblewrap".into());
        req.resources.memory_mb = Some(512);
        assert_eq!(
            select(&registry, &req).unwrap_err().rejected[0].code,
            BackendRejectCode::ResourceLimit
        );
        req.resources.memory_mb = None;
        req.resources.processes = Some(16);
        assert_eq!(
            select(&registry, &req).unwrap_err().rejected[0].code,
            BackendRejectCode::ResourceLimit
        );
    }

    #[cfg(unix)]
    #[test]
    fn 选择_符号链接挂载在选择前拒绝() {
        let project = Temp::new();
        let outside = Temp::new();
        let link = project.0.join("escape");
        std::os::unix::fs::symlink(&outside.0, &link).unwrap();
        let registry = ExecutionBackendRegistry::builtin(true, true, false);
        let mut req = request(context(
            &["terminal"],
            false,
            Some(project.0.to_str().unwrap()),
        ));
        req.mounts.push(MountRequest {
            host_path: link.to_string_lossy().into_owned(),
            read_only: true,
            purpose: "fixture".into(),
        });
        assert_eq!(
            select(&registry, &req).unwrap_err().code,
            BackendRejectCode::OutsideProjectMount
        );
    }

    #[test]
    fn 选择_项目内挂载与批准网络可选中隔离容器() {
        let mut registry = ExecutionBackendRegistry::new();
        let mut container = ExecutionBackendRegistry::builtin(false, true, false)
            .get("container")
            .expect("内置容器")
            .clone();
        container.capabilities.network = NetworkRequirement::allowlisted(["good.example"]);
        registry.register(container).expect("注册隔离容器");

        let temp = Temp::new();
        let project = temp.0.to_str().unwrap();
        let mut req = request(context(&["terminal"], false, Some(project)));
        req.network = NetworkRequirement::allowlisted(["good.example"]);
        req.mounts = vec![MountRequest {
            host_path: project.into(),
            read_only: false,
            purpose: "project".into(),
        }];
        req.isolation_required = true;
        let approved =
            authority(&req).with_network(NetworkRequirement::allowlisted(["good.example"]));
        let selected = registry.select(&approved, &req).expect("容器应满足约束");
        assert_eq!(selected.backend.id, "container");
        assert_eq!(selected.binding.project_id.as_deref(), Some("proj-1"));
        assert_eq!(selected.binding.session_id, "sess-exec");
        assert_eq!(selected.mounts.len(), 1);
    }

    #[test]
    fn 严格沙箱_隔离不可用时拒绝宿主回退() {
        assert!(strict_isolation_refusal(true, true, false).is_some());
        assert!(strict_isolation_refusal(true, true, true).is_none());
        assert!(strict_isolation_refusal(false, true, false).is_none());
        assert!(strict_isolation_refusal(true, false, false).is_none());
    }

    struct FakeLauncher {
        starts: AtomicUsize,
        cancels: AtomicUsize,
        cancel_fails: bool,
        poll: std::sync::Mutex<BackendJobPoll>,
        reap: std::sync::Mutex<Result<JobOutcome, String>>,
    }

    impl FakeLauncher {
        fn new(
            poll: BackendJobPoll,
            cancel_fails: bool,
            reap: Result<JobOutcome, String>,
        ) -> Arc<Self> {
            Arc::new(Self {
                starts: AtomicUsize::new(0),
                cancels: AtomicUsize::new(0),
                cancel_fails,
                poll: std::sync::Mutex::new(poll),
                reap: std::sync::Mutex::new(reap),
            })
        }
    }

    #[async_trait::async_trait]
    impl BackendLauncher for FakeLauncher {
        fn backend_id(&self) -> &str {
            "container"
        }

        async fn start(&self, job: &PreparedJob, job_id: &str) -> Result<BackendJobHandle, String> {
            self.starts.fetch_add(1, Ordering::SeqCst);
            Ok(BackendJobHandle {
                job_id: job_id.into(),
                backend_id: job.spec.backend_id.clone(),
                reference: format!("ref-{job_id}"),
            })
        }

        async fn poll(&self, _handle: &BackendJobHandle) -> BackendJobPoll {
            self.poll.lock().unwrap_or_else(|e| e.into_inner()).clone()
        }

        async fn cancel(&self, _handle: &BackendJobHandle) -> Result<(), String> {
            self.cancels.fetch_add(1, Ordering::SeqCst);
            if self.cancel_fails {
                Err("cancel rpc failed".into())
            } else {
                Ok(())
            }
        }

        async fn reap(&self, _handle: &BackendJobHandle) -> Result<JobOutcome, String> {
            self.reap.lock().unwrap_or_else(|e| e.into_inner()).clone()
        }
    }

    fn prepare(spec: JobSpec) -> PreparedJob {
        let mut req = request(spec.execution.clone());
        req.required_backend = Some("container".into());
        req.resources.timeout_ms = spec.timeout_ms;
        ExecutionBackendRegistry::builtin(false, true, false)
            .prepare_job(&authority(&req), &req, spec)
            .expect("通过真实选择准备作业")
    }

    fn job_spec(operation: JobOperation, timeout_ms: u64) -> PreparedJob {
        prepare(JobSpec {
            execution: context(&["terminal"], false, None),
            project_id: Some("proj-1".into()),
            backend_id: "container".into(),
            operation,
            operation_id: "fixture-call".into(),
            command: vec!["sh".into(), "-c".into(), "true".into()],
            timeout_ms,
            artifacts: vec![],
            summary: "fixture job".into(),
        })
    }

    #[tokio::test]
    async fn 生命周期_超时请求取消且写操作不自动重放() {
        let launcher = FakeLauncher::new(
            BackendJobPoll::Running,
            false,
            Ok(JobOutcome {
                code: Some(0),
                artifacts: vec![],
                logs: vec![],
            }),
        );
        let supervisor = ExecJobSupervisor::new(launcher.clone());
        let started = supervisor
            .start(job_spec(JobOperation::Write, 10))
            .await
            .expect("启动");
        let timed_out = supervisor
            .poll_at(&started.job_id, started.deadline_ms + 1)
            .await
            .expect("轮询");
        assert_eq!(timed_out.state, JobState::TimedOut);
        assert_eq!(timed_out.end_reason, Some(JobEndReason::DeadlineExceeded));
        assert!(timed_out.cancel_requested);
        assert_eq!(timed_out.attempts, 1);
        assert_eq!(timed_out.evidence.session_id, "sess-exec");
        assert_eq!(timed_out.evidence.project_id.as_deref(), Some("proj-1"));
        assert_eq!(
            timed_out.evidence.root_execution_id,
            started.evidence.root_execution_id
        );
        assert_eq!(launcher.cancels.load(Ordering::SeqCst), 1);
        assert_eq!(launcher.starts.load(Ordering::SeqCst), 1);
        // 再轮询不会重启作业
        let again = supervisor.poll(&started.job_id).await.expect("轮询");
        assert_eq!(again.state, JobState::TimedOut);
        assert_eq!(launcher.starts.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn 生命周期_取消失败超时进入unknown且保留人工核验动作() {
        let launcher = FakeLauncher::new(
            BackendJobPoll::Running,
            true,
            Ok(JobOutcome {
                code: Some(0),
                artifacts: vec![],
                logs: vec![],
            }),
        );
        let supervisor = ExecJobSupervisor::new(launcher.clone());
        let started = supervisor
            .start(job_spec(JobOperation::Write, 10))
            .await
            .expect("启动");
        let snapshot = supervisor
            .poll_at(&started.job_id, started.deadline_ms + 1)
            .await
            .expect("轮询");
        assert_eq!(snapshot.state, JobState::Unknown);
        assert_eq!(snapshot.end_reason, Some(JobEndReason::CancelFailed));
        assert!(snapshot
            .recoverable_action
            .as_deref()
            .unwrap_or_default()
            .contains("禁止自动重放"));
        assert_eq!(launcher.starts.load(Ordering::SeqCst), 1);
        assert_eq!(snapshot.attempts, 1);
    }

    #[tokio::test]
    async fn 生命周期_远端断连与回收失败返回unknown() {
        let launcher = FakeLauncher::new(
            BackendJobPoll::Disconnected {
                detail: "worker connection reset".into(),
            },
            false,
            Err("reap rpc timeout".into()),
        );
        let supervisor = ExecJobSupervisor::new(launcher.clone());
        let started = supervisor
            .start(job_spec(JobOperation::Write, 60_000))
            .await
            .expect("启动");
        let snapshot = supervisor.poll(&started.job_id).await.expect("轮询");
        assert_eq!(snapshot.state, JobState::Unknown);
        assert_eq!(snapshot.end_reason, Some(JobEndReason::BackendDisconnected));
        assert!(snapshot
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("断连"));
        assert_eq!(snapshot.attempts, 1);

        let reaped = supervisor.reap(&started.job_id).await.expect("回收");
        assert_eq!(reaped.state, JobState::Unknown);
        assert_eq!(reaped.end_reason, Some(JobEndReason::ReapFailed));
        assert!(!reaped.reaped);
        assert!(reaped
            .recoverable_action
            .as_deref()
            .unwrap_or_default()
            .contains("人工清理"));
        assert_eq!(launcher.starts.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn 生命周期_完成与回收记录产物日志且证据可追溯() {
        let temp = Temp::new();
        let result = temp.0.join("result.png");
        std::fs::write(&result, b"fixture artifact").unwrap();
        let artifact = result.to_string_lossy().into_owned();
        let launcher = FakeLauncher::new(
            BackendJobPoll::Exited {
                code: 0,
                artifacts: vec![artifact.clone()],
                logs: vec!["step ok".into()],
            },
            false,
            Ok(JobOutcome {
                code: Some(0),
                artifacts: vec![artifact.clone()],
                logs: vec!["reaped".into()],
            }),
        );
        let supervisor = ExecJobSupervisor::new(launcher);
        let mut spec = job_spec(JobOperation::Read, 60_000).spec;
        spec.execution.workspace = WorkspaceBoundary::project(temp.0.to_string_lossy());
        spec.artifacts = vec![temp
            .0
            .join("not-created.txt")
            .to_string_lossy()
            .into_owned()];
        let started = supervisor.start(prepare(spec)).await.expect("启动");
        let done = supervisor.poll(&started.job_id).await.expect("轮询");
        assert_eq!(done.state, JobState::Succeeded);
        assert_eq!(done.end_reason, Some(JobEndReason::Completed));
        assert!(done.evidence.artifacts.contains(&artifact));
        assert_eq!(
            done.evidence.artifacts.len(),
            1,
            "期望但不存在的文件不能登记成产物"
        );
        assert!(done.evidence.logs.contains(&"step ok".to_string()));

        let reaped = supervisor.reap(&started.job_id).await.expect("回收");
        assert!(reaped.reaped);
        assert_eq!(reaped.state, JobState::Succeeded);
        assert_eq!(
            reaped
                .evidence
                .artifacts
                .iter()
                .filter(|item| *item == &artifact)
                .count(),
            1,
            "产物去重"
        );
        assert!(reaped.evidence.logs.contains(&"reaped".to_string()));
        assert!(!reaped.cancel_requested, "正常完成后回收不能冒充用户取消");
    }

    #[derive(Clone, Copy)]
    enum Fault {
        Start,
        Poll,
        Cancel,
        Reap,
        Identity,
    }

    struct FaultLauncher {
        inner: Arc<FakeLauncher>,
        fault: Fault,
    }

    #[async_trait::async_trait]
    impl BackendLauncher for FaultLauncher {
        fn backend_id(&self) -> &str {
            "container"
        }
        async fn start(&self, job: &PreparedJob, id: &str) -> Result<BackendJobHandle, String> {
            if matches!(self.fault, Fault::Start) {
                self.inner.starts.fetch_add(1, Ordering::SeqCst);
                return std::future::pending().await;
            }
            let mut handle = self.inner.start(job, id).await?;
            if matches!(self.fault, Fault::Identity) {
                handle.backend_id = "local-managed".into();
            }
            Ok(handle)
        }
        async fn poll(&self, handle: &BackendJobHandle) -> BackendJobPoll {
            if matches!(self.fault, Fault::Poll) {
                return std::future::pending().await;
            }
            self.inner.poll(handle).await
        }
        async fn cancel(&self, handle: &BackendJobHandle) -> Result<(), String> {
            if matches!(self.fault, Fault::Cancel) {
                return std::future::pending().await;
            }
            self.inner.cancel(handle).await
        }
        async fn reap(&self, handle: &BackendJobHandle) -> Result<JobOutcome, String> {
            if matches!(self.fault, Fault::Reap) {
                return std::future::pending().await;
            }
            self.inner.reap(handle).await
        }
    }

    fn running() -> Arc<FakeLauncher> {
        FakeLauncher::new(
            BackendJobPoll::Running,
            false,
            Ok(JobOutcome {
                code: None,
                artifacts: vec![],
                logs: vec![],
            }),
        )
    }

    #[tokio::test]
    async fn 生命周期_无人轮询也自动超时取消() {
        let launcher = running();
        let supervisor = ExecJobSupervisor::new(launcher.clone());
        let started = supervisor
            .start(job_spec(JobOperation::Write, 25))
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            while launcher.cancels.load(Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("无需轮询也应触发截止取消");
        let result = supervisor.snapshot(&started.job_id).await.unwrap();
        assert_eq!(result.state, JobState::TimedOut);
        assert_eq!(launcher.starts.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn 生命周期_控制调用挂起在有界时间返回真实未知态() {
        for fault in [
            Fault::Start,
            Fault::Poll,
            Fault::Cancel,
            Fault::Reap,
            Fault::Identity,
        ] {
            let inner = running();
            let launcher = Arc::new(FaultLauncher {
                inner: inner.clone(),
                fault,
            });
            let supervisor = ExecJobSupervisor::with_timeouts(
                launcher,
                Duration::from_millis(10),
                Duration::from_millis(10),
            );
            let result = tokio::time::timeout(Duration::from_secs(1), async {
                match supervisor
                    .start(job_spec(JobOperation::Write, 10_000))
                    .await
                {
                    Err(error) => {
                        let snapshot = supervisor
                            .snapshot(error.job_id.as_deref().expect("部分启动须保留可查询 ID"))
                            .await
                            .unwrap();
                        assert!(matches!(fault, Fault::Start | Fault::Identity));
                        snapshot
                    }
                    Ok(snapshot) => match fault {
                        Fault::Poll => supervisor.poll(&snapshot.job_id).await.unwrap(),
                        Fault::Cancel => supervisor.cancel(&snapshot.job_id).await.unwrap(),
                        Fault::Reap => supervisor.reap(&snapshot.job_id).await.unwrap(),
                        _ => panic!("挂起或身份不符的启动不应成功"),
                    },
                }
            })
            .await
            .expect("启动/查询/取消/回收必须有界");
            assert_eq!(result.state, JobState::Unknown);
            assert_eq!(result.attempts, 1);
            assert!(result
                .recoverable_action
                .as_deref()
                .unwrap()
                .contains("禁止自动重放"));
            assert_eq!(inner.starts.load(Ordering::SeqCst), 1);
        }
    }

    #[tokio::test]
    async fn 生命周期_未知写结果按动作id幂等且参数变化冲突() {
        let launcher = FakeLauncher::new(
            BackendJobPoll::Disconnected {
                detail: "response lost".into(),
            },
            false,
            Ok(JobOutcome {
                code: None,
                artifacts: vec![],
                logs: vec![],
            }),
        );
        let supervisor = ExecJobSupervisor::new(launcher.clone());
        let prepared = job_spec(JobOperation::Write, 60_000);
        let started = supervisor.start(prepared.clone()).await.unwrap();
        let unknown = supervisor.poll(&started.job_id).await.unwrap();
        assert_eq!(unknown.state, JobState::Unknown);
        let duplicate = supervisor
            .start(prepare(prepared.spec.clone()))
            .await
            .unwrap();
        assert_eq!(duplicate.job_id, started.job_id);
        assert_eq!(duplicate.state, JobState::Unknown);
        let mut different = prepared.spec;
        different.command.push("different".into());
        let conflict = supervisor.start(prepare(different)).await.unwrap_err();
        assert_eq!(conflict.code, "idempotency_conflict");
        assert_eq!(launcher.starts.load(Ordering::SeqCst), 1);
        let reaped = supervisor.reap(&started.job_id).await.unwrap();
        assert!(reaped.reaped);
        assert_eq!(
            reaped.state,
            JobState::Unknown,
            "只确认回收、没有退出码不能编造命令成功"
        );
    }

    #[tokio::test]
    async fn 生命周期_未知状态不撤销截止清理() {
        let launcher = FakeLauncher::new(
            BackendJobPoll::Disconnected {
                detail: "lost".into(),
            },
            false,
            Ok(JobOutcome {
                code: None,
                artifacts: vec![],
                logs: vec![],
            }),
        );
        let supervisor = ExecJobSupervisor::new(launcher.clone());
        let started = supervisor
            .start(job_spec(JobOperation::Write, 40))
            .await
            .unwrap();
        assert_eq!(
            supervisor.poll(&started.job_id).await.unwrap().state,
            JobState::Unknown
        );
        tokio::time::timeout(Duration::from_secs(1), async {
            while launcher.cancels.load(Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("断连后仍需要在截止时清理");
        assert_eq!(
            supervisor.snapshot(&started.job_id).await.unwrap().state,
            JobState::Unknown
        );
        assert_eq!(launcher.starts.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn 生命周期_活动作业回收后不再显示running() {
        let supervisor = ExecJobSupervisor::new(running());
        let started = supervisor
            .start(job_spec(JobOperation::Write, 60_000))
            .await
            .unwrap();
        let reaped = supervisor.reap(&started.job_id).await.unwrap();
        assert!(reaped.reaped);
        assert_eq!(reaped.state, JobState::Cancelled);
        assert!(reaped.cancel_requested);
    }

    #[tokio::test]
    async fn 生命周期_父子取消绑定与其他执行隔离() {
        let mut parent = context(&["terminal"], false, None);
        parent.authorization.allow_delegation = true;
        let child = parent
            .child("child-exec", "child-evidence", &["terminal".into()])
            .unwrap();
        let launcher = running();
        let supervisor = ExecJobSupervisor::new(launcher.clone());
        let mut spec = job_spec(JobOperation::Read, 60_000).spec;
        spec.execution = child.clone();
        let started = supervisor.start(prepare(spec)).await.unwrap();
        let other = supervisor
            .start(job_spec(JobOperation::Read, 60_000))
            .await
            .unwrap();
        let cancelled = supervisor
            .cancel_binding(&parent.cancellation.token_id)
            .await;
        assert_eq!(cancelled.len(), 1);
        let result = &cancelled[0];
        assert_eq!(result.job_id, started.job_id);
        assert_eq!(result.evidence.root_execution_id, parent.execution_id);
        assert_eq!(result.evidence.execution_id, child.execution_id);
        assert_eq!(
            result.evidence.parent_execution_id.as_deref(),
            Some(parent.execution_id.as_str())
        );
        assert_eq!(result.evidence.evidence_session_id, "child-evidence");
        assert_eq!(
            supervisor.snapshot(&other.job_id).await.unwrap().state,
            JobState::Running
        );
        assert_eq!(launcher.cancels.load(Ordering::SeqCst), 1);
        supervisor.reap(&other.job_id).await.unwrap();
    }

    #[tokio::test]
    async fn 生命周期_伪造文件证据不能推进成功() {
        let launcher = FakeLauncher::new(
            BackendJobPoll::Exited {
                code: 0,
                artifacts: vec!["https://fake.invalid/result.png".into()],
                logs: vec!["fixture log".into()],
            },
            false,
            Ok(JobOutcome {
                code: None,
                artifacts: vec![],
                logs: vec![],
            }),
        );
        let supervisor = ExecJobSupervisor::new(launcher);
        let started = supervisor
            .start(job_spec(JobOperation::Read, 60_000))
            .await
            .unwrap();
        let result = supervisor.poll(&started.job_id).await.unwrap();
        assert_eq!(result.state, JobState::Unknown);
        assert_eq!(result.end_reason, Some(JobEndReason::InvalidEvidence));
        assert!(result.evidence.artifacts.is_empty());
        assert!(result.evidence.logs.contains(&"fixture log".into()));
        assert_eq!(
            supervisor.reap(&started.job_id).await.unwrap().state,
            JobState::Unknown
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn 生命周期_选择后挂载路径被换成符号链接仍在启动前拒绝() {
        let project = Temp::new();
        let outside = Temp::new();
        let mount = project.0.join("mount");
        std::fs::create_dir(&mount).unwrap();
        let mut spec = job_spec(JobOperation::Write, 60_000).spec;
        spec.execution.workspace = WorkspaceBoundary::project(project.0.to_string_lossy());
        let mut req = request(spec.execution.clone());
        req.required_backend = Some("container".into());
        req.mounts.push(MountRequest {
            host_path: mount.to_string_lossy().into_owned(),
            read_only: false,
            purpose: "fixture".into(),
        });
        let registry = ExecutionBackendRegistry::builtin(false, true, false);
        let job = registry.prepare_job(&authority(&req), &req, spec).unwrap();
        std::fs::remove_dir(&mount).unwrap();
        std::os::unix::fs::symlink(&outside.0, &mount).unwrap();
        let launcher = running();
        let supervisor = ExecJobSupervisor::new(launcher.clone());
        let error = supervisor.start(job).await.unwrap_err();
        assert_eq!(error.code, "outside_project_mount");
        assert!(error.job_id.is_none());
        assert_eq!(launcher.starts.load(Ordering::SeqCst), 0);
    }
}
