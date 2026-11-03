//! tools/dynamic.rs —— 自 tools.rs 按领域拆出（纯搬移，零行为变更）

/// 动态工具输出：文本 + 可选图像（文件路径；视觉模型经 data URL 回灌）。
/// 既有 (bool, String) 元组语义经 From 兼容升级（images 空）。
pub struct DynamicToolOutput {
    pub ok: bool,
    pub text: String,
    /// 图像文件路径（工作区内；read_image_data_url 白名单格式与大小上限）
    pub images: Vec<String>,
}

impl From<(bool, String)> for DynamicToolOutput {
    fn from(v: (bool, String)) -> Self {
        Self {
            ok: v.0,
            text: v.1,
            images: Vec::new(),
        }
    }
}

/// 动态工具执行器（T3 内置插件注册）：接收 (agent_id, session_id, args)，
/// 返回输出（文本 + 可选图像路径）。异步闭包由注册方构造（平台 API / 会话态访问）。
pub type DynamicToolExec = std::sync::Arc<
    dyn Fn(
            &str,
            &str,
            serde_json::Value,
        ) -> futures_util::future::BoxFuture<'static, DynamicToolOutput>
        + Send
        + Sync,
>;

/// 便捷构造动态执行器（闭包返回 anyhow::Result<String>）
pub fn dynamic_exec<F, Fut>(f: F) -> DynamicToolExec
where
    F: Fn(&str, &str, serde_json::Value) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = anyhow::Result<String>> + Send + 'static,
{
    std::sync::Arc::new(move |agent, session, args| {
        let fut = f(agent, session, args);
        Box::pin(async move {
            match fut.await {
                Ok(text) => DynamicToolOutput {
                    ok: true,
                    text,
                    images: Vec::new(),
                },
                Err(e) => DynamicToolOutput {
                    ok: false,
                    text: e.to_string(),
                    images: Vec::new(),
                },
            }
        })
    })
}

/// 带图像输出版 dynamic_exec：闭包返回 anyhow::Result<(文本, 图像路径列表)>
pub fn dynamic_exec_images<F, Fut>(f: F) -> DynamicToolExec
where
    F: Fn(&str, &str, serde_json::Value) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = anyhow::Result<(String, Vec<String>)>> + Send + 'static,
{
    std::sync::Arc::new(move |agent, session, args| {
        let fut = f(agent, session, args);
        Box::pin(async move {
            match fut.await {
                Ok((text, images)) => DynamicToolOutput {
                    ok: true,
                    text,
                    images,
                },
                Err(e) => DynamicToolOutput {
                    ok: false,
                    text: e.to_string(),
                    images: Vec::new(),
                },
            }
        })
    })
}
