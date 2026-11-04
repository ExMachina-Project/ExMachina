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
    let mut out = s.to_string();
    for (pat, mask) in inline_patterns() {
        if let Ok(re) = regex::Regex::new(pat) {
            out = re.replace_all(&out, mask.to_string()).to_string();
        }
    }
    out
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
        let r = redact_str("export KEY=sk-proj-abcdefgh1234567890 && curl -H 'x: AKIAIOSFODNN7EXAMPLE' x");
        assert!(r.contains("sk-***"), "{r}");
        assert!(r.contains("AKIA***"), "{r}");
        assert!(!r.contains("abcdefgh1234567890"));
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
