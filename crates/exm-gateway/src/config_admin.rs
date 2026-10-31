//! gateway/config_admin —— 自 lib.rs 按资源域拆出（纯搬移，零行为变更）

use super::*;

pub(crate) async fn get_config(State(st): State<AppState>) -> impl IntoResponse {
    let cfg = st.core.config();
    // 主 Key 与 Key 池都为空才算未配置（池可用时运行时同样能出请求，掩码语义保持一致）
    let masked = if cfg.llm.api_key.is_empty() && cfg.llm.api_keys.is_empty() {
        String::new()
    } else {
        "***已配置***".to_string()
    };
    Json(json!({
        "llm": {
            "baseUrl": cfg.llm.base_url,
            "apiKey": masked,
            "model": cfg.llm.model,
        },
        "maxConcurrency": cfg.max_concurrency,
        "maxSessionTokens": cfg.max_session_tokens,
        "memory": {
            "enabled": cfg.memory_enabled,
            "recallLimit": cfg.memory_recall_limit,
            "halfLifeDays": cfg.memory_half_life_days,
            "semanticModel": cfg.memory_semantic_model,
            "mdMaxChars": cfg.memory_md_max_chars,
        },
        "security": {
            "execApproval": cfg.security.exec_approval,
            "execAllowlist": cfg.security.exec_allowlist,
            "authKey": if cfg.security.auth_key.is_empty() { String::new() } else { "***已配置***".to_string() },
            "terminalTimeoutSecs": cfg.security.terminal_timeout_secs,
            "toolOutputSpillChars": cfg.security.tool_output_spill_chars,
            "approvalWaitSecs": cfg.security.approval_wait_secs,
            "keySetupDismissed": cfg.security.key_setup_dismissed,
        },
        "automation": {
            "heartbeatEnabled": cfg.automation.heartbeat_enabled,
            "heartbeatIntervalMinutes": cfg.automation.heartbeat_interval_minutes,
            "heartbeatPrompt": cfg.automation.heartbeat_prompt,
            "heartbeatAgent": cfg.automation.heartbeat_agent,
            "autoAdapt": cfg.automation.auto_adapt,
            "unitMaxSteps": cfg.automation.unit_max_steps,
        },
        "search": {
            "provider": cfg.search.provider,
            "endpoint": cfg.search.endpoint,
            "apiKey": if cfg.search.api_key.is_empty() { String::new() } else { "***已配置***".to_string() },
            "maxResults": cfg.search.max_results,
        },
        "hooks": {
            "preTool": cfg.hooks.pre_tool,
            "postTool": cfg.hooks.post_tool,
            "onRunEnd": cfg.hooks.on_run_end,
        },
        "sandbox": {
            "mode": cfg.sandbox.mode,
            "allowNetwork": cfg.sandbox.allow_network,
            "useBwrap": cfg.sandbox.use_bwrap,
            "memoryMb": cfg.sandbox.memory_mb,
            "maxProcesses": cfg.sandbox.max_processes,
        },
        "browser": {
            "executable": cfg.browser.executable,
            "headless": cfg.browser.headless,
            "timeoutSecs": cfg.browser.timeout_secs,
            "maxChars": cfg.browser.max_chars,
        },
        "tools": {
            "custom": cfg.tools.custom,
        },
        "trash": {
            "retentionDays": cfg.trash.retention_days,
            "maxBytes": cfg.trash.max_bytes,
        },
        "experiments": {
            "dynamicCompaction": cfg.experiments.dynamic_compaction,
            "groupSharedContext": cfg.experiments.group_shared_context,
        },
        "mock": st.core.is_mock(),
    }))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ConfigBody {
    #[serde(default)]
    llm: Option<PartialLlm>,
    #[serde(default)]
    max_concurrency: Option<usize>,
    #[serde(default)]
    max_session_tokens: Option<u64>,
    #[serde(default)]
    memory: Option<PartialMemory>,
    /// 安全与审批
    #[serde(default)]
    security: Option<PartialSecurity>,
    /// Computer Use
    #[serde(default)]
    computer: Option<PartialComputer>,
    #[serde(default)]
    automation: Option<PartialAutomation>,
    /// 联网搜索后端（web_search 工具）
    #[serde(default)]
    search: Option<exm_core::config::SearchConfig>,
    /// 生命周期钩子（字段接受「逗号分隔字符串」或「字符串数组」两种形态）
    #[serde(default)]
    hooks: Option<PartialHooks>,
    /// 沙箱执行
    #[serde(default)]
    sandbox: Option<exm_core::config::SandboxConfig>,
    /// 浏览器自动化
    #[serde(default)]
    browser: Option<exm_core::config::BrowserConfig>,
    /// 工具面（声明式自定义工具；custom 接受数组或 JSON 字符串两种形态）
    #[serde(default)]
    tools: Option<PartialTools>,
    /// 回收站保留契约（保留期 + 容量上限）
    #[serde(default)]
    trash: Option<PartialTrash>,
    /// 实验性功能开关（动态压缩 / 组共享上下文）
    #[serde(default)]
    experiments: Option<PartialExperiments>,
}

#[derive(Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PartialTools {
    #[serde(default)]
    custom: Option<serde_json::Value>,
}

