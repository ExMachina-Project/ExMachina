//! `exm skill` / `exm cron` / `exm approval` —— 平台能力命令（docs/10）

use crate::render::*;
use exm_core::types::{ApprovalRequest, CronJob, SkillDef};
use exm_core::Core;
use owo_colors::OwoColorize;

// ---------------------------------------------------------------- 技能包

pub fn run_skill_list(core: &Core) -> anyhow::Result<()> {
    let skills = core.skills()?;
    if skills.is_empty() {
        println!(
            "{}",
            dim("（当前组无技能包；exm skill add <id> --name <名称> --instructions <指令> 创建）")
        );
        return Ok(());
    }
    println!(
        "{}",
        format!("技能包（全体组共用，共 {} 个）", skills.len()).bold()
    );
    for s in skills {
        let agents = if s.agents.is_empty() {
            "全体".to_string()
        } else {
            s.agents.join("、")
        };
        println!(
            "  {} {} {}",
            s.id.cyan(),
            s.name.bold(),
            dim(format!("适用 [{}]", agents))
        );
        if !s.description.is_empty() {
            println!("    {}", dim(&s.description));
        }
        println!(
            "    {} {}",
            dim("触发:"),
            if s.triggers.is_empty() {
                "—".to_string()
            } else {
                s.triggers.join("、")
            }
        );
        if s.version > 0 {
            let mut meta = format!("版本 v{}", s.version);
            if !s.updated_at.is_empty() {
                meta.push_str(&format!("（更新于 {}）", s.updated_at));
            }
            println!("    {}", dim(meta));
        }
        println!(
            "    {}",
            s.instructions.chars().take(120).collect::<String>()
        );
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub fn run_skill_add(
    core: &Core,
    id: &str,
    name: &str,
    description: &str,
    instructions: &str,
    triggers: Vec<String>,
    agents: Vec<String>,
) -> anyhow::Result<()> {
    let skill = SkillDef {
        id: id.trim().to_string(),
        name: name.trim().to_string(),
        description: description.trim().to_string(),
        triggers,
        instructions: instructions.trim().to_string(),
        agents,
        created_at: exm_core::types::now_iso(),
        source: "manual".into(),
        ..Default::default()
    };
    let written = core.add_skill(skill)?;
    println!(
        "{} 技能包已写入：{}（v{}，全体组共用）",
        ok("完成"),
        id.cyan(),
        written.version
    );
    println!(
        "{}",
        dim("派发时任务目标命中触发词即自动携带该指令；热装载，无需重启。")
    );
    Ok(())
}

pub fn run_skill_remove(core: &Core, id: &str) -> anyhow::Result<()> {
    if core.remove_skill(id)? {
        println!("{} 技能包已删除：{id}", ok("完成"));
    } else {
        anyhow::bail!("技能包不存在: {id}");
    }
    Ok(())
}

/// 从 SKILL.md 文件导入（标准 frontmatter + Markdown 正文；同 id 覆盖）
pub fn run_skill_import(core: &Core, file: &str) -> anyhow::Result<()> {
    let raw =
        std::fs::read_to_string(file).map_err(|e| anyhow::anyhow!("读取文件失败 {file}: {e}"))?;
    let skill = core.import_skill_md(&raw)?;
    println!(
        "{} 技能包已导入：{}（{}，v{}，触发词 {} 个，适用 {}）",
        ok("完成"),
        skill.id.cyan(),
        skill.name,
        skill.version,
        skill.triggers.len(),
        if skill.agents.is_empty() {
            "全体".to_string()
        } else {
            skill.agents.join("、")
        }
    );
    Ok(())
}

/// 导出技能为 SKILL.md（out 为空 = stdout）
pub fn run_skill_export(core: &Core, id: &str, out: &str) -> anyhow::Result<()> {
    let md = core
        .export_skill_md(id)?
        .ok_or_else(|| anyhow::anyhow!("技能包不存在: {id}"))?;
    if out.trim().is_empty() {
        print!("{md}");
    } else {
        std::fs::write(out, md)?;
        println!("{} 已导出到 {}", ok("完成"), out.cyan());
    }
    Ok(())
}

/// 技能版本历史（capability-parity 6.5）：CLI 与 REST/面板共用 registry 同一事实源。
pub fn run_skill_versions(core: &Core, id: &str) -> anyhow::Result<()> {
    let history = core.skill_history_detailed(id)?;
    if history.is_empty() {
        anyhow::bail!("技能包不存在或无版本记录: {id}");
    }
    println!("{}", format!("技能包 {id} 版本历史（旧 → 新）").bold());
    // 已删除：没有「当前生效版本」——与 REST 同一口径，不能把最后一个归档版本报成生效中
    let deleted = core.skill_is_deleted(id)?;
    let current = if deleted {
        None
    } else {
        history.last().map(|r| r.version)
    };
    if deleted {
        println!("  {}", warn("技能包已删除：历史保留，可回滚恢复任一版本"));
    }
    for r in &history {
        let marker = if Some(r.version) == current {
            ok("● 当前")
        } else {
            dim("○")
        };
        println!("  {} v{} {}", marker, r.version, dim(&r.name));
        // 每版写了什么：名字通常各版相同，只列名字等于没有信息
        let head: String = r
            .instructions
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or("")
            .chars()
            .take(80)
            .collect();
        if !head.is_empty() {
            println!("      {}", dim(head));
        }
    }
    if history.len() > 1 {
        println!(
            "{}",
            dim(format!(
                "回滚：exm skill rollback {id} --to <版本号>（以旧内容写新版本行，审计链不断）"
            ))
        );
    }
    Ok(())
}

/// 技能回滚（capability-parity 6.5 用户撤销学习）：历史版本经同一校验
/// 成为新版本行；CLI/REST/聊天同链路。
pub fn run_skill_rollback(core: &Core, id: &str, to: u64) -> anyhow::Result<()> {
    let rolled = core.skill_rollback(id, to, "cli")?;
    println!(
        "{} 技能包 {id} 已回滚到 v{to}（新版本行 v{}，后续派发即生效）",
        ok("完成"),
        rolled.version
    );
    Ok(())
}

// ---------------------------------------------------------------- 上下文来源

/// 一条来源的可读状态行（CLI 呈现）。事实字段与 REST/通道同形：
/// `status` / `baseStatus` / `disabled` / `reason` / `chars` / `modifiedAt`。
fn context_line(s: &serde_json::Value) -> String {
    let key = s["key"].as_str().unwrap_or("?");
    let status = s["status"].as_str().unwrap_or("?");
    let layer = s["layer"]
        .as_str()
        .map(|l| format!("（{} 层）", context_layer_label(l)))
        .unwrap_or_default();
    let base = s["baseStatus"]
        .as_str()
        .filter(|b| *b != status)
        .map(|b| format!("，原状态 {b}"))
        .unwrap_or_default();
    let chars = s["chars"]
        .as_u64()
        .map(|c| format!("，{c} 字"))
        .unwrap_or_default();
    let modified = s["modifiedAt"]
        .as_u64()
        .filter(|t| *t > 0)
        .map(|t| format!("，修改于 {}（UTC）", context_utc(t)))
        .unwrap_or_default();
    let reason = s["reason"]
        .as_str()
        .map(|r| format!("：{r}"))
        .unwrap_or_default();
    format!(
        "[{}] {key}{layer} — {status}{base}{chars}{modified}{reason}",
        s["kind"].as_str().unwrap_or("?")
    )
}

/// Unix 秒 → `YYYY-MM-DD HH:MM`（UTC）。exm-cli 不依赖 chrono，也不读本地时区——
/// 用标准库换算公历日期，并**标明是 UTC**：显示一个时区错误的时间比显示 UTC 更糟。
/// 算法：Howard Hinnant 的 civil-from-days（days 从 1970-01-01 起，可为负）。
fn context_utc(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (h, m) = (rem / 3_600, (rem % 3_600) / 60);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };
    format!("{year:04}-{month:02}-{d:02} {h:02}:{m:02}")
}

/// 层级的外显名（API 契约里是 snake_case：project / user）在中文输出里的可读名。
fn context_layer_label(layer: &str) -> &'static str {
    match layer {
        "project" => "项目",
        "user" => "用户",
        "agent_prompt" => "智能体提示",
        "system_safety" => "系统安全",
        _ => "未知",
    }
}

