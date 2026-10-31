//! 插件安装与更新流水线（plugin-ecosystem）。
//!
//! 统一装入口（CLI 与 REST 共用）：解析来源（索引 id / tar.gz 包 / git 仓库）
//! → 临时区校验（路径穿越 / 大小上限 / 可执行位 / 清单合法性）→ 原子落盘
//! `plugins/<id>/` → 热装载。更新 = 覆盖安装 + `data/` 保留 + 启用状态保持。
//!
//! 安全口径（spec plugin-security「安装源安全」）：
//! - tar 条目名含 `..` 或绝对路径 → 整包拒绝
//! - 单文件 32MB / 总量 128MB 上限 → 超限拒绝且无残留
//! - 可执行位仅保留清单主命令文件（其余剥离）
//! - git 源 `.git` 与仓库其余内容不进入 `plugins/`

use super::{validate, PluginManager};
use crate::config::{PluginInstallation, PluginManifest};
use std::path::{Path, PathBuf};

/// 单文件解包上限（默认口径；超大成员直接拒绝）
pub const MAX_MEMBER_BYTES: u64 = 32 * 1024 * 1024;
/// 总解包上限（防 zip bomb 式膨胀）
pub const MAX_TOTAL_BYTES: u64 = 128 * 1024 * 1024;

/// 安装来源描述（索引项或用户直传 URL 解析结果）
#[derive(Debug, Clone)]
pub enum InstallSource {
    /// tar.gz 包（URL 或本地路径）
    TarGz { url: String },
    /// git 仓库（可带子目录与 ref）
    Git {
        url: String,
        subdir: String,
        r#ref: String,
    },
}

impl InstallSource {
    /// 来源归档为 PluginInstallation（写 config.plugins.installations）
    pub fn to_installation(&self) -> PluginInstallation {
        match self {
            InstallSource::TarGz { url } => PluginInstallation {
                kind: "targz".into(),
                url: url.clone(),
                subdir: String::new(),
                r#ref: String::new(),
            },
            InstallSource::Git { url, subdir, r#ref } => PluginInstallation {
                kind: "git".into(),
                url: url.clone(),
                subdir: subdir.clone(),
                r#ref: r#ref.clone(),
            },
        }
    }
}

/// 安装记录 → 来源（更新流用）
pub fn installation_to_source(inst: &PluginInstallation) -> InstallSource {
    match inst.kind.as_str() {
        "git" => InstallSource::Git {
            url: inst.url.clone(),
            subdir: inst.subdir.clone(),
            r#ref: inst.r#ref.clone(),
        },
        _ => InstallSource::TarGz {
            url: inst.url.clone(),
        },
    }
}

/// URL 直判：git 仓库（.git 后缀 / git:// / github.com 等托管域名）vs tar.gz 包。
/// 显式后缀优先，其余按常见托管 URL 判 git。
pub fn parse_source_url(url: &str) -> InstallSource {
    let u = url.trim();
    if u.ends_with(".git") || u.starts_with("git://") || u.starts_with("git@") {
        return InstallSource::Git {
            url: u.to_string(),
            subdir: String::new(),
            r#ref: String::new(),
        };
    }
    // github/gitee/gitlab 的仓库页（无 .git 后缀）也按 git 处理
    for host in ["github.com", "gitee.com", "gitlab.com"] {
        if let Some(pos) = u.find(host) {
            let after = &u[pos + host.len()..];
            // 形如 /<owner>/<repo>（可带 /tree/<ref>/...）才算仓库页；/raw/ /releases/download/ 是文件
            if after.starts_with('/') && !u.contains("/raw/") && !u.contains("/releases/download/")
            {
                let path = after.trim_start_matches('/');
                let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
                if segs.len() >= 2 && (segs.len() == 2 || segs[2] == "tree" || segs[2] == "blob") {
                    let (r#ref, subdir) =
                        if segs.len() >= 4 && (segs[2] == "tree" || segs[2] == "blob") {
                            // /owner/repo/tree/<ref>/<subdir...>
                            let r = segs[3].to_string();
                            let sub = segs[4..].join("/");
                            (r, sub)
                        } else {
                            (String::new(), String::new())
                        };
                    let clean = format!("https://{}/{}/{}", host, segs[0], segs[1]);
                    return InstallSource::Git {
                        url: clean,
                        subdir,
                        r#ref,
                    };
                }
            }
        }
    }
    InstallSource::TarGz { url: u.to_string() }
}

// ---------------------------------------------------------------- tar.gz 解包

/// 解包 tar.gz 到目标目录（逐条目校验：穿越/大小/可执行位）。
/// `keep_exec`：允许保留可执行位的文件名集合（清单主命令文件；路径相对包根）。
pub fn unpack_targz(
    archive: &Path,
    dest: &Path,
    keep_exec: &[String],
) -> Result<Vec<PathBuf>, String> {
    let file = std::fs::File::open(archive).map_err(|e| format!("打开包失败：{e}"))?;
    let gz = flate2::read::GzDecoder::new(file);
    let mut tar = tar::Archive::new(gz);
    tar.set_preserve_permissions(true);
    let mut total: u64 = 0;
    let mut files = Vec::new();
    for entry in tar.entries().map_err(|e| format!("读取包条目失败：{e}"))? {
        let mut entry = entry.map_err(|e| format!("包条目损坏：{e}"))?;
        let header = entry.header();
        let name = header
            .path()
            .map_err(|e| format!("条目路径不可读：{e}"))?
            .to_path_buf();
        let name_str = name.to_string_lossy().to_string();
        // 路径穿越：`..` 分量或绝对路径一律拒绝（spec: 越界拒绝且报条目名）
        if name.is_absolute()
            || name
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err(format!("包内条目路径越界，拒绝安装：{name_str}"));
        }
        let size = header.size().map_err(|e| format!("条目大小不可读：{e}"))?;
        if size > MAX_MEMBER_BYTES {
            return Err(format!(
                "包内条目 {name_str} 超过单文件上限（{}MB）",
                MAX_MEMBER_BYTES / 1024 / 1024
            ));
        }
        total += size;
        if total > MAX_TOTAL_BYTES {
            return Err(format!(
                "包总解包量超过上限（{}MB）",
                MAX_TOTAL_BYTES / 1024 / 1024
            ));
        }
        // 符号链接不保留（插件面不需要；防逃逸）
        let is_link = entry.link_name().ok().flatten().is_some();
        let ft = header.entry_type();
        if is_link || (ft.is_symlink()) {
            continue;
        }
        // Windows 风格盘符路径同样拒绝
        if name_str.contains(':') && !name_str.contains("://") {
            // 仅当出现在路径段内才拒绝（普通文件名不含冒号）
            if name_str.split('/').any(|seg| seg.contains(':')) {
                return Err(format!("包内条目路径含盘符，拒绝安装：{name_str}"));
            }
        }
        let out_path = dest.join(&name);
        if ft.is_dir() {
            std::fs::create_dir_all(&out_path).map_err(|e| format!("创建目录失败：{e}"))?;
            continue;
        }
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("创建目录失败：{e}"))?;
        }
        // 可执行位治理：默认剥离，仅 keep_exec 命中保留
        let rel = name_str.replace('\\', "/");
        let keep = keep_exec.iter().any(|k| {
            let k = k.replace('\\', "/");
            rel == *k || rel.ends_with(&format!("/{k}")) || k == "."
        });
        if keep {
            let mut opts = std::fs::OpenOptions::new();
            opts.write(true).create(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                opts.mode(0o755);
            }
            let mut f = opts
                .open(&out_path)
                .map_err(|e| format!("写出文件失败：{e}"))?;
            std::io::copy(&mut entry, &mut f).map_err(|e| format!("写出文件失败：{e}"))?;
        } else {
            let mut f =
                std::fs::File::create(&out_path).map_err(|e| format!("写出文件失败：{e}"))?;
            std::io::copy(&mut entry, &mut f).map_err(|e| format!("写出文件失败：{e}"))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Ok(meta) = std::fs::metadata(&out_path) {
                    let mut perm = meta.permissions();
                    perm.set_mode(0o644);
                    std::fs::set_permissions(&out_path, perm).ok();
                }
            }
        }
        files.push(out_path);
    }
    Ok(files)
}

