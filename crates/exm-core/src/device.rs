//! 独立于远程 Nexus `LinkTarget` 的实验性物理设备领域模型。
//!
//! 这里只定义稳定的设备标识、能力授权与运行状态；设备凭据、临时配对材料和
//! 媒体正文不得进入这些普通领域记录。

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures_util::StreamExt;
use parking_lot::{Mutex, RwLock};
use tokio::sync::{broadcast, watch};

/// 设备连接所使用的传输或设备协议。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceTransport {
    Bluetooth,
    SystemAudio,
    WifiHttpJpeg,
    OnvifSnapshot,
}

/// 物理设备的主要类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceKind {
    Speaker,
    Camera,
    Other,
}

/// 设备适配器可明确声明的动作能力。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceCapability {
    AudioOutput,
    ImageCapture,
}

/// 已由用户批准的能力授权。凭据只由服务端安全存储管理，不属于 grant。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceGrant {
    pub id: String,
    pub device_id: String,
    pub capabilities: BTreeSet<DeviceCapability>,
    /// 旧授权缺少主体时仍可回读，但不能授权任何 Agent 动作。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub granted_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<String>,
}

impl DeviceGrant {
    pub fn is_active(&self) -> bool {
        self.revoked_at.is_none()
    }

    pub fn allows(&self, capability: DeviceCapability) -> bool {
        self.is_active() && self.capabilities.contains(&capability)
    }

    pub fn allows_for(
        &self,
        agent_id: &str,
        session_id: &str,
        capability: DeviceCapability,
    ) -> bool {
        self.allows(capability)
            && !agent_id.trim().is_empty()
            && !session_id.trim().is_empty()
            && self.agent_id.as_deref() == Some(agent_id)
            && self
                .session_id
                .as_deref()
                .is_none_or(|scope| scope == session_id)
    }
}

/// 持久设备登记。与 `config::LinkTarget` 分域、分授权、分执行路径。
///
/// `endpoint` 仅用于保存用户批准的非凭据端点引用；访问令牌、密码和配对密钥
/// 必须存放于服务端安全配置，不得填入此结构。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceRecord {
    pub id: String,
    pub name: String,
    pub transport: DeviceTransport,
    pub kind: DeviceKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    #[serde(default)]
    pub capabilities: BTreeSet<DeviceCapability>,
    #[serde(default)]
    pub grants: Vec<DeviceGrant>,
    #[serde(default = "device_enabled_by_default")]
    pub enabled: bool,
}

fn device_enabled_by_default() -> bool {
    true
}

/// 易于 API / 事件稳定表达的设备连接状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceStatus {
    Unknown,
    Discovered,
    Unpaired,
    Disconnected,
    Connecting,
    Connected,
    Disconnecting,
    Unsupported,
    Unavailable,
    Error,
}

/// 稳定的设备失败类别；具体平台错误不得改变类别契约。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceErrorCategory {
    Unsupported,
    Unavailable,
    Forbidden,
    InvalidRequest,
    NotFound,
    Conflict,
    LimitExceeded,
    InvalidMedia,
    Cancelled,
    Internal,
}

/// 可安全传递到管理面或事件流的设备错误摘要。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceError {
    pub category: DeviceErrorCategory,
    pub message: String,
}

impl DeviceError {
    pub fn new(category: DeviceErrorCategory, message: impl Into<String>) -> Self {
        Self {
            category,
            message: message.into(),
        }
    }
}

impl fmt::Display for DeviceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.category.as_str(), self.message)
    }
}

impl std::error::Error for DeviceError {}

impl DeviceErrorCategory {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unsupported => "unsupported",
            Self::Unavailable => "unavailable",
            Self::Forbidden => "forbidden",
            Self::InvalidRequest => "invalid_request",
            Self::NotFound => "not_found",
            Self::Conflict => "conflict",
            Self::LimitExceeded => "limit_exceeded",
            Self::InvalidMedia => "invalid_media",
            Self::Cancelled => "cancelled",
            Self::Internal => "internal",
        }
    }
}

/// 设备适配器契约版本。能力协议演进时递增，旧适配器不得被静默当作新能力。
pub const DEVICE_ADAPTER_API_VERSION: u32 = 1;
const MAX_AUDIT_EVENTS: usize = 4096;

/// 所有设备管理/动作请求的最小上下文。`request_id` 会进入审计和状态事件。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceRequestContext {
    pub request_id: String,
    pub actor: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

impl DeviceRequestContext {
    pub fn new(actor: impl Into<String>, session_id: Option<String>) -> Self {
        Self {
            request_id: crate::types::new_id(),
            actor: actor.into(),
            session_id,
        }
    }
}

/// 用户主动发起的一次有界发现请求；适配器不得扩展其范围或创建后台扫描。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveryRequest {
    pub transport: DeviceTransport,
    /// 蓝牙为用户选择的本机适配器范围；Wi-Fi 为已批准的 endpoint 前缀/设备引用。
    #[serde(default)]
    pub scope: String,
    pub duration_secs: u64,
    #[serde(default)]
    pub max_results: usize,
}

/// 适配器发现出的待授权候选，不代表已配对或可执行。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceCandidate {
    pub id: String,
    pub name: String,
    pub transport: DeviceTransport,
    pub kind: DeviceKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    #[serde(default)]
    pub capabilities: BTreeSet<DeviceCapability>,
}

/// 设备运行态只保存连接状态和错误摘要，不保存句柄、凭据或媒体正文。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceRuntimeState {
    pub status: DeviceStatus,
    pub updated_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<DeviceError>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
}

impl Default for DeviceRuntimeState {
    fn default() -> Self {
        Self {
            status: DeviceStatus::Unknown,
            updated_at: crate::types::now_iso(),
            last_error: None,
            request_id: None,
        }
    }
}

/// 管理面/拓扑使用的安全设备快照。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceSnapshot {
    pub record: DeviceRecord,
    pub runtime: DeviceRuntimeState,
}

/// 设备动作的非媒体结果。只有适配器确认成功后才会产生 `succeeded`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceActionReceipt {
    pub request_id: String,
    pub device_id: String,
    pub status: DeviceStatus,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<usize>,
}

/// 可安全展示的宿主蓝牙能力探测结果；不代表已经发现或连接设备。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BluetoothHostCapabilities {
    pub backend_available: bool,
    pub backend_id: Option<String>,
    pub transports: BTreeSet<DeviceTransport>,
    pub capabilities: BTreeSet<DeviceCapability>,
    pub status: DeviceStatus,
}

/// 适配器面向管理面的安全能力快照，不包含后端句柄或设备凭据。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceAdapterProbe {
    pub id: String,
    pub api_version: u32,
    pub available: bool,
    pub transports: BTreeSet<DeviceTransport>,
    pub capabilities: BTreeSet<DeviceCapability>,
    pub status: DeviceStatus,
}

/// 摄像头适配器的单帧结果；正文只在受控媒体链路中返回，绝不进入审计事件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceCapture {
    pub receipt: DeviceActionReceipt,
    pub mime_type: String,
    pub bytes: Vec<u8>,
}

/// 不含媒体正文/凭据的设备审计回执。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceAuditEvent {
    pub request_id: String,
    pub actor: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_id: Option<String>,
    pub result: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_category: Option<DeviceErrorCategory>,
    pub at: String,
}

/// 版本化设备适配器契约。实现只能通过显式声明的 transport/capability 执行动作。
#[async_trait]
pub trait DeviceAdapter: Send + Sync {
    fn api_version(&self) -> u32 {
        DEVICE_ADAPTER_API_VERSION
    }

    fn id(&self) -> &str;

    fn transports(&self) -> BTreeSet<DeviceTransport>;

    fn capabilities(&self) -> BTreeSet<DeviceCapability>;

    fn probe(&self) -> DeviceAdapterProbe {
        DeviceAdapterProbe {
            id: self.id().to_string(),
            api_version: self.api_version(),
            available: true,
            transports: self.transports(),
            capabilities: self.capabilities(),
            status: DeviceStatus::Unknown,
        }
    }

    async fn discover(
        &self,
        request: &DiscoveryRequest,
    ) -> Result<Vec<DeviceCandidate>, DeviceError>;

    async fn pair(&self, candidate: &DeviceCandidate) -> Result<DeviceRecord, DeviceError>;

    async fn unpair(&self, record: &DeviceRecord) -> Result<(), DeviceError>;

    async fn connect(&self, record: &DeviceRecord) -> Result<DeviceStatus, DeviceError>;

    async fn disconnect(&self, record: &DeviceRecord) -> Result<DeviceStatus, DeviceError>;

    async fn status(&self, record: &DeviceRecord) -> Result<DeviceStatus, DeviceError>;

    async fn audio_play(
        &self,
        _ctx: &DeviceRequestContext,
        _record: &DeviceRecord,
        _audio: &[u8],
    ) -> Result<DeviceActionReceipt, DeviceError> {
        Err(DeviceError::new(
            DeviceErrorCategory::Unsupported,
            "设备适配器未声明音频输出动作",
        ))
    }

    async fn camera_capture(
        &self,
        _ctx: &DeviceRequestContext,
        _record: &DeviceRecord,
    ) -> Result<DeviceCapture, DeviceError> {
        Err(DeviceError::new(
            DeviceErrorCategory::Unsupported,
            "设备适配器未声明图像采集动作",
        ))
    }

    async fn cancel(&self, _request_id: &str) -> Result<(), DeviceError> {
        Ok(())
    }
}

struct ActiveDeviceRequest {
    device_id: String,
    agent_id: String,
    session_id: Option<String>,
    capability: DeviceCapability,
    cancellation: watch::Sender<bool>,
}

struct CaptureConfirmation {
    agent_id: String,
    session_id: String,
    device_id: String,
    expires_at: Instant,
}

/// 可由解绑、授权撤销或实验开关关闭触发取消的单次动作句柄。
pub struct DeviceActionLease {
    pub request_id: String,
    pub device_id: String,
    pub capability: DeviceCapability,
    cancellation: watch::Receiver<bool>,
}

impl DeviceActionLease {
    pub fn is_cancelled(&self) -> bool {
        *self.cancellation.borrow()
    }

    pub async fn cancelled(&mut self) {
        while !self.is_cancelled() && self.cancellation.changed().await.is_ok() {}
    }
}

/// Core 设备注册表与运行态服务。默认没有适配器，因此不会扫描、连接或伪造能力。
pub struct DeviceService {
    config: RwLock<crate::config::DeviceConnectivityConfig>,
    records: RwLock<BTreeMap<String, DeviceRecord>>,
    candidates: RwLock<BTreeMap<String, DeviceCandidate>>,
    candidate_adapters: RwLock<HashMap<String, String>>,
    runtimes: RwLock<BTreeMap<String, DeviceRuntimeState>>,
    adapters: RwLock<Vec<Arc<dyn DeviceAdapter>>>,
    audits: Mutex<Vec<DeviceAuditEvent>>,
    active: Mutex<HashMap<String, ActiveDeviceRequest>>,
    capture_confirmations: Mutex<HashMap<String, CaptureConfirmation>>,
    discovery_gate: tokio::sync::Mutex<()>,
    events: broadcast::Sender<crate::types::CoreEvent>,
}

impl DeviceService {
    pub fn new(
        config: crate::config::DeviceConnectivityConfig,
        records: Vec<DeviceRecord>,
        events: broadcast::Sender<crate::types::CoreEvent>,
    ) -> Result<Self, DeviceError> {
        Self::validate_records(&records)?;
        let records: BTreeMap<String, DeviceRecord> = records
            .into_iter()
            .map(|record| (record.id.clone(), record))
            .collect();
        let runtimes = records
            .keys()
            .map(|device_id| {
                (
                    device_id.clone(),
                    DeviceRuntimeState {
                        status: DeviceStatus::Disconnected,
                        ..Default::default()
                    },
                )
            })
            .collect();
        Ok(Self {
            config: RwLock::new(config.bounded()),
            records: RwLock::new(records),
            candidates: RwLock::new(BTreeMap::new()),
            candidate_adapters: RwLock::new(HashMap::new()),
            runtimes: RwLock::new(runtimes),
            adapters: RwLock::new(Vec::new()),
            audits: Mutex::new(Vec::new()),
            active: Mutex::new(HashMap::new()),
            capture_confirmations: Mutex::new(HashMap::new()),
            discovery_gate: tokio::sync::Mutex::new(()),
            events,
        })
    }

    pub fn validate_records(records: &[DeviceRecord]) -> Result<(), DeviceError> {
        let mut ids = HashSet::new();
        for record in records {
            if record.id.trim().is_empty() || record.name.trim().is_empty() {
                return Err(DeviceError::new(
                    DeviceErrorCategory::InvalidRequest,
                    "设备 id 与名称不能为空",
                ));
            }
            if !ids.insert(record.id.as_str()) {
                return Err(DeviceError::new(
                    DeviceErrorCategory::Conflict,
                    "设备登记包含重复 id",
                ));
            }
            validate_endpoint(record.transport, record.endpoint.as_deref())?;
            let mut grant_ids = HashSet::new();
            for grant in &record.grants {
                if grant.device_id != record.id
                    || grant.id.trim().is_empty()
                    || !grant_ids.insert(grant.id.as_str())
                    || !grant.capabilities.is_subset(&record.capabilities)
                    || grant
                        .agent_id
                        .as_ref()
                        .is_some_and(|id| id.trim().is_empty() || id.len() > 128)
                    || grant
                        .session_id
                        .as_ref()
                        .is_some_and(|id| id.trim().is_empty() || id.len() > 256)
                {
                    return Err(DeviceError::new(
                        DeviceErrorCategory::InvalidRequest,
                        "设备授权标识、设备归属或能力声明无效",
                    ));
                }
            }
        }
        Ok(())
    }

    pub fn enabled(&self) -> bool {
        self.config.read().enabled
    }

