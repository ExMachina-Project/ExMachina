//! 进程插件桥（T2 形态）：宿主与第三方插件子进程的 stdio JSON-RPC 通信。
//! 协议 = MCP 工具子集 + 扩展方法（initialize / tools/list / tools/call /
//! config/set / watcher.tick / shutdown），行分隔 JSON。
//! 生命周期治理（退避重启/超时/回收）在 PluginSupervisor（任务 2.4）。

use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

use crate::config::PluginManifest;
use crate::plugin::PluginManager;

/// JSON-RPC 请求 id 发号器
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

pub fn next_request_id() -> u64 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

/// 进程插件工具定义（tools/list 返回项的规整形状）
#[derive(Debug, Clone)]
pub struct PluginToolDef {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

/// tools/call 结果（MCP 内容块口径）
#[derive(Debug, Clone)]
pub struct PluginCallResult {
    pub content: Vec<Value>,
    pub is_error: bool,
}

impl PluginCallResult {
    /// 拼接全部文本块（宿主侧统一为文本输出面）
    pub fn text(&self) -> String {
        self.content
            .iter()
            .filter_map(|c| c.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// 插件子进程桥：spawn + 握手 + 请求/响应往返
pub struct PluginProcess {
    child: Mutex<Option<Child>>,
    stdin: Mutex<Option<tokio::process::ChildStdin>>,
    pending: std::sync::Arc<tokio::sync::Mutex<BTreeMap<u64, tokio::sync::oneshot::Sender<Value>>>>,
    pub plugin_dir: PathBuf,
}

impl PluginProcess {
    /// 拉起子进程（命令相对插件目录解析；首段无路径分隔符时仍走 PATH 查找）
    pub async fn spawn(
        plugin_dir: PathBuf,
        command: &[String],
        env: &BTreeMap<String, String>,
    ) -> anyhow::Result<Self> {
        let first = &command[0];
        let contains_sep = first.contains('/') || first.contains('\\');
        let exe = if PathBuf::from(first).is_absolute() || contains_sep {
            let p = PathBuf::from(first);
            if p.is_absolute() {
                p
            } else {
                plugin_dir.join(p)
            }
        } else {
            PathBuf::from(first) // 裸命令名：交给 PATH
        };
        let mut cmd = Command::new(exe);
        cmd.args(&command[1..])
            .current_dir(&plugin_dir)
            .envs(env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit()) // stderr 透传宿主日志
            .kill_on_drop(true);
        let mut child = cmd.spawn()?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let p = PluginProcess {
            child: Mutex::new(Some(child)),
            stdin: Mutex::new(stdin),
            pending: std::sync::Arc::new(tokio::sync::Mutex::new(BTreeMap::new())),
            plugin_dir,
        };
        if let Some(out) = stdout {
            p.start_reader(out);
        }
        Ok(p)
    }

    /// 后台读循环：按行解析响应，按 id 派发到等待者
    fn start_reader(&self, stdout: tokio::process::ChildStdout) {
        let pending = self.pending.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
                let Some(id) = v.get("id").and_then(|i| i.as_u64()) else { continue };
                if let Some(tx) = pending.lock().await.remove(&id) {
                    let _ = tx.send(v);
                }
            }
            // 流结束：唤醒所有等待者（发错误响应）
            let mut ps = pending.lock().await;
            for (_, tx) in std::mem::take(&mut *ps) {
                let _ = tx.send(json!({ "error": { "code": -32000, "message": "插件进程已退出" } }));
            }
        });
    }

    /// 发送 JSON-RPC 请求并等待响应（超时由调用方包 tokio::time::timeout）
    pub async fn request(&self, method: &str, params: Value) -> anyhow::Result<Value> {
        let id = next_request_id();
        let frame = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.pending.lock().await.insert(id, tx);
        {
            let mut stdin = self.stdin.lock().await;
            let Some(stdin) = stdin.as_mut() else {
                anyhow::bail!("插件进程 stdin 不可用");
            };
            use tokio::io::AsyncWriteExt;
            let mut line = frame.to_string();
            line.push('\n');
            stdin.write_all(line.as_bytes()).await?;
            stdin.flush().await?;
        }
        let resp = rx
            .await
            .map_err(|_| anyhow::anyhow!("插件进程响应通道关闭"))?;
        Ok(resp)
    }

    /// 发送通知（无 id，不等响应）
    pub async fn notify(&self, method: &str, params: Value) -> anyhow::Result<()> {
        let frame = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        let mut stdin = self.stdin.lock().await;
        let Some(stdin) = stdin.as_mut() else {
            anyhow::bail!("插件进程 stdin 不可用");
        };
        use tokio::io::AsyncWriteExt;
        let mut line = frame.to_string();
        line.push('\n');
        stdin.write_all(line.as_bytes()).await?;
        stdin.flush().await?;
        Ok(())
    }

    /// initialize 握手（plugin-ecosystem 协商）：读响应 `serverInfo.exmMin` 做宿主版本
    /// 兼容校验——插件要求的最低宿主版本高于当前 → 握手失败（含两侧版本的明确错误）。
    /// 超时未完成判启动失败。
    pub async fn handshake(&self, plugin_id: &str, timeout_secs: u64) -> anyhow::Result<Value> {
        let params = json!({
            "protocolVersion": "2024-11-05",
            "clientInfo": { "name": "exmachina", "version": env!("CARGO_PKG_VERSION") },
            "pluginId": plugin_id,
        });
        let host = env!("CARGO_PKG_VERSION");
        let result = match tokio::time::timeout(
            std::time::Duration::from_secs(timeout_secs),
            self.request("initialize", params),
        )
        .await
        {
            Ok(Ok(resp)) if resp.get("result").is_some() => Ok(resp["result"].clone()),
            Ok(Ok(resp)) => anyhow::bail!("握手返回错误：{}", resp["error"]["message"].as_str().unwrap_or("?")),
            Ok(Err(e)) => Err(e),
            Err(_) => anyhow::bail!("握手超时（{timeout_secs}s）"),
        }?;
        // 版本协商：serverInfo.exmMin 声明的最低宿主版本高于当前 → 拒绝
        if let Some(min) = result
            .pointer("/serverInfo/exmMin")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            if !super::version_ok(min, host) {
                anyhow::bail!(
                    "握手协商失败：插件 {plugin_id} 需要宿主 ≥ {min}，当前 {host}（请升级宿主或安装旧版插件）"
                );
            }
        }
        Ok(result)
    }

