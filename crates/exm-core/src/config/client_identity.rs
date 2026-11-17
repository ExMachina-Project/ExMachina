//! 提供商客户端兼容信息：原生请求模式与仅请求头覆盖共用同一预设。

use reqwest::header::{HeaderMap, HeaderName, HeaderValue, USER_AGENT};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ClientIdentityPreset {
    #[default]
    Default,
    ClaudeCode,
    Codex,
    Opencode,
    Cline,
    Custom,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ClientRequestMode {
    #[default]
    Native,
    Headers,
}

impl ClientRequestMode {
    fn is_native(&self) -> bool {
        *self == Self::Native
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClientIdentity {
    #[serde(default)]
    pub preset: ClientIdentityPreset,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub version: String,
    #[serde(default, skip_serializing_if = "ClientRequestMode::is_native")]
    pub request_mode: ClientRequestMode,
    /// 非空时覆盖预设的 User-Agent；默认身份不允许覆盖值。
    #[serde(default)]
    pub user_agent: String,
    /// 客户端标识头；名称不区分大小写，同名覆盖预设。
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
}

impl ClientIdentity {
    pub fn client_version(&self) -> &str {
        if !self.version.is_empty() {
            return &self.version;
        }
        match self.preset {
            ClientIdentityPreset::ClaudeCode => "2.1.326",
            ClientIdentityPreset::Codex => "0.169.0",
            ClientIdentityPreset::Opencode => "2.3.0",
            ClientIdentityPreset::Cline => "4.1.26",
            _ => "",
        }
    }

    pub fn effective_format<'a>(&self, configured: &'a str) -> &'a str {
        let configured = match configured {
            "anthropic" | "azure" | "gemini" | "responses" => configured,
            _ => "openai",
        };
        if self.request_mode == ClientRequestMode::Headers {
            return configured;
        }
        match (self.preset, configured) {
            (ClientIdentityPreset::Codex, "openai" | "responses") => "responses",
            (ClientIdentityPreset::ClaudeCode, "openai" | "responses" | "anthropic") => "anthropic",
            _ => configured,
        }
    }

    /// 默认值不写入旧配置，保持升级前的连接行为。
    pub fn is_default(&self) -> bool {
        self == &Self::default()
    }

    pub fn request_headers(&self) -> anyhow::Result<HeaderMap> {
        anyhow::ensure!(
            self.version.len() <= 64
                && self
                    .version
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b".-_+".contains(&b)),
            "客户端版本仅允许字母、数字和 .-_+，最多 64 字节"
        );
        if self.preset == ClientIdentityPreset::Default {
            anyhow::ensure!(
                self.user_agent.is_empty() && self.headers.is_empty() && self.version.is_empty(),
                "默认客户端身份不能包含覆盖值"
            );
            return Ok(HeaderMap::new());
        }
        anyhow::ensure!(self.headers.len() <= 32, "客户端标识头最多 32 个");
        let mut custom = HeaderMap::new();
        for (name, value) in &self.headers {
            let name = HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| anyhow::anyhow!("客户端标识头名称无效"))?;
            let allowed = matches!(
                name.as_str(),
                "originator"
                    | "version"
                    | "x-app"
                    | "x-title"
                    | "x-openrouter-title"
                    | "x-openrouter-categories"
                    | "http-referer"
                    | "editor-version"
                    | "editor-plugin-version"
                    | "anthropic-beta"
                    | "anthropic-dangerous-direct-browser-access"
                    | "x-claude-code-session-id"
                    | "x-session-id"
                    | "x-session-affinity"
                    | "session_id"
                    | "x-task-id"
                    | "x-is-multiroot"
                    | "x-platform"
                    | "x-platform-version"
                    | "x-core-version"
            ) || name
                .as_str()
                .strip_prefix("x-client-")
                .is_some_and(|s| !s.is_empty())
                || name
                    .as_str()
                    .strip_prefix("x-opencode-")
                    .is_some_and(|s| !s.is_empty())
                || name
                    .as_str()
                    .strip_prefix("x-stainless-")
                    .is_some_and(|s| !s.is_empty());
            anyhow::ensure!(allowed, "该请求头不能用作客户端标识：{name}");
            anyhow::ensure!(!custom.contains_key(&name), "客户端标识头重复：{name}");
            custom.insert(name, Self::header_value(value)?);
        }
        let user_agent = if self.user_agent.is_empty() {
            None
        } else {
            Some(Self::header_value(&self.user_agent)?)
        };
        let mut headers = HeaderMap::new();
        let version = self.client_version();
        let (agent, preset_headers): (String, &[(&str, &str)]) = match self.preset {
            ClientIdentityPreset::Default => unreachable!("default client identity returned early"),
            ClientIdentityPreset::ClaudeCode => (
                format!("claude-cli/{version} (external, cli)"),
                &[("x-app", "cli"), ("anthropic-beta", "claude-code-20250219")],
            ),
            ClientIdentityPreset::Codex => (
                format!(
                    "codex_cli_rs/{version} ({}; {})",
                    std::env::consts::OS,
                    std::env::consts::ARCH
                ),
                &[("originator", "codex_cli_rs")],
            ),
            ClientIdentityPreset::Opencode => (
                format!("opencode/{version}"),
                &[
                    ("http-referer", "https://opencode.ai/"),
                    ("x-title", "opencode"),
                ],
            ),
            ClientIdentityPreset::Cline => (
                format!("Cline/{version}"),
                &[
                    ("http-referer", "https://cline.bot"),
                    ("x-title", "Cline"),
                    ("x-client-type", "cline-cli"),
                    ("x-is-multiroot", "false"),
                ],
            ),
            ClientIdentityPreset::Custom => (String::new(), &[]),
        };
        if !agent.is_empty() {
            headers.insert(USER_AGENT, Self::header_value(&agent)?);
        }
        for &(name, value) in preset_headers {
            headers.insert(
                HeaderName::from_static(name),
                HeaderValue::from_static(value),
            );
        }
        if self.preset == ClientIdentityPreset::ClaudeCode {
            for (name, value) in [
                ("x-stainless-lang", "js"),
                ("x-stainless-runtime", "node"),
                (
                    "x-stainless-os",
                    match std::env::consts::OS {
                        "macos" => "MacOS",
                        "windows" => "Windows",
                        _ => "Linux",
                    },
                ),
                (
                    "x-stainless-arch",
                    match std::env::consts::ARCH {
                        "x86_64" => "x64",
                        "aarch64" => "arm64",
                        arch => arch,
                    },
                ),
                ("x-stainless-retry-count", "0"),
                ("x-stainless-timeout", "600"),
            ] {
                headers.insert(HeaderName::from_static(name), Self::header_value(value)?);
            }
        }
        if self.preset == ClientIdentityPreset::Cline {
            headers.insert("x-client-version", Self::header_value(version)?);
            headers.insert("x-core-version", Self::header_value(version)?);
            headers.insert("x-platform", HeaderValue::from_static("cli"));
            headers.insert("x-platform-version", Self::header_value(version)?);
        }
        headers.extend(custom);
        if let Some(value) = user_agent {
            headers.insert(USER_AGENT, value);
        }
        Ok(headers)
    }

    fn header_value(value: &str) -> anyhow::Result<HeaderValue> {
        anyhow::ensure!(
            value.len() <= 2048 && value.bytes().all(|b| (0x20..=0x7e).contains(&b)),
            "客户端标识值须为单行 ASCII 文本，最多 2048 字节"
        );
        HeaderValue::from_str(value).map_err(|_| anyhow::anyhow!("客户端标识值无效"))
    }
}