    pub fn config(&self) -> crate::config::DeviceConnectivityConfig {
        self.config.read().clone()
    }

    pub fn register_adapter(&self, adapter: Arc<dyn DeviceAdapter>) -> Result<(), DeviceError> {
        if adapter.api_version() != DEVICE_ADAPTER_API_VERSION {
            return Err(DeviceError::new(
                DeviceErrorCategory::Unsupported,
                "设备适配器契约版本不兼容",
            ));
        }
        if adapter.id().trim().is_empty() || adapter.transports().is_empty() {
            return Err(DeviceError::new(
                DeviceErrorCategory::InvalidRequest,
                "设备适配器标识或传输声明无效",
            ));
        }
        let mut adapters = self.adapters.write();
        if adapters.iter().any(|item| item.id() == adapter.id()) {
            return Err(DeviceError::new(
                DeviceErrorCategory::Conflict,
                "设备适配器已注册",
            ));
        }
        adapters.push(adapter);
        Ok(())
    }

    pub fn adapters(&self) -> Vec<String> {
        self.adapters
            .read()
            .iter()
            .map(|adapter| adapter.id().to_string())
            .collect()
    }

    pub fn adapter_probes(&self) -> Vec<DeviceAdapterProbe> {
        self.adapters
            .read()
            .iter()
            .map(|adapter| adapter.probe())
            .collect()
    }

    pub fn candidates(&self) -> Vec<DeviceCandidate> {
        self.candidates.read().values().cloned().collect()
    }

    pub fn records(&self) -> Vec<DeviceRecord> {
        self.records.read().values().cloned().collect()
    }

    pub fn audits(&self) -> Vec<DeviceAuditEvent> {
        self.audits.lock().clone()
    }

    pub fn snapshots(&self) -> Vec<DeviceSnapshot> {
        self.records
            .read()
            .values()
            .map(|record| self.snapshot_for(record))
            .collect()
    }

    /// 同一判定供工具 schema 和执行入口使用，目录存在不等于该主体可执行。
    pub fn authorized_devices(
        &self,
        agent_id: &str,
        session_id: &str,
        capability: DeviceCapability,
    ) -> Vec<String> {
        if !self.enabled() {
            return Vec::new();
        }
        self.records
            .read()
            .values()
            .filter(|record| {
                record.enabled
                    && record.capabilities.contains(&capability)
                    && record
                        .grants
                        .iter()
                        .any(|grant| grant.allows_for(agent_id, session_id, capability))
                    && self.adapter_for(record.transport).is_some_and(|adapter| {
                        let probe = adapter.probe();
                        probe.available && probe.capabilities.contains(&capability)
                    })
            })
            .map(|record| record.id.clone())
            .collect()
    }

    /// 只能由已认证管理入口在取得用户确认后调用；模型工具不暴露此方法。
    pub fn confirm_camera_capture(
        &self,
        ctx: &DeviceRequestContext,
        device_id: &str,
        agent_id: &str,
        session_id: &str,
    ) -> Result<String, DeviceError> {
        self.ensure_enabled(ctx, "camera.confirm", Some(device_id))?;
        if !self
            .authorized_devices(agent_id, session_id, DeviceCapability::ImageCapture)
            .iter()
            .any(|id| id == device_id)
        {
            let error = DeviceError::new(
                DeviceErrorCategory::Forbidden,
                "该 Agent/会话未获摄像头采集授权",
            );
            self.audit_error(ctx, "camera.confirm", Some(device_id), &error);
            return Err(error);
        }
        let now = Instant::now();
        let mut confirmations = self.capture_confirmations.lock();
        confirmations.retain(|_, item| item.expires_at > now);
        if confirmations.values().any(|item| {
            item.agent_id == agent_id
                && item.session_id == session_id
                && item.device_id == device_id
        }) {
            return Err(DeviceError::new(
                DeviceErrorCategory::Conflict,
                "该设备/会话/Agent 已有待消费的一次性确认",
            ));
        }
        if confirmations.len() >= 256 {
            return Err(DeviceError::new(
                DeviceErrorCategory::LimitExceeded,
                "待消费的摄像头确认已达上限",
            ));
        }
        let id = crate::types::new_id();
        confirmations.insert(
            id.clone(),
            CaptureConfirmation {
                agent_id: agent_id.to_string(),
                session_id: session_id.to_string(),
                device_id: device_id.to_string(),
                expires_at: now + Duration::from_secs(60),
            },
        );
        drop(confirmations);
        self.audit_success(
            ctx,
            "camera.confirm",
            Some(device_id),
            "用户已确认一次摄像头采集",
        );
        Ok(id)
    }

    fn consume_camera_confirmation(&self, ctx: &DeviceRequestContext, device_id: &str) -> bool {
        let now = Instant::now();
        let mut confirmations = self.capture_confirmations.lock();
        confirmations.retain(|_, item| item.expires_at > now);
        let id = confirmations
            .iter()
            .find(|(_, item)| {
                item.agent_id == ctx.actor
                    && Some(item.session_id.as_str()) == ctx.session_id.as_deref()
                    && item.device_id == device_id
            })
            .map(|(id, _)| id.clone());
        id.is_some_and(|id| confirmations.remove(&id).is_some())
    }

    pub fn end_session(&self, session_id: &str) {
        self.capture_confirmations
            .lock()
            .retain(|_, item| item.session_id != session_id);
        for action in self.active.lock().values() {
            if action.session_id.as_deref() == Some(session_id) {
                action.cancellation.send_replace(true);
            }
        }
    }

    pub async fn camera_capture(
        &self,
        ctx: &DeviceRequestContext,
        device_id: &str,
    ) -> Result<DeviceCapture, DeviceError> {
        if ctx.session_id.is_none() {
            return Err(DeviceError::new(
                DeviceErrorCategory::Forbidden,
                "摄像头采集必须绑定会话",
            ));
        }
        let mut lease = self.begin_action(ctx, device_id, DeviceCapability::ImageCapture)?;
        let record = self
            .record(device_id)
            .ok_or_else(|| DeviceError::new(DeviceErrorCategory::NotFound, "设备不存在"))?;
        let adapter = self.adapter_for(record.transport).ok_or_else(|| {
            DeviceError::new(DeviceErrorCategory::Unsupported, "设备适配器已不可用")
        })?;
        let result = tokio::select! {
            result = adapter.camera_capture(ctx, &record) => result,
            _ = lease.cancelled() => {
                let _ = adapter.cancel(&ctx.request_id).await;
                Err(DeviceError::new(DeviceErrorCategory::Cancelled, "摄像头采集已取消"))
            }
        };
        match result {
            Ok(capture) if !lease.is_cancelled() => {
                self.finish_action(ctx, &lease, Ok(()))?;
                Ok(capture)
            }
            Ok(_) => {
                let error = DeviceError::new(DeviceErrorCategory::Cancelled, "摄像头采集已取消");
                let _ = self.finish_action(ctx, &lease, Err(error.clone()));
                Err(error)
            }
            Err(error) => {
                let error = safe_error(error);
                let _ = self.finish_action(ctx, &lease, Err(error.clone()));
                Err(error)
            }
        }
    }

    pub async fn audio_play(
        &self,
        ctx: &DeviceRequestContext,
        device_id: &str,
        audio: &[u8],
    ) -> Result<DeviceActionReceipt, DeviceError> {
        let mut lease = self.begin_action(ctx, device_id, DeviceCapability::AudioOutput)?;
        let record = self
            .record(device_id)
            .ok_or_else(|| DeviceError::new(DeviceErrorCategory::NotFound, "设备不存在"))?;
        let adapter = self.adapter_for(record.transport).ok_or_else(|| {
            DeviceError::new(DeviceErrorCategory::Unsupported, "设备适配器已不可用")
        })?;
        let result = tokio::select! {
            result = adapter.audio_play(ctx, &record, audio) => result,
            _ = lease.cancelled() => {
                let _ = adapter.cancel(&ctx.request_id).await;
                Err(DeviceError::new(DeviceErrorCategory::Cancelled, "音频播放已取消"))
            }
        };
        match result {
            Ok(receipt) if !lease.is_cancelled() => {
                self.finish_action(ctx, &lease, Ok(()))?;
                Ok(receipt)
            }
            Ok(_) => {
                let error = DeviceError::new(DeviceErrorCategory::Cancelled, "音频播放已取消");
                let _ = self.finish_action(ctx, &lease, Err(error.clone()));
                Err(error)
            }
            Err(error) => {
                let error = safe_error(error);
                let _ = self.finish_action(ctx, &lease, Err(error.clone()));
                Err(error)
            }
        }
    }

    pub fn apply_config(
        &self,
        ctx: &DeviceRequestContext,
        config: crate::config::DeviceConnectivityConfig,
        records: Vec<DeviceRecord>,
    ) -> Result<(), DeviceError> {
        self.validate_context(ctx)?;
        Self::validate_records(&records)?;
        let config = config.bounded();
        let was_enabled = self.config.read().enabled;
        let enabled = config.enabled;
        let next_records: BTreeMap<String, DeviceRecord> = records
            .into_iter()
            .map(|record| (record.id.clone(), record))
            .collect();
        for action in self.active.lock().values() {
            let still_authorized = next_records.get(&action.device_id).is_some_and(|record| {
                record.enabled
                    && record.capabilities.contains(&action.capability)
                    && record.grants.iter().any(|grant| {
                        grant.allows_for(
                            &action.agent_id,
                            action.session_id.as_deref().unwrap_or_default(),
                            action.capability,
                        )
                    })
            });
            if !still_authorized {
                action.cancellation.send_replace(true);
            }
        }
        let mut runtimes = self.runtimes.write();
        runtimes.retain(|device_id, _| next_records.contains_key(device_id));
        for device_id in next_records.keys() {
            runtimes
                .entry(device_id.clone())
                .or_insert_with(|| DeviceRuntimeState {
                    status: DeviceStatus::Disconnected,
                    ..Default::default()
                });
        }
        drop(runtimes);
        *self.config.write() = config;
        *self.records.write() = next_records;
        if was_enabled && !enabled {
            self.cancel_all_active();
            self.capture_confirmations.lock().clear();
            self.candidates.write().clear();
            self.candidate_adapters.write().clear();
            for runtime in self.runtimes.write().values_mut() {
                runtime.status = DeviceStatus::Disconnected;
                runtime.request_id = None;
                runtime.updated_at = crate::types::now_iso();
            }
            self.audit_success(ctx, "disable", None, "设备实验已关闭，活动动作已停止");
        } else {
            self.audit_success(ctx, "configure", None, "设备配置已更新");
        }
        Ok(())
    }

    pub async fn discover(
        &self,
        ctx: &DeviceRequestContext,
        request: DiscoveryRequest,
    ) -> Result<Vec<DeviceCandidate>, DeviceError> {
        self.ensure_enabled(ctx, "discover", None)?;
        if request.duration_secs == 0 || request.scope.len() > 256 {
            let error = DeviceError::new(DeviceErrorCategory::InvalidRequest, "发现范围或时长无效");
            self.audit_error(ctx, "discover", None, &error);
            return Err(error);
        }
        let _discovery = self.discovery_gate.lock().await;
        self.candidates.write().clear();
        self.candidate_adapters.write().clear();
        let config = self.config();
        let bounded = DiscoveryRequest {
            duration_secs: request.duration_secs.min(config.max_discovery_secs),
            max_results: if request.max_results == 0 {
                config.max_discovery_results
            } else {
                request.max_results.min(config.max_discovery_results)
            },
            ..request
        };
        let adapters = self
            .adapters
            .read()
            .iter()
            .filter(|adapter| adapter.transports().contains(&bounded.transport))
            .cloned()
            .collect::<Vec<_>>();
        if adapters.is_empty() {
            let error = DeviceError::new(
                DeviceErrorCategory::Unsupported,
                "当前宿主没有可用的设备发现后端",
            );
            self.audit_error(ctx, "discover", None, &error);
            return Err(error);
        }
        let mut found: BTreeMap<String, (DeviceCandidate, String)> = BTreeMap::new();
        let deadline = Instant::now() + Duration::from_secs(bounded.duration_secs);
        let mut last_backend_error = None;
        for adapter in adapters {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let discovered = if remaining.is_zero() {
                Err(DeviceError::new(
                    DeviceErrorCategory::Unavailable,
                    "设备发现超过时限",
                ))
            } else {
                match tokio::time::timeout(remaining, adapter.discover(&bounded)).await {
                    Ok(result) => result,
                    Err(_) => Err(DeviceError::new(
                        DeviceErrorCategory::Unavailable,
                        "设备发现超过时限",
                    )),
                }
            };
            match discovered {
                Ok(candidates) => {
                    for candidate in candidates {
                        if candidate.transport != bounded.transport
                            || self.records.read().contains_key(&candidate.id)
                        {
                            continue;
                        }
                        if let Err(error) = validate_candidate(&candidate) {
                            self.audit_error(ctx, "discover", None, &error);
                            return Err(error);
                        }
                        if !candidate.capabilities.is_subset(&adapter.capabilities()) {
                            let error = DeviceError::new(
                                DeviceErrorCategory::Unsupported,
                                "设备候选声明了适配器未支持的能力",
                            );
                            self.audit_error(ctx, "discover", None, &error);
                            return Err(error);
                        }
                        found
                            .entry(candidate.id.clone())
                            .or_insert_with(|| (candidate, adapter.id().to_string()));
                        if found.len() >= bounded.max_results {
                            break;
                        }
                    }
                }
                Err(error) => {
                    let error = safe_error(error);
                    if matches!(
                        error.category,
                        DeviceErrorCategory::Unsupported | DeviceErrorCategory::Unavailable
                    ) {
                        last_backend_error = Some(error);
                        continue;
                    }
                    self.audit_error(ctx, "discover", None, &error);
                    return Err(error);
                }
            }
            if found.len() >= bounded.max_results {
                break;
            }
        }
        if found.is_empty() {
            if let Some(error) = last_backend_error {
                self.audit_error(ctx, "discover", None, &error);
                return Err(error);
            }
        }
        let found: Vec<_> = found.into_values().take(bounded.max_results).collect();
        let result: Vec<_> = found
            .iter()
            .map(|(candidate, _)| candidate.clone())
            .collect();
        *self.candidates.write() = result
            .iter()
            .cloned()
            .map(|candidate| (candidate.id.clone(), candidate))
            .collect();
        *self.candidate_adapters.write() = found
            .into_iter()
            .map(|(candidate, adapter_id)| (candidate.id, adapter_id))
            .collect();
        self.audit_success(ctx, "discover", None, "已完成一次有界设备发现");
        Ok(result)
    }

