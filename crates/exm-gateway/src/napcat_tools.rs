//! napcat 会话工具（napcat__*）：群文件检索 / 群文件下载 / 出站文件发送。
//!
//! 背景（部署实测缺陷）：「把这个群文件下载下来解压私发给我」整链不可达——
//! 模型工具面只有通用 read/terminal，够不到 QQ 群文件（需 NapCat 扩展 API）；
//! 也没有「发文件」入口。本工具组补齐三段：
//! ① napcat__group_files：列群文件（根目录 / 指定文件夹）
//! ② napcat__group_file：经 get_group_file_url 取直链下载到工作区 downloads/
//!   （read/terminal 可后续加工：解压、转格式、抽取内容……）
//! ③ napcat__send_file：把工作区文件登记进会话发件箱（outbox），
//!   轮次收束时 spawn_reply → upload_group/private_file 真发（napcat 发文件
//!   走 base64:// 段，跨容器无需共享文件系统）
//!
//! 注入口径：napcat 通道会话经 inject_session_tools 绑定（会话级来源收窄），
//! 与 video__* 观看工具组同一机制；API 回包经 napcat::api_call（echo 等待表）。

use crate::napcat::api_call;
use crate::platform::Channel;
use exm_core::config::CustomTool;
use exm_core::tools::{dynamic_exec, DynamicToolExec};
use exm_core::Core;
use std::sync::Arc;

/// 会话工具组 schema（三件套）
pub(crate) fn tool_specs() -> Vec<CustomTool> {
    vec![
        CustomTool {
            name: "napcat__group_files".into(),
            description: "列出 QQ 群文件（根目录或指定文件夹）。返回文件名/大小/时间的清单与 file_id，配合 napcat__group_file 下载。".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "folderId": { "type": "string", "description": "文件夹 id（可选，缺省列根目录）" }
                }
            }),
            ..Default::default()
        },
        CustomTool {
            name: "napcat__group_file".into(),
            description: "下载 QQ 群文件到工作区 downloads/ 目录，返回保存路径（可用 read/terminal 加工：解压、转格式、读取内容）。先用 napcat__group_files 拿 fileId。".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "fileId": { "type": "string", "description": "群文件 id（group_files 返回的 file_id）" },
                    "busid": { "type": "integer", "description": "文件 busid（group_files 返回里有；未知填 0 走默认 102）" },
                    "name": { "type": "string", "description": "保存文件名（可选，缺省用群文件原名）" }
                },
                "required": ["fileId"]
            }),
            ..Default::default()
        },
        CustomTool {
            name: "napcat__send_file".into(),
            description: "把工作区文件发送给用户（群内直发；私聊场景发给对话者）。文件须已在工作区（如 downloads/ 下的加工产物）。返回即已投递，无需在回复里复述文件内容。".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "工作区相对路径（如 downloads/result.zip）" },
                    "caption": { "type": "string", "description": "文件说明（可选，随文件附注）" }
                },
                "required": ["path"]
            }),
            ..Default::default()
        },
    ]
}

/// 构建注入集（napcat 通道会话用；闭包绑定通道快照、会话流键与工具名）。
/// `chat_key` 为流键（QQ 群号），群文件 API 的目标群从这里来——群聊会话
/// 群号即 chat_key；私聊会话为 None（群文件工具如实报不可用）。
pub(crate) fn build_injections(
    core: &Arc<Core>,
    ch: &Channel,
    chat_key: &str,
) -> Vec<(CustomTool, DynamicToolExec)> {
    let group_id = chat_key.parse::<i64>().ok();
    tool_specs()
        .into_iter()
        .map(|spec| {
            let exec = dynamic_exec({
                let core = core.clone();
                let ch = ch.clone();
                let name = spec.name.clone();
                move |_agent: &str, session: &str, args: serde_json::Value| {
                    let core = core.clone();
                    let ch = ch.clone();
                    let name = name.clone();
                    let session = session.to_string();
                    async move { dispatch(&core, &ch, &session, &name, group_id, args).await }
                }
            });
            (spec, exec)
        })
        .collect()
}

