//! EXMACHINA 桌面端（Tauri v2 壳）—— 桌面集成（组 11）
//!
//! 桌面优先形态：**主窗口 = 独立 React 编程工作区**（desktop/src → desktop/dist，经 frontendDist 嵌入二进制），
//! 参考常见 agent 桌面客户端：打开即对话，会话列表 + 流式回答 + 审批内联裁决。
//! 壳拉起捆绑的 exm-gateway 作为后端（REST/WS 同端口），全部管理能力（模型/组/通道/定时/
//! 安全）由工作区设置视图直连环口 API 完成；亦支持 `--remote <url>`。
//! 移动端使用独立应用连接实例后端；浏览器打开网关地址进入 WebUI 管理台。
//!
//! 启动形态：
//! - `exmachina-desktop`                                          —— 拉起捆绑网关，主窗口进入对话界面
//! - `exmachina-desktop --remote http://host:4173 --key <authKey>` —— 直连远程网关
//!
//! 网关地址与密钥经 Tauri 初始化脚本注入 `window.__EXM_GATEWAY__`，对话 UI 据此直连环口。
//!
//! 组 11 桌面集成：
//! - 系统托盘（11.1）：常驻托盘菜单（打开控制面板 / 显示隐藏 / 退出）；关窗默认驻留（可配置完全退出）；
//!   退出级联终止网关子进程
//! - 原生通知（11.2）：壳内 WS 事件流 → 审批 / 定时推送 / 任务完成三类通知（类别开关，desktop.rs 映射单测）
//! - 全局快捷键（11.3）：默认唤起/收起主窗口（可经 desktop.json 或 setDesktopConfig 命令改绑，冲突提示）
//! - 捆绑网关健康监控（6.3）：子进程退出检测 + 一键重启（复用原端口）+ UI 状态层
//! - 版本比对（6.2）：壳注入版本（CARGO_PKG_VERSION），UI 启动 + 6h 周期比对 /api/version

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod desktop;

use desktop::{launch_gateway_authenticated, wait_gateway_ready};
use std::net::TcpListener;
use std::process::Child;
use std::sync::Mutex;
use std::time::Duration;
use tauri::Manager;
use tauri_plugin_notification::NotificationExt;

/// 应用状态：随应用存活的网关子进程（退出时终止）+ 网关地址与密钥（事件桥 / 深链定位用）
struct GatewayChild(Mutex<Option<Child>>);
struct LocalGatewayInfo(Mutex<Option<GatewayInfo>>);
struct ActiveGatewayInfo(Mutex<GatewayInfo>);

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct GatewayInfo {
    /// 网关基址（http://host:port）
    url: String,
    /// 访问密钥（事件桥 WS 鉴权；远程模式来自 --key）
    key: String,
    /// 远程模式（true = 壳不托管网关进程，健康监控只看连通性不看重启）
    remote: bool,
    console_url: String,
    lan_enabled: bool,
    lan_url: Option<String>,
    workspace: Option<String>,
}

