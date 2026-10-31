//! 回收站保留契约（spec: trash-retention）：
//! filesystem delete 产生的 `.exmachina/trash/<日期>/` 内容必须有界——
//! 按保留期与容量上限自动清理，防无限膨胀拖垮磁盘。双约束并存，先到先清。
//!
//! 纯逻辑与 IO 均在本模块（trash 属 core 工作区文件系统语义）；
//! 周期调度由网关侧 spawn（复用 supervisor 周期模式，不另起常驻线程）。

use crate::config::TrashConfig;
use crate::Core;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// 回收站根目录（`<工作区>/.exmachina/trash`）
pub fn trash_root(core: &Core) -> PathBuf {
    core.config().workspace_root.join(".exmachina").join("trash")
}

/// 一次巡检的结果（审计与状态面数据源）
#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct CleanupReport {
    /// 本轮删除的日期目录（目录名，ISO 日期）
    pub removed_days: Vec<String>,
    /// 释放容量（字节）
    pub freed_bytes: u64,
    /// 清理原因计数：超保留期删除的目录数
    pub expired_removed: usize,
    /// 清理原因计数：超容量淘汰的目录数
    pub overflow_removed: usize,
    /// 巡检时回收站总量（字节，删除后）
    pub total_bytes_after: u64,
}

/// 列出回收站的日期目录（目录名 ISO 日期，按名字升序 = 时间升序）
fn day_dirs(root: &Path) -> Vec<(String, PathBuf)> {
    let Ok(rd) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut out: Vec<(String, PathBuf)> = rd
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            // 目录名即日期（ISO YYYY-MM-DD）；非日期名（历史手工目录）不参与清理
            let iso = name.len() == 10 && name.as_bytes().get(4) == Some(&b'-');
            iso.then_some((name, e.path()))
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// 目录树总大小（字节；读取失败的部分按 0 计，不阻断巡检）
fn dir_size(path: &Path) -> u64 {
    let mut total = 0u64;
    let Ok(rd) = std::fs::read_dir(path) else {
        return 0;
    };
    for entry in rd.filter_map(|e| e.ok()) {
        let p = entry.path();
        let Ok(meta) = std::fs::symlink_metadata(&p) else {
            continue;
        };
        if meta.is_dir() {
            total += dir_size(&p);
        } else {
            total += meta.len();
        }
    }
    total
}

/// 今天（ISO 日期，本地时区）
fn today() -> String {
    crate::types::now_iso().chars().take(10).collect::<String>()
}

/// 日期字符串年龄（天）：与今天之差；解析失败按 0（不动，保守）
fn age_days(day: &str) -> u64 {
    let t = today();
    let parse = |s: &str| -> Option<(i64, i64, i64)> {
        let b = s.as_bytes();
        if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
            return None;
        }
        let y: i64 = s.get(0..4)?.parse().ok()?;
        let m: i64 = s.get(5..7)?.parse().ok()?;
        let d: i64 = s.get(8..10)?.parse().ok()?;
        Some((y, m, d))
    };
    match (parse(day), parse(&t)) {
        (Some((y1, m1, d1)), Some((y2, m2, d2))) => {
            // 简化儒略日（Fliegel & Van Flandern 公式，不依赖 chrono）：天数差即年龄
            let jd = |y: i64, m: i64, d: i64| 367 * y - (7 * (y + (m + 9)) / 12) / 4 + (275 * m) / 9 + d + 1721013;
            (jd(y2, m2, d2) - jd(y1, m1, d1)).max(0) as u64
        }
        _ => 0,
    }
}

/// 执行一轮清理（幂等；供周期巡检与手动触发共用）。
/// 返回巡检结果；目录删除失败按跳过计（不阻断其余目录的清理）。
pub fn run_cleanup(core: &Arc<Core>, cfg: &TrashConfig) -> CleanupReport {
    let root = trash_root(core);
    let mut report = CleanupReport::default();
    let mut total: u64 = 0;
    let mut dirs = day_dirs(&root);
    // 各目录大小缓存（容量淘汰阶段复用，避免重复遍历）
    let mut sizes: Vec<u64> = Vec::with_capacity(dirs.len());
    for (_, p) in &dirs {
        let s = dir_size(p);
        sizes.push(s);
        total += s;
    }

    // 约束一：保留期——超期目录整删
    if cfg.retention_days > 0 {
        let mut i = 0;
        while i < dirs.len() {
            let (day, path) = &dirs[i];
            if age_days(day) > cfg.retention_days {
                if let Ok(()) = std::fs::remove_dir_all(path) {
                    report.removed_days.push(day.clone());
                    report.expired_removed += 1;
                    report.freed_bytes += sizes[i];
                    total -= total.min(sizes[i]);
                    dirs.remove(i);
                    sizes.remove(i);
                    continue;
                }
            }
            i += 1;
        }
    }

    // 约束二：容量上限——最旧优先淘汰直至回到上限内
    if cfg.max_bytes > 0 {
        let mut i = 0;
        while total > cfg.max_bytes && i < dirs.len() {
            let (day, path) = &dirs[i];
            if std::fs::remove_dir_all(path).is_ok() {
                report.removed_days.push(day.clone());
                report.overflow_removed += 1;
                report.freed_bytes += sizes[i];
                total -= total.min(sizes[i]);
                dirs.remove(i);
                sizes.remove(i);
                continue;
            }
            i += 1;
        }
    }

    report.total_bytes_after = total;
    report
}

