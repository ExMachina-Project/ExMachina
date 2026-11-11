//! config 域分文件：Agent 执行身份约束（Linux）。

use super::*;

/// 解析 security.execUser / execHome → 执行身份。
/// - execUser 空 → None（不约束，旧行为）
/// - 数字 uid → 直接生效（无需 /etc/passwd 条目），home 用 execHome（缺省 /home/<uid>）
/// - 用户名 → getpwuid 体系查询 uid/gid/home；查不到 → Err（显式失败,不静默降级回原身份——那等于假安全）
/// - execHome 非空必须绝对路径（/ 开头）,否则 Err
#[cfg(unix)]
pub fn resolve_exec_identity(exec_user: &str, exec_home: &str) -> Result<Option<ExecIdentity>, String> {
    let user = exec_user.trim();
    if user.is_empty() {
        return Ok(None);
    }
    let home_override = exec_home.trim();
    if !home_override.is_empty() && !home_override.starts_with('/') {
        return Err(format!("security.execHome 必须是绝对路径: {exec_home}"));
    }
    // 数字 uid:无需 passwd 条目;组取同号 gid(无 passwd 时的合理缺省)
    if let Ok(uid) = user.parse::<u32>() {
        let home = if home_override.is_empty() {
            format!("/home/{uid}")
        } else {
            home_override.to_string()
        };
        return Ok(Some(ExecIdentity { uid, gid: uid, home }));
    }
    // 用户名:经 getent 查询(免 libc getpwnam 依赖,结果含 uid:gid:home)
    let out = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("getent passwd {user} 2>/dev/null"))
        .output();
    let parsed = match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        _ => return Err(format!("约束用户不存在或不可查: {user}(getent passwd 无结果)")),
    };
    // passwd 格式:name:passwd:uid:gid:gecos:home:shell
    let fields: Vec<&str> = parsed.split(':').collect();
    if fields.len() < 7 {
        return Err(format!("约束用户 passwd 记录异常: {parsed}"));
    }
    let uid: u32 = fields[2]
        .parse()
        .map_err(|_| format!("约束用户 uid 解析失败: {}", fields[2]))?;
    let gid: u32 = fields[3]
        .parse()
        .map_err(|_| format!("约束用户 gid 解析失败: {}", fields[3]))?;
    let home = if home_override.is_empty() {
        fields[5].to_string()
    } else {
        home_override.to_string()
    };
    Ok(Some(ExecIdentity { uid, gid, home }))
}

/// 非 Unix 平台:execUser 配置无效化(None)——Windows 无 uid 语义
#[cfg(not(unix))]
pub fn resolve_exec_identity(_exec_user: &str, _exec_home: &str) -> Result<Option<ExecIdentity>, String> {
    Ok(None)
}

