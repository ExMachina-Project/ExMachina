// CLI 版本参数（审计补面）：--version / -V 打印即退，不建运行时、不拉起实例。
// 覆盖两个 bin（exm / exmachina）与 clap 自动版本路径。
use std::process::{Command, Stdio};

fn bin_path(name: &str) -> std::path::PathBuf {
    // cargo test 时 CARGO_BIN_EXE_<name> 指向已构建的 bin
    std::path::PathBuf::from(
        std::env::var(format!("CARGO_BIN_EXE_{name}"))
            .unwrap_or_else(|_| format!("./target/debug/{name}")),
    )
}

#[test]
fn cli_version_flag_exits_zero() {
    for name in ["exm", "exmachina"] {
        let out = Command::new(bin_path(name))
            .arg("--version")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output()
            .expect("启动 bin 失败");
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        assert!(
            out.status.success(),
            "{name} --version 应退出码 0，实得 {:?}",
            out.status.code()
        );
        // 版本号来自 CARGO_PKG_VERSION（形如 "0.6.0"），不误拉起 TUI
        assert!(
            stdout.contains(env!("CARGO_PKG_VERSION")),
            "{name} --version 输出应含版本号，实得：{stdout}"
        );
    }
}

#[test]
fn cli_short_version_flag_matches_long() {
    let long = Command::new(bin_path("exm"))
        .arg("--version")
        .output()
        .expect("启动失败");
    let short = Command::new(bin_path("exm"))
        .arg("-V")
        .output()
        .expect("启动失败");
    assert_eq!(long.stdout, short.stdout, "--version 与 -V 输出应一致");
}