/// 整层缺席说明的一行（Core 给出的原因原样呈现，入口不改写、不推断）。
pub(crate) fn context_gap_line(gap: &serde_json::Value) -> String {
    let layer = gap["layer"].as_str().unwrap_or("未知");
    let reason = gap["reason"].as_str().unwrap_or("（未给出原因）");
    format!("[{layer} 层] {reason}", layer = context_layer_label(layer))
}

/// `exm context <session> list`：四类来源清单 + 缺失原因 + 禁用位。
pub fn run_context_list(core: &Core, session_id: &str) -> anyhow::Result<()> {
    let sources = core.context_sources(session_id)?;
    // 整层缺席的文件来源及其原因：由 Core 给出事实（未绑定项目 / 项目目录不可用 /
    // 没有 ~/.exmachina）。清单里一个 file: 都没有时，这里是唯一的解释——
    // 不在入口自己猜「为什么没有」。
    let gaps = core.context_source_gaps(session_id)?;
    if sources.is_empty() {
        println!("{}", dim("当前会话没有可列出的上下文来源。"));
        for g in &gaps {
            println!("  {}", dim(context_gap_line(g)));
        }
        return Ok(());
    }
    println!(
        "{}",
        format!("会话 {session_id} 的上下文来源（{} 项）", sources.len()).bold()
    );
    for s in &sources {
        println!("  {}", context_line(s));
    }
    if !gaps.is_empty() {
        println!(
            "{}",
            dim("  未加载的文件来源层（说明，不是可操作的来源）：")
        );
        for g in &gaps {
            println!("    {}", dim(context_gap_line(g)));
        }
    }
    let disabled: Vec<&str> = sources
        .iter()
        .filter(|s| s["disabled"].as_bool() == Some(true))
        .filter_map(|s| s["key"].as_str())
        .collect();
    if !disabled.is_empty() {
        println!(
            "{}",
            warn(format!(
                "已停用自动注入：{}（停用只管自动注入；模型主动的记忆召回与用户显式 /skill <id> 不受影响）",
                disabled.join("、")
            ))
        );
    }
    println!(
        "{}",
        dim(format!(
            "操作：exm context {session_id} detail <key> | disable <key> | enable <key> | refresh [key]"
        ))
    );
    Ok(())
}