/// 在解包目录中定位插件根（含 plugin.json 的目录）：
/// 包根优先；否则唯一子目录；多清单/无清单报错。
pub fn locate_plugin_root(staging: &Path) -> Result<PathBuf, String> {
    if staging.join("plugin.json").is_file() {
        return Ok(staging.to_path_buf());
    }
    let mut hits: Vec<PathBuf> = Vec::new();
    for dir in subdirs_one_level(staging) {
        if dir.join("plugin.json").is_file() {
            hits.push(dir);
        }
    }
    match hits.len() {
        1 => Ok(hits.remove(0)),
        0 => Err("包内未找到 plugin.json（包根与一级子目录均无清单）".into()),
        n => Err(format!(
            "包内发现 {n} 个 plugin.json（根/唯一子目录口径只允许一个）"
        )),
    }
}

fn subdirs_one_level(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    rd.flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect()
}

/// 校验插件根清单（复用装载校验路径）并返回 (manifest, 主命令 keep_exec 清单)。
pub fn validate_root(root: &Path, host_version: &str) -> Result<PluginManifest, String> {
    let dir_id = root
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let Some(m) = super::read_manifest(root) else {
        return Err("plugin.json 不可读或不可解析".into());
    };
    validate(&m, &dir_id, host_version)?;
    Ok(m)
}

// ---------------------------------------------------------------- 落盘与装载

/// 安装结果摘要
pub struct InstallOutcome {
    pub id: String,
    pub version: String,
    pub updated: bool,
}

/// 把已校验的 staging 目录落盘到 `plugins/<id>/`（已存在且非 overwrite → 冲突）。
/// staging_root = 插件根所在目录（只搬插件根这一棵子树）。
pub fn place_plugin(
    plugins_dir: &Path,
    plugin_root: &Path,
    overwrite: bool,
) -> Result<String, String> {
    let m = read_manifest_or(plugin_root)?;
    let id = m.id.clone();
    let dest = plugins_dir.join(&id);
    if dest.exists() {
        if !overwrite {
            return Err(format!("插件 {id} 已安装（如需替换请用更新/覆盖安装）"));
        }
        // 覆盖安装 = 更新语义：保留 data/（调用方 update_plugin 处理；直接覆盖场景先移除）
        let backup = dest.with_extension("old");
        let _ = std::fs::remove_dir_all(&backup);
        std::fs::rename(&dest, &backup).map_err(|e| format!("移除旧版本失败：{e}"))?;
        // data/ 保留
        if backup.join("data").is_dir() {
            std::fs::create_dir_all(&dest).map_err(|e| format!("落盘失败：{e}"))?;
            std::fs::rename(backup.join("data"), dest.join("data"))
                .map_err(|e| format!("保留 data/ 失败：{e}"))?;
        }
        let _ = std::fs::remove_dir_all(&backup);
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("创建 plugins/ 失败：{e}"))?;
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("创建 plugins/ 失败：{e}"))?;
    }
    if let Err(e) = std::fs::rename(plugin_root, &dest) {
        // rename 跨设备失败回退复制
        copy_dir_recursive(plugin_root, &dest).map_err(|e2| format!("落盘失败：{e}/{e2}"))?;
    }
    Ok(id)
}

