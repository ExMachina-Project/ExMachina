//! panel 域分文件：中断监听（Interrupt）。

use super::*;

pub(crate) struct Interrupt {
    pub(crate) busy: AtomicBool,
    pub(crate) closing: AtomicBool,
    pub(crate) session: Mutex<String>,
}

impl Interrupt {
    pub(crate) fn new(session: &str) -> Self {
        Interrupt {
            busy: AtomicBool::new(false),
            closing: AtomicBool::new(false),
            session: Mutex::new(session.to_string()),
        }
    }
    pub(crate) fn set_session(&self, id: &str) {
        *self.session.lock() = id.to_string();
    }
    fn current_session(&self) -> String {
        self.session.lock().clone()
    }
}

/// 常驻监听 Ctrl+C：运行中 → 中止当前轮次；空闲 → 打印告别并退出；连按 → 强制退出
pub(crate) fn spawn_interrupt_listener(core: Arc<Core>, itr: Arc<Interrupt>) {
    tokio::spawn(async move {
        loop {
            if tokio::signal::ctrl_c().await.is_err() {
                return;
            }
            if itr.closing.load(Ordering::SeqCst) {
                std::process::exit(130);
            }
            if itr.busy.load(Ordering::SeqCst) {
                let sid = itr.current_session();
                if !sid.is_empty() {
                    let _ = core.stop_session(&sid);
                }
                println!(
                    "\n{}",
                    warn("⏹ 已请求中止当前轮次（再按一次 Ctrl+C 退出面板）")
                );
            } else {
                itr.closing.store(true, Ordering::SeqCst);
                println!(
                    "\n{}",
                    dim("已退出面板；会话已保留（再次进入后 /resume 或 /sessions 可切回）。")
                );
                std::process::exit(0);
            }
        }
    });
}
