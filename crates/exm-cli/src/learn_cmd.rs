//! `exm learn` —— 学习候选的查看与人工裁决（capability-parity 6.4/6.6）。
//!
//! 与 REST `GET/POST /api/learning/candidates`、通道 `/learn` 共用同一 Core 函数
//! （`learning_candidates` / `learning_decide`）：候选账本、转正写入、版本事实源
//! 都只有一条，这里只是它的 CLI 呈现。

use crate::render::*;
use exm_core::learning_candidates::Promotion;
use exm_core::Core;
use owo_colors::OwoColorize;

pub fn run_learn_list(core: &Core, status: Option<&str>, limit: usize) -> anyhow::Result<()> {
    let list = core.learning_candidates(status)?;
    if list.is_empty() {
        println!(
            "{}",
            dim("（无学习候选；智能体任务后凭真实证据自动提交，见 /learn 或学习候选 API）")
        );
        return Ok(());
    }
    println!(
        "{}",
        format!("学习候选（{} 项，最新在前）", list.len()).bold()
    );
    for c in list.iter().take(limit) {
        let status_word = match c.status.as_str() {
            "pending" => "待裁决".yellow().to_string(),
            "approved" => "已批准".green().to_string(),
            "rejected" => "已拒绝".red().to_string(),
            other => other.to_string(),
        };
        let evidence: Vec<String> = c
            .evidence
            .iter()
            .map(|e| format!("{}·{}", e.kind.key(), e.verification))
            .collect();
        println!(
            "  {} {} {} {}",
            c.id.cyan(),
            status_word,
            format!("{}→{}", c.target, c.scope).bold(),
            dim(format!("置信 {:.0}%", c.confidence * 100.0))
        );
        println!("    {}", c.title);
        println!(
            "    {}",
            dim(format!(
                "证据 {} 条：{}；触发词 {}；来源会话 {}",
                c.evidence.len(),
                evidence.join("、"),
                c.triggers.join(","),
                c.source_session
            ))
        );
        // 转正落点（已批准的）：写进了哪条版本链、哪个版本——与通道回执同一口径
        if let Some(p) = &c.promotion {
            if p.ok {
                let v = p
                    .version
                    .map(|n| format!(" v{n}"))
                    .unwrap_or_else(String::new);
                println!("    {}", ok(format!("已转正 → {}{v}", p.ref_id)));
            } else {
                println!(
                    "    {}",
                    warn(format!(
                        "转正失败：{}",
                        p.error.as_deref().unwrap_or("未知原因")
                    ))
                );
            }
        }
    }
    Ok(())
}

pub fn run_learn_decide(
    core: &Core,
    id: &str,
    approve: bool,
    reason: Option<&str>,
) -> anyhow::Result<()> {
    let c = core.learning_decide(id, approve, "cli", reason)?;
    if approve {
        match &c.promotion {
            Some(p) if !p.ok => {
                // 批准 ≠ 生效：转正失败如实报（裁决保留，失败留痕）
                anyhow::bail!(
                    "候选 {id} 已批准，但转正失败：{}（裁决保留，可经版本链人工写入）",
                    p.error.as_deref().unwrap_or("未知原因")
                );
            }
            // 落点 id 与**实际生效**版本号都取自 Core 的转正回执（与通道/REST 同一事实源）；
            // 版本号不在这里推断——续接既有版本链时不是 v1。
            Some(p) => println!("{} {}", ok("完成"), promoted_line(p)),
            None => println!("{} 已批准（转正由技能/记忆写入链承接）", ok("完成")),
        }
    } else {
        println!(
            "{} 候选 {id} 已拒绝{}",
            ok("完成"),
            reason.map(|r| format!("：{r}")).unwrap_or_default()
        );
    }
    Ok(())
}

/// 转正成功回执正文（不含「完成」前缀与颜色）：`已批准并转正 → <落点> v<版本>`。
/// 纯函数——CLI 的回执文本因此可直接被测试断言，不必捕获 stdout。
pub fn promoted_line(p: &Promotion) -> String {
    let v = p.version.map(|n| format!(" v{n}")).unwrap_or_default();
    format!("已批准并转正 → {}{v}", p.ref_id)
}
