//! 插件系统（plugins/ 目录）：统一能力包契约。
//!
//! 四层扩展模型共用同一清单与注册面：
//! - T1 声明式（plugin.json 内联 HTTP 工具 / 资源，零代码零进程）
//! - T2 进程插件（stdio JSON-RPC，任意语言；见 plugin_bridge.rs）
//! - T3 内置插件（Rust trait 实现，静态注册；video 是第一个）
//! - T0（entities / config CustomTool）收编为内置虚拟插件，行为不变
//!
//! 装载顺序：清单扫描 → 校验（id/能力/版本）→ 按形态构造 provider →
//! 注册进 ToolGateway / 通道工厂 / watcher 调度 / skills 装载。

pub mod bridge;

use crate::config::{CustomTool, PluginManifest};
use serde_json::Value;

#[cfg(test)]
use serde_json::json;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

// ---------------------------------------------------------------- 能力 trait

/// 工具组能力：插件向会话工具面注入工具（经命名空间前缀注册）
pub trait ToolProvider: Send + Sync {
    /// 工具定义（名字已含命名空间前缀 `<plugin>__<tool>`）
    fn tool_specs(&self) -> Vec<CustomTool>;
    /// 执行（name 为命名空间全名）；返回工具结果文本或错误
    fn call_tool(&self, name: &str, args: &Value) -> anyhow::Result<String>;
}

/// 通道适配能力：声明平台 kind 与凭据面，供网关枚举 spawn
pub trait ChannelProvider: Send + Sync {
    /// 适配的平台 kind 清表（如 video 插件的 bilibili/youtube）
    fn kinds(&self) -> Vec<String>;
    /// 凭据字段清单（WebUI 通道配置页按此渲染）
    fn credential_fields(&self) -> Vec<crate::config::PluginCredentialField>;
}

/// watcher 生命周期能力：宿主统一调度启动/停止
pub trait WatchProvider: Send + Sync {
    /// 启动轮询循环（实现自管理节奏；停止由 abort/取消信号控制）
    fn spawn(self: Arc<Self>, core: Arc<crate::Core>);
}

/// 资源注入能力：技能/提示词文件装载（与 entities/skills 同格式）
pub trait ResourceProvider: Send + Sync {
    /// 资源文件绝对路径清单（相对插件目录展开后）
    fn resource_paths(&self) -> Vec<PathBuf>;
}

/// 插件 provider 总契约：一种形态（builtin/process/声明式）一个实现
pub trait PluginProvider: Send + Sync {
    fn manifest(&self) -> &PluginManifest;
    fn tool_provider(&self) -> Option<Arc<dyn ToolProvider>> {
        None
    }
    fn channel_provider(&self) -> Option<Arc<dyn ChannelProvider>> {
        None
    }
    fn watch_provider(&self) -> Option<Arc<dyn WatchProvider>> {
        None
    }
    fn resource_provider(&self) -> Option<Arc<dyn ResourceProvider>> {
        None
    }
}

// ---------------------------------------------------------------- 装载与注册

/// 单个已装载插件的运行态快照
#[derive(Debug, Clone)]
pub struct PluginEntry {
    pub manifest: PluginManifest,
    /// builtin | process | declarative | virtual（virtual = 收编的既有机制，仅呈现层登记）
    pub form: String,
    pub enabled: bool,
    /// 插件目录（清单所在目录；内置插件 = plugins/<id>/ 或 None）
    pub dir: Option<PathBuf>,
    /// 装载拒绝/运行错误原因（None = 正常）
    pub error: Option<String>,
}

/// 插件管理器：扫描 plugins/ 目录、校验清单、持注册表与启停状态
pub struct PluginManager {
    entries: parking_lot::RwLock<BTreeMap<String, PluginEntry>>,
    providers: parking_lot::RwLock<BTreeMap<String, Arc<dyn PluginProvider>>>,
    /// builtin 静态注册表（编译期登记；video 是第一个）
    builtin: BTreeMap<String, (PluginManifest, fn() -> Option<Arc<dyn PluginProvider>>)>,
    /// config.plugins.state 覆盖快照（load_all 时更新；late 注册时读取）
    overrides: parking_lot::RwLock<BTreeMap<String, bool>>,
    plugins_dir: PathBuf,
    host_version: String,
}

