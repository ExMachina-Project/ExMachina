//! 无头单轮调用（headless print）—— 脚本 / CI / 外部编排的稳定入口。
//!
//! `exm -p "任务"` / `echo ... | exm -p`：不进任何交互 UI，单轮执行后退出。
//! 输出契约三种：text（缺省，纯正文）/ json（单行结构化）/ stream-json（JSONL 过程事件）。
//! 退出码：0 成功 / 2 使用错误（参数、无匹配会话、空输入）/ 1 执行失败。

use exm_core::types::{CoreEvent, Statement};
use exm_core::Core;
use std::io::Write;
use std::sync::Arc;
use tokio::sync::broadcast::error::RecvError;

/// 无头输出格式（--output-format）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OutputFormat {
    #[default]
    Text,
    Json,
    StreamJson,
}

impl OutputFormat {
    pub fn parse(s: &str) -> anyhow::Result<Self> {
        match s {
            "text" => Ok(OutputFormat::Text),
            "json" => Ok(OutputFormat::Json),
            "stream-json" => Ok(OutputFormat::StreamJson),
            other => Err(anyhow::anyhow!(
                "无效的 --output-format：{other}（可选 text | json | stream-json）"
            )),
        }
    }
}

/// 无头调用入参（-p 路径的解析产物；文本非空与格式合法由 lib.rs 前置校验）
pub struct HeadlessOptions {
    /// 任务文本（显式参数或 stdin 读入）
    pub text: String,
    /// 目标会话（None = 新建）
    pub session: Option<String>,
    pub format: OutputFormat,
}

/// 无头调用结果：退出码契约的载体（0/1/2 由 lib.rs 映射到 process::exit）
pub struct HeadlessOutcome {
    /// 0 成功 / 1 执行失败 / 2 使用错误
    pub code: i32,
    /// 失败时的错误信息（text 模式走 stderr；json/stream-json 已并入终态输出，恒 None）
    pub error: Option<String>,
}

impl HeadlessOutcome {
    fn usage_err(msg: impl Into<String>) -> Self {
        HeadlessOutcome { code: 2, error: Some(msg.into()) }
    }
}

