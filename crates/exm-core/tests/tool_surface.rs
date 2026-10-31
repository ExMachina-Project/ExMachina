//! 验证：编成修复后，coding-agent / orchestrator / machina 的工具面
//! 确实包含提示词所教的 edit/grep/glob。
use exm_core::tools::ToolGateway;
use exm_core::types::{AgentDefinition, ToolName};
use std::path::PathBuf;

/// 仓库根（集成测试 cwd = crate 根）
fn repo_root() -> PathBuf {
    std::env::current_dir().unwrap().join("..").join("..")
}

fn keys(list: &[ToolName]) -> Vec<&'static str> {
    list.iter().map(|t| t.key()).collect()
}

#[test]
fn 编成白名单_提示词教到的工具全部在面() {
    let cases: &[(&str, &[&str])] = &[
        // coding-agent：提示词「优先 edit 精确替换」「先 grep/glob 摸清调用方」
        (
            "coding-agent",
            &[
                "read",
                "grep",
                "glob",
                "filesystem",
                "terminal",
                "edit",
                "patch",
            ],
        ),
        // orchestrator：编码工作流整节都在教 edit/grep/glob
        (
            "orchestrator",
            &[
                "read",
                "grep",
                "glob",
                "filesystem",
                "terminal",
                "edit",
                "web_search",
            ],
        ),
        // frontend-agent：同属实作域
        (
            "frontend-agent",
            &["read", "grep", "glob", "filesystem", "terminal", "edit"],
        ),
    ];
    for (id, need) in cases {
        let path = repo_root().join(format!("entities/groups/exmachina/agents/{id}.json"));
        let def: AgentDefinition =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let have = keys(&def.tools);
        for n in *need {
            assert!(
                have.contains(n),
                "{id} 工具面缺 {n}（现有 {have:?}）——提示词教的工具必须在白名单"
            );
        }
    }
}

#[test]
fn 单聊智能体_machina_工具面含编辑检索() {
    let def: AgentDefinition = serde_json::from_str(
        &std::fs::read_to_string(repo_root().join("entities/agents/machina/agent.json")).unwrap(),
    )
    .unwrap();
    let have = keys(&def.tools);
    for n in ["read", "grep", "glob", "edit", "filesystem", "terminal"] {
        assert!(have.contains(&n), "machina 缺 {n}（现有 {have:?}）");
    }
}

#[test]
fn specs_下发面_含全部开发工具() {
    let allowlist: Vec<ToolName> = [
        "read",
        "grep",
        "glob",
        "filesystem",
        "terminal",
        "edit",
        "patch",
    ]
    .iter()
    .filter_map(|k| ToolName::parse(k))
    .collect();
    let specs = ToolGateway::tool_specs(&allowlist, false, false, false);
    let names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();
    for n in [
        "read",
        "grep",
        "glob",
        "filesystem",
        "terminal",
        "edit",
        "patch",
    ] {
        assert!(names.contains(&n), "specs 未下发 {n}（现有 {names:?}）");
    }
}