/// 实验性功能开关（设置页）：部分更新语义——缺省字段沿用旧值
#[derive(Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PartialExperiments {
    #[serde(default)]
    dynamic_compaction: Option<bool>,
    #[serde(default)]
    group_shared_context: Option<bool>,
}

/// 回收站保留契约（部分更新：缺省沿用旧值；0 视为未提供回默认）
#[derive(Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PartialTrash {
    #[serde(default)]
    retention_days: Option<u64>,
    #[serde(default)]
    max_bytes: Option<u64>,
}

/// 自定义工具清单归一：JSON 数组（程序化）或 JSON 字符串（设置页文本框）；解析失败返回 None（沿用旧值）
pub(crate) fn to_custom_tools(v: serde_json::Value) -> Option<Vec<exm_core::config::CustomTool>> {
    let parsed: Option<Vec<exm_core::config::CustomTool>> = match v {
        serde_json::Value::Array(_) => serde_json::from_value(v).ok(),
        serde_json::Value::String(s) => {
            let t = s.trim();
            if t.is_empty() {
                Some(Vec::new())
            } else {
                serde_json::from_str(t).ok()
            }
        }
        _ => None,
    };
    parsed.map(|list| {
        list.into_iter()
            .filter(|c| c.valid() && !c.target.trim().is_empty())
            .collect()
    })
}

#[derive(Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PartialHooks {
    #[serde(default)]
    pre_tool: Option<serde_json::Value>,
    #[serde(default)]
    post_tool: Option<serde_json::Value>,
    #[serde(default)]
    on_run_end: Option<serde_json::Value>,
}

