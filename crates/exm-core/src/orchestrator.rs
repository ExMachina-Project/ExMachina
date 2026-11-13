//! 指挥体宿主 —— docs/01 §2.1
//! 收拢 → 分解 → 选路 → 派发 → 监控 → 裁决 → 收束

use crate::memory::{MemoryDraft, MemoryKind, MemoryStore};
use crate::parse;
use crate::provider::{
    ChatMessage, ChatRequest, FailoverState, LlmProvider, ModelChain, ModelPool, StreamDelta,
};
use crate::registry::LocalRegistry;
use crate::runtime::AgentRuntime;
use crate::store::Store;
use crate::task::{NewNode, NodeOutcome, Reports, Scheduler, TaskGraphModel};
use crate::types::*;
use futures_util::future::BoxFuture;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc::unbounded_channel, Mutex};

pub const ORCHESTRATOR_ID: &str = "orchestrator";

// ---------------------------------------------------------------- 会话轮次模式（思考模式：直答 / 集群）

/// 本轮模式暂存（会话 → 模式）：对话请求携带、`plan()` 取走即消费。
/// direct = 强制 L0 直答（一轮工具循环内闭环）；full = 强制拆解派发（禁止 L0）。
static SESSION_MODES: std::sync::OnceLock<
    parking_lot::Mutex<std::collections::HashMap<String, String>>,
> = std::sync::OnceLock::new();

fn session_modes() -> &'static parking_lot::Mutex<std::collections::HashMap<String, String>> {
    SESSION_MODES.get_or_init(|| parking_lot::Mutex::new(std::collections::HashMap::new()))
}

/// 登记某会话下一轮的执行模式（"direct" | "full"；其他值忽略）
pub fn set_session_mode(session_id: &str, mode: &str) {
    match mode {
        "direct" | "full" => {
            session_modes()
                .lock()
                .insert(session_id.to_string(), mode.to_string());
        }
        _ => {}
    }
}

fn take_session_mode(session_id: &str) -> Option<String> {
    session_modes().lock().remove(session_id)
}

// ---------------------------------------------------------------- IM 通道表达纪律

/// IM 通道会话登记（会话 → 是否经即时通讯通道交互）。
/// 通道 begin 时登记、会话删除时除名；轮次组装系统提示时读取——
/// IM 场景（QQ/TG/Discord 等）注入简洁表达纪律，避免一次性回复大量内容。
static IM_SESSIONS: std::sync::OnceLock<parking_lot::Mutex<std::collections::HashSet<String>>> =
    std::sync::OnceLock::new();

fn im_sessions() -> &'static parking_lot::Mutex<std::collections::HashSet<String>> {
    IM_SESSIONS.get_or_init(|| parking_lot::Mutex::new(std::collections::HashSet::new()))
}

/// 登记会话为 IM 通道交互（begin 时调用；重复登记幂等）
pub fn mark_im_session(session_id: &str) {
    im_sessions().lock().insert(session_id.to_string());
}

/// 除名（会话删除 / 通道解绑时调用）
pub fn unmark_im_session(session_id: &str) {
    im_sessions().lock().remove(session_id);
}

/// 会话是否经 IM 通道交互
pub fn is_im_session(session_id: &str) -> bool {
    im_sessions().lock().contains(session_id)
}

/// 探活消息判定（IM 通道短路直答用）：纯问候 / 在线确认 / 简单致谢收尾，
/// 无任何任务语义——不值得进规划流水线（实证：全量注册表 + 记忆召回注入，
/// 就为回一句「在」）。规则刻意保守：命中才短路，模糊输入照常走完整轮次。
pub fn is_probing_message(text: &str) -> bool {
    probe_reply(text).is_some()
}

/// 探活消息 → 直答文案（按语义分类，避免「晚安」回「在」的错位）。
/// None = 非探活（走完整轮次）。
pub fn probe_reply(text: &str) -> Option<&'static str> {
    let mut t = text.trim();
    // @ 前缀剥离（im-probe-at-prefix）：群聊唤醒消息必带「@机器人名/号」前缀，
    // 不剥就撑过 10 字符上限——探活短路在群聊实际失效（部署实测：「@id 摸摸」
    // 也要跑 46 步工具循环）。@ 名字/号本身非探活词，从首个非 @ 词起判定：
    // 剥掉「@名字」记号（连续非空白直到空白或下一个 @），重复直到正文开头。
    while let Some(rest) = t.strip_prefix('@') {
        let after = rest.trim_start();
        // @ 后的名字段：到空白或下一个 @ 为止
        let end = after
            .find(|c: char| c.is_whitespace() || c == '@')
            .unwrap_or(after.len());
        t = after[end..].trim_start();
    }
    if t.is_empty() {
        // 裸 @（只点名无正文）：在线确认语义
        return Some("在。有什么需要直接说。");
    }
    if t.chars().count() > 10 {
        return None;
    }
    // 标点剥离后的裸词表（问候语常带 ？/～/。/! 等语气修饰）
    let bare = t
        .trim_matches(|c: char| !c.is_alphanumeric())
        .to_lowercase();
    match bare.as_str() {
        // 在线确认
        "在吗" | "在么" | "在不在" | "在" | "吗" => Some("在。有什么需要直接说。"),
        // 问候（含时段问候——按问候回，不逐时段定制）
        "你好" | "您好" | "hello" | "hi" | "hey" | "哈喽" | "嗨" | "喂" | "早" | "早上好"
        | "早安" | "中午好" | "下午好" | "晚上好" => Some("你好，在的。有事直接说。"),
        // 晚安是收尾语义，不引导继续
        "晚安" => Some("晚安。"),
        // 致谢
        "谢谢" | "多谢" | "感谢" | "thanks" | "thank you" => Some("不客气。"),
        // 简单确认 / 收到：最短回应
        "ok" | "okay" | "好的" | "嗯" | "嗯嗯" | "哦" | "收到" => Some("好。"),
        // 链路测试
        "测试" | "test" => Some("收到，链路正常。"),
        _ => None,
    }
}

/// IM 通道表达纪律（注入系统提示；与平台消息长度限制协同，
/// 提示层约束输出冗长、通道层截断兜底）
pub const IM_DISCIPLINE: &str = "## 表达纪律（强制：IM 通道）

本会话经由即时通讯通道进行（QQ / Telegram / Discord 等），回复将作为聊天消息送达：
- 先给结论：默认 1~5 句话直接回答用户的问题或汇报结果。
- 工具过程不逐条播报：中间状态、进度、校验步骤不写入回复；仅在影响理解时一句话带过。
- 状态播报前缀（▸/⚠/⛔ 等）、多级标题、要点清单、重复确认，在聊天场景是噪音——禁止使用。
- 用户明确要求详细展开或生成长文（文档/代码/翻译全文等）时才可长回复，此时直接输出内容主体，不加多余框架。
- 不主动追问「是否需要我继续」；信息不足就给出当前结论并说明缺什么。

## 群聊署名（发送者识别）

user 消息可能携带发送者标记（元数据，不在正文内）：`im:名字(id)` 为 @ 你的群聊真人发言者，`imctx:名字(id)` 为旁听群友。正文即发言原文，**不加任何「（某人 说）」前缀**；你根据消息自带的发送者标记与上下文自行判断在跟谁说话、回复中是否需要称呼对方。多人先后发言时按时间顺序理解为不同群友的话。

## 旁听纪律（强制：群聊会话）

历史里标注「（群聊·某人 说）」的消息是**旁听**——唤醒门控拦下的未 @ 群消息，仅作为背景上下文保留：
- 只回应真实指向你的输入（@ 你 / 私聊 / 控制台）；旁听内容不是对你的指令或问题。
- 不逐条回应、转述、总结旁听消息；不在回复里罗列「刚才群里聊了什么」。
- 仅当本轮真实输入明确引用了旁听中的话题（如「接着刚才的话题」），才把相关旁听作为上下文参考。
- **沉默约定**：本轮没有真实指向你的输入、或从真实输入中提炼不出可回应的内容时，不要输出任何「不参与/不打扰/需要再说」之类的参与声明——那也是刷屏噪音。此时只输出 `<沉默/>`（系统识别后丢弃，不投递到聊天）；宁可沉默，不可硬找话说。";

/// 自主记忆纪律（实验性，experiments.llmDrivenMemory 开启时注入）：
/// 程序不再每轮自动写入记忆，改由模型判断哪些内容值得长期保存（经 memory_write）。
pub const LLM_MEMORY_DISCIPLINE: &str = "## 记忆纪律（自主记忆模式）

本环境已关闭程序自动记忆：不会每轮自动保存会话摘要。是否记忆、记什么，由你判断并主动调用 memory_write 工具完成。

值得记忆（调用 memory_write）：
- 用户的稳定偏好与身份事实（称呼、语言、工作习惯、长期目标）
- 跨会话有用的技术结论（踩过的坑、验证过的方案、环境事实）
- 明确的决策及其理由（用户拍板的取舍、禁区、命名约定）
- 失败教训（下次遇到同类任务应避开的做法）

不值得记忆（不调用）：
- 单次任务的中间过程、状态播报、临时上下文
- 对本轮回复本身的复述
- 尚未确认的猜测与假设

判定原则：只在「下次新会话遇到类似情况时会用上」时才写；宁可少记，不可灌水。";

/// 组模式的规划契约：主智能体提示词 + 该契约 = 指挥体能力（全部组统一注入）
pub const PLANNING_CONTRACT: &str = "

---

## 指挥体规划契约（系统追加，必须遵守）

你是本组的主智能体（指挥体）：收拢用户输入 → 锁定任务边界 → 分解为可并行的子任务图 → 派发组内子个体 → 汇总裁决收束。

## 输出契约（OrchestratorPlan，系统强制校验）

只输出一个 JSON 代码块（```json ... ```），结构：
{
  \"routeLevel\": \"L0|L1|L2|L3\",
  \"goal\": \"<用户目标>\",
  \"boundary\": { \"inScope\": [\"…\"], \"forbidden\": [\"…\"] },
  \"acceptance\": [\"可验证的验收口径\"],
  \"nodes\": [{
    \"id\": \"T1\", \"title\": \"…\", \"agentIdentifier\": \"<组内子个体 identifier>\",
    \"objective\": \"<该节点要完成什么>\", \"acceptance\": [\"…\"],
    \"dependsOn\": [], \"priority\": \"P0|P1|P2|P3\"
  }],
  \"finalAnswer\": [{ \"tag\": \"报告|肯定|…\", \"text\": \"…\" }]
}
- L0 = 本机直接回答（此时 nodes 为空、finalAnswer 必填）；L1 单链；L2 并行；L3 含裁决。
- agentIdentifier 只能取自下方可调度子个体清单。
- 节点 objective 必须自包含（子个体看不到会话历史）；并行节点的写入路径不得重叠，实施与校验不得合并到同一节点；acceptance 必须可判定。
- 创建/修改组内个体必须由用户明确要求；用户未要求时禁止规划任何个体管理类节点。
- 输入无任务语义（闲聊 / 单字重复 / 无法理解的碎片）时必须 L0：要么请求澄清，要么简短回应，**禁止从会话历史捡旧任务执行**——用户没在本轮下达的任务不是任务。";

pub struct Orchestrator {
    pub registry: Arc<LocalRegistry>,
    pub orch_provider: Arc<dyn LlmProvider>,
    /// 模型档案运行池：按个体/组默认模型解析生效 (Provider, 模型名)
    pub model_pool: Arc<ModelPool>,
    /// 失败冷却状态（档案失败后一段时间内被回退链跳过）
    pub failover: Arc<FailoverState>,
    /// 远程执行器（工作者节点池；None = 全本地执行）
    pub remote: Option<Arc<dyn crate::remote::RemoteExecutor>>,
    /// 远程派发开关（工作者在线即路由远程，失败回落本地）
    pub remote_enabled: bool,
    pub unit_runtime: Arc<AgentRuntime>,
    /// 工具网关（run 收束钩子、工具能力探测）
    pub tools: Arc<crate::tools::ToolGateway>,
    pub store: Arc<Store>,
    pub memory: Arc<MemoryStore>,
    /// 共享知识库（knowledge-base）：工具入口在 ToolGateway；此处仅供
    /// 系统提示注入（inject_prompt_section）——两条路径都需要，避免再经 tools 转借
    pub knowledge: Arc<crate::knowledge::KnowledgeStore>,
    pub memory_enabled: bool,
    pub memory_recall_limit: usize,
    pub memory_md_path: PathBuf,
    /// 知识库注入配置（knowledge-base）：top_k = 0 不注入（默认零行为）
    pub knowledge_inject_top_k: usize,
    pub knowledge_inject_max_chars: usize,
    pub events: broadcast::Sender<CoreEvent>,
    pub orch_model: String,
    /// 语义检索目标："档案ID" 或 "档案ID/模型名"；空 = 仅词项召回（由记忆设置指定）
    pub memory_semantic_model: String,
    /// 能力模型槽位（"档案ID" 或 "档案ID/模型名"）：语音合成 / 语音转述 / 视觉转述
    pub speech_target: String,
    pub stt_target: String,
    pub vision_relay_target: String,
    /// 图像生成槽位（"档案ID" 或 "档案ID/模型名"；空 = 未装配）
    pub image_gen_target: String,
    pub max_concurrency: usize,
    /// 子个体单次派发的最大工具步数（配置分档：L0 直答 3 / 一般 8 / 编码类 20）
    pub max_unit_steps: usize,
    /// 任务队列出队防递归标记（task-queue）：出队任务收束时不再触发钩子
    pub(crate) queue_draining: parking_lot::Mutex<std::collections::HashSet<String>>,
    /// 智能连结等路径读取的全量配置快照（apply/构建时写入）
    pub config_snapshot: Arc<parking_lot::RwLock<crate::config::ExmConfig>>,
    /// 新教训达阈值时自动提炼经验改进要点（docs/10 §5）
    pub auto_adapt: bool,
}

