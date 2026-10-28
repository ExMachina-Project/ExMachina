//! 进程插件桥集成测试（任务 2.4 e2e）：
//! ① 杀进程自动恢复（health_check 驱动重启）
//! ② 宿主关闭全量回收无孤儿进程
//! ③ 全链路：装载 → 握手 → tools/list 注册 → tools/call 往返

use exm_core::plugin::bridge::{PluginState, PluginSupervisor};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

/// mock 插件脚本：把自身 pid 写入 pid 文件（孤儿检测用），全协议应答
const MOCK: &str = r#"
import sys, json, os
open("pid","w").write(str(os.getpid()))
for line in sys.stdin:
    line = line.strip()
    if not line: continue
    req = json.loads(line)
    m, rid = req.get("method"), req.get("id")
    if m == "initialize":
        resp = {"jsonrpc":"2.0","id":rid,"result":{"serverInfo":{"name":"mock"}}}
    elif m == "tools/list":
        resp = {"jsonrpc":"2.0","id":rid,"result":{"tools":[
            {"name":"echo","description":"回声","inputSchema":{"type":"object","properties":{"q":{"type":"string"}}}}
        ]}}
    elif m == "tools/call":
        q = (req["params"].get("arguments") or {}).get("q","")
        resp = {"jsonrpc":"2.0","id":rid,"result":{"content":[{"type":"text","text":"echo:"+str(q)}]}}
    elif m == "shutdown":
        sys.exit(0)
    else:
        continue
    sys.stdout.write(json.dumps(resp)+"\n"); sys.stdout.flush()
"#;

fn mock_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("exm-bridge-e2e-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("mock.py"), MOCK).unwrap();
    dir
}

fn pid_alive(pid: u32) -> bool {
    // 信号 0 探活：进程不存在时 kill 返回 -1
    unsafe { libc_kill(pid) }
}

#[cfg(unix)]
unsafe fn libc_kill(pid: u32) -> bool {
    // 直接用系统调用探活，避免引 libc 依赖
    let r = std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    r.map(|s| s.success()).unwrap_or(false)
}

#[cfg(not(unix))]
unsafe fn libc_kill(_pid: u32) -> bool {
    false
}

fn read_pid(dir: &PathBuf) -> Option<u32> {
    std::fs::read_to_string(dir.join("pid")).ok()?.trim().parse().ok()
}

/// 场景①：全链路——拉起 → 握手 → tools/list → tools/call 往返
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn 场景1_进程插件全链路往返() {
    let dir = mock_dir("full");
    let s = PluginSupervisor::new(
        "mock",
        dir.clone(),
        vec!["python3".into(), "mock.py".into()],
        BTreeMap::new(),
        10,
        15,
        4,
    );
    s.start().await.expect("启动握手");
    assert_eq!(s.state(), PluginState::Running);
    let r = s.call_tool("echo", serde_json::json!({ "q": "桥" })).await.unwrap();
    assert_eq!(r.text(), "echo:桥");
    s.stop().await;
    let _ = std::fs::remove_dir_all(&dir);
}

/// 场景②：宿主关闭全量回收——shutdown 后进程退出，无孤儿
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn 场景2_宿主关闭无孤儿进程() {
    let dir = mock_dir("reap");
    let s = std::sync::Arc::new(PluginSupervisor::new(
        "mock",
        dir.clone(),
        vec!["python3".into(), "mock.py".into()],
        BTreeMap::new(),
        10,
        15,
        4,
    ));
    s.start().await.unwrap();
    let pid = read_pid(&dir).expect("mock 应写入 pid");
    assert!(pid_alive(pid), "运行中进程应存活");
    // 宿主关闭：全量回收
    exm_core::plugin::bridge::shutdown_all(&[s.clone()]).await;
    // 给 OS 一点时间收尸
    for _ in 0..20 {
        if !pid_alive(pid) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(!pid_alive(pid), "关闭后不应残留孤儿进程（pid={pid}）");
    let _ = std::fs::remove_dir_all(&dir);
}

/// 场景③：进程被杀后 health_check 驱动自动恢复
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn 场景3_杀进程自动恢复() {
    let dir = mock_dir("kill");
    let s = std::sync::Arc::new(PluginSupervisor::new(
        "mock",
        dir.clone(),
        vec!["python3".into(), "mock.py".into()],
        BTreeMap::new(),
        10,
        15,
        4,
    ));
    s.start().await.unwrap();
    let pid1 = read_pid(&dir).expect("pid 文件");
    // 外部强杀（SIGKILL）：模拟崩溃
    let _ = std::process::Command::new("kill")
        .args(["-9", &pid1.to_string()])
        .status();
    tokio::time::sleep(Duration::from_millis(300)).await;
    // 健康检查发现死亡 → 重启
    let ok = s.health_check().await;
    assert!(ok, "health_check 应完成恢复");
    assert_eq!(s.state(), PluginState::Running);
    // 新进程新 pid
    let pid2 = loop {
        if let Some(p) = read_pid(&dir) {
            if p != pid1 {
                break p;
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert_ne!(pid1, pid2, "重启应产生新进程");
    // 恢复后调用面健康
    let r = s.call_tool("echo", serde_json::json!({ "q": "after-kill" })).await.unwrap();
    assert_eq!(r.text(), "echo:after-kill");
    s.stop().await;
    let _ = std::fs::remove_dir_all(&dir);
}
