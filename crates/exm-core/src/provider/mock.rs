//! provider 域分文件（自 provider.rs 按职责拆出，纯搬移零行为变更）。

use super::*;
/// 未配置端点/密钥的模型档案：调用即返回可执行的配置指引。
///
/// 产品行为：**不做任何静默降级**——没有模型就没有回答，并明确指出去哪里配置。
pub struct UnconfiguredProvider {
    /// 展示给用户的档案标识（如「全局通道」或档案 id）
    pub label: String,
}

impl UnconfiguredProvider {
    pub fn new(label: impl Into<String>) -> Self {
        UnconfiguredProvider {
            label: label.into(),
        }
    }

    fn hint(&self) -> String {
        format!(
            "模型通道未配置（{}）：请在「提供商」页填写端点与 API Key，\
             或执行 `exm model add <id> --base-url <url> --api-key <key> --orch-model <m> --unit-model <m>` 后 `exm model use <id>`。",
            self.label
        )
    }
}

#[async_trait]
impl LlmProvider for UnconfiguredProvider {
    fn name(&self) -> &'static str {
        "unconfigured"
    }

    async fn chat(&self, _req: ChatRequest) -> anyhow::Result<ChatResponse> {
        anyhow::bail!("{}", self.hint())
    }

    async fn stream(
        &self,
        _req: ChatRequest,
        _tx: UnboundedSender<StreamDelta>,
    ) -> anyhow::Result<ChatResponse> {
        anyhow::bail!("{}", self.hint())
    }

    async fn embed(&self, _model: &str, _texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
        anyhow::bail!("{}", self.hint())
    }

    async fn transcribe(
        &self,
        _model: &str,
        _audio: &[u8],
        _filename: &str,
    ) -> anyhow::Result<String> {
        anyhow::bail!("{}", self.hint())
    }

    async fn speak(&self, _model: &str, _text: &str) -> anyhow::Result<Vec<u8>> {
        anyhow::bail!("{}", self.hint())
    }
}

// ---------------------------------------------------------------- 测试替身（确定性）

/// 测试替身：计划/子个体/收束等请求各自产出合法契约产物，供单测与端到端验收使用。
///
/// 仅在测试代码或显式设置 `EXM_LLM_MOCK=1`（联调流程时）启用；产品运行时不参与推理。
pub struct MockLlmProvider;

/// Eval→act 通过路径开关（仅测试）：置位后下一次 Auto Agent Eval 产出
/// act:true（一次性，用毕复位）——auto_agent_tick 的 user 文本固定，无法用
/// 输入标记驱动；通过路径由测试显式拨动此开关断言全链路。
pub static MOCK_EVAL_ACT_ONCE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// 确定性伪嵌入（Mock 通道）：64 维，token 哈希累加后归一——同义文本向量稳定相近
fn mock_embedding(text: &str) -> Vec<f32> {
    let mut v = vec![0f32; 64];
    let chars: Vec<char> = text.chars().collect();
    for i in 0..chars.len() {
        let gram: String = if i + 1 < chars.len() {
            chars[i..i + 2].iter().collect()
        } else {
            chars[i].to_string()
        };
        let h: u32 = gram.bytes().fold(2166136261u32, |a, b| {
            a.wrapping_mul(16777619).wrapping_add(b as u32)
        });
        v[(h as usize) % 64] += 1.0;
    }
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-6);
    v.iter().map(|x| x / norm).collect()
}