impl Orchestrator {
    /// 配置快照（智能连结等需要读全量配置的路径用）
    pub fn config_snapshot(&self) -> crate::config::ExmConfig {
        self.config_snapshot.read().clone()
    }

    /// 能力槽位目标（组覆盖优先）：激活组的组级覆盖 → 全局槽位（模型设置页）。
    /// 返回 None = 该槽位全局与组都未配置（调用方回落出厂默认）。
    fn capability_target(&self, slot: &str) -> Option<String> {
        let from_group = self
            .registry
            .active_group_meta()
            .and_then(|m| m.capabilities)
            .and_then(|c| match slot {
                "speech" => c.speech,
                "transcribe" => c.transcribe,
                "vision_relay" => c.vision_relay,
                "embedding" => c.embedding,
                "image_gen" => c.image_gen,
                _ => None,
            });
        let target = from_group.or_else(|| match slot {
            "speech" => Some(self.speech_target.clone()),
            "transcribe" => Some(self.stt_target.clone()),
            "vision_relay" => Some(self.vision_relay_target.clone()),
            "embedding" => Some(self.memory_semantic_model.clone()),
            "image_gen" => Some(self.image_gen_target.clone()),
            _ => None,
        });
        target
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    /// 指挥体身份 = 激活组主智能体 identifier（组感知；内置组即 orchestrator）。
    /// 语音转写：组覆盖 → 全局槽位指定的档案/模型；未配置回落全局生效档案的 whisper-1
    pub async fn transcribe_audio(&self, audio: &[u8], filename: &str) -> anyhow::Result<String> {
        match self
            .capability_target("transcribe")
            .and_then(|t| self.resolve_capability(&t))
        {
            Some((provider, model)) => provider.transcribe(&model, audio, filename).await,
            None => {
                self.orch_provider
                    .transcribe("whisper-1", audio, filename)
                    .await
            }
        }
    }

    /// 语音合成：组覆盖 → 全局槽位指定的档案/模型；未配置回落全局生效档案的 tts-1
    pub async fn speak_audio(&self, text: &str) -> anyhow::Result<Vec<u8>> {
        match self
            .capability_target("speech")
            .and_then(|t| self.resolve_capability(&t))
        {
            Some((provider, model)) => provider.speak(&model, text).await,
            None => self.orch_provider.speak("tts-1", text).await,
        }
    }

    /// 图像生成（image-gen）：能力槽位链（组覆盖 → 全局）。
    /// 与 speak 的「tts-1 兜底」不同：无兜底模型——未配置即 Err（工具面
    /// 按装配状态门控下发，能进到这里说明槽位配置过但解析失败，明确报错）
    pub async fn image_gen_bytes(&self, prompt: &str, size: &str) -> anyhow::Result<Vec<u8>> {
        let target = self
            .capability_target("image_gen")
            .ok_or_else(|| anyhow::anyhow!("图像生成槽位未配置（capabilities.imageGen）"))?;
        let (provider, model) = self
            .resolve_capability(&target)
            .ok_or_else(|| anyhow::anyhow!("图像生成槽位无法解析：{target}"))?;
        provider.image_generate(&model, prompt, size).await
    }

    /// 图像生成是否装配（image_gen 工具下发门控：未配置不承诺）
    pub fn image_gen_ready(&self) -> bool {
        self.capability_target("image_gen")
            .and_then(|t| self.resolve_capability(&t))
            .is_some()
    }

    /// 视觉转述：生效模型未标记支持视觉（清单里明确 vision=false）且配置了转述模型时，
    /// 把图片逐张交给转述模型转成文字描述。返回 None = 无需/无法转述（保持直通或忽略）。
    async fn relay_images(&self, images: &[String]) -> Option<String> {
        if images.is_empty() {
            return None;
        }
        // 目标模型视觉能力：链首候选（真正会接这批图片的模型）
        let vision = self
            .orch_candidates()
            .first()
            .and_then(|(pid, _, model)| self.model_pool.capability(pid, model, true));
        match vision {
            Some(true) | None => return None, // 支持视觉 / 能力未知（保持直通）
            Some(false) => {}
        }
        let (provider, model) = self
            .capability_target("vision_relay")
            .and_then(|t| self.resolve_capability(&t))?;
        let mut out = String::new();
        for (i, img) in images.iter().enumerate() {
            let mut msg = ChatMessage::user(
                "请用中文详细描述这张图片的内容：主体与场景、可见文字、数据与关键细节。只输出描述正文。",
            );
            msg.images = vec![img.clone()];
            let req = ChatRequest::new(model.clone(), vec![msg]);
            match provider.chat(req).await {
                Ok(resp) if !resp.content.trim().is_empty() => {
                    out.push_str(&format!("【图片{}】{}\n", i + 1, resp.content.trim()));
                }
                Ok(_) => {}
                Err(e) => eprintln!("[vision-relay] 图片转述失败：{e}"),
            }
        }
        if out.is_empty() {
            None
        } else {
            Some(out)
        }
    }

    /// memory.md 超限自主压缩：LLM 简略化，旧全文归档（深层开 = 存数据库；关 = 存工作区归档文件）
    /// 返回 (压缩前字数, 压缩后字数)；未超限返回 Ok((len, len))
    pub async fn compact_memory_md(&self, max_chars: usize) -> anyhow::Result<(usize, usize)> {
        let md = std::fs::read_to_string(&self.memory_md_path).unwrap_or_default();
        let len = md.chars().count();
        if len <= max_chars {
            return Ok((len, len));
        }
        let system = "你是记忆压缩器【记忆压缩】。把 memory.md 压简为要点清单：保留目标、偏好、事实与教训的关键信息，删除重复与冗余；只输出压缩后的 Markdown 正文，不要任何前后缀。";
        let user = format!(
            "以下 memory.md 超出上限（{len} 字符 > {max_chars}），请压缩到约 {} 字符以内：\n\n{md}",
            max_chars * 4 / 5
        );
        let req = ChatRequest::new(
            self.orch_model.clone(),
            vec![ChatMessage::system(system), ChatMessage::user(user)],
        );
        let resp = self.orch_provider.chat(req).await?;
        let compressed = resp.content.trim().to_string();
        anyhow::ensure!(!compressed.is_empty(), "压缩结果为空");

        // 归档：深层开 → 存数据库（digest 条目，全局可见）；关 → 追加工作区归档文件
        if self.memory_enabled {
            let draft = MemoryDraft::new(
                MemoryKind::Digest,
                format!("memory.md 归档 {}", now_iso()),
                md.clone(),
            )
            .importance(0.4);
            let _ = self.memory.remember(&draft);
        } else {
            let archive = self.memory_md_path.with_file_name("memory_archive.md");
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&archive)
            {
                use std::io::Write as _;
                let _ = writeln!(f, "\n## 归档 {}\n\n{md}\n", now_iso());
            }
        }
        std::fs::write(&self.memory_md_path, &compressed)?;
        Ok((len, compressed.chars().count()))
    }

    /// 会话的有效记忆命名空间 + 分区标记（None = 同组共享）。
    /// 外部通道「记忆隔离」把分区键写进会话，规划/派发/工具面统一走这里。
    fn memory_scope_for(&self, session_id: &str) -> (String, Option<String>) {
        let partition = self
            .store
            .get_session(session_id)
            .ok()
            .flatten()
            .and_then(|s| s.memory_partition);
        let group = partition
            .clone()
            .unwrap_or_else(|| self.registry.active_group());
        (group, partition)
    }

    /// 会话有效记忆命名空间（规划召回 / 派发召回 / 自动写入共用）。
    fn memory_group_for_session(&self, session_id: &str) -> String {
        self.memory_scope_for(session_id).0
    }

