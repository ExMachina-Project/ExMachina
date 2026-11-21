//! 轮次过程轨迹暂存 —— 思维链与工具调用的会话级收集器
//!
//! 服务端在轮次进行中收集两类过程数据：
//! - 思维链增量（orchestrator 的 Thinking 流）
//! - 工具调用记录（每次 execute_named 完成后的摘要）
//!
//! 消息收束（最终 orchestrator 消息落盘前）由 `drain` 一次性取走并随消息持久化——
//! 这是「过程收起栏」在刷新/换端后仍然可回看的数据来源（process-transparency 能力）。
//! 另含会话级取消令牌：工具循环可检查，受管进程可等待取消通知。

use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::watch;

/// 单次工具调用记录（与前端 ToolCallItem 契约一致，camelCase 序列化）
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallRecord {
    pub call_id: String,
    pub agent_id: String,
    pub tool: String,
    pub args: serde_json::Value,
    pub ok: bool,
    #[serde(default)]
    pub duration_ms: u64,
    #[serde(default)]
    pub summary: String,
}

/// 有序过程步骤：思考段落与工具调用按发生顺序交错（「按调用顺序显示每一次」数据源）。
/// 连续思考增量在采集时合并进同一段落，避免逐字成条。
/// 序列化陷阱：enum 的 rename_all 只作用于变体名（tag 值），**变体字段**必须各自带
/// rename_all（serde 语义）——此前写在容器级导致 call_id 落库为蛇形键，前端 camelCase
/// 读取失配、刷新后工具卡全部消失。alias 保留旧蛇形键的读取兼容（存量消息零迁移）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum TraceStep {
    /// 思考段落（连续增量已合并）
    Thinking { text: String },
    /// 一次工具调用（callId 关联 ToolCallRecord）。
    /// 字段级 rename/alias：rename_all 不适用于 enum 变体字段，且会把 alias 目标
    /// 一并变形——必须逐字段写死 camelCase 键名 + 蛇形旧键别名（存量消息零迁移）
    Tool {
        #[serde(rename = "callId", alias = "call_id")]
        call_id: String,
    },
}

#[derive(Default)]
struct RoundState {
    thinking: String,
    tools: Vec<ToolCallRecord>,
    steps: Vec<TraceStep>,
    cancelled: watch::Sender<bool>,
    steering: Vec<SteeringInput>,
    steering_open: bool,
}

/// 一条已被运行中会话接收、等待安全检查点消费的 steering。
#[derive(Debug, Clone)]
pub(crate) struct SteeringInput {
    pub(crate) id: String,
    pub(crate) sequence: u64,
    pub(crate) text: String,
    pub(crate) received_at: String,
}

#[derive(Debug)]
pub(crate) enum SteeringAdmissionError {
    NotRunning,
    Closed,
    QueueFull,
    Persistence(anyhow::Error),
}

fn next_steering_sequence() -> u64 {
    static SEQUENCE: AtomicU64 = AtomicU64::new(1);
    SEQUENCE.fetch_add(1, Ordering::Relaxed)
}

/// 原子 admission：与轮次关闭/结束共用同一状态锁；运行中才持久化并暂存。
/// 不持有会话运行锁，也不触碰取消令牌。
pub(crate) fn admit_steering<F>(
    session_id: &str,
    text: &str,
    persist: F,
) -> Result<SteeringInput, SteeringAdmissionError>
where
    F: FnOnce() -> anyhow::Result<()>,
{
    let text = text.trim();
    let mut map = states().lock();
    let Some(state) = map.get_mut(session_id) else {
        return Err(SteeringAdmissionError::NotRunning);
    };
    if !state.steering_open {
        return Err(SteeringAdmissionError::Closed);
    }
    if state.steering.len() >= 32 {
        return Err(SteeringAdmissionError::QueueFull);
    }
    let input = SteeringInput {
        id: format!("st-{}", &crate::types::new_id()[..8]),
        sequence: next_steering_sequence(),
        text: text.to_string(),
        received_at: crate::types::now_iso(),
    };
    if let Err(error) = persist() {
        return Err(SteeringAdmissionError::Persistence(error));
    }
    state.steering.push(input.clone());
    Ok(input)
}

/// 安全检查点取走当前会话已接收的 steering，保持 admission 顺序。
pub(crate) fn take_steering(session_id: &str) -> Vec<SteeringInput> {
    states()
        .lock()
        .get_mut(session_id)
        .map(|state| std::mem::take(&mut state.steering))
        .unwrap_or_default()
}