/// 宿主当前版本（semver 就绪前的简化口径）
fn host_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// 版本比较（简化 SemVer：按点分段数字比较，预发布后缀截断；解析失败视为兼容）
fn version_ok(exm_min: &str, host: &str) -> bool {
    let parse = |s: &str| -> Vec<u64> {
        // 先整体截掉预发布/构建后缀（-rc.1 / +build），再按点分段
        let core = s.split(['-', '+']).next().unwrap_or(s);
        core.split('.')
            .map(|p| p.trim().trim_start_matches('v').parse().unwrap_or(0))
            .collect()
    };
    // 预发布（-rc.1 等）视为低于同号正式版：0.5.2-rc.1 < 0.5.2。
    // host 为预发布时按「数值段领先即满足」口径近似满足插件门槛判断。
    fn strip_pre(s: &str) -> &str {
        s.split(['-', '+']).next().unwrap_or(s)
    }
    let (a, b) = (parse(exm_min), parse(host));
    if a.is_empty() {
        return true;
    }
    let pre_host = host.contains('-') || host.contains('+');
    for i in 0..a.len().max(b.len()) {
        let x = a.get(i).copied().unwrap_or(0);
        let y = b.get(i).copied().unwrap_or(0);
        if y > x {
            return true;
        }
        if y < x {
            // host 是预发布且数值段领先：视为满足（1.0.0-rc.1 ≥ 1.0.0 不成立，
            // 但门槛场景（min=1.0.0, host=1.1.0-rc.1）应满足）
            if pre_host && strip_pre(host) != strip_pre(exm_min) {
                return true;
            }
            return false;
        }
    }
    // 数值段全等：min 带预发布后缀而 host 是正式版 → 满足（1.0.0 正式 ≥ 1.0.0-rc.1）
    true
}

impl PluginManager {
    pub fn new(plugins_dir: impl Into<PathBuf>) -> Self {
        PluginManager {
            entries: parking_lot::RwLock::new(BTreeMap::new()),
            providers: parking_lot::RwLock::new(BTreeMap::new()),
            builtin: BTreeMap::new(),
            overrides: parking_lot::RwLock::new(BTreeMap::new()),
            plugins_dir: plugins_dir.into(),
            host_version: host_version(),
        }
    }

    /// 静态注册 builtin 插件（Core 装配时调用；manifest 声明 + provider 构造函数）
    pub fn register_builtin(
        &mut self,
        manifest: PluginManifest,
        build: fn() -> Option<Arc<dyn PluginProvider>>,
    ) {
        self.builtin.insert(manifest.id.clone(), (manifest, build));
    }

    /// 运行期补注册 builtin 插件（gateway 侧装配用：Core 已创建、PluginManager
    /// 已包 Arc 无法 &mut）。登记 entry + provider 一步到位；watcher 由调用方驱动。
    pub fn register_builtin_late(&self, provider: Box<dyn PluginProvider>) {
        let m = provider.manifest().clone();
        let id = m.id.clone();
        let dir = self.plugins_dir.join(&id);
        let enabled = self
            .overrides
            .read()
            .get(&id)
            .copied()
            .unwrap_or(true); // config.plugins.state 未覆盖时缺省启用
        let arc: Arc<dyn PluginProvider> = provider.into();
        if enabled {
            self.providers.write().insert(id.clone(), arc);
        }
        self.entries.write().insert(
            id,
            PluginEntry {
                manifest: m,
                form: "builtin".into(),
                enabled,
                dir: dir.exists().then_some(dir),
                error: None,
            },
        );
    }

    /// 收编既有机制为内置虚拟插件（呈现层登记：管理面可见、不可禁用卸载）。
    /// CustomTool（custom-tools）与 entities/skills（builtin-resources）走这里——
    /// 执行面保持原路径不变（零迁移），仅让注册面能枚举它们的存在。
    /// 返回 false = 该虚拟插件当前无内容（无 CustomTool / 无技能文件），不登记。
    pub fn register_virtual(&self, id: &str, has_content: bool) -> bool {
        if !has_content {
            return false;
        }
        let (name, desc) = match id {
            "custom-tools" => (
                "自定义工具",
                "config 声明的 HTTP/shell 工具（设置页维护，不可禁用）",
            ),
            "builtin-resources" => (
                "内置资源",
                "entities/skills 技能文件与提示词（热装载，不可禁用）",
            ),
            _ => ("虚拟插件", ""),
        };
        self.entries.write().insert(
            id.to_string(),
            PluginEntry {
                manifest: PluginManifest {
                    id: id.to_string(),
                    name: name.into(),
                    description: desc.into(),
                    ..Default::default()
                },
                form: "virtual".into(),
                enabled: true,
                dir: None,
                error: None,
            },
        );
        true
    }

