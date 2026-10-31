//! tools/files.rs —— 自 tools.rs 按领域拆出（纯搬移，零行为变更）

use super::*;

// ---------------------------------------------------------------- 自由函数

/// 去掉 Windows canonicalize 产生的 `\\?\` 前缀（便于跨路径比较）
fn strip_verbatim(p: PathBuf) -> PathBuf {
    let s = p.to_string_lossy().to_string();
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        PathBuf::from(rest)
    } else {
        p
    }
}

/// 词法规范化：消解 `.` 与 `..`（不解析符号链接）
fn normalize_lexically(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
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

/// 尽可能规范化：路径存在则 canonicalize；不存在时向父目录回溯，
/// 拼接规范化后的祖先 + 剩余部分（词法消解 `..`）——保证越界判定用的是真实路径。
fn canonicalize_best(p: &Path) -> PathBuf {
    if let Ok(c) = p.canonicalize() {
        return strip_verbatim(c);
    }
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    let mut cur = p.to_path_buf();
    loop {
        if let Ok(base) = cur.canonicalize() {
            let mut joined = strip_verbatim(base);
            for t in tail.iter().rev() {
                joined.push(t);
            }
            return normalize_lexically(&joined);
        }
        let Some(name) = cur.file_name().map(|s| s.to_os_string()) else {
            return normalize_lexically(p);
        };
        tail.push(name);
        if !cur.pop() {
            return normalize_lexically(p);
        }
    }
}

/// 人类可读大小
fn human_size(n: u64) -> String {
    if n < 1024 {
        format!("{n} B")
    } else if n < 1024 * 1024 {
        format!("{:.1} KB", n as f64 / 1024.0)
    } else {
        format!("{:.1} MB", n as f64 / 1048576.0)
    }
}

/// 工作台树遍历（spec: webui-code-workbench）：相对根收集节点，目录优先排序。
/// 排除 .git/target/node_modules 与隐藏目录（.gitignore 的文件面过滤在 build_walker；
/// 工作台树要的是稳定可浏览结构，重目录手工排除即可）
fn walk_tree(
    root: &Path,
    dir: &Path,
    depth: usize,
    depth_limit: usize,
    nodes: &mut Vec<serde_json::Value>,
    count: &mut usize,
    max_entries: usize,
) -> anyhow::Result<()> {
    if depth > depth_limit || *count >= max_entries {
        return Ok(());
    }
    let mut entries: Vec<(String, bool, u64)> = std::fs::read_dir(dir)?
        .flatten()
        .map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            let meta = e.metadata().ok();
            let is_dir = meta.as_ref().map(|m| m.is_dir()).unwrap_or(false);
            let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
            (name, is_dir, size)
        })
        .collect();
    entries.sort_by(|a, b| {
        b.1.cmp(&a.1)
            .then_with(|| a.0.to_lowercase().cmp(&b.0.to_lowercase()))
    });
    for (name, is_dir, size) in entries {
        if *count >= max_entries {
            return Ok(());
        }
        if name.starts_with('.') && name != ".github" && name != ".gitignore" {
            continue;
        }
        if name == ".git" || name == "target" || name == "node_modules" {
            continue;
        }
        let child = dir.join(&name);
        let rel = child
            .strip_prefix(root)
            .unwrap_or(&child)
            .to_string_lossy()
            .to_string();
        *count += 1;
        if is_dir {
            nodes.push(serde_json::json!({
                "path": rel, "name": name, "type": "dir", "children": [],
            }));
            walk_tree(
                root,
                &child,
                depth + 1,
                depth_limit,
                nodes,
                count,
                max_entries,
            )?;
        } else {
            nodes.push(serde_json::json!({
                "path": rel, "name": name, "type": "file", "size": size,
            }));
        }
    }
    Ok(())
}

