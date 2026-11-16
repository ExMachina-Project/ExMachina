//! 模型设置的机器契约；与 llm_admin 的部分更新和密钥掩码语义保持一致。

use super::Endpoint;
use serde_json::{json, Value};

fn reference(name: &str) -> Value {
    json!({ "$ref": format!("#/components/schemas/{name}") })
}

fn nullable(schema: Value) -> Value {
    json!({ "anyOf": [schema, { "type": "null" }] })
}

fn identity_example() -> Value {
    json!({ "preset": "claude-code", "userAgent": "my-agent/1.0", "headers": { "x-app": "cli" } })
}

fn model_example() -> Value {
    json!({ "model": "example-model", "vision": true, "audio": false, "contextWindow": 128000,
        "purposes":["chat","coding","review","vision"],"cost":"balanced","usageNotes":"适合一般对话与代码检查。","autoEligible":false })
}

pub(super) fn describe(ep: Endpoint) -> Endpoint {
    match (ep.method, ep.path) {
        ("get", "/api/llm/profiles") => ep.json_response("LlmProfiles", json!({
            "active": "primary", "mock": false,
            "profiles": [{
                "id": "primary", "name": "Primary", "baseUrl": "https://example.invalid/v1",
                "apiFormat": "openai", "apiKey": "***已配置***", "apiKeys": [],
                "model": "example-model", "models": [model_example()], "fallback": null,
                "clientIdentity": identity_example(),
            }],
        })),
        ("post", "/api/llm/profiles") => ep
            .json_request("LlmProfileInput", json!({
                "id": "primary", "name": "Primary", "baseUrl": "https://example.invalid/v1",
                "apiFormat": "openai", "apiKey": "***已配置***", "apiKeys": [{ "keep": 0 }],
                "model": "example-model", "models": [model_example()],
                "clientIdentity": identity_example(),
            }))
            .json_response("LlmProfileSaved", json!({ "ok": true, "id": "primary", "mock": false })),
        ("delete", "/api/llm/profiles/:id") => ep
            .json_response("LlmProfileDeleted", json!({ "ok": true, "active": "primary" })),
        ("put", "/api/llm/active") => ep
            .json_request("LlmProfileSelection", json!({ "id": "primary" }))
            .json_response("LlmProfileActivated", json!({ "ok": true, "active": "primary", "mock": false })),
        ("post", "/api/llm/test") => ep
            .json_request("LlmTestInput", json!({ "id": "primary" }))
            .json_response("LlmTestResult", json!({ "ok": true, "status": 200, "latencyMs": 120, "snippet": "upstream response" })),
        ("post", "/api/llm/models") => ep
            .json_request("LlmModelsInput", json!({ "id": "primary", "clientIdentity": identity_example() }))
            .json_response("LlmModelsResult", json!({ "ok": true, "models": ["example-model"] })),
        ("get", "/api/llm/capabilities") => ep
            .json_response("LlmCapabilities", json!({ "speech": "primary/tts-1", "transcribe": "", "visionRelay": "", "embedding": "" })),
        ("put", "/api/llm/capabilities") => ep
            .json_request("LlmCapabilitiesInput", json!({ "speech": "primary/tts-1", "embedding": "" }))
            .json_response("LlmCapabilitiesSaved", json!({ "ok": true, "speech": "primary/tts-1", "transcribe": "", "visionRelay": "", "embedding": "" })),
        ("get", "/api/llm/effort") => ep
            .json_response("LlmEffort", json!({ "effort": null, "reasoningTokenCap": null, "effectiveCap": 16384 })),
        ("put", "/api/llm/effort") => ep
            .json_request("LlmEffortInput", json!({ "effort": "low", "reasoningTokenCap": 8192 }))
            .json_response("LlmEffortSaved", json!({ "ok": true, "effort": "low", "reasoningTokenCap": 8192, "effectiveCap": 8192 })),
        _ => ep,
    }
}

fn header_name_pattern() -> String {
    let names = [
        "originator",
        "x-app",
        "x-title",
        "http-referer",
        "editor-version",
        "editor-plugin-version",
    ];
    let mut patterns: Vec<String> = names.iter().map(|name| case_insensitive(name)).collect();
    for prefix in ["x-client-", "x-stainless-"] {
        patterns.push(format!(
            "{}[!#$%&'*+.^_`|~0-9A-Za-z-]+",
            case_insensitive(prefix)
        ));
    }
    format!("^({})$", patterns.join("|"))
}

