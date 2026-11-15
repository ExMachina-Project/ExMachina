//! provider 域分文件（自 provider.rs 按职责拆出，纯搬移零行为变更）。
//! 模型思考强度（reasoning effort）：进程级全局、请求级优先、token 上限。


/// 进程级思考强度（None = 端点默认）：网关经 /api/llm/effort 设置后对全部 LLM 调用生效
/// （指挥体规划、直答、子个体执行、工具回路共用）；请求级 `ChatRequest.reasoning` 显式值优先。
static REASONING_EFFORT: parking_lot::RwLock<Option<String>> = parking_lot::RwLock::new(None);

/// 设置全局思考强度；None 或空值 = 恢复端点默认。非法值归一为 medium。
pub fn set_reasoning_effort(v: Option<&str>) {
    let mut w = REASONING_EFFORT.write();
    *w = v.map(normalize_effort).filter(|s| !s.is_empty());
}

/// 当前全局思考强度
pub fn reasoning_effort() -> Option<String> {
    REASONING_EFFORT.read().clone()
}

/// 合法值收敛：low | medium | high（其余一律 medium，防端点 400）
pub(crate) fn normalize_effort(v: &str) -> String {
    match v.trim().to_lowercase().as_str() {
        "low" | "medium" | "high" => v.trim().to_lowercase(),
        _ => "medium".into(),
    }
}

// ------------------------------------------------- 推理预算硬上限（anti-runaway-thinking）

/// 进程级推理 token 上限：OpenAI 兼容端点写 max_tokens（reasoning+content 共享预算）。
/// 部署实证：不设上限时模型在规划阶段心算大数分解，单轮 thinking 20 万字符
/// （943s 延迟并触发上游 429 限流）——这是对推理模型唯一有效的「思考时长」硬闸。
/// 优先级：请求级 max_tokens > 进程级本值 > 默认安全网 16384（拦 9 万 token 级
/// 失控心算，不拦正常长回答）。网关经 /api/llm/effort 设置（body.reasoningTokenCap）。
static REASONING_TOKEN_CAP: parking_lot::RwLock<Option<u32>> = parking_lot::RwLock::new(None);

/// 设置全局推理 token 上限；None 或 <1024 = 回落默认安全网。
pub fn set_reasoning_token_cap(v: Option<u32>) {
    let mut w = REASONING_TOKEN_CAP.write();
    *w = v.filter(|v| *v >= 1024);
}

/// 当前全局推理 token 上限
pub fn reasoning_token_cap() -> Option<u32> {
    *REASONING_TOKEN_CAP.read()
}

