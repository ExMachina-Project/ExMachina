//! 桌面端配置与事件映射（组 11）
//!
//! - `DesktopConfig`：托盘驻留 / 全局快捷键 / 通知类别开关（`<数据目录>/ExMachina/desktop.json`）
//! - `map_event`：网关 WS 事件 → 原生通知载荷的纯函数映射（事件映射单测覆盖，D11 零新事件通路）

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

/// 桌面端配置：关窗行为 / 全局快捷键 / 通知类别（默认值 = spec 口径）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DesktopConfig {
    /// 关闭主窗口 = 隐藏并驻留托盘（false）；true = 直接完全退出
    pub exit_on_close: bool,
    /// 全局快捷键（唤起 / 收起主窗口；冲突时提示并引导改绑）
    pub shortcut: String,
    /// 通知类别开关
    pub notify_approval: bool,
    pub notify_cron: bool,
    pub notify_task_done: bool,
    /// Allow authenticated clients on trusted LANs to connect to this bundled gateway.
    pub allow_lan: bool,
}

impl Default for DesktopConfig {
    fn default() -> Self {
        DesktopConfig {
            exit_on_close: false, // 关窗驻留默认开（spec）
            shortcut: "Alt+Shift+E".into(),
            notify_approval: true,
            notify_cron: true,
            notify_task_done: true,
            allow_lan: false,
        }
    }
}

impl DesktopConfig {
    fn path() -> std::path::PathBuf {
        data_dir().join("ExMachina").join("desktop.json")
    }