    /// 全量装载：扫描 plugins/ 目录 + builtin 注册表，校验并登记。
    /// enabled_overrides 来自 config.plugins.state（false = 禁用；缺省 = 启用）。
    /// 单插件失败不阻塞整体（记录错误继续），返回（成功数, 拒绝数）。
    pub fn load_all(
        &self,
        enabled_overrides: &BTreeMap<String, bool>,
    ) -> (usize, usize) {
        let mut ok = 0usize;
        let mut rejected = 0usize;
        // 覆盖快照（late 注册读同一口径）
        *self.overrides.write() = enabled_overrides.clone();

        // builtin：静态注册表 + plugins/<id>/plugin.json（清单可选——缺省用注册表内置清单）
        for (id, (manifest, build)) in &self.builtin {
            let dir = self.plugins_dir.join(id);
            let mut m = manifest.clone();
            if let Some(disk) = read_manifest(&dir) {
                m = disk; // 磁盘清单优先（可覆盖内置声明的描述/凭据面）
            }
            let entry = match validate(&m, id, &self.host_version) {
                Err(e) => {
                    rejected += 1;
                    PluginEntry {
                        manifest: m,
                        form: "builtin".into(),
                        enabled: false,
                        dir: dir.exists().then_some(dir),
                        error: Some(e),
                    }
                }
                Ok(()) => {
                    ok += 1;
                    let enabled = enabled_overrides.get(id).copied().unwrap_or(true);
                    if enabled {
                        if let Some(p) = build() {
                            self.providers.write().insert(id.clone(), p);
                        }
                    }
                    PluginEntry {
                        manifest: m,
                        form: "builtin".into(),
                        enabled,
                        dir: dir.exists().then_some(dir),
                        error: None,
                    }
                }
            };
            self.entries.write().insert(id.clone(), entry);
        }

        // plugins/ 目录扫描：builtin 已登记的跳过（上面已处理），其余按清单装载
        let dirs = list_plugin_dirs(&self.plugins_dir);
        for dir in dirs {
            let id = dir.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
            if id.is_empty() || self.entries.read().contains_key(&id) {
                continue;
            }
            let Some(m) = read_manifest(&dir) else {
                rejected += 1;
                self.entries.write().insert(
                    id.clone(),
                    PluginEntry {
                        manifest: PluginManifest { id: id.clone(), ..Default::default() },
                        form: "unknown".into(),
                        enabled: false,
                        dir: Some(dir.clone()),
                        error: Some("缺少 plugin.json 或不可解析".into()),
                    },
                );
                continue;
            };
            let form = if m.is_process() { "process" } else { "declarative" };
            match validate(&m, &id, &self.host_version) {
                Err(e) => {
                    rejected += 1;
                    self.entries.write().insert(
                        id.clone(),
                        PluginEntry { manifest: m, form: form.into(), enabled: false, dir: Some(dir), error: Some(e) },
                    );
                }
                Ok(()) => {
                    let enabled = enabled_overrides.get(&id).copied().unwrap_or(true);
                    ok += 1;
                    self.entries.write().insert(
                        id.clone(),
                        PluginEntry { manifest: m, form: form.into(), enabled, dir: Some(dir), error: None },
                    );
                }
            }
        }

        (ok, rejected)
    }

    /// 登记运行期 provider（进程桥握手完成后由桥调用）
    pub fn set_provider(&self, id: &str, provider: Arc<dyn PluginProvider>) {
        self.providers.write().insert(id.to_string(), provider);
    }

    /// 移除 provider（插件禁用/进程死亡）
    pub fn remove_provider(&self, id: &str) {
        self.providers.write().remove(id);
    }

    /// 移除登记条目（虚拟插件内容清空时用）
    pub fn remove_entry(&self, id: &str) {
        self.entries.write().remove(id);
    }

    /// 设置启停（持久化由调用方写回 config）。virtual 形态不可禁用（收编的既有机制）。
    pub fn set_enabled(&self, id: &str, enabled: bool) -> bool {
        let mut es = self.entries.write();
        match es.get_mut(id) {
            Some(e) if e.form == "virtual" => false,
            Some(e) => {
                e.enabled = enabled;
                if !enabled {
                    self.providers.write().remove(id);
                }
                true
            }
            None => false,
        }
    }

    /// 插件列表快照（管理端点/doctor 用）
    pub fn list(&self) -> Vec<PluginEntry> {
        self.entries.read().values().cloned().collect()
    }

    pub fn get(&self, id: &str) -> Option<PluginEntry> {
        self.entries.read().get(id).cloned()
    }