    pub async fn pair(
        &self,
        ctx: &DeviceRequestContext,
        candidate_id: &str,
        agent_id: &str,
        session_id: Option<&str>,
        capabilities: BTreeSet<DeviceCapability>,
        confirmed: bool,
    ) -> Result<DeviceSnapshot, DeviceError> {
        self.ensure_enabled(ctx, "pair", Some(candidate_id))?;
        if (!capabilities.is_empty() && agent_id.trim().is_empty())
            || agent_id.len() > 128
            || session_id.is_some_and(|id| id.trim().is_empty() || id.len() > 256)
        {
            let error = DeviceError::new(
                DeviceErrorCategory::InvalidRequest,
                "设备能力授权必须指定有效 Agent 主体和会话范围",
            );
            self.audit_error(ctx, "pair", Some(candidate_id), &error);
            return Err(error);
        }
        if !confirmed {
            let error = DeviceError::new(DeviceErrorCategory::Forbidden, "配对必须经过用户确认");
            self.audit_error(ctx, "pair", Some(candidate_id), &error);
            return Err(error);
        }
        let candidate = self
            .candidates
            .read()
            .get(candidate_id)
            .cloned()
            .ok_or_else(|| {
                let error = DeviceError::new(DeviceErrorCategory::NotFound, "设备候选不存在");
                self.audit_error(ctx, "pair", Some(candidate_id), &error);
                error
            })?;
        if self.records.read().contains_key(candidate_id) {
            let error = DeviceError::new(DeviceErrorCategory::Conflict, "设备已经配对");
            self.audit_error(ctx, "pair", Some(candidate_id), &error);
            return Err(error);
        }
        let adapter_id = match self.candidate_adapters.read().get(candidate_id).cloned() {
            Some(adapter_id) => adapter_id,
            None => {
                let error =
                    DeviceError::new(DeviceErrorCategory::Unavailable, "发现适配器已不可用");
                self.audit_error(ctx, "pair", Some(candidate_id), &error);
                return Err(error);
            }
        };
        let adapter = match self.adapter_by_id(&adapter_id) {
            Some(adapter) => adapter,
            None => {
                let error =
                    DeviceError::new(DeviceErrorCategory::Unsupported, "设备适配器已不可用");
                self.audit_error(ctx, "pair", Some(candidate_id), &error);
                return Err(error);
            }
        };
        if !capabilities.is_subset(&candidate.capabilities)
            || !capabilities.is_subset(&adapter.capabilities())
        {
            let error = DeviceError::new(
                DeviceErrorCategory::Forbidden,
                "请求的设备能力未由适配器声明",
            );
            self.audit_error(ctx, "pair", Some(candidate_id), &error);
            return Err(error);
        }
        let mut record = match adapter.pair(&candidate).await {
            Ok(record) => record,
            Err(error) => {
                let error = safe_error(error);
                self.audit_error(ctx, "pair", Some(candidate_id), &error);
                return Err(error);
            }
        };
        if record.id != candidate.id {
            let _ = adapter.unpair(&record).await;
            let error = DeviceError::new(
                DeviceErrorCategory::Internal,
                "适配器返回了不匹配的设备标识",
            );
            self.audit_error(ctx, "pair", Some(candidate_id), &error);
            return Err(error);
        }
        record.id = candidate.id.clone();
        record.name = candidate.name.clone();
        record.transport = candidate.transport;
        record.kind = candidate.kind;
        record.endpoint = candidate.endpoint.clone();
        record.capabilities = candidate.capabilities.clone();
        record.grants = if capabilities.is_empty() || agent_id.trim().is_empty() {
            Vec::new()
        } else {
            vec![DeviceGrant {
                id: crate::types::new_id(),
                device_id: record.id.clone(),
                capabilities,
                agent_id: Some(agent_id.trim().to_string()),
                session_id: session_id.map(str::to_string),
                granted_at: crate::types::now_iso(),
                revoked_at: None,
            }]
        };
        record.enabled = true;
        self.records
            .write()
            .insert(record.id.clone(), record.clone());
        self.runtimes.write().insert(
            record.id.clone(),
            DeviceRuntimeState {
                status: DeviceStatus::Disconnected,
                updated_at: crate::types::now_iso(),
                last_error: None,
                request_id: Some(ctx.request_id.clone()),
            },
        );
        self.candidates.write().remove(candidate_id);
        self.candidate_adapters.write().remove(candidate_id);
        self.audit_success(
            ctx,
            "pair",
            Some(&record.id),
            "设备已配对，能力按用户选择授权",
        );
        Ok(self.snapshot_for(&record))
    }

    pub async fn unpair(
        &self,
        ctx: &DeviceRequestContext,
        device_id: &str,
    ) -> Result<(), DeviceError> {
        self.validate_context(ctx)?;
        let record = match self.record(device_id) {
            Some(record) => record,
            None => {
                let error = DeviceError::new(DeviceErrorCategory::NotFound, "设备不存在");
                self.audit_error(ctx, "unpair", Some(device_id), &error);
                return Err(error);
            }
        };
        self.cancel_device_actions(device_id);
        if let Some(adapter) = self.adapter_for(record.transport) {
            if let Err(error) = adapter.unpair(&record).await {
                let error = safe_error(error);
                self.audit_error(ctx, "unpair", Some(device_id), &error);
                return Err(error);
            }
        }
        self.records.write().remove(device_id);
        self.runtimes.write().remove(device_id);
        self.audit_success(ctx, "unpair", Some(device_id), "设备已解绑，活动动作已停止");
        Ok(())
    }

    pub async fn connect(
        &self,
        ctx: &DeviceRequestContext,
        device_id: &str,
    ) -> Result<DeviceSnapshot, DeviceError> {
        self.ensure_enabled(ctx, "connect", Some(device_id))?;
        let record = match self.record(device_id) {
            Some(record) => record,
            None => {
                let error = DeviceError::new(DeviceErrorCategory::NotFound, "设备不存在");
                self.audit_error(ctx, "connect", Some(device_id), &error);
                return Err(error);
            }
        };
        if !record.enabled {
            let error = DeviceError::new(DeviceErrorCategory::Forbidden, "设备已被禁用");
            self.audit_error(ctx, "connect", Some(device_id), &error);
            return Err(error);
        }
        let adapter = match self.adapter_for(record.transport) {
            Some(adapter) => adapter,
            None => {
                let error =
                    DeviceError::new(DeviceErrorCategory::Unsupported, "设备适配器已不可用");
                self.audit_error(ctx, "connect", Some(device_id), &error);
                return Err(error);
            }
        };
        self.set_runtime(device_id, DeviceStatus::Connecting, None, Some(ctx));
        match adapter.connect(&record).await {
            Ok(DeviceStatus::Connected) => {
                let status = DeviceStatus::Connected;
                self.set_runtime(device_id, status, None, Some(ctx));
                self.audit_success(
                    ctx,
                    "connect",
                    Some(device_id),
                    "设备连接状态已由适配器确认",
                );
                Ok(self.snapshot_for(&record))
            }
            Ok(status) => {
                self.set_runtime(device_id, status, None, Some(ctx));
                let category = match status {
                    DeviceStatus::Unsupported => DeviceErrorCategory::Unsupported,
                    DeviceStatus::Error => DeviceErrorCategory::Internal,
                    _ => DeviceErrorCategory::Unavailable,
                };
                let error = DeviceError::new(category, "适配器未确认设备已连接");
                self.audit_error(ctx, "connect", Some(device_id), &error);
                Err(error)
            }
            Err(error) => {
                let error = safe_error(error);
                let status = match error.category {
                    DeviceErrorCategory::Unsupported => DeviceStatus::Unsupported,
                    DeviceErrorCategory::Unavailable => DeviceStatus::Unavailable,
                    _ => DeviceStatus::Error,
                };
                self.set_runtime(device_id, status, Some(error.clone()), Some(ctx));
                self.audit_error(ctx, "connect", Some(device_id), &error);
                Err(error)
            }
        }
    }

    pub async fn disconnect(
        &self,
        ctx: &DeviceRequestContext,
        device_id: &str,
    ) -> Result<DeviceSnapshot, DeviceError> {
        self.validate_context(ctx)?;
        let record = match self.record(device_id) {
            Some(record) => record,
            None => {
                let error = DeviceError::new(DeviceErrorCategory::NotFound, "设备不存在");
                self.audit_error(ctx, "disconnect", Some(device_id), &error);
                return Err(error);
            }
        };
        self.cancel_device_actions(device_id);
        self.set_runtime(device_id, DeviceStatus::Disconnecting, None, Some(ctx));
        let status = if let Some(adapter) = self.adapter_for(record.transport) {
            match adapter.disconnect(&record).await {
                Ok(status) => status,
                Err(error) => {
                    let error = safe_error(error);
                    self.set_runtime(
                        device_id,
                        DeviceStatus::Error,
                        Some(error.clone()),
                        Some(ctx),
                    );
                    self.audit_error(ctx, "disconnect", Some(device_id), &error);
                    return Err(error);
                }
            }
        } else {
            DeviceStatus::Disconnected
        };
        match status {
            DeviceStatus::Disconnected => {
                let status = DeviceStatus::Disconnected;
                self.set_runtime(device_id, status, None, Some(ctx));
                self.audit_success(ctx, "disconnect", Some(device_id), "设备已断开");
                Ok(self.snapshot_for(&record))
            }
            not_disconnected => {
                self.set_runtime(device_id, not_disconnected, None, Some(ctx));
                let error =
                    DeviceError::new(DeviceErrorCategory::Unavailable, "适配器尚未确认设备已断开");
                self.audit_error(ctx, "disconnect", Some(device_id), &error);
                Err(error)
            }
        }
    }

    pub async fn status(
        &self,
        ctx: &DeviceRequestContext,
        device_id: &str,
    ) -> Result<DeviceSnapshot, DeviceError> {
        self.validate_context(ctx)?;
        let record = match self.record(device_id) {
            Some(record) => record,
            None => {
                let error = DeviceError::new(DeviceErrorCategory::NotFound, "设备不存在");
                self.audit_error(ctx, "status", Some(device_id), &error);
                return Err(error);
            }
        };
        if self.enabled() {
            match self.adapter_for(record.transport) {
                Some(adapter) => match adapter.status(&record).await {
                    Ok(status) => self.set_runtime(device_id, status, None, Some(ctx)),
                    Err(error) => {
                        let error = safe_error(error);
                        self.set_runtime(
                            device_id,
                            DeviceStatus::Unavailable,
                            Some(error.clone()),
                            Some(ctx),
                        );
                        self.audit_error(ctx, "status", Some(device_id), &error);
                    }
                },
                None => {
                    let error =
                        DeviceError::new(DeviceErrorCategory::Unsupported, "设备适配器已不可用");
                    self.set_runtime(
                        device_id,
                        DeviceStatus::Unsupported,
                        Some(error.clone()),
                        Some(ctx),
                    );
                    self.audit_error(ctx, "status", Some(device_id), &error);
                }
            }
        }
        if !self.enabled() {
            self.audit_success(
                ctx,
                "status",
                Some(device_id),
                "设备实验已关闭，返回最后运行状态",
            );
        }
        Ok(self.snapshot_for(&record))
    }

    pub fn grant_capabilities(
        &self,
        ctx: &DeviceRequestContext,
        device_id: &str,
        agent_id: &str,
        session_id: Option<&str>,
        capabilities: BTreeSet<DeviceCapability>,
    ) -> Result<DeviceSnapshot, DeviceError> {
        self.ensure_enabled(ctx, "grant", Some(device_id))?;
        let mut records = self.records.write();
        let record = match records.get_mut(device_id) {
            Some(record) => record,
            None => {
                let error = DeviceError::new(DeviceErrorCategory::NotFound, "设备不存在");
                drop(records);
                self.audit_error(ctx, "grant", Some(device_id), &error);
                return Err(error);
            }
        };
        if capabilities.is_empty()
            || agent_id.trim().is_empty()
            || agent_id.len() > 128
            || session_id.is_some_and(|id| id.trim().is_empty() || id.len() > 256)
            || !capabilities.is_subset(&record.capabilities)
        {
            let error = DeviceError::new(DeviceErrorCategory::Forbidden, "设备未声明请求的能力");
            drop(records);
            self.audit_error(ctx, "grant", Some(device_id), &error);
            return Err(error);
        }
        record.grants.push(DeviceGrant {
            id: crate::types::new_id(),
            device_id: device_id.to_string(),
            capabilities,
            agent_id: Some(agent_id.trim().to_string()),
            session_id: session_id.map(str::to_string),
            granted_at: crate::types::now_iso(),
            revoked_at: None,
        });
        let snapshot = self.snapshot_for(record);
        drop(records);
        self.audit_success(ctx, "grant", Some(device_id), "设备能力授权已更新");
        Ok(snapshot)
    }