    /// 分区记忆的 memory.md 路径：共享档沿用全局路径；隔离档落到独立文件，
    /// 避免把整个记忆库渲染进隔离平台的文件记忆视图（泄漏）。
    fn memory_md_path_for(&self, session_id: &str) -> std::path::PathBuf {
        let (_, partition) = self.memory_scope_for(session_id);
        match partition {
            None => self.memory_md_path.clone(),
            Some(p) => {
                let safe: String = p
                    .chars()
                    .map(|c| {
                        if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                            c
                        } else {
                            '_'
                        }
                    })
                    .collect();
                self.memory_md_path
                    .with_file_name(format!("memory-{safe}.md"))
            }
        }
    }

    /// 工作者端执行入口：远程派发在本地执行（供 WorkerSession 调用）
    pub async fn execute_unit<F>(
        &self,
        def: &AgentDefinition,
        order: &DispatchOrder,
        session_id: &str,
        attach_images: bool,
        on_delta: F,
    ) -> anyhow::Result<SyncReport>
    where
        F: FnMut(StreamDelta),
    {
        let chain = self.unit_chain_for(def);
        // 工作者端本地执行：过程归集键与用量同键（无子会话概念）。
        // 能力边界已在上游裁剪进 order.tool_allowlist，工作者端按完整档执行即可。
        self.unit_runtime
            .execute(
                def,
                order,
                chain,
                session_id,
                session_id,
                attach_images,
                ToolScope::Computer,
                on_delta,
            )
            .await
    }

    pub fn orch_id(&self) -> String {
        self.registry
            .primary()
            .map(|p| p.identifier)
            .unwrap_or_else(|| ORCHESTRATOR_ID.to_string())
    }

    // ---------------------------------------------------------- 模型解析

    /// 当前交互目标的模型提示：智能体模式 = 该智能体 model_hint；组模式 = 组默认模型
    fn target_model_hint(&self) -> Option<String> {
        if let Some(id) = self.registry.active_single() {
            return self.registry.single(&id).and_then(|d| d.model_hint);
        }
        self.registry.active_group_meta().and_then(|m| m.model)
    }

    /// 指挥体生效模型：目标提示 → 全局生效档案
    fn resolve_orch_model(&self) -> (Arc<dyn LlmProvider>, String) {
        self.orch_candidates()
            .into_iter()
            .next()
            .map(|(_, p, m)| (p, m))
            .unwrap_or_else(|| (self.orch_provider.clone(), self.orch_model.clone()))
    }

    /// 回退链展开：起始档案 → fallback 链 → 全局生效档案兜底；冷却中的档案跳过（保底留一个）
    fn expand_chain(&self, start: &str) -> Vec<(String, Arc<dyn LlmProvider>, String)> {
        let mut ids = self.model_pool.chain(start);
        let active = self.model_pool.active_id();
        if !active.is_empty() && !ids.contains(&active) {
            ids.push(active);
        }
        let total = ids.len();
        let mut out: Vec<(String, Arc<dyn LlmProvider>, String)> = Vec::new();
        for (n, id) in ids.into_iter().enumerate() {
            let Some((provider, model)) = self.model_pool.entry(&id) else {
                continue;
            };
            if !out.is_empty() && n + 1 < total && self.failover.cooling(&id) {
                continue; // 冷却中且后面还有候选：跳过
            }
            out.push((id, provider, model));
        }
        out
    }

    /// 写入路径向量回填：本轮新条目批量嵌入（标题+正文），失败静默（词项召回仍可用）
    async fn backfill_embeddings(&self, ids: &[String]) {
        let Some(cfg) = self.semantic_recall() else {
            return;
        };
        let (provider, model) = cfg;
        let mut pending: Vec<(String, String)> = Vec::new();
        for id in ids {
            if let Ok(Some(e)) = self.memory.get_entry_public(id) {
                if e.embedding.is_none() {
                    pending.push((id.clone(), format!("{}\n{}", e.title, e.body)));
                }
            }
        }
        if pending.is_empty() {
            return;
        }
        let texts: Vec<String> = pending.iter().map(|(_, t)| t.clone()).collect();
        match provider.embed(&model, &texts).await {
            Ok(vectors) if vectors.len() == pending.len() => {
                for ((id, _), v) in pending.iter().zip(vectors) {
                    let _ = self.memory.set_embedding(id, v);
                }
            }
            Ok(_) => eprintln!("[memory] 嵌入返回数量不符，跳过本轮回填"),
            Err(e) => eprintln!("[memory] 嵌入回填失败（词项召回不受影响）: {e}"),
        }
    }

    /// 查询嵌入（混合记忆检索）：语义检索指向的档案 + 模型，否则 None（纯词项）
    async fn embed_query(&self, text: &str) -> Option<Vec<f32>> {
        let (provider, model) = self.semantic_recall()?;
        provider
            .embed(&model, &[text.to_string()])
            .await
            .ok()?
            .into_iter()
            .next()
    }

    /// 语义查询向量（记忆检索调试视图 8.4）：REST 检索复用——语义检索启用时
    /// 记忆页搜索与对话内召回同口径（词项 + 语义混合），未启用返回 None
    pub async fn memory_query_embedding(&self, text: &str) -> Option<Vec<f32>> {
        self.embed_query(text).await
    }

    /// 解析语义检索目标（组覆盖 → 全局记忆设置指定的档案与模型）；未配置返回 None
    fn semantic_recall(&self) -> Option<(Arc<dyn LlmProvider>, String)> {
        let target = self.capability_target("embedding")?;
        self.resolve_capability(&target)
            .filter(|(_, m)| !m.is_empty())
    }

    /// 解析能力槽位目标（"档案ID" 或 "档案ID/模型名"）→ (Provider, 生效模型名)。
    /// 目标为空或档案不存在返回 None（调用方回落默认行为）。
    fn resolve_capability(&self, target: &str) -> Option<(Arc<dyn LlmProvider>, String)> {
        let id = target.trim().to_string();
        if id.is_empty() {
            return None;
        }
        let (pid, model) = match id.split_once('/') {
            Some((p, m)) if !m.trim().is_empty() => (p.trim().to_string(), m.trim().to_string()),
            _ => (id, String::new()),
        };
        let (provider, profile_model) = self.model_pool.entry(&pid)?;
        let model = if model.is_empty() {
            profile_model
        } else {
            model
        };
        Some((provider, model))
    }

    /// 指挥体候选链（目标提示 → 全局 → 回退链）
    fn orch_candidates(&self) -> Vec<(String, Arc<dyn LlmProvider>, String)> {
        let start = self
            .target_model_hint()
            .and_then(|h| self.model_pool.resolve_full(&h).map(|(id, _, _)| id))
            .unwrap_or_else(|| self.model_pool.active_id());
        if start.is_empty() {
            return Vec::new();
        }
        self.expand_chain(&start)
    }

    /// 子个体候选链（个体 → 组 → 全局 → 回退链）
    fn unit_candidates(
        &self,
        def: &AgentDefinition,
    ) -> Vec<(String, Arc<dyn LlmProvider>, String)> {
        let start = def
            .model_hint
            .clone()
            .or_else(|| self.registry.active_group_meta().and_then(|m| m.model))
            .and_then(|h| self.model_pool.resolve_full(&h).map(|(id, _, _)| id))
            .unwrap_or_else(|| self.model_pool.active_id());
        if start.is_empty() {
            return Vec::new();
        }
        self.expand_chain(&start)
    }

    /// 子个体回退链（run_node → AgentRuntime）
    fn unit_chain_for(&self, def: &AgentDefinition) -> ModelChain {
        ModelChain {
            candidates: self.unit_candidates(def),
            failover: self.failover.clone(),
        }
    }

    // ---------------------------------------------------------- 事件

    pub fn emit(&self, session_id: &str, kind: &str, payload: serde_json::Value) {
        let evt = CoreEvent {
            kind: kind.to_string(),
            session_id: session_id.to_string(),
            payload,
        };
        let _ = self.events.send(evt.clone());
        // 流式增量（token/thinking）只走实时广播、不落事件库：
        // 一次运行可达数千条，落库会把事件溯源刷成 token 流水账（事件页被逐字淹没）。
        if Self::TRANSIENT_EVENT_KINDS.contains(&kind) {
            return;
        }
        // 事件溯源（可回放，docs/04 §8）
        let env = Envelope::new(
            format!("session.{session_id}.events"),
            self.orch_id(),
            "broadcast",
            "event",
            session_id,
            serde_json::to_value(&evt).unwrap_or(serde_json::Value::Null),
        );
        let _ = self.store.append_event(&env);
    }

    /// 只广播不落库的事件名（流式增量）。
    const TRANSIENT_EVENT_KINDS: [&'static str; 4] = [
        "orchestrator.token",
        "orchestrator.thinking",
        "unit.token",
        "unit.thinking",
    ];

    // ---------------------------------------------------------- 主入口

    /// collect：上一条非 user（orchestrator/unit/system）消息之后的所有 user 输入合并
    fn collect_pending_inputs(&self, session_id: &str) -> Option<String> {
        // list_messages 为时间正序（read_lines 语义）：从尾部反向收集连续 user 消息
        let msgs: Vec<crate::types::ChatMessage> =
            self.store.list_messages(session_id, 200).ok()?;
        pending_inputs_from(&msgs)
    }

    /// 上下文压缩（docs/架构与设计.md）：历史超过阈值时，旧消息压成滚动摘要，保留最近窗口原文
    /// 返回 (滚动摘要, 最近窗口消息原文文本)——共享实现见 compaction.rs（单智能体循环同口径复用）
    async fn ensure_compacted(&self, session_id: &str) -> (Option<String>, String) {
        let (provider, model) = self.resolve_orch_model();
        let cfg = self.config_snapshot.read().clone();
        let sliding = cfg.experiments.sliding_context_window;
        // token 占比判据（compaction-tuning）：规划路径以指挥体生效模型窗口为基数
        let budget_ratio = self.tools.context_budget_ratio();
        let budget_chars = self
            .orch_candidates()
            .first()
            .and_then(|(pid, _, model)| self.model_pool.context_window(pid, model))
            .map(|w| (w as f64 * budget_ratio).floor() as usize * crate::budget::CHARS_PER_TOKEN);
        let params = crate::compaction::CompactParams {
            sliding,
            trigger_ratio: cfg.compaction.trigger_ratio,
            budget_chars,
            ..Default::default()
        };
        let c =
            crate::compaction::ensure_compacted(&self.store, &provider, &model, session_id, params)
                .await;
        (c.summary, c.recent)
    }

    pub async fn handle_user_message(
        self: &Arc<Self>,
        session_id: &str,
        text: &str,
    ) -> anyhow::Result<()> {
        self.handle_user_message_scoped(session_id, text, ToolScope::Computer, None)
            .await
    }

    /// 带能力边界处理一轮（外部通道入口）：scope 贯穿规划/派发/单聊执行。
    /// `origin`：消息来源标记，落入消息 agent_id——"auto:xxx" 系统自动内容、
    /// "im:名字(id)" 群聊发送者（展示层消息头署名）、None = 真人输入。
    /// 带能力边界处理一轮（外部通道入口）：scope 贯穿规划/派发/单聊执行。
    /// `origin`：消息来源标记，落入消息 agent_id——"auto:xxx" 系统自动内容、
    /// "im:名字(id)" 群聊发送者（展示层消息头署名）、None = 真人输入。
    ///
    /// 五阶段编排（codebase-refactor D3 提取，行为零变更）：
    /// ingest（落库+附件回填）→ probe（IM 探活短路）→ merge（goal 合并/排队）
    /// → dispatch（run_round 路由派发）→ finalize（钩子/事件/快照/队列收束）。
    pub async fn handle_user_message_scoped(
        self: &Arc<Self>,
        session_id: &str,
        text: &str,
        scope: ToolScope,
        origin: Option<&str>,
    ) -> anyhow::Result<()> {
        let session = self.store.get_session(session_id)?;
        if session.is_none() {
            anyhow::bail!("会话不存在: {session_id}");
        }
        self.ingest_user_message(session_id, text, origin).await?;
        if self.probe_shortcircuit(session_id, text).await? {
            return Ok(());
        }
        let goal = self.merge_or_enqueue(session_id, text)?;
        let result = self.dispatch_round(session_id, &goal, scope).await;
        self.finalize_round(session_id, result, scope).await
    }

    /// 阶段一 ingest：用户消息落库 + 入站附件按消息 id 回填。
    async fn ingest_user_message(
        self: &Arc<Self>,
        session_id: &str,
        text: &str,
        origin: Option<&str>,
    ) -> anyhow::Result<()> {
        let user_msg = self.store.add_message(
            session_id,
            MessageRole::User,
            origin,
            vec![Statement::new(SpeechTag::要求, text)],
        )?;
        // 入站附件随 user 消息落库（im-inbound-attach）：通道已把媒体元数据暂存，
        // 这里取走按消息 id 回填——WebUI 聊天流据此渲染缩略图/下载卡片。
        // 无附件即空转；普通 IM 文本链路零额外开销。
        let inbound = crate::inbox_stash::take(session_id);
        if !inbound.is_empty() {
            self.store
                .attach_inbound(session_id, &user_msg.id, inbound);
        }
        Ok(())
    }

    /// 阶段二 probe：IM 探活短路（纯问候/在线确认零 LLM 直答——这类输入无任务
    /// 语义，进规划流水线纯属浪费）。命中 → 直答落库 + run.finished + 快照，
    /// 返回 true（调用方早收束）。短路在 collect 之前：探活不并入排队窗口的
    /// goal 合并。
    async fn probe_shortcircuit(
        self: &Arc<Self>,
        session_id: &str,
        text: &str,
    ) -> anyhow::Result<bool> {
        if !is_im_session(session_id) {
            return Ok(false);
        }
        let Some(reply) = probe_reply(text) else {
            return Ok(false);
        };
        let statements = vec![Statement::new(SpeechTag::肯定, reply)];
        let (thinking, tool_calls, steps) = crate::round_trace::drain(session_id);
        self.store.add_message_full(
            session_id,
            MessageRole::Orchestrator,
            Some(self.orch_id().as_str()),
            statements.clone(),
            thinking,
            tool_calls,
            steps,
        )?;
        self.emit(
            session_id,
            "run.finished",
            serde_json::json!({
                "routeLevel": "L0",
                "shortCircuit": "probe",
                "agentId": self.orch_id(),
                "statements": statements,
                "artifacts": []
            }),
        );
        self.record_turn_snapshot(session_id, "done");
        Ok(true)
    }

    /// 阶段三 merge：collect 语义——「上一条非 user 消息之后到达的全部用户输入」
    /// 合并为本轮目标（串行锁保证的排队窗口内的多条消息 = 一轮处理，避免逐条
    /// 各跑一遍），写入台账并广播 ledger.updated。台账写失败随调用方 `?`
    /// 失败整轮（与提取前一致，行为零变更）。
    fn merge_or_enqueue(
        &self,
        session_id: &str,
        text: &str,
    ) -> anyhow::Result<String> {
        let goal = self
            .collect_pending_inputs(session_id)
            .unwrap_or_else(|| text.to_string());
        let ledger = self.store.mutate_ledger(session_id, |l| {
            l.task.goal = goal.clone();
        })?;
        self.emit(session_id, "ledger.updated", serde_json::to_value(&ledger)?);
        Ok(goal)
    }

    /// 阶段四 dispatch：本轮执行（单智能体直连 / 规划派发路由，见 run_round）。
    async fn dispatch_round(
        self: &Arc<Self>,
        session_id: &str,
        goal: &str,
        scope: ToolScope,
    ) -> anyhow::Result<()> {
        self.run_round(session_id, goal, scope).await
    }

    /// 阶段五 finalize：onRunEnd 钩子 / run.error 事件与落库 / 轮次快照 /
    /// 任务队列收束（draining 标记防递归：出队任务自己也是一轮
    /// handle_user_message_scoped，它收束时不再触发钩子，否则队列串行链
    /// 无限嵌套）。
    async fn finalize_round(
        self: &Arc<Self>,
        session_id: &str,
        result: anyhow::Result<()>,
        scope: ToolScope,
    ) -> anyhow::Result<()> {
        // onRunEnd 钩子：一轮收束（成功或失败）时触发，可做通知/留痕/指标
        let hook_status = if result.is_ok() { "done" } else { "error" };
        for note in self
            .tools
            .run_end_hooks(hook_status, session_id, &self.orch_id())
        {
            self.emit(
                session_id,
                "hook.failed",
                serde_json::json!({ "message": note }),
            );
        }
        if let Err(err) = &result {
            self.emit(
                session_id,
                "run.error",
                serde_json::json!({ "message": err.to_string() }),
            );
            let _ = self.store.add_message(
                session_id,
                MessageRole::System,
                None,
                vec![Statement::warn(format!("本轮运行失败：{err}"))],
            );
        }
        // 轮次快照（9.1）：每轮收束原子落一条（消息游标 + 检查点链 + 用量），undo / 编辑重发 / fork 基座
        self.record_turn_snapshot(session_id, if result.is_ok() { "done" } else { "failed" });
        // 任务队列收束钩子（task-queue）：本轮结束后出队下一条排队任务继续跑。
        if result.is_ok() && !self.queue_draining.lock().contains(session_id) {
            self.drain_task_queue(session_id, scope).await;
        }
        result
    }

    /// 任务队列出队执行（task-queue）：取队首 queued 任务，落库为 user 消息后
    /// 作为正常轮次执行（预算/锁/快照/事件全复用既有链路）。
    /// 在调用方（handle_user_message_scoped 收束钩子）已持轮次锁的异步上下文里
    /// 直接 await——串行语义天然成立；并行任务不经此处（spawn 已在跑）。
    async fn drain_task_queue(self: &Arc<Self>, session_id: &str, scope: ToolScope) {
        // 防递归标记（DrainingGuard RAII： panic 也摘除）
        struct DrainingGuard<'a>(&'a parking_lot::Mutex<std::collections::HashSet<String>>, String);
        impl Drop for DrainingGuard<'_> {
            fn drop(&mut self) {
                self.0.lock().remove(&self.1);
            }
        }
        self.queue_draining.lock().insert(session_id.to_string());
        let _guard = DrainingGuard(&self.queue_draining, session_id.to_string());

        // 用户停止过本轮 → 队列任务不再自动执行（停止语义覆盖队列）
        if crate::round_trace::is_cancelled(session_id) {
            return;
        }
        let Some(task) = crate::task_queue::dequeue_next(session_id) else {
            return;
        };
        let note = format!("任务队列出队自动执行（{}）", task.id);
        let items = crate::task_queue::list(session_id);
        let queued = items.iter().filter(|t| t.status == "queued").count();
        self.emit(
            session_id,
            "task_queue.updated",
            serde_json::json!({
                "items": items,
                "running": 1,
                "queued": queued,
                "dequeued": task.id,
            }),
        );
        // 落库为 user 消息（queue:auto 标记进 agent_id，展示层区分自动出队与真人输入）
        let msg = self.store.add_message(
            session_id,
            MessageRole::User,
            Some("queue:auto"),
            vec![Statement::new(SpeechTag::要求, format!("{}\n{}", note, task.task))],
        );
        if let Ok(m) = &msg {
            // 出队任务也可能带附件元数据（罕见：入队轮附件残留）——一致性回填
            let inbound = crate::inbox_stash::take(session_id);
            if !inbound.is_empty() {
                self.store.attach_inbound(session_id, &m.id, inbound);
            }
        }
        let round = self.run_round(session_id, &task.task, scope).await;
        let (status, note) = match &round {
            Ok(()) => ("done", None),
            Err(e) => ("failed", Some(e.to_string())),
        };
        crate::task_queue::set_status(session_id, &task.id, status, note);
        // 单会话一次钩子只出队一条：剩余任务等下一次真实输入的收束钩子继续
        // （避免一次收束连环跑整条队列把会话独占太久——用户随时可插话）
    }

    /// 停止轮次的统一收束：停止陈述作为最终消息（含过程数据），审计事件落账
    fn finish_stopped_round(&self, session_id: &str, tools_executed: usize) -> anyhow::Result<()> {
        let text = format!(
            "【警告】本轮已被用户停止（已执行 {} 次工具调用）。",
            tools_executed
        );
        let statements = vec![Statement::warn(text)];
        let (thinking, tool_calls, steps) = crate::round_trace::drain(session_id);
        self.store.add_message_full(
            session_id,
            MessageRole::System,
            None,
            statements,
            thinking,
            tool_calls,
            steps,
        )?;
        self.store.audit_tool_best_effort(
            "system",
            "session_stop",
            &serde_json::json!({ "session": session_id }),
            &format!("用户停止本轮（已执行 {} 次工具调用）", tools_executed),
            0,
        );
        self.emit(
            session_id,
            "run.finished",
            serde_json::json!({
                "routeLevel": "STOPPED",
                "agentId": self.orch_id(),
                "statements": [ { "tag": "警告", "text": "本轮已被用户停止。" } ],
                "stopped": true,
                "artifacts": self.store.outbox_drain_with_children(session_id).unwrap_or_default()
            }),
        );
        Ok(())
    }

    /// 轮次快照记录：turn = 既有快照数 + 1；游标 = 当前消息条数；
    /// 检查点链 = 本轮写类工具登记的检查点引用（读出即清）；用量 = 本轮增量（台账差分）
    fn record_turn_snapshot(&self, session_id: &str, status: &str) {
        let turn = match self.store.latest_turn_snapshot(session_id) {
            Ok(Some(last)) => last.turn + 1,
            _ => 1,
        };
        let count = self
            .store
            .list_messages(session_id, 0)
            .map(|m| m.len())
            .unwrap_or(0);
        let checkpoint_ids = self
            .store
            .drain_checkpoint_refs(session_id)
            .unwrap_or_default();
        let (p, c, _) = self.store.usage_total(session_id);
        let (prev_p, prev_c) = match turn {
            1 => (0, 0),
            _ => match self
                .store
                .list_turn_snapshots(session_id)
                .ok()
                .and_then(|l| l.last().cloned())
            {
                Some(last) => (last.prompt_tokens, last.completion_tokens),
                None => (0, 0),
            },
        };
        let snap = TurnSnapshot {
            id: format!("{session_id}:{turn}"),
            session_id: session_id.to_string(),
            turn,
            message_count: count,
            checkpoint_ids,
            prompt_tokens: p.saturating_sub(prev_p),
            completion_tokens: c.saturating_sub(prev_c),
            status: status.to_string(),
            created_at: now_iso(),
        };
        let _ = self.store.put_turn_snapshot(&snap);
        self.emit(
            session_id,
            "turn.snapshotted",
            serde_json::json!({ "turn": turn, "messageCount": count, "status": status }),
        );
    }

    /// 智能体模式直接执行轮(常见单 agent 应用同款):系统提示 + 会话历史 + 工具循环,
    /// 自主决策直至产出最终回复。无规划 JSON、无派发语义;过程分轨与留痕对齐既有体验。
    async fn single_direct_round(
        self: &Arc<Self>,
        session_id: &str,
        text: &str,
        scope: ToolScope,
    ) -> anyhow::Result<()> {
        let def = self
            .registry
            .primary()
            .ok_or_else(|| anyhow::anyhow!("智能体模式未配置交互目标"))?;
        // 轮次模式消费:direct/full 在智能体下无差异(工具循环自主决策),取走避免跨轮残留
        let _forced = take_session_mode(session_id);

        let mut system_prompt = self
            .registry
            .load_prompt(&def.prompt_file)
            .unwrap_or_else(|_| format!("# {}\n\n你是独立服务于操作者的智能体。", def.name));
        let soul = self
            .registry
            .active_single()
            .and_then(|id| self.registry.single_persona(&id).ok())
            .unwrap_or_else(|| LocalRegistry::DEFAULT_PERSONA.to_string());
        system_prompt.push_str(&format!("\n\n## SOUL（人格）\n{soul}"));
        // IM 通道会话：注入简洁表达纪律（一次性大量内容的提示层治理）
        if is_im_session(session_id) {
            system_prompt.push_str("\n\n");
            system_prompt.push_str(IM_DISCIPLINE);
        }
        // 自主记忆模式（实验性）：注入记忆纪律，引导模型经 memory_write 自主决定
        if self.config_snapshot.read().experiments.llm_driven_memory {
            system_prompt.push_str("\n\n");
            system_prompt.push_str(LLM_MEMORY_DISCIPLINE);
        }
        // Super Agent（实验性）：会话深度委托——静默长链路推进，只在停机条件产出消息
        if self.config_snapshot.read().experiments.super_agent {
            system_prompt.push_str("\n\n");
            system_prompt.push_str(crate::autonomy::SUPER_AGENT_DISCIPLINE);
        }

        // 会话历史窗口:最近 40 条按角色映射(User→user, Orchestrator→assistant);
        // 本轮输入已由 handle_user_message 落库,天然位于窗口末尾。
        // 窗口外更早历史接入共享滚动压缩(compaction.rs,与组调度同口径):
        // 压缩摘要注入系统提示,窗口内保持原文
        let rows = self.store.list_messages(session_id, 40)?;
        let cfg_now = self.config_snapshot.read().clone();
        let sliding = cfg_now.experiments.sliding_context_window;
        // token 占比判据基数（compaction-tuning）：模型窗口 × contextBudgetRatio（字符当量）
        let budget_ratio_for_compact = self.tools.context_budget_ratio();
        let window_for_compact = self
            .unit_chain_for(&def)
            .candidates
            .first()
            .and_then(|(pid, _, model)| self.model_pool.context_window(pid, model));
        let budget_chars_for_compact = window_for_compact.map(|w| {
            (w as f64 * budget_ratio_for_compact).floor() as usize
                * crate::budget::CHARS_PER_TOKEN
        });
        let compacted = crate::compaction::ensure_compacted(
            &self.store,
            &self.orch_provider,
            &self.orch_model,
            session_id,
            crate::compaction::CompactParams {
                trigger: 40,
                keep: 20,
                trigger_ratio: cfg_now.compaction.trigger_ratio,
                budget_chars: budget_chars_for_compact,
                sliding,
            },
        )
        .await;

        // ---- token 预算软着陆（budget.rs，D7）----
        // 估算口径 ~4 字符/token；仅当生效模型的 contextWindow 已登记时判定。
        // 超线时多级裁剪（滚动摘要收窄 → 旧消息截短），每次裁剪发 context.trim 事件留痕。
        let budget_ratio = self.tools.context_budget_ratio();
        let window_tokens = self
            .unit_chain_for(&def)
            .candidates
            .first()
            .and_then(|(pid, _, model)| self.model_pool.context_window(pid, model));
        let mut summary_opt = compacted.summary.clone();
        let mut rows = rows;
        if let Some(window) = window_tokens {
            let budget_chars =
                (window as f64 * budget_ratio).floor() as usize * crate::budget::CHARS_PER_TOKEN;
            let system_chars = system_prompt.chars().count();
            let summary_chars = summary_opt
                .as_deref()
                .map(|s| s.chars().count())
                .unwrap_or(0);
            let history_chars: usize = rows
                .iter()
                .map(|m| {
                    m.statements
                        .iter()
                        .map(|s| s.text.chars().count())
                        .sum::<usize>()
                })
                .sum();
            let total = system_chars + summary_chars + history_chars;
            if total > budget_chars {
                let over = total - budget_chars;
                let outcome =
                    crate::budget::trim_to_budget(over, None, None, summary_opt.as_deref());
                summary_opt = outcome.summary;
                // 消息截断落地：从最旧一侧逐条截短（保最新上下文完整性）
                let shorten_msgs = outcome.truncated_msg_chars.unwrap_or(0);
                if shorten_msgs > 0 {
                    crate::budget::trim_oldest_messages(&mut rows, shorten_msgs);
                }
                self.emit(
                    session_id,
                    "context.trim",
                    serde_json::json!({
                        "estimatedChars": total,
                        "budgetChars": budget_chars,
                        "windowTokens": window,
                        "ratio": budget_ratio,
                        "trims": outcome
                            .trims
                            .iter()
                            .map(|(k, from, to)| serde_json::json!({"block": k, "fromChars": from, "toChars": to}))
                            .chain(std::iter::once(serde_json::json!({
                                "block": "messages",
                                "fromChars": history_chars,
                                "toChars": rows.iter().map(|m| m.statements.iter().map(|s| s.text.chars().count()).sum::<usize>()).sum::<usize>(),
                            })))
                            .collect::<Vec<_>>(),
                    }),
                );
            }
        }

        let system_prompt = if let Some(summary) = summary_opt.clone() {
            if summary.trim().is_empty() {
                system_prompt
            } else {
                system_prompt.push_str(&format!(
                    "\n\n## 会话上下文（更早对话滚动摘要）\n{summary}\n"
                ));
                system_prompt
            }
        } else {
            system_prompt
        };
        // 知识库注入（knowledge-base，默认关）：按本轮输入检索 top-k 共享知识，
        // 与压缩摘要同层注入系统提示尾部；检索失败静默跳过
        let system_prompt = {
            let cfg_k = self.config_snapshot.read().clone();
            let section = crate::knowledge::inject_prompt_section(
                &self.knowledge,
                text,
                cfg_k.knowledge_inject_top_k,
                cfg_k.knowledge_inject_max_chars,
            );
            let mut sp = system_prompt;
            sp.push_str(&section);
            sp
        };
        let mut history: Vec<ChatMessage> = history_with_ambient(&rows);
        if history.last().map(|m| m.role != "user").unwrap_or(true) {
            history.push(ChatMessage::user(text.to_string()));
        }

        // 多模态输入:图片三态(视觉直附 / 转述模型转写 / 忽略并注记),与规划路径一致
        let images = crate::image_stash::take(session_id);
        if !images.is_empty() {
            let last = history.last_mut();
            match self.relay_images(&images).await {
                Some(desc) => {
                    if let Some(m) = last {
                        m.content.push_str(&format!(
                            "\n\n## 图片内容（由视觉转述模型转写，原始图片未直接下发）\n{desc}"
                        ));
                    }
                }
                None => {
                    let vision = self
                        .unit_chain_for(&def)
                        .candidates
                        .first()
                        .and_then(|(pid, _, model)| self.model_pool.capability(pid, model, true));
                    if vision == Some(false) {
                        if let Some(m) = last {
                            m.content.push_str(
                                "\n\n（用户随本轮附带了图片，但当前模型不支持视觉且未配置视觉转述模型，图片已忽略。）",
                            );
                        }
                    } else if let Some(m) = last {
                        m.images = images;
                    }
                }
            }
        }

        // 过程分轨:思考入思维轨与 round_trace,正文可见流式
        let em = self.clone();
        let sid = session_id.to_string();
        let on_delta = move |delta: StreamDelta| match delta {
            StreamDelta::Thinking(t) => {
                crate::round_trace::push_thinking(&sid, &t);
                em.emit(
                    &sid,
                    "orchestrator.thinking",
                    serde_json::json!({ "delta": t }),
                );
            }
            StreamDelta::Text(t) => {
                em.emit(
                    &sid,
                    "orchestrator.token",
                    serde_json::json!({ "delta": t }),
                );
            }
        };

        let chain = self.unit_chain_for(&def);
        let final_text = self
            .unit_runtime
            .chat_execute(
                &def,
                system_prompt,
                history,
                session_id,
                session_id,
                chain,
                true,
                scope,
                on_delta,
            )
            .await?;

        let (thinking, tool_calls, steps) = crate::round_trace::drain(session_id);
        let statements = vec![Statement::report(final_text)];
        self.store.add_message_full(
            session_id,
            MessageRole::Orchestrator,
            Some(def.identifier.as_str()),
            statements.clone(),
            thinking,
            tool_calls,
            steps,
        )?;
        self.emit(
            session_id,
            "run.finished",
            serde_json::json!({
                "routeLevel": "direct",
                "agentId": def.identifier,
                "statements": statements,
                "artifacts": self.store.outbox_drain_with_children(session_id).unwrap_or_default(),
            }),
        );
        Ok(())
    }

    /// 与子个体单独对话（桌面端子代理会话）：复用个体执行回路（工具循环/人格与派发执行一致），
    /// 上下文携带最近的派发单契约；流式经 unit.token / unit.thinking 通道，回复落库 role=unit。
    /// 操作者可在子代理会话界面直接与该个体对话，无需经过指挥体编排。
    pub async fn unit_direct_round(
        self: &Arc<Self>,
        session_id: &str,
        agent_identifier: &str,
        text: &str,
    ) -> anyhow::Result<()> {
        self.unit_direct_round_origin(session_id, agent_identifier, text, None)
            .await
    }

    /// 带来源标记的单体直连轮次（origin 语义同 handle_user_message_scoped）
    pub async fn unit_direct_round_origin(
        self: &Arc<Self>,
        session_id: &str,
        agent_identifier: &str,
        text: &str,
        origin: Option<&str>,
    ) -> anyhow::Result<()> {
        // 解析个体：组内子个体优先，其次独立智能体
        let def = self
            .registry
            .agents_in_group(&self.registry.active_group())
            .into_iter()
            .find(|d| d.identifier == agent_identifier)
            .or_else(|| self.registry.single(agent_identifier))
            .ok_or_else(|| anyhow::anyhow!("未找到个体: {agent_identifier}"))?;

        // 人格：个体提示词 + SOUL + 单独对话身份说明（区别于派发执行的收束格式）
        let mut system_prompt = self
            .registry
            .load_prompt(&def.prompt_file)
            .unwrap_or_else(|_| {
                format!(
                    "# {}\n\n你是组内子个体，独立、专业地与操作者对话。",
                    def.name
                )
            });
        let soul = self
            .registry
            .single_persona(&def.identifier)
            .ok()
            .unwrap_or_else(|| LocalRegistry::DEFAULT_PERSONA.to_string());
        system_prompt.push_str(&format!("\n\n## SOUL（人格）\n{soul}"));
        system_prompt.push_str(&format!(
            "\n\n## 身份\n你是「{}」（@{}，负责域：{}）。当前是操作者与你的单独对话，不是派发执行：直接、具体地回答，不要输出任务收束格式。个体说明：{}",
            def.name, def.identifier, def.domain, def.description
        ));

        // 最近派发单：任务契约（目标/验收）作为你的工作上下文一并携带
        if let Ok(evs) = self.store.list_events(session_id) {
            let dispatch = evs.into_iter().rev().find(|e| {
                e.get("type").and_then(|v| v.as_str()) == Some("dispatch")
                    && e.get("to").and_then(|v| v.as_str()) == Some(agent_identifier)
            });
            if let Some(p) = dispatch.and_then(|e| e.get("payload").cloned()) {
                let goal = p.get("goal").and_then(|v| v.as_str()).unwrap_or_default();
                if !goal.is_empty() {
                    let mut ctx = format!("\n\n## 指挥体最近派发给你的任务\n目标：{goal}");
                    let acc: Vec<String> = p
                        .get("acceptance")
                        .and_then(|v| v.as_array())
                        .map(|a| {
                            a.iter()
                                .filter_map(|x| x.as_str().map(String::from))
                                .collect()
                        })
                        .unwrap_or_default();
                    if !acc.is_empty() {
                        ctx.push_str(&format!("\n验收：{}", acc.join("；")));
                    }
                    system_prompt.push_str(&ctx);
                }
            }
        }

        // 本轮输入落库（与 handle_user_message 同口径，要求标签；agent_id 兼作自动来源标记）
        let user_msg = self.store.add_message(
            session_id,
            MessageRole::User,
            origin,
            vec![Statement::new(SpeechTag::要求, text)],
        )?;
        // 入站附件随 user 消息落库（im-inbound-attach）：与 handle_user_message_scoped
        // 同口径——单体直连目前无 IM 通道来源，暂存通常为空，接上以防链路断档。
        let inbound = crate::inbox_stash::take(session_id);
        if !inbound.is_empty() {
            self.store
                .attach_inbound(session_id, &user_msg.id, inbound);
        }

        // 历史窗口：User→user；Orchestrator→assistant（标注来源）；本个体→assistant；其余个体跳过
        let rows = self.store.list_messages(session_id, 40)?;
        let mut history: Vec<ChatMessage> = history_with_ambient_labeled(&rows, |m, content| {
            match m.role {
                // 指挥体回复带前缀（组内多个体可区分）
                MessageRole::Orchestrator => {
                    Some(ChatMessage::assistant(format!("【指挥体】{content}")))
                }
                // 本个体自己的历史消息归 assistant；其余个体的消息跳过
                MessageRole::Unit if m.agent_id.as_deref() == Some(agent_identifier) => {
                    Some(ChatMessage::assistant(content))
                }
                _ => None,
            }
        });
        if history.last().map(|m| m.role != "user").unwrap_or(true) {
            history.push(ChatMessage::user(text.to_string()));
        }

        // 多模态：视觉转述可用则转写注入，否则原图直附（能力不可用并注记忽略）
        let images = crate::image_stash::take(session_id);
        if !images.is_empty() {
            let last = history.last_mut();
            match self.relay_images(&images).await {
                Some(desc) => {
                    if let Some(m) = last {
                        m.content.push_str(&format!(
                            "\n\n## 图片内容（由视觉转述模型转写，原始图片未直接下发）\n{desc}"
                        ));
                    }
                }
                None => {
                    if let Some(m) = last {
                        m.images = images;
                    }
                }
            }
        }

        // 过程分轨：以派发执行同通道流式（前端子代理会话视图直接复用）
        let em = self.clone();
        let aid = agent_identifier.to_string();
        let sid = session_id.to_string();
        let on_delta = move |delta: StreamDelta| match delta {
            StreamDelta::Thinking(t) => {
                crate::round_trace::push_thinking(&sid, &t);
                em.emit(
                    &sid,
                    "unit.thinking",
                    serde_json::json!({ "agentId": aid, "delta": t }),
                );
            }
            StreamDelta::Text(t) => {
                em.emit(
                    &sid,
                    "unit.token",
                    serde_json::json!({ "agentId": aid, "delta": t }),
                );
            }
        };

        let chain = self.unit_chain_for(&def);
        let final_text = self
            .unit_runtime
            .chat_execute(
                &def,
                system_prompt,
                history,
                session_id,
                session_id,
                chain,
                true,
                ToolScope::Computer,
                on_delta,
            )
            .await?;

        let (thinking, tool_calls, steps) = crate::round_trace::drain(session_id);
        let statements = vec![Statement::report(final_text)];
        self.store.add_message_full(
            session_id,
            MessageRole::Unit,
            Some(agent_identifier),
            statements.clone(),
            thinking,
            tool_calls,
            steps,
        )?;
        // 收尾事件：子代理会话视图据此结束实时态
        self.emit(
            session_id,
            "unit.finished",
            serde_json::json!({ "agentId": agent_identifier, "statements": statements }),
        );
        Ok(())
    }

    async fn run_round(
        self: &Arc<Self>,
        session_id: &str,
        text: &str,
        scope: ToolScope,
    ) -> anyhow::Result<()> {
        // 智能体模式:直接工具循环(常见单 agent 应用同款),不进规划/派发语义
        if self.registry.single_mode() {
            return self.single_direct_round(session_id, text, scope).await;
        }
        let plan = self.plan(session_id, text).await?;

        let ledger = self.store.mutate_ledger(session_id, |l| {
            l.task.acceptance = plan.acceptance.clone();
            l.task.constraints = plan.boundary.in_scope.clone();
            l.task.forbidden = plan.boundary.forbidden.clone();
        })?;
        self.emit(session_id, "ledger.updated", serde_json::to_value(&ledger)?);

        // L0 直达：不派发
        if matches!(plan.route_level, RouteLevel::L0) || plan.nodes.is_empty() {
            // 用户请求停止：以停止陈述收束本轮，不再产出内容
            if crate::round_trace::is_cancelled(session_id) {
                let n = crate::round_trace::tool_count(session_id);
                return self.finish_stopped_round(session_id, n);
            }
            let statements = plan
                .final_answer
                .clone()
                .unwrap_or_else(|| vec![Statement::report("本机直接回答（L0）。")]);
            let (thinking, tool_calls, steps) = crate::round_trace::drain(session_id);
            self.store.add_message_full(
                session_id,
                MessageRole::Orchestrator,
                Some(self.orch_id().as_str()),
                statements.clone(),
                thinking,
                tool_calls,
                steps,
            )?;
            self.emit(
                session_id,
                "run.finished",
                serde_json::json!({ "routeLevel": "L0", "agentId": self.orch_id(), "statements": statements, "artifacts": self.store.outbox_drain_with_children(session_id).unwrap_or_default() }),
            );
            return Ok(());
        }

        let mut graph = TaskGraphModel::from_plan(&plan, session_id);
        self.store.save_graph(&graph.to_graph())?;
        self.emit(
            session_id,
            "graph.updated",
            serde_json::to_value(graph.to_graph())?,
        );

        let reports: Arc<Mutex<Reports>> = Arc::new(Mutex::new(Reports::new()));
        let exec = ExecCtx {
            orchestrator: self.clone(),
            session_id: session_id.to_string(),
            plan: plan.clone(),
            reports: reports.clone(),
            scope,
        };
        let exec = Arc::new(exec);

        let scheduler = Scheduler::new(self.max_concurrency);
        let store = self.store.clone();
        let events = self.events.clone();
        let session_for_cb = session_id.to_string();
        let executor = {
            let exec = exec.clone();
            move |node: TaskNode| -> BoxFuture<'static, NodeOutcome> {
                let exec = exec.clone();
                Box::pin(async move { exec.run_node(node).await })
            }
        };

        scheduler
            .run(&mut graph, executor, move |g| {
                let _ = store.save_graph(&g.to_graph());
                let evt = CoreEvent {
                    kind: "graph.updated".into(),
                    session_id: session_for_cb.clone(),
                    payload: serde_json::to_value(g.to_graph()).unwrap_or(serde_json::Value::Null),
                };
                let _ = events.send(evt);
            })
            .await;

        let final_reports = reports.lock().await.clone();
        // 用户请求停止：跳过 LLM 收束，以停止陈述收束本轮
        let cancelled = crate::round_trace::is_cancelled(session_id);
        let final_text = if cancelled {
            format!(
                "【警告】本轮已被用户停止（已执行 {} 次工具调用）。",
                crate::round_trace::tool_count(session_id)
            )
        } else {
            self.converge(session_id, &plan, &final_reports, &graph)
                .await?
        };
        let statements = text_to_statements(&final_text);
        let (thinking, tool_calls, steps) = crate::round_trace::drain(session_id);
        self.store.add_message_full(
            session_id,
            MessageRole::Orchestrator,
            Some(self.orch_id().as_str()),
            statements.clone(),
            thinking,
            tool_calls,
            steps,
        )?;

        // 只更新阻断项，保留并发节点写入的证据/风险（原子读改写）
        let blockers: Vec<String> = final_reports
            .values()
            .flat_map(|r| r.blockers.iter().map(|b| b.reason.clone()))
            .collect();
        let ledger = self.store.mutate_ledger(session_id, |l| {
            l.risk.blockers = blockers;
        })?;
        self.emit(session_id, "ledger.updated", serde_json::to_value(&ledger)?);

        // ---- 自我进化：写入记忆（摘要/决策/证据/教训/个体统计） ----
        if self.memory_enabled {
            // 自主记忆摘要（实验性，experiments.llmDrivenMemory）：关闭程序自动
            // 写入（治理记忆库被无关内容灌满），改由模型经 memory_write 自主决定。
            // 个体可靠性统计照常（运行指标非记忆内容）。
            if self.config_snapshot.read().experiments.llm_driven_memory {
                for node in graph.list() {
                    if let Some(report) = final_reports.get(&node.id) {
                        let status = match report.status {
                            SyncStatus::Done => "done",
                            _ => "blocked",
                        };
                        let _ = self
                            .memory
                            .record_agent_outcome(&node.agent_identifier, status, report.confidence);
                    }
                }
            } else {
                let ids =
                    self.write_memories(session_id, text, &plan, &graph, &final_reports, &statements);
                // 语义向量后台回填（混合记忆检索；无嵌入模型时静默跳过）
                if !ids.is_empty() {
                    let o = self.clone();
                    tokio::spawn(async move {
                        o.backfill_embeddings(&ids).await;
                    });
                }
            }
            // 经验优化自动触发：新教训达阈值时后台合成（docs/10 §5，只调优既有个体）。
            // 自主记忆模式下无自动教训产生，不触发（模型仍可手动调 optimize）。
            if self.auto_adapt && !self.config_snapshot.read().experiments.llm_driven_memory {
                let mut agent_ids: Vec<String> = graph
                    .list()
                    .iter()
                    .map(|n| n.agent_identifier.clone())
                    .collect();
                agent_ids.sort();
                agent_ids.dedup();
                let o = self.clone();
                tokio::spawn(async move { o.auto_adapt(&agent_ids).await });
            }
        }

        self.emit(
            session_id,
            "run.finished",
            serde_json::json!({
                "routeLevel": format!("{:?}", plan.route_level),
                "agentId": self.orch_id(),
                "graph": graph.to_graph(),
                "statements": statements,
                "artifacts": self.store.outbox_drain_with_children(session_id).unwrap_or_default()
            }),
        );
        Ok(())
    }


    // ------------------------------------------------- 经验优化（docs/10 §5）



    // ---------------------------------------------------------- 分解


    // ---------------------------------------------------------- 收束


}