fn main() {
    // 命令行解析：--remote <url>（远程模式）；--key <authKey>（访问密钥，可选）
    let mut remote: Option<String> = None;
    let mut key: Option<String> = None;
    let args: Vec<String> = std::env::args().collect();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--remote" => remote = args.get(i + 1).cloned(),
            "--key" => key = args.get(i + 1).cloned(),
            _ => {}
        }
        i += 2;
    }

    tauri::Builder::default()
        .manage(GatewayChild(Mutex::new(None)))
        .manage(LocalGatewayInfo(Mutex::new(None)))
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, _shortcut, event| {
                    // 全局快捷键：唤起 / 收起主窗口（按下沿触发，避免重复切换）
                    use tauri_plugin_global_shortcut::ShortcutState;
                    if event.state() == ShortcutState::Pressed {
                        toggle_main_window(app);
                    }
                })
                .build(),
        )
        .invoke_handler(tauri::generate_handler![
            get_desktop_config,
            set_desktop_config,
            min_window,
            max_window,
            close_window,
            hide_window,
            toggle_max_window,
            gateway_restart,
            get_local_gateway,
            set_event_gateway,
            open_console
        ])
        .setup(move |app| {
            let remote_mode = remote.is_some();
            let info = match remote {
                Some(r) => GatewayInfo { url: r.trim_end_matches('/').to_string(), key: key.clone().unwrap_or_default(), remote: true, console_url: r.trim_end_matches('/').to_string(), lan_enabled: false, lan_url: None, workspace: None },
                None => spawn_local_gateway(app.handle(), desktop::DesktopConfig::load().allow_lan)?,
            };
            let url = info.url;
            let key_val = info.key;
            let info = GatewayInfo { url: url.clone(), key: key_val.clone(), remote: remote_mode, console_url: info.console_url, lan_enabled: info.lan_enabled, lan_url: info.lan_url, workspace: None };
            app.manage(ActiveGatewayInfo(Mutex::new(info.clone())));

            // 主界面：独立 React 编程工作区（desktop/dist，经 frontendDist 打进二进制），
            // 网关只做后端（REST/WS）。网关地址与密钥经初始化脚本注入（window.__EXM_GATEWAY__），
            // UI 据此直连环口；管理组件仅在工作区设置中打开。
            let script = format!(
                "window.__EXM_GATEWAY__ = {{ url: {url}, key: {key}, version: {version}, remote: {remote} }};",
                url = serde_json::to_string(&url)?,
                key = serde_json::to_string(&key_val)?,
                version = serde_json::to_string(env!("CARGO_PKG_VERSION"))?,
                remote = remote_mode,
            );
            tauri::WebviewWindowBuilder::new(app, "main", tauri::WebviewUrl::App("index.html".into()))
                .title("EXMACHINA · 多智能体系统")
                .decorations(false)
                .inner_size(1320.0, 860.0)
                .min_inner_size(420.0, 360.0)
                .initialization_script(&script)
                .build()?;

            // 系统托盘（11.1）：常驻菜单 + 显隐 + 退出（退出级联终止网关子进程）
            build_tray(app.handle())?;

            // 全局快捷键（11.3）：注册失败（冲突/被占用）→ 提示并引导改绑
            register_shortcut(app.handle(), &desktop::DesktopConfig::load().shortcut);

            // 原生通知事件桥（11.2）：WS 事件流 → 三类通知（类别开关见 desktop.json）
            spawn_event_bridge(app.handle().clone());

            // 捆绑网关健康监控（6.3）：本机模式检测子进程退出 → 事件推 UI（状态显示 +
            // 一键重启）；远程模式不监控进程（只看 UI 侧 WS 连通，退避重连已有）
            if !remote_mode {
                spawn_gateway_monitor(app.handle().clone());
            }

            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("Tauri 启动失败")
        .run(|app, event| match event {
            // 关窗驻留（11.1）：默认隐藏并驻留托盘，网关进程持续运行；完全退出行为可配置
            tauri::RunEvent::WindowEvent { label, event: tauri::WindowEvent::CloseRequested { api, .. }, .. } if label == "main" => {
                let cfg = desktop::DesktopConfig::load();
                if !cfg.exit_on_close {
                    if let Some(w) = app.get_webview_window("main") {
                        let _ = w.hide();
                    }
                    api.prevent_close();
                }
            }
            // 退出即终止网关子进程（托盘退出 / 完全退出共用此收口）
            tauri::RunEvent::Exit => {
                if let Some(child) = app.state::<GatewayChild>().0.lock().unwrap().as_mut() {
                    let _ = child.kill();
                    let _ = child.wait();
                }
            }
            _ => {}
        });
}

// ---------------------------------------------------------------- 系统托盘（11.1）

fn build_tray(app: &tauri::AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    use tauri::menu::{Menu, MenuItem};
    let open = MenuItem::with_id(app, "open", "打开控制面板", true, None::<&str>)?;
    let toggle = MenuItem::with_id(app, "toggle", "显示 / 隐藏窗口", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &toggle, &quit])?;

    let mut builder = tauri::tray::TrayIconBuilder::with_id("main-tray")
        .menu(&menu)
        .tooltip("EXMACHINA · 多智能体系统")
        .show_menu_on_left_click(true)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "open" => show_main_window(app),
            "toggle" => toggle_main_window(app),
            "quit" => {
                // 托盘退出：级联终止网关子进程（RunEvent::Exit 收口）
                app.exit(0);
            }
            _ => {}
        });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;
    Ok(())
}

