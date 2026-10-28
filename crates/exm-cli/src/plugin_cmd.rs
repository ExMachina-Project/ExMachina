//! `exm plugin` —— 插件管理命令族：list / install / update / uninstall。
//!
//! 与 doctor 同款直开 core 模式（进程内装配，不经网关）；
//! 安装/更新走 exm-core 统一流水线（plugin/install.rs）。

use crate::render::*;
use exm_core::plugin::install::{self, IndexStatus, InstallSource};
use exm_core::Core;
use owo_colors::OwoColorize;
use std::path::Path;

/// 插件管理入口（dispatch 内直 await——外层已在 tokio runtime）
pub async fn run_plugin_action(workspace: &Path, action: PluginAction) -> anyhow::Result<()> {
    let core = Core::create(workspace)?;
    match action {
        PluginAction::List => list(&core).await,
        PluginAction::Install { source, update } => install_cmd(&core, &source, update).await,
        PluginAction::Update { id } => update_cmd(&core, id.as_deref()).await,
        PluginAction::Uninstall { id } => uninstall_cmd(&core, &id),
    }
}

/// 已装插件 + 索引对照（未装/可更新标记；索引拉取失败降级为仅本地列表）
async fn list(core: &Core) -> anyhow::Result<()> {
    let pm = core.plugins();
    let cfg = core.config();
    let host = exm_core::plugin::host_version_pub();
    // 索引（失败不阻塞本地列表）
    let idx = install::fetch_index(&install::effective_index_url(&cfg.plugins.index_url))
        .await
        .ok();
    let installed: std::collections::HashMap<String, (String, bool)> = pm
        .list()
        .into_iter()
        .map(|e| (e.manifest.id.clone(), (e.manifest.version.clone(), e.enabled)))
        .collect();

    println!("{}", "已安装插件".on_blue().white());
    if installed.is_empty() {
        println!("{}", dim("（无——从下方目录安装，或把插件目录放入 plugins/）"));
    }
    let mut rows: Vec<_> = pm.list().into_iter().collect();
    rows.sort_by(|a, b| a.manifest.id.cmp(&b.manifest.id));
    for e in rows {
        let state = if !e.enabled {
            warn("已禁用")
        } else if e.error.is_some() {
            err("异常")
        } else {
            ok("启用")
        };
        println!(
            "{} {} v{} [{}] {}",
            state,
            e.manifest.id.bold(),
            if e.manifest.version.is_empty() { "-" } else { &e.manifest.version },
            dim(&e.form),
            e.manifest.name
        );
        if let Some(err_msg) = &e.error {
            println!("    {} {}", err("↳"), err_msg);
        }
    }

    if let Some(entries) = idx {
        let compared = install::compare_index(&entries, &|id| installed.get(id).cloned(), &host);
        let catalog: Vec<_> = compared
            .into_iter()
            .filter(|(_, s)| !matches!(s, IndexStatus::Installed(_)))
            .collect();
        if !catalog.is_empty() {
            println!();
            println!("{}", "索引目录（可安装）".on_blue().white());
            for (e, status) in catalog {
                let mark = match status {
                    IndexStatus::Available => ok("可安装").to_string(),
                    IndexStatus::Updatable(_, new_v) => warn(format!("可更新 → v{new_v}")).to_string(),
                    IndexStatus::Incompatible(need) => dim(format!("需宿主 ≥ {need}")).to_string(),
                    IndexStatus::Installed(_) => unreachable!(),
                };
                println!("{} {} v{} {}", mark, e.id.bold(), e.version, dim(&e.name));
                if !e.description.is_empty() {
                    println!("    {}", dim(&e.description));
                }
            }
        }
    } else {
        println!();
        println!("{}", dim("（插件索引拉取失败——仅显示本地列表；可用 config plugins.indexUrl 自定义源）"));
    }
    Ok(())
}

/// 安装：索引 id 或直传 URL（tar.gz / git）
async fn install_cmd(core: &Core, source: &str, update: bool) -> anyhow::Result<()> {
    let src = resolve_source(core, source).await?;
    let out = core.plugin_install(&src, update).await?;
    println!(
        "{} 插件 {} v{} 已{}（声明式能力即时生效；进程插件在下次调用时拉起）",
        ok("✓"),
        out.id.bold(),
        out.version,
        if out.updated { "覆盖安装" } else { "安装" }
    );
    Ok(())
}

/// 更新：指定 id 或全量（有来源记录的逐一更新）
async fn update_cmd(core: &Core, id: Option<&str>) -> anyhow::Result<()> {
    match id {
        Some(id) => {
            let (updated, msg) = core.plugin_update(id).await?;
            println!("{} {}", if updated { ok("✓") } else { dim("·") }, msg);
        }
        None => {
            let ids: Vec<String> = {
                let cfg = core.config();
                cfg.plugins.installations.keys().cloned().collect()
            };
            if ids.is_empty() {
                println!("{}", dim("（无可更新插件——无安装来源记录）"));
                return Ok(());
            }
            for id in ids {
                let (updated, msg) = core.plugin_update(&id).await?;
                println!("{} {}", if updated { ok("✓") } else { dim("·") }, msg);
            }
        }
    }
    Ok(())
}

fn uninstall_cmd(core: &Core, id: &str) -> anyhow::Result<()> {
    core.plugin_uninstall(id)?;
    println!("{} 插件 {} 已卸载（目录与来源/凭据记录已清理）", ok("✓"), id.bold());
    Ok(())
}

/// 来源解析：URL 直判（git/tar.gz）；否则按索引 id 查
async fn resolve_source(core: &Core, input: &str) -> anyhow::Result<InstallSource> {
    let s = input.trim();
    if s.contains("://") || s.ends_with(".git") || s.starts_with("git@") || std::path::Path::new(s).is_file() {
        return Ok(install::parse_source_url(s));
    }
    // 索引 id
    let cfg = core.config();
    let entries = install::fetch_index(&install::effective_index_url(&cfg.plugins.index_url))
        .await
        .map_err(anyhow::Error::msg)?;
    let hit = entries
        .iter()
        .find(|e| e.id == s)
        .ok_or_else(|| anyhow::anyhow!("索引中未找到插件 {s}（或直传 tar.gz / git URL 安装）"))?;
    let host = exm_core::plugin::host_version_pub();
    if !hit.exm_min.trim().is_empty() && !exm_core::plugin::version_newer(&host, &hit.exm_min) {
        anyhow::bail!("插件 {} 需要宿主 ≥ {}，当前 {}", hit.id, hit.exm_min, host);
    }
    Ok(hit.to_install_source())
}

// ---------------------------------------------------------------- clap 定义

use clap::Subcommand;

/// 插件管理（list / install / update / uninstall）
#[derive(Subcommand)]
pub enum PluginAction {
    /// 列出已装插件与索引目录（含可更新标记）
    List,
    /// 安装插件：索引 id / tar.gz URL / git URL（本地 .tgz 路径亦可）
    Install {
        /// 索引 id 或包/仓库地址
        source: String,
        /// 已存在时覆盖（= 更新语义，保 data/）
        #[arg(long)]
        update: bool,
    },
    /// 更新插件（省略 id = 全量更新有来源记录的插件）
    Update {
        /// 插件 id（缺省全量）
        id: Option<String>,
    },
    /// 卸载插件（builtin/virtual 拒绝；目录与记录一并清理）
    Uninstall {
        /// 插件 id
        id: String,
    },
}