// ---------------------------------------------------------------- history 装配（群聊记录块）

/// 历史装配公共内核（可单测）：store 消息行 → 对话消息序列。
///
/// 群聊旁听（imctx:）采用主流 chat-log 转录口径：**连续旁听合并为一个
/// 「群聊记录」引用块**（行式 `名字：内容`，非逐条独立消息），块头标注
/// 这是背景记录。旁听块是背景上下文——不是对本体的指令；真实输入
/// （@ / 私聊 / 控制台）保持独立 user 消息（im: 发送者行内署名）。
/// 合并的动机：逐条独立 user 消息在模型视角与会话输入同权，实测会把
/// 旁听闲聊当成交谈对象一并回应（无关上下文回帖）。
pub(crate) fn history_with_ambient(
    rows: &[crate::types::ChatMessage],
) -> Vec<ChatMessage> {
    history_with_ambient_labeled(rows, |m, content| {
        // 默认口径：Orchestrator 消息呈现为助手回复；其余非 User 角色跳过
        if m.role == MessageRole::Orchestrator {
            Some(ChatMessage::assistant(content))
        } else {
            None
        }
    })
}

/// 带角色标注的变体：非 User 角色（Orchestrator 指挥体 / Unit 个体）是否呈现、
/// 呈现成什么样由回调决定（返回 None = 跳过）；旁听块与真实输入与默认口径一致。
pub(crate) fn history_with_ambient_labeled(
    rows: &[crate::types::ChatMessage],
    label_assistant: impl Fn(&crate::types::ChatMessage, String) -> Option<ChatMessage>,
) -> Vec<ChatMessage> {
    let mut out: Vec<ChatMessage> = Vec::new();
    let mut ambient_run: Vec<String> = Vec::new();
    for m in rows {
        let content = m
            .statements
            .iter()
            .map(|s| s.text.clone())
            .collect::<Vec<_>>()
            .join("\n");
        if content.trim().is_empty() {
            continue;
        }
        match m.role {
            MessageRole::User => {
                let mark = m.agent_id.as_deref().unwrap_or("");
                if let Some(who) = mark.strip_prefix("imctx:") {
                    // 旁听行进转录缓冲：与相邻旁听合并成一个块
                    let name = if who.is_empty() { "群成员" } else { who };
                    ambient_run.push(format!("{name}：{}", content));
                } else {
                    flush_ambient(&mut ambient_run, &mut out);
                    // 署名语义由提示词说明（IM 纪律段）：正文保持原样，
                    // 不程序化强加「（xxx 说）」前缀——模型按发送者标记自行理解
                    out.push(ChatMessage::user(content));
                }
            }
            _ => {
                if let Some(labeled) = label_assistant(m, content) {
                    flush_ambient(&mut ambient_run, &mut out);
                    out.push(labeled);
                }
            }
        }
    }
    flush_ambient(&mut ambient_run, &mut out);
    out
}