    pub fn revoke_authorization(
        &self,
        ctx: &DeviceRequestContext,
        device_id: &str,
        capabilities: Option<&BTreeSet<DeviceCapability>>,
    ) -> Result<DeviceSnapshot, DeviceError> {
        self.validate_context(ctx)?;
        let mut records = self.records.write();
        let record = match records.get_mut(device_id) {
            Some(record) => record,
            None => {
                let error = DeviceError::new(DeviceErrorCategory::NotFound, "设备不存在");
                drop(records);
                self.audit_error(ctx, "revoke", Some(device_id), &error);
                return Err(error);
            }
        };
        let now = crate::types::now_iso();
        let mut revoked = false;
        let mut revoked_capabilities = BTreeSet::new();
        for grant in &mut record.grants {
            if !grant.is_active() {
                continue;
            }
            if let Some(wanted) = capabilities {
                let removed = grant.capabilities.iter().any(|cap| wanted.contains(cap));
                if removed {
                    grant.capabilities.retain(|cap| !wanted.contains(cap));
                    revoked_capabilities.extend(wanted.iter().copied());
                    revoked = true;
                    if grant.capabilities.is_empty() {
                        grant.revoked_at = Some(now.clone());
                    }
                }
            } else {
                grant.revoked_at = Some(now.clone());
                revoked_capabilities.extend(grant.capabilities.iter().copied());
                revoked = true;
            }
        }
        if !revoked {
            let error = DeviceError::new(DeviceErrorCategory::Conflict, "设备没有匹配的活动授权");
            drop(records);
            self.audit_error(ctx, "revoke", Some(device_id), &error);
            return Err(error);
        }
        let snapshot = self.snapshot_for(record);
        drop(records);
        self.cancel_device_actions_for(device_id, Some(&revoked_capabilities));
        self.audit_success(
            ctx,
            "revoke",
            Some(device_id),
            "设备能力授权已撤销，活动动作已停止",
        );
        Ok(snapshot)
    }

    pub async fn cancel(
        &self,
        ctx: &DeviceRequestContext,
        request_id: &str,
    ) -> Result<(), DeviceError> {
        self.validate_context(ctx)?;
        let device_id = {
            let active = self.active.lock();
            match active.get(request_id) {
                Some(entry)
                    if entry.agent_id == ctx.actor
                        && entry.session_id.as_deref() == ctx.session_id.as_deref() =>
                {
                    entry.cancellation.send_replace(true);
                    entry.device_id.clone()
                }
                Some(entry) => {
                    let device_id = entry.device_id.clone();
                    drop(active);
                    let error = DeviceError::new(
                        DeviceErrorCategory::Forbidden,
                        "设备动作不属于当前 Agent/会话",
                    );
                    self.audit_error(ctx, "cancel", Some(&device_id), &error);
                    return Err(error);
                }
                None => {
                    let error =
                        DeviceError::new(DeviceErrorCategory::NotFound, "活动设备请求不存在");
                    drop(active);
                    self.audit_error(ctx, "cancel", None, &error);
                    return Err(error);
                }
            }
        };
        if let Some(record) = self.record(&device_id) {
            if let Some(adapter) = self.adapter_for(record.transport) {
                if let Err(error) = adapter.cancel(request_id).await {
                    let error = safe_error(error);
                    self.audit_error(ctx, "cancel", Some(&device_id), &error);
                    return Err(error);
                }
            }
        }
        self.audit_success(ctx, "cancel", Some(&device_id), "设备动作已请求取消");
        Ok(())
    }

    pub fn is_cancelled(&self, request_id: &str) -> bool {
        self.active
            .lock()
            .get(request_id)
            .is_some_and(|entry| *entry.cancellation.borrow())
    }

    pub fn begin_action(
        &self,
        ctx: &DeviceRequestContext,
        device_id: &str,
        capability: DeviceCapability,
    ) -> Result<DeviceActionLease, DeviceError> {
        self.ensure_enabled(ctx, "action", Some(device_id))?;
        let record = match self.record(device_id) {
            Some(record) => record,
            None => {
                let error = DeviceError::new(DeviceErrorCategory::NotFound, "设备不存在");
                self.audit_error(ctx, "action", Some(device_id), &error);
                return Err(error);
            }
        };
        let state = self
            .runtimes
            .read()
            .get(device_id)
            .cloned()
            .unwrap_or_default();
        if !record.enabled
            || !record.capabilities.contains(&capability)
            || !record.grants.iter().any(|grant| {
                grant.allows_for(
                    &ctx.actor,
                    ctx.session_id.as_deref().unwrap_or_default(),
                    capability,
                )
            })
        {
            let error = DeviceError::new(DeviceErrorCategory::Forbidden, "设备动作未获用户授权");
            self.audit_error(ctx, "action", Some(device_id), &error);
            return Err(error);
        }
        if state.status != DeviceStatus::Connected && capability != DeviceCapability::ImageCapture {
            let error = DeviceError::new(DeviceErrorCategory::Unavailable, "设备当前未连接");
            self.audit_error(ctx, "action", Some(device_id), &error);
            return Err(error);
        }
        let adapter = match self.adapter_for(record.transport) {
            Some(adapter) => adapter,
            None => {
                let error =
                    DeviceError::new(DeviceErrorCategory::Unsupported, "设备适配器已不可用");
                self.audit_error(ctx, "action", Some(device_id), &error);
                return Err(error);
            }
        };
        if !adapter.capabilities().contains(&capability) {
            let error =
                DeviceError::new(DeviceErrorCategory::Unsupported, "适配器未声明该设备动作");
            self.audit_error(ctx, "action", Some(device_id), &error);
            return Err(error);
        }
        let (cancellation_sender, cancellation_receiver) = watch::channel(false);
        let mut active = self.active.lock();
        if active.contains_key(&ctx.request_id)
            || active.values().any(|action| action.device_id == device_id)
        {
            let error = DeviceError::new(
                DeviceErrorCategory::Conflict,
                "request id 已用于活动设备动作",
            );
            drop(active);
            self.audit_error(ctx, "action", Some(device_id), &error);
            return Err(error);
        }
        if capability == DeviceCapability::ImageCapture
            && !self.consume_camera_confirmation(ctx, device_id)
        {
            drop(active);
            let error = DeviceError::new(
                DeviceErrorCategory::Forbidden,
                "缺少当前设备/会话/Agent 的一次性用户确认",
            );
            self.audit_error(ctx, "action", Some(device_id), &error);
            return Err(error);
        }
        active.insert(
            ctx.request_id.clone(),
            ActiveDeviceRequest {
                device_id: device_id.to_string(),
                agent_id: ctx.actor.clone(),
                session_id: ctx.session_id.clone(),
                capability,
                cancellation: cancellation_sender,
            },
        );
        drop(active);
        self.audit_success(ctx, "action_started", Some(device_id), "设备动作已受理");
        Ok(DeviceActionLease {
            request_id: ctx.request_id.clone(),
            device_id: device_id.to_string(),
            capability,
            cancellation: cancellation_receiver,
        })
    }

    pub fn finish_action(
        &self,
        ctx: &DeviceRequestContext,
        lease: &DeviceActionLease,
        result: Result<(), DeviceError>,
    ) -> Result<(), DeviceError> {
        if ctx.request_id != lease.request_id {
            let error = DeviceError::new(
                DeviceErrorCategory::InvalidRequest,
                "动作 request id 不匹配",
            );
            self.audit_error(ctx, "action_finished", Some(&lease.device_id), &error);
            return Err(error);
        }
        self.active.lock().remove(&lease.request_id);
        if lease.is_cancelled() {
            let error = DeviceError::new(DeviceErrorCategory::Cancelled, "设备动作已安全停止");
            self.audit_error(ctx, "action_cancelled", Some(&lease.device_id), &error);
            return Err(error);
        }
        match result {
            Ok(()) => {
                self.audit_success(
                    ctx,
                    "action_finished",
                    Some(&lease.device_id),
                    "设备动作已完成",
                );
                Ok(())
            }
            Err(error) => {
                let error = safe_error(error);
                self.audit_error(ctx, "action_finished", Some(&lease.device_id), &error);
                Err(error)
            }
        }
    }

    fn ensure_enabled(
        &self,
        ctx: &DeviceRequestContext,
        action: &str,
        device_id: Option<&str>,
    ) -> Result<(), DeviceError> {
        self.validate_context(ctx)?;
        if self.enabled() {
            Ok(())
        } else {
            let error = DeviceError::new(DeviceErrorCategory::Forbidden, "物理设备连接实验未启用");
            self.audit_error(ctx, action, device_id, &error);
            Err(error)
        }
    }

    fn record(&self, device_id: &str) -> Option<DeviceRecord> {
        self.records.read().get(device_id).cloned()
    }

    fn validate_context(&self, ctx: &DeviceRequestContext) -> Result<(), DeviceError> {
        if ctx.request_id.trim().is_empty() || ctx.actor.trim().is_empty() {
            return Err(DeviceError::new(
                DeviceErrorCategory::InvalidRequest,
                "设备请求必须包含 request id 与调用者",
            ));
        }
        if ctx.request_id.len() > 128
            || ctx.actor.len() > 128
            || ctx.session_id.as_ref().is_some_and(|id| id.len() > 256)
        {
            return Err(DeviceError::new(
                DeviceErrorCategory::InvalidRequest,
                "设备请求上下文超出长度上限",
            ));
        }
        Ok(())
    }

    fn adapter_for(&self, transport: DeviceTransport) -> Option<Arc<dyn DeviceAdapter>> {
        self.adapters
            .read()
            .iter()
            .find(|adapter| adapter.transports().contains(&transport))
            .cloned()
    }

    fn adapter_by_id(&self, adapter_id: &str) -> Option<Arc<dyn DeviceAdapter>> {
        self.adapters
            .read()
            .iter()
            .find(|adapter| adapter.id() == adapter_id)
            .cloned()
    }

    fn snapshot_for(&self, record: &DeviceRecord) -> DeviceSnapshot {
        DeviceSnapshot {
            record: record.clone(),
            runtime: self
                .runtimes
                .read()
                .get(&record.id)
                .cloned()
                .unwrap_or_default(),
        }
    }

    fn set_runtime(
        &self,
        device_id: &str,
        status: DeviceStatus,
        error: Option<DeviceError>,
        ctx: Option<&DeviceRequestContext>,
    ) {
        let request_id = ctx.map(|ctx| ctx.request_id.clone());
        self.runtimes.write().insert(
            device_id.to_string(),
            DeviceRuntimeState {
                status,
                updated_at: crate::types::now_iso(),
                last_error: error,
                request_id,
            },
        );
    }

    fn audit_success(
        &self,
        ctx: &DeviceRequestContext,
        action: &str,
        device_id: Option<&str>,
        result: &str,
    ) {
        self.audit(DeviceAuditEvent {
            request_id: ctx.request_id.clone(),
            actor: ctx.actor.clone(),
            session_id: ctx.session_id.clone(),
            action: action.to_string(),
            device_id: device_id.map(str::to_string),
            result: result.to_string(),
            error_category: None,
            at: crate::types::now_iso(),
        });
    }

    fn audit_error(
        &self,
        ctx: &DeviceRequestContext,
        action: &str,
        device_id: Option<&str>,
        error: &DeviceError,
    ) {
        self.audit(DeviceAuditEvent {
            request_id: ctx.request_id.clone(),
            actor: ctx.actor.clone(),
            session_id: ctx.session_id.clone(),
            action: action.to_string(),
            device_id: device_id.map(str::to_string),
            result: "failed".into(),
            error_category: Some(error.category),
            at: crate::types::now_iso(),
        });
    }

    fn audit(&self, audit: DeviceAuditEvent) {
        let payload = serde_json::to_value(&audit).unwrap_or(serde_json::Value::Null);
        let mut audits = self.audits.lock();
        if audits.len() == MAX_AUDIT_EVENTS {
            audits.remove(0);
        }
        audits.push(audit.clone());
        drop(audits);
        let _ = self.events.send(crate::types::CoreEvent {
            kind: "device.audit".into(),
            session_id: audit.session_id.clone().unwrap_or_default(),
            payload,
        });
    }

    fn cancel_device_actions(&self, device_id: &str) {
        self.cancel_device_actions_for(device_id, None);
    }

    fn cancel_device_actions_for(
        &self,
        device_id: &str,
        capabilities: Option<&BTreeSet<DeviceCapability>>,
    ) {
        if capabilities.map_or(true, |caps| caps.contains(&DeviceCapability::ImageCapture)) {
            self.capture_confirmations
                .lock()
                .retain(|_, item| item.device_id != device_id);
        }
        for entry in self.active.lock().values() {
            if entry.device_id == device_id
                && capabilities.is_none_or(|wanted| wanted.contains(&entry.capability))
            {
                entry.cancellation.send_replace(true);
            }
        }
    }

    fn cancel_all_active(&self) {
        for entry in self.active.lock().values() {
            entry.cancellation.send_replace(true);
        }
    }
}

fn safe_error(error: DeviceError) -> DeviceError {
    let message = match error.category {
        DeviceErrorCategory::Unsupported => "适配器不支持此设备动作",
        DeviceErrorCategory::Unavailable => "设备当前不可用或离线",
        DeviceErrorCategory::Forbidden => "设备动作未获授权",
        DeviceErrorCategory::InvalidRequest => "设备请求参数无效",
        DeviceErrorCategory::NotFound => "设备或设备候选不存在",
        DeviceErrorCategory::Conflict => "设备状态与请求冲突",
        DeviceErrorCategory::LimitExceeded => "设备动作超过资源限制",
        DeviceErrorCategory::InvalidMedia => "设备返回的媒体无效",
        DeviceErrorCategory::Cancelled => "设备动作已取消",
        DeviceErrorCategory::Internal => "设备适配器执行失败",
    };
    DeviceError::new(error.category, message)
}

fn validate_candidate(candidate: &DeviceCandidate) -> Result<(), DeviceError> {
    if candidate.id.trim().is_empty()
        || candidate.id.len() > 256
        || candidate.name.trim().is_empty()
        || candidate.name.len() > 256
        || candidate.id.chars().any(char::is_control)
        || candidate.name.chars().any(char::is_control)
    {
        return Err(DeviceError::new(
            DeviceErrorCategory::InvalidRequest,
            "设备候选标识或名称无效",
        ));
    }
    validate_endpoint(candidate.transport, candidate.endpoint.as_deref())
}

