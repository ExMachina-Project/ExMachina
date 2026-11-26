//! 连接契约的测试基座：不实现任务协议、操作账本或第二套执行器。
//!
//! 只向待测端提供 project 和 loopback 地址；oracle、服务端状态与故障令牌留在宿主。
//! 目录分离不是 OS 沙箱；运行不可信本地进程仍须使用既有执行后端。
//! 正常路径必须检查 close()/finish()，Drop 只负责异常路径回收。

use axum::{
    body::{Body, Bytes},
    extract::DefaultBodyLimit,
    http::{header::CONTENT_LENGTH, HeaderValue},
    response::Response,
    Router,
};
use std::{
    fs, io,
    net::{Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    net::TcpListener,
    sync::{oneshot, watch},
    task::JoinHandle,
    time::timeout,
};

pub const FIXTURE_TIMEOUT: Duration = Duration::from_secs(5);

/// 三个互不包含的目录；路径不从远程输入构造，也不修改 cwd/HOME 等进程环境。
#[derive(Debug)]
pub struct IsolatedDirs {
    root: PathBuf,
    project: PathBuf,
    service: PathBuf,
    oracle: PathBuf,
    closed: bool,
}

impl IsolatedDirs {
    pub fn new() -> io::Result<Self> {
        #[cfg(unix)]
        match fs::metadata("/tmp/opencode") {
            Ok(_) => return Self::new_in("/tmp/opencode"),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        Self::new_in(std::env::temp_dir())
    }

    /// 显式宿主目录覆盖，供其他测试复用及验证初始化失败；不会创建缺失的父目录。
    pub fn new_in(parent: impl AsRef<Path>) -> io::Result<Self> {
        let parent = parent.as_ref().canonicalize()?;
        let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()?;
        if parent.starts_with(repo) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "连接夹具不得在仓库内创建临时产物",
            ));
        }
        let root = parent.join(format!("exm-connection-fabric-{}", uuid::Uuid::new_v4()));
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&root)?;
        let dirs = Self {
            project: root.join("project"),
            service: root.join("service"),
            oracle: root.join("oracle"),
            root,
            closed: false,
        };
        for path in [&dirs.project, &dirs.service, &dirs.oracle] {
            fs::create_dir(path)?;
        }
        Ok(dirs)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn project(&self) -> &Path {
        &self.project
    }

    pub fn service(&self) -> &Path {
        &self.service
    }

    pub fn oracle(&self) -> &Path {
        &self.oracle
    }

    pub fn close(mut self) -> io::Result<()> {
        self.closed = true;
        fs::remove_dir_all(&self.root)
    }
}

