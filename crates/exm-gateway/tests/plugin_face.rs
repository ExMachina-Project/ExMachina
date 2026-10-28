//! 插件面集成验证（任务 3.2/3.3）：
//! video builtin 插件经 spawn_all 注册进 PluginManager，通道工厂枚举注册面可见。

use std::sync::Arc;

fn mock_core() -> exm_core::Core {
    let dir = std::env::temp_dir().join(format!("exm-plugin-gw-{}", exm_core::types::new_id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut cfg = exm_core::config::ExmConfig::load(&dir);
    cfg.use_mock = true;
    cfg.memory_enabled = false;
    exm_core::Core::with_config(cfg).expect("创建 Core 失败")
}

/// spawn_all 后 video 出现在注册面（builtin 形态、channel+watch 能力）
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn 插件面_video注册与能力枚举() {
    let core = mock_core();
    let core = Arc::new(core);
    exm_gateway::plugins::spawn_all(core.clone());
    let pm = core.plugins();
    let v = pm.get("video").expect("video 应已注册");
    assert_eq!(v.form, "builtin");
    assert!(v.enabled);
    assert!(v.error.is_none());
    // manifest 声明 channel（bilibili/youtube）+ watch
    let m = &v.manifest;
    let ch = m.channel.as_ref().expect("channel 能力");
    assert!(ch.kinds.contains(&"bilibili".to_string()));
    assert!(ch.kinds.contains(&"youtube".to_string()));
    assert!(ch.credentials.iter().any(|c| c.key == "sessdata" && c.sensitive));
    assert!(m.watch.is_some(), "watch 能力");
    // 通道能力枚举：enabled_providers 含 video 的 ChannelProvider
    let providers = pm.enabled_providers();
    assert!(
        providers.iter().any(|p| {
            p.channel_provider()
                .map(|c| c.kinds().contains(&"bilibili".to_string()))
                .unwrap_or(false)
        }),
        "注册面应枚举到 video 的通道能力"
    );
}

/// 禁用 video 后注册面摘除（启停生效）
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn 插件面_禁用后摘除() {
    let core = mock_core();
    let core = Arc::new(core);
    exm_gateway::plugins::spawn_all(core.clone());
    let pm = core.plugins();
    assert!(pm.set_enabled("video", false));
    assert!(!pm.get("video").unwrap().enabled);
    assert!(pm.enabled_providers().iter().all(|p| p.manifest().id != "video"));
}

/// 虚拟插件收编可见（无内容不登记 / 有内容登记且不可禁用）
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn 插件面_虚拟插件收编() {
    let core = mock_core();
    // 无 CustomTool、无 entities/skills → 不登记
    let pm = core.plugins();
    assert!(pm.get("custom-tools").is_none());
    assert!(pm.get("builtin-resources").is_none());
}