/// 构造遍历器：尊重 .gitignore / .ignore（隐藏文件也纳入，便于读 .github 等目录）；
/// glob 非空时只保留匹配项（`*.rs` 这类裸模式自动补 `**/` 前缀）。
fn build_walker(root: &Path, glob: &str) -> ignore::Walk {
    let mut builder = ignore::WalkBuilder::new(root);
    builder
        .hidden(false)
        .git_ignore(true)
        .git_global(true)
        .max_filesize(Some(4 * 1024 * 1024));
    if !glob.trim().is_empty() {
        let mut ov = ignore::overrides::OverrideBuilder::new(root);
        let pat = glob.trim();
        let normalized = if pat.contains('/') || pat.starts_with("**") {
            pat.to_string()
        } else {
            format!("**/{pat}")
        };
        if ov.add(&normalized).is_ok() {
            if let Ok(set) = ov.build() {
                builder.overrides(set);
            }
        }
    }
    builder.build()
}

impl ToolGateway {
    /// 路径安全解析：规范化后必须落在「组工作区」或「全局工作区」之内。
    /// 不存在的路径按「最近存在的父目录」规范化后再判定 —— 否则 `..` 拼接可绕过越界检查。
    pub(crate) fn resolve_safe(&self, raw: &str) -> anyhow::Result<PathBuf> {
        let raw_trim = raw.trim();
        if raw_trim.is_empty() {
            bail!("路径不能为空");
        }
        let group_root = self.effective_root();
        let candidate = if Path::new(raw_trim).is_absolute() {
            PathBuf::from(raw_trim)
        } else {
            group_root.join(raw_trim)
        };
        let group_canon = canonicalize_best(&group_root);
        let ws_canon = canonicalize_best(&self.workspace_root);
        let cand_canon = canonicalize_best(&candidate);
        if !cand_canon.starts_with(&group_canon) && !cand_canon.starts_with(&ws_canon) {
            bail!("路径越界（仅允许工作区内的路径）: {raw}");
        }
        Ok(candidate)
    }

    /// 写前快照（检查点）：原文件复制到 .exmachina/checkpoints/<日期>/<相对路径>。
    /// 同日同名已存在则跳过 —— 保存的是「当日首次改动前」的版本，供 `exm checkpoint restore` 回滚。
    /// `session_id` 非空时把检查点引用（date/rel）登记进该会话的轮次链（undo 联动回滚用）。
    pub(crate) fn checkpoint(&self, target: &Path, session_id: &str) -> anyhow::Result<()> {
        if !target.is_file() {
            return Ok(());
        }
        let root = self.effective_root();
        let Ok(rel) = target.strip_prefix(&root) else {
            return Ok(()); // 组工作区在工作区外：不做快照（无回滚能力，但也不阻断写入）
        };
        let day: String = crate::types::now_iso().chars().take(10).collect();
        let dst = self
            .workspace_root
            .join(".exmachina")
            .join("checkpoints")
            .join(&day)
            .join(rel);
        let fresh = !dst.exists();
        if fresh {
            if let Some(parent) = dst.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(target, &dst).context("写检查点失败")?;
        }
        // 轮次检查点链：仅登记当日首次快照（重复跳过的文件其既有检查点已可回滚）
        if fresh && !session_id.is_empty() {
            let rel_str = rel.to_string_lossy().replace('\\', "/");
            let _ = self.store.record_checkpoint_ref(session_id, &day, &rel_str);
        }
        Ok(())
    }