/// `exm context <session> detail <key>`。
pub fn run_context_detail(core: &Core, session_id: &str, key: &str) -> anyhow::Result<()> {
    let sources = core.context_sources(session_id)?;
    let Some(s) = sources
        .iter()
        .find(|s| s["key"].as_str() == Some(key.trim()))
    else {
        let keys: Vec<&str> = sources.iter().filter_map(|s| s["key"].as_str()).collect();
        anyhow::bail!("没有上下文来源 {key}。可用：{}", keys.join(" / "));
    };
    println!("{}", format!("来源 {key}：").bold());
    println!("  {}", context_line(s));
    if s["disabled"].as_bool() == Some(true) {
        println!(
            "  {}",
            warn("自动注入已停用（下一轮对话起不再注入）；exm context … enable 可恢复。模型主动的记忆召回与用户显式 /skill <id> 不受影响。")
        );
    } else {
        println!(
            "  {}",
            dim("自动注入开启中（下一轮对话按预算注入）；exm context … disable 可停用自动注入。")
        );
    }
    Ok(())
}

/// 单来源刷新并打印结果。返回是否成功（供整表刷新汇总；单来源时据此定退出码）。
fn context_refresh_one(core: &Core, session_id: &str, key: &str) -> anyhow::Result<bool> {
    let report = core.context_source_refresh(session_id, key)?;
    let others = report["others"].as_array().map(Vec::len).unwrap_or(0);
    let source = &report["source"];
    let is_file = source["kind"].as_str() == Some("file");
    let line = context_line(source);
    match (report["ok"].as_bool(), is_file) {
        (Some(false), _) => {
            println!(
                "  {} 重读来源 {key} 失败：{}\n    {line}\n    {}",
                err("失败"),
                report["error"].as_str().unwrap_or("未知原因"),
                dim(format!(
                    "其他 {others} 个来源未受影响（未重读、状态不变）；本来源的禁用设置也未改动"
                ))
            );
            Ok(false)
        }
        (Some(true), true) => {
            println!(
                "  {} 已重读来源 {key}\n    {line}\n    {}",
                ok("完成"),
                dim(format!("其他 {others} 个来源未重读、状态不变"))
            );
            Ok(true)
        }
        _ => {
            let ready = source["status"].as_str() == Some("ready");
            let verdict = if ready {
                "已就绪，按预算参与自动注入".to_string()
            } else {
                format!(
                    "未就绪：{}",
                    source["reason"].as_str().unwrap_or("原因未上报")
                )
            };
            println!(
                "  🔄 来源 {key} 没有可重读的文件（运行时来源），已重新观测：{verdict}\n    {line}\n    {}",
                dim(format!("其他 {others} 个来源未重读、状态不变"))
            );
            Ok(true)
        }
    }
}