fn validate_endpoint(
    transport: DeviceTransport,
    endpoint: Option<&str>,
) -> Result<(), DeviceError> {
    let network_transport = matches!(
        transport,
        DeviceTransport::WifiHttpJpeg | DeviceTransport::OnvifSnapshot
    );
    let Some(endpoint) = endpoint else {
        return if network_transport {
            Err(DeviceError::new(
                DeviceErrorCategory::InvalidRequest,
                "网络设备必须使用明确批准的 HTTP(S) endpoint",
            ))
        } else {
            Ok(())
        };
    };
    if endpoint.len() > 2048 || endpoint.chars().any(char::is_control) {
        return Err(DeviceError::new(
            DeviceErrorCategory::InvalidRequest,
            "设备 endpoint 无效或超过长度上限",
        ));
    }
    if !network_transport {
        return Ok(());
    }
    let url = reqwest::Url::parse(endpoint).map_err(|_| {
        DeviceError::new(
            DeviceErrorCategory::InvalidRequest,
            "设备 endpoint 必须是有效 URL",
        )
    })?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(DeviceError::new(
            DeviceErrorCategory::Forbidden,
            "设备 endpoint 协议或凭据字段不允许",
        ));
    }
    if url.query().is_some() {
        return Err(DeviceError::new(
            DeviceErrorCategory::Forbidden,
            "设备 endpoint 暂不允许 query 参数，避免凭据进入普通配置",
        ));
    }
    Ok(())
}

/// 用户明确批准的 Wi-Fi snapshot endpoint allowlist。
#[derive(Debug, Clone)]
pub struct WifiEndpointAllowlist {
    endpoints: BTreeSet<String>,
}

impl WifiEndpointAllowlist {
    pub fn new<I>(endpoints: I) -> Result<Self, DeviceError>
    where
        I: IntoIterator<Item = String>,
    {
        let mut allowed = BTreeSet::new();
        for endpoint in endpoints {
            let normalized = normalize_wifi_endpoint(&endpoint)?;
            allowed.insert(normalized);
        }
        Ok(Self { endpoints: allowed })
    }

    pub fn allows(&self, endpoint: &str) -> bool {
        normalize_wifi_endpoint(endpoint)
            .ok()
            .is_some_and(|normalized| self.endpoints.contains(&normalized))
    }

    pub fn endpoints(&self) -> Vec<String> {
        self.endpoints.iter().cloned().collect()
    }
}

/// Wi-Fi HTTP/JPEG 与 ONVIF snapshot 共用的受限网络适配器。
///
/// 它只接受 allowlist 中的单一 endpoint，不跟随重定向，不接受 URL 凭据，且在
/// 建立 HTTP 请求前完成 URL/host/解析地址校验。没有用户批准的 endpoint 时，
/// `discover` 不会发出任何网络请求。
pub struct WifiSnapshotAdapter {
    id: String,
    transport: DeviceTransport,
    allowlist: WifiEndpointAllowlist,
    timeout: Duration,
    max_bytes: usize,
    #[cfg(test)]
    allow_loopback_for_tests: bool,
}

impl WifiSnapshotAdapter {
    pub fn new(
        id: impl Into<String>,
        transport: DeviceTransport,
        allowlist: WifiEndpointAllowlist,
        timeout: Duration,
        max_bytes: usize,
    ) -> Result<Self, DeviceError> {
        let id = id.into();
        if !matches!(
            transport,
            DeviceTransport::WifiHttpJpeg | DeviceTransport::OnvifSnapshot
        ) {
            return Err(DeviceError::new(
                DeviceErrorCategory::InvalidRequest,
                "Wi-Fi snapshot 适配器只接受 HTTP/JPEG 或 ONVIF transport",
            ));
        }
        if id.trim().is_empty() || max_bytes == 0 || timeout.is_zero() {
            return Err(DeviceError::new(
                DeviceErrorCategory::InvalidRequest,
                "Wi-Fi snapshot 适配器参数无效",
            ));
        }
        Ok(Self {
            id,
            transport,
            allowlist,
            timeout: timeout.min(Duration::from_secs(30)),
            max_bytes: max_bytes.min(50 * 1024 * 1024),
            #[cfg(test)]
            allow_loopback_for_tests: false,
        })
    }

    #[cfg(test)]
    fn new_for_loopback_fixture(
        id: impl Into<String>,
        transport: DeviceTransport,
        allowlist: WifiEndpointAllowlist,
        timeout: Duration,
        max_bytes: usize,
    ) -> Result<Self, DeviceError> {
        let mut adapter = Self::new(id, transport, allowlist, timeout, max_bytes)?;
        adapter.allow_loopback_for_tests = true;
        Ok(adapter)
    }

    pub fn allows_endpoint(&self, endpoint: &str) -> bool {
        self.allowlist.allows(endpoint)
    }

    async fn validate_request_endpoint(
        &self,
        endpoint: &str,
    ) -> Result<(reqwest::Url, std::net::SocketAddr), DeviceError> {
        let normalized = normalize_wifi_endpoint(endpoint)?;
        if !self.allowlist.endpoints.contains(&normalized) {
            return Err(DeviceError::new(
                DeviceErrorCategory::Forbidden,
                "设备 endpoint 不在用户批准的 allowlist 中",
            ));
        }
        let url = reqwest::Url::parse(&normalized).map_err(|_| {
            DeviceError::new(
                DeviceErrorCategory::InvalidRequest,
                "设备 endpoint URL 无效",
            )
        })?;
        #[cfg(test)]
        let allow_loopback = self.allow_loopback_for_tests;
        #[cfg(not(test))]
        let allow_loopback = false;
        let resolved_address =
            tokio::time::timeout(self.timeout, validate_resolved_host(&url, allow_loopback))
                .await
                .map_err(|_| {
                    DeviceError::new(
                        DeviceErrorCategory::Unavailable,
                        "设备 endpoint DNS 校验超时",
                    )
                })??;
        Ok((url, resolved_address))
    }

    async fn fetch_snapshot(&self, endpoint: &str) -> Result<(String, Vec<u8>), DeviceError> {
        tokio::time::timeout(self.timeout, self.fetch_snapshot_inner(endpoint))
            .await
            .map_err(|_| {
                DeviceError::new(DeviceErrorCategory::Unavailable, "设备 snapshot 请求超时")
            })?
    }

    async fn fetch_snapshot_inner(&self, endpoint: &str) -> Result<(String, Vec<u8>), DeviceError> {
        let (url, resolved_address) = self.validate_request_endpoint(endpoint).await?;
        let host = url.host_str().ok_or_else(|| {
            DeviceError::new(
                DeviceErrorCategory::InvalidRequest,
                "設備 endpoint 缺少 host",
            )
        })?;
        let client = reqwest::Client::builder()
            .connect_timeout(self.timeout.min(Duration::from_secs(10)))
            .timeout(self.timeout)
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .resolve(host, resolved_address)
            .build()
            .map_err(|_| {
                DeviceError::new(DeviceErrorCategory::Internal, "无法建立设备 HTTP 客户端")
            })?;
        let response = client.get(url).send().await.map_err(|error| {
            if error.is_timeout() {
                DeviceError::new(DeviceErrorCategory::Unavailable, "设备 snapshot 请求超时")
            } else {
                DeviceError::new(DeviceErrorCategory::Unavailable, "设备 snapshot 请求失败")
            }
        })?;
        if response.status().is_redirection() {
            return Err(DeviceError::new(
                DeviceErrorCategory::Forbidden,
                "设备 snapshot 不允许重定向",
            ));
        }
        if !response.status().is_success() {
            return Err(DeviceError::new(
                DeviceErrorCategory::Unavailable,
                "设备 snapshot 返回非成功状态",
            ));
        }
        if response
            .content_length()
            .is_some_and(|length| length > self.max_bytes as u64)
        {
            return Err(DeviceError::new(
                DeviceErrorCategory::LimitExceeded,
                "设备 snapshot 超过大小限制",
            ));
        }
        let mime_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .unwrap_or("")
            .to_ascii_lowercase();
        if mime_type != "image/jpeg" && mime_type != "image/jpg" {
            return Err(DeviceError::new(
                DeviceErrorCategory::InvalidMedia,
                "设备 snapshot 不是 JPEG 图像",
            ));
        }
        let mut body = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| {
                DeviceError::new(DeviceErrorCategory::Unavailable, "设备 snapshot 读取失败")
            })?;
            if body.len().saturating_add(chunk.len()) > self.max_bytes {
                return Err(DeviceError::new(
                    DeviceErrorCategory::LimitExceeded,
                    "设备 snapshot 超过大小限制",
                ));
            }
            body.extend_from_slice(&chunk);
        }
        if !looks_like_jpeg(&body) {
            return Err(DeviceError::new(
                DeviceErrorCategory::InvalidMedia,
                "设备 snapshot 不是有效 JPEG",
            ));
        }
        Ok(("image/jpeg".into(), body))
    }

    fn candidate_for(&self, endpoint: &str) -> Result<DeviceCandidate, DeviceError> {
        let normalized = normalize_wifi_endpoint(endpoint)?;
        let hash = stable_endpoint_hash(normalized.as_bytes());
        Ok(DeviceCandidate {
            id: format!("wifi-camera-{hash:016x}"),
            name: normalized.clone(),
            transport: self.transport,
            kind: DeviceKind::Camera,
            endpoint: Some(normalized),
            capabilities: [DeviceCapability::ImageCapture].into_iter().collect(),
        })
    }
}