/// 无头单轮执行主入口。
///
/// 事件消费与 core.chat 并行（订阅先起，与 chat::run_round 同构防漏首事件）：
/// - text：静默等待收束，只打正文
/// - json：静默等待收束，打单行 JSON（sessionId/text/turn/usage）
/// - stream-json：过程事件逐行 JSONL（token 64ms 聚合），turn.end 终态收尾
pub async fn run_headless(core: &Arc<Core>, opts: HeadlessOptions) -> HeadlessOutcome {
    // 会话解析：显式 id 不存在 = 使用错误（退出码 2）
    let session = match &opts.session {
        Some(id) => match core.store.get_session(id) {
            Ok(Some(s)) => s,
            Ok(None) => return HeadlessOutcome::usage_err(format!("会话不存在: {id}")),
            Err(e) => return HeadlessOutcome::usage_err(format!("读取会话失败: {e}")),
        },
        None => {
            let title: String = opts.text.chars().take(30).collect();
            match core.create_session(&title) {
                Ok(s) => s,
                Err(e) => return HeadlessOutcome::usage_err(format!("创建会话失败: {e}")),
            }
        }
    };
    let sid = session.id.clone();
    let pump_sid = sid.clone();

    let fmt = opts.format;
    let mut rx = core.subscribe();
    let pump = tokio::spawn(async move {
        let mut collector = EventCollector::new(fmt);
        loop {
            match rx.recv().await {
                Ok(evt) => {
                    if evt.session_id != pump_sid {
                        continue;
                    }
                    for line in collector.consume(&evt) {
                        let mut out = std::io::stdout().lock();
                        let _ = writeln!(out, "{line}");
                        let _ = out.flush();
                    }
                    if collector.finished {
                        return collector;
                    }
                }
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => return collector,
            }
        }
    });

    // 单轮执行；失败 = 退出码 1（执行失败），错误按格式分流输出
    if let Err(e) = core.chat(&sid, &opts.text).await {
        pump.abort();
        return render_failure(fmt, &sid, e.to_string());
    }
    // 收束等待（与 run_round 同量级：600s 上限防挂死）
    let collector = match tokio::time::timeout(std::time::Duration::from_secs(600), pump).await {
        Ok(Ok(c)) => c,
        _ => return render_failure(fmt, &sid, "等待收束超时".to_string()),
    };
    // 事件流未达收束即关闭：执行失败口径
    if !collector.finished {
        let msg = "事件流提前结束".to_string();
        return match fmt {
            OutputFormat::Text => HeadlessOutcome { code: 1, error: Some(msg) },
            _ => render_failure(fmt, &sid, msg),
        };
    }
    // run.error 语义的失败（收束已到但带错误标记）
    if let Some(errmsg) = &collector.error {
        let msg = errmsg.clone();
        return match fmt {
            OutputFormat::Text => HeadlessOutcome { code: 1, error: Some(msg) },
            _ => render_failure(fmt, &sid, msg),
        };
    }

    let text = collector.final_text.clone().unwrap_or_default();
    // turn / usage：收束后从轮次快照读（chat 返回时快照已落库，比事件竞态可靠）
    let snap = core.store.latest_turn_snapshot(&sid).ok().flatten();
    match fmt {
        OutputFormat::Text => {
            println!("{text}");
            HeadlessOutcome { code: 0, error: None }
        }
        OutputFormat::Json => {
            let mut payload = serde_json::json!({
                "sessionId": sid,
                "text": text,
            });
            if let Some(s) = &snap {
                payload["turn"] = serde_json::json!(s.turn);
                payload["usage"] = serde_json::json!({
                    "promptTokens": s.prompt_tokens,
                    "completionTokens": s.completion_tokens,
                });
            }
            println!("{}", serde_json::to_string(&payload).unwrap_or_default());
            HeadlessOutcome { code: 0, error: None }
        }
        OutputFormat::StreamJson => {
            // turn.end 终态行已在 consume 内打印（含收束正文）
            HeadlessOutcome { code: 0, error: None }
        }
    }
}

/// 失败终态按格式分流输出（core.chat 报错 / 超时 / run.error 共用）
fn render_failure(fmt: OutputFormat, sid: &str, msg: String) -> HeadlessOutcome {
    match fmt {
        OutputFormat::Text => HeadlessOutcome { code: 1, error: Some(msg) },
        OutputFormat::Json => {
            let payload = serde_json::json!({
                "sessionId": sid,
                "text": "",
                "error": msg,
            });
            println!("{}", serde_json::to_string(&payload).unwrap_or_default());
            HeadlessOutcome { code: 1, error: None }
        }
        OutputFormat::StreamJson => {
            let payload = serde_json::json!({
                "type": "turn.end",
                "sessionId": sid,
                "error": msg,
            });
            println!("{}", serde_json::to_string(&payload).unwrap_or_default());
            HeadlessOutcome { code: 1, error: None }
        }
    }
}

// ---------------------------------------------------------------- 事件收集

/// token 聚合窗口（毫秒）：防每 token 一行刷爆管道
const TOKEN_AGG_MS: u64 = 64;

/// 事件流 → 三种输出格式的统一收集器。
///
/// stream-json 模式逐事件转行（consume 返回待打印行集）；text/json 模式
/// 只记收束产物（final_text / error），过程事件静默丢弃。
struct EventCollector {
    fmt: OutputFormat,
    /// 收束正文（statements 按行拼接；run.finished 未达 = None）
    final_text: Option<String>,
    /// run.error 的失败信息（收到即 finished）
    error: Option<String>,
    finished: bool,
    /// stream-json：turn.start 是否已发
    turn_started: bool,
    /// token 64ms 聚合缓冲：窗口内 delta 拼接后一行发射
    token_buf: String,
    token_window: tokio::time::Instant,
}

impl EventCollector {
    fn new(fmt: OutputFormat) -> Self {
        EventCollector {
            fmt,
            final_text: None,
            error: None,
            finished: false,
            turn_started: false,
            token_buf: String::new(),
            token_window: tokio::time::Instant::now(),
        }
    }