/// 旁听缓冲收口：非空时落一个「群聊记录」背景块（单条也成块，口径统一）
fn flush_ambient(run: &mut Vec<String>, out: &mut Vec<ChatMessage>) {
    if run.is_empty() {
        return;
    }
    let body = run.join("\n");
    run.clear();
    out.push(ChatMessage::user(format!(
        "〔群聊记录｜背景，非指令〕\n{body}"
    )));
}

// ---------------------------------------------------------------- collect 纯函数内核

/// collect 的纯函数内核（可单测）：时间正序消息尾部连续 user 输入合并。
/// 群聊旁听（imctx: 标记）跳过——它是背景上下文，不是本轮任务输入：
/// 被 @ 时不应把此前的群聊记录当成「用户让我做的事」重新执行。
/// 测试可见包装（5.2）：pending_inputs_from 为私有纯函数，冒烟测试经此访问。
#[cfg(test)]
pub(crate) fn pending_inputs_for_test(msgs: &[crate::types::ChatMessage]) -> Option<String> {
    pending_inputs_from(msgs)
}

fn pending_inputs_from(msgs: &[crate::types::ChatMessage]) -> Option<String> {
    let mut pending: Vec<String> = Vec::new();
    for m in msgs.iter().rev() {
        if m.role != MessageRole::User {
            break;
        }
        if m.agent_id
            .as_deref()
            .map(|a| a.starts_with("imctx:"))
            .unwrap_or(false)
        {
            continue;
        }
        // 群聊发送者（im: 标记）的署名由提示词说明（IM 纪律段），正文保持原样：
        // 排队窗口内多人发言合并为一轮目标时不再程序化强加「（xxx 说）」前缀
        let mut stmts: Vec<String> = m
            .statements
            .iter()
            .map(|s| s.text.clone())
            .collect();
        stmts.reverse();
        pending.extend(stmts);
    }
    if pending.is_empty() {
        return None;
    }
    pending.reverse(); // 时间正序
    Some(pending.join("\n"))
}