/// `exm context <session> refresh [key]`。
///
/// 带一个 key：只刷新该来源；失败 → 非零退出（失败原因与「其余未受影响」都打印出来）。
/// 不带 key：逐个刷新全部来源；**一个失败不中断其余**，全部跑完后汇总，
/// 有失败则非零退出——「单来源刷新失败不会改变其他来源」在 CLI 上就是：
/// 其余来源照样被刷新、照样如实回报。
pub fn run_context_refresh(core: &Core, session_id: &str, key: Option<&str>) -> anyhow::Result<()> {
    let Some(key) = key.map(str::trim).filter(|k| !k.is_empty()) else {
        let sources = core.context_sources(session_id)?;
        if sources.is_empty() {
            println!("{}", dim("当前会话没有可刷新的上下文来源。"));
            return Ok(());
        }
        println!(
            "{}",
            format!("刷新会话 {session_id} 的 {} 个上下文来源：", sources.len()).bold()
        );
        let mut failed = Vec::new();
        for s in &sources {
            let k = s["key"].as_str().unwrap_or_default();
            if !context_refresh_one(core, session_id, k)? {
                failed.push(k.to_string());
            }
        }
        if !failed.is_empty() {
            anyhow::bail!(
                "{} 个来源刷新失败（{}）；其余 {} 个来源已刷新、未受影响",
                failed.len(),
                failed.join("、"),
                sources.len() - failed.len()
            );
        }
        return Ok(());
    };
    // 未知键是用法错误（不是刷新失败）：给出可用键
    let sources = core.context_sources(session_id)?;
    if !sources.iter().any(|s| s["key"].as_str() == Some(key)) {
        let keys: Vec<&str> = sources.iter().filter_map(|s| s["key"].as_str()).collect();
        anyhow::bail!("没有上下文来源 {key}，无从刷新。可用：{}", keys.join(" / "));
    }
    if context_refresh_one(core, session_id, key)? {
        Ok(())
    } else {
        anyhow::bail!("来源 {key} 刷新失败（其余来源未受影响）")
    }
}

/// `exm context <session> disable|enable <key>`。
pub fn run_context_set(
    core: &Core,
    session_id: &str,
    key: &str,
    disable: bool,
) -> anyhow::Result<()> {
    let disabled = core.context_source_set(session_id, key, disable)?;
    let verb = if disable { "已停用" } else { "已恢复" };
    let state = if disabled.is_empty() {
        "（当前没有停用的来源）".to_string()
    } else {
        format!("（已停用自动注入：{}）", disabled.join("、"))
    };
    println!(
        "{} {verb}来源 {} 的自动注入，下一轮对话起生效{state}。",
        ok("完成"),
        key.trim()
    );
    Ok(())
}

/// 会话训练轨迹导出：按轮次切分，默认剥离思维链，工具结果可控截断
pub fn run_trajectory(
    core: &Core,
    session_id: &str,
    out: &str,
    jsonl: bool,
    include_thinking: bool,
    max_tool_chars: usize,
) -> anyhow::Result<()> {
    let session = core
        .store
        .get_session(session_id)?
        .ok_or_else(|| anyhow::anyhow!("会话不存在: {session_id}"))?;
    let messages = core.store.list_messages(session_id, 0)?;
    let opts = exm_core::trajectory::TrajectoryOptions {
        include_thinking,
        include_tool_args: true,
        max_tool_chars: max_tool_chars.clamp(100, 20_000),
    };
    let trajectory = exm_core::trajectory::build_trajectory(&session, &messages, &opts);
    let body = if jsonl {
        exm_core::trajectory::trajectory_jsonl(&trajectory)
    } else {
        serde_json::to_string_pretty(&trajectory)?
    };
    if out.trim().is_empty() {
        println!("{body}");
    } else {
        std::fs::write(out, body)?;
        println!(
            "{} 已导出 {} 条消息到 {}",
            ok("完成"),
            messages.len(),
            out.cyan()
        );
    }
    Ok(())
}

// ---------------------------------------------------------------- 经验优化（子个体自适应）

pub async fn run_agent_optimize(core: &Core, identifier: &str) -> anyhow::Result<()> {
    println!(
        "{}",
        dim(format!("基于历史教训合成经验改进要点（{}）…", identifier))
    );
    let a = core.orchestrator().optimize_agent(identifier).await?;
    println!(
        "{} {} 已更新至 rev{}（教训计入 {} 条）",
        ok("完成"),
        identifier.cyan(),
        a.revision,
        a.lessons_seen
    );
    println!("{}", a.content);
    println!(
        "{}",
        dim("要点将常驻注入该个体的后续派发；agent reset-adaptation 可重置。")
    );
    Ok(())
}

