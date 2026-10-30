//! T2 进程插件工具面回归：拉起 → 注册 ToolGateway → specs 下发含命名空间工具。
//! 覆盖装配缺口（进程插件工具曾只存管理面快照，会话内不可见不可调）。

use exm_core::Core;
use std::path::PathBuf;

fn mock_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("exm-t2-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// 最小进程插件脚本：initialize 应答 serverInfo（exmMin 低门槛）+ tools/list 一项 hello
fn mock_script() -> &'static str {
    r#"import json, sys
def respond(req):
    m = req.get("method", "")
    if m == "initialize":
        return {"serverInfo": {"name": "t2", "version": "0.1.0", "exmMin": "0.5.0"}}
    if m == "tools/list":
        return {"tools": [{"name": "hello", "description": "打招呼", "parameters": {"type": "object"}}]}
    if m == "tools/call":
        return {"content": [{"type": "text", "text": "hello:" + str((req.get("params") or {}).get("name", "?"))}]}
    return {}
for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    req = json.loads(line)
    sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": req.get("id"), "result": respond(req)}) + "\n")
    sys.stdout.flush()
"#
}

fn write_plugin(dir: &PathBuf, id: &str) {
    let pdir = dir.join("plugins").join(id);
    std::fs::create_dir_all(&pdir).unwrap();
    std::fs::write(pdir.join("plugin.py"), mock_script()).unwrap();
    std::fs::write(
        pdir.join("plugin.json"),
        format!(r#"{{"id": "{id}", "name": "T2测试", "version": "0.1.0", "exmMin": "0.5.0", "process": {{"command": ["python3", "plugin.py"], "handshakeSecs": 10, "callTimeoutSecs": 20, "maxConcurrency": 2}}}}"#),
    )
    .unwrap();
}

#[tokio::test]
async fn 进程插件工具_下发面含命名空间工具() {
    let dir = mock_dir("specs");
    write_plugin(&dir, "t2p");

    let mut cfg = exm_core::config::ExmConfig::load(&dir);
    cfg.agents_dir = dir.join("entities");
    std::fs::create_dir_all(&cfg.agents_dir).unwrap();
    cfg.data_dir = dir.join("data");
    cfg.use_mock = true;
    let core = Core::with_config(cfg).unwrap();

    // 等装载完成（load_all 同步执行）
    let pm = core.plugins();
    assert!(pm.get("t2p").is_some(), "插件应装载");

    // 拉起 + 注册（复刻网关 spawn_all 的装配序列）
    let dir_p = pm.get("t2p").unwrap().dir.clone().unwrap();
    exm_core::plugin::bridge::spawn_process_plugin(
        &pm,
        &pm.get("t2p").unwrap().manifest,
        &dir_p,
        None,
    )
    .await
    .unwrap();
    // 网关侧回接（同 plugin_admin::attach_process_tools）
    if let Some(p) = pm.provider_of("t2p") {
        if let Some(tp) = p.tool_provider() {
            core.orchestrator()
                .tools
                .register_plugin_runtime_tools("t2p", tp);
        }
    }

    // 核心断言：specs 下发面含命名空间工具（任意个体可见）
    let specs = core.orchestrator().tools.custom_specs_for("machina");
    let names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();
    assert!(
        names.contains(&"t2p__hello"),
        "machina 的工具下发面应含 t2p__hello，实得: {names:?}"
    );

    // 热更新回归：apply_config 重建 orchestrator 后 T2 注册必须仍在
    // （曾只回接 T1 声明式——任何配置保存都让进程工具从会话面消失）
    let cfg2 = core.config().as_ref().clone();
    core.apply_config(cfg2).unwrap();
    let specs2 = core.orchestrator().tools.custom_specs_for("machina");
    let names2: Vec<&str> = specs2.iter().map(|s| s.name.as_str()).collect();
    assert!(
        names2.contains(&"t2p__hello"),
        "apply_config 后 t2p__hello 仍在下发面，实得: {names2:?}"
    );

    exm_core::plugin::bridge::stop_all_processes().await;
    let _ = std::fs::remove_dir_all(&dir);
}