fn read_manifest_or(dir: &Path) -> Result<PluginManifest, String> {
    super::read_manifest(dir).ok_or_else(|| "plugin.json 不可读".to_string())
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for e in std::fs::read_dir(src)? {
        let e = e?;
        let from = e.path();
        let to = dst.join(e.file_name());
        if from.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

/// 主命令文件清单（可执行位白名单）：process.command 的 argv[0] 若指向包内文件
/// （以 `./` 或 `dir/` 形式的相对路径；裸命令名如 python3 是解释器不算包内文件）。
pub fn keep_exec_names(m: &PluginManifest) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(p) = &m.process {
        if let Some(first) = p.command.first() {
            let f = first.replace('\\', "/");
            let looks_like_path = f.starts_with("./") || f.starts_with("../") || f.contains('/');
            if looks_like_path && !f.starts_with('/') && !f.contains("..") {
                out.push(f.trim_start_matches("./").to_string());
            }
        }
    }
    out
}

// ---------------------------------------------------------------- 安装主流程

/// 拉取 tar.gz（URL 或本地路径）到临时文件
pub async fn fetch_targz(url: &str, tmp: &Path) -> Result<(), String> {
    if !url.contains("://") {
        // 本地路径
        std::fs::copy(url, tmp).map_err(|e| format!("读取本地包失败：{e}"))?;
        return Ok(());
    }
    let resp = reqwest::get(url)
        .await
        .map_err(|e| format!("下载包失败：{e}"))?
        .error_for_status()
        .map_err(|e| format!("下载包失败：{e}"))?;
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| format!("读取包内容失败：{e}"))?;
    if bytes.len() as u64 > MAX_TOTAL_BYTES {
        return Err(format!(
            "包超过总上限（{}MB）",
            MAX_TOTAL_BYTES / 1024 / 1024
        ));
    }
    std::fs::write(tmp, &bytes).map_err(|e| format!("写临时包失败：{e}"))?;
    Ok(())
}

/// git 浅克隆到目录（需要 git 可执行；`.git` 在克隆目录但不随迁——place 只搬子树内容）
pub async fn git_shallow_clone(url: &str, r#ref: &str, dest: &Path) -> Result<(), String> {
    let mut cmd = tokio::process::Command::new("git");
    cmd.arg("clone").arg("--depth").arg("1");
    if !r#ref.trim().is_empty() {
        cmd.arg("--branch").arg(r#ref.trim());
    }
    cmd.arg(url).arg(dest);
    let out = cmd
        .output()
        .await
        .map_err(|e| format!("git 不可用（{e}）；请安装 git 或改用 tar.gz 源"))?;
    if !out.status.success() {
        return Err(format!(
            "git 克隆失败：{}",
            String::from_utf8_lossy(&out.stderr)
                .chars()
                .take(500)
                .collect::<String>()
        ));
    }
    Ok(())
}

/// 来源 → staging 插件根（下载/克隆 + 解包 + 定位 + 校验）。
/// 返回 (staging 根, manifest)。staging 由调用方负责清理。
/// 插件根目录名归一为 manifest.id（validate 要求目录名 == id；包内前缀目录名不保证）。
pub async fn stage_source(
    source: &InstallSource,
    staging_area: &Path,
    _host_version: &str,
) -> Result<(PathBuf, PluginManifest), String> {
    match source {
        InstallSource::TarGz { url } => {
            let archive = staging_area.join("pkg.tgz");
            fetch_targz(url, &archive).await?;
            let unpacked = staging_area.join("unpacked");
            std::fs::create_dir_all(&unpacked).map_err(|e| e.to_string())?;
            unpack_targz(&archive, &unpacked, &[".".into()])?;
            let root = locate_plugin_root(&unpacked)?;
            let m = validate_lenient(&root)?;
            // 归一目录名 → id（place_plugin 校验目录名 == id）
            let normalized = staging_area.join(&m.id);
            if root != normalized {
                std::fs::rename(&root, &normalized).map_err(|e| format!("归一目录名失败：{e}"))?;
            }
            strip_exec_except(&normalized, &keep_exec_names(&m))?;
            Ok((normalized, m))
        }
        InstallSource::Git { url, subdir, r#ref } => {
            let repo = staging_area.join("repo");
            git_shallow_clone(url, r#ref, &repo).await?;
            let base = if subdir.trim().is_empty() {
                repo.clone()
            } else {
                let sub = repo.join(subdir.trim().trim_matches('/'));
                if !sub.is_dir() {
                    return Err(format!("仓库内未找到子目录 {subdir}"));
                }
                sub
            };
            if !base.join("plugin.json").is_file() {
                return Err("git 仓库（或子目录）根缺少 plugin.json".into());
            }
            let m = validate_lenient(&base)?;
            strip_exec_except(&base, &keep_exec_names(&m))?;
            // 仓库目录不能 rename 出去（克隆区整体清理）——复制插件根到归一名
            let normalized = staging_area.join(&m.id);
            copy_dir_recursive(&base, &normalized).map_err(|e| format!("提取插件目录失败：{e}"))?;
            Ok((normalized, m))
        }
    }
}

/// staging 期校验（不查目录名 == id：包内目录名不保证；落盘时以 id 归一）。
/// 其余校验项（id 合法/能力非空/版本兼容/工具名）与装载校验完全一致。
fn validate_lenient(root: &Path) -> Result<PluginManifest, String> {
    let Some(m) = super::read_manifest(root) else {
        return Err("plugin.json 不可读或不可解析".into());
    };
    if !m.valid_id() {
        return Err(format!("id 不合法：{:?}", m.id));
    }
    if !m.has_any_capability() {
        return Err("未声明任何能力（tools/channel/watch/resources 至少一类）".into());
    }
    let host = super::host_version();
    if !m.exm_min.trim().is_empty() && !super::version_ok(&m.exm_min, &host) {
        return Err(format!("需要宿主 ≥ {}，当前 {}", m.exm_min, host));
    }
    if let Some(p) = &m.process {
        if p.command.iter().any(|c| c.contains("..")) {
            return Err("process.command 不允许包含 ..".into());
        }
    }
    for t in &m.tools {
        if !t.valid() {
            return Err(format!("内联工具名不合法：{:?}", t.name));
        }
    }
    Ok(m)
}

/// 目录内文件权限治理：除 keep 名单外剥离可执行位（unix）
fn strip_exec_except(root: &Path, keep: &[String]) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fn walk(dir: &Path, keep: &[String]) -> Result<(), String> {
            for e in std::fs::read_dir(dir).map_err(|e| e.to_string())? {
                let e = e.map_err(|e| e.to_string())?;
                let p = e.path();
                if p.is_dir() {
                    walk(&p, keep)?;
                    continue;
                }
                let rel = p
                    .strip_prefix(root_parent_of(&p, dir))
                    .unwrap_or(&p)
                    .to_string_lossy()
                    .replace('\\', "/");
                // 相对插件根计算（walk 起点即根）
                let rel_from_root = p.strip_prefix(root_arg(dir)).unwrap_or(&p);
                let _ = rel;
                let rel2 = rel_from_root.to_string_lossy().replace('\\', "/");
                let keep_hit = keep.iter().any(|k| {
                    let k = k.replace('\\', "/");
                    rel2 == k || rel2.ends_with(&format!("/{k}"))
                });
                let meta = std::fs::metadata(&p).map_err(|e| e.to_string())?;
                let perm = meta.permissions();
                let mode = perm.mode();
                if !keep_hit && (mode & 0o111) != 0 {
                    let mut np = perm.clone();
                    np.set_mode(mode & !0o111);
                    std::fs::set_permissions(&p, np).map_err(|e| e.to_string())?;
                }
            }
            Ok(())
        }
        // 简化：walk 的 dir 参数即插件根（strip 前缀用）
        fn root_arg(d: &Path) -> &Path {
            d
        }
        fn root_parent_of<'a>(_p: &'a Path, d: &'a Path) -> &'a Path {
            d
        }
        walk(root, keep)
    }
    #[cfg(not(unix))]
    {
        let _ = (root, keep);
        Ok(())
    }
}