pub fn run_agent_adaptation(core: &Core, identifier: &str) -> anyhow::Result<()> {
    match core.registry().load_adaptation(identifier)? {
        Some(a) => {
            println!(
                "{} 经验改进要点 rev{}（更新于 {}，教训计入 {} 条）",
                identifier.cyan(),
                a.revision,
                dim(&a.updated_at),
                a.lessons_seen
            );
            if let Some(b) = &a.basis {
                println!("  {} {}", dim("依据"), b);
            }
            println!("{}", a.content);
            if !a.previous.is_empty() {
                println!(
                    "  {} {:?}",
                    dim("历史版本"),
                    a.previous.iter().map(|v| v.revision).collect::<Vec<_>>()
                );
            }
        }
        None => println!(
            "{}",
            dim("该个体尚无经验要点；agent optimize <id> 可手动合成。")
        ),
    }
    Ok(())
}

pub fn run_agent_reset_adaptation(core: &Core, identifier: &str) -> anyhow::Result<()> {
    if core.registry().reset_adaptation(identifier)? {
        println!("{} 经验要点已重置：{identifier}", ok("完成"));
    } else {
        anyhow::bail!("该个体尚无经验要点");
    }
    Ok(())
}

// ---------------------------------------------------------------- 模型档案（多厂商）

fn masked(key: &str) -> String {
    if key.trim().is_empty() {
        "（空 = Mock 通道）".to_string()
    } else {
        "已配置".to_string()
    }
}

pub fn run_model_list(core: &Core) -> anyhow::Result<()> {
    let cfg = core.config();
    println!(
        "{}",
        format!("模型档案（生效：{}）", cfg.active_profile.cyan()).bold()
    );
    for p in &cfg.llm_profiles {
        let mark = if p.id == cfg.active_profile {
            "●".green().to_string()
        } else {
            " ".into()
        };
        println!(
            "{} {} {} {}",
            mark,
            p.id.cyan(),
            p.name.bold(),
            dim(format!("{}｜{}", p.base_url, p.model))
        );
        let key_count = p
            .api_keys
            .len()
            .max(if p.api_key.is_empty() { 0 } else { 1 });
        println!(
            "    {}",
            dim(format!(
                "apiKey: {}｜Key 池 {} 把",
                masked(&p.api_key),
                key_count
            ))
        );
    }
    let configured = !cfg.llm.api_key.trim().is_empty() || !cfg.llm.api_keys.is_empty();
    println!(
        "{}",
        dim(format!(
            "通道：{}",
            if cfg.use_mock {
                "测试替身（EXM_LLM_MOCK=1）"
            } else if !configured {
                "未配置 —— 请为档案填写 API Key"
            } else {
                "真实推理"
            }
        ))
    );
    Ok(())
}

pub fn run_model_add(
    core: &Core,
    id: &str,
    name: &str,
    base_url: &str,
    api_key: &str,
    model: &str,
) -> anyhow::Result<()> {
    use exm_core::config::LlmProfile;
    let mut cfg = (*core.config()).clone();
    let profile = LlmProfile {
        fallback: None,
        id: id.trim().to_string(),
        name: name.trim().to_string(),
        base_url: base_url.trim().to_string(),
        api_key: api_key.trim().to_string(),
        api_keys: Vec::new(),
        api_format: String::new(),
        client_identity: cfg
            .llm_profiles
            .iter()
            .find(|p| p.id == id.trim())
            .map(|p| p.client_identity.clone())
            .unwrap_or_default(),
        model: model.trim().to_string(),
        models: Vec::new(),
    };
    if let Some(slot) = cfg.llm_profiles.iter_mut().find(|p| p.id == profile.id) {
        *slot = profile.clone();
    } else {
        cfg.llm_profiles.push(profile.clone());
    }
    if cfg.active_profile == profile.id {
        cfg.llm = exm_core::config::LlmConfig {
            base_url: profile.base_url,
            api_key: profile.api_key,
            api_keys: Vec::new(),
            api_format: profile.api_format.clone(),
            client_identity: profile.client_identity.clone(),
            model: profile.model,
        };
    }
    cfg.use_mock = cfg.use_mock || exm_core::config::mock_enabled_from_env();
    core.apply_config(cfg)?;
    println!("{} 模型档案已保存：{}", ok("完成"), id.cyan());
    Ok(())
}

