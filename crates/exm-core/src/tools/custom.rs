//! tools/custom.rs —— 自 tools.rs 按领域拆出（纯搬移，零行为变更）

use super::*;

pub(crate) fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char)
            }
            b' ' => out.push_str("%20"),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

// ---------------------------------------------------------------- 自定义工具插值

/// 模板里的占位名清单（`{name}`；不完整的 `{` 忽略）
pub(crate) fn placeholder_names(template: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = template;
    while let Some(i) = rest.find('{') {
        match rest[i + 1..].find('}') {
            Some(j) => {
                let name = rest[i + 1..i + 1 + j].trim().to_string();
                if !name.is_empty() && !out.contains(&name) {
                    out.push(name);
                }
                rest = &rest[i + j + 2..];
            }
            None => break,
        }
    }
    out
}


pub(crate) fn interpolate(
    template: &str,
    vals: &std::collections::BTreeMap<String, String>,
    f: impl Fn(&str) -> String,
) -> String {
    let mut out = String::with_capacity(template.len() + 32);
    let mut rest = template;
    while let Some(i) = rest.find('{') {
        out.push_str(&rest[..i]);
        match rest[i + 1..].find('}') {
            Some(j) => {
                let name = rest[i + 1..i + 1 + j].trim();
                out.push_str(&f(vals.get(name).map(String::as_str).unwrap_or("")));
                rest = &rest[i + j + 2..];
            }
            None => {
                out.push('{');
                rest = &rest[i + 1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// 简化 JSON path 求值：`$.a.b[0].c`（点取字段 / 下标取数组；前导 `$.` 可省）。
/// 命中返回子树；路径不存在/类型不符返回 None。
pub(crate) fn json_path<'v>(root: &'v serde_json::Value, path: &str) -> Option<&'v serde_json::Value> {
    let p = path.trim();
    let p = p.strip_prefix('$').unwrap_or(p);
    let p = p.strip_prefix('.').unwrap_or(p);
    if p.is_empty() {
        // `$` 或 `$.` 之外的空段（如 `""`/`"$"`）= 整根；尾随孤立 `.` 非法
        if path.trim() == "$" || path.trim().is_empty() {
            return Some(root);
        }
        return None;
    }
    let mut cur = root;
    let mut rest = p;
    while !rest.is_empty() {
        // 前导 `.` 分隔（首轮无）
        if let Some(stripped) = rest.strip_prefix('.') {
            if stripped.is_empty() {
                return None; // 尾随 `.` 非法
            }
            rest = stripped;
        }
        // 字段名段：到下一个 `.` 或 `[`
        let seg_end = rest.find(['.', '[']).unwrap_or(rest.len());
        if seg_end > 0 {
            let key = &rest[..seg_end];
            cur = cur.as_object().and_then(|o| o.get(key))?;
            rest = &rest[seg_end..];
        }
        // 下标段：连续的 `[n]`（必须紧跟字段或前一 `]`）
        while rest.starts_with('[') {
            let close = rest.find(']')?;
            let idx: usize = rest[1..close].trim().parse().ok()?;
            cur = cur.as_array().and_then(|a| a.get(idx))?;
            rest = &rest[close + 1..];
        }
        if !rest.is_empty() && !rest.starts_with('.') {
            return None; // 残余非法语法
        }
    }
    Some(cur)
}

/// 自定义工具响应治理（插件生态扩展）：JSON 响应按 select 摘取 → maxChars 截断。
/// select 路径不存在返回明确错误（拼空串比报错更坑）；非 JSON 忽略 select。
pub(crate) fn shape_response(tool: &crate::config::CustomTool, text: &str) -> ToolResult {
    if let Some(sel) = tool
        .response_select
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(text) {
            return match json_path(&v, sel) {
                Some(sub) => {
                    let s = match sub {
                        serde_json::Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    shape_truncate(tool, &s)
                }
                None => ToolResult::err(format!(
                    "工具 {} 的响应不含 select 路径 {sel}（响应为 JSON 但路径不存在）",
                    tool.name
                )),
            };
        }
        // 非 JSON：忽略 select，仅截断
    }
    shape_truncate(tool, text)
}

pub(crate) fn shape_truncate(tool: &crate::config::CustomTool, text: &str) -> ToolResult {
    let cap = tool.response_max_chars.unwrap_or(8000);
    let mut out: String = text.chars().take(cap).collect();
    if out.len() < text.len() {
        out.push_str("…（截断）");
    }
    ToolResult::ok(out)
}

impl ToolGateway {
    // ---------------------------------------------------------------- 声明式自定义工具

    /// 执行声明式自定义工具：http（GET query / POST JSON）或 shell（模板插值 + 审批闸门）。
    /// 输出与内置工具同口径截断，并共享结果落盘 / 钩子 / 审计链路（execute_named 统一走这里）。
    pub(crate) async fn run_custom(
        &self,
        agent_id: &str,
        session_id: &str,
        tool: &crate::config::CustomTool,
        args: &serde_json::Value,
    ) -> ToolResult {
        let Some(params) = args.as_object() else {
            return ToolResult::err("自定义工具的参数必须是对象");
        };
        // 插值用字符串表：字符串直取，其余 JSON 序列化
        let mut vals: std::collections::BTreeMap<String, String> = Default::default();
        for (k, v) in params {
            let s = match v {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            vals.insert(k.clone(), s);
        }
        // target 引用的占位：schema 声明 required 而调用未给 → 明确报错（拼空串比报错更坑）
        let required: Vec<String> = tool
            .parameters
            .pointer("/required")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        for name in placeholder_names(&tool.target) {
            if let std::collections::btree_map::Entry::Vacant(slot) = vals.entry(name) {
                if required.iter().any(|r| r == slot.key()) {
                    return ToolResult::err(format!(
                        "自定义工具 {} 缺少必填参数 {}",
                        tool.name,
                        slot.key()
                    ));
                }
                slot.insert(String::new());
            }
        }
        if tool.kind_norm() == "shell" {
            return self
                .run_custom_shell(agent_id, session_id, tool, &vals)
                .await;
        }
        self.run_custom_http(tool, &vals, args).await
    }

    /// http 类自定义工具：GET 把实参拼进已插值的 URL；POST/PUT 把全部实参作为 JSON body
    pub(crate) async fn run_custom_http(
        &self,
        tool: &crate::config::CustomTool,
        vals: &std::collections::BTreeMap<String, String>,
        args: &serde_json::Value,
    ) -> ToolResult {
        // 插件凭据变量（plugin-ecosystem 1.3）：`${credentials.<key>}` 宿主侧解析——
        // 清单零密钥分发。仅命名空间工具可解析（config CustomTool 无插件归属，
        // 引用变量视为缺键）。缺失时拒绝调用（不发部分鉴权请求）。
        let plugin_id = crate::plugin::parse_namespaced(&tool.name).map(|(p, _)| p);
        let (url, mut missing) = match &plugin_id {
            Some(pid) => self.resolve_credential_vars(pid, &tool.target),
            None if tool.target.contains("${credentials.") => {
                return ToolResult::err(format!(
                    "工具 {} 引用了 ${{credentials.*}} 变量但无插件归属（仅插件清单工具可声明凭据变量）",
                    tool.name
                ))
            }
            None => (tool.target.clone(), Vec::new()),
        };
        let mut headers: std::collections::BTreeMap<String, String> = Default::default();
        for (k, v) in &tool.headers {
            let resolved = match &plugin_id {
                Some(pid) => self.resolve_credential_vars(pid, v),
                None => (v.clone(), Vec::new()),
            };
            missing.extend(resolved.1);
            headers.insert(k.clone(), resolved.0);
        }
        if !missing.is_empty() {
            return ToolResult::err(format!(
                "工具 {} 的凭据未配置：{}（请在插件详情的凭据区配置后重试）",
                tool.name,
                missing.join("、")
            ));
        }
        // 实参插值（凭据解析后的 URL 上做 `{参数名}` 占位替换）
        let url = interpolate(&url, vals, urlencode);
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return ToolResult::err("http 类自定义工具的 target 必须是 http(s) URL");
        }
        let method = {
            let m = tool.method.trim().to_ascii_uppercase();
            if m.is_empty() {
                "POST".to_string()
            } else {
                m
            }
        };
        let client = match reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
        {
            Ok(c) => c,
            Err(e) => return ToolResult::err(format!("HTTP 客户端构建失败: {e}")),
        };
        let parsed =
            reqwest::Method::from_bytes(method.as_bytes()).unwrap_or(reqwest::Method::POST);
        let mut req = client.request(parsed, &url);
        for (k, v) in &headers {
            req = req.header(k, v);
        }
        if method == "POST" || method == "PUT" || method == "PATCH" {
            req = req.json(args);
        }
        match req.send().await {
            Ok(r) => {
                let status = r.status();
                let text = r.text().await.unwrap_or_default();
                if !status.is_success() {
                    return ToolResult::err(format!(
                        "HTTP {status}：{}",
                        text.chars().take(2000).collect::<String>()
                    ));
                }
                shape_response(tool, &text)
            }
            Err(e) => ToolResult::err(format!("请求失败: {e}")),
        }
    }

    /// shell 类自定义工具：模板插值（值经白名单校验 + 引号包裹）→ 与 terminal 同一道审批闸门
    pub(crate) async fn run_custom_shell(
        &self,
        agent_id: &str,
        session_id: &str,
        tool: &crate::config::CustomTool,
        vals: &std::collections::BTreeMap<String, String>,
    ) -> ToolResult {
        // 实参字符白名单：模板插值 + 引号包裹仍挡不住的 shell 元字符一律拒绝 —— 宁可失败不留注入面
        for (k, v) in vals {
            if let Some(c) = v.chars().find(|c| shell_unsafe_char(*c)) {
                return ToolResult::err(format!(
                    "自定义工具 {} 的参数 {k} 含不允许的字符 {c:?}（shell 类工具实参仅支持普通文本）",
                    tool.name
                ));
            }
        }
        let cmd = interpolate(&tool.target, vals, shell_quote);
        // 与 terminal 同一道闸门：声明式模板不是越权通道（risky 拦高危 / always 全拦）
        let lowered = cmd.to_lowercase();
        let mode = self.security.exec_approval.as_str();
        if mode == "risky" || mode == "always" {
            let allowlisted = self.security.exec_allowlist.iter().any(|p| {
                !p.trim().is_empty() && lowered.starts_with(p.trim().to_lowercase().as_str())
            });
            let gated = mode == "always" || is_destructive_command(&lowered);
            if gated && !allowlisted {
                return self.gate_command(agent_id, session_id, &cmd).await;
            }
        }
        let opts = self.shell_opts(&cmd);
        execute_shell_with(&self.effective_root(), &cmd, 120, &opts).await
    }
}