impl Drop for IsolatedDirs {
    fn drop(&mut self) {
        if !self.closed {
            // 仅删除本实例以 create_dir 独占创建的随机子目录，不清理整个临时树。
            if let Err(error) = fs::remove_dir_all(&self.root) {
                if !std::thread::panicking() {
                    panic!("连接夹具回收失败：{error}");
                }
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ActionCounts {
    pub probes: usize,
    pub calls: usize,
    pub effects: usize,
}

/// 由受信服务端 handler 记录，不反序列化远程计数，也不提供网络读取/重置接口。
/// 每个软件、AI 或能力可以持有独立实例；快照为副本，不能反向改写 oracle。
#[derive(Clone, Default)]
pub struct ActionOracle {
    counts: Arc<Mutex<ActionCounts>>,
}

impl ActionOracle {
    pub fn record_probe(&self) {
        self.counts.lock().expect("oracle 锁损坏").probes += 1;
    }

    pub fn record_call(&self) {
        self.counts.lock().expect("oracle 锁损坏").calls += 1;
    }

    /// 只在实际效果已经发生后调用，不以请求正文的“成功”或 HTTP 受理作依据。
    pub fn record_effect(&self) {
        self.counts.lock().expect("oracle 锁损坏").effects += 1;
    }

    pub fn snapshot(&self) -> ActionCounts {
        *self.counts.lock().expect("oracle 锁损坏")
    }
}

/// 宿主独占的释放能力；不放入请求、响应或可反序列化的配置。
pub struct BarrierControl {
    entered: watch::Receiver<bool>,
    released: watch::Sender<bool>,
}

/// 服务端/持久化边界仅能进入和等待，不能自行释放屏障。
#[derive(Clone)]
pub struct FaultGate {
    entered: watch::Sender<bool>,
    released: watch::Receiver<bool>,
}

pub fn fault_barrier() -> (BarrierControl, FaultGate) {
    let (entered_tx, entered_rx) = watch::channel(false);
    let (released_tx, released_rx) = watch::channel(false);
    (
        BarrierControl {
            entered: entered_rx,
            released: released_tx,
        },
        FaultGate {
            entered: entered_tx,
            released: released_rx,
        },
    )
}

impl BarrierControl {
    pub async fn wait_until_entered(&self) -> io::Result<()> {
        wait_for_true(self.entered.clone()).await
    }

    pub fn release(&self) -> io::Result<()> {
        self.released.send(true).map_err(|_| barrier_closed())
    }
}

impl FaultGate {
    pub async fn arrive_and_wait(&self) -> io::Result<()> {
        self.entered.send(true).map_err(|_| barrier_closed())?;
        wait_for_true(self.released.clone()).await
    }
}

fn barrier_closed() -> io::Error {
    io::Error::new(io::ErrorKind::BrokenPipe, "故障屏障的另一端已销毁")
}

async fn wait_for_true(mut state: watch::Receiver<bool>) -> io::Result<()> {
    timeout(FIXTURE_TIMEOUT, async {
        loop {
            let reached = *state.borrow_and_update();
            if reached {
                return Ok(());
            }
            state.changed().await.map_err(|_| barrier_closed())?;
        }
    })
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "故障屏障等待超时"))?
}

/// 通用 loopback 容器：后续文档软件和 AI 替身自定义 Router，不复制生产 runner。
pub struct LoopbackServer {
    address: SocketAddr,
    stop: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<io::Result<()>>>,
}

impl LoopbackServer {
    pub async fn spawn(router: Router) -> io::Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
        let address = listener.local_addr()?;
        let (stop, stopped) = oneshot::channel();
        let router = router.layer(DefaultBodyLimit::max(64 * 1024));
        let task = tokio::spawn(async move {
            axum::serve(listener, router)
                .with_graceful_shutdown(async move {
                    let _ = stopped.await;
                })
                .await
        });
        Ok(Self {
            address,
            stop: Some(stop),
            task: Some(task),
        })
    }

    pub fn address(&self) -> SocketAddr {
        self.address
    }

    pub fn endpoint(&self) -> String {
        format!("http://{}", self.address)
    }

    /// 检查真实服务的退出错误/panic；不把服务启动失败当作“环境不支持”跳过。
    pub async fn finish(mut self) -> io::Result<()> {
        if let Some(stop) = self.stop.take() {
            // 服务可能已失败退出；仍须 join，不能因发送失败丢掉原始错误。
            let _ = stop.send(());
        }
        let joined = timeout(
            FIXTURE_TIMEOUT,
            self.task.as_mut().expect("loopback 服务已回收"),
        )
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "loopback 服务回收超时"))?;
        self.task.take();
        joined.map_err(io::Error::other)?
    }
}

impl Drop for LoopbackServer {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

/// 实际截断 HTTP 响应流；调用方必须先施加效果，再使用此响应模拟回执丢失。
pub fn disconnected_response() -> Response {
    let body = Body::from_stream(futures_util::stream::once(async {
        Err::<Bytes, _>(io::Error::new(
            io::ErrorKind::ConnectionReset,
            "夹具在效果发生后断开响应",
        ))
    }));
    let mut response = Response::new(body);
    response
        .headers_mut()
        .insert(CONTENT_LENGTH, HeaderValue::from_static("1"));
    response
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PersistencePoint {
    Write,
    Sync,
}

/// 在既有存储 I/O 闭包外加故障点，不实现另一套存储或操作协议。
/// 故障选择和屏障由宿主构造；远程输入只可作为 action 的数据，不可选择 failure。
pub struct ControlledIo {
    point: PersistencePoint,
    gate: Option<FaultGate>,
    failure: Option<io::ErrorKind>,
}

impl ControlledIo {
    pub fn new(
        point: PersistencePoint,
        gate: Option<FaultGate>,
        failure: Option<io::ErrorKind>,
    ) -> Self {
        Self {
            point,
            gate,
            failure,
        }
    }

    pub async fn run<T>(
        &self,
        point: PersistencePoint,
        action: impl FnOnce() -> io::Result<T>,
    ) -> io::Result<T> {
        if point == self.point {
            if let Some(gate) = &self.gate {
                gate.arrive_and_wait().await?;
            }
            if let Some(kind) = self.failure {
                return Err(io::Error::new(kind, "宿主注入的持久化故障"));
            }
        }
        action()
    }
}