    /// 当前启用的 provider 清单（能力面枚举）
    pub fn enabled_providers(&self) -> Vec<Arc<dyn PluginProvider>> {
        let entries = self.entries.read();
        let ps = self.providers.read();
        entries
            .values()
            .filter(|e| e.enabled)
            .filter_map(|e| ps.get(&e.manifest.id).cloned())
            .collect()
    }

    /// 声明式插件的内联工具（零代码形态：清单 tools 直接注册）
    /// 返回 (plugin_id, 命名空间名, 工具定义)
    pub fn declarative_tools(&self) -> Vec<(String, String, CustomTool)> {
        self.entries
            .read()
            .values()
            .filter(|e| e.enabled && e.form == "declarative" && e.error.is_none())
            .flat_map(|e| {
                e.manifest
                    .tools
                    .iter()
                    .filter(|t| t.valid() && !t.target.trim().is_empty())
                    .map(move |t| {
                        let mut ct = t.clone();
                        ct.name = namespaced(&e.manifest.id, &t.name);
                        (e.manifest.id.clone(), ct.name.clone(), ct)
                    })
                    .collect::<Vec<_>>()
            })
            .collect()
    }
}

/// 命名空间工具名：`<plugin>__<tool>`
pub fn namespaced(plugin_id: &str, tool: &str) -> String {
    format!("{plugin_id}__{tool}")
}

/// 解析命名空间名 → (plugin_id, tool)；无前缀返回 None
pub fn parse_namespaced(name: &str) -> Option<(String, String)> {
    let (p, t) = name.split_once("__")?;
    if p.is_empty() || t.is_empty() {
        return None;
    }
    Some((p.to_string(), t.to_string()))
}

fn list_plugin_dirs(root: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = rd
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| e.path())
        .collect();
    dirs.sort();
    dirs
}

fn read_manifest(dir: &Path) -> Option<PluginManifest> {
    let p = dir.join("plugin.json");
    let text = std::fs::read_to_string(p).ok()?;
    serde_json::from_str(&text).ok()
}