fn show_main_window(app: &tauri::AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

fn toggle_main_window(app: &tauri::AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let visible = w.is_visible().unwrap_or(false);
        let focused = w.is_focused().unwrap_or(false);
        if visible && focused {
            let _ = w.hide();
        } else {
            let _ = w.show();
            let _ = w.unminimize();
            let _ = w.set_focus();
        }
    }
}

// ---------------------------------------------------------------- 全局快捷键（11.3）

/// 注册全局快捷键；冲突 / 被占用 → 原生通知提示并引导改绑（desktop.json 或 setDesktopConfig 命令）
fn register_shortcut(app: &tauri::AppHandle, combo: &str) {
    use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut};
    let parsed: Shortcut = match combo.parse() {
        Ok(s) => s,
        Err(_) => {
            eprintln!("[desktop] 快捷键不可解析: {combo}");
            return;
        }
    };
    let gs = app.global_shortcut();
    let _ = gs.unregister_all();
    if let Err(e) = gs.register(parsed) {
        eprintln!("[desktop] 快捷键注册失败（可能被其他应用占用）: {combo}: {e}");
        let _ = app
            .notification()
            .builder()
            .title("全局快捷键注册失败")
            .body(format!("组合键 {combo} 被占用，请在 desktop.json 中改绑 shortcut 字段"))
            .show();
    }
}

/// 读桌面配置（前端设置 / 诊断用）
#[tauri::command]
fn get_desktop_config() -> desktop::DesktopConfig {
    desktop::DesktopConfig::load()
}

// 自绘标题栏的窗口控制按钮（decorations=false，零 ACL 依赖走自有命令）
#[tauri::command]
fn min_window(w: tauri::Window) {
    let _ = w.minimize();
}

#[tauri::command]
fn max_window(w: tauri::Window) {
    if w.is_maximized().unwrap_or(false) {
        let _ = w.unmaximize();
    } else {
        let _ = w.maximize();
    }
}

#[tauri::command]
fn close_window(w: tauri::Window) {
    // 走 CloseRequested 事件：默认隐藏驻留托盘（exit_on_close 时真退出）
    let _ = w.close();
}

#[tauri::command]
fn hide_window(w: tauri::Window) {
    let _ = w.hide();
}

#[tauri::command]
fn toggle_max_window(w: tauri::Window) -> bool {
    let maximized = w.is_maximized().unwrap_or(false);
    if maximized {
        let _ = w.unmaximize();
    } else {
        let _ = w.maximize();
    }
    !maximized
}

