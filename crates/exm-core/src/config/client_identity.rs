//! 提供商连接使用的客户端标识；仅影响请求头，不改变协议、鉴权或消息。

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

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClientIdentity {
    #[serde(default)]
    pub preset: ClientIdentityPreset,
    /// 非空时覆盖预设的 User-Agent；默认身份忽略全部覆盖值。
    #[serde(default)]
    pub user_agent: String,
    /// 客户端标识头；名称不区分大小写，同名覆盖预设。
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
}

impl ClientIdentity {
    /// 默认值不写入旧配置，保持升级前的连接行为。
    pub fn is_default(&self) -> bool {
        self == &Self::default()
    }

    pub fn request_headers(&self) -> anyhow::Result<HeaderMap> {
        if self.preset == ClientIdentityPreset::Default {
            anyhow::ensure!(
                self.user_agent.is_empty() && self.headers.is_empty(),
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
                    | "x-app"
                    | "x-title"
                    | "http-referer"
                    | "editor-version"
                    | "editor-plugin-version"
            ) || name
                .as_str()
                .strip_prefix("x-client-")
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
        let (agent, preset_headers): (&str, &[(&str, &str)]) = match self.preset {
            ClientIdentityPreset::Default => unreachable!("default client identity returned early"),
            ClientIdentityPreset::ClaudeCode => ("claude-cli (external, cli)", &[("x-app", "cli")]),
            ClientIdentityPreset::Codex => ("codex_cli_rs", &[("originator", "codex_cli_rs")]),
            ClientIdentityPreset::Opencode => (
                "opencode",
                &[
                    ("http-referer", "https://opencode.ai/"),
                    ("x-title", "opencode"),
                ],
            ),
            ClientIdentityPreset::Cline => (
                "Cline",
                &[("http-referer", "https://cline.bot"), ("x-title", "Cline")],
            ),
            ClientIdentityPreset::Custom => ("", &[]),
        };
        if !agent.is_empty() {
            headers.insert(USER_AGENT, HeaderValue::from_static(agent));
        }
        for &(name, value) in preset_headers {
            headers.insert(
                HeaderName::from_static(name),
                HeaderValue::from_static(value),
            );
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