/// 周期巡检（网关侧 spawn 的循环体；1h 间隔 + 启动错峰）。
/// 有清理动作时落审计（kind=trash_cleanup）；空轮不刷审计噪音。
pub fn cleanup_once(core: &Arc<Core>) -> CleanupReport {
    let cfg = core.config().trash.clone();
    let report = run_cleanup(core, &cfg);
    if !report.removed_days.is_empty() {
        let _ = core.store.audit_tool(
            "system",
            "trash_cleanup",
            &serde_json::json!({
                "removedDays": report.removed_days,
                "freedBytes": report.freed_bytes,
                "expiredRemoved": report.expired_removed,
                "overflowRemoved": report.overflow_removed,
            }),
            &format!(
                "回收站清理：删除 {} 个日期目录（超期 {} / 超容 {}），释放 {}",
                report.removed_days.len(),
                report.expired_removed,
                report.overflow_removed,
                humansize(report.freed_bytes)
            ),
            0,
        );
    }
    report
}

/// 字节数的人读格式（审计摘要用；不引依赖）
fn humansize(bytes: u64) -> String {
    let units = ["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u < units.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{bytes} B")
    } else {
        format!("{:.1} {}", v, units[u])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (Arc<Core>, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("exm-trash-{}", crate::types::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut cfg = crate::config::ExmConfig::load(&dir);
        cfg.use_mock = true;
        cfg.memory_enabled = false;
        let core = Arc::new(crate::Core::with_config(cfg).expect("创建 Core 失败"));
        let root = trash_root(&core);
        std::fs::create_dir_all(&root).unwrap();
        (core, root)
    }

    fn mk_day(root: &Path, day: &str, bytes: usize) {
        let d = root.join(day);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("f.bin"), vec![0u8; bytes]).unwrap();
    }

    /// 超期目录清理：超过保留期的日期目录被删，保留期内不动
    #[test]
    fn 超期清理_保留期内不动() {
        let (core, root) = setup();
        mk_day(&root, "2020-01-01", 10); // 远古：超任何保留期
        mk_day(&root, &today(), 10); // 今天：任何保留期内
        let cfg = TrashConfig {
            retention_days: 7,
            max_bytes: 0, // 关容量约束，单测保留期
        };
        let r = run_cleanup(&core, &cfg);
        assert_eq!(r.expired_removed, 1, "只删超期目录");
        assert!(!root.join("2020-01-01").exists());
        assert!(root.join(today()).exists(), "保留期内不动");
    }

    /// 超容淘汰最旧：总量超上限时最旧日期目录先走
    #[test]
    fn 超容淘汰_最旧优先() {
        let (core, root) = setup();
        mk_day(&root, "2020-01-01", 4096);
        mk_day(&root, "2021-01-01", 4096);
        mk_day(&root, "2022-01-01", 4096);
        let cfg = TrashConfig {
            retention_days: 0, // 关保留期，单测容量
            max_bytes: 5 * 1024, // 5KB：删一个（余 8192）仍超，须删两个（余 4096）
        };
        let r = run_cleanup(&core, &cfg);
        assert_eq!(r.overflow_removed, 2, "应淘汰至回到上限内：{r:?}");
        assert!(!root.join("2020-01-01").exists(), "最旧先走");
        assert!(root.join("2022-01-01").exists(), "最新保留");
        assert!(r.total_bytes_after <= cfg.max_bytes);
    }

    /// 配置热生效：retention_days 收紧后同状态下一轮口径变化
    #[test]
    fn 配置热生效_收紧口径() {
        let (core, root) = setup();
        // 造一个 3 天前、一个 6 天前的目录
        let t = today();
        let shift = |days: i64| -> String {
            let jd = |y: i64, m: i64, d: i64| 367 * y - (7 * (y + (m + 9) / 12)) / 4 + (275 * m) / 9 + d + 1721013;
            let (y, m, d) = parse_iso(&t).unwrap();
            let target = jd(y, m, d) - days;
            jd_to_iso(target)
        };
        mk_day(&root, &shift(3), 10);
        mk_day(&root, &shift(6), 10);
        // 7 天口径：都不动
        let r = run_cleanup(
            &core,
            &TrashConfig {
                retention_days: 7,
                max_bytes: 0,
            },
        );
        assert_eq!(r.expired_removed, 0);
        // 收紧到 4 天：6 天前的走
        let r = run_cleanup(
            &core,
            &TrashConfig {
                retention_days: 4,
                max_bytes: 0,
            },
        );
        assert_eq!(r.expired_removed, 1);
        assert!(!root.join(shift(6)).exists());
        assert!(root.join(shift(3)).exists());
    }

    // ---- 测试辅助：ISO 日期换算（与 age_days 同源的逆运算） ----
    fn parse_iso(s: &str) -> Option<(i64, i64, i64)> {
        let b = s.as_bytes();
        if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
            return None;
        }
        Some((
            s.get(0..4)?.parse().ok()?,
            s.get(5..7)?.parse().ok()?,
            s.get(8..10)?.parse().ok()?,
        ))
    }

    fn jd_to_iso(jd: i64) -> String {
        // Fliegel & Van Flandern 逆公式
        let l = jd + 68569;
        let n = (4 * l) / 146097;
        let l = l - (146097 * n + 3) / 4;
        let i = (4000 * (l + 1)) / 1461001;
        let l = l - (1461 * i) / 4 + 31;
        let j = (80 * l) / 2447;
        let d = l - (2447 * j) / 80;
        let l = j / 11;
        let m = j + 2 - 12 * l;
        let y = 100 * (n - 49) + i + l;
        format!("{y:04}-{m:02}-{d:02}")
    }
}