/// 清单校验：id 合法且与目录一致、至少一类能力、版本兼容、内联工具合法。
/// 返回 Err(原因) 或 Ok。
fn validate(m: &PluginManifest, dir_id: &str, host: &str) -> Result<(), String> {
    if !m.valid_id() {
        return Err(format!("id 不合法：{:?}", m.id));
    }
    if m.id != dir_id {
        return Err(format!("id 与目录名不一致：{} ≠ {}", m.id, dir_id));
    }
    if !m.has_any_capability() {
        return Err("未声明任何能力（tools/channel/watch/resources 至少一类）".into());
    }
    if !m.exm_min.trim().is_empty() && !version_ok(&m.exm_min, host) {
        return Err(format!("需要宿主 ≥ {}，当前 {}", m.exm_min, host));
    }
    if m.is_process() {
        if let Some(p) = &m.process {
            if p.command.iter().any(|c| c.contains("..")) {
                return Err("process.command 不允许包含 ..".into());
            }
        }
    }
    for t in &m.tools {
        if !t.valid() {
            return Err(format!("内联工具名不合法：{:?}", t.name));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------- 单测

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("exm-plugin-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn write_plugin(root: &Path, id: &str, manifest: Value) -> PathBuf {
        let p = root.join(id);
        std::fs::create_dir_all(&p).unwrap();
        std::fs::write(p.join("plugin.json"), manifest.to_string()).unwrap();
        p
    }

    fn manifest_json(id: &str) -> Value {
        json!({ "id": id, "name": id, "tools": [ { "name": "ping", "description": "测试", "kind": "http", "target": "https://example.com", "method": "GET" } ] })
    }

    #[test]
    fn 装载_合法清单() {
        let root = dir("ok");
        write_plugin(&root, "alpha", manifest_json("alpha"));
        let pm = PluginManager::new(&root);
        let (ok, rej) = pm.load_all(&BTreeMap::new());
        assert_eq!((ok, rej), (1, 0));
        assert!(pm.get("alpha").unwrap().enabled);
        let tools = pm.declarative_tools();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].2.name, "alpha__ping");
        assert_eq!(tools[0].2.target, "https://example.com");
        assert_eq!(tools[0].1, "alpha__ping");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn 装载_缺清单被拒() {
        let root = dir("missing");
        std::fs::create_dir_all(root.join("beta")).unwrap();
        let pm = PluginManager::new(&root);
        let (ok, rej) = pm.load_all(&BTreeMap::new());
        assert_eq!((ok, rej), (0, 1));
        let e = pm.get("beta").unwrap();
        assert!(e.error.unwrap().contains("plugin.json"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn 装载_坏json被拒() {
        let root = dir("badjson");
        let p = root.join("gamma");
        std::fs::create_dir_all(&p).unwrap();
        std::fs::write(p.join("plugin.json"), "{ not json").unwrap();
        let pm = PluginManager::new(&root);
        let (_, rej) = pm.load_all(&BTreeMap::new());
        assert_eq!(rej, 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn 装载_id冲突被拒() {
        let root = dir("conflict");
        // builtin 先注册 alpha，目录里再来一个 alpha：目录版被跳过（builtin 优先）
        write_plugin(&root, "alpha", manifest_json("alpha"));
        let mut pm = PluginManager::new(&root);
        pm.register_builtin(
            PluginManifest { id: "alpha".into(), name: "内置".into(), ..Default::default() },
            || None,
        );
        let (_, rej) = pm.load_all(&BTreeMap::new());
        // builtin 自身合法（有 name 无能力——builtin 不要求清单能力，代码即能力）
        assert_eq!(rej, 0);
        let e = pm.get("alpha").unwrap();
        assert_eq!(e.form, "builtin");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn 装载_空能力被拒() {
        let root = dir("nocap");
        write_plugin(&root, "delta", json!({ "id": "delta", "name": "空" }));
        let pm = PluginManager::new(&root);
        let (_, rej) = pm.load_all(&BTreeMap::new());
        assert_eq!(rej, 1);
        assert!(pm.get("delta").unwrap().error.unwrap().contains("能力"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn 装载_版本不满足被拒() {
        let root = dir("ver");
        write_plugin(&root, "eps", json!({ "id": "eps", "exmMin": "999.0.0", "resources": ["skill.json"] }));
        let pm = PluginManager::new(&root);
        let (_, rej) = pm.load_all(&BTreeMap::new());
        assert_eq!(rej, 1);
        let err = pm.get("eps").unwrap().error.unwrap();
        assert!(err.contains("999.0.0"), "{err}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn 装载_内联工具注册() {
        let root = dir("inline");
        write_plugin(&root, "zeta", manifest_json("zeta"));
        let pm = PluginManager::new(&root);
        pm.load_all(&BTreeMap::new());
        let tools = pm.declarative_tools();
        assert_eq!(tools[0].2.target, "https://example.com");
        assert_eq!(tools[0].1, "zeta__ping");
        assert_eq!(namespaced("zeta", "ping"), "zeta__ping");
        assert_eq!(parse_namespaced("zeta__ping"), Some(("zeta".into(), "ping".into())));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn 启停_禁用摘除与持久化语义() {
        let root = dir("toggle");
        write_plugin(&root, "eta", manifest_json("eta"));
        let pm = PluginManager::new(&root);
        pm.load_all(&BTreeMap::new());
        assert!(pm.set_enabled("eta", false));
        assert!(!pm.get("eta").unwrap().enabled);
        assert!(pm.declarative_tools().is_empty());
        // 状态重启保持：用同一 overrides 再装载
        let mut ov = BTreeMap::new();
        ov.insert("eta".to_string(), false);
        pm.load_all(&ov);
        assert!(!pm.get("eta").unwrap().enabled);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn 版本比较_边界() {
        assert!(version_ok("0.5.0", "0.5.2"));
        assert!(version_ok("0.5.2", "0.5.2"));
        assert!(!version_ok("0.6.0", "0.5.2"));
        assert!(version_ok("", "0.5.2"));
        assert!(version_ok("1.0.0-rc.1", "1.0.0"));
    }

    #[test]
    fn 虚拟插件_收编登记与不可禁用() {
        let root = dir("virtual");
        let pm = PluginManager::new(&root);
        pm.load_all(&BTreeMap::new());
        // 无内容 → 不登记
        assert!(!pm.register_virtual("custom-tools", false));
        assert!(pm.get("custom-tools").is_none());
        // 有内容 → 登记、可见、enabled
        assert!(pm.register_virtual("custom-tools", true));
        let e = pm.get("custom-tools").unwrap();
        assert_eq!(e.form, "virtual");
        assert!(e.enabled);
        // 不可禁用
        assert!(!pm.set_enabled("custom-tools", false));
        assert!(pm.get("custom-tools").unwrap().enabled);
        // 可移除（内容清空时）
        pm.remove_entry("custom-tools");
        assert!(pm.get("custom-tools").is_none());
        let _ = std::fs::remove_dir_all(&root);
    }
}
