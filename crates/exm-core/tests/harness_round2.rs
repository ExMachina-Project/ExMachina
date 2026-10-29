//! harness 工具链第二轮回归（change: harness-tools-round2）：
//! 1. terminal 白名单扩充：新命令免审批、联网类仍受沙箱闸门、docker 子命令约束
//! 2. grep contextLines：上下文块输出
//! 3. read 图像：images 副产物回灌
//! 4. terminal op=list：后台任务跨轮次可寻址（会话归属过滤）

use exm_core::tools::{is_network_command, ToolGateway};
use exm_core::types::ToolName;
use exm_core::{config::ExmConfig, Core};

fn mock_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("exm-h2-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// 在临时目录建 Core（数据落临时库），返回 strict 沙箱形态的网关
fn gateway(dir: &std::path::PathBuf, sandbox_net: bool) -> ToolGateway {
    let cfg = ExmConfig::load(&dir);
    let core = Core::with_config(cfg).expect("创建 Core 失败");
    let (events, _rx) = tokio::sync::broadcast::channel(64);
    // 审批等待压到 2s：联网拦截测试走「等待超时受阻回流」路径，不等 120s
    let sec = exm_core::config::SecurityConfig {
        approval_wait_secs: 2,
        ..Default::default()
    };
    let sb = exm_core::config::SandboxConfig {
        mode: "strict".into(),
        allow_network: sandbox_net,
        ..Default::default()
    };
    ToolGateway::new(
        core.config().workspace_root.clone(),
        core.store.clone(),
        core.registry().clone(),
        events,
        sec,
    )
    .with_sandbox(sb)
}

/// 生成一张 1×1 PNG（最小合法头 + IHDR + IEND，无需图像库）
fn tiny_png(path: &std::path::Path) {
    // 1x1 黑色像素 PNG 的完整字节（67 字节，合法文件）
    let bytes: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49,
        0x48, 0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06,
        0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44,
        0x41, 0x54, 0x78, 0x9C, 0x62, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D,
        0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42,
        0x60, 0x82,
    ];
    std::fs::write(path, bytes).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn 白名单扩充_新命令免审批直执() {
    let dir = mock_dir("allow");
    let gw = gateway(&dir, false);
    for cmd in [
        "stat .",
        "jq --version",
        "tar --version",
        "sqlite3 --version",
        "awk --version",
    ] {
        let r = gw
            .execute(
                "coding-agent",
                "ses-h2-a",
                &[ToolName::Terminal],
                ToolName::Terminal,
                &serde_json::json!({ "command": cmd, "timeoutSecs": 10 }),
            )
            .await;
        assert!(r.ok, "白名单命令应放行直执：{cmd}，错误 {:?}", r.error);
        assert!(
            !r.error.unwrap_or_default().contains("审批"),
            "白名单命令不应转审批：{cmd}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn 白名单扩充_联网命令仍受沙箱闸门() {
    let dir = mock_dir("net");
    let gw = gateway(&dir, false);
    // curl/ssh 在白名单内，但 strict 沙箱 + 未开联网 → 仍转审批
    for cmd in ["curl https://example.com", "ssh host ls"] {
        let r = gw
            .execute(
                "coding-agent",
                "ses-h2-b",
                &[ToolName::Terminal],
                ToolName::Terminal,
                &serde_json::json!({ "command": cmd, "timeoutSecs": 10 }),
            )
            .await;
        assert!(!r.ok, "联网命令在 strict 未开联网时应拦截：{cmd}");
        assert!(
            r.error.unwrap_or_default().contains("审批"),
            "拦截原因应是转审批：{cmd}"
        );
    }
    // is_network_command 覆盖新增联网命令
    for mark in [
        "curl -s x",
        "wget x",
        "rsync -a a b:",
        "sftp host",
        "git push origin dev",
        "docker pull nginx",
    ] {
        assert!(is_network_command(mark), "联网特征应覆盖：{mark}");
    }
    // 反例：本地命令不误伤
    for ok in ["stat src/main.rs", "jq . package.json", "tar -tf a.tar"] {
        assert!(!is_network_command(ok), "本地命令不应误判联网：{ok}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn 白名单扩充_docker子命令约束() {
    let dir = mock_dir("docker");
    let gw = gateway(&dir, false);
    // docker ps 在约束内 → 白名单放行（本地 docker 调用不触发联网闸门需联网特征，
    // 但 "docker " 前缀属联网特征，strict 下仍转审批——这里验证白名单层面命中）
    let r_ps = gw
        .execute(
            "coding-agent",
            "ses-h2-c",
            &[ToolName::Terminal],
            ToolName::Terminal,
            &serde_json::json!({ "command": "docker ps", "timeoutSecs": 10 }),
        )
        .await;
    // strict 未开联网时 docker 前缀命中联网特征 → 拦截转审批（预期行为：docker 需外联守护进程）
    assert!(!r_ps.ok, "docker ps 在 strict 未开联网时应转审批");
    assert!(r_ps.error.unwrap_or_default().contains("审批"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn grep_上下文行_块输出() {
    let dir = mock_dir("grep");
    // 构造文件：命中行分散（第 2、9 行），contextLines=2 时各成块
    std::fs::write(
        dir.join("a.txt"),
        "line1\nline2 target\nline3\nline4\nline5\nline6\nline7\nline8\nline9 target\nline10\n",
    )
    .unwrap();
    let gw = gateway(&dir, false);
    let r = gw
        .execute(
            "coding-agent",
            "ses-h2-d",
            &[ToolName::Grep],
            ToolName::Grep,
            &serde_json::json!({ "pattern": "target", "path": ".", "contextLines": 2 }),
        )
        .await;
    assert!(r.ok, "grep 应成功：{:?}", r.error);
    let out = r.output;
    // 两块：2±2 与 9±2，块间以 — 分隔
    assert!(out.contains("a.txt:2:> line2 target"), "命中行标注 >：{out}");
    assert!(out.contains("a.txt:1:  line1"), "上下文行带行号：{out}");
    assert!(out.contains("—"), "多块间应有分隔符：{out}");
    assert!(out.contains("a.txt:9:> line9 target"));
    // 默认（无 contextLines）：单行输出、无分隔符
    let r2 = gw
        .execute(
            "coding-agent",
            "ses-h2-d",
            &[ToolName::Grep],
            ToolName::Grep,
            &serde_json::json!({ "pattern": "target", "path": "." }),
        )
        .await;
    assert!(r2.ok);
    assert!(!r2.output.contains("—"), "默认无上下文不应有块分隔：{}", r2.output);
    assert!(r2.output.contains("a.txt:2:line2 target"), "默认单行格式不变：{}", r2.output);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn read_图像_副产物回灌() {
    let dir = mock_dir("img");
    tiny_png(dir.join("shot.png").as_path());
    let gw = gateway(&dir, false);
    let r = gw
        .execute(
            "coding-agent",
            "ses-h2-e",
            &[ToolName::Read],
            ToolName::Read,
            &serde_json::json!({ "path": "shot.png" }),
        )
        .await;
    assert!(r.ok, "read 图像应成功：{:?}", r.error);
    assert!(!r.images.is_empty(), "images 应携带路径供回灌");
    assert!(r.output.contains("图像文件"), "应返回图像摘要：{}", r.output);
    // data URL 回灌链路
    let url = gw.read_image_data_url(&r.images[0]);
    assert!(url.is_some(), "图像路径应能转 data URL");
    assert!(url.unwrap().starts_with("data:image/png;base64,"));
    // 非图像二进制仍报错
    std::fs::write(dir.join("blob.bin"), [0u8, 159, 146, 150]).unwrap();
    let r2 = gw
        .execute(
            "coding-agent",
            "ses-h2-e",
            &[ToolName::Read],
            ToolName::Read,
            &serde_json::json!({ "path": "blob.bin" }),
        )
        .await;
    assert!(!r2.ok, "非图像二进制应报错");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn 后台任务_list_跨轮次寻址() {
    let dir = mock_dir("bg");
    let gw = gateway(&dir, false);
    // 会话 A 启动后台任务
    let r = gw
        .execute(
            "coding-agent",
            "ses-A",
            &[ToolName::Terminal],
            ToolName::Terminal,
            &serde_json::json!({ "command": "sleep 30", "background": true }),
        )
        .await;
    assert!(r.ok, "后台启动应成功：{:?}", r.error);
    let task_id = r
        .output
        .lines()
        .find_map(|l| l.split("taskId=").nth(1).map(|s| s.trim().to_string()))
        .expect("启动输出应含 taskId");
    // 会话 B 的 list：看不到 A 的任务
    let rb = gw
        .execute(
            "coding-agent",
            "ses-B",
            &[ToolName::Terminal],
            ToolName::Terminal,
            &serde_json::json!({ "op": "list" }),
        )
        .await;
    assert!(rb.ok);
    assert!(rb.output.contains("无后台任务"), "会话隔离：B 不应看到 A 的任务：{}", rb.output);
    // 会话 A 的 list：可寻址
    let ra = gw
        .execute(
            "coding-agent",
            "ses-A",
            &[ToolName::Terminal],
            ToolName::Terminal,
            &serde_json::json!({ "op": "list" }),
        )
        .await;
    assert!(ra.ok);
    assert!(ra.output.contains(&task_id[..8]), "list 应含 taskId：{}", ra.output);
    assert!(ra.output.contains("sleep 30"), "list 应含命令：{}", ra.output);
    assert!(ra.output.contains("运行中") || ra.output.contains("已"), "list 应含状态：{}", ra.output);
    // kill 清理
    let _ = gw
        .execute(
            "coding-agent",
            "ses-A",
            &[ToolName::Terminal],
            ToolName::Terminal,
            &serde_json::json!({ "op": "kill", "taskId": task_id }),
        )
        .await;
}