pub fn run_model_use(core: &Core, id: &str) -> anyhow::Result<()> {
    let mut cfg = (*core.config()).clone();
    let Some(p) = cfg.llm_profiles.iter().find(|p| p.id == id).cloned() else {
        anyhow::bail!("档案不存在: {id}");
    };
    cfg.active_profile = p.id.clone();
    let mut keys = p.api_keys.clone();
    if keys.is_empty() && !p.api_key.trim().is_empty() {
        keys.push(p.api_key.clone());
    }
    cfg.llm = exm_core::config::LlmConfig {
        base_url: p.base_url,
        api_key: p.api_key,
        api_keys: keys,
        api_format: p.api_format.clone(),
        client_identity: p.client_identity.clone(),
        model: p.model,
    };
    cfg.use_mock = cfg.use_mock || exm_core::config::mock_enabled_from_env();
    let base = cfg.llm.base_url.clone();
    core.apply_config(cfg)?;
    println!(
        "{} 已切换到档案 {}（{}）",
        ok("完成"),
        id.cyan(),
        dim(&base)
    );
    Ok(())
}

pub fn run_model_remove(core: &Core, id: &str) -> anyhow::Result<()> {
    let mut cfg = (*core.config()).clone();
    if cfg.llm_profiles.len() <= 1 {
        anyhow::bail!("至少保留一个模型档案");
    }
    let Some(pos) = cfg.llm_profiles.iter().position(|p| p.id == id) else {
        anyhow::bail!("档案不存在: {id}");
    };
    cfg.llm_profiles.remove(pos);
    if cfg.active_profile == id {
        let first = cfg.llm_profiles[0].clone();
        cfg.active_profile = first.id.clone();
        let mut keys = first.api_keys.clone();
        if keys.is_empty() && !first.api_key.trim().is_empty() {
            keys.push(first.api_key.clone());
        }
        cfg.llm = exm_core::config::LlmConfig {
            base_url: first.base_url.clone(),
            api_key: first.api_key.clone(),
            api_keys: keys,
            api_format: first.api_format.clone(),
            client_identity: first.client_identity.clone(),
            model: first.model.clone(),
        };
    }
    cfg.use_mock = cfg.use_mock || exm_core::config::mock_enabled_from_env();
    core.apply_config(cfg)?;
    println!("{} 模型档案已删除：{id}", ok("完成"));
    Ok(())
}

pub async fn run_model_test(core: &Core, id: Option<&str>) -> anyhow::Result<()> {
    let cfg = core.config();
    let profile = match id {
        Some(x) => cfg.llm_profiles.iter().find(|p| p.id == x).cloned(),
        None => cfg
            .llm_profiles
            .iter()
            .find(|p| p.id == cfg.active_profile)
            .cloned(),
    };
    let Some(p) = profile else {
        anyhow::bail!("档案不存在");
    };
    println!("{}", dim(format!("测试 {}（{}）…", p.id, p.base_url)));
    use exm_core::provider::LlmProvider;
    let mut keys = p.api_keys.clone();
    if keys.is_empty() && !p.api_key.trim().is_empty() {
        keys.push(p.api_key.clone());
    }
    let provider: std::sync::Arc<dyn LlmProvider> = std::sync::Arc::new(
        exm_core::provider::OpenAiCompatibleProvider::with_format(
            p.base_url.clone(),
            keys,
            &p.api_format,
        )
        .with_client_identity(&p.client_identity)?,
    );
    let req = exm_core::provider::ChatRequest::new(
        p.model.clone(),
        vec![exm_core::provider::ChatMessage::user("ping".to_string())],
    );
    match tokio::time::timeout(std::time::Duration::from_secs(15), provider.chat(req)).await {
        Ok(Ok(resp)) => println!(
            "{} 连通正常：模型 {} 返回 {} 字符",
            ok("完成"),
            p.model,
            resp.content.chars().count()
        ),
        Ok(Err(e)) => println!("{} 连通失败：{e}", err("失败")),
        Err(_) => println!("{} 连接超时（15s）", err("失败")),
    }
    Ok(())
}

// ---------------------------------------------------------------- 定时任务

