//! tools/result.rs —— 自 tools.rs 按领域拆出（纯搬移，零行为变更）

pub struct ToolResult {
    pub ok: bool,
    pub output: String,
    pub error: Option<String>,
    /// 副产物图像（如 computer 截图）的落盘路径：runtime 在目标模型具备视觉能力时
    /// 作为图像输入回灌；渠道（WebUI）据此在工具卡里预览。
    pub images: Vec<String>,
}

impl ToolResult {
    pub(crate) fn ok(output: impl Into<String>) -> Self {
        ToolResult {
            ok: true,
            output: output.into(),
            error: None,
            images: Vec::new(),
        }
    }
    /// 成功 + 图像副产物（动态插件工具的视觉回灌：video__shots 等）
    pub(crate) fn ok_with_images(output: impl Into<String>, images: Vec<String>) -> Self {
        ToolResult {
            ok: true,
            output: output.into(),
            error: None,
            images,
        }
    }
    pub(crate) fn err(msg: impl Into<String>) -> Self {
        ToolResult {
            ok: false,
            output: String::new(),
            error: Some(msg.into()),
            images: Vec::new(),
        }
    }
    pub(crate) fn with_images(mut self, images: Vec<String>) -> Self {
        self.images = images;
        self
    }
}