/// 保存桌面配置：快捷键改绑即时重注册；冲突返回提示（不落盘失败配置的快捷键行为）
#[tauri::command]
fn set_desktop_config(
    app: tauri::AppHandle,
    exit_on_close: Option<bool>,
    shortcut: Option<String>,
    notify_approval: Option<bool>,
    notify_cron: Option<bool>,
    notify_task_done: Option<bool>,
    allow_lan: Option<bool>,
) -> Result<desktop::DesktopConfig, String> {
    let mut cfg = desktop::DesktopConfig::load();
    if let Some(v) = exit_on_close {
        cfg.exit_on_close = v;
    }
    if let Some(v) = notify_approval {
        cfg.notify_approval = v;
    }
    if let Some(v) = notify_cron {
        cfg.notify_cron = v;
    }
    if let Some(v) = notify_task_done {
        cfg.notify_task_done = v;
    }
    if let Some(v) = allow_lan {
        cfg.allow_lan = v;
    }
    if let Some(s) = shortcut {
        let s = s.trim().to_string();
        if !s.is_empty() {
            use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut};
            let parsed: Shortcut = s.parse().map_err(|_| format!("快捷键不可解析: {s}"))?;
            let gs = app.global_shortcut();
            let _ = gs.unregister_all();
            gs.register(parsed).map_err(|_e| {
                // 冲突：恢复旧键注册并提示（不静默吞掉失败）
                let restored: Result<tauri_plugin_global_shortcut::Shortcut, _> =
                    desktop::DesktopConfig::load().shortcut.parse();
                let _ = gs.register(restored.unwrap_or_else(|_| "Alt+Shift+E".parse().unwrap()));
                format!("组合键 {s} 被其他应用占用，请改绑")
            })?;
            cfg.shortcut = s;
        }
    }
    cfg.save()?;
    Ok(cfg)
}

// ---------------------------------------------------------------- 原生通知事件桥（11.2）

/// WS 事件桥线程：订阅网关事件流，审批 / 定时推送 / 任务完成映射为系统原生通知
/// （desktop::map_event 纯函数映射，含类别开关与单测覆盖）。审批通知触发时聚焦主窗口。
fn spawn_event_bridge(app: tauri::AppHandle) {
    std::thread::spawn(move || {
        let rt = match tokio::runtime::Runtime::new() {
            Ok(rt) => rt,
            Err(e) => {
                eprintln!("[desktop] 事件桥启动失败: {e}");
                return;
            }
        };
        rt.block_on(async move {
            loop {
                let active = app.state::<ActiveGatewayInfo>();
                let info = active.0.lock().map(|info| info.clone());
                let Ok(info) = info else { return };
                let ws_url = event_stream_url(&info.url, &info.key, info.workspace.as_deref());
                match connect_and_pump(&app, &ws_url, &info).await {
                    Ok(()) => return,
                    Err(e) => {
                        let delay = if e == "active gateway changed" { Duration::from_millis(100) } else { Duration::from_secs(5) };
                        eprintln!("[desktop] 事件流断开，{delay:?} 后重连: {e}");
                        tokio::time::sleep(delay).await;
                    }
                }
            }
        });
    });
}

async fn connect_and_pump(app: &tauri::AppHandle, ws_url: &str, expected: &GatewayInfo) -> Result<(), String> {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;
    let (mut ws, _resp) = tokio_tungstenite::connect_async(ws_url)
        .await
        .map_err(|e| e.to_string())?;
    let mut check = tokio::time::interval(Duration::from_secs(2));
    loop {
        tokio::select! {
            _ = check.tick() => {
                let active = app.state::<ActiveGatewayInfo>().0.lock().map_err(|e| e.to_string())?.clone();
                if active.url != expected.url || active.key != expected.key || active.workspace != expected.workspace {
                    return Err("active gateway changed".into());
                }
            }
            msg = ws.next() => match msg {
            Some(msg) => match msg {
            Ok(Message::Text(text)) => {
                let Ok(evt) = serde_json::from_str::<serde_json::Value>(&text) else { continue };
                let kind = evt.get("type").and_then(|v| v.as_str()).unwrap_or("");
                let cfg = desktop::DesktopConfig::load();
                let payload = evt.get("payload").cloned().unwrap_or(serde_json::Value::Null);
                if let Some(n) = desktop::map_event(kind, &payload, &cfg) {
                    let _ = app
                        .notification()
                        .builder()
                        .title(n.title)
                        .body(n.body)
                        .show();
                    // 审批请求需要人工立即处理：聚焦主窗口（点击定位的平台兜底口径）
                    if n.category == "approval" {
                        show_main_window(app);
                    }
                }
            }
            Ok(Message::Ping(_)) => {
                // Tungstenite queues Pong while reading; flush it even when no events arrive.
                ws.flush().await.map_err(|e| e.to_string())?;
            }
            Ok(Message::Close(_)) | Err(_) => return Err("事件流已关闭".into()),
            _ => {}
            },
            None => return Err("事件流已结束".into()),
            }
        }
    }
}