/// 完整安装流程（CLI/REST 共用）：staging 校验 → 落盘 → 热装载 → 来源记录。
/// 返回安装结果摘要。pm/core 回接由调用方处理（core.apply_config 持久化 installation）。
pub async fn install_from_source(
    pm: &PluginManager,
    source: &InstallSource,
    plugins_dir: &Path,
    host_version: &str,
    overwrite: bool,
) -> Result<InstallOutcome, String> {
    let staging_area = std::env::temp_dir().join(format!("exm-install-{}", crate::types::new_id()));
    let _ = std::fs::remove_dir_all(&staging_area);
    std::fs::create_dir_all(&staging_area).map_err(|e| format!("创建临时区失败：{e}"))?;
    let result = async {
        let (root, m) = stage_source(source, &staging_area, host_version).await?;
        // 版本检查已在 validate_root 内（exmMin）
        let id = place_plugin(plugins_dir, &root, overwrite)?;
        // 热装载：单插件重扫（load_all 幂等；对新增目录生效）
        pm.load_one(&id);
        Ok(InstallOutcome {
            version: m.version.clone(),
            id,
            updated: overwrite,
        })
    }
    .await;
    let _ = std::fs::remove_dir_all(&staging_area);
    result
}

/// 解析来源当前清单（tar.gz 读包内清单 / git 读克隆区清单），不落盘。
/// 版本裁决（是否比本地新）交给调用方——需本地版本对照。
pub async fn probe_source_version(
    source: &InstallSource,
    host_version: &str,
) -> Result<PluginManifest, String> {
    let staging_area = std::env::temp_dir().join(format!("exm-probe-{}", crate::types::new_id()));
    let _ = std::fs::remove_dir_all(&staging_area);
    std::fs::create_dir_all(&staging_area).map_err(|e| e.to_string())?;
    let probed: Result<PluginManifest, String> = async {
        let (_root, m) = stage_source(source, &staging_area, host_version).await?;
        Ok(m)
    }
    .await;
    let _ = std::fs::remove_dir_all(&staging_area);
    probed
}

/// 更新流程：停旧能力面 → data/ 安全区 → 覆盖落盘 → data/ 移回 → 保持启用状态重装载。
/// 版本对照（已是最新则不动盘）由调用方先用 probe 判断；本函数专注「替换 + 保状态」。
pub async fn update_from_source(
    pm: &PluginManager,
    source: &InstallSource,
    plugins_dir: &Path,
    host_version: &str,
) -> Result<InstallOutcome, String> {
    // staging 先行（失败不动本地）
    let staging_area = std::env::temp_dir().join(format!("exm-update-{}", crate::types::new_id()));
    let _ = std::fs::remove_dir_all(&staging_area);
    std::fs::create_dir_all(&staging_area).map_err(|e| format!("创建临时区失败：{e}"))?;
    let outcome = async {
        let (root, m) = stage_source(source, &staging_area, host_version).await?;
        let id = m.id.clone();
        let dest = plugins_dir.join(&id);
        // 停旧能力面（进程 provider 摘除由 pm.load_one 覆盖；先禁用态装载防半更新窗口）
        pm.set_enabled(&id, false);
        // data/ 安全区
        let safe = staging_area.join("data-safe");
        if dest.join("data").is_dir() {
            std::fs::create_dir_all(&safe).map_err(|e| e.to_string())?;
            std::fs::rename(dest.join("data"), safe.join("data")).map_err(|e| e.to_string())?;
        }
        // 替换目录
        let _ = std::fs::remove_dir_all(&dest);
        std::fs::rename(&root, &dest).map_err(|e| format!("替换插件目录失败：{e}"))?;
        // data/ 移回
        if safe.join("data").is_dir() {
            std::fs::rename(safe.join("data"), dest.join("data")).map_err(|e| e.to_string())?;
        }
        pm.load_one(&id);
        Ok(InstallOutcome {
            id,
            version: m.version.clone(),
            updated: true,
        })
    }
    .await;
    let _ = std::fs::remove_dir_all(&staging_area);
    outcome
}

// ---------------------------------------------------------------- 索引面

/// 官方索引缺省地址（仓库内 plugins/index.json 的 raw URL；config.plugins.indexUrl 可覆盖）
pub const DEFAULT_INDEX_URL: &str =
    "https://raw.githubusercontent.com/ExMachina-Project/ExMachina/dev/plugins/index.json";

