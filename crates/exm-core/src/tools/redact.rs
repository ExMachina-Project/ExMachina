//! 事件载荷脱敏（agent-budget-audit 3.5）：命令/参数中的密钥类值在进事件与
//! 审计前替换为掩码。规则表驱动（正则族），不做 LLM 判定——脱敏必须确定性、
//! 零成本、可审计（与 inbound-sanitization 同一设计哲学）。

use serde_json::Value;

/// 密钥类参数模式（键名 → 掩码宽度）。命中键名的值整体掩码，
/// 不保留前后缀——长度泄露也是泄露。
const SENSITIVE_KEYS: &[&str] = &[
    "password",
    "passwd",
    "secret",
    "token",
    "api_key",
    "apikey",
    "api-key",
    "access_key",
    "secret_key",
    "private_key",
    "authorization",
    "auth",
    "cookie",
    "session_id",
    "credential",
    "client_secret",
];

/// 值内联密钥形态（正则族）：sk- 开头的 API key、Bearer 头、长 hex/base64 疑似密钥。
/// 这些形态即使不是键值对（裸串在命令行里）也要打码。
fn inline_patterns() -> Vec<(&'static str, &'static str)> {
    vec![
        // OpenAI 风格 key（sk-...）
        ("sk-[A-Za-z0-9_\\-]{8,}", "sk-***"),
        // Bearer 头
        ("(?i)bearer\\s+[A-Za-z0-9_\\-.=+/]{8,}", "Bearer ***"),
        // gh-p_/github_pat_ 等 PAT
        ("(ghp|gho|ghu|ghs|github_pat)_[A-Za-z0-9]{16,}", "***"),
        // AWS 风格
        ("AKIA[0-9A-Z]{12,}", "AKIA***"),
        // 长十六进制串（≥32，JWT 段/私钥片段常见形态）
        ("[a-fA-F0-9]{32,}", "***"),
        // 长_base64ish（≥40 且无空格，token 常见形态；保守阈值减少误伤普通词）
        ("[A-Za-z0-9+/=]{40,}", "***"),
    ]
}

/// 键名是否敏感（含子串匹配：`--api-key=xxx` 这类复合键也命中）
fn is_sensitive_key(key: &str) -> bool {
    let lowered = key.to_lowercase();
    SENSITIVE_KEYS.iter().any(|k| lowered.contains(k))
}

/// 标量值脱敏：内联密钥形态打码；对象/数组递归（对象内敏感键在 redact_json
/// 的对象分支处理——此函数承接值位置的嵌套结构）。
fn redact_value(v: &Value) -> Value {
    match v {
        Value::String(s) => Value::String(redact_str(s)),
        Value::Object(_) => redact_json(v),
        Value::Array(a) => Value::Array(a.iter().map(redact_value).collect()),
        other => other.clone(),
    }
}

/// 字符串脱敏：应用全部内联模式
pub fn redact_str(s: &str) -> String {
    static PATTERNS: std::sync::LazyLock<Vec<(regex::Regex, &'static str)>> =
        std::sync::LazyLock::new(|| {
            inline_patterns()
                .into_iter()
                .filter_map(|(pat, mask)| regex::Regex::new(pat).ok().map(|re| (re, mask)))
                .collect()
        });
    let mut out = s.to_string();
    static KEY_VALUE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(
            r#"(?i)(\b(?:password|passwd|secret|token|api[_-]?key|access[_-]?key|secret[_-]?key|private[_-]?key|client[_-]?secret|authorization|credential)\b[\"']?\s*[:=]\s*)(\"(?:\\.|[^\"\\\r\n])*\"|'[^'\r\n]*'|(?:bearer\s+)?[^\s\"'&,;}\]]+)"#,
        )
        .expect("redaction key/value regex must compile")
    });
    out = KEY_VALUE
        .replace_all(&out, |captures: &regex::Captures<'_>| {
            let value = &captures[2];
            let mask = if value.starts_with('"') {
                "\"***\""
            } else if value.starts_with('\'') {
                "'***'"
            } else if value.to_ascii_lowercase().starts_with("bearer ") {
                "Bearer ***"
            } else {
                "***"
            };
            format!("{}{mask}", &captures[1])
        })
        .into_owned();
    for (re, mask) in PATTERNS.iter() {
        if let std::borrow::Cow::Owned(replaced) = re.replace_all(&out, *mask) {
            out = replaced;
        }
    }
    out
}

