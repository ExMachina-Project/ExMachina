//! 自主性实验（experiments.superAgent / experiments.autoAgent）。
//!
//! 工作法源自 super-agent 设计（尽可能少占用人注意力，尽可能多交付价值）：
//! - **只在该说话时说话**：工作中静默；需要决策 / 交付结果 / 无法继续时才产出一条消息；
//! - **按影响停，不按工作量停**：本地推进到「可审阅的结果」为止；需要设计决策时
//!   带着一个具体问题停下来；
//! - **诚实汇报**：草稿标注未完成，失败就是失败，不绕过限制避免提问。
//!
//! 两个实验共享该模块：
//! - **Super Agent（深度委托）**：会话级模式。用户给目标 → agent 在该会话内
//!   静默长链路推进（工具循环 + spawn 并行子代理），只在停机条件产出消息。
//!   实现为系统提示注入（无新执行路径：单智能体轮/组调度原样跑）。
//! - **Auto Agent（完全自主）**：进程级模式。调度器每周期触发一次 **Eval 自评**
//!   （「有没有值得做的事」）而非直接执行任务；Eval 通过才自主开会话执行，
//!   连续无任务退避（防空转）。Eval 与执行共用 run，落普通会话（可审计可回放）。

use crate::config::ExmConfig;

/// Super Agent 系统提示（experiments.superAgent 开启时注入用户会话）。
///
/// 约定输出契约：最终回复只在该说话时非空——需要决策（一个问题）/ 交付结果 /
/// 无法继续（说明缺口）。其余情况产出简短完成注记。
pub const SUPER_AGENT_DISCIPLINE: &str = "## 工作法（Super Agent 深度委托）

本会话运行在深度委托模式：管理者给了目标，期望你自主推进到可交付，而不是频繁汇报。

行为准则：
- **静默推进**：过程中不请求确认、不播报进度；用工具把事做完（可 spawn 并行子代理拆解独立子任务）。
- **组模式**：规划时把独立子任务并行拆给组内个体（nodes 无依赖即并行）；收束只汇总结论与验证证据，不复述过程。
- **按影响停，不按工作量停**：本地能推进的（查证、实现、验证）一律推进到「可审阅的结果」；
  需要管理者设计决策/授权才能继续时才停，停下时只问**一个具体问题**（带选项与建议）。
- **只在三种情况产出非空回复**：
  1. 交付结果（结论 + 关键产出引用 + 验证证据）；
  2. 需要决策（一个具体问题 + 候选方案 + 你的建议）；
  3. 无法继续（明确说明缺口与已尝试的路径）。
- **诚实**：未完成就标注未完成；失败如实报；不要为了不提问而绕过限制。
- **信息足够就假设并推进**：把显式假设写进交付说明，而不是停下来问可以自行验证的事。";

/// Auto Agent 的 Eval 自评提示词（每周期一次；不是执行任务，是判断值不值得做）。
///
/// 输出契约：JSON `{"act": false}` 或 `{"act": true, "task": "一句话目标", "why": "为什么现在值得做"}`。
pub const AUTO_EVAL_PROMPT: &str = r#"你是系统的自主巡评体（Auto Agent Eval）。检查当前环境，判断**现在有没有值得自主去做的事**。

检查来源（用工具看，别猜）：
- 待办与未决：todo 清单、审批队列、未读的子代理产出
- 记忆：最近教训/决策里有没有指向未完成的承诺
- 环境：工作区有无可维护的漂移（失败测试、过期文档、堆积的产物）

判定纪律：
- 只挑「不做会更糟」或「明显增值且低风险」的事；日常闲聊、探索性尝试、大重构一律不做；
- 一次最多一个任务，目标一句话可验收；
- 不确定 = 不做（no act）；诚实优先于活跃。

只输出一个 JSON（不要任何其他文本）：
{"act": false}
或
{"act": true, "task": "一句话目标", "why": "为什么现在值得做"}"#;

/// Auto Agent 的执行系统提示（Eval 通过后自主行动轮）。
pub const AUTO_ACT_DISCIPLINE: &str = "## 工作法（Auto Agent 自主行动）

你是系统的自主执行体（Auto Agent）：Eval 已选定目标（见任务输入），无人在场监督。

行为准则：
- **完全自主推进**：不提问、不等确认（没人回答）；用工具做完并自行验证；
- **风险闸门**：涉及删除、覆盖他人工作、外部发送、消费配额的操作——跳过并在结果中记录「已跳过 + 原因」，留给管理者；
- **步数预算有限**：优先完成主路径；预算耗尽前进度落盘（文件/记忆），输出「进度 + 下次续点」；
- **结束时产出**：做了什么 / 结果与验证 / 被跳过的事项（如有）——管理者醒来只读这一段。";