fn case_insensitive(value: &str) -> String {
    value
        .chars()
        .map(|c| {
            if c.is_ascii_alphabetic() {
                format!("[{c}{}]", c.to_ascii_uppercase())
            } else {
                c.to_string()
            }
        })
        .collect()
}

pub(super) fn schemas() -> Value {
    let string = json!({ "type": "string" });
    let optional_string = nullable(string.clone());
    let format = json!({
        "type": "string", "examples": ["openai", "anthropic", "azure", "gemini"],
        "description": "API 协议；空值或未识别字符串按 OpenAI 兼容协议处理。",
    });
    let ascii = json!({ "type": "string", "maxLength": 2048, "not": { "pattern": "[^ -~]" } });
    let capabilities = json!({
        "speech": string, "transcribe": string, "visionRelay": string, "embedding": string,
    });
    let effort = json!({
        "effort": { "type": ["string", "null"] },
        "reasoningTokenCap": { "type": ["integer", "null"], "minimum": 1024, "maximum": u32::MAX },
        "effectiveCap": { "type": "integer", "minimum": 1024, "maximum": u32::MAX },
    });
    json!({
        "Error": {
            "type": "object", "required": ["error"],
            "properties": { "error": { "type": "string" } },
        },
        "ClientIdentity": {
            "type": "object", "additionalProperties": false,
            "description": "提供商身份；省略或 null 表示沿用。默认身份的覆盖值必须为空。请求头名不区分大小写，同名覆盖预设，大小写重复由服务端拒绝。",
            "properties": {
                "preset": { "type": "string", "enum": ["default", "claude-code", "codex", "opencode", "cline", "custom"], "default": "default" },
                "userAgent": ascii,
                "headers": {
                    "type": "object", "maxProperties": 32,
                    "propertyNames": { "pattern": header_name_pattern(), "not": { "pattern": "[^!#$%&'*+.^_`|~0-9A-Za-z-]" } },
                    "additionalProperties": ascii,
                },
            },
            "if": { "properties": { "preset": { "const": "default" } } },
            "then": { "properties": { "userAgent": { "maxLength": 0 }, "headers": { "maxProperties": 0 } } },
            "examples": [{ "preset": "default" }, identity_example()],
        },
        "ProfileModelInput": {
            "type": "object", "required": ["model"],
            "properties": {
                "model": string, "vision": { "type": "boolean", "default": false },
                "audio": { "type": "boolean", "default": false },
                "contextWindow": { "type": ["integer", "null"], "minimum": 0, "maximum": u64::MAX },
                "purposes": {"type":["array","null"],"maxItems":11,"items":reference("ModelPurpose")},
                "cost":nullable(reference("ModelCost")),"usageNotes":{"type":["string","null"],"maxLength":2000},"autoEligible":{"type":["boolean","null"]},
            },
        },
        "ProfileModel": {
            "type": "object", "required": ["model", "vision", "audio", "contextWindow"],
            "properties": {
                "model": string, "vision": { "type": "boolean" }, "audio": { "type": "boolean" },
                "contextWindow": { "type": "integer", "minimum": 0, "maximum": u64::MAX },
                "purposes":{"type":"array","items":reference("ModelPurpose")},"cost":reference("ModelCost"),"usageNotes":{"type":"string","maxLength":2000},"autoEligible":{"type":"boolean"},
            },
        },
        "LlmProfileInput": {
            "type": "object",
            "description": "部分更新；缺省/null 沿用旧值。新档案需非空 baseUrl；id 省略时自动生成。",
            "properties": {
                "id": optional_string, "name": optional_string, "baseUrl": optional_string,
                "apiFormat": nullable(format.clone()),
                "apiKey": { "type": ["string", "null"], "writeOnly": true,
                    "description": "空/null/掩码 ***已配置*** 沿用；__CLEAR__ 清除；其他字符串替换主密钥。" },
                "apiKeys": { "type": ["array", "null"], "description": "空数组或 null 沿用；__CLEAR__ 删除一项；keep 为旧池 0 起索引。",
                    "items": { "oneOf": [
                        { "type": "string", "writeOnly": true },
                        { "type": "object", "required": ["keep"], "properties": { "keep": { "type": "integer", "minimum": 0 } } },
                    ] } },
                "clearKeys": { "type": "boolean", "default": false, "description": "true 清空主密钥和密钥池，优先于沿用引用。" },
                "model": optional_string,
                "models": { "type": ["array", "null"], "items": reference("ProfileModelInput") },
                "fallback": { "type": ["string", "null"], "description": "缺省/null 沿用；空串清除。" },
                "clientIdentity": nullable(reference("ClientIdentity")),
            },
        },
        "LlmProfile": {
            "type": "object", "required": ["id", "name", "baseUrl", "apiFormat", "apiKey", "apiKeys", "model", "models", "fallback", "clientIdentity"],
            "properties": {
                "id": string, "name": string, "baseUrl": string, "apiFormat": format,
                "apiKey": { "type": "string", "enum": ["", "***已配置***"] },
                "apiKeys": { "type": "array", "items": { "type": "string", "enum": ["", "***已配置***"] } },
                "model": string, "models": { "type": "array", "items": reference("ProfileModel") },
                "fallback": optional_string, "clientIdentity": reference("ClientIdentity"),
            },
        },
        "LlmProfiles": {
            "type": "object", "required": ["active", "mock", "profiles"],
            "properties": { "active": string, "mock": { "type": "boolean" }, "profiles": { "type": "array", "items": reference("LlmProfile") } },
        },
        "LlmProfileSelection": { "type": "object", "required": ["id"], "properties": { "id": string } },
        "LlmProfileSaved": { "type": "object", "required": ["ok", "id", "mock"], "properties": {
            "ok": { "const": true }, "id": string, "mock": { "type": "boolean" },
        } },
        "LlmProfileActivated": { "type": "object", "required": ["ok", "active", "mock"], "properties": {
            "ok": { "const": true }, "active": string, "mock": { "type": "boolean" },
        } },
        "LlmProfileDeleted": { "type": "object", "required": ["ok"], "properties": {
            "ok": { "const": true }, "active": string,
        } },
        "LlmTestInput": { "type": "object", "properties": { "id": optional_string },
            "description": "id 省略/null 测试当前配置，否则测试该已保存档案；其他连接字段不覆盖档案。" },
        "LlmTestResult": { "type": "object", "required": ["ok"], "properties": {
            "ok": { "type": "boolean" }, "configured": { "const": false },
            "status": { "type": "integer", "minimum": 100, "maximum": 599 },
            "latencyMs": { "type": "integer", "minimum": 0 }, "snippet": { "type": "string", "maxLength": 300 },
            "message": string, "error": string,
        }, "description": "上游非 2xx 或网络失败以 HTTP 200 / ok:false 返回；status 表示上游状态码。" },
        "LlmModelsInput": { "type": "object", "properties": {
            "id": optional_string, "baseUrl": optional_string, "apiKey": { "type": ["string", "null"], "writeOnly": true },
            "apiFormat": nullable(format), "clientIdentity": nullable(reference("ClientIdentity")),
        }, "anyOf": [
            { "required": ["id"], "properties": { "id": { "type": "string" } } },
            { "required": ["baseUrl"], "properties": { "baseUrl": { "type": "string", "minLength": 1 } } },
        ], "description": "有 id 时用已保存端点/密钥/协议，可临时覆盖身份；无 id 时需 baseUrl；缺少 apiKey 返回 configured:false。探测不落盘。" },
        "LlmModelsResult": { "type": "object", "required": ["ok", "models"], "properties": {
            "ok": { "type": "boolean" }, "models": { "type": "array", "items": string },
            "configured": { "const": false }, "status": { "type": "integer", "minimum": 100, "maximum": 599 },
            "message": string, "error": string,
        } },
        "LlmCapabilitiesInput": { "type": "object", "properties": {
            "speech": optional_string, "transcribe": optional_string, "visionRelay": optional_string, "embedding": optional_string,
        }, "description": "值为档案ID或档案ID/模型名；缺省/null 沿用，空串清除。" },
        "LlmCapabilities": { "type": "object", "required": ["speech", "transcribe", "visionRelay", "embedding"], "properties": capabilities },
        "LlmCapabilitiesSaved": { "allOf": [reference("LlmCapabilities"), {
            "type": "object", "required": ["ok"], "properties": { "ok": { "const": true } },
        }] },
        "LlmEffortInput": { "type": "object", "properties": {
            "effort": { "type": ["string", "null"], "description": "low/medium/high；其他字符串归一 medium；省略/null 沿用。" },
            "reasoningTokenCap": { "type": ["integer", "null"], "minimum": 0, "maximum": u32::MAX,
                "description": "省略/null 沿用；<1024 清除覆盖并使用默认安全网 16384。" },
        } },
        "LlmEffort": { "type": "object", "required": ["effort", "reasoningTokenCap", "effectiveCap"], "properties": effort },
        "LlmEffortSaved": { "allOf": [reference("LlmEffort"), {
            "type": "object", "required": ["ok"], "properties": { "ok": { "const": true } },
        }] },
    })
}
