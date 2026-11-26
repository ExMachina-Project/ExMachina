//! 可选远端后端的版本化 HTTP 适配器。端点/凭据只由服务端配置提供，模型不能选择。
//! 不重用旧 worker 的“失败后宿主回退”语义；任何提交只发送一次，未知态按稳定 ID 查询。

use super::*;
use reqwest::{Client, Method, Url};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};

const RESPONSE_LIMIT: usize = 1_048_576;

pub struct RemoteBackendLauncher {
    endpoint: Url,
    client: Client,
    token: String,
    descriptor: ExecutionBackendDescriptor,
    owned: parking_lot::Mutex<BTreeSet<String>>,
}

impl RemoteBackendLauncher {
    /// 查询受信端点的能力声明；仅允许 HTTPS 或数字 loopback HTTP。
    /// 服务端配置者仍须核验远端实际隔离边界，未核验的 manifest 应返回 Unverified。
    pub async fn connect(endpoint: &str, token: String) -> Result<Self, String> {
        let mut endpoint = Url::parse(endpoint).map_err(|_| "远端后端 URL 无效")?;
        let numeric_loopback = endpoint.host_str().is_some_and(|host| {
            host.trim_matches(['[', ']'])
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
        });
        if !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
            || !(endpoint.scheme() == "https" || (endpoint.scheme() == "http" && numeric_loopback))
        {
            return Err("远端后端仅接受无内嵌凭据的 HTTPS 或数字 loopback HTTP".into());
        }
        if !endpoint.path().ends_with('/') {
            endpoint.set_path(&format!("{}/", endpoint.path()));
        }
        let client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(Duration::from_secs(2))
            .timeout(Duration::from_secs(2))
            .build()
            .map_err(|_| "无法构造远端后端客户端")?;
        let url = endpoint
            .join("capabilities")
            .map_err(|_| "后端能力路径无效")?;
        let mut request = client.get(url);
        if !token.is_empty() {
            request = request.bearer_auth(&token);
        }
        let response = request.send().await.map_err(|_| "远端后端能力查询失败")?;
        let descriptor: ExecutionBackendDescriptor = bounded_json(response).await?;
        if descriptor.id != "remote" || descriptor.kind != BackendKind::Remote {
            return Err("远端能力响应身份不符".into());
        }
        let mut registry = ExecutionBackendRegistry::new();
        registry.register(descriptor.clone())?;
        Ok(Self {
            endpoint,
            client,
            token,
            descriptor,
            owned: parking_lot::Mutex::new(BTreeSet::new()),
        })
    }

    pub fn descriptor(&self) -> &ExecutionBackendDescriptor {
        &self.descriptor
    }

    async fn call<T: DeserializeOwned>(
        &self,
        method: Method,
        id: &str,
        suffix: &str,
        payload: Option<Value>,
    ) -> Result<T, String> {
        if !self.owned.lock().contains(id) {
            return Err("作业不属于此远端适配器".into());
        }
        // ID 由监督器生成而非模型提供；再防御路径控制字符与斜杠。
        if !id.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '-') {
            return Err("远端作业 ID 非法".into());
        }
        let url = self
            .endpoint
            .join(&format!("jobs/{id}{suffix}"))
            .map_err(|_| "远端作业路径无效")?;
        let mut request = self
            .client
            .request(method, url)
            .header("Idempotency-Key", id);
        if !self.token.is_empty() {
            request = request.bearer_auth(&self.token);
        }
        if let Some(payload) = payload {
            request = request.json(&payload);
        }
        let response = request
            .send()
            .await
            .map_err(|_| "远端请求超时或断连；结果待核验")?;
        let frame: Value = bounded_json(response).await?;
        if frame["schemaVersion"].as_u64() != Some(u64::from(EXEC_BACKEND_SCHEMA_VERSION))
            || frame["jobId"].as_str() != Some(id)
        {
            return Err("远端回执的版本或作业 ID 不匹配".into());
        }
        serde_json::from_value(frame["result"].clone()).map_err(|_| "远端作业结果不符合协议".into())
    }

    fn check_handle(&self, handle: &BackendJobHandle) -> Result<(), String> {
        if handle.backend_id != "remote" || handle.reference != handle.job_id {
            return Err("远端作业句柄身份不符".into());
        }
        Ok(())
    }
}

async fn bounded_json<T: DeserializeOwned>(mut response: reqwest::Response) -> Result<T, String> {
    if !response.status().is_success() {
        // 不把远端正文/URL/凭据拼进错误。
        return Err(format!("远端后端返回 HTTP {}", response.status().as_u16()));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| "远端响应中断")? {
        if bytes.len().saturating_add(chunk.len()) > RESPONSE_LIMIT {
            return Err("远端响应超过大小上限".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| "远端响应不符合后端协议".into())
}

#[async_trait::async_trait]
impl BackendLauncher for RemoteBackendLauncher {
    fn backend_id(&self) -> &str {
        "remote"
    }

    async fn start(&self, job: &PreparedJob, job_id: &str) -> Result<BackendJobHandle, String> {
        if job.selection.backend != self.descriptor || !self.descriptor.ready() {
            return Err("远端能力已变化或尚未验证，应重新选择".into());
        }
        self.owned.lock().insert(job_id.into());
        let reply: Value = self
            .call(
                Method::POST,
                job_id,
                "",
                Some(json!({
                    "schemaVersion": EXEC_BACKEND_SCHEMA_VERSION,
                    "jobId": job_id, "selection": job.selection, "spec": job.spec,
                })),
            )
            .await?;
        if reply["accepted"] != true {
            return Err("远端启动回执未确认接收".into());
        }
        Ok(BackendJobHandle {
            job_id: job_id.into(),
            backend_id: "remote".into(),
            reference: job_id.into(),
        })
    }

    async fn poll(&self, handle: &BackendJobHandle) -> BackendJobPoll {
        let result = async {
            self.check_handle(handle)?;
            self.call(Method::GET, &handle.job_id, "", None).await
        }
        .await;
        result.unwrap_or_else(|detail| BackendJobPoll::Disconnected { detail })
    }

    async fn cancel(&self, handle: &BackendJobHandle) -> Result<(), String> {
        self.check_handle(handle)?;
        let result: Value = self
            .call(Method::POST, &handle.job_id, "/cancel", None)
            .await?;
        if result["stopped"] != true {
            return Err("远端只接收取消请求，尚未确认停止".into());
        }
        Ok(())
    }

    async fn reap(&self, handle: &BackendJobHandle) -> Result<JobOutcome, String> {
        self.check_handle(handle)?;
        self.call(Method::DELETE, &handle.job_id, "", None).await
    }
}