/// Keep reverse-proxy path prefixes, and append authentication after the WS path.
fn event_stream_url(base: &str, key: &str, workspace: Option<&str>) -> String {
    let base = base.trim_end_matches('/');
    let base = if let Some(rest) = base.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = base.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        base.to_string()
    };
    let mut params = Vec::new();
    if let Some(workspace) = workspace {
        params.push(format!("workspace={}", percent_encode_query_value(workspace)));
    }
    if !key.is_empty() { params.push(format!("key={}", percent_encode_query_value(key))); }
    if params.is_empty() { format!("{base}/ws") } else { format!("{base}/ws?{}", params.join("&")) }
}

fn percent_encode_query_value(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(char::from(byte));
        } else {
            use std::fmt::Write;
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    encoded
}

// ---------------------------------------------------------------- 本地网关（捆绑交付）

/// 捆绑网关健康监控（6.3）：轮询子进程退出状态，检测到退出即发 `gateway://down`
/// 事件到主窗口（UI 呈现状态 + 一键重启）。恢复正常（重启成功）发 `gateway://up`。
/// 退出检测后停止轮询（进程已死，重启由用户经 UI 触发 gateway_restart 命令）。
fn spawn_gateway_monitor(app: tauri::AppHandle) {
    std::thread::spawn(move || loop {
        // 退出判定：子进程已收尸（Some 状态）且槽位被清（重启中/已重启的窗口期不再误报）
        let exited = {
            let state = app.state::<GatewayChild>();
            let mut guard = match state.0.lock() {
                Ok(g) => g,
                Err(_) => return, // 锁中毒：监控线程退出（不应发生；壳层 Mutex 无已知 panic 路径）
            };
            match guard.as_mut() {
                None => false, // 槽位空 = 正在重启或尚未拉起
                Some(child) => matches!(child.try_wait(), Ok(Some(_))),
            }
        };
        if exited {
            // 收尸并清槽（退出码留日志；重启由 UI 触发）
            if let Ok(mut guard) = app.state::<GatewayChild>().0.lock() {
                *guard = None;
            }
            eprintln!("[desktop] 捆绑网关进程退出，通知 UI（可一键重启）");
            use tauri::Emitter;
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.emit("gateway://down", ());
            }
            return; // 一次性监控：重启命令会重新拉起监控线程
        }
        std::thread::sleep(Duration::from_secs(2));
    });
}