    fn stream(&self) -> bool {
        self.fmt == OutputFormat::StreamJson
    }

    fn line(payload: &serde_json::Value) -> String {
        serde_json::to_string(payload).unwrap_or_default()
    }

    /// 消费一个事件，返回待打印行集（非 stream 模式恒空）。
    fn consume(&mut self, evt: &CoreEvent) -> Vec<String> {
        if self.finished {
            return Vec::new();
        }
        let mut lines = Vec::new();
        if self.stream() && !self.turn_started {
            // 轮次开始标记：首个到达本会话的事件前发射
            self.turn_started = true;
            lines.push(Self::line(&serde_json::json!({
                "type": "turn.start",
                "sessionId": evt.session_id,
            })));
        }
        match evt.kind.as_str() {
            "orchestrator.token" => {
                if self.stream() {
                    let delta = evt.payload.get("delta").and_then(|v| v.as_str()).unwrap_or("");
                    self.token_buf.push_str(delta);
                    if self.token_window.elapsed().as_millis() as u64 >= TOKEN_AGG_MS {
                        if let Some(l) = self.take_token_line() {
                            lines.push(l);
                        }
                    }
                }
            }
            "tool.call" => {
                if self.stream() {
                    self.flush_tokens_into(&mut lines);
                    lines.push(Self::line(&serde_json::json!({
                        "type": "tool.call",
                        "tool": evt.payload.get("tool").cloned().unwrap_or_default(),
                        "args": evt.payload.get("args").cloned().unwrap_or_default(),
                    })));
                }
            }
            "tool.result" => {
                if self.stream() {
                    self.flush_tokens_into(&mut lines);
                    lines.push(Self::line(&serde_json::json!({
                        "type": "tool.result",
                        "ok": evt.payload.get("ok").and_then(|v| v.as_bool()).unwrap_or(false),
                        "durationMs": evt
                            .payload
                            .get("durationMs")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0),
                    })));
                }
            }
            "run.finished" => {
                self.finished = true;
                let statements: Vec<Statement> = evt
                    .payload
                    .get("statements")
                    .cloned()
                    .and_then(|v| serde_json::from_value(v).ok())
                    .unwrap_or_default();
                self.final_text = Some(
                    statements
                        .iter()
                        .map(|s| s.text.as_str())
                        .collect::<Vec<_>>()
                        .join("\n"),
                );
                if self.stream() {
                    self.flush_tokens_into(&mut lines);
                    lines.push(Self::line(&serde_json::json!({
                        "type": "turn.end",
                        "sessionId": evt.session_id,
                        "text": self.final_text,
                        "statements": evt.payload.get("statements").cloned().unwrap_or_default(),
                    })));
                }
            }
            "run.error" => {
                self.finished = true;
                let msg = evt
                    .payload
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("运行失败")
                    .to_string();
                self.error = Some(msg);
                if self.stream() {
                    self.flush_tokens_into(&mut lines);
                    lines.push(Self::line(&serde_json::json!({
                        "type": "turn.end",
                        "sessionId": evt.session_id,
                        "error": self.error,
                    })));
                }
            }
            // 其余过程事件（graph/sync/memory/thinking 等）无头模式不转发：
            // 契约面保持最小稳定，未来需要再增发不改既有行
            _ => {}
        }
        lines
    }

    /// 取出聚合 token 行（缓冲空则只重置窗口）
    fn take_token_line(&mut self) -> Option<String> {
        self.token_window = tokio::time::Instant::now();
        if self.token_buf.is_empty() {
            return None;
        }
        let text = std::mem::take(&mut self.token_buf);
        Some(Self::line(&serde_json::json!({ "type": "token", "text": text })))
    }

    /// 工具/收束事件前清空 token 缓冲（保序：缓冲内文本先于后续事件行）
    fn flush_tokens_into(&mut self, lines: &mut Vec<String>) {
        if let Some(l) = self.take_token_line() {
            lines.push(l);
        }
    }
}

// ---------------------------------------------------------------- 单测

#[cfg(test)]
mod tests {
    use super::*;