/// 钩子字段归一：接受字符串（逗号分隔）或数组；None = 沿用旧值
pub(crate) fn to_hook_list(v: Option<serde_json::Value>) -> Option<Vec<String>> {
    match v? {
        serde_json::Value::String(s) => Some(
            s.split(',')
                .map(|x| x.trim().to_string())
                .filter(|x| !x.is_empty())
                .collect(),
        ),
        serde_json::Value::Array(a) => Some(
            a.iter()
                .filter_map(|x| x.as_str().map(|s| s.trim().to_string()))
                .filter(|s| !s.is_empty())
                .collect(),
        ),
        _ => None,
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PartialSecurity {
    #[serde(default)]
    exec_approval: Option<String>,
    /// 命令前缀白名单：接受「逗号分隔字符串」或「字符串数组」两种形态（schema 下发 string，设置页文本框直存）
    #[serde(default)]
    exec_allowlist: Option<serde_json::Value>,
    #[serde(default)]
    auth_key: Option<String>,
    #[serde(default)]
    terminal_timeout_secs: Option<u32>,
    #[serde(default)]
    tool_output_spill_chars: Option<usize>,
    #[serde(default)]
    approval_wait_secs: Option<u64>,
    chat_max_steps: Option<usize>,
    context_budget_ratio: Option<f64>,
}

/// Computer Use（设置页）：部分更新语义——缺省字段沿用旧值
#[derive(Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PartialComputer {
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    approval: Option<bool>,
    #[serde(default)]
    max_edge: Option<u32>,
    #[serde(default)]
    action_interval_ms: Option<u64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PartialAutomation {
    #[serde(default)]
    heartbeat_enabled: Option<bool>,
    #[serde(default)]
    heartbeat_interval_minutes: Option<u32>,
    #[serde(default)]
    heartbeat_prompt: Option<String>,
    #[serde(default)]
    heartbeat_agent: Option<String>,
    #[serde(default)]
    auto_adapt: Option<bool>,
    #[serde(default)]
    unit_max_steps: Option<usize>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PartialMemory {
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    recall_limit: Option<usize>,
    #[serde(default)]
    half_life_days: Option<f64>,
    #[serde(default)]
    semantic_model: Option<String>,
    /// memory.md 字数上限（超限触发 AI 自主压缩；0 = 恢复默认 5000）
    #[serde(default)]
    md_max_chars: Option<usize>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PartialLlm {
    #[serde(default)]
    base_url: Option<String>,
    #[serde(default)]
    api_key: Option<String>,
    #[serde(default)]
    model: Option<String>,
}

pub(crate) async fn put_config(State(st): State<AppState>, Json(body): Json<ConfigBody>) -> impl IntoResponse {
    let current = st.core.config();
    let partial = body.llm.unwrap_or(PartialLlm {
        base_url: None,
        api_key: None,
        model: None,
    });
    let api_key = match partial.api_key.as_deref() {
        Some("***已配置***") | None => current.llm.api_key.clone(),
        Some(v) if v.trim().is_empty() => current.llm.api_key.clone(),
        Some(v) => v.to_string(),
    };
    let mem = body.memory.unwrap_or(PartialMemory {
        enabled: None,
        recall_limit: None,
        half_life_days: None,
        semantic_model: None,
        md_max_chars: None,
    });
    let sec = body.security.unwrap_or(PartialSecurity {
        exec_approval: None,
        exec_allowlist: None,
        auth_key: None,
        terminal_timeout_secs: None,
        tool_output_spill_chars: None,
        approval_wait_secs: None,
        chat_max_steps: None,
        context_budget_ratio: None,
    });
    let computer = body.computer.clone().unwrap_or(PartialComputer {
        enabled: None,
        approval: None,
        max_edge: None,
        action_interval_ms: None,
    });
    let auto = body.automation.unwrap_or(PartialAutomation {
        heartbeat_enabled: None,
        heartbeat_interval_minutes: None,
        heartbeat_prompt: None,
        heartbeat_agent: None,
        auto_adapt: None,
        unit_max_steps: None,
    });
    let sec_search = body.search.clone();
    let sec_hooks = body.hooks.clone();
    // 搜索 API Key 三态语义（与 auth_key 一致）：掩码/缺省 = 沿用旧值；显式空串 = 清除；明文 = 新值
    let search = match sec_search {
        None => current.search.clone(),
        Some(s) => exm_core::config::SearchConfig {
            provider: s.provider.trim().to_string(),
            endpoint: s.endpoint.trim().to_string(),
            api_key: match s.api_key.trim() {
                "" => String::new(),
                "***已配置***" => current.search.api_key.clone(),
                other => other.to_string(),
            },
            max_results: s.max_results.clamp(1, 20),
        },
    };
    let hooks = match sec_hooks {
        None => current.hooks.clone(),
        Some(h) => {
            let mut next = current.hooks.clone();
            if let Some(v) = to_hook_list(h.pre_tool) {
                next.pre_tool = v;
            }
            if let Some(v) = to_hook_list(h.post_tool) {
                next.post_tool = v;
            }
            if let Some(v) = to_hook_list(h.on_run_end) {
                next.on_run_end = v;
            }
            next
        }
    };
    let sandbox = body
        .sandbox
        .clone()
        .unwrap_or_else(|| current.sandbox.clone());
    let browser = body
        .browser
        .clone()
        .unwrap_or_else(|| current.browser.clone());
    // 自定义工具：数组或 JSON 字符串；非法 JSON 保留旧清单（设置页文本框手滑不至于清空工具面）
    let tools = match &body.tools {
        None => current.tools.clone(),
        Some(pt) => match pt.custom.clone() {
            Some(v) => match to_custom_tools(v) {
                Some(list) => exm_core::config::ToolsConfig { custom: list },
                None => current.tools.clone(),
            },
            None => current.tools.clone(),
        },
    };
    let mut next = ExmConfig {
        llm: LlmConfig {
            base_url: partial
                .base_url
                .unwrap_or_else(|| current.llm.base_url.clone()),
            api_key,
            api_keys: current.llm.api_keys.clone(),
            api_format: current.llm.api_format.clone(),
            model: partial.model.unwrap_or_else(|| current.llm.model.clone()),
        },
        max_concurrency: body.max_concurrency.unwrap_or(current.max_concurrency),
        max_session_tokens: body
            .max_session_tokens
            .unwrap_or(current.max_session_tokens),
        memory_md_max_chars: mem.md_max_chars.unwrap_or(current.memory_md_max_chars),
        memory_enabled: mem.enabled.unwrap_or(current.memory_enabled),
        memory_recall_limit: mem.recall_limit.unwrap_or(current.memory_recall_limit),
        memory_half_life_days: mem.half_life_days.unwrap_or(current.memory_half_life_days),
        memory_semantic_model: mem
            .semantic_model
            .unwrap_or_else(|| current.memory_semantic_model.clone()),
        use_mock: false,
        security: exm_core::config::SecurityConfig {
            exec_approval: sec
                .exec_approval
                .unwrap_or_else(|| current.security.exec_approval.clone()),
            // 白名单：逗号分隔字符串 / 字符串数组双形态归一（设置页 schema kind=string）
            exec_allowlist: to_hook_list(sec.exec_allowlist)
                .unwrap_or_else(|| current.security.exec_allowlist.clone()),
            // 掩码/缺省 = 沿用旧密钥；显式空串 = 清除（回到免鉴权）
            auth_key: match sec.auth_key.as_deref() {
                Some("***已配置***") => current.security.auth_key.clone(),
                Some(k) => k.trim().to_string(),
                None => current.security.auth_key.clone(),
            },
            terminal_timeout_secs: sec
                .terminal_timeout_secs
                .unwrap_or(current.security.terminal_timeout_secs),
            tool_output_spill_chars: sec
                .tool_output_spill_chars
                .unwrap_or(current.security.tool_output_spill_chars),
            approval_wait_secs: sec
                .approval_wait_secs
                .unwrap_or(current.security.approval_wait_secs),
            chat_max_steps: sec
                .chat_max_steps
                .filter(|v| *v > 0)
                .unwrap_or(current.security.chat_max_steps),
            context_budget_ratio: sec
                .context_budget_ratio
                .filter(|v| (0.5..=1.0).contains(v))
                .unwrap_or(current.security.context_budget_ratio),
            // 首启引导提醒：不由设置页改写（专属端点 auth/dismiss 控制）
            key_setup_dismissed: current.security.key_setup_dismissed,
        },
        computer: exm_core::config::ComputerUseConfig {
            enabled: computer.enabled.unwrap_or(current.computer.enabled),
            approval: computer.approval.unwrap_or(current.computer.approval),
            max_edge: computer.max_edge.unwrap_or(current.computer.max_edge),
            action_interval_ms: computer
                .action_interval_ms
                .unwrap_or(current.computer.action_interval_ms),
        },
        automation: exm_core::config::AutomationConfig {
            heartbeat_enabled: auto
                .heartbeat_enabled
                .unwrap_or(current.automation.heartbeat_enabled),
            heartbeat_interval_minutes: auto
                .heartbeat_interval_minutes
                .unwrap_or(current.automation.heartbeat_interval_minutes),
            heartbeat_prompt: auto
                .heartbeat_prompt
                .unwrap_or_else(|| current.automation.heartbeat_prompt.clone()),
            // 个体目标：显式空串 = 清除（回到全局巡检）；缺省 = 沿用旧值
            heartbeat_agent: match auto.heartbeat_agent {
                Some(s) => {
                    let t = s.trim().to_string();
                    if t.is_empty() {
                        None
                    } else {
                        Some(t)
                    }
                }
                None => current.automation.heartbeat_agent.clone(),
            },
            auto_adapt: auto.auto_adapt.unwrap_or(current.automation.auto_adapt),
            unit_max_steps: auto
                .unit_max_steps
                .unwrap_or(current.automation.unit_max_steps),
        },
        search,
        hooks,
        sandbox,
        browser,
        tools,
        trash: {
            let t = body.trash.clone().unwrap_or_default();
            exm_core::config::TrashConfig {
                // 0 = 恢复默认（与设置页「留空回默认」口径一致）
                retention_days: t
                    .retention_days
                    .filter(|v| *v > 0)
                    .unwrap_or(current.trash.retention_days),
                max_bytes: t
                    .max_bytes
                    .filter(|v| *v > 0)
                    .unwrap_or(current.trash.max_bytes),
            }
        },
        experiments: {
            let e = body.experiments.clone().unwrap_or_default();
            exm_core::config::ExperimentsConfig {
                dynamic_compaction: e
                    .dynamic_compaction
                    .unwrap_or(current.experiments.dynamic_compaction),
                group_shared_context: e
                    .group_shared_context
                    .unwrap_or(current.experiments.group_shared_context),
            }
        },
        ..(*current).clone()
    };
    // 测试替身状态由运行期/环境决定，配置载荷不改变它（设置页的 llm 修改只在"真实通道"语义内生效）
    next.use_mock = st.core.is_mock() || exm_core::config::mock_enabled_from_env();
    // 设置页对 llm 的修改同步回生效档案（避免档案切换时被旧值回退）。
    // Key 池污染修复：运行时池可能是「activate 时池空补主 Key」的产物（Key 轮换需要
    // 非空池），整体回写会把主 Key 复制进档案池（曾致 pb9a743 档案主 Key == 池[0]）。
    // 回写时剔除「与主 Key 同串且来自运行时补位」的键：档案池本身为空时只回写主 Key
    // 与空池，运行时补位键不落档案。
    if let Some(slot) = next
        .llm_profiles
        .iter_mut()
        .find(|p| p.id == next.active_profile)
    {
        let profile_pool_was_empty = slot.api_keys.iter().all(|k| k.trim().is_empty());
        let mut runtime_pool = next.llm.api_keys.clone();
        if profile_pool_was_empty && runtime_pool.len() == 1 && runtime_pool[0] == next.llm.api_key {
            // 运行时池只是主 Key 补位：档案池保持空（激活/加载时会再补位，不丢功能）
            runtime_pool.clear();
        }
        slot.base_url = next.llm.base_url.clone();
        slot.api_key = next.llm.api_key.clone();
        slot.api_keys = runtime_pool;
        slot.model = next.llm.model.clone();
    }

    match st.core.apply_config(next) {
        Ok(_) => Json(json!({ "ok": true, "mock": st.core.is_mock() })).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

// ---------------------------------------------------------------- 记忆系统

pub(crate) async fn get_config_schema() -> impl IntoResponse {
    Json(exm_core::config::config_schema())
}