pub fn run_cron_list(core: &Core) -> anyhow::Result<()> {
    let jobs = core.cron.list()?;
    if jobs.is_empty() {
        println!("{}", dim("（无定时任务；exm cron add <名称> --prompt <提示词> --cron \"分 时 日 月 周\" 创建；连续任务用 --kind queue --steps id1,id2）"));
        return Ok(());
    }
    println!("{}", "定时任务（exm gateway 常驻时自动调度）".bold());
    for j in jobs {
        let state = if j.enabled {
            "启用".green().to_string()
        } else {
            "停用".dimmed().to_string()
        };
        // 任务三类型（v0.4.0）：cron 周期 / at 一次性 / queue 连续；缺省按 cron/at 推断
        let kind = j
            .kind
            .as_deref()
            .unwrap_or(if j.cron.is_some() { "cron" } else { "at" });
        let schedule = match kind {
            "queue" => format!("连续[{}]", j.steps.join(" → ")),
            _ => match (&j.cron, &j.at) {
                (Some(c), _) => format!("cron[{c}]"),
                (None, Some(a)) => format!("一次性[{a}]"),
                _ => "—".into(),
            },
        };
        println!(
            "  {} {} {} {} {}",
            j.id.cyan(),
            j.name.bold(),
            state,
            schedule,
            dim(format!(
                "上次: {}",
                j.last_run_at.clone().unwrap_or_else(|| "未运行".into())
            ))
        );
        println!(
            "    {}",
            dim(format!(
                "组: {}｜会话: {}",
                j.group.as_deref().unwrap_or("(激活组)"),
                j.session_title.as_deref().unwrap_or("job-<id>")
            ))
        );
        println!("    {}", j.prompt.chars().take(100).collect::<String>());
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub fn run_cron_add(
    core: &Core,
    name: &str,
    prompt: &str,
    cron: Option<String>,
    at: Option<String>,
    group: Option<String>,
    session_title: Option<String>,
    kind: Option<String>,
    target_session: Option<String>,
    steps: Vec<String>,
) -> anyhow::Result<()> {
    // 任务三类型（v0.4.0）：cron 周期 / at 一次性 / queue 连续（按步骤 id 顺序执行）
    let is_queue = matches!(kind.as_deref(), Some("queue"));
    if !is_queue && cron.is_none() && at.is_none() {
        anyhow::bail!("需要 --cron \"分 时 日 月 周\" 或 --at <ISO8601 时间>（连续任务用 --kind queue --steps）");
    }
    let steps: Vec<String> = steps
        .into_iter()
        .flat_map(|s| {
            s.split(',')
                .map(|x| x.trim().to_string())
                .collect::<Vec<_>>()
        })
        .filter(|s| !s.is_empty())
        .collect();
    if is_queue && steps.is_empty() {
        anyhow::bail!("连续任务（--kind queue）需要至少一个步骤任务 id：--steps id1,id2");
    }
    let job = core.cron.upsert(CronJob {
        id: String::new(),
        name: name.trim().to_string(),
        prompt: prompt.trim().to_string(),
        cron: if is_queue { None } else { cron },
        at: if is_queue { None } else { at },
        kind: kind.or_else(|| {
            Some(if is_queue {
                "queue".into()
            } else {
                "cron".into()
            })
        }),
        target_session,
        steps,
        group,
        session_title,
        enabled: true,
        last_run_at: None,
        last_status: None,
        last_run_minute: None,
        notify_channels: Vec::new(),
        created_at: String::new(),
    })?;
    println!(
        "{} 定时任务已创建：{}（{}）",
        ok("完成"),
        job.name,
        job.id.cyan()
    );
    println!(
        "{}",
        dim("exm gateway 常驻时由网关调度器自动触发；exm cron run <id> 可手动执行。")
    );
    Ok(())
}

pub fn run_cron_enable(core: &Core, id: &str, enabled: bool) -> anyhow::Result<()> {
    let mut job = core
        .cron
        .get(id)?
        .ok_or_else(|| anyhow::anyhow!("任务不存在: {id}"))?;
    job.enabled = enabled;
    core.cron.upsert(job)?;
    println!(
        "{} 任务 {id} 已{}",
        ok("完成"),
        if enabled { "启用" } else { "停用" }
    );
    Ok(())
}

pub fn run_cron_remove(core: &Core, id: &str) -> anyhow::Result<()> {
    if core.cron.remove(id)? {
        println!("{} 定时任务已删除：{id}", ok("完成"));
    } else {
        anyhow::bail!("任务不存在: {id}");
    }
    Ok(())
}

pub async fn run_cron_now(core: &Core, id: &str) -> anyhow::Result<()> {
    let job = core
        .cron
        .get(id)?
        .ok_or_else(|| anyhow::anyhow!("任务不存在: {id}"))?;
    println!("{}", dim(format!("执行任务 {}（{}）…", job.name, job.id)));
    let run = core.run_cron_job(&job).await;
    println!(
        "{} 状态 {}｜会话 {}｜{}",
        ok("完成"),
        run.status,
        run.session_id.cyan(),
        dim(&run.summary)
    );
    Ok(())
}

pub fn run_cron_runs(core: &Core, job: Option<&str>, limit: usize) -> anyhow::Result<()> {
    let runs = core.cron.runs(job, limit)?;
    if runs.is_empty() {
        println!("{}", dim("（无运行记录）"));
        return Ok(());
    }
    for r in runs {
        println!(
            "  {} {} {} {}",
            r.started_at.dimmed(),
            r.job_name.bold(),
            if r.status == "done" {
                "done".green().to_string()
            } else {
                "failed".red().to_string()
            },
            dim(&r.summary)
        );
    }
    Ok(())
}

// ---------------------------------------------------------------- 执行审批

pub fn run_approval_list(core: &Core, status: Option<&str>, limit: usize) -> anyhow::Result<()> {
    let items = core.approval_list(status, limit)?;
    if items.is_empty() {
        println!("{}", dim("（无审批单）"));
        return Ok(());
    }
    for r in items {
        print_approval(&r);
    }
    Ok(())
}

fn print_approval(r: &ApprovalRequest) {
    let status = match r.status.as_str() {
        "pending" => "待审批".yellow().to_string(),
        "approved" => "已批准".green().to_string(),
        "denied" => "已拒绝".red().to_string(),
        "executed" => "已执行".green().to_string(),
        "failed" => "执行失败".red().to_string(),
        other => other.to_string(),
    };
    println!(
        "  {} {} {} {}",
        r.id.cyan(),
        status,
        dim(format!("个体 {}", r.agent_id)),
        dim(&r.created_at)
    );
    println!("    $ {}", r.command);
    if let Some(out) = &r.result {
        println!(
            "    {}",
            dim(out.replace('\n', " ").chars().take(200).collect::<String>())
        );
    }
}

pub async fn run_approval_decide(core: &Core, id: &str, approve: bool) -> anyhow::Result<()> {
    let r = core.approval_decide(id, approve).await?;
    match r.status.as_str() {
        "denied" => println!("{} 审批单 {id} 已拒绝", ok("完成")),
        "executed" => {
            println!("{} 审批单 {id} 已批准并代执行", ok("完成"));
            if let Some(out) = &r.result {
                println!(
                    "{}",
                    dim(format!(
                        "输出：{}",
                        out.chars().take(600).collect::<String>()
                    ))
                );
            }
        }
        other => println!("{} 审批单 {id} 状态：{other}", ok("完成")),
    }
    Ok(())
}

#[cfg(test)]
mod context_time_tests {
    use super::context_utc;

    /// 换算钉在已知时刻上（取值来自公历事实，不取自本函数自身）。
    #[test]
    fn unix秒_转_utc_已知时刻() {
        assert_eq!(context_utc(0), "1970-01-01 00:00");
        // 2024-02-29 00:00:00 UTC（闰日）
        assert_eq!(context_utc(1_709_164_800), "2024-02-29 00:00");
        // 2024-03-01 00:00:00 UTC（闰日之后）
        assert_eq!(context_utc(1_709_251_200), "2024-03-01 00:00");
        // 2000-02-29 12:34:56 UTC（能被 400 整除的闰年）
        assert_eq!(context_utc(951_827_696), "2000-02-29 12:34");
        // 1999-12-31 23:59:59 → 2000-01-01 00:00:00（跨年边界）
        assert_eq!(context_utc(946_684_799), "1999-12-31 23:59");
        assert_eq!(context_utc(946_684_800), "2000-01-01 00:00");
        // 2100 能被 100 整除、不被 400 整除 → 平年：2 月只有 28 天
        assert_eq!(context_utc(4_107_456_000), "2100-02-28 00:00");
        assert_eq!(context_utc(4_107_542_400), "2100-03-01 00:00");
        // 2400 能被 400 整除 → 闰年
        assert_eq!(context_utc(13_574_563_200), "2400-02-29 00:00");
        // 近期时刻
        assert_eq!(context_utc(1_795_499_940), "2026-11-24 05:59");
    }
}
