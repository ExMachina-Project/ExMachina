//! 桌面端配置与事件映射（组 11）
//!
//! - `DesktopConfig`：托盘驻留 / 全局快捷键 / 通知类别开关（`<数据目录>/ExMachina/desktop.json`）
//! - `map_event`：网关 WS 事件 → 原生通知载荷的纯函数映射（事件映射单测覆盖，D11 零新事件通路）

use serde::{Deserialize, Serialize};
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
}

impl Default for DesktopConfig {
    fn default() -> Self {
        DesktopConfig {
            exit_on_close: false, // 关窗驻留默认开（spec）
            shortcut: "Alt+Shift+E".into(),
            notify_approval: true,
            notify_cron: true,
            notify_task_done: true,
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

// ---------------------------------------------------------------- 捆绑网关编排（6.2/6.3）

/// 构造网关子进程（独立数据目录；Windows 抑制控制台弹窗）。
/// 与 AppHandle 解耦——首启 / 健康监控重启 / 测试共用同一启动语义。
pub fn launch_gateway(
    gateway: &std::path::Path,
    data_dir: &std::path::Path,
    port: u16,
) -> std::io::Result<Child> {
    std::fs::create_dir_all(data_dir.join("data"))?;

    // 默认工作区 = 用户数据目录下的 workspace/（干活的项目根），
    // 绝不落在安装目录：agent 的终端/文件工具默认以此为根，搞不坏应用自身
    let workspace = data_dir.join("workspace");
    std::fs::create_dir_all(&workspace)?;

    let mut cmd = Command::new(gateway);
    cmd.arg("serve")
        .arg("--port")
        .arg(port.to_string())
        .env("EXM_DATA_DIR", data_dir.join("data"))
        .env("EXM_WORKSPACE_ROOT", &workspace)
        .current_dir(&workspace);
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
}