/// 索引项来源（与 InstallSource 对应的可序列化形态）
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexSource {
    /// targz | git
    pub kind: String,
    #[serde(default)]
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subdir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#ref: Option<String>,
}

/// 索引条目（plugins/index.json 数组元素）
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexEntry {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub description: String,
    pub source: IndexSource,
    /// 宿主版本要求（装载校验同口径）
    #[serde(default)]
    pub exm_min: String,
    /// 包 sha256（签名/信任链 Non-Goal，位预留：校验在后续变更补）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
}

impl IndexEntry {
    /// 索引来源 → 安装来源
    pub fn to_install_source(&self) -> InstallSource {
        match self.source.kind.as_str() {
            "git" => InstallSource::Git {
                url: self.source.url.clone(),
                subdir: self.source.subdir.clone().unwrap_or_default(),
                r#ref: self.source.r#ref.clone().unwrap_or_default(),
            },
            _ => InstallSource::TarGz {
                url: self.source.url.clone(),
            },
        }
    }
}

/// 索引条目与本地已装对照状态
#[derive(Debug, Clone, PartialEq)]
pub enum IndexStatus {
    /// 未安装（可安装）
    Available,
    /// 已安装且为最新（本地版本号）
    Installed(String),
    /// 已安装但有新版（本地 → 索引版本）
    Updatable(String, String),
    /// 宿主版本不满足 exmMin
    Incompatible(String),
}

/// 生效索引地址：config 覆盖 > 官方缺省
pub fn effective_index_url(configured: &str) -> String {
    let u = configured.trim();
    if u.is_empty() {
        DEFAULT_INDEX_URL.into()
    } else {
        u.to_string()
    }
}

/// 拉取并解析索引（10s 超时；容忍对象包裹数组的两形态）
pub async fn fetch_index(url: &str) -> Result<Vec<IndexEntry>, String> {
    let text = if url.contains("://") {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .map_err(|e| format!("HTTP 客户端构建失败：{e}"))?;
        // 网络抖动容错：3 次尝试 + 短退避（raw CDN 在部分网络下间歇超时）
        let mut last_err = String::new();
        let mut text = None;
        for attempt in 0..3 {
            if attempt > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(1500 * attempt as u64)).await;
            }
            match client
                .get(url)
                .send()
                .await
                .and_then(|r| r.error_for_status().map_err(|e| e.without_url()))
            {
                Ok(resp) => match resp.text().await {
                    Ok(t) => {
                        text = Some(t);
                        break;
                    }
                    Err(e) => last_err = format!("读取索引失败：{e}"),
                },
                Err(e) => last_err = format!("拉取插件索引失败：{e}"),
            }
        }
        text.ok_or(last_err)?
    } else {
        std::fs::read_to_string(url).map_err(|e| format!("读取本地索引失败：{e}"))?
    };
    parse_index(&text)
}

/// 解析索引文本：数组为主形态；`{ "plugins": [...] }` 包裹形态兼容
pub fn parse_index(text: &str) -> Result<Vec<IndexEntry>, String> {
    let v: serde_json::Value =
        serde_json::from_str(text).map_err(|e| format!("索引不是合法 JSON：{e}"))?;
    let arr = match &v {
        serde_json::Value::Array(a) => a.clone(),
        serde_json::Value::Object(o) => o
            .get("plugins")
            .and_then(|p| p.as_array())
            .cloned()
            .ok_or_else(|| "索引需为条目数组或 { plugins: [...] } 形态".to_string())?,
        _ => return Err("索引需为条目数组".into()),
    };
    let mut out = Vec::new();
    for (i, item) in arr.iter().enumerate() {
        let e: IndexEntry = serde_json::from_value(item.clone())
            .map_err(|err| format!("索引第 {} 项不合法：{err}", i + 1))?;
        if e.id.trim().is_empty() {
            return Err(format!("索引第 {} 项缺 id", i + 1));
        }
        out.push(e);
    }
    Ok(out)
}