    pub fn load() -> Self {
        std::fs::read_to_string(Self::path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> Result<(), String> {
        let p = Self::path();
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(&p, serde_json::to_string_pretty(self).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())
    }
}

/// 数据目录：跨平台取用户数据区（不引额外依赖）
pub fn data_dir() -> std::path::PathBuf {
    #[cfg(target_os = "windows")]
    {
        std::env::var("LOCALAPPDATA")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| std::env::temp_dir())
    }
    #[cfg(not(target_os = "windows"))]
    {
        std::env::var("XDG_DATA_HOME")
            .map(std::path::PathBuf::from)
            .or_else(|_| {
                std::env::var("HOME").map(|h| std::path::PathBuf::from(h).join(".local").join("share"))
            })
            .unwrap_or_else(|_| std::env::temp_dir())
    }
}

// ---------------------------------------------------------------- 事件映射（11.2）

/// 原生通知载荷（由网关事件映射而来）
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notif {
    /// 通知标题（含类别前缀，用户可辨识）
    pub title: String,
    /// 通知正文
    pub body: String,
    /// 类别：approval | cron | task_done（点击定位与类别开关的键）
    pub category: &'static str,
}

/// 网关 WS 事件 → 原生通知（类别关闭 = None；无关事件 = None）。
/// 覆盖三类：审批请求（approval.required）、定时任务推送（cron.finished）、任务完成（run.finished）。
pub fn map_event(kind: &str, payload: &serde_json::Value, cfg: &DesktopConfig) -> Option<Notif> {
    let s = |k: &str| payload.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
    match kind {
        "approval.required" => {
            if !cfg.notify_approval {
                return None;
            }
            let agent = s("agentId");
            let command = s("command");
            Some(Notif {
                title: "审批请求待处理".into(),
                body: if command.is_empty() {
                    format!("{agent} 发起了一个需要批准的命令")
                } else {
                    format!("{agent}: {command}")
                },
                category: "approval",
            })
        }
        "cron.finished" => {
            if !cfg.notify_cron {
                return None;
            }
            let name = s("jobName");
            let status = s("status");
            let summary = s("summary");
            let mark = if status == "done" { "完成" } else { "失败" };
            Some(Notif {
                title: format!("定时任务{mark}：{name}"),
                body: if summary.is_empty() { status } else { summary },
                category: "cron",
            })
        }
        "run.finished" => {
            if !cfg.notify_task_done {
                return None;
            }
            // 收束陈述首条（截断，防长正文撑爆系统通知）
            let text = payload
                .get("statements")
                .and_then(|v| v.as_array())
                .and_then(|a| a.first())
                .and_then(|st| st.get("text"))
                .and_then(|t| t.as_str())
                .unwrap_or("本轮任务已收束")
                .chars()
                .take(120)
                .collect::<String>();
            Some(Notif {
                title: "任务完成".into(),
                body: text,
                category: "task_done",
            })
        }
        _ => None,
    }
}

// ---------------------------------------------------------------- 托盘计数（6.1）

const APPROVAL_SNAPSHOT_LIMIT: usize = 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingApprovalCount {
    Syncing,
    Unavailable,
    Known { count: usize, exact: bool },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrayCounts {
    unread: Option<usize>,
    pending: PendingApprovalCount,
    pending_ids: HashSet<String>,
    resolved_ids: HashSet<String>,
    approval_revision: u64,
    snapshot_revision: u64,
}

impl Default for TrayCounts {
    fn default() -> Self {
        Self {
            unread: Some(0),
            pending: PendingApprovalCount::Syncing,
            pending_ids: HashSet::new(),
            resolved_ids: HashSet::new(),
            approval_revision: 0,
            snapshot_revision: 0,
        }
    }
}

impl TrayCounts {
    pub fn unread(&self) -> Option<usize> {
        self.unread
    }

    pub fn pending(&self) -> &PendingApprovalCount {
        &self.pending
    }

    #[cfg(any(target_os = "windows", test))]
    pub fn badge_count(&self) -> Option<usize> {
        let unread = self.unread?;
        let pending = match self.pending {
            PendingApprovalCount::Known { count, .. } => count,
            PendingApprovalCount::Syncing | PendingApprovalCount::Unavailable => 0,
        };
        Some(unread.saturating_add(pending))
    }

    pub fn clear_unread(&mut self) {
        self.unread = Some(0);
    }

    pub fn on_window_focus(&mut self, focused: bool, event_stream_connected: bool) {
        if focused {
            self.clear_unread();
        } else if !event_stream_connected {
            self.unread = None;
        }
    }

    pub fn on_stream_disconnect_in_background(&mut self, main_window_in_background: bool) {
        if main_window_in_background {
            self.unread = None;
        }
        self.pending_refresh_failed();
    }

    pub fn begin_pending_refresh(&mut self) {
        let _ = self.start_approval_refresh();
        self.pending = PendingApprovalCount::Syncing;
        self.pending_ids.clear();
        self.resolved_ids.clear();
    }

    pub fn start_approval_refresh(&mut self) -> (u64, u64) {
        self.snapshot_revision = self.snapshot_revision.wrapping_add(1);
        (self.approval_revision, self.snapshot_revision)
    }

    pub fn pending_refresh_failed(&mut self) {
        self.snapshot_revision = self.snapshot_revision.wrapping_add(1);
        self.pending = PendingApprovalCount::Unavailable;
        self.pending_ids.clear();
        self.resolved_ids.clear();
    }

    pub fn set_pending_snapshot(&mut self, ids: impl IntoIterator<Item = String>) {
        self.pending_ids = ids.into_iter().filter(|id| !id.is_empty()).collect();
        let count = self.pending_ids.len();
        self.pending = PendingApprovalCount::Known {
            count,
            exact: count < APPROVAL_SNAPSHOT_LIMIT,
        };
        self.resolved_ids.clear();
    }

    pub fn finish_approval_refresh(
        &mut self,
        ids: impl IntoIterator<Item = String>,
        approval_revision: u64,
        snapshot_revision: u64,
    ) -> bool {
        if approval_revision != self.approval_revision
            || snapshot_revision != self.snapshot_revision
        {
            return false;
        }
        self.set_pending_snapshot(ids);
        true
    }

    pub fn fail_approval_refresh(
        &mut self,
        approval_revision: u64,
        snapshot_revision: u64,
    ) -> bool {
        if approval_revision != self.approval_revision
            || snapshot_revision != self.snapshot_revision
        {
            return false;
        }
        self.pending_refresh_failed();
        true
    }

    pub fn reset_for_scope(&mut self) {
        self.unread = Some(0);
        self.begin_pending_refresh();
    }

    /// Applies count-relevant events only. A completed run accrues only while the main
    /// workspace is outside the foreground; missed events during a disconnect remain unknown.
    pub fn record_event(
        &mut self,
        kind: &str,
        payload: &serde_json::Value,
        main_window_in_background: bool,
    ) -> bool {
        match kind {
            "run.finished" | "run.error" if main_window_in_background => {
                if let Some(unread) = &mut self.unread {
                    *unread = unread.saturating_add(1);
                }
                true
            }
            "approval.required" => {
                let Some(id) = payload.get("approvalId").and_then(|value| value.as_str()) else {
                    return false;
                };
                if id.is_empty() { return false; }
                self.approval_revision = self.approval_revision.wrapping_add(1);
                let PendingApprovalCount::Known { count, .. } = &mut self.pending else {
                    return false;
                };
                if !self.pending_ids.insert(id.to_owned()) {
                    return false;
                }
                *count = count.saturating_add(1);
                true
            }
            "approval.resolved" => {
                let Some(id) = payload.get("approvalId").and_then(|value| value.as_str()) else {
                    return false;
                };
                if id.is_empty() { return false; }
                self.approval_revision = self.approval_revision.wrapping_add(1);
                let PendingApprovalCount::Known { count, .. } = &mut self.pending else {
                    return false;
                };
                if !self.resolved_ids.insert(id.to_owned()) {
                    return false;
                }
                let was_tracked = self.pending_ids.remove(id);
                if was_tracked {
                    *count = count.saturating_sub(1);
                    true
                } else {
                    false
                }
            }
            _ => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrayMenuLabels {
    pub workspace: String,
    pub approvals: String,
    pub tooltip: String,
}

pub fn tray_menu_labels(counts: &TrayCounts) -> TrayMenuLabels {
    let unread = counts.unread().map(display_menu_count);
    let workspace = match unread.as_deref() {
        Some("0") => "打开桌面工作区".to_owned(),
        Some(count) => format!("打开桌面工作区 · {count} 未读"),
        None => "打开桌面工作区 · 未读未同步".to_owned(),
    };
    let approvals = match *counts.pending() {
        PendingApprovalCount::Syncing => "待审批 · 同步中".to_owned(),
        PendingApprovalCount::Unavailable => "待审批 · 暂不可用".to_owned(),
        PendingApprovalCount::Known { count, exact: true } => format!("待审批 · {count}"),
        PendingApprovalCount::Known { count, exact: false } => {
            format!("待审批 · 至少 {count}")
        }
    };
    let pending_tooltip = match *counts.pending() {
        PendingApprovalCount::Syncing => "审批同步中".to_owned(),
        PendingApprovalCount::Unavailable => "审批数暂不可用".to_owned(),
        PendingApprovalCount::Known { count, exact: true } => format!("待审批 {count}"),
        PendingApprovalCount::Known { count, exact: false } => format!("待审批至少 {count}"),
    };
    TrayMenuLabels {
        workspace,
        approvals,
        tooltip: format!(
            "EXMACHINA · {} 未读 · {pending_tooltip}",
            unread.as_deref().unwrap_or("未同步")
        ),
    }
}

fn display_menu_count(count: usize) -> String {
    count.to_string()
}

#[cfg(any(target_os = "windows", test))]
fn display_badge_count(count: usize) -> String {
    if count > 99 { "99+".to_owned() } else { count.to_string() }
}

/// Composite a small numeric badge into the top-right of the app icon. The host uses this
/// for Windows' tray badge; other platforms use the live menu labels as their fallback.
#[cfg(any(target_os = "windows", test))]
pub fn render_tray_badge(icon: &tauri::image::Image<'_>, count: usize) -> tauri::image::Image<'static> {
    let (mut rgba, width, height) = tray_icon_pixels(icon);
    let width = width as i32;
    let height = height as i32;
    let radius = (width.min(height) / 3).min(20);
    if count == 0 || radius < 6 || rgba.len() != (width * height * 4) as usize {
        return tauri::image::Image::new_owned(rgba, width as u32, height as u32);
    }

    let center_x = width - radius - 1;
    let center_y = radius + 1;
    let outer_radius_sq = radius * radius;
    let inner_radius_sq = (radius - 1) * (radius - 1);
    for y in (center_y - radius).max(0)..=(center_y + radius).min(height - 1) {
        for x in (center_x - radius).max(0)..=(center_x + radius).min(width - 1) {
            let dx = x - center_x;
            let dy = y - center_y;
            let distance = dx * dx + dy * dy;
            let color = if distance <= inner_radius_sq {
                [220, 38, 38, 255]
            } else if distance <= outer_radius_sq {
                [255, 255, 255, 255]
            } else {
                continue;
            };
            put_pixel(&mut rgba, width, x, y, color);
        }
    }

    let text = display_badge_count(count);
    let text_width = text.chars().count() as i32 * 4 - 1;
    let start_x = center_x - text_width / 2;
    let start_y = center_y - 2;
    for (index, character) in text.chars().enumerate() {
        if let Some(rows) = badge_glyph(character) {
            for (row, bits) in rows.iter().enumerate() {
                for column in 0..3 {
                    if bits & (1 << (2 - column)) != 0 {
                        put_pixel(
                            &mut rgba,
                            width,
                            start_x + index as i32 * 4 + column,
                            start_y + row as i32,
                            [255, 255, 255, 255],
                        );
                    }
                }
            }
        }
    }
    tauri::image::Image::new_owned(rgba, width as u32, height as u32)
}

#[cfg(any(target_os = "windows", test))]
fn tray_icon_pixels(icon: &tauri::image::Image<'_>) -> (Vec<u8>, u32, u32) {
    let source_width = icon.width();
    let source_height = icon.height();
    let width = source_width.min(32);
    let height = source_height.min(32);
    if width == source_width && height == source_height {
        return (icon.rgba().to_vec(), width, height);
    }
    let source = icon.rgba();
    let mut rgba = vec![0; (width * height * 4) as usize];
    for y in 0..height {
        for x in 0..width {
            let sx = x * source_width / width;
            let sy = y * source_height / height;
            let source_offset = ((sy * source_width + sx) * 4) as usize;
            let target_offset = ((y * width + x) * 4) as usize;
            if let (Some(pixel), Some(sample)) = (
                rgba.get_mut(target_offset..target_offset + 4),
                source.get(source_offset..source_offset + 4),
            ) {
                pixel.copy_from_slice(sample);
            }
        }
    }
    (rgba, width, height)
}

#[cfg(any(target_os = "windows", test))]
fn put_pixel(rgba: &mut [u8], width: i32, x: i32, y: i32, color: [u8; 4]) {
    if x < 0 || y < 0 {
        return;
    }
    let offset = ((y * width + x) * 4) as usize;
    if let Some(pixel) = rgba.get_mut(offset..offset + 4) {
        pixel.copy_from_slice(&color);
    }
}

#[cfg(any(target_os = "windows", test))]
fn badge_glyph(character: char) -> Option<[u8; 5]> {
    match character {
        '0' => Some([0b111, 0b101, 0b101, 0b101, 0b111]),
        '1' => Some([0b010, 0b110, 0b010, 0b010, 0b111]),
        '2' => Some([0b111, 0b001, 0b111, 0b100, 0b111]),
        '3' => Some([0b111, 0b001, 0b111, 0b001, 0b111]),
        '4' => Some([0b101, 0b101, 0b111, 0b001, 0b001]),
        '5' => Some([0b111, 0b100, 0b111, 0b001, 0b111]),
        '6' => Some([0b111, 0b100, 0b111, 0b101, 0b111]),
        '7' => Some([0b111, 0b001, 0b010, 0b010, 0b010]),
        '8' => Some([0b111, 0b101, 0b111, 0b101, 0b111]),
        '9' => Some([0b111, 0b101, 0b111, 0b001, 0b111]),
        '+' => Some([0b000, 0b010, 0b111, 0b010, 0b000]),
        _ => None,
    }
}

/// JavaScript run in the existing desktop webview when a tray destination is selected.
/// This keeps navigation in the native shell and reuses the existing chat/approval UI.
pub fn tray_navigation_script(open_approvals: bool) -> &'static str {
    if open_approvals {
        "(() => { let n = 0; const t = setInterval(() => { const nav = document.querySelector('[aria-label=\"查看对话\"]'); if (nav) { clearInterval(t); nav.click(); let m = 0; const s = setInterval(() => { const card = document.querySelector('.desk-center-pane:not([hidden]) .approval-card'); if (card) { clearInterval(s); card.scrollIntoView({ block: 'center', behavior: 'smooth' }); } else if (++m > 40) clearInterval(s); }, 100); } else if (++n > 40) clearInterval(t); }, 100); })()"
    } else {
        "document.querySelector('[aria-label=\"查看对话\"]')?.click()"
    }
}

// ---------------------------------------------------------------- 捆绑网关编排（6.2/6.3）

/// 构造网关子进程（独立数据目录；Windows 抑制控制台弹窗）。
/// 与 AppHandle 解耦——首启 / 健康监控重启 / 测试共用同一启动语义。
#[cfg(test)]
pub fn launch_gateway(
    gateway: &std::path::Path,
    data_dir: &std::path::Path,
    port: u16,
) -> std::io::Result<Child> {
    launch_gateway_authenticated(gateway, data_dir, port, None, "127.0.0.1")
}

pub fn launch_gateway_authenticated(
    gateway: &std::path::Path,
    data_dir: &std::path::Path,
    port: u16,
    key: Option<&str>,
    host: &str,
) -> std::io::Result<Child> {
    std::fs::create_dir_all(data_dir.join("data"))?;

    // 默认工作区 = 用户数据目录下的 workspace/（干活的项目根），
    // 绝不落在安装目录：agent 的终端/文件工具默认以此为根，搞不坏应用自身
    let workspace = data_dir.join("workspace");
    std::fs::create_dir_all(&workspace)?;

    let mut cmd = Command::new(gateway);
    cmd.arg("serve")
        .args(["--host", host])
        .arg("--port")
        .arg(port.to_string())
        .env("EXM_DATA_DIR", data_dir.join("data"))
        .env("EXM_WORKSPACE_ROOT", &workspace)
        .current_dir(&workspace);
    if let Some(key) = key { cmd.env("EXM_AUTH_KEY", key); }
    // Windows：网关是控制台子系统程序，GUI 进程无控制台可继承 → 系统会为其新开终端窗口；
    // CREATE_NO_WINDOW (0x0800_0000) 抑制弹窗（日志经事件桥与 WebUI 可见，无需终端）
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    cmd.spawn()
}

/// 就绪等待：TCP 可连即视为就绪（最多 30s；提前退出立即报错）。
/// 返回 Ok 后子进程由调用方托管（壳：GatewayChild 槽位；测试：直接持有）。
pub fn wait_gateway_ready(child: &mut Child, port: u16) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Ok(Some(_)) = child.try_wait() {
            let _ = child.kill();
            return Err("exm-gateway 进程提前退出（多为模型通道未配置或端口被占，详见数据目录日志）".into());
        }
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return Ok(());
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            return Err("网关 30s 内未就绪".into());
        }
        std::thread::sleep(Duration::from_millis(300));
    }
}

/// 从网关 URL 提取端口（重启复用原端口的解析口径）
pub fn port_of_url(url: &str) -> Option<u16> {
    url.rsplit_once(':').and_then(|(_, p)| p.parse::<u16>().ok())
}

#[cfg(test)]
mod gateway_tests {
    use super::*;

    #[test]
    fn 端口解析_正常与畸形() {
        assert_eq!(port_of_url("http://127.0.0.1:58501"), Some(58501));
        assert_eq!(port_of_url("http://192.168.1.5:4173"), Some(4173));
        assert_eq!(port_of_url("http://127.0.0.1"), None);
        assert_eq!(port_of_url("not-a-url"), None);
    }

    /// 恢复链路集成测试（6.3 验收）：真网关二进制 spawn → kill → 同端口 respawn →
    /// HTTP 服务恢复。EXM_GATEWAY_BIN 指向仓库构建产物（verify 脚本设置）；
    /// 未设置时跳过（打包环境无仓库二进制）。验证与 UI 无关的核心编排链：
    /// 端口复用 = 重启后 UI/事件桥寻址不变的前提。
    #[test]
    fn 网关恢复链路_kill后同端口重启服务恢复() {
        let Ok(bin) = std::env::var("EXM_GATEWAY_BIN") else {
            eprintln!("跳过：未设置 EXM_GATEWAY_BIN（需指向 exm-gateway 二进制）");
            return;
        };
        let bin = std::path::Path::new(&bin);
        if !bin.is_file() {
            eprintln!("跳过：EXM_GATEWAY_BIN 不存在（{bin:?}）");
            return;
        }
        // 独立数据目录（不碰真实用户数据）
        let data_dir = std::env::temp_dir().join(format!("exm-desktop-gw-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&data_dir);
        let port = {
            std::net::TcpListener::bind(("127.0.0.1", 0))
                .and_then(|l| l.local_addr())
                .map(|a| a.port())
                .unwrap()
        };
        let spawn_gw = || {
            let mut child = launch_gateway(bin, &data_dir, port).expect("网关拉起失败");
            wait_gateway_ready(&mut child, port).expect("网关就绪超时");
            child
        };

        // ① 首次拉起 → HTTP 探活
        let mut child = spawn_gw();
        assert!(http_ok(port), "首启后 /api/health 应可访问");

        // ② kill（模拟网关崩溃）
        child.kill().expect("kill 失败");
        let _ = child.wait();
        std::thread::sleep(Duration::from_millis(300)); // 端口释放窗口
        assert!(!http_ok(port), "kill 后服务应不可达");

        // ③ 同端口重启（gateway_restart 的编排路径）→ 服务恢复
        let mut child2 = spawn_gw();
        assert!(http_ok(port), "同端口重启后 /api/health 应恢复");
        // 收尾：终止子进程 + 清理临时目录
        let _ = child2.kill();
        let _ = child2.wait();
        let _ = std::fs::remove_dir_all(&data_dir);
    }

    /// HTTP 探活：GET /api/health 返回 2xx
    fn http_ok(port: u16) -> bool {
        std::net::TcpStream::connect(("127.0.0.1", port))
            .ok()
            .and_then(|mut s| {
                use std::io::{Read, Write};
                let req = format!("GET /api/health HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n");
                s.write_all(req.as_bytes()).ok()?;
                let mut buf = String::new();
                s.read_to_string(&mut buf).ok()?;
                buf.starts_with("HTTP/1.1 2").then_some(true)
            })
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn 审批事件映射为审批通知() {
        let cfg = DesktopConfig::default();
        let n = map_event("approval.required", &json!({ "agentId": "coding-agent", "command": "cargo publish" }), &cfg).unwrap();
        assert_eq!(n.category, "approval");
        assert!(n.body.contains("coding-agent") && n.body.contains("cargo publish"));
        // 类别开关关闭 → 不通知
        let off = DesktopConfig { notify_approval: false, ..Default::default() };
        assert!(map_event("approval.required", &json!({}), &off).is_none(), "类别开关生效");
    }

    #[test]
    fn 定时任务与任务完成映射() {
        let cfg = DesktopConfig::default();
        let n = map_event("cron.finished", &json!({ "jobName": "每日报表", "status": "failed", "summary": "模型未配置" }), &cfg).unwrap();
        assert_eq!(n.category, "cron");
        assert!(n.title.contains("每日报表") && n.title.contains("失败"));
        assert!(n.body.contains("模型未配置"));

        let n = map_event(
            "run.finished",
            &json!({ "statements": [{ "tag": "报告", "text": "重构完成，全部测试通过" }] }),
            &cfg,
        )
        .unwrap();
        assert_eq!(n.category, "task_done");
        assert!(n.body.contains("全部测试通过"));

        // 任务完成开关关闭 → 不通知；审批不受影响
        let off_done = DesktopConfig { notify_task_done: false, ..Default::default() };
        assert!(map_event("run.finished", &json!({ "statements": [] }), &off_done).is_none());
        assert!(map_event("approval.required", &json!({}), &off_done).is_some(), "类别开关互不影响");
    }

    #[test]
    fn 无关事件与超长正文() {
        let cfg = DesktopConfig::default();
        assert!(map_event("graph.updated", &json!({}), &cfg).is_none(), "无关事件不映射");
        assert!(map_event("tool.call", &json!({}), &cfg).is_none());
        let long: String = "长".repeat(500);
        let n = map_event("run.finished", &json!({ "statements": [{ "text": long }] }), &cfg).unwrap();
        assert!(n.body.chars().count() <= 120, "正文封顶: {}", n.body.chars().count());
    }

    #[test]
    fn 托盘未读计数只累积后台完成并在聚焦时清零() {
        let mut counts = TrayCounts::default();
        assert!(!counts.record_event("run.finished", &json!({}), false));
        assert!(counts.record_event("run.finished", &json!({}), true));
        assert!(counts.record_event("run.error", &json!({}), true));
        assert_eq!(counts.unread(), Some(2));

        counts.on_window_focus(false, false);
        assert_eq!(counts.unread(), None, "断流期间失焦时不伪造精确未读数");
        counts.on_window_focus(true, false);
        assert_eq!(counts.unread(), Some(0), "用户回到窗口后当前未读归零");
    }

    #[test]
    fn 托盘审批计数由快照建立并按真实单号去重更新() {
        let mut counts = TrayCounts::default();
        let (approval_revision, snapshot_revision) = counts.start_approval_refresh();
        assert!(counts.finish_approval_refresh(
            ["a1".to_owned(), "a2".to_owned()],
            approval_revision,
            snapshot_revision,
        ));
        assert_eq!(counts.pending(), &PendingApprovalCount::Known { count: 2, exact: true });

        let required = json!({ "approvalId": "a3" });
        assert!(counts.record_event("approval.required", &required, false));
        assert!(!counts.record_event("approval.required", &required, false));
        assert_eq!(counts.pending(), &PendingApprovalCount::Known { count: 3, exact: true });

        let resolved = json!({ "approvalId": "a2" });
        assert!(counts.record_event("approval.resolved", &resolved, false));
        assert!(!counts.record_event("approval.resolved", &resolved, false));
        assert_eq!(counts.pending(), &PendingApprovalCount::Known { count: 2, exact: true });
    }

    #[test]
    fn 未跟踪到的解决事件不减少快照计数() {
        let mut counts = TrayCounts::default();
        counts.set_pending_snapshot((0..APPROVAL_SNAPSHOT_LIMIT).map(|id| id.to_string()));
        assert_eq!(counts.pending(), &PendingApprovalCount::Known {
            count: APPROVAL_SNAPSHOT_LIMIT,
            exact: false,
        });

        assert!(!counts.record_event(
            "approval.resolved",
            &json!({ "approvalId": "not-in-snapshot" }),
            false,
        ));
        assert_eq!(counts.pending(), &PendingApprovalCount::Known {
            count: APPROVAL_SNAPSHOT_LIMIT,
            exact: false,
        });
    }

    #[test]
    fn 断流和并发刷新不展示过期审批计数() {
        let mut counts = TrayCounts::default();
        let (event_revision, first_refresh) = counts.start_approval_refresh();
        assert!(counts.finish_approval_refresh(
            ["a1".to_owned()],
            event_revision,
            first_refresh,
        ));
        counts.on_stream_disconnect_in_background(true);
        assert_eq!(counts.pending(), &PendingApprovalCount::Unavailable);

        let (event_revision, first_refresh) = counts.start_approval_refresh();
        let (same_event_revision, latest_refresh) = counts.start_approval_refresh();
        assert!(!counts.finish_approval_refresh(
            ["stale".to_owned()],
            event_revision,
            first_refresh,
        ));
        assert!(counts.finish_approval_refresh(
            ["fresh".to_owned()],
            same_event_revision,
            latest_refresh,
        ));
        assert!(!counts.finish_approval_refresh(
            ["stale".to_owned()],
            event_revision,
            first_refresh,
        ));
        assert_eq!(counts.pending(), &PendingApprovalCount::Known { count: 1, exact: true });
    }

    #[test]
    fn 托盘菜单标签对同步状态和平台回退安全() {
        let mut counts = TrayCounts::default();
        let labels = tray_menu_labels(&counts);
        assert_eq!(labels.workspace, "打开桌面工作区");
        assert_eq!(labels.approvals, "待审批 · 同步中");
        assert!(labels.tooltip.contains("审批同步中"));

        counts.set_pending_snapshot(["a1".to_owned(), "a2".to_owned()]);
        counts.record_event("run.finished", &json!({}), true);
        let labels = tray_menu_labels(&counts);
        assert_eq!(labels.workspace, "打开桌面工作区 · 1 未读");
        assert_eq!(labels.approvals, "待审批 · 2");
        assert!(!labels.tooltip.contains("a1"), "标签不暴露审批 ID 或命令");

        counts.pending_refresh_failed();
        assert_eq!(tray_menu_labels(&counts).approvals, "待审批 · 暂不可用");
    }

    #[test]
    fn windows托盘徽标只修改图标像素并限制文本宽度() {
        let source_rgba = vec![0_u8; 32 * 32 * 4];
        let source = tauri::image::Image::new(&source_rgba, 32, 32);
        let no_badge = render_tray_badge(&source, 0);
        let badge = render_tray_badge(&source, 123);
        assert_eq!(no_badge.width(), 32);
        assert_eq!(badge.height(), 32);
        assert_eq!(no_badge.rgba().iter().filter(|&&byte| byte != 0).count(), 0);
        assert!(badge.rgba().iter().filter(|&&byte| byte != 0).count() > 0);
        assert_eq!(display_badge_count(123), "99+");
        let mut known = TrayCounts::default();
        known.set_pending_snapshot(["p1".to_owned()]);
        assert_eq!(known.badge_count(), Some(1));
        known.pending_refresh_failed();
        assert_eq!(known.badge_count(), Some(0), "已知未读为零时不显示未知审批数");
        known.record_event("run.finished", &json!({}), true);
        assert_eq!(known.badge_count(), Some(1), "审批未知时仍显示准确的未读计数");
    }

    #[test]
    fn 托盘审批菜单定位到现有审批卡片() {
        let script = tray_navigation_script(true);
        assert!(script.contains("查看对话"));
        assert!(script.contains(".approval-card"));
        assert!(script.contains("scrollIntoView"));
        assert_eq!(tray_navigation_script(false), "document.querySelector('[aria-label=\"查看对话\"]')?.click()");
    }
}
