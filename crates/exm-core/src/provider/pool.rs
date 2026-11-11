//! provider 域分文件（自 provider.rs 按职责拆出，纯搬移零行为变更）。

use super::*;
// ---------------------------------------------------------------- 模型档案运行池

/// 模型档案运行池：档案ID → (Provider, 模型名)。
/// 供按个体/组解析默认模型（提示格式："档案ID" 或 "档案ID/模型名"）；
/// 档案编辑经 apply_config 重建池，组/个体的提示则热读取。
/// 嵌入模型不属于池的条目——它由「记忆」页的语义检索设置从既有提供商中指定。
pub struct ModelPool {
    entries: std::collections::HashMap<String, (std::sync::Arc<dyn LlmProvider>, String)>,
    /// 档案模型清单（模型名 + 视觉/语音能力开关）：多模态路由的判定依据
    models: std::collections::HashMap<String, Vec<crate::config::ModelEntry>>,
    /// 档案失败回退链：id → 下一个档案 id
    fallbacks: std::collections::HashMap<String, String>,
    /// 全局生效档案 id（回退链的最终兜底）
    active_id: String,
}

impl Default for ModelPool {
    fn default() -> Self {
        Self::new()
    }
}

impl ModelPool {
    pub fn new() -> Self {
        ModelPool {
            entries: std::collections::HashMap::new(),
            models: std::collections::HashMap::new(),
            fallbacks: std::collections::HashMap::new(),
            active_id: String::new(),
        }
    }

    /// 登记档案的模型清单（能力开关随档案编辑经 apply_config 重建）
    pub fn set_models(&mut self, id: impl Into<String>, models: Vec<crate::config::ModelEntry>) {
        self.models.insert(id.into(), models);
    }

    /// 模型能力标记：档案清单里该模型名的 vision/audio 开关。
    /// None = 清单为空或模型不在清单里（能力未知，调用方保持直通行为）。
    pub fn capability(&self, pid: &str, model: &str, vision: bool) -> Option<bool> {
        let list = self.models.get(pid)?;
        list.iter()
            .find(|m| m.model == model)
            .map(|m| if vision { m.vision } else { m.audio })
    }

    /// 模型上下文窗口（token 数）；档案/模型未登记或登记 0 = 未知 None
    pub fn context_window(&self, pid: &str, model: &str) -> Option<u64> {
        let list = self.models.get(pid)?;
        list.iter()
            .find(|m| m.model == model)
            .and_then(|m| (m.context_window > 0).then_some(m.context_window))
    }

    /// 全局生效档案 id（build_orchestrator 注入）
    pub fn set_active(&mut self, id: impl Into<String>) {
        self.active_id = id.into();
    }

    pub fn active_id(&self) -> String {
        self.active_id.clone()
    }

    /// 声明档案失败回退（指向下一档案 id；空串清除）
    pub fn set_fallback(&mut self, id: &str, next: &str) {
        if next.trim().is_empty() {
            self.fallbacks.remove(id);
        } else {
            self.fallbacks
                .insert(id.to_string(), next.trim().to_string());
        }
    }

    /// 从起始档案展开回退链（去重 + 成环截断，最长 16 跳）
    pub fn chain(&self, start: &str) -> Vec<String> {
        let mut out = vec![start.to_string()];
        let mut cur = start.to_string();
        for _ in 0..16 {
            let Some(next) = self.fallbacks.get(&cur).cloned() else {
                break;
            };
            if next == start || out.contains(&next) || next.is_empty() {
                break;
            }
            out.push(next.clone());
            cur = next;
        }
        out
    }

    /// 取档案的 (Provider, 模型名)
    pub fn entry(&self, id: &str) -> Option<(std::sync::Arc<dyn LlmProvider>, String)> {
        let (provider, model) = self.entries.get(id)?;
        Some((provider.clone(), model.clone()))
    }

    pub fn insert(
        &mut self,
        id: impl Into<String>,
        provider: std::sync::Arc<dyn LlmProvider>,
        model: impl Into<String>,
    ) {
        self.entries.insert(id.into(), (provider, model.into()));
    }

    /// 解析模型提示：返回 (Provider, 生效模型名)；档案不存在返回 None（调用方回退全局）
    pub fn resolve(&self, hint: &str) -> Option<(std::sync::Arc<dyn LlmProvider>, String)> {
        self.resolve_full(hint).map(|(_, p, m)| (p, m))
    }

    /// 解析模型提示：返回 (档案ID, Provider, 生效模型名)。
    /// 提示格式 "档案ID" 或 "档案ID/模型名"（后者在档案默认模型上临时覆盖）。
    pub fn resolve_full(
        &self,
        hint: &str,
    ) -> Option<(String, std::sync::Arc<dyn LlmProvider>, String)> {
        let (pid, explicit) = match hint.split_once('/') {
            Some((p, m)) if !m.trim().is_empty() => (p, Some(m.trim().to_string())),
            _ => (hint, None),
        };
        let pid = pid.trim().to_string();
        let entry: &(std::sync::Arc<dyn LlmProvider>, String) = self.entries.get(&pid)?;
        let provider: std::sync::Arc<dyn LlmProvider> = entry.0.clone();
        let model: String = explicit.unwrap_or_else(|| entry.1.clone());
        Some((pid, provider, model))
    }
}

// ---------------------------------------------------------------- 失败回退

/// 失败冷却时长（秒）：档案失败后回退链上被跳过的时间窗
pub const FAILOVER_COOLDOWN_SECS: u64 = 45;

pub struct FailoverState {
    until: parking_lot::Mutex<std::collections::HashMap<String, std::time::Instant>>,
}

impl Default for FailoverState {
    fn default() -> Self {
        FailoverState {
            until: parking_lot::Mutex::new(std::collections::HashMap::new()),
        }
    }
}

impl FailoverState {
    pub fn cooling(&self, id: &str) -> bool {
        let mut m = self.until.lock();
        match m.get(id) {
            Some(t) if *t > std::time::Instant::now() => true,
            Some(_) => {
                m.remove(id);
                false
            }
            None => false,
        }
    }

    /// 标记冷却（秒）
    pub fn cool(&self, id: &str, secs: u64) {
        self.until.lock().insert(
            id.to_string(),
            std::time::Instant::now() + std::time::Duration::from_secs(secs),
        );
    }

    /// 成功即解除冷却
    pub fn clear(&self, id: &str) {
        self.until.lock().remove(id);
    }
}