/// 分发执行：按工具名路由；错误统一 anyhow → 工具结果 ok=false（模型可读）
async fn dispatch(
    core: &Core,
    ch: &Channel,
    session: &str,
    name: &str,
    group_id: Option<i64>,
    args: serde_json::Value,
) -> anyhow::Result<String> {
    match name {
        "napcat__group_files" => group_files(ch, group_id, args).await,
        "napcat__group_file" => group_file(core, ch, group_id, args).await,
        "napcat__send_file" => send_file(core, session, args),
        _ => anyhow::bail!("未知 napcat 工具: {name}"),
    }
}

/// 列群文件：根目录 get_group_root_files / 文件夹 get_group_files_by_folder。
/// 输出精简清单（文件名 + 可读大小 + file_id），模型可直接引用。
async fn group_files(
    ch: &Channel,
    group_id: Option<i64>,
    args: serde_json::Value,
) -> anyhow::Result<String> {
    let Some(gid) = group_id else {
        anyhow::bail!("私聊会话无群文件；请在群聊中使用该工具");
    };
    let folder = args
        .get("folderId")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let (action, params) = match folder {
        Some(fid) => (
            "get_group_files_by_folder",
            serde_json::json!({ "group_id": gid, "folder_id": fid }),
        ),
        None => ("get_group_root_files", serde_json::json!({ "group_id": gid })),
    };
    let resp = api_call(&ch.id, action, params)
        .await
        .ok_or_else(|| anyhow::anyhow!("群文件列表失败：NapCat 无响应或不支持该 API"))?;
    let data = resp.get("data").cloned().unwrap_or(serde_json::Value::Null);
    let mut lines: Vec<String> = Vec::new();
    for f in data
        .get("files")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default()
    {
        let name = f.get("file_name").and_then(|x| x.as_str()).unwrap_or("?");
        let size = f
            .get("file_size")
            .and_then(|x| x.as_i64())
            .or_else(|| f.get("size").and_then(|x| x.as_i64()))
            .unwrap_or(0);
        let fid = f.get("file_id").and_then(|x| x.as_str()).unwrap_or("");
        let busid = f.get("busid").and_then(|x| x.as_i64()).unwrap_or(102);
        if fid.is_empty() {
            continue;
        }
        lines.push(format!(
            "- {name}（{}，fileId={fid}，busid={busid}）",
            human_size(size)
        ));
    }
    // 子文件夹也列出（嵌套目录可逐层进）
    for d in data
        .get("folders")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default()
    {
        let name = d.get("folder_name").and_then(|x| x.as_str()).unwrap_or("?");
        let did = d.get("folder_id").and_then(|x| x.as_str()).unwrap_or("");
        if !did.is_empty() {
            lines.push(format!("- 📁 {name}（folderId={did}）"));
        }
    }
    if lines.is_empty() {
        return Ok("群文件为空（或该目录无文件）。".into());
    }
    Ok(format!(
        "群文件 {} 条：\n{}",
        lines.len(),
        lines.join("\n")
    ))
}

/// 下载群文件：get_group_file_url 取直链 → reqwest 下载 → 工作区 downloads/ 落盘。
/// 返回相对路径（模型用相对路径接 read/terminal）。
async fn group_file(
    core: &Core,
    ch: &Channel,
    group_id: Option<i64>,
    args: serde_json::Value,
) -> anyhow::Result<String> {
    let Some(gid) = group_id else {
        anyhow::bail!("私聊会话无群文件；请在群聊中使用该工具");
    };
    let fid = args
        .get("fileId")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("缺少 fileId（先用 napcat__group_files 查询）"))?;
    let busid = args.get("busid").and_then(|x| x.as_i64()).unwrap_or(102);
    let resp = api_call(
        &ch.id,
        "get_group_file_url",
        serde_json::json!({ "group_id": gid, "file_id": fid, "busid": busid }),
    )
    .await
    .ok_or_else(|| anyhow::anyhow!("获取下载直链失败：NapCat 无响应"))?;
    let url = resp
        .pointer("/data/url")
        .and_then(|x| x.as_str())
        .ok_or_else(|| anyhow::anyhow!("直链为空（文件可能过期或已删除）"))?;
    let bytes = crate::platform::download_bytes(url)
        .await
        .ok_or_else(|| anyhow::anyhow!("下载失败（直链可能已过期，请重试）"))?;
    if bytes.is_empty() {
        anyhow::bail!("下载内容为空");
    }
    // 文件名：参数指定 > URL 提示 > 兜底
    let name = args
        .get("name")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("qqfile-{}.bin", exm_core::types::now_ms()));
    let safe: String = sanitize_name(&name);
    let dir = core.config().workspace_root.join("downloads");
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| anyhow::anyhow!("创建 downloads/ 失败：{e}"))?;
    let path = dir.join(&safe);
    tokio::fs::write(&path, &bytes)
        .await
        .map_err(|e| anyhow::anyhow!("写入失败：{e}"))?;
    let rel = format!("downloads/{safe}");
    Ok(format!(
        "已下载 {} → {rel}（工作区相对路径，可用 read/terminal 加工；发回给用户用 napcat__send_file）",
        human_size(bytes.len() as i64)
    ))
}

