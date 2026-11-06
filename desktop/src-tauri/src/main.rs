//! EXMACHINA 桌面端（Tauri v2 壳）—— 桌面集成（组 11）
//!
//! 桌面优先形态：**主窗口 = 壳内嵌的对话式 UI**（desktop/ui，经 frontendDist 嵌入二进制），
//! 参考常见 agent 桌面客户端：打开即对话，会话列表 + 流式回答 + 审批内联裁决。
//! 壳拉起捆绑的 exm-gateway 作为后端（REST/WS 同端口），全部管理能力（模型/组/通道/定时/
//! 安全）由壳内原生设置视图直连环口 API 完成，**不依赖任何 webui**；亦支持 `--remote <url>`。
//! 移动端无需单独 App：手机浏览器 / PWA 打开网关地址即为同源客户端。
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

use desktop::{launch_gateway, wait_gateway_ready};
use std::net::TcpListener;
use std::process::Child;
use std::sync::Mutex;
use std::time::Duration;
use tauri::Manager;
use tauri_plugin_notification::NotificationExt;

/// 应用状态：随应用存活的网关子进程（退出时终止）+ 网关地址与密钥（事件桥 / 深链定位用）
struct GatewayChild(Mutex<Option<Child>>);

#[derive(Clone, serde::Serialize)]
struct GatewayInfo {
    /// 网关基址（http://host:port）
    url: String,
    /// 访问密钥（事件桥 WS 鉴权；远程模式来自 --key）
    key: String,
    /// 远程模式（true = 壳不托管网关进程，健康监控只看连通性不看重启）
    remote: bool,
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
            gateway_restart
        ])
        .setup(move |app| {
            let remote_mode = remote.is_some();
            let url: String = match remote {
                Some(r) => r.trim_end_matches('/').to_string(),
                None => spawn_local_gateway(app.handle())?,
            };
            let key_val = key.clone().unwrap_or_default();
            app.manage(GatewayInfo { url: url.clone(), key: key_val.clone(), remote: remote_mode });

            // 主界面：壳内嵌的对话式 UI（desktop/ui，经 frontendDist 打进二进制），
            // 网关只做后端（REST/WS）。网关地址与密钥经初始化脚本注入（window.__EXM_GATEWAY__），
            // UI 据此直连环口；管理能力经原生设置视图完成（桌面端零 webui 依赖）。
            let script = format!(
                "window.__EXM_GATEWAY__ = {{ url: '{url}', key: '{key}', version: '{ver}', remote: {remote} }};",
                url = url.replace('\\', "\\\\").replace('\'', "\\'"),
                key = key_val.replace('\\', "\\\\").replace('\'', "\\'"),
                ver = env!("CARGO_PKG_VERSION"),
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

/// 保存桌面配置：快捷键改绑即时重注册；冲突返回提示（不落盘失败配置的快捷键行为）
#[tauri::command]
fn set_desktop_config(
    app: tauri::AppHandle,
    exit_on_close: Option<bool>,
    shortcut: Option<String>,
    notify_approval: Option<bool>,
    notify_cron: Option<bool>,
    notify_task_done: Option<bool>,
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
                let info = app.state::<GatewayInfo>().inner().clone();
                let ws_url = format!("{}://{}{}/ws", ws_scheme(&info.url), host_of(&info.url), {
                    let mut q = String::new();
                    if !info.key.is_empty() {
                        q = format!("?key={}", info.key);
                    }
                    q
                });
                match connect_and_pump(&app, &ws_url).await {
                    Ok(()) => return, // 收到优雅关闭（不应发生，重连口径）
                    Err(e) => {
                        eprintln!("[desktop] 事件流断开，5s 后重连: {e}");
                        tokio::time::sleep(Duration::from_secs(5)).await;
                    }
                }
            }
        });
    });
}

async fn connect_and_pump(app: &tauri::AppHandle, ws_url: &str) -> Result<(), String> {
    use futures_util::StreamExt;
    use tokio_tungstenite::tungstenite::Message;
    let (ws, _resp) = tokio_tungstenite::connect_async(ws_url)
        .await
        .map_err(|e| e.to_string())?;
    let (_tx, mut rx) = ws.split();
    while let Some(msg) = rx.next().await {
        match msg {
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
            Ok(Message::Close(_)) | Err(_) => return Err("事件流已关闭".into()),
            _ => {}
        }
    }
    Ok(())
}

/// http(s) → ws(s)
fn ws_scheme(url: &str) -> &'static str {
    if url.starts_with("https") {
        "wss"
    } else {
        "ws"
    }
}

/// 提取 host:port（去掉 scheme 与路径）
fn host_of(url: &str) -> String {
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    rest.split('/').next().unwrap_or(rest).to_string()
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
fn gateway_restart(app: tauri::AppHandle) -> Result<String, String> {
    let info = app.state::<GatewayInfo>().inner().clone();
    if info.remote {
        return Err("远程模式：网关不在本机托管，无法重启（请在远端重启后重连）".into());
    }
    // 从当前 URL 提取原端口（重启复用——UI/事件桥寻址不变）
    let port = desktop::port_of_url(&info.url).ok_or("无法解析网关端口")?;
    match spawn_gateway_on_port(&app, port) {
        Ok(p) => {
            use tauri::Emitter;
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.emit("gateway://up", format!("http://127.0.0.1:{p}"));
            }
            spawn_gateway_monitor(app.clone());
            Ok(format!("http://127.0.0.1:{p}"))
        }
        Err(e) => Err(format!("网关重启失败：{e}")),
    }
}

/// 拉起捆绑的 exm-gateway：空闲端口 + 独立数据目录，就绪后返回 UI 地址（REST / WS / WebUI 同端口）
fn spawn_local_gateway(app: &tauri::AppHandle) -> Result<String, Box<dyn std::error::Error>> {
    let port = free_port();
    spawn_gateway_on_port(app, port).map(|p| format!("http://127.0.0.1:{p}"))
}

/// 在指定端口拉起捆绑网关（首启随机端口 / 重启复用原端口——UI 与事件桥寻址不变）。
/// 就绪语义同首启：TCP 可连即就绪（最多 30s；提前退出立即报错）。
fn spawn_gateway_on_port(
    app: &tauri::AppHandle,
    port: u16,
) -> Result<u16, Box<dyn std::error::Error>> {
    let bin_dir = app.path().resource_dir()?.join("binaries");
    let gateway = ["exm-gateway.exe", "exm-gateway"]
        .iter()
        .map(|n| bin_dir.join(n))
        .find(|p| p.is_file())
        .ok_or("未找到捆绑的 exm-gateway（检查 src-tauri/binaries/，见 desktop/README.md）")?;
    let data_dir = desktop::data_dir().join("ExMachina");
    let mut child = launch_gateway(&gateway, &data_dir, port)?;
    wait_gateway_ready(&mut child, port).map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
    *app.state::<GatewayChild>().0.lock().unwrap() = Some(child);
    Ok(port)
}

fn free_port() -> u16 {
    TcpListener::bind(("127.0.0.1", 0))
        .and_then(|l| l.local_addr())
        .map(|a| a.port())
        .unwrap_or(4173)
}
