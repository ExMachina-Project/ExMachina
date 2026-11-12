//! 自动化调度 —— 定时任务存储与 cron 匹配（docs/10）
//!
//! 任务 = 数据（`<data_dir>/cron/jobs.json`，原子写）；运行记录 = 追加日志（`runs.jsonl`）。
//! 匹配器为最小五段实现（分 时 日 月 周），不引入外部 cron 依赖。

use crate::types::{CronJob, CronRun};
use anyhow::{Context, Result};
use chrono::{DateTime, Datelike, Timelike, Utc};
use std::io::Write;
use std::path::{Path, PathBuf};

pub struct CronStore {
    dir: PathBuf,
}

impl CronStore {
    pub fn open(data_dir: impl AsRef<Path>) -> Result<Self> {
        let dir = data_dir.as_ref().join("cron");
        std::fs::create_dir_all(&dir)?;
        Ok(CronStore { dir })
    }

    fn jobs_path(&self) -> PathBuf {
        self.dir.join("jobs.json")
    }

    fn runs_path(&self) -> PathBuf {
        self.dir.join("runs.jsonl")
    }

    pub fn list(&self) -> Result<Vec<CronJob>> {
        let p = self.jobs_path();
        if !p.exists() {
            return Ok(vec![]);
        }
        serde_json::from_str(&std::fs::read_to_string(&p)?)
            .with_context(|| format!("解析定时任务失败: {}", p.display()))
    }

    /// 原子写全量任务表（临时文件 + 改名）
    pub fn save(&self, jobs: &[CronJob]) -> Result<()> {
        let tmp = self.dir.join("jobs.json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(jobs)?)?;
        std::fs::rename(&tmp, self.jobs_path())?;
        Ok(())
    }

    /// 新增/更新任务；id 缺省自动生成，created_at 缺省补当前时间
    pub fn upsert(&self, mut job: CronJob) -> Result<CronJob> {
        let mut jobs = self.list()?;
        if job.id.trim().is_empty() {
            job.id = crate::types::new_id()[..8].to_string();
        }
        if job.created_at.is_empty() {
            job.created_at = crate::types::now_iso();
        }
        match jobs.iter_mut().find(|j| j.id == job.id) {
            Some(slot) => *slot = job.clone(),
            None => jobs.push(job.clone()),
        }
        self.save(&jobs)?;
        Ok(job)
    }

    pub fn remove(&self, id: &str) -> Result<bool> {
        let mut jobs = self.list()?;
        let before = jobs.len();
        jobs.retain(|j| j.id != id);
        if jobs.len() == before {
            return Ok(false);
        }
        self.save(&jobs)?;
        Ok(true)
    }

    pub fn get(&self, id: &str) -> Result<Option<CronJob>> {
        Ok(self.list()?.into_iter().find(|j| j.id == id))
    }

    pub fn record_run(&self, run: &CronRun) -> Result<()> {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.runs_path())?;
        writeln!(f, "{}", serde_json::to_string(run)?)?;
        Ok(())
    }

    /// 运行记录（可按任务过滤，按时间倒序取最近 limit 条）
    pub fn runs(&self, job_id: Option<&str>, limit: usize) -> Result<Vec<CronRun>> {
        let p = self.runs_path();
        if !p.exists() {
            return Ok(vec![]);
        }
        let mut runs: Vec<CronRun> = std::fs::read_to_string(&p)?
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect();
        if let Some(jid) = job_id {
            runs.retain(|r| r.job_id == jid);
        }
        runs.sort_by(|a, b| b.started_at.cmp(&a.started_at));
        runs.truncate(limit);
        Ok(runs)
    }
}

/// 当前分钟的去重键：同一分钟内一个任务只触发一次
pub fn minute_key(now: DateTime<Utc>) -> String {
    now.format("%Y%m%d%H%M").to_string()
}

/// 任务是否到期：启用 + 未在本分钟跑过 +（cron 命中 或 一次性 at 已到）
pub fn job_due(job: &CronJob, now: DateTime<Utc>) -> bool {
    if !job.enabled {
        return false;
    }
    if job.last_run_minute.as_deref() == Some(&minute_key(now)) {
        return false;
    }
    if let Some(expr) = &job.cron {
        return cron_matches(expr, now);
    }
    if let Some(at) = &job.at {
        return DateTime::parse_from_rfc3339(at)
            .map(|t| now >= t.with_timezone(&Utc))
            .unwrap_or(false);
    }
    false
}