fn stable_endpoint_hash(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

#[async_trait]
impl DeviceAdapter for WifiSnapshotAdapter {
    fn id(&self) -> &str {
        &self.id
    }

    fn transports(&self) -> BTreeSet<DeviceTransport> {
        [self.transport].into_iter().collect()
    }

    fn capabilities(&self) -> BTreeSet<DeviceCapability> {
        [DeviceCapability::ImageCapture].into_iter().collect()
    }

    async fn discover(
        &self,
        request: &DiscoveryRequest,
    ) -> Result<Vec<DeviceCandidate>, DeviceError> {
        if request.transport != self.transport || request.scope.trim().is_empty() {
            return Err(DeviceError::new(
                DeviceErrorCategory::InvalidRequest,
                "Wi-Fi 设备发现必须指定单一 snapshot endpoint",
            ));
        }
        let candidate = self.candidate_for(request.scope.trim())?;
        let _ = self.validate_request_endpoint(request.scope.trim()).await?;
        Ok(vec![candidate])
    }

    async fn pair(&self, candidate: &DeviceCandidate) -> Result<DeviceRecord, DeviceError> {
        let endpoint = candidate.endpoint.as_deref().ok_or_else(|| {
            DeviceError::new(
                DeviceErrorCategory::InvalidRequest,
                "摄像头候选缺少 endpoint",
            )
        })?;
        let expected = self.candidate_for(endpoint)?;
        if expected.id != candidate.id || expected.transport != candidate.transport {
            return Err(DeviceError::new(
                DeviceErrorCategory::Conflict,
                "摄像头候选与批准 endpoint 不匹配",
            ));
        }
        Ok(DeviceRecord {
            id: expected.id,
            name: expected.name,
            transport: self.transport,
            kind: DeviceKind::Camera,
            endpoint: expected.endpoint,
            capabilities: expected.capabilities,
            grants: Vec::new(),
            enabled: true,
        })
    }

    async fn unpair(&self, _record: &DeviceRecord) -> Result<(), DeviceError> {
        Ok(())
    }

    async fn connect(&self, record: &DeviceRecord) -> Result<DeviceStatus, DeviceError> {
        let endpoint = record.endpoint.as_deref().ok_or_else(|| {
            DeviceError::new(DeviceErrorCategory::InvalidRequest, "摄像头缺少 endpoint")
        })?;
        let _ = self.validate_request_endpoint(endpoint).await?;
        Err(DeviceError::new(
            DeviceErrorCategory::Unsupported,
            "snapshot 摄像头没有持续连接；请按次发起采集",
        ))
    }

    async fn disconnect(&self, _record: &DeviceRecord) -> Result<DeviceStatus, DeviceError> {
        Ok(DeviceStatus::Disconnected)
    }

    async fn status(&self, record: &DeviceRecord) -> Result<DeviceStatus, DeviceError> {
        let endpoint = record.endpoint.as_deref().ok_or_else(|| {
            DeviceError::new(DeviceErrorCategory::InvalidRequest, "摄像头缺少 endpoint")
        })?;
        let _ = endpoint;
        Ok(DeviceStatus::Unknown)
    }

    async fn camera_capture(
        &self,
        ctx: &DeviceRequestContext,
        record: &DeviceRecord,
    ) -> Result<DeviceCapture, DeviceError> {
        let endpoint = record.endpoint.as_deref().ok_or_else(|| {
            DeviceError::new(DeviceErrorCategory::InvalidRequest, "摄像头缺少 endpoint")
        })?;
        let (mime_type, bytes) = self.fetch_snapshot(endpoint).await?;
        Ok(DeviceCapture {
            receipt: DeviceActionReceipt {
                request_id: ctx.request_id.clone(),
                device_id: record.id.clone(),
                status: DeviceStatus::Connected,
                summary: "摄像头已确认采集单帧图像".into(),
                bytes: Some(bytes.len()),
            },
            mime_type,
            bytes,
        })
    }
}

fn normalize_wifi_endpoint(endpoint: &str) -> Result<String, DeviceError> {
    validate_endpoint(DeviceTransport::WifiHttpJpeg, Some(endpoint))?;
    let mut url = reqwest::Url::parse(endpoint).map_err(|_| {
        DeviceError::new(
            DeviceErrorCategory::InvalidRequest,
            "设备 endpoint URL 无效",
        )
    })?;
    url.set_fragment(None);
    Ok(url.to_string())
}

async fn validate_resolved_host(
    url: &reqwest::Url,
    allow_loopback_for_tests: bool,
) -> Result<std::net::SocketAddr, DeviceError> {
    let host = url.host_str().ok_or_else(|| {
        DeviceError::new(
            DeviceErrorCategory::InvalidRequest,
            "设备 endpoint 缺少 host",
        )
    })?;
    let port = url.port_or_known_default().ok_or_else(|| {
        DeviceError::new(
            DeviceErrorCategory::InvalidRequest,
            "设备 endpoint 缺少端口",
        )
    })?;
    let addresses = tokio::net::lookup_host((host, port)).await.map_err(|_| {
        DeviceError::new(DeviceErrorCategory::Unavailable, "设备 endpoint 无法解析")
    })?;
    let mut selected = None;
    for address in addresses {
        if !is_private_lan_ip(address.ip(), allow_loopback_for_tests) {
            return Err(DeviceError::new(
                DeviceErrorCategory::Forbidden,
                "设备 endpoint 必须解析到用户所在的私有局域网",
            ));
        }
        selected.get_or_insert(address);
    }
    selected.ok_or_else(|| {
        DeviceError::new(
            DeviceErrorCategory::Unavailable,
            "设备 endpoint 没有可用地址",
        )
    })
}

fn is_private_lan_ip(ip: IpAddr, allow_loopback_for_tests: bool) -> bool {
    match ip {
        IpAddr::V4(ip) => ip.is_private() || (allow_loopback_for_tests && ip.is_loopback()),
        IpAddr::V6(ip) => {
            ip.segments()[0] & 0xfe00 == 0xfc00 || (allow_loopback_for_tests && ip.is_loopback())
        }
    }
}

fn looks_like_jpeg(bytes: &[u8]) -> bool {
    bytes.len() >= 4
        && bytes[0] == 0xff
        && bytes[1] == 0xd8
        && bytes[bytes.len() - 2..] == [0xff, 0xd9]
}

/// 宿主蓝牙后端的最小能力面。Core 不直接依赖 BlueZ、CoreBluetooth 或厂商 SDK。
#[async_trait]
pub trait BluetoothBackend: Send + Sync {
    fn id(&self) -> &str;

    fn capabilities(&self) -> BTreeSet<DeviceCapability> {
        [DeviceCapability::AudioOutput].into_iter().collect()
    }

    async fn discover(
        &self,
        request: &DiscoveryRequest,
    ) -> Result<Vec<DeviceCandidate>, DeviceError>;

    async fn pair(&self, candidate: &DeviceCandidate) -> Result<DeviceRecord, DeviceError>;

    async fn unpair(&self, record: &DeviceRecord) -> Result<(), DeviceError>;

    async fn connect(&self, record: &DeviceRecord) -> Result<DeviceStatus, DeviceError>;

    async fn disconnect(&self, record: &DeviceRecord) -> Result<DeviceStatus, DeviceError>;

    async fn status(&self, record: &DeviceRecord) -> Result<DeviceStatus, DeviceError>;

    async fn audio_play(
        &self,
        ctx: &DeviceRequestContext,
        record: &DeviceRecord,
        audio: &[u8],
    ) -> Result<DeviceActionReceipt, DeviceError>;

    async fn cancel(&self, request_id: &str) -> Result<(), DeviceError>;
}

/// 蓝牙音频适配器入口。
///
/// `host_default` 有意不伪造后端：当前进程未装配宿主蓝牙桥接时，发现明确返回
/// `unsupported`。测试或平台集成可通过 `with_backend` 注入真实/模拟后端。
pub struct BluetoothAdapter {
    backend: Option<Arc<dyn BluetoothBackend>>,
}

impl BluetoothAdapter {
    pub fn host_default() -> Self {
        Self { backend: None }
    }

    pub fn with_backend(backend: Arc<dyn BluetoothBackend>) -> Self {
        Self {
            backend: Some(backend),
        }
    }

    pub fn backend_available(&self) -> bool {
        self.backend.is_some()
    }

    pub fn probe_host(&self) -> BluetoothHostCapabilities {
        let backend = self.backend.as_ref();
        BluetoothHostCapabilities {
            backend_available: backend.is_some(),
            backend_id: backend.map(|backend| backend.id().to_string()),
            transports: if backend.is_some() {
                [DeviceTransport::Bluetooth].into_iter().collect()
            } else {
                BTreeSet::new()
            },
            capabilities: backend
                .map(|backend| backend.capabilities())
                .unwrap_or_default(),
            status: if backend.is_some() {
                DeviceStatus::Unknown
            } else {
                DeviceStatus::Unsupported
            },
        }
    }

    fn unavailable() -> DeviceError {
        DeviceError::new(
            DeviceErrorCategory::Unsupported,
            "当前宿主未装配蓝牙音频后端",
        )
    }
}

#[async_trait]
impl DeviceAdapter for BluetoothAdapter {
    fn id(&self) -> &str {
        "bluetooth-audio"
    }

    fn transports(&self) -> BTreeSet<DeviceTransport> {
        [DeviceTransport::Bluetooth].into_iter().collect()
    }

    fn capabilities(&self) -> BTreeSet<DeviceCapability> {
        self.backend
            .as_ref()
            .map(|backend| backend.capabilities())
            .unwrap_or_default()
    }

    fn probe(&self) -> DeviceAdapterProbe {
        let host = self.probe_host();
        DeviceAdapterProbe {
            id: self.id().to_string(),
            api_version: self.api_version(),
            available: host.backend_available,
            transports: host.transports,
            capabilities: host.capabilities,
            status: host.status,
        }
    }

    async fn discover(
        &self,
        request: &DiscoveryRequest,
    ) -> Result<Vec<DeviceCandidate>, DeviceError> {
        if request.transport != DeviceTransport::Bluetooth
            || request.scope.trim().is_empty()
            || request.scope.trim().len() > 128
            || request.duration_secs == 0
            || request.max_results == 0
        {
            return Err(DeviceError::new(
                DeviceErrorCategory::InvalidRequest,
                "蓝牙发现必须指定本机适配器、正时长和正结果上限",
            ));
        }
        let Some(backend) = &self.backend else {
            return Err(Self::unavailable());
        };
        if request.scope.trim() != backend.id() {
            return Ok(Vec::new());
        }
        let bounded = DiscoveryRequest {
            duration_secs: request.duration_secs.min(300),
            max_results: request.max_results.min(256),
            ..request.clone()
        };
        let candidates = tokio::time::timeout(
            Duration::from_secs(bounded.duration_secs),
            backend.discover(&bounded),
        )
        .await
        .map_err(|_| DeviceError::new(DeviceErrorCategory::Unavailable, "蓝牙发现超过时限"))??;
        Ok(candidates
            .into_iter()
            .filter(|candidate| candidate.transport == DeviceTransport::Bluetooth)
            .take(bounded.max_results)
            .collect())
    }

    async fn pair(&self, candidate: &DeviceCandidate) -> Result<DeviceRecord, DeviceError> {
        self.backend
            .as_ref()
            .ok_or_else(Self::unavailable)?
            .pair(candidate)
            .await
    }

    async fn unpair(&self, record: &DeviceRecord) -> Result<(), DeviceError> {
        self.backend
            .as_ref()
            .ok_or_else(Self::unavailable)?
            .unpair(record)
            .await
    }

    async fn connect(&self, record: &DeviceRecord) -> Result<DeviceStatus, DeviceError> {
        self.backend
            .as_ref()
            .ok_or_else(Self::unavailable)?
            .connect(record)
            .await
    }

    async fn disconnect(&self, record: &DeviceRecord) -> Result<DeviceStatus, DeviceError> {
        self.backend
            .as_ref()
            .ok_or_else(Self::unavailable)?
            .disconnect(record)
            .await
    }

    async fn status(&self, record: &DeviceRecord) -> Result<DeviceStatus, DeviceError> {
        self.backend
            .as_ref()
            .ok_or_else(Self::unavailable)?
            .status(record)
            .await
    }

    async fn audio_play(
        &self,
        ctx: &DeviceRequestContext,
        record: &DeviceRecord,
        audio: &[u8],
    ) -> Result<DeviceActionReceipt, DeviceError> {
        self.backend
            .as_ref()
            .ok_or_else(Self::unavailable)?
            .audio_play(ctx, record, audio)
            .await
    }

    async fn cancel(&self, request_id: &str) -> Result<(), DeviceError> {
        self.backend
            .as_ref()
            .ok_or_else(Self::unavailable)?
            .cancel(request_id)
            .await
    }
}

/// 仅供测试和无硬件验收使用的适配器；它永远不会扫描宿主或访问网络。
pub struct SimulatedAdapter {
    id: String,
    transport: DeviceTransport,
    candidates: RwLock<BTreeMap<String, DeviceCandidate>>,
    online: RwLock<HashSet<String>>,
    images: RwLock<HashMap<String, (String, Vec<u8>)>>,
    played: Mutex<Vec<(String, usize)>>,
}

impl SimulatedAdapter {
    pub fn new(id: impl Into<String>, transport: DeviceTransport) -> Self {
        Self {
            id: id.into(),
            transport,
            candidates: RwLock::new(BTreeMap::new()),
            online: RwLock::new(HashSet::new()),
            images: RwLock::new(HashMap::new()),
            played: Mutex::new(Vec::new()),
        }
    }

    pub fn add_candidate(&self, candidate: DeviceCandidate) {
        self.candidates
            .write()
            .insert(candidate.id.clone(), candidate);
    }

    pub fn set_online(&self, device_id: &str, online: bool) {
        if online {
            self.online.write().insert(device_id.to_string());
        } else {
            self.online.write().remove(device_id);
        }
    }

    pub fn set_image(&self, device_id: &str, mime_type: impl Into<String>, bytes: Vec<u8>) {
        self.images
            .write()
            .insert(device_id.to_string(), (mime_type.into(), bytes));
    }

    pub fn played(&self) -> Vec<(String, usize)> {
        self.played.lock().clone()
    }

    fn supports(&self, capability: DeviceCapability) -> bool {
        match self.transport {
            DeviceTransport::Bluetooth | DeviceTransport::SystemAudio => {
                capability == DeviceCapability::AudioOutput
            }
            DeviceTransport::WifiHttpJpeg | DeviceTransport::OnvifSnapshot => {
                capability == DeviceCapability::ImageCapture
            }
        }
    }

    fn unavailable(&self, device_id: &str) -> Result<(), DeviceError> {
        if self.online.read().contains(device_id) {
            Ok(())
        } else {
            Err(DeviceError::new(
                DeviceErrorCategory::Unavailable,
                "模拟设备当前离线",
            ))
        }
    }
}

#[async_trait]
impl DeviceAdapter for SimulatedAdapter {
    fn id(&self) -> &str {
        &self.id
    }

    fn transports(&self) -> BTreeSet<DeviceTransport> {
        [self.transport].into_iter().collect()
    }

    fn capabilities(&self) -> BTreeSet<DeviceCapability> {
        [
            DeviceCapability::AudioOutput,
            DeviceCapability::ImageCapture,
        ]
        .into_iter()
        .filter(|capability| self.supports(*capability))
        .collect()
    }

    async fn discover(
        &self,
        request: &DiscoveryRequest,
    ) -> Result<Vec<DeviceCandidate>, DeviceError> {
        if request.transport != self.transport {
            return Ok(Vec::new());
        }
        if self.transport == DeviceTransport::Bluetooth
            && !request.scope.trim().is_empty()
            && request.scope.trim() != self.id
        {
            return Ok(Vec::new());
        }
        Ok(self
            .candidates
            .read()
            .values()
            .filter(|candidate| {
                self.transport == DeviceTransport::Bluetooth
                    || request.scope.trim().is_empty()
                    || candidate.id.contains(request.scope.trim())
                    || candidate.name.contains(request.scope.trim())
                    || candidate
                        .endpoint
                        .as_deref()
                        .is_some_and(|endpoint| endpoint.starts_with(request.scope.trim()))
            })
            .take(request.max_results)
            .cloned()
            .collect())
    }

    async fn pair(&self, candidate: &DeviceCandidate) -> Result<DeviceRecord, DeviceError> {
        if candidate.transport != self.transport {
            return Err(DeviceError::new(
                DeviceErrorCategory::Unsupported,
                "模拟适配器不支持该传输类型",
            ));
        }
        Ok(DeviceRecord {
            id: candidate.id.clone(),
            name: candidate.name.clone(),
            transport: candidate.transport,
            kind: candidate.kind,
            endpoint: candidate.endpoint.clone(),
            capabilities: candidate.capabilities.clone(),
            grants: Vec::new(),
            enabled: true,
        })
    }

    async fn unpair(&self, record: &DeviceRecord) -> Result<(), DeviceError> {
        self.online.write().remove(&record.id);
        Ok(())
    }

    async fn connect(&self, record: &DeviceRecord) -> Result<DeviceStatus, DeviceError> {
        self.unavailable(&record.id)?;
        Ok(DeviceStatus::Connected)
    }

    async fn disconnect(&self, record: &DeviceRecord) -> Result<DeviceStatus, DeviceError> {
        self.online.write().remove(&record.id);
        Ok(DeviceStatus::Disconnected)
    }

    async fn status(&self, record: &DeviceRecord) -> Result<DeviceStatus, DeviceError> {
        if self.online.read().contains(&record.id) {
            Ok(DeviceStatus::Connected)
        } else {
            Ok(DeviceStatus::Disconnected)
        }
    }

    async fn audio_play(
        &self,
        ctx: &DeviceRequestContext,
        record: &DeviceRecord,
        audio: &[u8],
    ) -> Result<DeviceActionReceipt, DeviceError> {
        if !self.supports(DeviceCapability::AudioOutput) {
            return Err(DeviceError::new(
                DeviceErrorCategory::Unsupported,
                "模拟设备未声明音频输出",
            ));
        }
        self.unavailable(&record.id)?;
        self.played.lock().push((record.id.clone(), audio.len()));
        Ok(DeviceActionReceipt {
            request_id: ctx.request_id.clone(),
            device_id: record.id.clone(),
            status: DeviceStatus::Connected,
            summary: "模拟音频已由设备确认播放".into(),
            bytes: Some(audio.len()),
        })
    }

    async fn camera_capture(
        &self,
        ctx: &DeviceRequestContext,
        record: &DeviceRecord,
    ) -> Result<DeviceCapture, DeviceError> {
        if !self.supports(DeviceCapability::ImageCapture) {
            return Err(DeviceError::new(
                DeviceErrorCategory::Unsupported,
                "模拟设备未声明图像采集",
            ));
        }
        self.unavailable(&record.id)?;
        let (mime_type, bytes) = self
            .images
            .read()
            .get(&record.id)
            .cloned()
            .unwrap_or_else(|| ("image/jpeg".into(), vec![0xff, 0xd8, 0xff, 0xd9]));
        Ok(DeviceCapture {
            receipt: DeviceActionReceipt {
                request_id: ctx.request_id.clone(),
                device_id: record.id.clone(),
                status: DeviceStatus::Connected,
                summary: "模拟图像已由设备确认采集".into(),
                bytes: Some(bytes.len()),
            },
            mime_type,
            bytes,
        })
    }
}

/// 将内存 fixture 暴露为宿主蓝牙后端，用于验证平台后端注入路径。
/// 它仍只访问内存状态，不扫描宿主蓝牙或网络。
#[async_trait]
impl BluetoothBackend for SimulatedAdapter {
    fn id(&self) -> &str {
        &self.id
    }

    async fn discover(
        &self,
        request: &DiscoveryRequest,
    ) -> Result<Vec<DeviceCandidate>, DeviceError> {
        if self.transport != DeviceTransport::Bluetooth {
            return Err(DeviceError::new(
                DeviceErrorCategory::Unsupported,
                "模拟蓝牙后端未配置 Bluetooth transport",
            ));
        }
        <Self as DeviceAdapter>::discover(self, request).await
    }

    async fn pair(&self, candidate: &DeviceCandidate) -> Result<DeviceRecord, DeviceError> {
        <Self as DeviceAdapter>::pair(self, candidate).await
    }

    async fn unpair(&self, record: &DeviceRecord) -> Result<(), DeviceError> {
        <Self as DeviceAdapter>::unpair(self, record).await
    }

    async fn connect(&self, record: &DeviceRecord) -> Result<DeviceStatus, DeviceError> {
        <Self as DeviceAdapter>::connect(self, record).await
    }

    async fn disconnect(&self, record: &DeviceRecord) -> Result<DeviceStatus, DeviceError> {
        <Self as DeviceAdapter>::disconnect(self, record).await
    }

    async fn status(&self, record: &DeviceRecord) -> Result<DeviceStatus, DeviceError> {
        <Self as DeviceAdapter>::status(self, record).await
    }

    async fn audio_play(
        &self,
        ctx: &DeviceRequestContext,
        record: &DeviceRecord,
        audio: &[u8],
    ) -> Result<DeviceActionReceipt, DeviceError> {
        <Self as DeviceAdapter>::audio_play(self, ctx, record, audio).await
    }

    async fn cancel(&self, _request_id: &str) -> Result<(), DeviceError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{LinkTarget, NexusConfig};

    #[test]
    fn physical_device_model_round_trips_without_secrets() {
        let record = DeviceRecord {
            id: "camera-1".into(),
            name: "会议室摄像头".into(),
            transport: DeviceTransport::WifiHttpJpeg,
            kind: DeviceKind::Camera,
            endpoint: Some("http://192.168.1.20/snapshot.jpg".into()),
            capabilities: [DeviceCapability::ImageCapture].into_iter().collect(),
            grants: vec![DeviceGrant {
                id: "grant-1".into(),
                device_id: "camera-1".into(),
                capabilities: [DeviceCapability::ImageCapture].into_iter().collect(),
                agent_id: None,
                session_id: None,
                granted_at: "2026-11-21T00:00:00Z".into(),
                revoked_at: None,
            }],
            enabled: true,
        };

        let encoded = serde_json::to_value(&record).unwrap();
        let decoded: DeviceRecord = serde_json::from_value(encoded.clone()).unwrap();
        assert_eq!(decoded, record);
        assert!(encoded.get("credentials").is_none());
        assert_eq!(encoded["transport"], "wifi_http_jpeg");
        assert_eq!(encoded["grants"][0]["capabilities"][0], "image_capture");
    }

    #[test]
    fn legacy_nexus_link_configuration_round_trips_unchanged() {
        let legacy = serde_json::json!({
            "links": [{
                "id": "remote-agent",
                "name": "远端代理",
                "kind": "exmachina",
                "endpoint": "https://agent.example.invalid",
                "apiKey": "fixture-only-key",
                "command": "",
                "enabled": true
            }]
        });

        let nexus: NexusConfig = serde_json::from_value(legacy.clone()).unwrap();
        assert_eq!(nexus.links.len(), 1);
        assert_eq!(nexus.links[0].kind, "exmachina");
        assert_eq!(nexus.links[0].api_key, "fixture-only-key");
        assert_eq!(serde_json::to_value(nexus).unwrap(), legacy);

        // A physical device remains a separate type and cannot be parsed as a remote link.
        let physical = DeviceRecord {
            id: "speaker-1".into(),
            name: "客厅音响".into(),
            transport: DeviceTransport::Bluetooth,
            kind: DeviceKind::Speaker,
            endpoint: None,
            capabilities: [DeviceCapability::AudioOutput].into_iter().collect(),
            grants: Vec::new(),
            enabled: true,
        };
        let link = LinkTarget {
            id: physical.id,
            name: physical.name,
            ..LinkTarget::default()
        };
        assert_eq!(link.kind, "exmachina");
        assert!(link.endpoint.is_empty());
    }

    #[test]
    fn device_grant_revocation_and_errors_have_stable_wire_values() {
        let grant: DeviceGrant = serde_json::from_value(serde_json::json!({
            "id": "grant-1",
            "deviceId": "camera-1",
            "capabilities": ["image_capture"],
            "grantedAt": "2026-11-21T00:00:00Z"
        }))
        .unwrap();
        assert!(grant.allows(DeviceCapability::ImageCapture));
        assert!(!grant.allows_for("agent-1", "session-1", DeviceCapability::ImageCapture));
        let mut revoked = grant.clone();
        revoked.revoked_at = Some("2026-11-21T00:01:00Z".into());
        assert!(!revoked.allows(DeviceCapability::ImageCapture));

        let error = DeviceError::new(DeviceErrorCategory::Unsupported, "当前平台未装配蓝牙后端");
        let wire = serde_json::to_value(&error).unwrap();
        assert_eq!(wire["category"], "unsupported");
        assert_eq!(error.to_string(), "unsupported: 当前平台未装配蓝牙后端");
    }

    fn enabled_config() -> crate::config::DeviceConnectivityConfig {
        crate::config::DeviceConnectivityConfig {
            enabled: true,
            ..Default::default()
        }
    }

    fn speaker_candidate(id: &str) -> DeviceCandidate {
        DeviceCandidate {
            id: id.into(),
            name: "模拟音响".into(),
            transport: DeviceTransport::Bluetooth,
            kind: DeviceKind::Speaker,
            endpoint: None,
            capabilities: [DeviceCapability::AudioOutput].into_iter().collect(),
        }
    }

    #[tokio::test]
    async fn service_discovery_deduplicates_candidates_and_requires_confirmation() {
        let (events, _) = broadcast::channel(32);
        let service = DeviceService::new(enabled_config(), Vec::new(), events).unwrap();
        let first = Arc::new(SimulatedAdapter::new(
            "fixture-a",
            DeviceTransport::Bluetooth,
        ));
        let second = Arc::new(SimulatedAdapter::new(
            "fixture-b",
            DeviceTransport::Bluetooth,
        ));
        first.add_candidate(speaker_candidate("speaker-1"));
        second.add_candidate(speaker_candidate("speaker-1"));
        service.register_adapter(first).unwrap();
        service.register_adapter(second).unwrap();

        let discover_ctx = DeviceRequestContext::new("test-user", Some("session-1".into()));
        let found = service
            .discover(
                &discover_ctx,
                DiscoveryRequest {
                    transport: DeviceTransport::Bluetooth,
                    scope: String::new(),
                    duration_secs: 1,
                    max_results: 10,
                },
            )
            .await
            .unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(service.candidates().len(), 1);

        let denied = service
            .pair(
                &DeviceRequestContext::new("test-user", Some("session-1".into())),
                "speaker-1",
                "agent-1",
                Some("session-1"),
                [DeviceCapability::AudioOutput].into_iter().collect(),
                false,
            )
            .await
            .unwrap_err();
        assert_eq!(denied.category, DeviceErrorCategory::Forbidden);
        assert!(service.records().is_empty());
    }

    #[tokio::test]
    async fn service_pair_connect_revoke_cancels_action_and_keeps_audit_safe() {
        let (events, _) = broadcast::channel(64);
        let service = DeviceService::new(enabled_config(), Vec::new(), events).unwrap();
        let adapter = Arc::new(SimulatedAdapter::new("fixture", DeviceTransport::Bluetooth));
        adapter.add_candidate(speaker_candidate("speaker-1"));
        adapter.set_online("speaker-1", true);
        service.register_adapter(adapter).unwrap();

        let discover_ctx = DeviceRequestContext::new("test-user", Some("session-1".into()));
        service
            .discover(
                &discover_ctx,
                DiscoveryRequest {
                    transport: DeviceTransport::Bluetooth,
                    scope: String::new(),
                    duration_secs: 1,
                    max_results: 1,
                },
            )
            .await
            .unwrap();
        let pair_ctx = DeviceRequestContext::new("test-user", Some("session-1".into()));
        let paired = service
            .pair(
                &pair_ctx,
                "speaker-1",
                "agent-1",
                Some("session-1"),
                [DeviceCapability::AudioOutput].into_iter().collect(),
                true,
            )
            .await
            .unwrap();
        assert_eq!(paired.runtime.status, DeviceStatus::Disconnected);

        let connect_ctx = DeviceRequestContext::new("test-user", Some("session-1".into()));
        let connected = service.connect(&connect_ctx, "speaker-1").await.unwrap();
        assert_eq!(connected.runtime.status, DeviceStatus::Connected);

        let action_ctx = DeviceRequestContext::new("agent-1", Some("session-1".into()));
        let lease = service
            .begin_action(&action_ctx, "speaker-1", DeviceCapability::AudioOutput)
            .unwrap();
        let revoke_ctx = DeviceRequestContext::new("test-user", Some("session-1".into()));
        let revoked = service
            .revoke_authorization(&revoke_ctx, "speaker-1", None)
            .unwrap();
        assert!(revoked.record.grants.iter().all(|grant| !grant.is_active()));
        assert!(lease.is_cancelled());
        assert_eq!(
            service.finish_action(&action_ctx, &lease, Ok(())),
            Err(DeviceError::new(
                DeviceErrorCategory::Cancelled,
                "设备动作已安全停止"
            ))
        );

        let audit_json = serde_json::to_string(&service.audits()).unwrap();
        assert!(audit_json.contains(&action_ctx.request_id));
        assert!(!audit_json.contains("fixture-secret"));
        assert!(!audit_json.contains("media-body"));
        service.unpair(&revoke_ctx, "speaker-1").await.unwrap();
        assert!(service.records().is_empty());
        assert!(service
            .audits()
            .iter()
            .any(|event| event.action == "unpair"));
    }

    #[tokio::test]
    async fn service_reports_offline_without_faking_connection() {
        let (events, _) = broadcast::channel(32);
        let service = DeviceService::new(enabled_config(), Vec::new(), events).unwrap();
        let adapter = Arc::new(SimulatedAdapter::new("fixture", DeviceTransport::Bluetooth));
        adapter.add_candidate(speaker_candidate("speaker-1"));
        service.register_adapter(adapter.clone()).unwrap();
        let ctx = DeviceRequestContext::new("test-user", None);
        service
            .discover(
                &ctx,
                DiscoveryRequest {
                    transport: DeviceTransport::Bluetooth,
                    scope: String::new(),
                    duration_secs: 1,
                    max_results: 1,
                },
            )
            .await
            .unwrap();
        service
            .pair(
                &ctx,
                "speaker-1",
                "agent-1",
                None,
                [DeviceCapability::AudioOutput].into_iter().collect(),
                true,
            )
            .await
            .unwrap();
        let error = service.connect(&ctx, "speaker-1").await.unwrap_err();
        assert_eq!(error.category, DeviceErrorCategory::Unavailable);
        let snapshot = service.status(&ctx, "speaker-1").await.unwrap();
        assert_eq!(snapshot.runtime.status, DeviceStatus::Disconnected);
    }

    #[tokio::test]
    async fn disabled_service_does_not_call_adapter_or_expose_candidates() {
        let (events, _) = broadcast::channel(32);
        let service = DeviceService::new(Default::default(), Vec::new(), events).unwrap();
        let adapter = Arc::new(SimulatedAdapter::new("fixture", DeviceTransport::Bluetooth));
        adapter.add_candidate(speaker_candidate("speaker-1"));
        service.register_adapter(adapter).unwrap();
        let error = service
            .discover(
                &DeviceRequestContext::new("test-user", None),
                DiscoveryRequest {
                    transport: DeviceTransport::Bluetooth,
                    scope: String::new(),
                    duration_secs: 1,
                    max_results: 1,
                },
            )
            .await
            .unwrap_err();
        assert_eq!(error.category, DeviceErrorCategory::Forbidden);
        assert!(service.candidates().is_empty());
        assert!(service.records().is_empty());
    }

    #[tokio::test]
    async fn bluetooth_host_without_backend_reports_unsupported_without_fabricating_results() {
        let adapter = BluetoothAdapter::host_default();
        assert!(!adapter.backend_available());
        let probe = adapter.probe_host();
        assert!(!probe.backend_available);
        assert_eq!(probe.status, DeviceStatus::Unsupported);
        assert!(probe.transports.is_empty());
        let error = adapter
            .discover(&DiscoveryRequest {
                transport: DeviceTransport::Bluetooth,
                scope: "local-adapter-1".into(),
                duration_secs: 1,
                max_results: 1,
            })
            .await
            .unwrap_err();
        assert_eq!(error.category, DeviceErrorCategory::Unsupported);
    }

    #[tokio::test]
    async fn bluetooth_adapter_uses_injected_backend_and_preserves_user_scope() {
        let (events, _) = broadcast::channel(32);
        let service = DeviceService::new(enabled_config(), Vec::new(), events).unwrap();
        let backend = Arc::new(SimulatedAdapter::new(
            "bluez-fixture",
            DeviceTransport::Bluetooth,
        ));
        backend.add_candidate(speaker_candidate("speaker-1"));
        backend.add_candidate(speaker_candidate("other-speaker"));
        let adapter = Arc::new(BluetoothAdapter::with_backend(backend.clone()));
        service.register_adapter(adapter).unwrap();

        let found = service
            .discover(
                &DeviceRequestContext::new("test-user", None),
                DiscoveryRequest {
                    transport: DeviceTransport::Bluetooth,
                    scope: "bluez-fixture".into(),
                    duration_secs: 1,
                    max_results: 8,
                },
            )
            .await
            .unwrap();
        assert_eq!(found.len(), 2);
        assert!(found.iter().any(|candidate| candidate.id == "speaker-1"));
        assert!(found
            .iter()
            .any(|candidate| candidate.id == "other-speaker"));
        assert_eq!(service.candidates().len(), 2);

        backend.set_online("speaker-1", true);
        let paired = service
            .pair(
                &DeviceRequestContext::new("test-user", None),
                "speaker-1",
                "agent-1",
                None,
                [DeviceCapability::AudioOutput].into_iter().collect(),
                true,
            )
            .await
            .unwrap();
        assert_eq!(paired.record.transport, DeviceTransport::Bluetooth);
        let connected = service
            .connect(&DeviceRequestContext::new("test-user", None), "speaker-1")
            .await
            .unwrap();
        assert_eq!(connected.runtime.status, DeviceStatus::Connected);
    }

    #[tokio::test]
    async fn enabled_service_with_host_bluetooth_adapter_reports_unsupported() {
        let (events, _) = broadcast::channel(32);
        let service = DeviceService::new(enabled_config(), Vec::new(), events).unwrap();
        service
            .register_adapter(Arc::new(BluetoothAdapter::host_default()))
            .unwrap();
        let error = service
            .discover(
                &DeviceRequestContext::new("test-user", None),
                DiscoveryRequest {
                    transport: DeviceTransport::Bluetooth,
                    scope: "adapter-0".into(),
                    duration_secs: 1,
                    max_results: 1,
                },
            )
            .await
            .unwrap_err();
        assert_eq!(error.category, DeviceErrorCategory::Unsupported);
        assert!(service.candidates().is_empty());
    }

    #[tokio::test]
    async fn adapter_default_action_rejects_capability_not_declared_by_fixture() {
        let adapter = SimulatedAdapter::new("audio-only", DeviceTransport::Bluetooth);
        let record = DeviceRecord {
            id: "speaker-1".into(),
            name: "模拟音响".into(),
            transport: DeviceTransport::Bluetooth,
            kind: DeviceKind::Speaker,
            endpoint: None,
            capabilities: [DeviceCapability::AudioOutput].into_iter().collect(),
            grants: Vec::new(),
            enabled: true,
        };
        let error = adapter
            .camera_capture(&DeviceRequestContext::new("test", None), &record)
            .await
            .unwrap_err();
        assert_eq!(error.category, DeviceErrorCategory::Unsupported);
    }

    #[tokio::test]
    async fn wifi_snapshot_blocks_loopback_before_sending_request() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let endpoint = format!("http://127.0.0.1:{port}/snapshot.jpg");
        let allowlist = WifiEndpointAllowlist::new(vec![endpoint.clone()]).unwrap();
        let adapter = WifiSnapshotAdapter::new(
            "wifi-fixture",
            DeviceTransport::WifiHttpJpeg,
            allowlist,
            Duration::from_millis(100),
            1024,
        )
        .unwrap();
        let error = adapter
            .discover(&DiscoveryRequest {
                transport: DeviceTransport::WifiHttpJpeg,
                scope: endpoint,
                duration_secs: 1,
                max_results: 1,
            })
            .await
            .unwrap_err();
        assert_eq!(error.category, DeviceErrorCategory::Forbidden);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), listener.accept())
                .await
                .is_err(),
            "SSRF/loopback 拦截后不应建立 HTTP 请求"
        );
    }

    #[tokio::test]
    async fn wifi_discovery_and_status_never_fetch_a_snapshot() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/snapshot.jpg", listener.local_addr().unwrap());
        let adapter = WifiSnapshotAdapter::new_for_loopback_fixture(
            "wifi-fixture",
            DeviceTransport::WifiHttpJpeg,
            WifiEndpointAllowlist::new(vec![endpoint.clone()]).unwrap(),
            Duration::from_millis(100),
            1024,
        )
        .unwrap();
        let candidates = adapter
            .discover(&DiscoveryRequest {
                transport: DeviceTransport::WifiHttpJpeg,
                scope: endpoint.clone(),
                duration_secs: 1,
                max_results: 1,
            })
            .await
            .unwrap();
        assert_eq!(candidates.len(), 1);
        let record = adapter.pair(&candidates[0]).await.unwrap();
        assert_eq!(
            adapter.status(&record).await.unwrap(),
            DeviceStatus::Unknown
        );
        assert_eq!(
            adapter.connect(&record).await.unwrap_err().category,
            DeviceErrorCategory::Unsupported
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(50), listener.accept())
                .await
                .is_err(),
            "发现、配对、状态和连接验证不能请求 snapshot 图像"
        );
    }

    async fn start_http_fixture(
        response: Option<Vec<u8>>,
    ) -> (String, tokio::task::JoinHandle<()>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/snapshot.jpg", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 1024];
            let _ = socket.read(&mut request).await;
            if let Some(response) = response {
                let _ = socket.write_all(&response).await;
            } else {
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        });
        (endpoint, task)
    }

    fn camera_record(endpoint: &str) -> DeviceRecord {
        DeviceRecord {
            id: "camera-fixture".into(),
            name: "测试摄像头".into(),
            transport: DeviceTransport::WifiHttpJpeg,
            kind: DeviceKind::Camera,
            endpoint: Some(endpoint.into()),
            capabilities: [DeviceCapability::ImageCapture].into_iter().collect(),
            grants: Vec::new(),
            enabled: true,
        }
    }

    #[tokio::test]
    async fn wifi_snapshot_fixture_enforces_redirect_size_and_timeout_limits() {
        let jpeg = vec![0xff, 0xd8, 0xff, 0xd9];
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            jpeg.len()
        )
        .into_bytes();
        let mut response_with_body = response;
        response_with_body.extend_from_slice(&jpeg);
        let (endpoint, server) = start_http_fixture(Some(response_with_body)).await;
        let allowlist = WifiEndpointAllowlist::new(vec![endpoint.clone()]).unwrap();
        let adapter = WifiSnapshotAdapter::new_for_loopback_fixture(
            "wifi-fixture",
            DeviceTransport::WifiHttpJpeg,
            allowlist,
            Duration::from_millis(200),
            1024,
        )
        .unwrap();
        let capture = adapter
            .camera_capture(
                &DeviceRequestContext::new("test-user", Some("session-1".into())),
                &camera_record(&endpoint),
            )
            .await
            .unwrap();
        assert_eq!(capture.bytes, jpeg);
        server.await.unwrap();

        let oversized_body = vec![0xff, 0xd8, 0x00, 0x00, 0xff, 0xd9];
        let oversized_response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            oversized_body.len()
        )
        .into_bytes();
        let mut oversized_response_with_body = oversized_response;
        oversized_response_with_body.extend_from_slice(&oversized_body);
        let (oversized_endpoint, oversized_server) =
            start_http_fixture(Some(oversized_response_with_body)).await;
        let oversized_adapter = WifiSnapshotAdapter::new_for_loopback_fixture(
            "wifi-fixture-oversized",
            DeviceTransport::WifiHttpJpeg,
            WifiEndpointAllowlist::new(vec![oversized_endpoint.clone()]).unwrap(),
            Duration::from_millis(200),
            4,
        )
        .unwrap();
        let error = oversized_adapter
            .camera_capture(
                &DeviceRequestContext::new("test-user", None),
                &camera_record(&oversized_endpoint),
            )
            .await
            .unwrap_err();
        assert_eq!(error.category, DeviceErrorCategory::LimitExceeded);
        oversized_server.await.unwrap();

        let redirect_target = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let redirect_target_port = redirect_target.local_addr().unwrap().port();
        let redirect = format!(
            "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:{redirect_target_port}/next.jpg\r\nContent-Length: 0\r\n\r\n"
        );
        let (redirect_endpoint, redirect_server) =
            start_http_fixture(Some(redirect.into_bytes())).await;
        let redirect_adapter = WifiSnapshotAdapter::new_for_loopback_fixture(
            "wifi-fixture-redirect",
            DeviceTransport::WifiHttpJpeg,
            WifiEndpointAllowlist::new(vec![redirect_endpoint.clone()]).unwrap(),
            Duration::from_millis(200),
            1024,
        )
        .unwrap();
        let error = redirect_adapter
            .camera_capture(
                &DeviceRequestContext::new("test-user", None),
                &camera_record(&redirect_endpoint),
            )
            .await
            .unwrap_err();
        assert_eq!(error.category, DeviceErrorCategory::Forbidden);
        redirect_server.await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(50), redirect_target.accept())
                .await
                .is_err(),
            "重定向目标不应收到后续请求"
        );

        let (timeout_endpoint, timeout_server) = start_http_fixture(None).await;
        let timeout_adapter = WifiSnapshotAdapter::new_for_loopback_fixture(
            "wifi-fixture-timeout",
            DeviceTransport::WifiHttpJpeg,
            WifiEndpointAllowlist::new(vec![timeout_endpoint.clone()]).unwrap(),
            Duration::from_millis(20),
            1024,
        )
        .unwrap();
        let error = timeout_adapter
            .camera_capture(
                &DeviceRequestContext::new("test-user", None),
                &camera_record(&timeout_endpoint),
            )
            .await
            .unwrap_err();
        assert_eq!(error.category, DeviceErrorCategory::Unavailable);
        timeout_server.abort();
    }

    #[test]
    fn wifi_endpoint_allowlist_rejects_credentials_and_non_http_protocols() {
        assert!(WifiEndpointAllowlist::new(vec![
            "http://camera.local/snapshot.jpg?token=secret".into()
        ])
        .is_err());
        assert!(WifiEndpointAllowlist::new(vec!["file:///etc/passwd".into()]).is_err());
    }

    #[tokio::test]
    async fn disabling_experiment_cancels_actions_and_clears_discovery_candidates() {
        let (events, _) = broadcast::channel(32);
        let service = DeviceService::new(enabled_config(), Vec::new(), events).unwrap();
        let adapter = Arc::new(SimulatedAdapter::new("fixture", DeviceTransport::Bluetooth));
        adapter.add_candidate(speaker_candidate("speaker-1"));
        adapter.set_online("speaker-1", true);
        service.register_adapter(adapter).unwrap();
        let ctx = DeviceRequestContext::new("admin", Some("session-1".into()));
        service
            .discover(
                &ctx,
                DiscoveryRequest {
                    transport: DeviceTransport::Bluetooth,
                    scope: String::new(),
                    duration_secs: 1,
                    max_results: 1,
                },
            )
            .await
            .unwrap();
        service
            .pair(
                &ctx,
                "speaker-1",
                "admin",
                Some("session-1"),
                [DeviceCapability::AudioOutput].into_iter().collect(),
                true,
            )
            .await
            .unwrap();
        service.connect(&ctx, "speaker-1").await.unwrap();
        let lease = service
            .begin_action(&ctx, "speaker-1", DeviceCapability::AudioOutput)
            .unwrap();

        service
            .apply_config(
                &DeviceRequestContext::new("admin", None),
                Default::default(),
                service.records(),
            )
            .unwrap();
        assert!(lease.is_cancelled());
        assert!(service.candidates().is_empty());
        assert_eq!(
            service
                .finish_action(&ctx, &lease, Ok(()))
                .unwrap_err()
                .category,
            DeviceErrorCategory::Cancelled
        );
        let error = service
            .discover(
                &ctx,
                DiscoveryRequest {
                    transport: DeviceTransport::Bluetooth,
                    scope: String::new(),
                    duration_secs: 1,
                    max_results: 1,
                },
            )
            .await
            .unwrap_err();
        assert_eq!(error.category, DeviceErrorCategory::Forbidden);
    }

    #[test]
    fn duplicate_persisted_devices_are_rejected_instead_of_overwritten() {
        let candidate = speaker_candidate("speaker-duplicate");
        let record = DeviceRecord {
            id: candidate.id,
            name: candidate.name,
            transport: candidate.transport,
            kind: candidate.kind,
            endpoint: candidate.endpoint,
            capabilities: candidate.capabilities,
            grants: Vec::new(),
            enabled: true,
        };
        let (events, _) = broadcast::channel(8);
        let result = DeviceService::new(enabled_config(), vec![record.clone(), record], events);
        let error = match result {
            Ok(_) => panic!("重复设备应被拒绝"),
            Err(error) => error,
        };
        assert_eq!(error.category, DeviceErrorCategory::Conflict);
    }
}