// ---------------------------------------------------------------- 节点执行上下文

pub struct ExecCtx {
    pub orchestrator: Arc<Orchestrator>,
    pub session_id: String,
    pub plan: OrchestratorPlan,
    pub reports: Arc<Mutex<Reports>>,
    /// 本轮能力边界（沿派发链下传；派发节点工具面按此裁剪）
    pub scope: ToolScope,
}

impl ExecCtx {
    pub async fn run_node(&self, node: TaskNode) -> NodeOutcome {
        // 停止检查点：已请求停止的会话，剩余节点快速失败（不再派发 LLM/工具）
        if crate::round_trace::is_cancelled(&self.session_id) {
            return NodeOutcome::failed(node.id.clone(), String::from("本轮已被用户停止"));
        }
        let o = self.orchestrator.clone();
        // 上游回流快照（短暂持锁，随即释放，不跨 await）
        let snapshot: Arc<Reports> = Arc::new(self.reports.lock().await.clone());
        let o = o.as_ref();
        let Some(def) = o.registry.get(&node.agent_identifier) else {
            return NodeOutcome::failed(
                node.id.clone(),
                format!("注册表不存在个体: {}", node.agent_identifier),
            );
        };

        // 智能连结（实验性）：连结体代理个体 → 任务转交外部连结体执行，
        // 回流走与本地一致的 SyncReport 口径（存储/消息/事件齐全）
        if let Some(link_id) = &def.link {
            let objective = if node.objective.is_empty() {
                node.title.clone()
            } else {
                node.objective.clone()
            };
            let (status, statements, summary, confidence) = match crate::nexus::execute_link(
                &o.config_snapshot(),
                link_id,
                &objective,
                &node.acceptance,
            )
            .await
            {
                Ok((summary, output)) => (
                    SyncStatus::Done,
                    vec![Statement::report(output)],
                    summary,
                    0.85,
                ),
                Err(e) => (
                    SyncStatus::Blocked,
                    vec![Statement::warn(format!("连结体执行失败：{e}"))],
                    format!("连结体执行失败: {e}"),
                    0.2,
                ),
            };
            let report = SyncReport {
                source_agent: def.identifier.clone(),
                task_node_id: node.id.clone(),
                status,
                statements: statements.clone(),
                summary: summary.clone(),
                evidence: vec![],
                risks: vec![],
                blockers: vec![],
                conflicts: None,
                next_suggestion: Default::default(),
                confidence,
            };
            // 子个体回流与报告落库（P2-5 吞错收敛）：这是关键写路径——
            // 失败=用户看不到子个体产出，不能静默；留日志供事后核对
            if let Err(e) = o.store.add_sync_report(&report) {
                eprintln!("[dispatch] SyncReport 落库失败（{}）：{e}", self.session_id);
            }
            if let Err(e) = o.store.add_message(
                &self.session_id,
                MessageRole::Unit,
                Some(def.identifier.as_str()),
                statements,
            ) {
                eprintln!("[dispatch] 子个体回流落库失败（{}）：{e}", self.session_id);
            }
            o.emit(
                &self.session_id,
                "sync.received",
                serde_json::json!({
                    "nodeId": node.id,
                    "report": serde_json::to_value(&report).unwrap_or(serde_json::Value::Null),
                    "nexus": true
                }),
            );
            let outcome_status = if report.status == SyncStatus::Done {
                TaskStatus::Done
            } else {
                TaskStatus::Failed
            };
            return NodeOutcome {
                node_id: node.id.clone(),
                status: outcome_status,
                error: (outcome_status == TaskStatus::Failed).then_some(summary),
                appends: vec![],
            };
        }

        // 命中技能（注入与终态统计共用同一判定，避免二次加载口径漂移）
        let matched_skills: Vec<crate::types::SkillDef> = o
            .registry
            .load_skills()
            .unwrap_or_default()
            .into_iter()
            .filter(|s| {
                s.applies_to(&def.identifier)
                    && s.matches(&format!(
                        "{} {} {}",
                        self.plan.goal, node.title, node.objective
                    ))
            })
            .collect();
        let order = DispatchOrder {
            task_node_id: node.id.clone(),
            objective: node.objective.clone(),
            acceptance: node.acceptance.clone(),
            boundary: self.plan.boundary.clone(),
            inputs: {
                let mut inputs = vec![DispatchInput {
                    ref_id: "user".into(),
                    kind: "userInput".into(),
                    summary: self.plan.goal.clone(),
                }];
                for dep in &node.depends_on {
                    if let Some(r) = snapshot.get(dep) {
                        inputs.push(DispatchInput {
                            ref_id: dep.clone(),
                            kind: "syncReport".into(),
                            summary: r.summary.clone(),
                        });
                    }
                }
                // 个体记忆 + 群体记忆：该个体的私有教训与共享决策/偏好（docs/08）。
                // 记忆命名空间取会话分区（平台记忆隔离）——默认与组共享。
                if o.memory_enabled {
                    let query = format!("{} {} {}", def.name, def.identifier, node.title);
                    let memory_group = o.memory_group_for_session(&self.session_id);
                    if let Ok(hits) = o.memory.recall_for_agent(
                        &def.identifier,
                        &query,
                        3,
                        Some(&memory_group),
                        o.embed_query(&query).await.as_deref(),
                    ) {
                        if !hits.is_empty() {
                            let summary = hits
                                .iter()
                                .map(|h| {
                                    let layer = if h.entry.agent_id.is_some() {
                                        "个体记忆"
                                    } else {
                                        "群体记忆"
                                    };
                                    format!(
                                        "[{layer}|{}] {}：{}",
                                        h.entry.kind.key(),
                                        h.entry.title,
                                        h.entry
                                            .body
                                            .replace('\n', " ")
                                            .chars()
                                            .take(160)
                                            .collect::<String>()
                                    )
                                })
                                .collect::<Vec<_>>()
                                .join(" ｜ ");
                            inputs.push(DispatchInput {
                                ref_id: "memory".into(),
                                kind: "memory".into(),
                                summary,
                            });
                        }
                    }
                }
                // 技能包注入（docs/10）：任务目标命中触发词时，把技能指令随派发下发
                for s in &matched_skills {
                    inputs.push(DispatchInput {
                        ref_id: format!("skill:{}", s.id),
                        kind: "skill".into(),
                        summary: s.instructions.clone(),
                    });
                }
                // 协议层注入（按需）：任务目标命中触发词的协议全文随派发下发
                if let Ok(protocols) = o.registry.load_protocols() {
                    let haystack = format!("{} {} {}", self.plan.goal, node.title, node.objective);
                    for (id, body) in protocol_unit_inputs(&protocols, &def.identifier, &haystack) {
                        inputs.push(DispatchInput {
                            ref_id: format!("protocol:{id}"),
                            kind: "protocol".into(),
                            summary: body.to_string(),
                        });
                    }
                }
                // 组共享上下文（实验性，experiments.groupSharedContext）：
                // 携带组主会话的滚动摘要 + 最近对话窗口——子个体不再只有 objective 孤岛视角。
                // 摘要与窗口同源规划路径（ensure_compacted），口径一致。
                if o.config_snapshot.read().experiments.group_shared_context {
                    let (summary, recent) = o.ensure_compacted(&self.session_id).await;
                    if let Some(ctx) = shared_context_input(summary, recent) {
                        inputs.push(ctx);
                    }
                }
                // 知识库注入（knowledge-base，默认关）：按任务目标检索 top-k 共享知识，
                // 作为派发输入与组共享上下文同层；检索失败静默跳过
                {
                    let cfg_k = o.config_snapshot.read().clone();
                    let section = crate::knowledge::inject_prompt_section(
                        &o.knowledge,
                        &self.plan.goal,
                        cfg_k.knowledge_inject_top_k,
                        cfg_k.knowledge_inject_max_chars,
                    );
                    if !section.is_empty() {
                        inputs.push(DispatchInput {
                            ref_id: "knowledge:topk".into(),
                            kind: "knowledge".into(),
                            summary: section.trim().to_string(),
                        });
                    }
                }
                inputs
            },
            // 主智能体权限注入：组主智能体获得组内个体管理工具（自定义组）与记忆三工具（默认授权）；
            // 个体工具面 = 编成 JSON 的 tools 清单（8.2），再按通道能力边界裁剪（chat / tools / computer）
            tool_allowlist: self.scope.filter_allowlist(
                &crate::tools::ToolGateway::runtime_allowlist(
                    &def,
                    o.registry.active_group_meta().as_ref(),
                ),
            ),
            constraints: DispatchConstraints {
                // 步数预算按配置分档（复杂任务需要更多「思考-调工具」轮次）；超时随步数放宽
                max_steps: (o.max_unit_steps as u32).max(3),
                timeout_ms: (o.max_unit_steps as u64).max(3) * 30_000,
            },
            report_format: "SyncReport".into(),
        };

        // 子会话（派发运行台账）：子个体的过程流（思考/工具/输出）与最终消息落在子会话里，
        // 主会话只保留派发与回流标记——「把子个体的输出放到一个子会话中」。
        // 创建失败时回落主会话（trace_key = 主会话，行为同旧版）。
        // 标题取真实诉求（goal 常带摘要上下文：取 `[用户]` 之后的片段并压平空白）。
        let goal_hint = self
            .plan
            .goal
            .rsplit("[用户]")
            .next()
            .unwrap_or_default()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let goal_short: String = if goal_hint.trim().is_empty() {
            node.title.clone()
        } else {
            goal_hint
                .chars()
                .take(24)
                .collect::<String>()
                .trim_end()
                .to_string()
        };
        let trace_key = match o.store.create_child_session(
            &self.session_id,
            &format!("sub:{}:{}·{}", def.identifier, goal_short, node.id),
        ) {
            Ok(s) => {
                // 子会话首条消息：本节点派发要求（打开子会话即知上下文）
                let _ = o.store.add_message(
                    &s.id,
                    MessageRole::User,
                    None,
                    vec![Statement::new(
                        SpeechTag::要求,
                        format!(
                            "【要求】{}｜节点 {}：{}",
                            self.plan.goal, node.id, node.title
                        ),
                    )],
                );
                s.id
            }
            Err(e) => {
                eprintln!("[orchestrator] 子会话创建失败（回落主会话）: {e}");
                String::new()
            }
        };
        let trace_key = if trace_key.is_empty() {
            self.session_id.clone()
        } else {
            trace_key
        };

        o.emit(
            &self.session_id,
            "dispatch.sent",
            serde_json::json!({
                "nodeId": node.id,
                "agentIdentifier": def.identifier,
                "childSessionId": (trace_key != self.session_id).then(|| trace_key.clone()),
                "adaptationRevision": o
                    .registry
                    .load_adaptation(&def.identifier)
                    .ok()
                    .flatten()
                    .map(|a| a.revision)
                    .unwrap_or(0),
                "order": serde_json::to_value(&order).unwrap_or(serde_json::Value::Null)
            }),
        );

        // 全连结：派发消息进总线（可观测；替换为分布式总线时业务零改动）
        let env = Envelope::new(
            format!("task.{}.dispatch", node.graph_id),
            o.orch_id(),
            def.identifier.clone(),
            "dispatch",
            self.session_id.clone(),
            serde_json::to_value(&order).unwrap_or(serde_json::Value::Null),
        );
        let _ = o.store.append_event(&env);

        let session_id = self.session_id.clone();
        let node_id = node.id.clone();
        let agent_label = def.identifier.clone();
        let events = o.events.clone();
        // 派发审计起点（agent-budget-audit 3.4）：耗时口径 = 派发指令组装完成 →
        // 回流收束（含远程/本地执行全程）；token 取子会话 usage_total。
        let dispatch_started = std::time::Instant::now();
        // 视觉能力检测：候选链首个模型明确标记不支持视觉时不回灌截图（computer use 降级为“盲操作”）
        let attach_images = {
            let vision = o
                .unit_candidates(&def)
                .first()
                .and_then(|(pid, _, model)| o.model_pool.capability(pid, model, true));
            vision != Some(false)
        };
        // 远程工作者优先（失败/超时回落本地）；令牌流统一进事件总线
        let result = {
            let orch = o;
            // 事件会话 = 子会话：主会话只保留派发/回流标记，不再承载个体流；
            // 思考同步入有序轨迹（子会话消息「按调用顺序」数据源）
            let sid = trace_key.clone();
            let nid = node_id.clone();
            let label = agent_label.clone();
            let evts = events.clone();
            let trace = trace_key.clone();
            let relay = move |delta: crate::provider::StreamDelta| {
                let (kind, text) = match delta {
                    crate::provider::StreamDelta::Thinking(t) => ("unit.thinking", t),
                    crate::provider::StreamDelta::Text(t) => ("unit.token", t),
                };
                if kind == "unit.thinking" {
                    crate::round_trace::push_thinking(&trace, &text);
                }
                let _ = evts.send(CoreEvent {
                    kind: kind.into(),
                    session_id: sid.clone(),
                    payload: serde_json::json!({
                        "agentId": label, "nodeId": nid, "delta": text
                    }),
                });
            };
            // 先判定远程可用性（注册表同步查询，避免闭包在分支间移动）
            let use_remote = orch.remote_enabled
                && orch
                    .remote
                    .as_ref()
                    .map(|r| r.accepts(&def))
                    .unwrap_or(false);
            if use_remote {
                let remote = orch.remote.clone().unwrap();
                let def2 = def.clone();
                let order2 = order.clone();
                let sid2 = session_id.clone();
                match remote
                    .execute(&sid2, &def2, &order2, attach_images, &relay)
                    .await
                {
                    Ok(report) => Ok(report),
                    Err(e) => {
                        let _ = orch.events.send(CoreEvent {
                            kind: "remote.fallback".into(),
                            session_id: self.session_id.clone(),
                            payload: serde_json::json!({ "agentId": def.identifier, "reason": e.to_string() }),
                        });
                        let chain = orch.unit_chain_for(&def);
                        // 回落本地执行：思考增量照常挂到 trace（与本地直连路径一致），
                        // 否则回落轮的子会话过程轨迹缺失
                        orch.unit_runtime
                            .execute(
                                &def,
                                &order,
                                chain,
                                &self.session_id,
                                &trace_key,
                                attach_images,
                                self.scope,
                                &relay,
                            )
                            .await
                    }
                }
            } else {
                let chain = orch.unit_chain_for(&def);
                let sid_for_usage = self.session_id.clone();
                orch.unit_runtime
                    .execute(
                        &def,
                        &order,
                        chain,
                        &sid_for_usage,
                        &trace_key,
                        attach_images,
                        self.scope,
                        relay,
                    )
                    .await
            }
        };

        match result {
            Ok(report) => {
                // 技能使用统计：节点终态回填（Done = 成功；Blocked/需裁决 = 未成功）
                let skill_ok = !matches!(
                    report.status,
                    SyncStatus::Blocked | SyncStatus::NeedArbitration
                );
                for s in &matched_skills {
                    o.registry.record_skill_use(&s.id, skill_ok);
                }
                // 派发审计（agent-budget-audit 3.4）：成功/受阻/仲裁回流都记一条；
                // token 口径 = 子会话累计（remote/local 执行都会写子会话 usage）
                let (dp, dc, _) = o.store.usage_total(&trace_key);
                let _ = o.store.audit_dispatch(
                    &self.session_id,
                    &serde_json::json!({
                        "id": new_id(),
                        "nodeId": node.id,
                        "agentId": def.identifier,
                        "childSessionId": (trace_key != self.session_id).then(|| trace_key.clone()),
                        "summary": goal_short,
                        "durationMs": dispatch_started.elapsed().as_millis() as u64,
                        "promptTokens": dp,
                        "completionTokens": dc,
                        "status": format!("{:?}", report.status),
                        "ok": !matches!(report.status, SyncStatus::Blocked),
                        "createdAt": now_iso(),
                    }),
                );
                let report_id = match o.store.add_sync_report(&report) {
                    Ok(id) => id,
                    Err(e) => return NodeOutcome::failed(node.id.clone(), e.to_string()),
                };
                if let Err(e) =
                    o.store
                        .add_evidence(&self.session_id, &report.evidence, Some(&report_id))
                {
                    eprintln!("[orchestrator] 证据写入失败: {e}");
                }
                // 子个体输出落子会话（含有序过程步骤）；主会话不再直接展示个体消息
                if trace_key == self.session_id {
                    // 子会话创建失败（回落主会话）：保持旧行为
                    let _ = o.store.add_message(
                        &self.session_id,
                        MessageRole::Unit,
                        Some(def.identifier.as_str()),
                        report.statements.clone(),
                    );
                } else {
                    let (thinking, tool_calls, steps) = crate::round_trace::drain(&trace_key);
                    crate::round_trace::end(&trace_key);
                    let _ = o.store.add_message_full(
                        &trace_key,
                        MessageRole::Unit,
                        Some(def.identifier.as_str()),
                        report.statements.clone(),
                        thinking,
                        tool_calls,
                        steps,
                    );
                    // 收尾事件：子会话视图据此结束实时态并回拉持久消息（含有序步骤）
                    o.emit(
                        &trace_key,
                        "unit.finished",
                        serde_json::json!({
                            "agentId": def.identifier,
                            "nodeId": node.id,
                            "statements": report.statements,
                        }),
                    );
                }

                o.emit(
                    &self.session_id,
                    "sync.received",
                    serde_json::json!({
                        "nodeId": node.id,
                        "report": serde_json::to_value(&report).unwrap_or(serde_json::Value::Null),
                        "schemaValid": true
                    }),
                );

                // 三账：证据与风险（原子读改写，避免并发节点互相覆盖）
                let ledger = o
                    .store
                    .mutate_ledger(&self.session_id, |l| {
                        for e in &report.evidence {
                            l.evidence.confirmed.push(LedgerEvidenceItem {
                                text: format!("{} — {}", e.reference, e.note),
                                level: e.level,
                                source: def.identifier.clone(),
                            });
                        }
                        for r in &report.risks {
                            l.risk.impact.push(format!("{:?}: {}", r.severity, r.text));
                        }
                    })
                    .unwrap_or_default();
                o.emit(
                    &self.session_id,
                    "ledger.updated",
                    serde_json::to_value(&ledger).unwrap_or_default(),
                );

                self.reports
                    .lock()
                    .await
                    .insert(node.id.clone(), report.clone());

                if matches!(report.status, SyncStatus::Blocked) {
                    return NodeOutcome::blocked(node.id.clone());
                }

                let mut appends: Vec<NewNode> = Vec::new();
                let need_arbitration = matches!(report.status, SyncStatus::NeedArbitration)
                    || report
                        .conflicts
                        .as_ref()
                        .map(|c| !c.is_empty())
                        .unwrap_or(false);
                if need_arbitration {
                    let points = report
                        .conflicts
                        .as_ref()
                        .map(|cs| {
                            cs.iter()
                                .map(|c| c.point.clone())
                                .collect::<Vec<_>>()
                                .join("；")
                        })
                        .unwrap_or_default();
                    o.emit(
                        &self.session_id,
                        "arbitration.required",
                        serde_json::json!({ "nodeId": node.id, "conflicts": report.conflicts }),
                    );
                    // 裁决个体按数据声明动态选路（registry::arbiter），无命中回退主智能体
                    let arbiter_id = o
                        .registry
                        .arbiter()
                        .map(|d| d.identifier)
                        .unwrap_or_else(|| o.orch_id());
                    appends.push(NewNode {
                        agent_identifier: arbiter_id,
                        title: format!("裁决：{}", node.title),
                        objective: format!(
                            "基于以下冲突回流做出裁决：{points}。回流摘要：{}",
                            report.summary
                        ),
                        acceptance: vec![
                            "裁决结论带证据等级".into(),
                            "残余风险与回退路径明确".into(),
                        ],
                        priority: Priority::P0,
                    });
                }
                NodeOutcome::done(node.id.clone()).with_appends(appends)
            }
            Err(e) => {
                // 技能使用统计：执行失败记一次未成功（供后续 update 修正提供依据）
                for s in &matched_skills {
                    o.registry.record_skill_use(&s.id, false);
                }
                // 子会话留痕：失败原因 + 已收集的过程轨迹（思考/工具）一并落子会话，便于回看
                let (thinking, tools, steps) = crate::round_trace::drain(&trace_key);
                if trace_key != self.session_id {
                    let _ = o.store.add_message_full(
                        &trace_key,
                        MessageRole::System,
                        None,
                        vec![Statement::warn(format!("节点执行失败：{e}"))],
                        thinking,
                        tools,
                        steps,
                    );
                    o.emit(
                        &trace_key,
                        "unit.finished",
                        serde_json::json!({ "agentId": def.identifier, "nodeId": node.id, "failed": true }),
                    );
                }
                // 派发审计（agent-budget-audit 3.4）：失败/超时也记——异常高亮数据源
                let (dp_f, dc_f, _) = o.store.usage_total(&trace_key);
                let _ = o.store.audit_dispatch(
                    &self.session_id,
                    &serde_json::json!({
                        "id": new_id(),
                        "nodeId": node.id,
                        "agentId": def.identifier,
                        "childSessionId": (trace_key != self.session_id).then(|| trace_key.clone()),
                        "summary": goal_short,
                        "durationMs": dispatch_started.elapsed().as_millis() as u64,
                        "promptTokens": dp_f,
                        "completionTokens": dc_f,
                        "status": "Failed",
                        "ok": false,
                        "error": e.to_string(),
                        "createdAt": now_iso(),
                    }),
                );
                crate::round_trace::end(&trace_key);
                NodeOutcome::failed(node.id.clone(), e.to_string())
            }
        }
    }
}

