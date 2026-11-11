//! config 域分文件：插件系统（PluginManifest/PluginsConfig）。

use super::*;
use serde::{Deserialize, Serialize};

/// 插件清单（plugins/<id>/plugin.json）：能力包的打包与安装单元。
/// 四类能力（tools/channel/watch/resources）全部可选但至少声明一类；
/// 形态由声明决定——内联 tools/resources = 声明式（零代码零进程）、
/// process 字段 = 进程插件（stdio JSON-RPC）、builtin = 宿主内置实现（清单只做声明）。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PluginManifest {
    /// 插件 id（目录名；字母开头，字母/数字/-/_；全局唯一）
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub description: String,
    /// 最低宿主版本（SemVer；缺省 = 兼容）
    #[serde(default)]
    pub exm_min: String,
    /// 工具组：内联 HTTP 工具声明（CustomTool 字面形状，零代码）；
    /// process 形态时留空经协议 tools/list 发现
    #[serde(default)]
    pub tools: Vec<CustomTool>,
    /// 通道适配声明（平台 kind 与凭据字段清单）
    #[serde(default)]
    pub channel: Option<PluginChannelDecl>,
    /// 自动化 watcher 轮询声明
    #[serde(default)]
    pub watch: Option<PluginWatchDecl>,
    /// 资源注入：技能/提示词文件的相对路径清单（相对插件目录）
    #[serde(default)]
    pub resources: Vec<String>,
    /// 进程插件声明（存在即 process 形态）
    #[serde(default)]
    pub process: Option<PluginProcessDecl>,
}

/// 通道能力声明：凭据字段清单供 WebUI 通道配置页按清单渲染
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PluginChannelDecl {
    /// 平台 kind 列表（一个插件可适配多平台，如 video 插件的 bilibili/youtube）
    #[serde(default)]
    pub kinds: Vec<String>,
    /// 凭据字段（字段名/标签/是否敏感）
    #[serde(default)]
    pub credentials: Vec<PluginCredentialField>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PluginCredentialField {
    /// 配置键名（存 channel.config）
    pub key: String,
    /// 展示标签
    #[serde(default)]
    pub label: String,
    /// 敏感字段（管理端点/日志/诊断一律脱敏）
    #[serde(default)]
    pub sensitive: bool,
}

/// watcher 轮询声明
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PluginWatchDecl {
    /// 轮询间隔分钟（实现可按通道配置覆盖）
    #[serde(default = "default_watch_interval")]
    pub interval_minutes: u64,
}

/// 进程插件声明：宿主按 command 拉起子进程，stdio JSON-RPC 通信
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PluginProcessDecl {
    /// 可执行命令（argv；相对路径相对插件目录）
    #[serde(default)]
    pub command: Vec<String>,
    /// 附加环境变量
    #[serde(default)]
    pub env: std::collections::BTreeMap<String, String>,
    /// initialize 握手超时（秒；默认 15）
    #[serde(default = "default_handshake_secs")]
    pub handshake_secs: u64,
    /// 单次工具调用超时（秒；默认 60，上限 300）
    #[serde(default = "default_call_timeout_secs")]
    pub call_timeout_secs: u64,
    /// 并发调用上限（超出排队；默认 4）
    #[serde(default = "default_plugin_concurrency")]
    pub max_concurrency: usize,
}

impl PluginManifest {
    /// id 合法（字母开头，字母/数字/-/_；即目录名口径）
    pub fn valid_id(&self) -> bool {
        let id = self.id.trim();
        !id.is_empty()
            && id.len() <= 48
            && id.starts_with(|c: char| c.is_ascii_alphabetic())
            && id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    }

    /// 至少声明一类能力（tools/channel/watch/resources 任一非空）
    pub fn has_any_capability(&self) -> bool {
        !self.tools.is_empty()
            || self.channel.is_some()
            || self.watch.is_some()
            || !self.resources.is_empty()
            // process 形态：工具在握手后动态发现，清单天然无静态能力声明
            || self.is_process()
    }

    /// 是否进程形态（声明了 process 且给了可执行命令）
    pub fn is_process(&self) -> bool {
        self.process
            .as_ref()
            .map(|p| !p.command.is_empty())
            .unwrap_or(false)
    }
}

/// 插件启用状态持久化（config.plugins.state：id → enabled；缺省 = 启用）
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PluginsConfig {
    /// 运行状态覆盖：false = 禁用（清单初始态之上的人为覆盖）
    #[serde(default)]
    pub state: std::collections::BTreeMap<String, bool>,
    /// 插件级凭据区（插件id → 键值；T1 声明式工具 `${credentials.<key>}` 变量解析
    /// 与 T2 进程插件 config/set 下发的数据源）。管理端点回显一律掩码。
    #[serde(default)]
    pub credentials: std::collections::BTreeMap<String, std::collections::BTreeMap<String, String>>,
    /// 安装来源记录（插件id → 来源；更新流解析用。手工拷入的插件无记录 = 无法更新）
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub installations: std::collections::BTreeMap<String, PluginInstallation>,
    /// 插件索引地址（缺省官方源；自定义源需自担信任）
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub index_url: String,
}

/// 插件安装来源（plugin-ecosystem：更新流按此解析最新版）
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PluginInstallation {
    /// targz | git
    pub kind: String,
    /// 包 URL 或 git 仓库地址
    #[serde(default)]
    pub url: String,
    /// git 子目录（清单在仓库子目录时）
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub subdir: String,
    /// git ref（分支/标签；缺省默认分支）
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub r#ref: String,
}