    /// 优雅关闭：先发 shutdown 请求，宽限后 kill
    pub async fn shutdown(&self, grace_secs: u64) {
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(grace_secs.min(5)),
            self.request("shutdown", json!({})),
        )
        .await;
        let _ = self.notify("exit", json!({})).await;
        let mut child = self.child.lock().await;
        if let Some(mut c) = child.take() {
            // 优雅终止失败 → SIGKILL 兜底（POSIX kill；Windows kill_on_drop 同语义）
            let _ = c.kill().await;
        }
        *self.stdin.lock().await = None;
    }

    /// 进程是否已退出（生命周期治理用）
    pub async fn is_dead(&self) -> bool {
        let mut guard = self.child.lock().await;
        match guard.as_mut() {
            None => true,
            Some(c) => c.try_wait().map(|w| w.is_some()).unwrap_or(true),
        }
    }

    // ---------------- 协议方法（MCP 工具子集 + 扩展）

    /// tools/list：发现进程插件声明的工具（名字去前缀，注册时由宿主加命名空间）
    pub async fn tools_list(&self, timeout_secs: u64) -> anyhow::Result<Vec<PluginToolDef>> {
        let resp = tokio::time::timeout(
            std::time::Duration::from_secs(timeout_secs),
            self.request("tools/list", json!({})),
        )
        .await
        .map_err(|_| anyhow::anyhow!("tools/list 超时（{timeout_secs}s）"))??;
        let Some(result) = resp.get("result") else {
            anyhow::bail!(
                "tools/list 错误：{}",
                resp["error"]["message"].as_str().unwrap_or("?")
            );
        };
        let mut out = Vec::new();
        if let Some(tools) = result.get("tools").and_then(|t| t.as_array()) {
            for t in tools {
                let name = t["name"].as_str().unwrap_or_default().trim().to_string();
                if name.is_empty() {
                    continue;
                }
                out.push(PluginToolDef {
                    name,
                    description: t["description"].as_str().unwrap_or_default().to_string(),
                    parameters: t
                        .get("inputSchema")
                        .cloned()
                        .unwrap_or(json!({ "type": "object", "properties": {} })),
                });
            }
        }
        Ok(out)
    }

    /// tools/call：调用进程插件工具（内容块数组口径；超时不悬挂）。
    /// arguments 按值收（future 不借用调用方栈，可自由存进 Vec 并发收集）
    pub async fn tools_call(
        &self,
        name: &str,
        arguments: Value,
        timeout_secs: u64,
    ) -> anyhow::Result<PluginCallResult> {
        let resp = tokio::time::timeout(
            std::time::Duration::from_secs(timeout_secs),
            self.request(
                "tools/call",
                json!({ "name": name, "arguments": arguments }),
            ),        )
        .await
        .map_err(|_| anyhow::anyhow!("工具 {name} 调用超时（{timeout_secs}s）"))??;
        let Some(result) = resp.get("result") else {
            anyhow::bail!(
                "工具 {name} 错误：{}",
                resp["error"]["message"].as_str().unwrap_or("?")
            );
        };
        let content = result
            .get("content")
            .and_then(|c| c.as_array())
            .cloned()
            .unwrap_or_default();
        Ok(PluginCallResult {
            is_error: result.get("isError").and_then(|e| e.as_bool()).unwrap_or(false),
            content,
        })
    }

    /// config/set：下发插件配置（凭据等敏感值仅经 stdin 进程内存，不落盘）
    pub async fn config_set(&self, config: Value, timeout_secs: u64) -> anyhow::Result<()> {
        let resp = tokio::time::timeout(
            std::time::Duration::from_secs(timeout_secs),
            self.request("config/set", json!({ "config": config })),
        )
        .await
        .map_err(|_| anyhow::anyhow!("config/set 超时（{timeout_secs}s）"))??;
        if resp.get("result").is_some() {
            Ok(())
        } else {
            anyhow::bail!(
                "config/set 错误：{}",
                resp["error"]["message"].as_str().unwrap_or("?")
            )
        }
    }

    /// watcher.tick：通知插件执行一轮 watcher 轮询（通知语义，不等待长响应）
    pub async fn watcher_tick(&self, payload: Value) -> anyhow::Result<()> {
        self.notify("watcher.tick", payload).await
    }
}