#[async_trait]
impl LlmProvider for MockLlmProvider {
    fn name(&self) -> &'static str {
        "mock"
    }

    async fn chat(&self, req: ChatRequest) -> anyhow::Result<ChatResponse> {
        Ok(ChatResponse {
            content: respond(&req),
            reasoning: String::new(),
            prompt_tokens: 100,
            completion_tokens: 200,
            tool_calls: Vec::new(),
        })
    }

    async fn embed(&self, _model: &str, texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|t| mock_embedding(t)).collect())
    }

    async fn stream(
        &self,
        req: ChatRequest,
        tx: UnboundedSender<StreamDelta>,
    ) -> anyhow::Result<ChatResponse> {
        let content = respond(&req);
        // 思维链分轨：先流一段确定性思考（与真实推理模型同形状：思考 → 正文），
        // 让过程轨迹（有序步骤）在 Mock 联调/验收环境同样有数据可走。
        let _ = tx.send(StreamDelta::Thinking(
            "（模拟思维链）先读契约与上下文，再产出确定性结果。".to_string(),
        ));
        // 按字符边界切块（每块 ≈48 字节）：此前按字节切，边界落在多字节字符中间时
        // from_utf8_lossy 会把残字替换成 U+FFFD——中文模拟输出满屏「���」。
        for piece in char_chunks(&content, 48) {
            let _ = tx.send(StreamDelta::Text(piece));
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        }
        Ok(ChatResponse {
            content,
            reasoning: String::new(),
            prompt_tokens: 100,
            completion_tokens: 200,
            tool_calls: Vec::new(),
        })
    }
}