/// 五段 cron 匹配：分(0-59) 时(0-23) 日(1-31) 月(1-12) 周(0-6，0=周日)
pub fn cron_matches(expr: &str, now: DateTime<Utc>) -> bool {
    cron_matches_local(expr, &now.with_timezone(&chrono_tz_shim::UTC))
}

/// 五段 cron 匹配（本地时区语义）：表达式按给定时区的墙上时钟解释。
/// 时区为 None/非法名 = UTC（既有行为）。返回 (是否命中, 是否按非 UTC 解释)。
pub fn cron_matches_in(expr: &str, now: DateTime<Utc>, tz_name: &str) -> bool {
    match parse_tz(tz_name) {
        Some(tz) => cron_matches_local(expr, &now.with_timezone(&tz)),
        None => cron_matches_local(expr, &now.with_timezone(&chrono_tz_shim::UTC)),
    }
}

/// 解析 IANA 时区名（空/非法 = None → 调用方回落 UTC）
pub fn parse_tz(tz_name: &str) -> Option<chrono_tz::Tz> {
    tz_name
        .trim()
        .parse::<chrono_tz::Tz>()
        .ok()
        .filter(|_| !tz_name.trim().is_empty())
}

/// 内部统一实现：对任意时区 DateTime 做五段匹配（cron.rs 既有字段匹配复用）
fn cron_matches_local<Tz: chrono::TimeZone>(expr: &str, now: &chrono::DateTime<Tz>) -> bool {
    let fields: Vec<&str> = expr.split_whitespace().collect();
    if fields.len() != 5 {
        return false;
    }
    field_matches(fields[0], now.minute(), 0, 59)
        && field_matches(fields[1], now.hour(), 0, 23)
        && field_matches(fields[2], now.day(), 1, 31)
        && field_matches(fields[3], now.month(), 1, 12)
        && field_matches(fields[4], now.weekday().num_days_from_sunday(), 0, 7)
}

/// 任务是否到期（时区变体）：cron 按 `tz_name` 的墙上时钟解释；
/// at 一次性不受影响（ISO8601 自带偏移）。空/非法时区名 = UTC（既有行为）。
pub fn job_due_in(job: &CronJob, now: DateTime<Utc>, tz_name: &str) -> bool {
    if !job.enabled {
        return false;
    }
    if job.last_run_minute.as_deref() == Some(&minute_key(now)) {
        return false;
    }
    if let Some(expr) = &job.cron {
        return cron_matches_in(expr, now, tz_name);
    }
    if let Some(at) = &job.at {
        return DateTime::parse_from_rfc3339(at)
            .map(|t| now >= t.with_timezone(&Utc))
            .unwrap_or(false);
    }
    false
}

/// 下次执行时间（时区变体）：从 from 的下一分钟起在本地墙上时钟语义下扫描。
/// 返回 UTC 时刻（调用方按需转本地展示）。
pub fn next_run_in(
    expr: &str,
    from: DateTime<Utc>,
    tz_name: &str,
    max_minutes: u32,
) -> Option<DateTime<Utc>> {
    if expr.split_whitespace().count() != 5 {
        return None;
    }
    let tz = parse_tz(tz_name).unwrap_or(chrono_tz::Tz::UTC);
    let mut t = from + chrono::Duration::minutes(1);
    for _ in 0..max_minutes {
        if cron_matches_local(expr, &t.with_timezone(&tz)) {
            return Some(t);
        }
        t += chrono::Duration::minutes(1);
    }
    None
}

/// chrono_tz 的 UTC 常量便捷引用（避免每个调用点直接依赖 chrono_tz）
mod chrono_tz_shim {
    pub use chrono_tz::UTC;
}

/// 最近一次命中时间：从 from 的下一分钟起逐分钟扫描（上限 max_minutes）。
/// 五段匹配为纯算术，最坏 52.7 万次迭代在毫秒级——不引入 cron 解析依赖。
pub fn next_run(expr: &str, from: DateTime<Utc>, max_minutes: u32) -> Option<DateTime<Utc>> {
    if expr.split_whitespace().count() != 5 {
        return None;
    }
    let mut t = from + chrono::Duration::minutes(1);
    for _ in 0..max_minutes {
        if cron_matches(expr, t) {
            return Some(t);
        }
        t += chrono::Duration::minutes(1);
    }
    None
}