    fn mock_core() -> Arc<Core> {
        use exm_core::types::{AgentDefinition, Tier};
        let dir = std::env::temp_dir().join(format!("exm-headless-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut cfg = exm_core::config::ExmConfig::load(&dir);
        cfg.use_mock = true;
        cfg.memory_enabled = false;
        let core = Core::with_config(cfg).expect("创建 Core 失败");
        // 无头链路走完整编排：组 + 主智能体是 chat 的前置（与 core 侧测试同构）
        core.registry()
            .create_group(Some("t".into()), "无头测试组", "")
            .unwrap();
        let def = AgentDefinition {
            name: "指挥体".into(),
            identifier: "head-orch".into(),
            domain: "测试".into(),
            tier: Tier::Orchestrator,
            description: "无头测试主智能体".into(),
            capabilities: vec![],
            tools: vec![],
            when_to_call: String::new(),
            link: None,
            max_steps: None,
            dependencies: vec![],
            composable_with: vec![],
            input_schema: Default::default(),
            output_schema: Default::default(),
            prompt_file: String::new(),
            model_hint: None,
        };
        core.registry().upsert_agent("t", def, None).unwrap();
        core.registry().set_primary("t", "head-orch").unwrap();
        core.registry().set_active_group("t").unwrap();
        Arc::new(core)
    }

    async fn run(fmt: OutputFormat) -> HeadlessOutcome {
        let core = mock_core();
        run_headless(
            &core,
            HeadlessOptions {
                text: "无头测试任务".into(),
                session: None,
                format: fmt,
            },
        )
        .await
    }

    /// text 格式：mock 通道跑通全链路，退出码 0
    #[tokio::test]
    async fn 无头_text_格式_成功路径() {
        let out = run(OutputFormat::Text).await;
        assert_eq!(out.code, 0, "成功执行退出码 0");
        assert!(out.error.is_none());
    }

    /// json 格式：退出码 0（stdout 单行 JSON 由人工/脚本端到端验证）
    #[tokio::test]
    async fn 无头_json_格式_成功路径() {
        let out = run(OutputFormat::Json).await;
        assert_eq!(out.code, 0, "json 成功路径退出码 0");
    }

    /// stream-json 格式：退出码 0（JSONL 顺序由脚本端到端验证）
    #[tokio::test]
    async fn 无头_stream_json_格式_成功路径() {
        let out = run(OutputFormat::StreamJson).await;
        assert_eq!(out.code, 0, "stream-json 成功路径退出码 0");
    }

    /// 显式会话不存在：使用错误（退出码 2），不发起 LLM 调用
    #[tokio::test]
    async fn 无头_会话不存在_使用错误() {
        let core = mock_core();
        let out = run_headless(
            &core,
            HeadlessOptions {
                text: "x".into(),
                session: Some("no-such-session".into()),
                format: OutputFormat::Text,
            },
        )
        .await;
        assert_eq!(out.code, 2, "无匹配会话 = 使用错误（2）");
        assert!(out.error.is_some(), "使用错误必须带信息");
    }

    /// 输出格式解析：合法三态 + 非法值报错
    #[test]
    fn 输出格式_解析三态与非法值() {
        assert_eq!(OutputFormat::parse("text").unwrap(), OutputFormat::Text);
        assert_eq!(OutputFormat::parse("json").unwrap(), OutputFormat::Json);
        assert_eq!(OutputFormat::parse("stream-json").unwrap(), OutputFormat::StreamJson);
        assert!(OutputFormat::parse("yaml").is_err(), "非法格式必须报错");
    }

    /// token 聚合：多 delta 入缓冲后一次取行，窗口重置
    #[test]
    fn token_聚合_缓冲与窗口() {
        let mut c = EventCollector::new(OutputFormat::StreamJson);
        c.token_buf.push_str("你好");
        c.token_buf.push_str("世界");
        let line = c.take_token_line().expect("非空缓冲必须出行");
        assert!(line.contains("你好世界"), "聚合行应含全部缓冲文本：{line}");
        assert!(c.take_token_line().is_none(), "取行后缓冲清空");
    }
}