/// Sanitize the public payload before a CoreEvent crosses a broadcast boundary.
pub(crate) fn redact_core_event(mut event: crate::types::CoreEvent) -> crate::types::CoreEvent {
    event.payload = redact_observation(&event.payload);
    event
}

/// Observations contain typed IDs and token counters, not just tool arguments. Keep
/// those protocol fields intact; arbitrary tool arguments still use the stricter rule.
pub fn redact_observation(value: &Value) -> Value {
    match value {
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .map(|(key, value)| {
                    let safe = match key.as_str() {
                        "args" | "arguments" | "parameters" => redact_json(value),
                        "sessionId"
                        | "session_id"
                        | "childSessionId"
                        | "evidenceSessionId"
                        | "authorizationRevision" => redact_observation(value),
                        "promptTokens" | "completionTokens" | "inputTokens" | "outputTokens"
                        | "totalTokens" | "reasoningTokens" | "maxTokens" | "maxSessionTokens"
                        | "beforeTokens" | "afterTokens" | "tokenBudget" | "token_budget"
                            if value.is_number() || value.is_null() =>
                        {
                            value.clone()
                        }
                        "tokenUsage" | "tokenBudget" if value.is_object() || value.is_null() => {
                            redact_observation(value)
                        }
                        _ if is_sensitive_key(key) => Value::String("***".into()),
                        _ => redact_observation(value),
                    };
                    (key.clone(), safe)
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(redact_observation).collect()),
        Value::String(text) => Value::String(redact_str(text)),
        other => other.clone(),
    }
}

/// A tool may echo an opaque credential without its key name. Carry sensitive input
/// values only for this call's observation formatting, never into the audit record.
pub(crate) struct ToolRedactor {
    secrets: Vec<String>,
}

impl ToolRedactor {
    pub(crate) fn from_args(args: &Value) -> Self {
        fn collect(value: &Value, sensitive: bool, secrets: &mut Vec<String>) {
            match value {
                Value::String(text) if sensitive && !text.is_empty() => secrets.push(text.clone()),
                Value::Object(fields) => {
                    for (key, value) in fields {
                        collect(value, sensitive || is_sensitive_key(key), secrets);
                    }
                }
                Value::Array(items) => {
                    for value in items {
                        collect(value, sensitive, secrets);
                    }
                }
                _ => {}
            }
        }
        let mut secrets = Vec::new();
        collect(args, false, &mut secrets);
        secrets.sort_unstable_by_key(|secret| std::cmp::Reverse(secret.len()));
        secrets.dedup();
        Self { secrets }
    }

    pub(crate) fn text(&self, text: &str) -> String {
        redact_str(
            &self
                .secrets
                .iter()
                .fold(text.to_owned(), |text, secret| text.replace(secret, "***")),
        )
    }

    pub(crate) fn result(&self, mut result: super::ToolResult) -> super::ToolResult {
        result.output = self.text(&result.output);
        result.error = result.error.as_deref().map(|error| self.text(error));
        result
    }
}

/// JSON 载荷脱敏（事件 args / 审计 entry 通用）：
/// ① 敏感键的值整体掩码 ② 字符串值内联密钥形态打码
pub fn redact_json(v: &Value) -> Value {
    match v {
        Value::Object(m) => Value::Object(
            m.iter()
                .map(|(k, val)| {
                    let nv = if is_sensitive_key(k) {
                        Value::String("***".to_string())
                    } else {
                        redact_value(val)
                    };
                    (k.clone(), nv)
                })
                .collect(),
        ),
        Value::Array(a) => Value::Array(a.iter().map(redact_json).collect()),
        Value::String(s) => Value::String(redact_str(s)),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn 键名脱敏() {
        let v = json!({ "password": "hunter2", "api_key": "k-123", "path": "/tmp/x" });
        let r = redact_json(&v);
        assert_eq!(r["password"], "***");
        assert_eq!(r["api_key"], "***");
        assert_eq!(r["path"], "/tmp/x", "非敏感键不动");
    }

    #[test]
    fn 嵌套与数组脱敏() {
        let v = json!({
            "command": "curl -H 'Authorization: Bearer abcdefghijklmnop' https://x.io",
            "nested": { "token": "t-1", "list": ["sk-abcdefgh12345678", "plain"] }
        });
        let r = redact_json(&v);
        assert!(r["command"].as_str().unwrap().contains("Bearer ***"));
        assert!(!r["command"].as_str().unwrap().contains("abcdefghijklmnop"));
        assert_eq!(r["nested"]["token"], "***");
        assert_eq!(r["nested"]["list"][0], "sk-***");
        assert_eq!(r["nested"]["list"][1], "plain");
    }

    #[test]
    fn 内联密钥形态() {
        let r = redact_str(
            "export KEY=sk-proj-abcdefgh1234567890 && curl -H 'x: AKIAIOSFODNN7EXAMPLE' x",
        );
        assert!(r.contains("sk-***"), "{r}");
        assert!(r.contains("AKIA***"), "{r}");
        assert!(!r.contains("abcdefgh1234567890"));
    }

    #[test]
    fn 错误与结果中的键值文本脱敏() {
        for input in [
            r#"operation failed: api_key="e2e-sensitive-canary-91b7""#,
            "request rejected: Authorization: Bearer e2e-sensitive-canary-91b7",
            "token=e2e-sensitive-canary-91b7",
            "password='words with spaces'",
        ] {
            let redacted = redact_str(input);
            assert!(
                !redacted.contains("e2e-sensitive-canary-91b7"),
                "{redacted}"
            );
            assert!(!redacted.contains("words with spaces"), "{redacted}");
            assert!(redacted.contains("***"), "{redacted}");
        }
        assert_eq!(
            redact_str("token budget: 20 calls"),
            "token budget: 20 calls",
            "Ordinary prose following a key-like word should remain intact",
        );
    }

    #[test]
    fn 观察脱敏保留身份与用量但不放宽参数() {
        let value = json!({
            "sessionId": "session-a", "session_id": "session-b",
            "promptTokens": 13, "completionTokens": 5,
            "tokenUsage": { "prompt": 13, "completion": 5 },
            "args": { "session_id": "private-cookie", "promptTokens": "opaque-secret" },
            "authorizationRevision": "revision-2", "apiKey": "private-key"
        });
        let safe = redact_observation(&value);
        assert_eq!(safe["sessionId"], "session-a");
        assert_eq!(safe["session_id"], "session-b");
        assert_eq!(safe["promptTokens"], 13);
        assert_eq!(safe["completionTokens"], 5);
        assert_eq!(safe["tokenUsage"], value["tokenUsage"]);
        assert_eq!(safe["authorizationRevision"], "revision-2");
        assert_eq!(safe["args"]["session_id"], "***");
        assert_eq!(safe["args"]["promptTokens"], "***");
        assert_eq!(safe["apiKey"], "***");
    }

    #[test]
    fn 结果中的不透明凭据按本次参数替换() {
        let redactor = ToolRedactor::from_args(&json!({"token": "opaque-fixture-value"}));
        let result = redactor.result(super::super::ToolResult::err("echo opaque-fixture-value"));
        assert_eq!(result.error.as_deref(), Some("echo ***"));
    }

    #[test]
    fn 观察脱敏保留压缩计数与可选预算的类型() {
        let value = json!({
            "beforeTokens": 100, "afterTokens": 20, "tokenBudget": 50_000,
            "token_budget": null,
            "args": { "beforeTokens": 100, "tokenBudget": 50_000 }
        });
        let safe = redact_observation(&value);
        assert_eq!(safe["beforeTokens"], 100);
        assert_eq!(safe["afterTokens"], 20);
        assert_eq!(safe["tokenBudget"], 50_000);
        assert!(safe["token_budget"].is_null());
        assert_eq!(safe["args"]["beforeTokens"], "***");
        assert_eq!(safe["args"]["tokenBudget"], "***");
        let secret = redact_observation(&json!({"tokenBudget": "opaque-fixture-secret"}));
        assert_eq!(secret["tokenBudget"], "***");
    }

    #[test]
    fn 普通文本不误伤() {
        let r = redact_str("cargo build --release && echo done");
        assert_eq!(r, "cargo build --release && echo done", "普通命令不动");
        // 40 字符阈值：普通 URL path 段可能长但含 / : 等分隔符不在 base64ish 集内
        let r2 = redact_str("https://example.com/very/long/path/segments/here");
        assert_eq!(r2, "https://example.com/very/long/path/segments/here");
    }

    #[test]
    fn 复合键形态() {
        let v = json!({ "--api-key": "abc123", "X-API-Key": "k" });
        let r = redact_json(&v);
        assert_eq!(r["--api-key"], "***");
        assert_eq!(r["X-API-Key"], "***");
    }
}