/// 解析 Eval 输出：`{"act": true, "task": ..., "why": ...}` / `{"act": false}`。
/// 非法输出按「无任务」处理（保守侧：Eval 失败不触发行动）。
pub fn parse_auto_eval(output: &str) -> Option<(String, String)> {
    // 容错：模型可能带 ```json 围栏或前后缀文本——截取首个 {...} 平衡段
    let trimmed = output.trim();
    let start = trimmed.find('{')?;
    let end = trimmed.rfind('}')?;
    if end <= start {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(&trimmed[start..=end]).ok()?;
    if !v.get("act")?.as_bool()? {
        return None;
    }
    let task = v.get("task")?.as_str()?.trim().to_string();
    if task.is_empty() {
        return None;
    }
    let why = v.get("why").and_then(|x| x.as_str()).unwrap_or("").to_string();
    Some((task, why))
}

/// Auto Agent 退避间隔（分钟）：连续 idle 次 × 基础间隔，封顶 backoff_max 倍。
/// 首次空转 = 基础间隔；有任务即由调用方复位 idle=0（下次到期即评）。
pub fn auto_backoff_minutes(cfg: &ExmConfig, idle_streak: u32) -> u64 {
    let base = cfg.automation.auto_agent_interval_minutes.max(1) as u64;
    let cap = cfg.automation.auto_agent_backoff_max.max(1) as u64;
    let n = (idle_streak as u64).max(1);
    base * n.min(cap)
}

/// Auto Agent 单次行动步数预算：显式配置优先，0 = 沿用派发步数上限
pub fn auto_act_max_steps(cfg: &ExmConfig) -> usize {
    let s = cfg.automation.auto_agent_max_steps;
    if s > 0 {
        s
    } else {
        cfg.automation.unit_max_steps
    }
}

/// 主动搜索纪律（proactive-search）：任一搜索通路可用（API 后端 / 浏览器搜索）
/// 且开关开启时注入系统提示。目标是让模型**自主判断**何时该搜——不等用户显式
/// 说「搜一下」。工具下发面（web_search 是否出现在工具表）由 search_ready 决定，
/// 与本纪律无关；这里只解决「工具在表里但模型想不到用」的行为层缺口。
pub const SEARCH_DISCIPLINE: &str = "## 主动搜索纪律（联网已就绪）

你有联网检索能力（web_search API 后端，或 browser 的 op=search 浏览器搜索）
与 web_fetch / browser open 网页访问能力。以下情况**不等用户要求，先搜再答**：

- **时效性问题**：新闻、版本号、价格、「最近/最新/今天」——训练数据必然过期；
- **事实核验**：具体数字、日期、人名、API 参数等可证伪断言——引用前先核对；
- **未知概念**：遇到没把握的术语、库、框架——搜一下再回答，不要凭印象编造；
- **用户显式让你查**：直接搜，不要反问「要不要我帮你搜」。

搜索行为约束：
- 搜索是**手段不是表演**：结果不足以支撑结论时换关键词再搜（最多 2 轮），仍不足就
  如实说明「检索未果 + 已尝试的词」；
- 中间结果不播报：搜到什么直接用于回答；用户要来源时给「标题 + 链接」；
- 不要为聊天寒暄、纯观点、创作类请求搜索——这些不需要外部信息。

工具选择：
- 优先 web_search（结构化结果，快）；未配置该工具时用 browser op=search（浏览器
  直接搜，结果真实所见）；需要某条结果的完整内容用 browser open / web_fetch 打开。";

#[cfg(test)]
mod autonomy_tests {
    use super::*;

    #[test]
    fn eval解析_合法与容错() {
        // 标准输出
        let (task, why) = parse_auto_eval(r#"{"act": true, "task": "修复失败测试", "why": "主干红了"}"#)
            .expect("应解析出任务");
        assert_eq!(task, "修复失败测试");
        assert_eq!(why, "主干红了");
        // 围栏包裹
        let (t2, _) = parse_auto_eval(
            "```json\n{\"act\": true, \"task\": \"整理产物目录\", \"why\": \"堆积\"}\n```",
        )
        .expect("围栏容错");
        assert_eq!(t2, "整理产物目录");
        // act=false → 无任务
        assert!(parse_auto_eval(r#"{"act": false}"#).is_none());
        // 空任务 → 无任务
        assert!(parse_auto_eval(r#"{"act": true, "task": "  "}"#).is_none());
        // 非 JSON → 无任务（保守）
        assert!(parse_auto_eval("我觉得没事可做").is_none());
    }

    #[test]
    fn eval解析_缺字段保守拒绝() {
        // act 缺失
        assert!(parse_auto_eval(r#"{"task": "x"}"#).is_none());
        // task 非字符串
        assert!(parse_auto_eval(r#"{"act": true, "task": 42}"#).is_none());
    }

    #[test]
    fn 退避_线性封顶() {
        let dir = std::env::temp_dir().join(format!("exm-auto-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut cfg = ExmConfig::load(&dir);
        cfg.automation.auto_agent_interval_minutes = 10;
        cfg.automation.auto_agent_backoff_max = 3;
        assert_eq!(auto_backoff_minutes(&cfg, 0), 10, "无空转 = 基础间隔");
        assert_eq!(auto_backoff_minutes(&cfg, 1), 10, "首次空转 = 基础间隔");
        assert_eq!(auto_backoff_minutes(&cfg, 2), 20);
        assert_eq!(auto_backoff_minutes(&cfg, 3), 30);
        assert_eq!(auto_backoff_minutes(&cfg, 9), 30, "封顶 3 倍");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 步数预算_显式优先与回落() {
        let dir = std::env::temp_dir().join(format!("exm-auto-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut cfg = ExmConfig::load(&dir);
        cfg.automation.unit_max_steps = 16;
        cfg.automation.auto_agent_max_steps = 0;
        assert_eq!(auto_act_max_steps(&cfg), 16, "0 = 沿用派发上限");
        cfg.automation.auto_agent_max_steps = 40;
        assert_eq!(auto_act_max_steps(&cfg), 40, "显式配置优先");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 纪律文本_含关键约定() {
        assert!(SUPER_AGENT_DISCIPLINE.contains("一个具体问题"), "停机问答约束");
        assert!(SUPER_AGENT_DISCIPLINE.contains("静默推进"), "静默约束");
        assert!(SUPER_AGENT_DISCIPLINE.contains("组模式"), "组模式并行拆解指引");
        assert!(AUTO_ACT_DISCIPLINE.contains("风险闸门"), "自主行动风险闸门");
        assert!(AUTO_EVAL_PROMPT.contains("不确定 = 不做"), "Eval 保守纪律");
    }

}