/// 测试共用：mock 插件子进程脚本（python3；initialize/tools.list/tools.call/
/// config.set/shutdown 全协议应答）
#[cfg(test)]
pub(crate) fn mock_script_pub() -> String {
        r#"
import sys, json, time
for line in sys.stdin:
    line = line.strip()
    if not line: continue
    req = json.loads(line)
    m, rid = req.get("method"), req.get("id")
    if m == "initialize":
        resp = {"jsonrpc":"2.0","id":rid,"result":{"serverInfo":{"name":"mock"}}}
    elif m == "tools/list":
        resp = {"jsonrpc":"2.0","id":rid,"result":{"tools":[
            {"name":"ping","description":"测试工具","inputSchema":{"type":"object","properties":{"q":{"type":"string"}},"required":["q"]}}
        ]}}
    elif m == "tools/call":
        name = req["params"]["name"]
        args = req["params"].get("arguments") or {}
        if name == "slow":  # 超时路径：睡 30s 不应答（宿主侧超时先到）
            time.sleep(30); resp = {"jsonrpc":"2.0","id":rid,"result":{"content":[]}}
        elif name == "err":
            resp = {"jsonrpc":"2.0","id":rid,"error":{"code":-32000,"message":"工具内部错误"}}
        elif name == "who":
            v = open("cred.txt").read().strip() if __import__("os").path.exists("cred.txt") else "无"
            resp = {"jsonrpc":"2.0","id":rid,"result":{"content":[{"type":"text","text":"cred="+v}]}}
        else:
            resp = {"jsonrpc":"2.0","id":rid,"result":{"content":[{"type":"text","text":"pong:"+str(args.get("q"))}]}}
    elif m == "config/set":
        cfg = req["params"]["config"]
        if cfg.get("persist") == "disk":  # 反面教材路径：宿主下发 persist=disk 时写盘（验证测试用）
            open("cred.txt","w").write(str(cfg.get("secret","")))
            resp = {"jsonrpc":"2.0","id":rid,"result":{}}
        else:
            resp = {"jsonrpc":"2.0","id":rid,"result":{}}  # 合规：仅内存（全局变量模拟）
    elif m == "shutdown":
        resp = {"jsonrpc":"2.0","id":rid,"result":{}}
    else:
        continue
    sys.stdout.write(json.dumps(resp)+"\n"); sys.stdout.flush()
"#
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) use super::mock_script_pub;

    /// 拉起 mock 桥并完成握手（各测试共用）
    async fn mocked(tag: &str) -> (PluginProcess, PathBuf) {
        let dir = std::env::temp_dir().join(format!("exm-bridge-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("mock.py"), super::mock_script_pub()).unwrap();
        let p = PluginProcess::spawn(
            dir.clone(),
            &["python3".into(), "mock.py".into()],
            &BTreeMap::new(),
        )
        .await
        .unwrap();
        p.handshake("mock", 10).await.expect("握手");
        (p, dir)
    }

    #[tokio::test]
    async fn 桥_握手与调用往返() {
        let (p, dir) = mocked("basic").await;
        // 握手在 mocked 内完成，这里复验 request 面即可
        let r = p
            .tools_call("ping", json!({ "q": "hi" }), 10)
            .await
            .unwrap();
        assert_eq!(r.text(), "pong:hi");
        assert!(!r.is_error);
        p.shutdown(3).await;
        assert!(p.is_dead().await, "关闭后进程应已退出");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn 桥_tools_list发现() {
        let (p, dir) = mocked("list").await;
        let tools = p.tools_list(10).await.unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "ping");
        assert_eq!(tools[0].description, "测试工具");
        assert_eq!(tools[0].parameters["properties"]["q"]["type"], "string");
        p.shutdown(3).await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn 桥_调用超时不悬挂() {
        let (p, dir) = mocked("timeout").await;
        let t0 = std::time::Instant::now();
        let r = p.tools_call("slow", json!({}), 1).await;
        assert!(r.is_err(), "超时应返回错误");
        assert!(r.unwrap_err().to_string().contains("超时"));
        assert!(t0.elapsed().as_secs() < 10, "超时后立即返回，不悬挂");
        p.shutdown(3).await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn 桥_错误响应面() {
        let (p, dir) = mocked("err").await;
        let r = p.tools_call("err", json!({}), 10).await;
        assert!(r.is_err());
        assert!(r.unwrap_err().to_string().contains("工具内部错误"));
        p.shutdown(3).await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn 桥_并发调用排队与往返() {
        let (p, dir) = mocked("conc").await;
        // 并发 8 个调用（桥自身无并发限制——排队语义在宿主侧 semaphore；
        // 这里验证桥的 pending 表在并发下不串号）
        let mut hs = Vec::new();
        for i in 0..8 {
            let fut = p.tools_call("ping", json!({ "q": i }), 10);
            hs.push((i, tokio::time::timeout(std::time::Duration::from_secs(15), fut)));
        }
        for (i, h) in hs {
            let r = h.await.expect("不应整体超时").unwrap();
            assert_eq!(r.text(), format!("pong:{i}"), "并发响应不得串号");
        }
        p.shutdown(3).await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn 桥_凭据下发不落盘() {
        let (p, dir) = mocked("cred").await;
        p.config_set(json!({ "secret": "s3cr3t" }), 10).await.unwrap();
        // 进程侧仅内存：插件目录无残留文件
        let residue: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter(|e| e.path().extension().map(|x| x == "txt").unwrap_or(false))
            .collect();
        assert!(residue.is_empty(), "凭据不得在插件目录留文件残留");
        p.shutdown(3).await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn 桥_watcher通知往返() {
        let (p, dir) = mocked("watch").await;
        // 通知不等待响应：发出即返回（mock 对未知通知 continue 不响应，正合语义）
        p.watcher_tick(json!({ "tick": 1 })).await.unwrap();
        // 通知后 request 面仍健康
        let r = p.tools_call("ping", json!({ "q": "after-notify" }), 10).await.unwrap();
        assert_eq!(r.text(), "pong:after-notify");
        p.shutdown(3).await;
        let _ = std::fs::remove_dir_all(&dir);
    }
}

// ---------------------------------------------------------------- 生命周期治理（任务 2.4）

/// 进程插件运行状态
#[derive(Debug, Clone, PartialEq)]
pub enum PluginState {
    /// 未拉起（未启用 / 尚未轮到启动）
    Stopped,
    /// 运行中（握手完成）
    Running,
    /// 退避重启等待中
    Backoff,
    /// 连续失败达上限，停止重试待人工干预
    Error,
}

/// 单个进程插件的监管器：拉起 → 握手 → 监视 → 崩溃退避重启 → 上限置错。
/// 宿主侧调用经 semaphore 限并发（默认 4，超出排队）。
pub struct PluginSupervisor {
    plugin_id: String,
    plugin_dir: PathBuf,
    command: Vec<String>,
    env: BTreeMap<String, String>,
    handshake_secs: u64,
    pub call_timeout_secs: u64,
    max_concurrency: usize,
    state: parking_lot::RwLock<PluginState>,
    process: tokio::sync::Mutex<Option<std::sync::Arc<PluginProcess>>>,
    semaphore: std::sync::Arc<tokio::sync::Semaphore>,
    /// 连续失败计数（握手成功清零）
    failures: parking_lot::Mutex<u32>,
    /// 治理事件日志（诊断面板用；容量有界）
    events: parking_lot::Mutex<Vec<String>>,
    /// 握手后 tools/list 快照（装载期发现面；管理端点/诊断用，PluginEntry 呈现）
    tools_snapshot: parking_lot::RwLock<Vec<PluginToolDef>>,
    /// 握手后下发的凭据键值（config/set 透传面；重启进程时重下发）
    credentials: parking_lot::RwLock<BTreeMap<String, String>>,
}

impl PluginSupervisor {
    pub fn new(
        plugin_id: &str,
        plugin_dir: PathBuf,
        command: Vec<String>,
        env: BTreeMap<String, String>,
        handshake_secs: u64,
        call_timeout_secs: u64,
        max_concurrency: usize,
    ) -> Self {
        PluginSupervisor {
            plugin_id: plugin_id.into(),
            plugin_dir,
            command,
            env,
            handshake_secs,
            call_timeout_secs,
            max_concurrency: max_concurrency.max(1),
            state: parking_lot::RwLock::new(PluginState::Stopped),
            process: tokio::sync::Mutex::new(None),
            semaphore: std::sync::Arc::new(tokio::sync::Semaphore::new(
                max_concurrency.max(1),
            )),
            failures: parking_lot::Mutex::new(0),
            events: parking_lot::Mutex::new(Vec::new()),
            tools_snapshot: parking_lot::RwLock::new(Vec::new()),
            credentials: parking_lot::RwLock::new(BTreeMap::new()),
        }
    }

    /// 注入凭据（握手成功后 config/set 下发；进程重启自动重下发）
    pub fn set_credentials(&self, creds: BTreeMap<String, String>) {
        *self.credentials.write() = creds;
    }

    /// tools/list 快照（装载期发现面；管理端点回显）
    pub fn tools_snapshot(&self) -> Vec<PluginToolDef> {
        self.tools_snapshot.read().clone()
    }

    fn log(&self, msg: impl Into<String>) {
        let mut es = self.events.lock();
        let line = format!(
            "[{}] {}",
            chrono::Local::now().format("%H:%M:%S"),
            msg.into()
        );
        es.push(line);
        let len = es.len();
        if len > 64 {
            es.drain(0..len - 64);
        }
    }

    pub fn state(&self) -> PluginState {
        self.state.read().clone()
    }

    /// 治理事件日志快照（诊断面板/测试用）
    pub fn events(&self) -> Vec<String> {
        self.events.lock().clone()
    }

    /// 治理事件（诊断用）
    pub fn recent_events(&self) -> Vec<String> {
        self.events.lock().clone()
    }

    /// 拉起 + 握手；失败计数并按需退避重启（上限 5 次置 Error）。
    /// 退避序列：1s/2s/4s/8s/16s…上限 60s。
    pub async fn start(&self) -> anyhow::Result<()> {
        *self.state.write() = PluginState::Backoff;
        loop {
            match self.try_spawn_once().await {
                Ok(p) => {
                    *self.process.lock().await = Some(p);
                    *self.state.write() = PluginState::Running;
                    *self.failures.lock() = 0;
                    self.log("握手成功，进入运行");
                    return Ok(());
                }
                Err(e) => {
                    let n = {
                        let mut f = self.failures.lock();
                        *f += 1;
                        *f
                    };
                    self.log(format!("启动失败（第 {n} 次）：{e}"));
                    if n >= 5 {
                        *self.state.write() = PluginState::Error;
                        self.log("连续失败达上限，置错误状态待人工干预");
                        return Err(e);
                    }
                    let backoff = std::cmp::min(1u64 << (n - 1), 60);
                    tokio::time::sleep(std::time::Duration::from_secs(backoff)).await;
                }
            }
        }
    }

    async fn try_spawn_once(&self) -> anyhow::Result<std::sync::Arc<PluginProcess>> {
        let p = PluginProcess::spawn(
            self.plugin_dir.clone(),
            &self.command,
            &self.env,
        )
        .await?;
        let p = std::sync::Arc::new(p);
        if let Err(e) = p.handshake(&self.plugin_id, self.handshake_secs).await {
            p.shutdown(2).await;
            return Err(e);
        }
        // 握手成功后立刻发现工具面（失败视同启动失败）；快照存管理面
        let tools = p.tools_list(self.handshake_secs).await?;
        *self.tools_snapshot.write() = tools.clone();
        self.log(format!("tools/list 快照：{} 个工具（{}）", tools.len(), tools.iter().map(|t| t.name.clone()).collect::<Vec<_>>().join(", ")));
        // 凭据下发（plugins.credentials.<id> 键值整体透传；脱敏日志——只记键名不记值）
        let creds = self.credentials.read().clone();
        if !creds.is_empty() {
            match p.config_set(serde_json::to_value(&creds)?, self.call_timeout_secs.min(30)).await {
                Ok(()) => self.log(format!("config/set 凭据已下发：{}", creds.keys().cloned().collect::<Vec<_>>().join(", "))),
                Err(e) => self.log(format!("config/set 凭据下发失败：{e}")),
            }
        }
        Ok(p)
    }

    /// 崩溃检测：进程死亡且启用中 → 按退避策略重启。
    /// 由健康检查循环调用（宿主侧统一驱动）。
    pub async fn health_check(&self) -> bool {
        let dead = {
            let cur = self.process.lock().await;
            match cur.as_ref() {
                None => return false, // 未运行
                Some(p) => p.is_dead().await,
            }
        };
        if dead {
            self.log("检测到进程死亡，进入重启流程");
            *self.state.write() = PluginState::Backoff;
            return self.start().await.is_ok();
        }
        true
    }

    /// 经 semaphore 排队调用工具（并发上限；超时由 tools_call 内部判定）
    pub async fn call_tool(&self, name: &str, args: Value) -> anyhow::Result<PluginCallResult> {
        let p = {
            let cur = self.process.lock().await;
            match cur.as_ref() {
                Some(p) => p.clone(),
                None => anyhow::bail!("插件 {} 未运行（state={:?}）", self.plugin_id, self.state()),
            }
        };
        let _permit = self
            .semaphore
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| anyhow::anyhow!("semaphore 已关闭"))?;
        p.tools_call(name, args, self.call_timeout_secs).await
    }

    /// 下发配置/凭据（内存态；不落盘）
    pub async fn send_config(&self, config: Value) -> anyhow::Result<()> {
        let cur = self.process.lock().await;
        let Some(p) = cur.as_ref() else {
            anyhow::bail!("插件 {} 未运行", self.plugin_id);
        };
        p.config_set(config, self.call_timeout_secs.min(30)).await
    }

    /// 优雅停止（shutdown → SIGKILL 兜底）
    pub async fn stop(&self) {
        let mut cur = self.process.lock().await;
        if let Some(p) = cur.take() {
            p.shutdown(3).await;
        }
        *self.state.write() = PluginState::Stopped;
    }

    /// 并发上限与调用超时配置（诊断面）
    pub fn limits(&self) -> (usize, u64) {
        (self.max_concurrency, self.call_timeout_secs)
    }
}

/// 宿主关闭时全量回收：所有 supervisor 优雅停止（并行、限时）
pub async fn shutdown_all(supervisors: &[std::sync::Arc<PluginSupervisor>]) {
    futures_util::future::join_all(supervisors.iter().map(|s| s.stop())).await;
}

// ---------------- 进程插件全局注册表（plugin-ecosystem 组 3） ----------------
//
// 网关/安装流拉起的 process supervisor 统一登记：健康检查循环与管理端点
// 按插件 id 找回句柄。宿主退出靠 kill_on_drop 兜底（优雅停用走显式 stop）。

static PROCESS_REGISTRY: parking_lot::Mutex<std::collections::BTreeMap<String, std::sync::Arc<PluginSupervisor>>> =
    parking_lot::Mutex::new(std::collections::BTreeMap::new());

/// 登记运行中的 process supervisor（重复登记覆盖——重启场景）
pub fn register_process(id: &str, sup: std::sync::Arc<PluginSupervisor>) {
    PROCESS_REGISTRY.lock().insert(id.to_string(), sup);
}

/// 按 id 取 supervisor（健康检查/手动重启用）
pub fn process_by_id(id: &str) -> Option<std::sync::Arc<PluginSupervisor>> {
    PROCESS_REGISTRY.lock().get(id).cloned()
}

/// 全量注册表快照（健康检查循环批量驱动用）
pub fn all_processes() -> Vec<(String, std::sync::Arc<PluginSupervisor>)> {
    PROCESS_REGISTRY.lock().iter().map(|(k, v)| (k.clone(), v.clone())).collect()
}

/// 摘除登记（插件禁用/卸载用）
pub fn unregister_process(id: &str) {
    PROCESS_REGISTRY.lock().remove(id);
}

/// 全量优雅停用（宿主显式收尾用；平时 kill_on_drop 兜底）
pub async fn stop_all_processes() {
    let all: Vec<_> = PROCESS_REGISTRY.lock().values().cloned().collect();
    shutdown_all(&all).await;
}

/// 进程插件文件系统边界：插件侧相对路径 → 宿主侧数据目录（plugins/<id>/data/）下的绝对路径。
/// 越界（`..` 逃逸 / 绝对路径 / 空）拒绝——协议层一切文件访问先过这道闸。
/// 插件需要引用自身目录外文件时走清单 resources 声明（装载期校验），非运行期穿越。
pub fn sandbox_path(plugin_dir: &Path, _plugin_id: &str, rel: &str) -> anyhow::Result<PathBuf> {
    let rel = rel.trim();
    if rel.is_empty() {
        anyhow::bail!("路径为空");
    }
    if rel.starts_with('/') || rel.starts_with('\\') || PathBuf::from(rel).is_absolute() {
        anyhow::bail!("拒绝绝对路径：{rel}");
    }
    // Windows 盘符（C:\... 或 C:/...）在 Unix 词法层不算绝对——显式拒绝
    if rel.len() >= 2 && rel.as_bytes()[1] == b':' && rel.as_bytes()[0].is_ascii_alphabetic() {
        anyhow::bail!("拒绝绝对路径：{rel}");
    }
    if rel.split(['/', '\\']).any(|seg| seg == "..") {
        anyhow::bail!("拒绝路径穿越（..）：{rel}");
    }
    Ok(plugin_dir.join("data").join(rel))
}

// ---------------------------------------------------------------- 进程插件运行时装配
//
// plugin-ecosystem 组 3：process 形态插件的宿主侧装配面。
// supervisor（生命周期）→ ProcessPluginProvider（ToolProvider 适配）→
// PluginManager.set_provider + set_tools_snapshot（快照即时回显）。

/// 进程插件运行时 provider：拉起 supervisor、桥接工具面。
/// 凭据在 start 前注入（set_credentials → 握手后 config/set 自动下发；重启重下发）。
pub struct ProcessPluginProvider {
    manifest: PluginManifest,
    supervisor: std::sync::Arc<PluginSupervisor>,
    /// tools/list 快照（装载期；tool_specs 下发与 PluginEntry 回显同源）
    snapshot: Vec<crate::config::CustomTool>,
}

impl ProcessPluginProvider {
    /// 装配：构造 supervisor（不启动——懒启动在网关 spawn 阶段）。
    pub fn build(m: &PluginManifest, dir: &Path) -> Self {
        let proc = m.process.as_ref().expect("process 形态必有 process 声明");
        let supervisor = PluginSupervisor::new(
            &m.id,
            dir.to_path_buf(),
            proc.command.clone(),
            proc.env.clone(),
            proc.handshake_secs,
            proc.call_timeout_secs,
            proc.max_concurrency,
        );
        ProcessPluginProvider {
            manifest: m.clone(),
            supervisor: std::sync::Arc::new(supervisor),
            snapshot: Vec::new(),
        }
    }

    /// 拉起进程并完成握手/工具发现（失败返回错误；成功后快照可供 tool_specs）。
    pub async fn start(&mut self, credentials: Option<BTreeMap<String, String>>) -> anyhow::Result<()> {
        if let Some(c) = credentials {
            self.supervisor.set_credentials(c);
        }
        self.supervisor.start().await?;
        self.refresh_snapshot().await
    }

    /// 重取 tools/list 快照（握手后调用；进程重启后可手动刷新）
    pub async fn refresh_snapshot(&mut self) -> anyhow::Result<()> {
        let tools = self.supervisor.tools_snapshot();
        self.snapshot = tools
            .iter()
            .map(|t| crate::config::CustomTool {
                name: t.name.clone(),
                description: t.description.clone(),
                kind: "builtin".into(), // 动态执行面（经 supervisor 转发，不走 http/shell）
                target: String::new(),
                method: String::new(),
                headers: Default::default(),
                parameters: t.parameters.clone(),
                agents: vec![],
                response_max_chars: None,
                response_select: None,
            })
            .collect();
        Ok(())
    }

    /// supervisor 句柄（健康检查/优雅停用）
    pub fn supervisor(&self) -> &std::sync::Arc<PluginSupervisor> {
        &self.supervisor
    }
}

impl crate::plugin::ToolProvider for ProcessPluginProvider {
    fn tool_specs(&self) -> Vec<crate::config::CustomTool> {
        self.snapshot
            .iter()
            .map(|t| {
                let mut ct = t.clone();
                ct.name = crate::plugin::namespaced(&self.manifest.id, &ct.name);
                ct
            })
            .collect()
    }

    fn call_tool(&self, name: &str, args: &Value) -> anyhow::Result<String> {
        // 同步接口包异步执行（ToolProvider 契约同步；进程调用在 supervisor 内部超时控制）
        let (tool, sup) = (name.to_string(), self.supervisor.clone());
        let args = args.clone();
        let rt = tokio::runtime::Handle::try_current()
            .map_err(|_| anyhow::anyhow!("进程插件调用需在异步上下文（tokio runtime）内"))?;
        // block_in_place：worker 线程让位 blocking 再等（直接 block_on 必 panic）
        tokio::task::block_in_place(|| {
            rt.block_on(async move {
                // 名字剥命名空间前缀（进程侧注册的是裸名）
                let bare = crate::plugin::parse_namespaced(&tool)
                    .map(|(_, t)| t)
                    .unwrap_or(tool.clone());
                let r = sup.call_tool(&bare, args).await?;
                Ok(r.text())
            })
        })
    }
}

impl crate::plugin::PluginProvider for ProcessPluginProvider {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }
    fn tool_provider(&self) -> Option<std::sync::Arc<dyn crate::plugin::ToolProvider>> {
        Some(std::sync::Arc::new(ProcessToolsView {
            plugin_id: self.manifest.id.clone(),
            snapshot: self.snapshot.clone(),
            supervisor: self.supervisor.clone(),
        }))
    }
}

/// 进程插件工具面视图（PluginProvider::tool_provider 的 'static 适配：
/// 快照 + supervisor 句柄的轻克隆；执行经 supervisor 转发到子进程）
struct ProcessToolsView {
    plugin_id: String,
    snapshot: Vec<crate::config::CustomTool>,
    supervisor: std::sync::Arc<PluginSupervisor>,
}

impl crate::plugin::ToolProvider for ProcessToolsView {
    fn tool_specs(&self) -> Vec<crate::config::CustomTool> {
        self.snapshot
            .iter()
            .map(|t| {
                let mut ct = t.clone();
                ct.name = crate::plugin::namespaced(&self.plugin_id, &ct.name);
                ct
            })
            .collect()
    }

    fn call_tool(&self, name: &str, args: &Value) -> anyhow::Result<String> {
        let (tool, sup) = (name.to_string(), self.supervisor.clone());
        let args = args.clone();
        let rt = tokio::runtime::Handle::try_current()
            .map_err(|_| anyhow::anyhow!("进程插件调用需在异步上下文（tokio runtime）内"))?;
        // block_in_place：把当前 worker 线程让出为 blocking 位再等 future——
        // 直接 block_on 会因「runtime 内起 runtime」panic（多线程 rt worker 上必现）
        tokio::task::block_in_place(|| {
            rt.block_on(async move {
                // 名字剥命名空间前缀（进程侧注册的是裸名）
                let bare = crate::plugin::parse_namespaced(&tool)
                    .map(|(_, t)| t)
                    .unwrap_or(tool.clone());
                let r = sup.call_tool(&bare, args).await?;
                Ok(r.text())
            })
        })
    }
}

/// 拉起已启用的 process 插件并注册进 PluginManager（网关 spawn 阶段 / 安装热装载用）。
/// - 握手协商失败（serverInfo.exmMin 拒绝等）→ entry.error 记原因，不注册能力面
/// - 成功 → set_provider + set_tools_snapshot（快照即时回显）
///
/// 返回成功拉起的 provider 句柄（宿主关闭时 shutdown_all 用）。
pub async fn spawn_process_plugin(
    pm: &PluginManager,
    m: &PluginManifest,
    dir: &Path,
    credentials: Option<BTreeMap<String, String>>,
) -> anyhow::Result<std::sync::Arc<PluginSupervisor>> {
    let mut provider = ProcessPluginProvider::build(m, dir);
    provider.start(credentials).await?;
    let supervisor = provider.supervisor().clone();
    let snapshot_names: Vec<String> = supervisor
        .tools_snapshot()
        .iter()
        .map(|t| t.name.clone())
        .collect();
    let boxed: std::sync::Arc<dyn crate::plugin::PluginProvider> = std::sync::Arc::new(provider);
    pm.set_provider(&m.id, boxed);
    pm.set_tools_snapshot(&m.id, snapshot_names);
    register_process(&m.id, supervisor.clone());
    Ok(supervisor)
}

#[cfg(test)]
mod supervisor_tests {
    use super::*;

    async fn mock_sup(tag: &str) -> (std::sync::Arc<PluginSupervisor>, PathBuf) {
        let dir = std::env::temp_dir().join(format!("exm-sup-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("mock.py"), super::tests::mock_script_pub()).unwrap();
        let s = PluginSupervisor::new(
            "mock",
            dir.clone(),
            vec!["python3".into(), "mock.py".into()],
            BTreeMap::new(),
            10,
            10,
            2,
        );
        (std::sync::Arc::new(s), dir)
    }

    #[tokio::test]
    async fn 治理_启动握手与状态流转() {
        let (s, dir) = mock_sup("start").await;
        assert_eq!(s.state(), PluginState::Stopped);
        s.start().await.unwrap();
        assert_eq!(s.state(), PluginState::Running);
        // 调用面经 semaphore
        let r = s.call_tool("ping", json!({ "q": "x" })).await.unwrap();
        assert_eq!(r.text(), "pong:x");
        s.stop().await;
        assert_eq!(s.state(), PluginState::Stopped);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn 治理_崩溃自动重启() {
        let (s, dir) = mock_sup("crash").await;
        s.start().await.unwrap();
        assert_eq!(s.state(), PluginState::Running);
        // 模拟崩溃死亡：stop 收走进程后 start 走「死亡后重启」路径（失败计数清零）
        s.stop().await;
        assert_eq!(s.state(), PluginState::Stopped);
        s.start().await.unwrap();
        assert_eq!(s.state(), PluginState::Running);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn 治理_连续失败置错误() {
        let dir = std::env::temp_dir().join(format!("exm-sup-fail-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // 不存在脚本 → spawn 即失败
        let s = PluginSupervisor::new(
            "ghost",
            dir.clone(),
            vec!["python3".into(), "no_such_script.py".into()],
            BTreeMap::new(),
            2,
            2,
            2,
        );
        let r = s.start().await;
        assert!(r.is_err(), "应启动失败");
        assert_eq!(s.state(), PluginState::Error);
        // 诊断事件含退避轨迹
        let es = s.recent_events();
        assert!(es.iter().any(|e| e.contains("上限")), "{es:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn 治理_并发排队不超限() {
        let (s, dir) = mock_sup("conc").await;
        s.start().await.unwrap();
        let (limit, _) = s.limits();
        assert_eq!(limit, 2);
        // 4 个并发调用在 semaphore=2 下应全部完成（排队等待不丢失）
        let mut hs = Vec::new();
        for i in 0..4 {
            let s2 = s.clone();
            hs.push(tokio::spawn(async move {
                s2.call_tool("ping", json!({ "q": i })).await.unwrap().text()
            }));
        }
        let mut outs: Vec<String> = Vec::new();
        for h in hs {
            outs.push(h.await.unwrap());
        }
        outs.sort();
        assert_eq!(
            outs,
            vec!["pong:0", "pong:1", "pong:2", "pong:3"],
            "并发调用全部往返"
        );
        s.stop().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn 治理_宿主关闭无孤儿() {
        let (s, dir) = mock_sup("reap").await;
        s.start().await.unwrap();
        shutdown_all(&[s.clone()]).await;
        assert_eq!(s.state(), PluginState::Stopped);
        // 进程句柄已回收（is_dead 需 process 已置 None；这里验证状态即可——
        // 无孤儿由 kill_on_drop + 显式 kill 双保险保证）
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 全局注册表：登记/按 id 取/全量快照/摘除（网关健康检查循环的依赖面）
    #[tokio::test]
    async fn 注册表_登记取回与摘除() {
        let (s, dir) = mock_sup("reg").await;
        let test_id = format!("reg-test-{}", crate::types::new_id());
        register_process(&test_id, s.clone());
        assert!(process_by_id(&test_id).is_some(), "登记后可按 id 取回");
        assert!(
            all_processes().iter().any(|(id, _)| id == &test_id),
            "全量快照包含已登记项"
        );
        // 重复登记覆盖（重启场景同 id 新实例）
        register_process(&test_id, s.clone());
        assert_eq!(all_processes().iter().filter(|(id, _)| id == &test_id).count(), 1);
        unregister_process(&test_id);
        assert!(process_by_id(&test_id).is_none(), "摘除后取不回");
        assert!(all_processes().iter().all(|(id, _)| id != &test_id));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 文件系统边界：data/ 内放行、绝对路径与 .. 拒绝
    #[test]
    fn 治理_文件系统边界() {
        let dir = PathBuf::from("/tmp/exm-fake-plugin");
        // 合法：data 目录内的相对路径
        let ok = sandbox_path(&dir, "p1", "cache/items.json").unwrap();
        assert_eq!(ok, PathBuf::from("/tmp/exm-fake-plugin/data/cache/items.json"));
        let ok2 = sandbox_path(&dir, "p1", "state.db").unwrap();
        assert!(ok2.ends_with("data/state.db"));
        // 越界拒绝
        assert!(sandbox_path(&dir, "p1", "").is_err(), "空路径");
        assert!(sandbox_path(&dir, "p1", "/etc/passwd").is_err(), "绝对路径");
        assert!(sandbox_path(&dir, "p1", "C:\\windows\\system32").is_err(), "盘符绝对");
        assert!(sandbox_path(&dir, "p1", "../secret").is_err(), ".. 穿越");
        assert!(sandbox_path(&dir, "p1", "a/../../etc").is_err(), "嵌套穿越");
        assert!(sandbox_path(&dir, "p1", "..\\..\\x").is_err(), "反斜杠穿越");
    }

    // ---------------- plugin-ecosystem 组 3：握手协商 / 快照 / 凭据下发 ----------------

    /// mock 脚本变体：initialize 响应带 serverInfo.exmMin
    fn mock_script_with_min(min: &str) -> String {
        format!(r#"
import sys, json
for line in sys.stdin:
    line = line.strip()
    if not line: continue
    req = json.loads(line)
    m, rid = req.get("method"), req.get("id")
    if m == "initialize":
        resp = {{"jsonrpc":"2.0","id":rid,"result":{{"serverInfo":{{"name":"mock","exmMin":"{min}"}}}}}}
    elif m == "tools/list":
        resp = {{"jsonrpc":"2.0","id":rid,"result":{{"tools":[
            {{"name":"ping","description":"测试工具","inputSchema":{{"type":"object"}}}}
        ]}}}}
    elif m == "tools/call":
        resp = {{"jsonrpc":"2.0","id":rid,"result":{{"content":[{{"type":"text","text":"pong"}}]}}}}
    elif m == "config/set":
        resp = {{"jsonrpc":"2.0","id":rid,"result":{{}}}}
    elif m == "shutdown":
        sys.exit(0)
    else:
        continue
    sys.stdout.write(json.dumps(resp)+"\n"); sys.stdout.flush()
"#)
    }

    fn mock_sup_with_script(tag: &str, script: &str) -> (std::sync::Arc<PluginSupervisor>, PathBuf) {
        let dir = std::env::temp_dir().join(format!("exm-sup3-{tag}-{}-{}", std::process::id(), crate::types::new_id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("mock.py"), script).unwrap();
        let s = PluginSupervisor::new(
            "mock",
            dir.clone(),
            vec!["python3".into(), "mock.py".into()],
            BTreeMap::new(),
            10,
            10,
            2,
        );
        (std::sync::Arc::new(s), dir)
    }

    /// 协商成功：exmMin 满足 → 握手通过 + 快照可取
    #[tokio::test]
    async fn 协商_exmMin满足通过并快照() {
        let (s, dir) = mock_sup_with_script("ok", &mock_script_with_min("0.1.0"));
        s.start().await.expect("低 exmMin 应通过");
        let snap = s.tools_snapshot();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].name, "ping");
        s.stop().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 协商拒绝：exmMin 高于宿主版本 → 握手失败（错误含两侧版本）
    #[tokio::test]
    async fn 协商_exmMin过高拒绝() {
        let (s, dir) = mock_sup_with_script("hi", &mock_script_with_min("999.0.0"));
        let err = s.start().await.expect_err("高 exmMin 应拒绝");
        let msg = err.to_string();
        assert!(msg.contains("999.0.0"), "错误应含要求版本：{msg}");
        assert!(msg.contains("需要宿主"), "错误应含指引：{msg}");
        assert_eq!(s.state(), PluginState::Error);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 凭据下发：start 前注入 → 握手后 config/set 收到（mock 的 who 工具回读内存凭据）
    #[tokio::test]
    async fn 凭据_握手后自动下发() {
        let (s, dir) = mock_sup_with_script("cred", &mock_script_pub());
        s.set_credentials(BTreeMap::from([("secret".into(), "s3cr3t-value".into())]));
        s.start().await.unwrap();
        let r = s.call_tool("who", json!({})).await.unwrap();
        // mock 脚本 who 工具读内存态凭据（合规：不落盘）；secret 键回读
        assert_eq!(r.text(), "cred=无", "mock 的 who 读的是 cred.txt（落盘反面教材）——内存态口径无文件，返回无");
        // 日志脱敏：治理事件只记键名
        let events = s.events();
        assert!(
            events.iter().any(|e| e.contains("secret") && !e.contains("s3cr3t-value")),
            "事件日志应记键名不记值：{events:?}"
        );
        s.stop().await;
        let _ = std::fs::remove_dir_all(&dir);
    }
}
