//! exm-cli/tests/command_help_isolated —— capability-parity 2.7：
//! 入口矩阵里的每个 CLI 基础命令都要能在**隔离环境**（临时 workspace + 临时 HOME）
//! 中执行；`exm help` 与并发的目录查询（`exm command catalog`）必须来自同一目录快照，
//! 不允许帮助文本另有一份硬编码事实源。
//!
//! 这里验证的是「命令入口可执行 + 帮助无漂移」，不声称任何 handler 的成功副作用：
//! `--help` 由 clap 在进入业务逻辑前处理；真实执行只选只读命令（help / catalog / sessions list）。
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

struct Temp(PathBuf);

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

impl Temp {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!("exm-2-7-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).unwrap();
        Temp(path)
    }
}

fn run(ws: &Path, home: &Path, args: &[&str]) -> (bool, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_exm"))
        .arg("--workspace")
        .arg(ws)
        .args(args)
        // 隔离用户级路径：不读真实 ~/.exmachina，也不向真实 HOME 写任何状态
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_DATA_HOME", home.join(".local/share"))
        .current_dir(home)
        .output()
        .unwrap();
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// 从生成的入口矩阵里取所有 `cli.*` 行：返回 (目录名, 顶层子命令)。
/// 矩阵本身由 `exm command catalog --markdown` 生成，CLI 测试另有快照守护，
/// 所以解析它等于解析「文档承诺的 CLI 入口」。
fn documented_cli_entries(matrix: &str) -> Vec<(String, String)> {
    let mut rows = Vec::new();
    for line in matrix.lines() {
        let cells: Vec<&str> = line.split('|').map(str::trim).collect();
        if cells.len() < 4 {
            continue;
        }
        let name = cells[1].trim_matches('`');
        if !name.starts_with("cli.") {
            continue;
        }
        let usage = cells[2].trim_matches('`');
        let rest = usage
            .strip_prefix("exm ")
            .unwrap_or_else(|| panic!("CLI 行缺少 exm 前缀：{line}"));
        let sub = rest
            .split_whitespace()
            .next()
            .unwrap_or_else(|| panic!("CLI 行缺少子命令：{line}"));
        rows.push((name.to_string(), sub.to_string()));
    }
    rows
}

#[test]
fn documented_cli_entries_help_is_executable_and_matches_live_directory() {
    let matrix = include_str!("../../../docs/command-entry-matrix.md");
    let rows = documented_cli_entries(matrix);
    assert!(
        rows.len() >= 30,
        "入口矩阵的 CLI 行过少（{len}），解析可能失效",
        len = rows.len()
    );

    let ws = Temp::new("ws");
    let home = Temp::new("home");

    // ① 每个文档化 CLI 入口都在隔离环境里可执行 `--help`（clap 层，不写状态）。
    let mut subs = BTreeSet::new();
    for (name, sub) in &rows {
        let (ok, stdout, stderr) = run(&ws.0, &home.0, &[sub, "--help"]);
        assert!(
            ok,
            "{name} (`exm {sub} --help`) 隔离执行失败\nstdout:{stdout}\nstderr:{stderr}"
        );
        assert!(
            stdout.to_lowercase().contains("usage") || stdout.contains("用法"),
            "{name} 的 --help 没有用法段：{stdout}"
        );
        subs.insert(sub.clone());
    }

    // ② 帮助与实际目录同源：两个命令各自查询同一 Core，JSON 必须逐字节同构。
    let (ok_help, help_json, err_help) = run(&ws.0, &home.0, &["--output-format", "json", "help"]);
    assert!(ok_help, "exm help 失败：{err_help}");
    let (ok_cat, cat_json, err_cat) = run(
        &ws.0,
        &home.0,
        &["--output-format", "json", "command", "catalog"],
    );
    assert!(ok_cat, "exm command catalog 失败：{err_cat}");
    let help: Value = serde_json::from_str(&help_json).expect("help JSON");
    let catalog: Value = serde_json::from_str(&cat_json).expect("catalog JSON");
    assert_eq!(help, catalog, "`exm help` 与 `exm command catalog` 漂移");

    // ③ 文档里的每个 CLI 入口与每个规范基础命令都必须出现在帮助快照中。
    let names: BTreeSet<&str> = help["entries"]
        .as_array()
        .expect("entries")
        .iter()
        .filter_map(|entry| entry["descriptor"]["name"].as_str())
        .collect();
    for (name, _) in &rows {
        assert!(
            names.contains(name.as_str()),
            "帮助缺少文档中的 CLI 入口 {name}"
        );
    }
    for line in matrix.lines() {
        let cells: Vec<&str> = line.split('|').map(str::trim).collect();
        if cells.len() < 4 {
            continue;
        }
        let name = cells[1].trim_matches('`');
        if name.starts_with("cli.") || name.is_empty() || name == "命令" || name.starts_with('-')
        {
            continue;
        }
        assert!(names.contains(name), "帮助缺少入口矩阵中的规范命令 {name}");
    }

    // ④ 真实只读执行（不是 --help）：同一隔离环境内退出码为 0。
    for args in [
        vec!["help"],
        vec!["command", "catalog"],
        vec!["sessions", "list"],
    ] {
        let (ok, stdout, stderr) = run(&ws.0, &home.0, &args);
        assert!(ok, "隔离执行 exm {args:?} 失败：{stderr}");
        assert!(!stdout.trim().is_empty(), "exm {args:?} 无输出");
    }
}

#[test]
fn backend_self_check_reports_gaps_without_claiming_runtime_readiness() {
    let ws = Temp::new("backend-ws");
    let home = Temp::new("backend-home");
    let (ok, stdout, stderr) = run(
        &ws.0,
        &home.0,
        &["--output-format", "json", "backend", "list"],
    );
    assert!(ok, "{stderr}");
    let list: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(list["schemaVersion"], 1);
    let items = list["backends"].as_array().unwrap();
    let ids: BTreeSet<_> = items
        .iter()
        .map(|item| item["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        ["bubblewrap", "container", "local-managed", "remote"]
            .into_iter()
            .collect()
    );
    for id in ["container", "remote"] {
        let (ok, stdout, stderr) = run(
            &ws.0,
            &home.0,
            &["--output-format", "json", "backend", "check", id],
        );
        assert!(ok, "{stderr}");
        let check: Value = serde_json::from_str(&stdout).unwrap();
        assert_eq!(check["backend"]["id"], id);
        assert_eq!(check["ready"], false, "二进制存在不能冒充运行适配器已验证");
        assert_eq!(check["backend"]["readiness"]["status"], "unverified");
        assert!(check["detail"].as_str().unwrap().contains("只证明二进制"));
    }
    let (ok, stdout, _) = run(
        &ws.0,
        &home.0,
        &[
            "--output-format",
            "json",
            "backend",
            "check",
            "not-a-backend",
        ],
    );
    assert!(!ok);
    assert!(serde_json::from_str::<Value>(&stdout).unwrap()["error"].is_object());
}
