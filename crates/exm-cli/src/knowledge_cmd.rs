//! `exm knowledge` —— 共享知识库的查看、检索、沉淀与治理（knowledge-base）。
//!
//! 与 `exm memory` 的分工：记忆存本组经历（分区隔离），知识存实例级
//! 通用客观结论（全局共享）。

use crate::render::*;
use exm_core::Core;
use exm_core::knowledge::KnowledgeDraft;
use owo_colors::OwoColorize;

pub fn run_knowledge_list(core: &Core, limit: usize) -> anyhow::Result<()> {
    let entries = core.knowledge.recent(limit);
    if entries.is_empty() {
        println!("{}", dim("（知识库为空；`exm knowledge add` 或模型 knowledge_write 后写入）"));
        return Ok(());
    }
    println!("{}", format!("共享知识库最近 {} 条（createdAt 倒序）", entries.len()).bold());
    for e in entries {
        println!(
            "  {} {}",
            e.title.bold(),
            dim(format!("hits={}", e.hits))
        );
        println!(
            "   {}",
            e.body.replace('\n', " ").chars().take(160).collect::<String>()
        );
        println!(
            "   {}",
            dim(format!(
                "id={}  来源={}  tags={}  {}",
                e.id,
                e.source_agent,
                e.tags.join(","),
                e.updated_at
            ))
        );
    }
    Ok(())
}

pub fn run_knowledge_search(core: &Core, query: &str, limit: usize) -> anyhow::Result<()> {
    let hits = core.knowledge.search(query, limit)?;
    if hits.is_empty() {
        println!("{}", dim("（无命中）"));
        return Ok(());
    }
    println!("{}", format!("检索「{query}」命中 {} 条", hits.len()).bold());
    for e in hits {
        println!("  {}（hits={}）", e.title.bold(), e.hits);
        println!(
            "     {}",
            e.body.replace('\n', " ").chars().take(200).collect::<String>()
        );
        println!("     {}", dim(format!("id={}  来源={}", e.id, e.source_agent)));
    }
    Ok(())
}

pub fn run_knowledge_add(core: &Core, title: &str, body: &str, tags: Vec<String>) -> anyhow::Result<()> {
    let draft = KnowledgeDraft {
        title: title.to_string(),
        body: body.to_string(),
        tags,
        source_agent: "manual".into(),
        source_session: None,
    };
    let outcome = core.knowledge.write(&draft)?;
    let e = outcome.entry();
    let status = if outcome.deduped() {
        "已存在（去重提升）"
    } else {
        "新建"
    };
    println!("{} {}（{status}，id={}）", ok("已写入"), e.title, e.id);
    Ok(())
}

pub fn run_knowledge_delete(core: &Core, id: &str) -> anyhow::Result<()> {
    match core.knowledge.delete(id) {
        Ok(true) => {
            println!("{} {}", ok("已删除"), id);
            Ok(())
        }
        Ok(false) => anyhow::bail!("知识条目不存在：{id}"),
        Err(e) => Err(e),
    }
}