/// 一键重启捆绑网关（6.3）：复用原端口（UI 与事件桥寻址不变），拉起成功发
/// `gateway://up` 事件并重启健康监控；失败返回错误文案（UI 呈现原因）。
/// 远程模式无子进程可重启，直接明示。
#[tauri::command]
fn gateway_restart(app: tauri::AppHandle) -> Result<GatewayInfo, String> {
    let mut info = app.state::<ActiveGatewayInfo>().0.lock().map_err(|e| e.to_string())?.clone();
    if info.remote { return Err("远程实例在服务器上运行，桌面客户端不能重启它".into()); }
    {
        let gateway_child = app.state::<GatewayChild>();
        let mut child = gateway_child.0.lock().map_err(|e| e.to_string())?;
        if let Some(mut running) = child.take() {
            if running.try_wait().ok().flatten().is_none() { running.kill().map_err(|e| e.to_string())?; }
            let _ = running.wait();
        }
    }
    // 从当前 URL 提取原端口（重启复用——UI/事件桥寻址不变）
    let port = desktop::port_of_url(&info.url).ok_or("无法解析网关端口")?;
    let allow_lan = desktop::DesktopConfig::load().allow_lan;
    match spawn_gateway_on_port(&app, port, &info.key, allow_lan) {
        Ok(p) => {
            info.url = format!("http://127.0.0.1:{p}");
            info.lan_enabled = allow_lan;
            info.lan_url = info.lan_enabled.then(|| lan_address(p)).flatten();
            info.console_url = info.lan_url.clone().unwrap_or_default();
            if let Ok(mut active) = app.state::<ActiveGatewayInfo>().0.lock() { *active = info.clone(); }
            if let Ok(mut local) = app.state::<LocalGatewayInfo>().0.lock() { *local = Some(info.clone()); }
            use tauri::Emitter;
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.emit("gateway://up", info.url.clone());
            }
            spawn_gateway_monitor(app.clone());
            Ok(())
        }
        Err(e) => Err(format!("网关重启失败：{e}")),
    }?;
    let active = app.state::<ActiveGatewayInfo>();
    active.0.lock().map(|info| info.clone()).map_err(|e| e.to_string())
}

/// 拉起捆绑的 exm-gateway：空闲端口 + 独立数据目录，就绪后返回 UI 地址（REST / WS / WebUI 同端口）
fn spawn_local_gateway(app: &tauri::AppHandle, allow_lan: bool) -> Result<GatewayInfo, Box<dyn std::error::Error>> {
    let state = app.state::<LocalGatewayInfo>();
    let mut local = state.0.lock().map_err(|e| e.to_string())?;
    if let Some(info) = local.as_ref() { return Ok(info.clone()); }
    let port = free_port();
    let key = uuid::Uuid::new_v4().to_string();
    spawn_gateway_on_port(app, port, &key, allow_lan)?;
    let lan_url = allow_lan.then(|| lan_address(port)).flatten();
    let console_url = lan_url.clone().unwrap_or_default();
    let info = GatewayInfo { url: format!("http://127.0.0.1:{port}"), key, remote: false, console_url, lan_enabled: allow_lan, lan_url, workspace: None };
    *local = Some(info.clone());
    Ok(info)
}

#[tauri::command]
async fn get_local_gateway(app: tauri::AppHandle) -> Result<GatewayInfo, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let was_started = app.state::<LocalGatewayInfo>().0.lock().map_err(|e| e.to_string())?.is_some();
        let info = spawn_local_gateway(&app, desktop::DesktopConfig::load().allow_lan).map_err(|e| e.to_string())?;
        *app.state::<ActiveGatewayInfo>().0.lock().map_err(|e| e.to_string())? = info.clone();
        if !was_started {
            let info = info.clone();
            let handle = app.clone();
            tauri::async_runtime::spawn(async move {
                if let Some(window) = handle.get_webview_window("main") {
                    let script = format!("window.__EXM_GATEWAY__={{url:{},key:{},remote:false}};", serde_json::to_string(&info.url).unwrap(), serde_json::to_string(&info.key).unwrap());
                    if let Err(e) = window.eval(&script) { eprintln!("[desktop] 本机网关密钥注入失败: {e}"); }
                }
            });
            spawn_gateway_monitor(app);
        }
        Ok(info)
    }).await.map_err(|e| e.to_string())?
}

#[tauri::command]
fn set_event_gateway(app: tauri::AppHandle, url: String, key: String, remote: bool, workspace: Option<String>) -> Result<(), String> {
    let parsed = tauri::Url::parse(&url).map_err(|e| e.to_string())?;
    if !matches!(parsed.scheme(), "http" | "https") || !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("网关地址仅支持 HTTP(S)".into());
    }
    *app.state::<ActiveGatewayInfo>().0.lock().map_err(|e| e.to_string())? = GatewayInfo {
        url: url.clone(), key, remote, console_url: url, lan_enabled: false, lan_url: None, workspace,
    };
    Ok(())
}