/// 最终回复检查点：若还有 steering 则交给模型处理；队列为空时原子关闭 admission。
/// 关闭后到达的输入会由入口作为下一轮普通输入排队，绝不冒充当前轮已接收。
pub(crate) fn take_or_close_steering(session_id: &str) -> Vec<SteeringInput> {
    let mut map = states().lock();
    let Some(state) = map.get_mut(session_id) else {
        return Vec::new();
    };
    if !state.steering.is_empty() {
        return std::mem::take(&mut state.steering);
    }
    state.steering_open = false;
    Vec::new()
}

/// 删除会话时清理尚未消费的 steering。
pub(crate) fn clear_steering(session_id: &str) {
    if let Some(state) = states().lock().get_mut(session_id) {
        state.steering.clear();
        state.steering_open = false;
    }
}

fn states() -> &'static Mutex<HashMap<String, RoundState>> {
    static S: std::sync::OnceLock<Mutex<HashMap<String, RoundState>>> = std::sync::OnceLock::new();
    S.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 追加思维链增量（同时并入有序步骤：与上一步同为思考则续写该段落）
pub fn push_thinking(session_id: &str, delta: &str) {
    if delta.is_empty() {
        return;
    }
    let mut map = states().lock();
    let st = map.entry(session_id.to_string()).or_default();
    st.thinking.push_str(delta);
    match st.steps.last_mut() {
        Some(TraceStep::Thinking { text }) => text.push_str(delta),
        _ => st.steps.push(TraceStep::Thinking {
            text: delta.to_string(),
        }),
    }
}

/// 记录一次完成的工具调用（同步并入有序步骤）
pub fn push_tool(session_id: &str, record: ToolCallRecord) {
    let mut map = states().lock();
    let st = map.entry(session_id.to_string()).or_default();
    st.steps.push(TraceStep::Tool {
        call_id: record.call_id.clone(),
    });
    st.tools.push(record);
}

/// 轮次收束时取走全部过程数据（取走即清空该类数据，取消令牌不受影响）
pub fn drain(session_id: &str) -> (Option<String>, Vec<ToolCallRecord>, Vec<TraceStep>) {
    let mut map = states().lock();
    let Some(st) = map.get_mut(session_id) else {
        return (None, Vec::new(), Vec::new());
    };
    let thinking = (!st.thinking.trim().is_empty()).then(|| std::mem::take(&mut st.thinking));
    let tools = std::mem::take(&mut st.tools);
    let steps = std::mem::take(&mut st.steps);
    (thinking, tools, steps)
}

/// 只读快照：工具轨迹摘要（元认知反思钩子输入——不取走数据，不打扰轮次收束）。
/// 输出「工具名(ok/failed)」序列，失败一目了然；截断到最近 12 条防提示膨胀。
pub fn snapshot_tool_trace(session_id: &str) -> String {
    let map = states().lock();
    let Some(st) = map.get(session_id) else {
        return String::new();
    };
    let n = st.tools.len();
    let start = n.saturating_sub(12);
    st.tools[start..]
        .iter()
        .map(|t| format!("{}({})", t.tool, if t.ok { "ok" } else { "failed" }))
        .collect::<Vec<_>>()
        .join(" → ")
}

/// Bounded, non-consuming evidence for a dispatched task's fallback report.
/// Arguments are kept out of reports; callers use only the recorded result summary.
pub fn snapshot_tool_records(session_id: &str, limit: usize) -> Vec<ToolCallRecord> {
    let map = states().lock();
    let Some(st) = map.get(session_id) else {
        return Vec::new();
    };
    st.tools[st.tools.len().saturating_sub(limit)..].to_vec()
}

/// 会话运行标志：chat 开始时置存在、结束移除（存在 = 有运行中轮次）
pub fn begin(session_id: &str) {
    let mut map = states().lock();
    let state = map.entry(session_id.to_string()).or_default();
    state.steering.clear();
    state.steering_open = true;
}

/// Own the round lifetime so errors, panics and cancelled futures release its state.
/// The caller must hold the session run lock for the lifetime of this guard.
pub(crate) struct RoundGuard(String);

impl RoundGuard {
    pub(crate) fn begin(session_id: &str) -> Self {
        begin(session_id);
        Self(session_id.to_owned())
    }
}

impl Drop for RoundGuard {
    fn drop(&mut self) {
        end(&self.0);
    }
}

/// 会话运行结束：移除整个轮次状态（含取消令牌）
pub fn end(session_id: &str) {
    states().lock().remove(session_id);
}

/// 会话是否有运行中轮次
pub fn is_running(session_id: &str) -> bool {
    states().lock().contains_key(session_id)
}

/// Number of live session turns, used to refuse a gateway restart that would interrupt work.
pub fn running_count() -> usize {
    states().lock().len()
}