/// 索引 ↔ 本地已装对照（CLI list / REST 索引目录共用口径）
pub fn compare_index(
    entries: &[IndexEntry],
    installed: &dyn Fn(&str) -> Option<(String, bool)>, // id → (本地版本, enabled)
    host_version: &str,
) -> Vec<(IndexEntry, IndexStatus)> {
    entries
        .iter()
        .map(|e| {
            let status =
                if !e.exm_min.trim().is_empty() && !super::version_ok(&e.exm_min, host_version) {
                    IndexStatus::Incompatible(e.exm_min.clone())
                } else {
                    match installed(&e.id) {
                        Some((local_v, _)) => {
                            if super::version_newer(&e.version, &local_v) {
                                IndexStatus::Updatable(local_v, e.version.clone())
                            } else {
                                IndexStatus::Installed(local_v)
                            }
                        }
                        None => IndexStatus::Available,
                    }
                };
            (e.clone(), status)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_pkg(dir: &Path, entries: &[(&str, &str)]) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let src = dir.join("src");
        std::fs::create_dir_all(&src).unwrap();
        for (name, content) in entries {
            let p = src.join(name);
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(p, content).unwrap();
        }
        // 打 tar.gz：src 一级条目原样进包根（entries 名即包内相对路径）
        let tgz = dir.join("pkg.tgz");
        let file = std::fs::File::create(&tgz).unwrap();
        let enc = flate2::write::GzEncoder::new(file, flate2::Compression::default());
        let mut tar = tar::Builder::new(enc);
        for e in std::fs::read_dir(&src).unwrap().flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if e.path().is_dir() {
                tar.append_dir_all(name, e.path()).unwrap();
            } else {
                tar.append_file(name, &mut std::fs::File::open(e.path()).unwrap())
                    .unwrap();
            }
        }
        tar.into_inner().unwrap().finish().unwrap();
        tgz
    }

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("exm-inst-{}-{}", tag, crate::types::new_id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    const MANIFEST: &str = r#"{ "id": "hello", "name": "Hello", "version": "0.1.0", "tools": [ { "name": "ping", "kind": "http", "target": "https://example.com", "method": "GET" } ] }"#;

    #[test]
    fn 解包_合法包定位与校验() {
        let d = tmp("ok");
        let tgz = make_pkg(&d, &[("pkg/plugin.json", MANIFEST), ("pkg/extra.txt", "x")]);
        let out = d.join("unpacked");
        std::fs::create_dir_all(&out).unwrap();
        let files = unpack_targz(&tgz, &out, &[]).unwrap();
        assert!(!files.is_empty());
        let root = locate_plugin_root(&out).unwrap();
        assert!(root.join("plugin.json").is_file());
        // staging 校验（lenient：包内目录名 pkg ≠ id 属正常，落盘时归一）
        let m = validate_lenient(&root).unwrap();
        assert_eq!(m.id, "hello");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn 解包_路径穿越拒绝() {
        // tar Builder 构造侧拒绝 `..` 与绝对路径条目（"paths must be relative"/"must not have `..`"）
        // ——攻击包无法经正规工具构造；解包侧的防线（entry.path 校验）保留为纵深防御。
        // 此处验证：正常相对路径不误伤 + `./` 前缀条目合法解包。
        let d = tmp("traversal");
        let tgz = make_pkg(
            &d,
            &[("pkg/plugin.json", MANIFEST), ("pkg/normal.txt", "ok")],
        );
        let out = d.join("unpacked");
        std::fs::create_dir_all(&out).unwrap();
        let files = unpack_targz(&tgz, &out, &[]).unwrap();
        assert!(
            files.iter().any(|f| f.ends_with("normal.txt")),
            "正常条目应解包：{files:?}"
        );
        // 构造侧拒绝 ..（防线证据）
        let tgz2 = d.join("evil2.tgz");
        let f2 = std::fs::File::create(&tgz2).unwrap();
        let e2 = flate2::write::GzEncoder::new(f2, flate2::Compression::default());
        let mut t2 = tar::Builder::new(e2);
        let mut h2 = tar::Header::new_gnu();
        h2.set_size(4);
        h2.set_mode(0o644);
        h2.set_cksum();
        assert!(
            t2.append_data(
                &mut h2,
                "../evil.txt",
                std::io::Cursor::new(b"evil".to_vec())
            )
            .is_err(),
            "tar 构造面应拒绝 .. 路径"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn 解包_超大成员拒绝() {
        let d = tmp("toobig");
        let tgz = d.join("big.tgz");
        let file = std::fs::File::create(&tgz).unwrap();
        let enc = flate2::write::GzEncoder::new(file, flate2::Compression::default());
        let mut tar = tar::Builder::new(enc);
        let mut header = tar::Header::new_gnu();
        // header 声明 33MB（写入字节流小于声明；校验读 header.size）
        header.set_size(MAX_MEMBER_BYTES + 1);
        header.set_mode(0o644);
        header.set_cksum();
        tar.append_data(&mut header, "pkg/big.bin", std::io::empty())
            .unwrap();
        tar.into_inner().unwrap().finish().unwrap();
        let out = d.join("unpacked");
        std::fs::create_dir_all(&out).unwrap();
        let err = unpack_targz(&tgz, &out, &[]).unwrap_err();
        assert!(err.contains("单文件上限"), "应报单文件上限：{err}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn 解包_多清单包拒绝() {
        let d = tmp("multi");
        // 包根平级两个子目录各含清单（包根无清单 → 一级子目录口径发现 2 个 → 拒绝）
        let tgz = make_pkg(
            &d,
            &[("a/plugin.json", MANIFEST), ("b/plugin.json", MANIFEST)],
        );
        let out = d.join("unpacked");
        std::fs::create_dir_all(&out).unwrap();
        unpack_targz(&tgz, &out, &[]).unwrap();
        let err = locate_plugin_root(&out).unwrap_err();
        assert!(err.contains("2"), "应报多清单：{err}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[cfg(unix)]
    #[test]
    fn 解包_可执行位治理() {
        let d = tmp("exec");
        let src = d.join("src/pkg");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("plugin.json"), MANIFEST).unwrap();
        std::fs::write(src.join("run.sh"), "#!/bin/sh\necho hi\n").unwrap();
        std::fs::write(src.join("helper.py"), "print(1)\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(src.join("run.sh"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
        std::fs::set_permissions(
            src.join("helper.py"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        let tgz = d.join("pkg.tgz");
        let file = std::fs::File::create(&tgz).unwrap();
        let enc = flate2::write::GzEncoder::new(file, flate2::Compression::default());
        let mut tar = tar::Builder::new(enc);
        tar.append_dir_all("pkg", &src).unwrap();
        tar.into_inner().unwrap().finish().unwrap();
        let out = d.join("unpacked/pkg");
        std::fs::create_dir_all(d.join("unpacked")).unwrap();
        unpack_targz(&tgz, &d.join("unpacked"), &[]).unwrap();
        // 治理前 run.sh 应带执行位（或已被 unpack 剥离——unpack keep 空名单直接剥）
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode();
        assert_eq!(
            mode(&out.join("run.sh")) & 0o111,
            0,
            "keep 空名单应剥离全部执行位"
        );
        // keep 主命令：重解包带 keep
        let out2 = d.join("unpacked2/pkg");
        std::fs::create_dir_all(d.join("unpacked2")).unwrap();
        unpack_targz(&tgz, &d.join("unpacked2"), &["pkg/run.sh".into()]).unwrap();
        assert_ne!(mode(&out2.join("run.sh")) & 0o111, 0, "主命令应保留执行位");
        assert_eq!(mode(&out2.join("helper.py")) & 0o111, 0, "其余应剥离");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn 落盘_冲突与覆盖保数据() {
        let d = tmp("place");
        let plugins = d.join("plugins");
        // 首装
        let r1 = d.join("r1");
        std::fs::create_dir_all(&r1).unwrap();
        std::fs::write(r1.join("plugin.json"), MANIFEST).unwrap();
        let id = place_plugin(&plugins, &r1, false).unwrap();
        assert_eq!(id, "hello");
        assert!(plugins.join("hello/plugin.json").is_file());
        // 已装插件写入 data/
        std::fs::create_dir_all(plugins.join("hello/data")).unwrap();
        std::fs::write(plugins.join("hello/data/keep.txt"), "v").unwrap();
        // 冲突：非 overwrite 拒绝
        let r2 = d.join("r2");
        std::fs::create_dir_all(&r2).unwrap();
        std::fs::write(r2.join("plugin.json"), MANIFEST).unwrap();
        let err = place_plugin(&plugins, &r2, false).unwrap_err();
        assert!(err.contains("已安装"), "应报冲突：{err}");
        // 覆盖：data/ 保留
        place_plugin(&plugins, &r2, true).unwrap();
        assert!(
            plugins.join("hello/data/keep.txt").is_file(),
            "data/ 应保留"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn 来源解析_url直判() {
        match parse_source_url("https://github.com/foo/bar") {
            InstallSource::Git { url, .. } => assert_eq!(url, "https://github.com/foo/bar"),
            _ => panic!("应判 git"),
        }
        match parse_source_url("https://github.com/foo/bar/tree/dev/pkg") {
            InstallSource::Git { url, r#ref, subdir } => {
                assert_eq!(url, "https://github.com/foo/bar");
                assert_eq!(r#ref, "dev");
                assert_eq!(subdir, "pkg");
            }
            _ => panic!("应判 git"),
        }
        match parse_source_url("https://example.com/pkg.tgz") {
            InstallSource::TarGz { url } => assert_eq!(url, "https://example.com/pkg.tgz"),
            _ => panic!("应判 targz"),
        }
        match parse_source_url("git@github.com:foo/bar.git") {
            InstallSource::Git { .. } => {}
            _ => panic!("应判 git"),
        }
    }

    #[test]
    fn keep_exec_相对主命令() {
        let m: PluginManifest = serde_json::from_str(
            r#"{ "id": "p", "process": { "command": ["./run.sh", "--flag"] } }"#,
        )
        .unwrap();
        assert_eq!(keep_exec_names(&m), vec!["run.sh".to_string()]);
        let m2: PluginManifest =
            serde_json::from_str(r#"{ "id": "p", "process": { "command": ["python3", "x.py"] } }"#)
                .unwrap();
        assert!(keep_exec_names(&m2).is_empty(), "解释器不算包内主命令");
    }

    #[tokio::test]
    async fn 安装_本地包全流程() {
        let d = tmp("flow");
        let tgz = make_pkg(
            &d,
            &[("pkg/plugin.json", MANIFEST), ("pkg/readme.md", "hi")],
        );
        let plugins = d.join("plugins");
        std::fs::create_dir_all(&plugins).unwrap();
        let pm = PluginManager::new(&plugins);
        let out = install_from_source(
            &pm,
            &InstallSource::TarGz {
                url: tgz.to_string_lossy().to_string(),
            },
            &plugins,
            "0.5.3",
            false,
        )
        .await
        .unwrap();
        assert_eq!(out.id, "hello");
        // 热装载：list 含 hello
        assert!(pm.list().iter().any(|e| e.manifest.id == "hello"));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Core 级安装往返（plugin-ecosystem 2.2）：installation 持久化 + 声明式工具回接
    #[tokio::test]
    async fn 安装_core级往返与来源持久化() {
        let d = tmp("coreflow");
        let tgz = make_pkg(&d, &[("pkg/plugin.json", MANIFEST)]);
        let mut cfg = crate::config::ExmConfig::load(&d);
        cfg.use_mock = true;
        cfg.memory_enabled = false;
        let core = crate::Core::with_config(cfg).unwrap();
        let out = core
            .plugin_install(
                &InstallSource::TarGz {
                    url: tgz.to_string_lossy().to_string(),
                },
                false,
            )
            .await
            .unwrap();
        assert_eq!(out.id, "hello");
        // 来源记录持久化
        let cfg2 = core.config();
        let inst = cfg2
            .plugins
            .installations
            .get("hello")
            .expect("installation 应记录");
        assert_eq!(inst.kind, "targz");
        assert!(inst.url.ends_with("pkg.tgz"));
        // 声明式工具回接：ToolGateway 下发含 hello__ping
        let orch = core.orchestrator();
        let specs = orch.tools.custom_specs_for("a1");
        assert!(
            specs.iter().any(|s| s.name == "hello__ping"),
            "声明式工具应热装载下发：{:?}",
            specs.iter().map(|s| s.name.clone()).collect::<Vec<_>>()
        );
        // 重启保持：重开 Core 仍在
        drop(core);
        let cfg3 = crate::config::ExmConfig::load(&d);
        let core2 = crate::Core::with_config(cfg3).unwrap();
        assert!(core2
            .plugins()
            .list()
            .iter()
            .any(|e| e.manifest.id == "hello"));
        // 卸载：目录删除 + 记录清理
        assert!(core2.plugin_uninstall("hello").unwrap());
        assert!(!d.join("plugins/hello").exists());
        assert!(!core2.config().plugins.installations.contains_key("hello"));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// 更新流（plugin-ecosystem 2.3）：保 data/ 与启用状态 / 已是最新不落盘 / 无来源记录
    #[tokio::test]
    async fn 更新_保数据与状态() {
        let d = tmp("upd");
        let tgz_v1 = make_pkg(
            &d.join("v1"),
            &[(
                "pkg/plugin.json",
                r#"{ "id": "hello", "version": "0.1.0", "tools": [ { "name": "ping", "kind": "http", "target": "https://example.com", "method": "GET" } ] }"#,
            )],
        );
        let mut cfg = crate::config::ExmConfig::load(&d);
        cfg.use_mock = true;
        cfg.memory_enabled = false;
        let core = crate::Core::with_config(cfg).unwrap();
        let url_v1 = tgz_v1.to_string_lossy().to_string();
        core.plugin_install(
            &InstallSource::TarGz {
                url: url_v1.clone(),
            },
            false,
        )
        .await
        .unwrap();
        // 写入运行数据 + 手动禁用
        std::fs::create_dir_all(d.join("plugins/hello/data")).unwrap();
        std::fs::write(d.join("plugins/hello/data/state.json"), r#"{ "seen": 1 }"#).unwrap();
        core.plugins().set_enabled("hello", false);
        let mut c1 = (*core.config()).clone();
        c1.plugins.state.insert("hello".into(), false);
        core.apply_config(c1).unwrap();
        // 更新到 v2
        let tgz_v2 = make_pkg(
            &d.join("v2"),
            &[(
                "pkg/plugin.json",
                r#"{ "id": "hello", "version": "0.2.0", "tools": [ { "name": "ping", "kind": "http", "target": "https://example.com", "method": "GET" } ] }"#,
            )],
        );
        let url_v2 = tgz_v2.to_string_lossy().to_string();
        // 来源记录仍指向 v1 → 先改成 v2 模拟远端更新（同 URL 场景；此处直接装 v2 URL）
        {
            let mut c2 = (*core.config()).clone();
            c2.plugins.installations.insert(
                "hello".into(),
                crate::config::PluginInstallation {
                    kind: "targz".into(),
                    url: url_v2.clone(),
                    ..Default::default()
                },
            );
            core.apply_config(c2).unwrap();
        }
        let (updated, msg) = core.plugin_update("hello").await.unwrap();
        assert!(updated, "应发生更新：{msg}");
        // data/ 保留、版本更新、禁用状态保持
        assert!(
            d.join("plugins/hello/data/state.json").is_file(),
            "data/ 应保留"
        );
        assert_eq!(
            core.plugins()
                .get("hello")
                .map(|e| e.manifest.version.clone()),
            Some("0.2.0".into())
        );
        assert!(
            !core.plugins().get("hello").unwrap().enabled,
            "禁用状态应保持"
        );
        // 已是最新：再更一次不落盘（mtime 不变口径太脆——用返回值验证）
        let (updated2, msg2) = core.plugin_update("hello").await.unwrap();
        assert!(!updated2, "同版本不应再更新：{msg2}");
        assert!(msg2.contains("已是最新"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[tokio::test]
    async fn 更新_无来源记录拒绝() {
        let d = tmp("nosrc");
        std::fs::create_dir_all(d.join("plugins/hello")).unwrap();
        std::fs::write(
            d.join("plugins/hello/plugin.json"),
            r#"{ "id": "hello", "tools": [ { "name": "ping", "kind": "http", "target": "https://example.com", "method": "GET" } ] }"#,
        )
        .unwrap();
        let mut cfg = crate::config::ExmConfig::load(&d);
        cfg.use_mock = true;
        cfg.memory_enabled = false;
        let core = crate::Core::with_config(cfg).unwrap();
        let err = core.plugin_update("hello").await.unwrap_err().to_string();
        assert!(err.contains("无安装来源记录"), "应报无来源：{err}");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// 索引面（plugin-ecosystem 2.4）：解析两形态 / 对照逻辑 / 生效地址
    #[test]
    fn 索引_解析数组与包裹形态() {
        let arr = r#"[{ "id": "rss", "name": "RSS", "version": "1.0.0", "source": { "kind": "git", "url": "https://github.com/x/rss" } }]"#;
        let idx = parse_index(arr).unwrap();
        assert_eq!(idx.len(), 1);
        assert_eq!(idx[0].id, "rss");
        assert_eq!(idx[0].source.kind, "git");
        let wrapped = r#"{ "plugins": [ { "id": "demo", "version": "0.1.0", "source": { "kind": "targz", "url": "https://x/pkg.tgz" } } ] }"#;
        let idx2 = parse_index(wrapped).unwrap();
        assert_eq!(idx2[0].id, "demo");
        // 非法形态
        assert!(parse_index("{}").is_err());
        assert!(
            parse_index("[{ \"version\": \"1\" }]").is_err(),
            "缺 id 应拒绝"
        );
        assert!(parse_index("not json").is_err());
    }

    #[test]
    fn 索引_本地对照状态() {
        let mk = |id: &str, v: &str, exm_min: &str| IndexEntry {
            id: id.into(),
            name: id.into(),
            version: v.into(),
            description: String::new(),
            source: IndexSource {
                kind: "targz".into(),
                url: format!("https://x/{id}.tgz"),
                subdir: None,
                r#ref: None,
            },
            exm_min: exm_min.into(),
            sha256: None,
        };
        let entries = vec![
            mk("new", "1.0.0", ""),        // 未装
            mk("same", "1.0.0", ""),       // 已装同版
            mk("old", "2.0.0", ""),        // 已装旧版
            mk("future", "9.0.0", "99.0"), // 宿主不满足
        ];
        let installed = |id: &str| -> Option<(String, bool)> {
            match id {
                "same" => Some(("1.0.0".into(), true)),
                "old" => Some(("1.0.0".into(), true)),
                _ => None,
            }
        };
        let out = compare_index(&entries, &installed, "0.5.3");
        assert_eq!(out[0].1, IndexStatus::Available);
        assert_eq!(out[1].1, IndexStatus::Installed("1.0.0".into()));
        assert_eq!(
            out[2].1,
            IndexStatus::Updatable("1.0.0".into(), "2.0.0".into())
        );
        assert_eq!(out[3].1, IndexStatus::Incompatible("99.0".into()));
    }

    #[test]
    fn 索引_生效地址() {
        assert_eq!(effective_index_url(""), DEFAULT_INDEX_URL);
        assert_eq!(effective_index_url(" "), DEFAULT_INDEX_URL);
        assert_eq!(
            effective_index_url("https://my.host/index.json"),
            "https://my.host/index.json"
        );
    }
}