/// 单字段匹配：`*` | 数字 | `a-b` | 列表 `a,b,c` | 步进 `*/n`、`a-b/n`
fn field_matches(field: &str, value: u32, lo: u32, hi: u32) -> bool {
    for part in field.split(',') {
        let (range, step) = match part.split_once('/') {
            Some((r, s)) => (r, s.parse::<u32>().unwrap_or(1).max(1)),
            None => (part, 1),
        };
        let (start, end) = if range == "*" {
            (lo, hi)
        } else if let Some((a, b)) = range.split_once('-') {
            match (a.trim().parse::<u32>(), b.trim().parse::<u32>()) {
                (Ok(a), Ok(b)) => (a, b),
                _ => continue,
            }
        } else {
            match range.trim().parse::<u32>() {
                Ok(a) => (a, a),
                Err(_) => continue,
            }
        };
        if value < start || value > end {
            continue;
        }
        if step == 1 || (value - start).is_multiple_of(step) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, 0).unwrap()
    }

    #[test]
    fn cron匹配_五段与步进() {
        assert!(cron_matches("* * * * *", at(2026, 9, 12, 10, 30)));
        assert!(cron_matches("30 10 * * *", at(2026, 9, 12, 10, 30)));
        assert!(!cron_matches("30 10 * * *", at(2026, 9, 12, 10, 31)));
        assert!(cron_matches("*/15 * * * *", at(2026, 9, 12, 10, 45)));
        assert!(!cron_matches("*/15 * * * *", at(2026, 9, 12, 10, 50)));
        assert!(cron_matches("0 9 * * 1-5", at(2026, 9, 14, 9, 0))); // 周一
        assert!(!cron_matches("0 9 * * 1-5", at(2026, 9, 13, 9, 0))); // 周日
        assert!(cron_matches(
            "5,25 8-18/2 1,15 * *",
            at(2026, 9, 15, 10, 25)
        ));
        assert!(!cron_matches("bad expr", at(2026, 9, 12, 10, 30)));
    }

    #[test]
    fn 下次执行时间_命中与无命中() {
        // 每日 9:00：从 10:30 起最近命中是次日 9:00
        let from = at(2026, 10, 7, 10, 30);
        let next = next_run("0 9 * * *", from, 527040).unwrap();
        assert_eq!(next, at(2026, 10, 8, 9, 0));
        // 每小时整点：从 10:30 起最近命中 11:00
        let next = next_run("0 * * * *", from, 60).unwrap();
        assert_eq!(next, at(2026, 10, 7, 11, 0));
        // 非法表达式 / 无命中
        assert!(next_run("bad expr", from, 10).is_none());
        assert!(
            next_run("0 9 * * *", from, 10).is_none(),
            "上限内无命中返回 None"
        );
    }

    #[test]
    fn 任务存取与到期判定() {
        let dir = std::env::temp_dir().join(format!("exm-cron-{}", crate::types::new_id()));
        let store = CronStore::open(&dir).unwrap();

        let job = store
            .upsert(CronJob {
                id: String::new(),
                name: "每日巡检".into(),
                prompt: "巡检任务".into(),
                cron: Some("0 9 * * *".into()),
                at: None,
                kind: None,
                target_session: None,
                steps: Vec::new(),
                group: None,
                session_title: None,
                enabled: true,
                last_run_at: None,
                last_status: None,
                last_run_minute: None,
                notify_channels: Vec::new(),
                created_at: String::new(),
            })
            .unwrap();
        assert_eq!(job.id.len(), 8, "id 应自动生成");

        // 同一分钟去重：每分钟任务在本分钟只触发一次
        let now = at(2026, 9, 13, 9, 0);
        assert!(job_due(&job, now));
        let mut ran = job.clone();
        ran.last_run_minute = Some(minute_key(now));
        assert!(!job_due(&ran, now));
        // cron 命中下一分钟（每日 9:00 的任务在 9:01 不命中，换每分钟任务验证）
        let mut every_min = ran.clone();
        every_min.cron = Some("* * * * *".into());
        assert!(job_due(&every_min, at(2026, 9, 13, 9, 1)));
        assert!(!cron_matches(
            job.cron.as_deref().unwrap(),
            at(2026, 9, 13, 9, 1)
        ));

        // 一次性任务：到期触发、停用后不触发
        let mut once = job.clone();
        once.cron = None;
        once.at = Some("2026-09-13T08:00:00Z".into());
        assert!(job_due(&once, now));
        once.enabled = false;
        assert!(!job_due(&once, now));

        // 运行记录
        store
            .record_run(&CronRun {
                id: "r1".into(),
                job_id: job.id.clone(),
                job_name: job.name.clone(),
                session_id: "s1".into(),
                started_at: now.to_rfc3339(),
                finished_at: now.to_rfc3339(),
                status: "done".into(),
                summary: "ok".into(),
            })
            .unwrap();
        assert_eq!(store.runs(Some(&job.id), 10).unwrap().len(), 1);

        assert!(store.remove(&job.id).unwrap());
        assert!(!store.remove(&job.id).unwrap());

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---------------- cron 时区（assistant-parity T1） ----------------

    /// 时区化匹配：「每天 8 点」在 Asia/Shanghai 的 8 点 = UTC 0 点命中
    #[test]
    fn 时区匹配_上海八点即utc零点() {
        let utc_0 = at(2026, 11, 12, 0, 0); // 北京时间 08:00
        // UTC 语义下不命中（8 点才命中）
        assert!(!cron_matches("0 8 * * *", utc_0));
        // Shanghai 语义下命中
        assert!(cron_matches_in("0 8 * * *", utc_0, "Asia/Shanghai"));
        // UTC 语义下 8 点命中（旧行为不变）
        let utc_8 = at(2026, 11, 12, 8, 0);
        assert!(cron_matches("0 8 * * *", utc_8));
        assert!(!cron_matches_in("0 8 * * *", utc_8, "Asia/Shanghai")); // 北京 16 点
    }

    /// 空/非法时区名回落 UTC（既有行为零回归）
    #[test]
    fn 时区解析_空与非法回落utc() {
        assert!(parse_tz("").is_none());
        assert!(parse_tz("  ").is_none());
        assert!(parse_tz("Not/AZone").is_none());
        assert!(parse_tz("Asia/Shanghai").is_some());
        // 回落语义：空名 = UTC 匹配
        let utc_8 = at(2026, 11, 12, 8, 0);
        assert!(cron_matches_in("0 8 * * *", utc_8, ""));
        assert!(cron_matches_in("0 8 * * *", utc_8, "bad/tz"));
    }

    /// job_due_in：cron 按配置时区解释；at 不受时区影响（自带偏移）
    #[test]
    fn 到期判定_时区变体() {
        let mut job = crate::types::CronJob {
            id: "t1".into(),
            name: "早报".into(),
            prompt: "天气".into(),
            cron: Some("0 8 * * *".into()),
            at: None,
            kind: None,
            target_session: None,
            steps: Vec::new(),
            group: None,
            session_title: None,
            enabled: true,
            last_run_at: None,
            last_status: None,
            last_run_minute: None,
            notify_channels: Vec::new(),
            created_at: String::new(),
        };
        // UTC 0 点 = 上海 8 点：时区配置下应到期
        let now = at(2026, 11, 12, 0, 0);
        assert!(!job_due(&job, now), "UTC 语义下不到点");
        assert!(job_due_in(&job, now, "Asia/Shanghai"), "上海语义下到点");
        // 停用/分钟去重语义与 job_due 同源
        job.enabled = false;
        assert!(!job_due_in(&job, now, "Asia/Shanghai"));
        job.enabled = true;
        job.last_run_minute = Some(minute_key(now));
        assert!(!job_due_in(&job, now, "Asia/Shanghai"));
        // at 一次性：ISO8601 带偏移，时区配置无影响
        job.cron = None;
        job.enabled = true;
        job.last_run_minute = None;
        job.at = Some("2026-11-11T23:00:00Z".into()); // 已过
        assert!(job_due_in(&job, now, "Asia/Shanghai"));
        assert!(job_due_in(&job, now, ""));
    }

    /// next_run_in：下次执行按本地语义扫描，返回 UTC 时刻
    #[test]
    fn 下次执行_时区变体() {
        let from = at(2026, 10, 7, 10, 30); // UTC = 上海 18:30
        // 每日上海 8:00 的任务：下一次 = UTC 次日 0:00
        let next = next_run_in("0 8 * * *", from, "Asia/Shanghai", 527_040).unwrap();
        assert_eq!(next, at(2026, 10, 8, 0, 0));
        // 空 = 既有 UTC 语义（每日 8:00 UTC = 次日 UTC 8:00）
        let next_utc = next_run_in("0 8 * * *", from, "", 527_040).unwrap();
        assert_eq!(next_utc, at(2026, 10, 8, 8, 0));
    }
}