/// 提炼结果净化：只保留 "- " 列表行，最多 6 条、每条 ≤100 字、去重
fn sanitize_adaptation(text: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        let Some(rest) = line.strip_prefix("- ").or_else(|| line.strip_prefix("• ")) else {
            continue;
        };
        let rest = rest.trim();
        if rest.is_empty() || rest.contains("暂无") {
            continue;
        }
        let item = rest.chars().take(100).collect::<String>();
        if !lines.contains(&item) {
            lines.push(item);
        }
        if lines.len() >= 6 {
            break;
        }
    }
    lines
        .into_iter()
        .map(|l| format!("- {l}"))
        .collect::<Vec<_>>()
        .join(
            "
",
        )
}

// ---------------------------------------------------------------- 收束文本 → 陈述序列

/// 收束降级文本：收束 LLM 不可用时，把各节点回流按最小结构聚合为最终交付。
/// 为什么不直接报错：调度已执行完毕、子个体回流已落库，因「收束这一跳」失败而丢弃
/// 全部成果太亏——降级输出保留节点状态、摘要、阻塞项与失败原因，轮次仍有可执行结论。
/// 形态纪律（anti-audit-converge）：不用分节标题的审计腔——用户要的是「结果 + 缺口」
/// 两件事，几行说完；节点明细是支撑信息，压缩进列表即可。
fn fallback_converge_text(
    plan: &OrchestratorPlan,
    reports: &Reports,
    graph: &TaskGraphModel,
    reason: &str,
) -> String {
    let (mut done, mut blocked, mut failed) = (0usize, 0usize, 0usize);
    let mut node_lines: Vec<String> = Vec::new();
    for n in graph.list() {
        match n.status {
            TaskStatus::Done => done += 1,
            TaskStatus::Failed | TaskStatus::Cancelled => failed += 1,
            TaskStatus::Blocked => blocked += 1,
            _ => {}
        }
        let Some(r) = reports.get(&n.id) else {
            node_lines.push(format!(
                "- {} {}({})：无回流",
                n.id, n.title, n.agent_identifier
            ));
            continue;
        };
        let mut line = format!(
            "- {} {}({}) [{}]：{}",
            n.id,
            n.title,
            n.agent_identifier,
            n.status.label(),
            r.summary.chars().take(300).collect::<String>()
        );
        for b in &r.blockers {
            line.push_str(&format!(
                "（阻塞：{}；解除条件：{}）",
                b.reason, b.unblock_condition
            ));
        }
        node_lines.push(line);
    }
    let mut lines: Vec<String> = Vec::new();
    // 目标一句话 + 结果统计先行（用户最先要知道的）
    lines.push(format!(
        "目标「{}」执行完毕：{} 个节点，完成 {done}、受阻 {blocked}、失败 {failed}。",
        plan.goal,
        graph.list().len()
    ));
    if !plan.acceptance.is_empty() {
        lines.push(format!("验收口径：{}。", plan.acceptance.join("；")));
    }
    lines.push(String::new());
    lines.push("各节点结果：".into());
    lines.extend(node_lines);
    lines.push(String::new());
    lines.push(format!(
        "（综合裁决未完成：收束模型调用失败 {reason}。以上为各节点回流的直接聚合，未经主智能体综合；恢复模型通道后重发同类任务可获得综合结论。）"
    ));
    lines.join("\n")
}