    /// 读取文件或目录：文件按行带行号返回（offset/limit 分页），目录返回条目清单
    pub(crate) fn tool_read(&self, args: &serde_json::Value) -> ToolResult {
        let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
        let offset = args
            .get("offset")
            .and_then(|v| v.as_u64())
            .unwrap_or(1)
            .max(1) as usize;
        let limit = args
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(400)
            .clamp(1, 5000) as usize;
        // maxChars 与 limit 同口径设上限：模型传超大值（1e9）不应绕过单次读取的预算
        // （默认 24000，硬上限 100000；再大的内容请分段读）
        let max_chars = args
            .get("maxChars")
            .and_then(|v| v.as_u64())
            .unwrap_or(24_000)
            .clamp(1, 100_000) as usize;
        let p = match self.resolve_safe(path) {
            Ok(p) => p,
            Err(e) => return ToolResult::err(e.to_string()),
        };
        if p.is_dir() {
            return self.list_dir(&p);
        }
        // 图像副产物（spec: agent-terminal-tools read 图像副产物）：
        // 按扩展名识别图像，返回摘要 + images 路径（runtime 视觉模型回灌）
        let ext = p
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .unwrap_or_default();
        if matches!(
            ext.as_str(),
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp"
        ) {
            let meta = match std::fs::metadata(&p) {
                Ok(m) => m,
                Err(e) => return ToolResult::err(format!("读取失败: {e}")),
            };
            let abs = std::fs::canonicalize(&p)
                .map(|x| x.display().to_string())
                .unwrap_or_else(|_| p.display().to_string());
            return ToolResult::ok(format!(
                "图像文件：{path}（{ext}，{} 字节）—— 已作为图像输入回灌，具备视觉能力的模型可直接查看",
                meta.len()
            ))
            .with_images(vec![abs]);
        }
        let bytes = match std::fs::read(&p) {
            Ok(b) => b,
            Err(e) => return ToolResult::err(format!("读取失败: {e}")),
        };
        let text = match String::from_utf8(bytes) {
            Ok(t) => t,
            Err(e) => {
                return ToolResult::err(format!(
                    "不是 UTF-8 文本文件（{} 字节），无法按行读取；二进制处理请用 terminal",
                    e.as_bytes().len()
                ))
            }
        };
        let lines: Vec<&str> = text.lines().collect();
        let total = lines.len();
        if total == 0 {
            return ToolResult::ok(format!("（空文件，0 行）{path}"));
        }
        let start = (offset - 1).min(total);
        // 越界 offset 给出可行动的提示而非「已显示第 无 行」：模型下一跳就能自己纠正
        if start >= total {
            return ToolResult::ok(format!(
                "{path}：共 {total} 行，offset={offset} 已超出文件末尾；请用 offset≤{total}"
            ));
        }
        let mut out = String::new();
        let mut used = 0usize;
        let mut end = start;
        for (i, line) in lines.iter().enumerate().skip(start) {
            if i - start >= limit {
                break;
            }
            let rendered = format!("{:>5}→{}\n", i + 1, line);
            if used + rendered.chars().count() > max_chars {
                if i == start {
                    out.push_str(&format!(
                        "{:>5}→{}\n",
                        i + 1,
                        line.chars().take(600).collect::<String>()
                    ));
                    end = i + 1;
                }
                break;
            }
            used += rendered.chars().count();
            out.push_str(&rendered);
            end = i + 1;
        }
        let shown = if end > start {
            format!("{}–{}", start + 1, end)
        } else {
            "无".to_string()
        };
        out.push_str(&format!("\n（{path}：共 {total} 行；已显示第 {shown} 行）"));
        if end < total {
            out.push_str(&format!("；续读 offset={}", end + 1));
        }
        ToolResult::ok(out)
    }