/// 请求停止：置位取消令牌；返回会话是否有运行中轮次
pub fn request_cancel(session_id: &str) -> bool {
    let map = states().lock();
    match map.get(session_id) {
        Some(st) => {
            st.cancelled.send_replace(true);
            true
        }
        None => false,
    }
}

/// 检查点：会话是否已被请求停止
pub fn is_cancelled(session_id: &str) -> bool {
    states()
        .lock()
        .get(session_id)
        .map(|st| *st.cancelled.borrow())
        .unwrap_or(false)
}

/// 订阅实际启动时的轮次。轮次正常结束不应取消跨轮次的后台任务。
pub(crate) fn subscribe_cancel(session_id: &str) -> Option<watch::Receiver<bool>> {
    states()
        .lock()
        .get(session_id)
        .map(|st| st.cancelled.subscribe())
}

pub(crate) async fn wait_for_cancel(cancellation: Option<watch::Receiver<bool>>) {
    if let Some(mut receiver) = cancellation {
        if receiver.wait_for(|cancelled| *cancelled).await.is_ok() {
            return;
        }
    }
    // 没有轮次，或其已正常结束：仍由任务自身的超时/显式 kill 管理。
    std::future::pending::<()>().await;
}

/// 本轮已记录的工具调用数
pub fn tool_count(session_id: &str) -> usize {
    states()
        .lock()
        .get(session_id)
        .map(|st| st.tools.len())
        .unwrap_or(0)
}

#[cfg(test)]
mod serde_contract_tests {
    use super::*;

    #[test]
    fn steering_admission_is_fifo_and_closes_atomically() {
        let session = format!("steer-round-{}", crate::types::new_id());
        begin(&session);
        let persisted = std::cell::Cell::new(0);
        let first = admit_steering(&session, "第一条", || {
            persisted.set(persisted.get() + 1);
            Ok(())
        })
        .unwrap();
        let second = admit_steering(&session, "第二条", || {
            persisted.set(persisted.get() + 1);
            Ok(())
        })
        .unwrap();
        assert_eq!(persisted.get(), 2);
        assert!(first.sequence < second.sequence);
        let pending = take_steering(&session);
        assert_eq!(pending.len(), 2);
        assert_eq!(pending[0].text, "第一条");
        assert_eq!(pending[1].text, "第二条");

        assert!(take_or_close_steering(&session).is_empty());
        let called = std::cell::Cell::new(false);
        assert!(matches!(
            admit_steering(&session, "迟到输入", || {
                called.set(true);
                Ok(())
            }),
            Err(SteeringAdmissionError::Closed)
        ));
        assert!(!called.get(), "已关闭的轮次不能持久化为 steering");
        end(&session);
    }

    /// 序列化契约回归（P0 修复）：工具步骤键名 MUST 为 camelCase（callId），
    /// 且读取兼容旧蛇形键（call_id）——存量消息零迁移。
    /// 此前容器级 rename_all 对 enum 变体字段不生效，落库蛇形键导致
    /// 前端 callId 关联全失配、刷新后工具卡消失。
    #[test]
    fn 工具步骤_序列化键名契约() {
        // 1) 序列化：出 callId，不出蛇形 call_id
        let j = serde_json::to_string(&TraceStep::Tool {
            call_id: "t1".into(),
        })
        .unwrap();
        assert!(j.contains("\"callId\""), "序列化应含 callId：{j}");
        assert!(!j.contains("call_id"), "序列化不应含蛇形键：{j}");
        // tag 值维持小写（前端按 kind=tool 分支渲染）
        assert!(j.contains("\"kind\":\"tool\""), "tag 值应为 tool：{j}");

        // 2) 反序列化：吃 camelCase（新数据）
        let v: TraceStep = serde_json::from_str(r#"{"kind":"tool","callId":"t2"}"#).unwrap();
        match v {
            TraceStep::Tool { call_id } => assert_eq!(call_id, "t2"),
            other => panic!("应解析为 Tool：{other:?}"),
        }

        // 3) 反序列化：吃蛇形旧键（存量消息兼容）
        let v: TraceStep = serde_json::from_str(r#"{"kind":"tool","call_id":"t3"}"#).unwrap();
        match v {
            TraceStep::Tool { call_id } => assert_eq!(call_id, "t3"),
            other => panic!("应解析为 Tool：{other:?}"),
        }

        // 4) 思考段落序列化形态不受影响
        let j = serde_json::to_string(&TraceStep::Thinking { text: "hi".into() }).unwrap();
        assert!(j.contains("\"kind\":\"thinking\""), "思考 tag：{j}");
        let v: TraceStep = serde_json::from_str(&j).unwrap();
        assert!(matches!(v, TraceStep::Thinking { ref text } if text == "hi"));
    }
}