/// 剥离行首列表标记（`- `/`* `/`+ `/`1. `/`1) `/`1、`），返回剩余内容。
/// 收束输出常以列表形式给句式前缀行（`- 【否定】…`），标记不剥会把
/// 带标记的陈述行误判为未标记文本（双层前缀的根源之一）。
fn strip_list_marker(t: &str) -> &str {
    let s = t.trim_start();
    for m in ["- ", "* ", "+ ", "-\t"] {
        if let Some(rest) = s.strip_prefix(m) {
            return rest.trim_start();
        }
    }
    if s == "-" {
        return "";
    }
    if let Some(rest) = s.strip_prefix('-') {
        if rest.starts_with(char::is_whitespace) {
            return rest.trim_start();
        }
    }
    // 数字序号：1. / 1) / 1、（序号后可直接跟内容）
    let digits_end = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(0);
    if digits_end > 0 {
        let tail = &s[digits_end..];
        let sep_len = tail
            .chars()
            .next()
            .filter(|c| matches!(*c, '.' | ')' | '、'))
            .map(|c| c.len_utf8())
            .unwrap_or(0);
        if sep_len > 0 {
            return tail[sep_len..].trim_start();
        }
    }
    s
}

pub fn text_to_statements(text: &str) -> Vec<Statement> {
    const TAGS: [SpeechTag; 8] = [
        SpeechTag::肯定,
        SpeechTag::否定,
        SpeechTag::疑问,
        SpeechTag::报告,
        SpeechTag::提案,
        SpeechTag::警告,
        SpeechTag::要求,
        SpeechTag::观测,
    ];
    let mut out = Vec::new();
    // 未标记行按段落聚合：同一句陈述跨多行（列表项/续行）合并为一条，
    // 避免逐行各起一条【报告】把 IM 回复刷成前缀墙（部署实测）。
    let mut pending: Vec<String> = Vec::new();
    fn flush(pending: &mut Vec<String>, out: &mut Vec<Statement>) {
        if pending.is_empty() {
            return;
        }
        let joined = pending.join("\n");
        pending.clear();
        let block = strip_list_marker(&joined);
        let t = block.trim();
        if t.is_empty() {
            return;
        }
        if let Some(rest) = t.strip_prefix("##") {
            out.push(Statement::report(rest.trim()));
        } else {
            out.push(Statement::report(t));
        }
    }
    for line in text.lines() {
        let raw = line.trim();
        if raw.is_empty() {
            // 空行 = 段落边界：聚合中的未标记块收口成一条陈述
            flush(&mut pending, &mut out);
            continue;
        }
        let t = strip_list_marker(raw);
        let mut matched = false;
        for tag in TAGS {
            let prefix = format!("【{}】", tag.as_str());
            if let Some(rest) = t.strip_prefix(&prefix) {
                flush(&mut pending, &mut out);
                out.push(Statement::new(tag, rest.trim()));
                matched = true;
                break;
            }
        }
        if matched {
            continue;
        }
        if t.starts_with("##") {
            flush(&mut pending, &mut out);
            out.push(Statement::report(t.strip_prefix("##").unwrap_or(t).trim()));
            continue;
        }
        pending.push(raw.to_string());
    }
    flush(&mut pending, &mut out);
    if out.is_empty() {
        out.push(Statement::report("（收束输出为空）"));
    }
    out
}

// ---------------------------------------------------------------- 协议层注入（按需）

/// 单次注入上限：触发词宽泛时防止协议全文挤占上下文
const PROTOCOL_INJECT_CAP: usize = 4;

/// 规划期协议段：常驻目录（id/用途/触发词/注入点）+ 命中触发词的协议全文。
/// 命中序：plan 专属优先（如收拢/分派），其余保持文件名序；上限 PROTOCOL_INJECT_CAP。
fn protocol_plan_block(protocols: &[ProtocolDef], text: &str) -> String {
    if protocols.is_empty() {
        return String::new();
    }
    let mut out = String::from(
        "## 协议层（按需注入）\n命中协议的全文附于本节末尾，规划时必须遵守；注入点标注「派发」的协议会随对应节点下发给子个体。\n",
    );
    for p in protocols {
        let slot = match (p.injects_plan(), p.injects_unit()) {
            (true, true) => "规划+派发",
            (true, false) => "规划",
            _ => "派发",
        };
        out.push_str(&format!(
            "- {}（{}）：{}；触发 [{}]；注入 {}\n",
            p.id,
            p.name,
            if p.description.is_empty() {
                "—"
            } else {
                p.description.as_str()
            },
            if p.triggers.is_empty() {
                "—".to_string()
            } else {
                p.triggers.join("、")
            },
            slot
        ));
    }
    let mut hits: Vec<&ProtocolDef> = protocols
        .iter()
        .filter(|p| p.injects_plan() && p.matches(text))
        .collect();
    hits.sort_by_key(|p| usize::from(p.injects_unit()));
    let hits: Vec<&ProtocolDef> = hits.into_iter().take(PROTOCOL_INJECT_CAP).collect();
    if !hits.is_empty() {
        out.push_str("\n### 本次任务命中的协议（全文）\n");
        for p in hits {
            out.push_str(&format!("\n#### {}（{}）\n{}\n", p.name, p.id, p.body));
        }
    }
    out.push('\n');
    out
}

/// 派发期协议输入：命中触发词的 unit 范围协议（id, 正文）。上限 PROTOCOL_INJECT_CAP。
fn protocol_unit_inputs<'a>(
    protocols: &'a [ProtocolDef],
    agent: &str,
    haystack: &str,
) -> Vec<(&'a str, &'a str)> {
    protocols
        .iter()
        .filter(|p| p.injects_unit() && p.applies_to(agent) && p.matches(haystack))
        .take(PROTOCOL_INJECT_CAP)
        .map(|p| (p.id.as_str(), p.body.as_str()))
        .collect()
}

/// 组共享上下文的注入体（实验性，experiments.groupSharedContext）：
/// 滚动摘要 + 最近窗口 → 一条 DispatchInput；两者皆空时返回 None（不注入空块）。
fn shared_context_input(summary: Option<String>, recent: String) -> Option<DispatchInput> {
    let mut ctx = String::new();
    if let Some(s) = summary.filter(|s| !s.trim().is_empty()) {
        ctx.push_str(&format!("### 会话滚动摘要（更早对话）\n{s}\n\n"));
    }
    if !recent.trim().is_empty() {
        ctx.push_str(&format!("### 最近对话窗口\n{recent}"));
    }
    if ctx.trim().is_empty() {
        return None;
    }
    Some(DispatchInput {
        ref_id: "sharedContext".into(),
        kind: "context".into(),
        summary: ctx.trim().to_string(),
    })
}

// ---------------------------------------------------------------- 领域分文件（5.3）
// plan（分解/规划调用）/ finalize（收束回填）/ optimize（经验优化）按域拆出；
// 主文件保留：轮次入口五阶段、执行循环、事件、模型解析、协议注入、history 装配。
mod finalize;
mod optimize;
mod plan;

// 原 orchestrator.rs 内联测试整体迁出（纯搬移）→ orchestrator/tests.rs
#[cfg(test)]
mod tests;