    /// 目录清单：目录在前，带大小与条目总数
    pub(crate) fn list_dir(&self, p: &Path) -> ToolResult {
        let entries = match std::fs::read_dir(p) {
            Ok(e) => e,
            Err(e) => return ToolResult::err(format!("列目录失败: {e}")),
        };
        let mut rows: Vec<(String, bool, u64)> = Vec::new();
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            let meta = e.metadata().ok();
            let is_dir = meta.as_ref().map(|m| m.is_dir()).unwrap_or(false);
            let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
            rows.push((name, is_dir, size));
        }
        rows.sort_by(|a, b| {
            b.1.cmp(&a.1)
                .then_with(|| a.0.to_lowercase().cmp(&b.0.to_lowercase()))
        });
        let total = rows.len();
        let body = rows
            .iter()
            .take(200)
            .map(|(n, d, s)| {
                if *d {
                    format!("[dir]  {n}/")
                } else {
                    format!("[file] {n}  ({})", human_size(*s))
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        if body.is_empty() {
            return ToolResult::ok("（空目录）");
        }
        ToolResult::ok(format!(
            "{body}\n（共 {total} 项，已列 {}）",
            total.min(200)
        ))
    }

    /// 精确编辑：old_string → new_string（唯一命中校验；支持 replace_all）
    pub(crate) fn tool_edit(&self, session_id: &str, args: &serde_json::Value) -> ToolResult {
        let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
        let old = args
            .get("old_string")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let new = args
            .get("new_string")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let replace_all = args
            .get("replace_all")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if old.is_empty() {
            return ToolResult::err("old_string 不能为空；新建文件请用 filesystem write");
        }
        if old == new {
            return ToolResult::err("old_string 与 new_string 完全相同，无需编辑");
        }
        let p = match self.resolve_safe(path) {
            Ok(p) => p,
            Err(e) => return ToolResult::err(e.to_string()),
        };
        if !p.is_file() {
            return ToolResult::err(format!("文件不存在或不是普通文件: {path}"));
        }
        let content = match std::fs::read_to_string(&p) {
            Ok(c) => c,
            Err(e) => return ToolResult::err(format!("读取失败: {e}")),
        };
        let hits: Vec<usize> = content
            .match_indices(old)
            .map(|(i, _)| content[..i].matches('\n').count() + 1)
            .collect();
        if hits.is_empty() {
            return ToolResult::err(format!(
                "未找到匹配文本（{path}）。old_string 需与文件内容逐字符一致（含缩进与换行）——建议先用 read 读取该段再复制。"
            ));
        }
        if hits.len() > 1 && !replace_all {
            let at: Vec<String> = hits.iter().take(8).map(|l| l.to_string()).collect();
            return ToolResult::err(format!(
                "old_string 命中 {} 处（行 {}），无法判断改哪一处：请扩大上下文使其唯一，或置 replace_all=true 全部替换。",
                hits.len(),
                at.join(", ")
            ));
        }
        let (updated, count) = if replace_all {
            (content.replace(old, new), hits.len())
        } else {
            (content.replacen(old, new, 1), 1)
        };
        if let Err(e) = self.checkpoint(&p, session_id) {
            return ToolResult::err(format!("写前检查点失败，已中止编辑：{e}"));
        }
        if let Err(e) = std::fs::write(&p, &updated) {
            return ToolResult::err(format!("写入失败: {e}"));
        }
        let delta = updated.lines().count() as i64 - content.lines().count() as i64;
        let mut msg = format!("已替换 {count} 处（{path}，首处位于第 {} 行）", hits[0]);
        if delta != 0 {
            msg.push_str(&format!("；行数 {delta:+}"));
        }
        ToolResult::ok(msg)
    }

    /// 统一 diff 补丁：逐文件在内存中校验并计算新内容，任一文件失败则整体拒绝；
    /// 全部通过后统一落检查点再写入（多文件原子语义）
    pub(crate) fn tool_patch(&self, session_id: &str, args: &serde_json::Value) -> ToolResult {
        let patch_text = args.get("patch").and_then(|v| v.as_str()).unwrap_or("");
        if patch_text.trim().is_empty() {
            return ToolResult::err("patch 不能为空：请提供统一 diff 格式的补丁全文");
        }
        let file_patches = match crate::patch::parse_unified_diff(patch_text) {
            Ok(p) => p,
            Err(e) => return ToolResult::err(format!("补丁解析失败: {e}")),
        };

        struct Plan {
            path: PathBuf,
            rel: String,
            content: String,
            delete: bool,
            added: usize,
            removed: usize,
        }
        let mut plans: Vec<Plan> = Vec::new();
        let mut failures: Vec<String> = Vec::new();

        for fp in &file_patches {
            let p = match self.resolve_safe(&fp.path) {
                Ok(p) => p,
                Err(e) => {
                    failures.push(format!("{}: {e}", fp.path));
                    continue;
                }
            };
            let old = if fp.new_file {
                if p.is_file() {
                    failures.push(format!("{}: 标记为新文件但已存在", fp.path));
                    continue;
                }
                String::new()
            } else {
                match std::fs::read_to_string(&p) {
                    Ok(c) => c,
                    Err(e) => {
                        failures.push(format!(
                            "{}: 读取失败（二进制文件不支持补丁）: {e}",
                            fp.path
                        ));
                        continue;
                    }
                }
            };
            match crate::patch::apply_file(&old, fp) {
                Ok(new) => {
                    let added = fp
                        .hunks
                        .iter()
                        .flat_map(|h| h.lines.iter())
                        .filter(|l| l.kind == crate::patch::LineKind::Add)
                        .count();
                    let removed = fp
                        .hunks
                        .iter()
                        .flat_map(|h| h.lines.iter())
                        .filter(|l| l.kind == crate::patch::LineKind::Del)
                        .count();
                    plans.push(Plan {
                        path: p,
                        rel: fp.path.clone(),
                        content: new,
                        delete: fp.delete_file,
                        added,
                        removed,
                    });
                }
                Err(e) => failures.push(e),
            }
        }

        // 整体拒绝：报告全部冲突，不落任何写入
        if !failures.is_empty() {
            let mut msg = format!(
                "补丁整体拒绝：{} 个文件中 {} 个校验失败，未写入任何文件。\n失败详情：",
                file_patches.len(),
                failures.len()
            );
            for f in &failures {
                msg.push_str(&format!("\n- {f}"));
            }
            msg.push_str("\n请用 read 读取目标文件后修正补丁上下文再重试。");
            return ToolResult::err(msg);
        }

        // 全部通过：检查点 → 写入（/删除）
        for plan in &plans {
            if let Err(e) = self.checkpoint(&plan.path, session_id) {
                return ToolResult::err(format!("写前检查点失败，已中止补丁应用：{e}"));
            }
            if plan.delete {
                if let Err(e) = std::fs::remove_file(&plan.path) {
                    return ToolResult::err(format!("删除文件失败（{}）: {e}", plan.rel));
                }
            } else if let Some(parent) = plan.path.parent() {
                if let Err(e) = std::fs::create_dir_all(parent) {
                    return ToolResult::err(format!("创建目录失败（{}）: {e}", plan.rel));
                }
                if let Err(e) = std::fs::write(&plan.path, &plan.content) {
                    return ToolResult::err(format!("写入失败（{}）: {e}", plan.rel));
                }
            }
        }
        let detail: Vec<String> = plans
            .iter()
            .map(|p| {
                if p.delete {
                    format!("{}（删除）", p.rel)
                } else {
                    format!("{}（+{}/-{}）", p.rel, p.added, p.removed)
                }
            })
            .collect();
        ToolResult::ok(format!(
            "补丁已应用 {} 个文件：\n- {}",
            plans.len(),
            detail.join("\n- ")
        ))
    }

    /// 仓库检索：正则搜索文件内容（尊重 .gitignore），返回 文件:行号:内容
    pub(crate) fn tool_grep(&self, args: &serde_json::Value) -> ToolResult {
        let pattern = args.get("pattern").and_then(|v| v.as_str()).unwrap_or("");
        if pattern.trim().is_empty() {
            return ToolResult::err("pattern 不能为空");
        }
        let sub = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
        let glob = args
            .get("glob")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        let ic = args
            .get("ignoreCase")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let max_results = args
            .get("maxResults")
            .and_then(|v| v.as_u64())
            .unwrap_or(100)
            .clamp(1, 500) as usize;
        // 上下文行数（spec: agent-terminal-tools grep 上下文行）：0 = 现状单行输出
        let ctx_n = args
            .get("contextLines")
            .and_then(|v| v.as_u64())
            .unwrap_or(0)
            .clamp(0, 5) as usize;
        let root = match self.resolve_safe(sub) {
            Ok(p) => p,
            Err(e) => return ToolResult::err(e.to_string()),
        };
        let re = match regex::RegexBuilder::new(pattern)
            .case_insensitive(ic)
            .build()
        {
            Ok(r) => r,
            Err(e) => return ToolResult::err(format!("正则无效: {e}")),
        };
        let walker = build_walker(&root, &glob);
        let base = self.effective_root();
        let mut out: Vec<String> = Vec::new();
        let mut hit_files = 0usize;
        let mut truncated = false;
        let mut hit_lines: Vec<(usize, String)> = Vec::new();
        'files: for entry in walker {
            let Ok(entry) = entry else { continue };
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let Ok(meta) = entry.metadata() else { continue };
            if meta.len() > 2 * 1024 * 1024 {
                continue; // 跳过 >2MB 的文件
            }
            let Ok(text) = std::fs::read_to_string(path) else {
                continue;
            };
            let rel = path
                .strip_prefix(&base)
                .unwrap_or(path)
                .display()
                .to_string()
                .replace('\\', "/");
            let mut file_hit = false;
            hit_lines.clear();
            for (i, line) in text.lines().enumerate() {
                if re.is_match(line) {
                    if !file_hit {
                        file_hit = true;
                        hit_files += 1;
                    }
                    // contextLines>0 时记录行号，稍后按块合并输出（含前后上下文行）
                    if ctx_n > 0 {
                        hit_lines.push((i, line.to_string()));
                        continue;
                    }
                    let trimmed: String = line.trim().chars().take(240).collect();
                    out.push(format!("{rel}:{}:{}", i + 1, trimmed));
                    if out.len() >= max_results {
                        truncated = true;
                        break 'files;
                    }
                }
            }
            // 上下文块输出：相邻命中（间隔 ≤ 2N+1）合并为一块，
            // 块内逐行 `rel:行号: 内容`，命中行内容保持 240 字符截断
            if ctx_n > 0 && !hit_lines.is_empty() {
                let all_lines: Vec<&str> = text.lines().collect();
                let mut blocks: Vec<(usize, usize)> = Vec::new(); // [start, end] 行号闭区间
                for (i, _) in &hit_lines {
                    let s = i.saturating_sub(ctx_n);
                    let e = (i + ctx_n).min(all_lines.len().saturating_sub(1));
                    match blocks.last_mut() {
                        Some((bs, be)) if s <= *be + 1 => {
                            if e > *be {
                                *be = e;
                            }
                        }
                        _ => blocks.push((s, e)),
                    }
                }
                for (s, e) in blocks {
                    if !out.is_empty() {
                        out.push(format!("{rel}—"));
                    }
                    for ln in s..=e {
                        if ln >= all_lines.len() {
                            break;
                        }
                        let content = &all_lines[ln];
                        let mark = if re.is_match(content) { ">" } else { " " };
                        let trimmed: String = content.trim().chars().take(240).collect();
                        out.push(format!("{rel}:{}:{} {}", ln + 1, mark, trimmed));
                    }
                    if out.len() >= max_results {
                        truncated = true;
                        break 'files;
                    }
                }
            }
        }
        if out.is_empty() {
            return ToolResult::ok(format!("无匹配（pattern: {pattern}）"));
        }
        let mut body = out.join("\n");
        body.push_str(&format!(
            "\n（{} 条命中，涉及 {hit_files} 个文件{}）",
            out.len(),
            if truncated {
                "，已达结果上限——请收窄 pattern/path"
            } else {
                ""
            }
        ));
        ToolResult::ok(body)
    }

    /// 按 glob 模式列出文件路径
    pub(crate) fn tool_glob(&self, args: &serde_json::Value) -> ToolResult {
        let pattern = args
            .get("pattern")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if pattern.is_empty() {
            return ToolResult::err("pattern 不能为空（如 **/*.rs）");
        }
        let sub = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
        let root = match self.resolve_safe(sub) {
            Ok(p) => p,
            Err(e) => return ToolResult::err(e.to_string()),
        };
        let walker = build_walker(&root, &pattern);
        let base = self.effective_root();
        let mut files: Vec<String> = Vec::new();
        for entry in walker {
            let Ok(entry) = entry else { continue };
            if entry.path().is_file() {
                let rel = entry
                    .path()
                    .strip_prefix(&base)
                    .unwrap_or(entry.path())
                    .display()
                    .to_string()
                    .replace('\\', "/");
                files.push(rel);
                if files.len() >= 500 {
                    break;
                }
            }
        }
        files.sort();
        if files.is_empty() {
            return ToolResult::ok(format!("无匹配文件（pattern: {pattern}）"));
        }
        let n = files.len();
        ToolResult::ok(format!("{}\n（{n} 个文件）", files.join("\n")))
    }

    /// 递归复制目录树/文件（回收站跨设备 fallback 用）：
    /// 目标已存在则清空重建，保证「移入」语义的原子性近似
    pub(crate) fn copy_tree(src: &Path, dst: &Path) -> anyhow::Result<()> {
        use std::fs;
        if src.is_dir() {
            if dst.exists() {
                // 目标可能是文件或目录，分别清理
                if dst.is_dir() {
                    fs::remove_dir_all(dst).context("回收站旧目标清理失败")?;
                } else {
                    fs::remove_file(dst).context("回收站旧目标清理失败")?;
                }
            }
            fs::create_dir_all(dst).context("回收站目录创建失败")?;
            for entry in fs::read_dir(src).context("读取源目录失败")? {
                let entry = entry?;
                let ty = entry.file_type()?;
                let child_dst = dst.join(entry.file_name());
                if ty.is_dir() {
                    fs::create_dir_all(&child_dst)?;
                    Self::copy_tree(&entry.path(), &child_dst)?;
                } else {
                    fs::copy(entry.path(), &child_dst).context("复制文件失败")?;
                }
            }
            Ok(())
        } else {
            if let Some(parent) = dst.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(src, dst).context("复制文件失败").map(|_| ())
        }
    }

    pub(crate) fn tool_fs(&self, session_id: &str, args: &serde_json::Value) -> ToolResult {
        let op = args.get("op").and_then(|v| v.as_str()).unwrap_or("write");
        let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
        let target = match self.resolve_safe(path) {
            Ok(p) => p,
            Err(e) => return ToolResult::err(e.to_string()),
        };
        match op {
            "write" => {
                let content = args.get("content").and_then(|v| v.as_str()).unwrap_or("");
                if let Some(parent) = target.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                if let Err(e) = self.checkpoint(&target, session_id) {
                    return ToolResult::err(format!("写前检查点失败，已中止写入：{e}"));
                }
                match std::fs::write(&target, content).context("写入失败") {
                    Ok(_) => {
                        // 产物登记（D12）：写入投递目录 `.exmachina/outbox/` 的文件 → 会话 outbox
                        let norm = target
                            .strip_prefix(&self.workspace_root)
                            .map(|p| p.display().to_string().replace('\\', "/"))
                            .unwrap_or_default();
                        if norm.starts_with(".exmachina/outbox/") {
                            let _ = self.store.outbox_push(session_id, "file", &norm, "");
                        }
                        ToolResult::ok(format!(
                            "written: {}（{} 行 / {} 字符）",
                            target.display(),
                            content.lines().count(),
                            content.chars().count()
                        ))
                    }
                    Err(e) => ToolResult::err(e.to_string()),
                }
            }
            "mkdir" => match std::fs::create_dir_all(&target) {
                Ok(_) => ToolResult::ok(format!("mkdir: {}", target.display())),
                Err(e) => ToolResult::err(e.to_string()),
            },
            "list" => {
                if target.is_dir() {
                    self.list_dir(&target)
                } else {
                    ToolResult::err(format!("不是目录: {path}"))
                }
            }
            // 删除进回收站（spec: filesystem-ops）：不裸删——整树移入
            // .exmachina/trash/<日期>/<相对路径>，保留原内容供人工恢复
            "delete" => {
                let root = self.effective_root();
                // 保护区：工作区根 / .exmachina 系统区（含检查点与回收站自身）
                if target == root {
                    return ToolResult::err("拒绝删除：工作区根目录不可删除");
                }
                let sys = self.workspace_root.join(".exmachina");
                if target.starts_with(&sys) {
                    return ToolResult::err(
                        "拒绝删除：.exmachina 系统区（检查点/回收站/沙箱等）不可经此删除；确需清理请用终端并过审批",
                    );
                }
                if !target.exists() {
                    return ToolResult::err(format!("目标不存在: {path}"));
                }
                let Ok(rel) = target.strip_prefix(&root) else {
                    return ToolResult::err("拒绝删除：目标不在当前工作区内");
                };
                let day: String = crate::types::now_iso().chars().take(10).collect();
                let mut dst = sys.join("trash").join(&day).join(rel);
                // 同日同名冲突：追加序号（build.log → build.log.2）
                let mut seq = 2u32;
                while dst.exists() {
                    let name = format!(
                        "{}.{seq}",
                        rel.file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_default()
                    );
                    dst = sys.join("trash").join(&day).join(rel.with_file_name(name));
                    seq += 1;
                }
                if let Some(parent) = dst.parent() {
                    if let Err(e) = std::fs::create_dir_all(parent) {
                        return ToolResult::err(format!("回收站目录创建失败：{e}"));
                    }
                }
                let rel_disp = dst
                    .strip_prefix(&self.workspace_root)
                    .map(|p| p.display().to_string().replace('\\', "/"))
                    .unwrap_or_else(|_| dst.display().to_string());
                // fs::rename 跨设备（EXDEV：回收站常为独立挂载卷）时 fallback copy+删源。
                // 逐条目复制保序（目录先建、文件后拷），任一失败即中止（残留部分文件不影响原位数据）
                let moved = std::fs::rename(&target, &dst)
                    .or_else(|_| Self::copy_tree(&target, &dst))
                    .and_then(|_| {
                        // rename 成功时源已不在；copy 路径源仍在，删源完成「移入」语义
                        if target.exists() {
                            if target.is_dir() {
                                std::fs::remove_dir_all(&target)
                            } else {
                                std::fs::remove_file(&target)
                            }
                            .context("回收站已写入但删源失败（原位数据未动）")
                        } else {
                            Ok(())
                        }
                    });
                match moved {
                    Ok(_) => ToolResult::ok(format!(
                        "已删除（移入回收站，可人工恢复）：{path} → {rel_disp}"
                    )),
                    Err(e) => ToolResult::err(format!("删除失败（回收站移入）: {e}")),
                }
            }
            // 重命名/移动（spec: filesystem-ops）：不覆盖 + 源检查点（undo 链不断）
            "rename" => {
                let new_path = args.get("newPath").and_then(|v| v.as_str()).unwrap_or("");
                if new_path.trim().is_empty() {
                    return ToolResult::err("op=rename 需要 newPath（相对工作区的目标路径）");
                }
                let new_target = match self.resolve_safe(new_path) {
                    Ok(p) => p,
                    Err(e) => return ToolResult::err(e.to_string()),
                };
                if !target.exists() {
                    return ToolResult::err(format!("源不存在: {path}"));
                }
                if target == new_target {
                    return ToolResult::err("newPath 与原路径相同，无需重命名");
                }
                if new_target.exists() {
                    return ToolResult::err(format!(
                        "目标已存在: {new_path}（rename 不覆盖；如需替换请先 delete）"
                    ));
                }
                // 源写前检查点：undo 链可回滚到重命名前
                if target.is_file() {
                    if let Err(e) = self.checkpoint(&target, session_id) {
                        return ToolResult::err(format!("写前检查点失败，已中止重命名：{e}"));
                    }
                }
                if let Some(parent) = new_target.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                match std::fs::rename(&target, &new_target) {
                    Ok(_) => ToolResult::ok(format!("renamed: {path} -> {new_path}")),
                    Err(e) => ToolResult::err(format!("重命名失败: {e}")),
                }
            }
            other => ToolResult::err(format!(
                "未知 filesystem 操作: {other}（支持 write / mkdir / list / delete / rename）"
            )),
        }
    }

    /// 工作台文件树（对话内 @ 文件提及与 e2e 用，spec: webui-code-workbench → chat-as-coding）：
    /// 相对工作区根的目录列表（.gitignore 过滤 + 深度/数量限制）。
    /// 与工具同一工作区边界（resolve_safe），越权路径拒绝。
    pub fn workbench_tree(
        &self,
        rel: &str,
        depth_limit: usize,
    ) -> anyhow::Result<serde_json::Value> {
        let root = self.effective_root();
        let start = if rel.trim().is_empty() || rel == "." {
            root.clone()
        } else {
            self.resolve_safe(rel)?
        };
        let mut nodes = Vec::new();
        let mut count = 0usize;
        let max_entries = 2000usize;
        walk_tree(
            &root,
            &start,
            0,
            depth_limit,
            &mut nodes,
            &mut count,
            max_entries,
        )?;
        Ok(serde_json::json!({
            "root": rel,
            "count": nodes.len(),
            "nodes": nodes,
        }))
    }

    /// 工作台文件读取（与 read 工具同口径分页：offset/limit/maxChars）
    pub fn workbench_file(
        &self,
        path: &str,
        offset: u64,
        limit: u64,
    ) -> anyhow::Result<serde_json::Value> {
        let p = self.resolve_safe(path)?;
        if p.is_dir() {
            anyhow::bail!("是目录不是文件：{path}");
        }
        let bytes = std::fs::read(&p)?;
        let text = String::from_utf8_lossy(&bytes).to_string();
        let total = text.lines().count();
        let off = (offset.max(1) as usize).saturating_sub(1);
        let lim = limit.clamp(1, 5000) as usize;
        let page: Vec<&str> = text.lines().skip(off).take(lim).collect();
        Ok(serde_json::json!({
            "path": path,
            "offset": off + 1,
            "limit": lim,
            "totalLines": total,
            "content": page.join("\n"),
        }))
    }
}