/// 按**字符**边界把文本切成约 `max_bytes` 一块的流式增量。
///
/// 为什么不能 `as_bytes().chunks(n)`：UTF-8 多字节字符被边界切开时，
/// `String::from_utf8_lossy` 会把残字替换成 U+FFFD（终端里就是「���」），
/// 中文内容每 ~48 字节必现一次。逐字符累积到字节预算再收口即可。
pub(crate) fn char_chunks(text: &str, max_bytes: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut bytes = 0usize;
    for ch in text.chars() {
        let ch_len = ch.len_utf8();
        // 单字符超预算（极长组合字符等）时单独成块，避免死循环
        if !cur.is_empty() && bytes + ch_len > max_bytes {
            out.push(std::mem::take(&mut cur));
            bytes = 0;
        }
        cur.push(ch);
        bytes += ch_len;
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn respond(req: &ChatRequest) -> String {
    let system = req
        .messages
        .iter()
        .find(|m| m.role == "system")
        .map(|m| m.content.clone())
        .unwrap_or_default();
    let user = req
        .messages
        .last()
        .map(|m| m.content.clone())
        .unwrap_or_default();

    if system.contains("记忆压缩") {
        let kept: String = user.chars().take(120).collect();
        format!("- 【已压缩】{kept}\n- 细节已归档，按需查阅")
    } else if system.contains("Auto Agent Eval") {
        // Auto Agent 自评（9.2 e2e）：确定性两态——开关拨动（一次性）或 user 文本
        // 带标记时产出「值得做」，否则保守拒绝（与 parse_auto_eval 契约对齐，
        // 驱动 Eval→act 链路与退避断言）
        let act = MOCK_EVAL_ACT_ONCE.swap(false, std::sync::atomic::Ordering::SeqCst)
            || user.contains("值得做");
        if act {
            r#"{"act": true, "task": "检查记忆库健康度", "why": "e2e 驱动的确定性自评"}"#.to_string()
        } else {
            r#"{"act": false}"#.to_string()
        }
    } else if system.contains("自主记忆摘要") {
        // 记忆归纳（9.1）：确定性摘要输出（不依赖模型质量，驱动去重/落库断言）
        format!("E2E 会话主题归纳。{}", user.chars().take(60).collect::<String>())
    } else if system.contains("会话压缩") {
        "目标与进展：用户持续在推进当前任务；关键结论与未决事项已由最近对话承载。".to_string()
    } else if system.contains("经验改进要点合成") {
        adaptation_text(&system)
    } else if user.contains("exm-tool-loop-ok") {
        // 智能体直接循环第二步:工具回灌已收到,收敛为最终文本
        "▸ 工具回灌已收到：输出包含 exm-tool-loop-ok，任务完成。".to_string()
    } else if user.contains("触发工具循环") {
        // 智能体直接循环第一步:文本协议工具调用(确定性)
        r#"{"tool": "terminal", "args": {"command": "echo exm-tool-loop-ok"}}"#.to_string()
    } else if user.starts_with("【收束请求】") {
        converge_text(&user)
    } else if system.contains("OrchestratorPlan") {
        // 指挥体规划请求（内置组与自定义组共用该契约标记）
        plan_json(&system, &user)
    } else {
        sync_json(&system, &user)
    }
}

/// 模拟通道的经验要点合成：确定性产出
fn adaptation_text(system: &str) -> String {
    let agent = system
        .split("个体：")
        .nth(1)
        .and_then(|s| s.split("（").next())
        .unwrap_or("个体")
        .trim()
        .to_string();
    format!(
        "- {agent} 在失败与受阻任务上先给出最小复现路径，再扩大处理范围
- 受阻回流必须携带解除条件，避免重复尝试同一方案
- 结论与证据等级对齐：无直接证据的判断显式标注为推断
- 输出前自查 SyncReport 契约字段完整性，缺项视为未完成"
    )
}

fn goal_of(user: &str) -> String {
    user.split("【要求】")
        .next()
        .unwrap_or(user)
        .replace("用户任务输入：", "")
        .trim()
        .chars()
        .take(200)
        .collect::<String>()
}

/// 从规划提示的「可调度子个体」清单解析 identifier（组感知：自定义组也能出合法计划）。
/// 仅接受 "- ident（" 形式（全角左括号紧跟），避免误吞可靠性统计/记忆等其它块。
fn parse_dispatchable(system: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_block = false;
    for line in system.lines() {
        if line.starts_with("## ") {
            in_block = line.contains("可调度子个体");
            continue;
        }
        if !in_block {
            continue;
        }
        let Some(rest) = line.strip_prefix("- ") else {
            continue;
        };
        let ident: String = rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
            .collect();
        if !ident.is_empty() && rest[ident.len()..].starts_with('（') {
            out.push(ident);
        }
    }
    out.dedup();
    out
}

fn plan_json(system: &str, user: &str) -> String {
    let goal = goal_of(user);
    let agents = parse_dispatchable(system);

    // 组内无子个体：主智能体 L0 直答（用户可先建组再让主智能体创建个体）
    if agents.is_empty() {
        let l0 = serde_json::json!({
            "routeLevel": "L0",
            "goal": goal,
            "boundary": {
                "inScope": ["当前会话目标"],
                "forbidden": ["不可逆变更", "越权调度"]
            },
            "acceptance": ["直接回答完整覆盖目标"],
            "nodes": [],
            "finalAnswer": [
                { "tag": "报告", "text": format!("（模拟通道·L0 直答）{goal}") },
                { "tag": "观测", "text": "当前组仅有主智能体；可要求主智能体创建子个体后再分发任务。" }
            ]
        });
        return format!(
            "```json\n{}\n```",
            serde_json::to_string_pretty(&l0).unwrap_or_default()
        );
    }

    let a = |i: usize| agents.get(i).cloned().unwrap_or_else(|| agents[0].clone());
    let nodes = if agents.len() == 1 {
        serde_json::json!([
            {
                "id": "T1", "title": "任务执行", "agentIdentifier": a(0),
                "objective": format!("完成以下任务的全部职责范围内工作：{goal}"),
                "acceptance": ["结论带证据等级", "残余未知显式保留"],
                "dependsOn": [], "priority": "P1"
            },
            {
                "id": "T2", "title": "复核收束", "agentIdentifier": a(0),
                "objective": format!("复核 T1 产出，整理结论、风险与下一步：{goal}"),
                "acceptance": ["结论带证据等级", "风险与残余未知完整"],
                "dependsOn": ["T1"], "priority": "P1"
            }
        ])
    } else {
        serde_json::json!([
            {
                "id": "T1", "title": "背景补齐", "agentIdentifier": a(0),
                "objective": format!("补齐以下任务的背景、依赖、约束与边界：{goal}"),
                "acceptance": ["背景事实带证据等级", "缺口显式列出"],
                "dependsOn": [], "priority": "P1"
            },
            {
                "id": "T2", "title": "入口侦察", "agentIdentifier": a(1 % agents.len()),
                "objective": format!("定位与任务相关的文件、入口、依赖与现有模式：{goal}"),
                "acceptance": ["相关位置清单", "现有模式说明"],
                "dependsOn": [], "priority": "P1"
            },
            {
                "id": "T3", "title": "方案比对", "agentIdentifier": a(0),
                "objective": format!("基于 T1/T2 回流，对比候选路径的差异、取舍与适配边界：{goal}"),
                "acceptance": ["候选对比表", "推荐项与理由"],
                "dependsOn": ["T1", "T2"], "priority": "P1"
            },
            {
                "id": "T4", "title": "汇报收束", "agentIdentifier": a(agents.len().min(3) - 1),
                "objective": format!("汇总全部上游回流，输出结构化结论、风险与下一步：{goal}"),
                "acceptance": ["结论带证据等级", "风险与残余未知完整"],
                "dependsOn": ["T3"], "priority": "P1"
            }
        ])
    };

    let plan = serde_json::json!({
        "routeLevel": if agents.len() == 1 { "L1" } else { "L2" },
        "boundary": {
            "inScope": ["当前会话目标", "用户提供的材料"],
            "forbidden": ["不可逆变更", "越权调度", "无证据结论"]
        },
        "goal": goal,
        "acceptance": ["产出带证据等级的结论", "残余未知显式保留"],
        "nodes": nodes
    });
    format!(
        "```json\n{}\n```",
        serde_json::to_string_pretty(&plan).unwrap_or_default()
    )
}

fn sync_json(system: &str, user: &str) -> String {
    // identifier 只取 ASCII 字母/数字/连字符（提示词里紧跟中文说明，不能整段吞掉）
    let identifier = system
        .split("identifier: ")
        .nth(1)
        .map(|s| {
            s.chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
                .collect::<String>()
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown-agent".to_string());

    let (node_id, objective) = if let Some(v) = crate::parse::extract_json(user) {
        (
            v.get("taskNodeId")
                .and_then(|x| x.as_str())
                .unwrap_or("T?")
                .to_string(),
            v.get("objective")
                .and_then(|x| x.as_str())
                .unwrap_or("未解析到目标")
                .chars()
                .take(120)
                .collect::<String>(),
        )
    } else {
        ("T?".to_string(), "未解析到目标".to_string())
    };

    let report = serde_json::json!({
        "sourceAgent": identifier,
        "taskNodeId": node_id,
        "status": "done",
        "statements": [
            { "tag": "报告", "text": format!("节点 {node_id} 执行完成（模拟通道）。目标：{objective}") },
            { "tag": "肯定", "text": "已按 DispatchOrder 的边界与验收口径完成本节点职责范围内的分析。", "evidenceLevel": "B" },
            { "tag": "观测", "text": "当前为模拟执行通道；接入真实 LLM 端点后此处为实际推理产出。" }
        ],
        "summary": format!("节点 {node_id}（{identifier}）已完成：{objective}。结论为模拟产出，证据等级 B，未发现阻断项。"),
        "evidence": [{
            "level": "B", "kind": "reasoning",
            "ref": format!("mock://{identifier}/{node_id}"),
            "note": "模拟通道占位证据；真实通道下为代码/日志/测试等直接证据。"
        }],
        "risks": [{
            "text": "模拟通道结论不可直接支撑高风险决策",
            "severity": "low",
            "revertPath": "配置真实 LLM 端点后重跑该节点"
        }],
        "blockers": [],
        "nextSuggestion": {},
        "confidence": 0.82
    });
    format!(
        "```json\n{}\n```",
        serde_json::to_string_pretty(&report).unwrap_or_default()
    )
}

fn converge_text(user: &str) -> String {
    let goal = goal_of(user);
    [
        "## 任务边界".to_string(),
        "- 目标：以用户输入为准；排除项：不可逆变更与越权动作。".into(),
        "".into(),
        "## 已启用链路".into(),
        "- 链路与个体序列见编排图；并行安排按 DAG 拓扑执行（模拟通道）。".into(),
        "".into(),
        "## 关键结论".into(),
        "- 【报告】全部节点已回流并通过 SyncReport 契约校验。（证据B，置信度 0.82）".into(),
        "- 【观测】当前为模拟执行通道，结论结构完整、内容为占位。".into(),
        "".into(),
        "## 冲突裁决（如有）".into(),
        "- 本轮未出现 need_arbitration 回流，无需裁决。".into(),
        "".into(),
        "## 残余未知".into(),
        "- 接入真实 LLM 端点后需以 A/B 级证据复核本轮结论。".into(),
        "".into(),
        "## 最终交付".into(),
        "- 流程闭环已验证：对话 → 分解 → 并行调度 → 回流 → 收束。".into(),
        format!("- 输入摘要：{goal}"),
    ]
    .join("\n")
}