/// 发送工作区文件：登记会话发件箱（kind=file），spawn_reply 在收束时投递。
/// 同步返回——outbox 是落库即生效，真正投递随本轮 run.finished artifacts。
fn send_file(core: &Core, session: &str, args: serde_json::Value) -> anyhow::Result<String> {
    let rel = args
        .get("path")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("缺少 path（工作区相对路径）"))?;
    // 路径安全：词法消解后禁上跳与绝对路径（文件面同口径，越界即拒）
    let p = std::path::Path::new(rel);
    anyhow::ensure!(!p.is_absolute(), "路径须为工作区相对路径");
    let norm_path = lexical_norm(p);
    for comp in norm_path.components() {
        anyhow::ensure!(
            comp != std::path::Component::ParentDir,
            "路径不允许 ..（越界拒绝）"
        );
    }
    let norm = norm_path.to_string_lossy().replace('\\', "/");
    let abs = core.config().workspace_root.join(&norm);
    let meta = std::fs::metadata(&abs).map_err(|_| anyhow::anyhow!("文件不存在：{norm}"))?;
    if meta.is_dir() {
        anyhow::bail!("{norm} 是目录，只支持发送文件");
    }
    let caption = args
        .get("caption")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    core.store
        .outbox_push(session, "file", &norm, &caption)
        .map_err(|e| anyhow::anyhow!("发件箱登记失败：{e}"))?;
    Ok(format!(
        "已投递 {norm}（{}）——文件将随本轮结束自动发送。",
        human_size(meta.len() as i64)
    ))
}

// ---------------------------------------------------------------- 小件

/// 词法规范化（不触盘）：消解 `.` 与 `..`
fn lexical_norm(p: &std::path::Path) -> std::path::PathBuf {
    let mut out = std::path::PathBuf::new();
    for comp in p.components() {
        match comp {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn human_size(n: i64) -> String {
    if n < 1024 {
        format!("{n}B")
    } else if n < 1024 * 1024 {
        format!("{:.1}KB", n as f64 / 1024.0)
    } else if n < 1024 * 1024 * 1024 {
        format!("{:.1}MB", n as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.2}GB", n as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}

fn sanitize_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || "._- ()（）".contains(c) && c != '/' && c != '\\' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() || trimmed == "." || trimmed == ".." {
        format!("qqfile-{}.bin", exm_core::types::now_ms())
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 文件名清洗_路径符替换() {
        assert_eq!(sanitize_name("a/b.txt"), "a_b.txt");
        assert_eq!(sanitize_name("../../etc/passwd"), ".._.._etc_passwd");
        assert!(!sanitize_name("小爱人格提示词.docx").is_empty(), "中文名保留");
        assert_eq!(
            sanitize_name(".."),
            format!("qqfile-{}.bin", exm_core::types::now_ms()),
            "裸点号换兜底名"
        );
    }

    #[test]
    fn 人类可读大小() {
        assert_eq!(human_size(512), "512B");
        assert_eq!(human_size(17391), "17.0KB");
        assert_eq!(human_size(2 * 1024 * 1024), "2.0MB");
    }
}