#[tauri::command]
fn open_console(url: String) -> Result<(), String> {
    let parsed = tauri::Url::parse(&url).map_err(|e| e.to_string())?;
    if !matches!(parsed.scheme(), "http" | "https") || !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("控制台仅支持 HTTP(S) 地址".into());
    }
    #[cfg(target_os = "windows")]
    let mut command = { let mut cmd = std::process::Command::new("rundll32"); cmd.arg("url.dll,FileProtocolHandler"); cmd };
    #[cfg(target_os = "macos")]
    let mut command = std::process::Command::new("open");
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut command = std::process::Command::new("xdg-open");
    command.arg(parsed.as_str()).spawn().map(|_| ()).map_err(|e| e.to_string())
}

/// 在指定端口拉起捆绑网关（首启随机端口 / 重启复用原端口——UI 与事件桥寻址不变）。
/// 就绪语义同首启：TCP 可连即就绪（最多 30s；提前退出立即报错）。
fn spawn_gateway_on_port(
    app: &tauri::AppHandle,
    port: u16,
    key: &str,
    allow_lan: bool,
) -> Result<u16, Box<dyn std::error::Error>> {
    let mut bin_dirs = vec![app.path().resource_dir()?.join("binaries")];
    // `tauri dev`/debug binaries do not always carry the cargo marker that
    // tauri-utils uses to identify a target directory. Keep the production
    // lookup above authoritative, but let an adjacent debug `binaries/` dir
    // work for local Linux/macOS acceptance runs and contributors.
    #[cfg(debug_assertions)]
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            bin_dirs.push(parent.join("binaries"));
        }
    }
    let gateway = bin_dirs
        .into_iter()
        .flat_map(|dir| {
            ["exm-gateway.exe", "exm-gateway"]
                .into_iter()
                .map(move |name| dir.join(name))
        })
        .find(|path| path.is_file())
        .ok_or("未找到捆绑的 exm-gateway（检查 src-tauri/binaries/，见 desktop/README.md）")?;
    let data_dir = desktop::data_dir().join("ExMachina");
    let mut child = launch_gateway_authenticated(&gateway, &data_dir, port, Some(key), if allow_lan { "0.0.0.0" } else { "127.0.0.1" })?;
    wait_gateway_ready(&mut child, port).map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
    *app.state::<GatewayChild>().0.lock().unwrap() = Some(child);
    Ok(port)
}

fn lan_address(port: u16) -> Option<String> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("192.0.2.1:80").ok()?;
    let ip = socket.local_addr().ok()?.ip();
    (!ip.is_loopback()).then(|| format!("http://{ip}:{port}"))
}

fn free_port() -> u16 {
    TcpListener::bind(("127.0.0.1", 0))
        .and_then(|l| l.local_addr())
        .map(|a| a.port())
        .unwrap_or(4173)
}

#[cfg(test)]
mod query_encoding_tests {
    use super::{event_stream_url, percent_encode_query_value};

    #[test]
    fn notification_stream_path_precedes_authentication() {
        assert_eq!(
            event_stream_url("http://localhost:4173/", "a&b", None),
            "ws://localhost:4173/ws?key=a%26b"
        );
        assert_eq!(
            event_stream_url("http://localhost:4173/", "k", Some("/tmp/a b")),
            "ws://localhost:4173/ws?workspace=%2Ftmp%2Fa%20b&key=k"
        );
        assert_eq!(
            event_stream_url("https://example.com/agent/", "", None),
            "wss://example.com/agent/ws"
        );
    }

    #[test]
    fn websocket_key_is_encoded_as_a_query_value() {
        assert_eq!(
            percent_encode_query_value("key with+/&中文"),
            "key%20with%2B%2F%26%E4%B8%AD%E6%96%87"
        );
        assert_eq!(percent_encode_query_value("safe-._~"), "safe-._~");
    }
}
