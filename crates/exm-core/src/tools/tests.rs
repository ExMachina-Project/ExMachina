//! tools/tests.rs —— 原 tools.rs 内联测试随拆分整体迁出（纯搬移）

use super::*;
mod memory_tool_tests {
    use super::*;
    use std::sync::Arc;
    use crate::store::Store;
    use crate::types::{AgentDefinition, GroupMeta, Tier};

    struct TestRig {
        gw: ToolGateway,
        dir: std::path::PathBuf,
    }

    fn rig() -> TestRig {
        let dir = std::env::temp_dir().join(format!("exm-tools-{}", crate::types::new_id()));
        std::fs::create_dir_all(dir.join("data")).unwrap();
        let store = Arc::new(Store::open(dir.join("data")).unwrap());
        let registry = Arc::new(LocalRegistry::new(dir.join("agents")).unwrap());
        // 自定义测试组（内置组定义受保护，不可增删个体）：注册主智能体与个体
        registry
            .create_group(Some("t".into()), "测试组", "")
            .unwrap();
        let _ = registry.upsert_agent(
            "t",
            crate::types::AgentDefinition {
                name: "orch-1".into(),
                identifier: "orch-1".into(),
                domain: "测试".into(),
                tier: Tier::Orchestrator,
                description: "测试主智能体".into(),
                capabilities: vec![],
                tools: vec![],
                when_to_call: String::new(),
                link: None,
                max_steps: None,
                dependencies: vec![],
                composable_with: vec![],
                input_schema: Default::default(),
                output_schema: Default::default(),
                prompt_file: String::new(),
                model_hint: None,
            },
            None,
        );
        registry.set_primary("t", "orch-1").unwrap();
        registry.set_active_group("t").unwrap();
        let (events, _rx) = tokio::sync::broadcast::channel(64);
        let memory = Arc::new(crate::memory::MemoryStore::open(dir.join("data")).unwrap());
        let gw = ToolGateway::new(&dir, store, registry, events, SecurityConfig::default())
            .with_memory(memory);
        TestRig { gw, dir }
    }

    /// 通道记忆隔离（memory_isolation）：memory_write/read 必须跟随会话分区，
    /// 否则隔离平台的会话会把知识写进共享组、或读到共享组的记忆——隔离形同虚设。
    #[test]
    fn 记忆隔离_工具读写跟随会话分区() {
        let r = rig();
        let mk = |title: &str, body: &str| serde_json::json!({ "kind": "fact", "title": title, "body": body });
        // 两个会话同组：shared 共享记忆；iso 开了隔离（模拟通道写入分区键）
        let shared = r.gw.store.create_session("共享会话", "t").unwrap();
        let iso = r.gw.store.create_session("隔离会话", "t").unwrap();
        r.gw.store
            .set_session_memory_partition(&iso.id, Some("t::channel:iso"))
            .unwrap();

        let a =
            r.gw.tool_memory_write("orch-1", &shared.id, &mk("共享暗号", "shared-secret"));
        assert!(a.ok, "{}", a.error.unwrap_or_default());
        let b =
            r.gw.tool_memory_write("orch-1", &iso.id, &mk("隔离暗号", "iso-secret"));
        assert!(b.ok, "{}", b.error.unwrap_or_default());

        // 写入落进各自命名空间（组键不同），互不可见
        let sees_shared = r.gw.tool_memory_read(
            "orch-1",
            &shared.id,
            &serde_json::json!({ "query": "暗号" }),
        );
        assert!(
            sees_shared.output.contains("共享暗号"),
            "共享会话应读到共享条目"
        );
        assert!(
            !sees_shared.output.contains("隔离暗号"),
            "共享会话不得读到隔离条目"
        );
        let sees_iso =
            r.gw.tool_memory_read("orch-1", &iso.id, &serde_json::json!({ "query": "暗号" }));
        assert!(
            sees_iso.output.contains("隔离暗号"),
            "隔离会话应读到隔离条目"
        );
        assert!(
            !sees_iso.output.contains("共享暗号"),
            "隔离会话不得读到共享条目"
        );

        // 关闭隔离立即回到共享：分区键被清除，随后写入/读取都归组
        r.gw.store
            .set_session_memory_partition(&iso.id, None)
            .unwrap();
        let back =
            r.gw.tool_memory_read("orch-1", &iso.id, &serde_json::json!({ "query": "暗号" }));
        assert!(back.output.contains("共享暗号"), "关闭隔离后应回到共享记忆");
    }

    fn def(identifier: &str, tier: Tier, tools: &[ToolName]) -> AgentDefinition {
        AgentDefinition {
            name: identifier.into(),
            identifier: identifier.into(),
            domain: "测试".into(),
            tier,
            description: "测试个体".into(),
            capabilities: vec![],
            link: None,
            max_steps: None,
            tools: tools.to_vec(),
            when_to_call: String::new(),
            dependencies: vec![],
            composable_with: vec![],
            input_schema: Default::default(),
            output_schema: Default::default(),
            prompt_file: String::new(),
            model_hint: None,
        }
    }

    fn group_meta(primary: &str, builtin: bool) -> GroupMeta {
        GroupMeta {
            id: "exmachina".into(),
            name: "测试组".into(),
            description: String::new(),
            primary: Some(primary.into()),
            workspace: None,
            model: None,
            capabilities: None,
            builtin,
            created_at: String::new(),
        }
    }

    #[test]
    fn 工具面快照_主智能体默认可见记忆三工具() {
        let primary = def("orch-1", Tier::Orchestrator, &[ToolName::Read]);
        let allow = ToolGateway::runtime_allowlist(&primary, Some(&group_meta("orch-1", true)));
        for t in [
            ToolName::MemoryRead,
            ToolName::MemoryWrite,
            ToolName::MemoryLink,
        ] {
            assert!(allow.contains(&t), "主智能体应默认获得 {t:?}（内置组）");
        }
        assert!(
            !allow.contains(&ToolName::AgentManage),
            "内置组主智能体不获得个体管理工具"
        );

        // 自定义组主智能体：记忆三工具 + 个体管理
        let allow2 = ToolGateway::runtime_allowlist(&primary, Some(&group_meta("orch-1", false)));
        assert!(allow2.contains(&ToolName::AgentManage));
        assert!(allow2.contains(&ToolName::MemoryWrite));
    }

    #[test]
    fn 工具面快照_个体未经编成授权不可见() {
        let unit = def("unit-1", Tier::Unit, &[ToolName::Read, ToolName::Terminal]);
        let allow = ToolGateway::runtime_allowlist(&unit, Some(&group_meta("orch-1", true)));
        assert!(
            !allow.contains(&ToolName::MemoryRead),
            "未授权个体不应看到记忆检索"
        );
        assert!(
            !allow.contains(&ToolName::MemoryWrite),
            "未授权个体不应看到记忆写入"
        );
        assert!(!allow.contains(&ToolName::MemoryLink));

        // 编成 JSON 显式授权：仅授权的工具可见（memory_write 授权但 memory_read 未授权）
        let granted = def(
            "unit-2",
            Tier::Unit,
            &[ToolName::Read, ToolName::MemoryWrite],
        );
        let allow2 = ToolGateway::runtime_allowlist(&granted, Some(&group_meta("orch-1", true)));
        assert!(allow2.contains(&ToolName::MemoryWrite));
        assert!(
            !allow2.contains(&ToolName::MemoryRead),
            "逐工具授权，不整组放行"
        );
    }

    #[test]
    fn 工具面快照_schema按白名单下发() {
        let primary = def("orch-1", Tier::Orchestrator, &[ToolName::Read]);
        let allow = ToolGateway::runtime_allowlist(&primary, Some(&group_meta("orch-1", true)));
        let specs = ToolGateway::tool_specs(&allow, false, false, false);
        let names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();
        assert!(
            names.contains(&"memory_read")
                && names.contains(&"memory_write")
                && names.contains(&"memory_link")
        );
        assert!(
            !names.contains(&"web_search"),
            "后端未就绪不下发（口径不因新工具改变）"
        );

        // 未授权个体：schema 不含记忆工具
        let unit = def("unit-1", Tier::Unit, &[ToolName::Read]);
        let specs2 = ToolGateway::tool_specs(
            &ToolGateway::runtime_allowlist(&unit, None),
            false,
            false,
            false,
        );
        assert!(specs2.iter().all(|s| !s.name.starts_with("memory_")));
    }

    #[test]
    fn 记忆写入_哈希去重提升既有条目() {
        let r = rig();
        let args = serde_json::json!({ "kind": "fact", "title": "部署约定", "body": "生产环境用 docker compose 部署", "tags": ["deploy"], "importance": 0.4 });
        let out = r.gw.tool_memory_write("orch-1", "test-session", &args);
        assert!(out.ok, "{}", out.error.unwrap_or_default());
        let again = r.gw.tool_memory_write("orch-1", "test-session", &args);
        assert!(again.ok);
        // 同内容二次写入：库内仅一条，重要性按既有规则提升（+0.08）
        let entries = r.gw.memory.as_ref().unwrap().list(None, 100).unwrap();
        assert_eq!(entries.len(), 1, "哈希去重：相同内容不得产生重复条目");
        assert!(
            entries[0].importance > 0.4,
            "重复写入应提升重要性：{}",
            entries[0].importance
        );
        assert_eq!(
            entries[0].source_ref.as_deref(),
            Some("agent"),
            "来源标注 source=agent"
        );
        assert_eq!(
            entries[0].agent_id.as_deref(),
            Some("orch-1"),
            "归属调用个体"
        );
        assert_eq!(entries[0].group_id.as_deref(), Some("t"), "归属激活组");
    }

    #[test]
    fn 记忆检索_相关度排序与来源标注() {
        let r = rig();
        let mk = |t: &str, b: &str| serde_json::json!({ "kind": "fact", "title": t, "body": b });
        r.gw.tool_memory_write(
            "orch-1",
            "test-session",
            &mk("redis 部署手册", "redis 用 compose 起单实例"),
        );
        r.gw.tool_memory_write(
            "orch-1",
            "test-session",
            &mk("前端构建", "npm run build:webui 产出 dist"),
        );
        r.gw.tool_memory_write(
            "unit-1",
            "test-session",
            &mk("redis 持久化教训", "redis 未开 AOF 重启丢数据"),
        );
        // 群体共享条目（无个体归属）：来源应标 "组:t"
        r.gw.memory
            .as_ref()
            .unwrap()
            .remember(
                &crate::memory::MemoryDraft::new(
                    crate::memory::MemoryKind::Fact,
                    "redis 端口约定",
                    "统一用 6379",
                )
                .group("t"),
            )
            .unwrap();

        let out = r.gw.tool_memory_read(
            "orch-1",
            "test-session",
            &serde_json::json!({ "query": "redis", "limit": 5 }),
        );
        assert!(out.ok, "{}", out.error.unwrap_or_default());
        let text = out.output;
        assert!(
            text.contains("2 条命中") || text.contains("3 条命中"),
            "应含全部 redis 条目：{text}"
        );
        assert!(text.contains("redis 部署手册"), "标题命中应排前：{text}");
        assert!(
            text.contains("个体:unit-1"),
            "主智能体可见个体私有层并标注来源：{text}"
        );
        assert!(text.contains("组:t"), "群体条目标注来源：{text}");
        // 个体视角：只见自己私有 + 群体，不见他体私有
        let unit_view = r.gw.tool_memory_read(
            "unit-1",
            "test-session",
            &serde_json::json!({ "query": "redis" }),
        );
        assert!(
            unit_view.output.contains("redis 持久化教训"),
            "自己的私有层可见"
        );
        let _ = std::fs::remove_dir_all(&r.dir);
    }

    #[test]
    fn 记忆检索_越组隔离() {
        let r = rig();
        // 在 default 组写入一条
        let w = r.gw.tool_memory_write("orch-1", "test-session", &serde_json::json!({ "kind": "fact", "title": "机密暗号", "body": "仅 default 组可见的暗号 xyzzy" }));
        assert!(w.ok, "{}", w.error.unwrap_or_default());
        assert!(r
            .gw
            .registry
            .create_group(Some("other".into()), "他组", "")
            .is_ok());
        r.gw.registry.set_active_group("other").unwrap();
        let out = r.gw.tool_memory_read(
            "orch-1",
            "test-session",
            &serde_json::json!({ "query": "机密暗号 xyzzy" }),
        );
        assert!(
            out.output.contains("无命中"),
            "越组检索不得返回他组条目：{}",
            out.output
        );
        // 写入归属切换后的组：不串组
        let w2 = r.gw.tool_memory_write(
            "orch-1",
            "test-session",
            &serde_json::json!({ "kind": "fact", "title": "他组约定", "body": "other 组的约定" }),
        );
        assert!(w2.ok);
        r.gw.registry.set_active_group("exmachina").unwrap();
        let back = r.gw.tool_memory_read(
            "orch-1",
            "test-session",
            &serde_json::json!({ "query": "他组约定" }),
        );
        assert!(
            back.output.contains("无命中"),
            "other 组条目不得泄漏进 default 视图：{}",
            back.output
        );
        let _ = std::fs::remove_dir_all(&r.dir);
    }

    /// 从写入结果文本提取条目 id（输出格式：…（id: <id>，类型 …)
    fn id_of(r: &ToolResult) -> String {
        r.output
            .rsplit("id: ")
            .next()
            .unwrap_or("")
            .split('，')
            .next()
            .unwrap_or("")
            .trim()
            .to_string()
    }

    #[test]
    fn 记忆关联_建立与列示_越组拒绝() {
        let r = rig();
        let mk =
            |t: &str, b: &str| serde_json::json!({ "kind": "decision", "title": t, "body": b });
        let a =
            r.gw.tool_memory_write("orch-1", "test-session", &mk("决策A", "采用方案 A"));
        let b =
            r.gw.tool_memory_write("orch-1", "test-session", &mk("决策B", "由 A 派生方案 B"));
        assert!(
            a.ok && b.ok,
            "{} {}",
            a.error.unwrap_or_default(),
            b.error.unwrap_or_default()
        );
        let id_a = id_of(&a);
        let id_b = id_of(&b);
        assert!(
            !id_a.is_empty() && !id_b.is_empty(),
            "id 解析：{} / {}",
            a.output,
            b.output
        );

        let link = r.gw.tool_memory_link("orch-1", "test-session", &serde_json::json!({ "op": "link", "from": id_a, "to": id_b, "relation": "derives-from" }));
        assert!(link.ok, "{}", link.error.unwrap_or_default());

        // 列示可见关联链
        let list = r.gw.tool_memory_link(
            "orch-1",
            "test-session",
            &serde_json::json!({ "op": "list", "query": "决策", "limit": 10 }),
        );
        assert!(list.ok);
        assert!(
            list.output.contains("derives-from"),
            "列示应含关联链：{}",
            list.output
        );

        // 越组关联拒绝
        assert!(r
            .gw
            .registry
            .create_group(Some("elsewhere".into()), "他组", "")
            .is_ok());
        r.gw.registry.set_active_group("elsewhere").unwrap();
        let c =
            r.gw.tool_memory_write("orch-1", "test-session", &mk("他组条目", "other 组的决策"));
        r.gw.registry.set_active_group("exmachina").unwrap();
        assert!(c.ok);
        let id_c = id_of(&c);
        let denied = r.gw.tool_memory_link(
            "orch-1",
            "test-session",
            &serde_json::json!({ "op": "link", "from": id_a, "to": id_c, "relation": "related" }),
        );
        assert!(!denied.ok, "越组关联应被拒绝");
        assert!(denied.error.unwrap_or_default().contains("越组隔离"));

        let _ = std::fs::remove_dir_all(&r.dir);
    }

    #[test]
    fn 记忆写入_参数校验() {
        let r = rig();
        let bad_kind = r.gw.tool_memory_write(
            "orch-1",
            "test-session",
            &serde_json::json!({ "kind": "nope", "title": "t", "body": "b" }),
        );
        assert!(!bad_kind.ok, "非法类型应拒绝");
        let no_body = r.gw.tool_memory_write(
            "orch-1",
            "test-session",
            &serde_json::json!({ "kind": "fact", "title": "t" }),
        );
        assert!(!no_body.ok, "缺 body 应拒绝");
        let _ = std::fs::remove_dir_all(&r.dir);
    }

    // ---------------- 文件记忆模式（memory.enabled=false）行为一致性 ----------------

    /// 装配成文件记忆模式（深层记忆关闭）：memory.md 与 orchestrator 规划注入同路径
    fn file_mode(md_content: Option<&str>) -> TestRig {
        let mut r = rig();
        let md_path = r.dir.join("memory.md");
        match md_content {
            Some(c) => std::fs::write(&md_path, c).unwrap(),
            None => {
                let _ = std::fs::remove_file(&md_path);
            }
        }
        r.gw.memory_deep = false;
        r.gw.memory_md_path = Some(md_path);
        r
    }

    #[test]
    fn 文件记忆模式_读取回落memory_md() {
        let r = file_mode(Some("# 记忆\n- 用中文汇报"));
        let out = r.gw.tool_memory_read(
            "orch-1",
            "test-session",
            &serde_json::json!({ "query": "任意" }),
        );
        assert!(out.ok, "{}", out.error.unwrap_or_default());
        assert!(
            out.output.contains("用中文汇报"),
            "应返回 memory.md 内容：{}",
            out.output
        );
        assert!(
            out.output.contains("深层记忆已关闭"),
            "应标注文件记忆模式：{}",
            out.output
        );

        // 无文件：友好提示而非报错（文件模式下空记忆是正常态）
        let r2 = file_mode(None);
        let out2 = r2.gw.tool_memory_read(
            "orch-1",
            "test-session",
            &serde_json::json!({ "query": "任意" }),
        );
        assert!(out2.ok, "{}", out2.error.unwrap_or_default());
        assert!(out2.output.contains("尚无记忆内容"), "{}", out2.output);

        // 超长 memory.md：与规划注入同一 8000 字符上限并声明截断
        let r3 = file_mode(Some("长".repeat(20_000).as_str()));
        let out3 = r3
            .gw
            .tool_memory_read("orch-1", "test-session", &serde_json::json!({}));
        assert!(out3.ok);
        assert!(out3.output.contains("已截断"), "超长 md 应声明截断");

        let _ = std::fs::remove_dir_all(&r.dir);
        let _ = std::fs::remove_dir_all(&r2.dir);
        let _ = std::fs::remove_dir_all(&r3.dir);
    }

    #[test]
    fn 文件记忆模式_写入关联拒绝并引导() {
        let r = file_mode(Some("已有内容"));
        let args = serde_json::json!({ "kind": "fact", "title": "t", "body": "b" });
        let w = r.gw.tool_memory_write("orch-1", "test-session", &args);
        assert!(!w.ok, "文件记忆模式下写库应拒绝（规划面不召回 = 写入黑洞）");
        assert!(
            w.error.unwrap_or_default().contains("文件记忆模式"),
            "错误应引导用户："
        );

        let l = r.gw.tool_memory_link(
            "orch-1",
            "test-session",
            &serde_json::json!({ "op": "list" }),
        );
        assert!(!l.ok, "文件记忆模式下关联应拒绝");
        // 拒绝是真拒绝：数据库确实没被写入
        assert_eq!(
            r.gw.memory.as_ref().unwrap().list(None, 10).unwrap().len(),
            0
        );

        let _ = std::fs::remove_dir_all(&r.dir);
    }

    #[test]
    fn 文件记忆模式_schema不下发写与关联() {
        let primary = def("orch-1", Tier::Orchestrator, &[ToolName::Read]);
        let allow = ToolGateway::runtime_allowlist(&primary, Some(&group_meta("orch-1", true)));
        // 深层开启：三工具照常下发（兼容口径不变）
        let deep = ToolGateway::tool_specs_gated(&allow, false, false, false, true);
        assert!(deep.iter().any(|s| s.name == "memory_write"));
        assert!(deep.iter().any(|s| s.name == "memory_link"));
        // 文件模式：write/link 不承诺（必然失败的工具不下发），read 保留（读 md 有意义）
        let file = ToolGateway::tool_specs_gated(&allow, false, false, false, false);
        assert!(
            file.iter().any(|s| s.name == "memory_read"),
            "memory_read 保留"
        );
        assert!(
            !file.iter().any(|s| s.name == "memory_write"),
            "文件模式不下发 memory_write"
        );
        assert!(
            !file.iter().any(|s| s.name == "memory_link"),
            "文件模式不下发 memory_link"
        );
    }

    #[test]
    fn 列示空查询_分层不越权() {
        let r = rig();
        // unit-1 的私有记忆 + 一条群体记忆；unit-1 编成里显式授权三工具由 def() 提供
        let mut draft = crate::memory::MemoryDraft::new(
            crate::memory::MemoryKind::Lesson,
            "unit私有",
            "只属于 unit-1",
        );
        draft.agent_id = Some("unit-1".into());
        r.gw.memory.as_ref().unwrap().remember(&draft).unwrap();
        r.gw.memory
            .as_ref()
            .unwrap()
            .remember(
                &crate::memory::MemoryDraft::new(
                    crate::memory::MemoryKind::Fact,
                    "群体事实",
                    "无归属共享",
                )
                .group("t"),
            )
            .unwrap();

        // 个体空查询列示：见群体，不见他人（主智能体 orch-1）尚未写私有 → 只验不泄漏
        let out = r.gw.tool_memory_link(
            "unit-1",
            "test-session",
            &serde_json::json!({ "op": "list", "query": "" }),
        );
        assert!(out.ok, "{}", out.error.unwrap_or_default());
        assert!(
            out.output.contains("群体事实"),
            "个体列示应含群体条目：{}",
            out.output
        );

        // 主智能体再写一条私有；unit-1 的空查询列示不得看到
        let mut o = crate::memory::MemoryDraft::new(
            crate::memory::MemoryKind::Fact,
            "orch私有",
            "只属于 orch-1",
        );
        o.agent_id = Some("orch-1".into());
        r.gw.memory.as_ref().unwrap().remember(&o).unwrap();
        let out2 = r.gw.tool_memory_link(
            "unit-1",
            "test-session",
            &serde_json::json!({ "op": "list", "query": "" }),
        );
        assert!(
            !out2.output.contains("orch私有"),
            "个体空查询列示不得泄漏主智能体私有记忆：{}",
            out2.output
        );
        // 主智能体全层视角：双方私有都可见
        let out3 = r.gw.tool_memory_link(
            "orch-1",
            "test-session",
            &serde_json::json!({ "op": "list", "query": "" }),
        );
        assert!(
            out3.output.contains("orch私有") && out3.output.contains("unit私有"),
            "主智能体全层列示：{}",
            out3.output
        );

        let _ = std::fs::remove_dir_all(&r.dir);
    }

    #[test]
    fn 读取_offset越界与max_chars上限() {
        let r = rig();
        let p = r.dir.join("sample.txt");
        std::fs::write(&p, "l1\nl2\nl3\n").unwrap();

        let ok = r.gw.tool_read(&serde_json::json!({ "path": "sample.txt" }));
        assert!(ok.ok, "{}", ok.error.unwrap_or_default());
        assert!(ok.output.contains("共 3 行"));

        // 越界 offset：给可行动的提示而非「已显示第 无 行」
        let beyond =
            r.gw.tool_read(&serde_json::json!({ "path": "sample.txt", "offset": 99 }));
        assert!(beyond.ok, "{}", beyond.error.unwrap_or_default());
        assert!(beyond.output.contains("超出文件末尾"), "{}", beyond.output);
        assert!(
            beyond.output.contains("offset≤3"),
            "应提示合法上界：{}",
            beyond.output
        );

        // maxChars 传超大值：被钳到硬上限，不绕过单次读取预算（小文件内容不受影响）
        let clamped = r
            .gw
            .tool_read(&serde_json::json!({ "path": "sample.txt", "maxChars": 1_000_000_000u64 }));
        assert!(clamped.ok, "{}", clamped.error.unwrap_or_default());
        assert!(
            clamped.output.contains("l3"),
            "小文件不受钳制影响：{}",
            clamped.output
        );

        let _ = std::fs::remove_dir_all(&r.dir);
    }
}

// ---------------------------------------------------------------- 终端超时诊断测试

#[cfg(test)]
mod timeout_output_tests {
    use super::*;

    /// 超时强杀后必须带回终止前的部分输出（回归：超时只报「已终止」、输出全丢，
    /// 长构建/测试卡死时无法诊断卡在哪一步）
    #[tokio::test]
    async fn 终端超时_部分输出随错误带回() {
        let root = std::env::temp_dir();
        // 先产出一段标记输出，再挂死等强杀
        let cmd = if cfg!(windows) {
            "echo out-start & ping -n 30 127.0.0.1".to_string()
        } else {
            "printf out-start; sleep 30".to_string()
        };
        let started = std::time::Instant::now();
        let r = execute_shell_command_timed(&root, &cmd, 1).await;
        let elapsed = started.elapsed();
        assert!(!r.ok, "挂死命令应失败");
        let err = r.error.unwrap_or_default();
        assert!(err.contains("超时"), "应为超时错误: {err}");
        assert!(
            err.contains("out-start"),
            "超时错误应包含终止前的部分输出，实际: {err}"
        );
        assert!(
            elapsed < std::time::Duration::from_secs(10),
            "应在时限附近快速返回，实际 {elapsed:?}"
        );
    }

    /// 正常路径不受影响：快速完成的命令照常返回成功输出
    #[tokio::test]
    async fn 终端超时_正常完成不受影响() {
        let root = std::env::temp_dir();
        let cmd = "echo quick-ok";
        let r = execute_shell_command_timed(&root, cmd, 15).await;
        assert!(r.ok, "快速命令应成功");
        assert!(r.output.contains("quick-ok"), "输出应完整: {}", r.output);
    }

    /// 失败命令必须可诊断：退出码与输出摘要进 error——
    /// 回灌层（runtime::tool_feedback）与事件/审计摘要只取 error 字段，
    /// 之前失败命令 error 留空，模型与用户看到的失败原因是一句空话。
    /// output 保留原文（审批代执行等老调用方按 output 取输出，向后兼容）。
    #[tokio::test]
    async fn 终端失败_退出码与输出进入错误字段() {
        let root = std::env::temp_dir();
        let cmd = if cfg!(windows) {
            // 执行器已按 cmd /C <命令> 拼装，这里再包一层 cmd /C "..." 会被内层当程序名
            "echo out-tail & echo boom 1>&2 & exit /b 3".to_string()
        } else {
            "printf out-tail; echo boom >&2; exit 3".to_string()
        };
        let r = execute_shell_command_timed(&root, &cmd, 15).await;
        assert!(!r.ok, "非零退出应判定失败");
        let err = r.error.unwrap_or_else(|| "（error 为空）".into());
        assert!(err.contains("退出码 3"), "错误应含退出码: {err}");
        assert!(err.contains("boom"), "错误应含 stderr 摘要: {err}");
        assert!(err.contains("out-tail"), "错误应含 stdout 尾部摘要: {err}");
        assert!(
            r.output.contains("out-tail"),
            "output 保留原文（向后兼容）: {}",
            r.output
        );
    }

    /// 超长输出截断必须带标记：模型不得把半截输出当完整结论
    /// （典型误判：测试/构建日志尾部被切，把半路输出读成「成功」）
    #[tokio::test]
    async fn 终端输出_超长截断带标记() {
        let root = std::env::temp_dir();
        let cmd = if cfg!(windows) {
            // for /L 从 0 起到 1499：与 unix 分支对齐（断言头部含 -0、尾部 -1499 被截）
            "for /L %i in (0,1,1499) do @echo aaaaaaaaaaaaaaaaaaaa-%i".to_string()
        } else {
            "i=0; while [ $i -lt 1500 ]; do echo aaaaaaaaaaaaaaaaaaaa-$i; i=$((i+1)); done"
                .to_string()
        };
        let r = execute_shell_command_timed(&root, &cmd, 30).await;
        assert!(r.ok, "{}", r.error.unwrap_or_default());
        assert!(r.output.contains("输出已截断"), "超长输出必须带截断标记");
        assert!(
            r.output.contains("aaaaaaaaaaaaaaaaaaaa-0"),
            "截断应保留头部"
        );
        let tail = &r.output[r.output.len().saturating_sub(400)..];
        assert!(
            !tail.contains("-1499"),
            "尾部内容应被截掉，实际尾部: {tail}"
        );
    }
}

// ---------------------------------------------------------------- 终端白名单与审批路径

#[cfg(test)]
mod terminal_gate_tests {
    use super::*;

    /// 测试执行器闭包形态（spawn 白名单断言用；长类型起别名避免 clippy type_complexity）
    type SpawnExec = Arc<
        dyn Fn(&str, &str, &str, Vec<ToolName>) -> futures_util::future::BoxFuture<'static, anyhow::Result<String>>
            + Send
            + Sync,
    >;

    /// filesystem delete/rename（spec: filesystem-ops）回归
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn fs删除_跨设备fallback用copy_tree() {
        // rename EXDEV 场景无法在本机模拟挂载边界——直接验证 copy_tree 语义
        //（fallback 路径的核心：目录树完整复制 + 目标已存在清空重建）
        let dir = std::env::temp_dir().join(format!("exm-fscopy-{}", crate::types::new_id()));
        std::fs::create_dir_all(dir.join("src/nested")).unwrap();
        std::fs::write(dir.join("src/a.txt"), "A").unwrap();
        std::fs::write(dir.join("src/nested/b.txt"), "B").unwrap();
        let dst = dir.join("trash-copy");
        std::fs::write(&dst, "旧残留").unwrap(); // 已存在文件（非目录）
        ToolGateway::copy_tree(&dir.join("src"), &dst).unwrap();
        assert!(dst.is_dir(), "已存在文件应被清空重建为目录");
        assert_eq!(std::fs::read_to_string(dst.join("a.txt")).unwrap(), "A");
        assert_eq!(
            std::fs::read_to_string(dst.join("nested/b.txt")).unwrap(),
            "B"
        );
        // 单文件路径
        let f = dir.join("one.log");
        std::fs::write(&f, "x").unwrap();
        ToolGateway::copy_tree(&f, &dir.join("t/one.log")).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("t/one.log")).unwrap(), "x");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn fs删除_移入回收站可恢复() {
        let dir = std::env::temp_dir().join(format!("exm-fsdel-{}", crate::types::new_id()));
        std::fs::create_dir_all(dir.join("data")).unwrap();
        let store = Arc::new(Store::open(dir.join("data")).unwrap());
        let registry = Arc::new(LocalRegistry::new(dir.join("agents")).unwrap());
        let (events, _rx) = tokio::sync::broadcast::channel(64);
        let gw = ToolGateway::new(&dir, store, registry, events, SecurityConfig::default());
        std::fs::write(dir.join("tmp.log"), "hello").unwrap();
        // 删除 → 回收站
        let r = gw
            .execute(
                "a",
                "s-fs-1",
                &[ToolName::Filesystem],
                ToolName::Filesystem,
                &serde_json::json!({ "op": "delete", "path": "tmp.log" }),
            )
            .await;
        assert!(r.ok, "删除应成功：{:?}", r.error);
        assert!(
            r.output.contains("回收站"),
            "应说明回收站路径：{}",
            r.output
        );
        assert!(!dir.join("tmp.log").exists(), "原位应消失");
        // 回收站里可找到（按日期目录搜索）
        let trash_root = dir.join(".exmachina").join("trash");
        let found = std::fs::read_dir(&trash_root).unwrap().any(|day| {
            let day = day.unwrap().path();
            day.join("tmp.log").exists()
        });
        assert!(found, "回收站应含被删文件");
        // 不存在目标 → 报错
        let r = gw
            .execute(
                "a",
                "s-fs-1",
                &[ToolName::Filesystem],
                ToolName::Filesystem,
                &serde_json::json!({ "op": "delete", "path": "tmp.log" }),
            )
            .await;
        assert!(!r.ok, "重复删除应报目标不存在");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn fs删除_目录递归与同名序号() {
        let dir = std::env::temp_dir().join(format!("exm-fsdir-{}", crate::types::new_id()));
        std::fs::create_dir_all(dir.join("data")).unwrap();
        let store = Arc::new(Store::open(dir.join("data")).unwrap());
        let registry = Arc::new(LocalRegistry::new(dir.join("agents")).unwrap());
        let (events, _rx) = tokio::sync::broadcast::channel(64);
        let gw = ToolGateway::new(&dir, store, registry, events, SecurityConfig::default());
        // 目录树
        std::fs::create_dir_all(dir.join("build").join("nested")).unwrap();
        std::fs::write(dir.join("build").join("a.txt"), "1").unwrap();
        std::fs::write(dir.join("build").join("nested").join("b.txt"), "2").unwrap();
        let r = gw
            .execute(
                "a",
                "s-fs-2",
                &[ToolName::Filesystem],
                ToolName::Filesystem,
                &serde_json::json!({ "op": "delete", "path": "build" }),
            )
            .await;
        assert!(r.ok, "目录删除应成功：{:?}", r.error);
        assert!(!dir.join("build").exists(), "目录树应整体移入");
        // 同名序号：同日两次删除同名文件，第二次带 .2
        std::fs::write(dir.join("x.log"), "v1").unwrap();
        let r1 = gw
            .execute(
                "a",
                "s-fs-2",
                &[ToolName::Filesystem],
                ToolName::Filesystem,
                &serde_json::json!({ "op": "delete", "path": "x.log" }),
            )
            .await;
        assert!(r1.ok, "第一次删除应成功");
        std::fs::write(dir.join("x.log"), "v2").unwrap();
        let r2 = gw
            .execute(
                "a",
                "s-fs-2",
                &[ToolName::Filesystem],
                ToolName::Filesystem,
                &serde_json::json!({ "op": "delete", "path": "x.log" }),
            )
            .await;
        assert!(r2.ok);
        assert!(
            r2.output.contains(".2"),
            "第二次删除应带序号：{}",
            r2.output
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn fs删除_保护区拒绝() {
        let dir = std::env::temp_dir().join(format!("exm-fspro-{}", crate::types::new_id()));
        std::fs::create_dir_all(dir.join("data")).unwrap();
        let store = Arc::new(Store::open(dir.join("data")).unwrap());
        let registry = Arc::new(LocalRegistry::new(dir.join("agents")).unwrap());
        let (events, _rx) = tokio::sync::broadcast::channel(64);
        let gw = ToolGateway::new(&dir, store, registry, events, SecurityConfig::default());
        // .exmachina 系统区
        std::fs::create_dir_all(dir.join(".exmachina").join("checkpoints")).unwrap();
        let r = gw
            .execute(
                "a",
                "s-fs-3",
                &[ToolName::Filesystem],
                ToolName::Filesystem,
                &serde_json::json!({ "op": "delete", "path": ".exmachina" }),
            )
            .await;
        assert!(!r.ok, "系统区删除应拒绝");
        assert!(r.error.unwrap_or_default().contains("系统区"));
        // 工作区根（resolve_safe(".") 应解析到根）
        let r = gw
            .execute(
                "a",
                "s-fs-3",
                &[ToolName::Filesystem],
                ToolName::Filesystem,
                &serde_json::json!({ "op": "delete", "path": "." }),
            )
            .await;
        assert!(!r.ok, "根删除应拒绝");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn fs重命名_成功不覆盖与跨目录() {
        let dir = std::env::temp_dir().join(format!("exm-fsren-{}", crate::types::new_id()));
        std::fs::create_dir_all(dir.join("data")).unwrap();
        let store = Arc::new(Store::open(dir.join("data")).unwrap());
        let registry = Arc::new(LocalRegistry::new(dir.join("agents")).unwrap());
        let (events, _rx) = tokio::sync::broadcast::channel(64);
        let gw = ToolGateway::new(&dir, store, registry, events, SecurityConfig::default());
        std::fs::write(dir.join("old.txt"), "content-1").unwrap();
        // 成功重命名
        let r = gw
            .execute(
                "a",
                "s-fs-4",
                &[ToolName::Filesystem],
                ToolName::Filesystem,
                &serde_json::json!({ "op": "rename", "path": "old.txt", "newPath": "new.txt" }),
            )
            .await;
        assert!(r.ok, "重命名应成功：{:?}", r.error);
        assert!(dir.join("new.txt").exists());
        assert!(!dir.join("old.txt").exists());
        assert_eq!(
            std::fs::read_to_string(dir.join("new.txt")).unwrap(),
            "content-1"
        );
        // 不覆盖：目标已存在拒绝
        std::fs::write(dir.join("other.txt"), "x").unwrap();
        let r = gw
            .execute(
                "a",
                "s-fs-4",
                &[ToolName::Filesystem],
                ToolName::Filesystem,
                &serde_json::json!({ "op": "rename", "path": "other.txt", "newPath": "new.txt" }),
            )
            .await;
        assert!(!r.ok, "覆盖应拒绝");
        assert!(r.error.unwrap_or_default().contains("已存在"));
        assert_eq!(
            std::fs::read_to_string(dir.join("new.txt")).unwrap(),
            "content-1",
            "原内容不应被破坏"
        );
        // 跨目录移动（父目录自动创建）
        let r = gw
            .execute(
                "a",
                "s-fs-4",
                &[ToolName::Filesystem],
                ToolName::Filesystem,
                &serde_json::json!({ "op": "rename", "path": "new.txt", "newPath": "sub/dir/moved.txt" }),
            )
            .await;
        assert!(r.ok, "跨目录移动应成功：{:?}", r.error);
        assert!(dir.join("sub").join("dir").join("moved.txt").exists());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn fs删除重命名_plan模式拦截() {
        let dir = std::env::temp_dir().join(format!("exm-fsplan-{}", crate::types::new_id()));
        std::fs::create_dir_all(dir.join("data")).unwrap();
        let store = Arc::new(Store::open(dir.join("data")).unwrap());
        let registry = Arc::new(LocalRegistry::new(dir.join("agents")).unwrap());
        let (events, _rx) = tokio::sync::broadcast::channel(64);
        let gw = ToolGateway::new(&dir, store, registry, events, SecurityConfig::default());
        std::fs::write(dir.join("plan-target.txt"), "p").unwrap();
        crate::plan::set_plan_mode("s-fs-5", true);
        let r = gw
            .execute(
                "a",
                "s-fs-5",
                &[ToolName::Filesystem],
                ToolName::Filesystem,
                &serde_json::json!({ "op": "delete", "path": "plan-target.txt" }),
            )
            .await;
        assert!(r.ok);
        assert!(dir.join("plan-target.txt").exists(), "plan 模式不应真删");
        assert_eq!(crate::plan::list_items("s-fs-5").len(), 1, "应转为计划条目");
        let r = gw
            .execute(
                "a",
                "s-fs-5",
                &[ToolName::Filesystem],
                ToolName::Filesystem,
                &serde_json::json!({ "op": "rename", "path": "plan-target.txt", "newPath": "z.txt" }),
            )
            .await;
        assert!(r.ok);
        assert_eq!(
            crate::plan::list_items("s-fs-5").len(),
            2,
            "rename 也应拦截"
        );
    }

    #[test]
    fn 白名单_日常开发命令放行() {
        // 本轮修复核心场景：mkdir/cat/go/pytest/tsc 等此前全部被硬拒
        for cmd in [
            "cat README.md",
            "mkdir -p src/new",
            "cp a.rs b.rs",
            "mv old.rs new.rs",
            "touch main.go",
            "diff -u a.rs b.rs",
            "grep -rn TODO src",
            "find . -name x",
            "go build ./...",
            "go test ./...",
            "pytest -x",
            "jest",
            "vitest run",
            "tsc --noEmit",
            "rg pattern",
            "which cargo",
            "uname -a",
            "cargo check",
            "cargo test",
            "git status",
            "git diff",
            "ls -la",
            "npm run build",
            "node script.js",
            "python3 -m venv .venv",
            "make",
            "cmake .",
            "dotnet build",
            "mvn package",
        ] {
            assert!(terminal_allowed(cmd), "日常开发命令应放行: {cmd}");
        }
    }

    #[test]
    fn 白名单_危险与未知命令不放行() {
        for cmd in [
            "rm -rf /",
            "sudo rm x",
            "vim",
            "unknown-cmd",
            // docker 只放只读/exec/run 子命令；rmi/build 等高危仍拒
            "docker rmi nginx",
            "docker system prune",
            // ip 只放 addr/route/link；netns 配置类仍拒
            "ip netns add foo",
        ] {
            assert!(!terminal_allowed(cmd), "不应放行: {cmd}");
        }
        // curl/wget/chmod 已入白名单（harness 第二轮）：联网/权限语义由后置闸门分层把关
        for cmd in ["curl http://x", "wget x", "chmod 777 a"] {
            assert!(terminal_allowed(cmd), "应放行（后置闸门把关）: {cmd}");
        }
    }

    #[test]
    fn 白名单_子命令约束依旧生效() {
        // npm 只放行特定子命令
        assert!(terminal_allowed("npm install"));
        assert!(!terminal_allowed("npm exec something-weird"));
        // git push 不在子命令清单
        assert!(!terminal_allowed("git push origin main"));
        // cargo 全部子命令受约束
        assert!(terminal_allowed("cargo build"));
        assert!(!terminal_allowed("cargoSomething"));
    }

    #[test]
    fn 链式命令_逐段白名单() {
        // 每段都过白名单：放行（agent 干活的基本形态：拉数据 | 解析）
        for cmd in [
            "curl -s \"https://x.com/a?b=1&c=2\" | python3 -c \"import sys,json;print(json.load(sys.stdin))\"",
            "ls -la | grep src | head -5",
            "cat a.txt && echo done",
            "echo a; echo b || echo c",
            "git status && git diff",
            "cargo build --lib && cargo test --lib",
        ] {
            assert!(chain_allowed(cmd), "全段白名单应放行: {cmd}");
        }
        // 任一段非白名单：拒绝（危险命令藏在任何一段都不放行——攻击面与单命令一致）
        for cmd in [
            "curl -s x | python3 -c y | sudo rm -rf /",
            "ls && vim",
            "echo a; unknown-cmd",
            "cat a || docker rmi nginx",
            "git status && git push origin main",
        ] {
            assert!(!chain_allowed(cmd), "含非白名单段应拒绝: {cmd}");
        }
        // 逃逸/写入语义：链式放行不适用于这些符号（contains_shell_chain 拦截层）
        for frag in ["`id`", "$(id)", "> out.txt", "< in.txt", "cmd &"] {
            assert!(
                contains_shell_chain(&format!("echo {frag}")),
                "逃逸符应被拦截层识别: {frag}"
            );
        }
        // 引号内的符号不是连接符（URL 带 & 不误判）
        assert!(!contains_shell_chain("curl -s \"https://x.com/a?b=1&c=2\""));
        // 引号内的管道不参与分段
        assert!(chain_allowed("echo \"a | b\" | cat"));
    }

    #[test]
    fn 链式分段_引号感知() {
        let segs = split_chain_segments("curl -s url | python3 -c \"print(1 | 2)\" && echo ok");
        assert_eq!(segs.len(), 3, "引号内管道不分段: {segs:?}");
        assert!(segs[0].trim_start().starts_with("curl"));
        assert!(segs[1].trim_start().starts_with("python3"));
        assert_eq!(segs[2].trim(), "echo ok");
        // 空段（收尾管道）：不产出、不 panic
        let empty = split_chain_segments("echo a |");
        assert_eq!(empty, vec!["echo a ".to_string()]);
    }

    fn gw_with_approval(mode: &str, wait_secs: u64) -> ToolGateway {
        let dir = std::env::temp_dir().join(format!("exm-gate-{}", crate::types::new_id()));
        std::fs::create_dir_all(dir.join("data")).unwrap();
        let store = Arc::new(Store::open(dir.join("data")).unwrap());
        let registry = Arc::new(LocalRegistry::new(dir.join("agents")).unwrap());
        let (events, _rx) = tokio::sync::broadcast::channel(64);
        let sec = SecurityConfig {
            exec_approval: mode.into(),
            approval_wait_secs: wait_secs,
            ..Default::default()
        };
        ToolGateway::new(&dir, store, registry, events, sec)
    }

    /// spawn 白名单剔除：子级请求 spawn/agent_manage 被过滤；全剔除报错
    #[tokio::test]
    async fn spawn工具_白名单强制剔除spawn与agent_manage() {
        let gw = gw_with_approval("off", 0);
        // 未装配执行器时先到"未装配"错误（白名单校验在其之前）——装个空执行器测过滤
        let exec: SpawnExec = Arc::new(|_a, _s, _t, tools| {
            Box::pin(async move {
                assert!(
                    !tools.contains(&ToolName::Spawn) && !tools.contains(&ToolName::AgentManage),
                    "子级白名单不应含 spawn/agent_manage"
                );
                Ok("结论".into())
            })
        });
        gw.set_spawn_executor(exec);
        let r = gw
            .execute(
                "a1",
                "s1",
                &[ToolName::Spawn],
                ToolName::Spawn,
                &serde_json::json!({
                    "task": "检索测试",
                    "tools": ["read", "spawn", "agent_manage", "grep"]
                }),
            )
            .await;
        assert!(r.ok, "spawn 应执行成功：{:?}", r.error);
        // 全剔除 → 报错
        let r2 = gw
            .execute(
                "a1",
                "s1",
                &[ToolName::Spawn],
                ToolName::Spawn,
                &serde_json::json!({ "task": "x", "tools": ["spawn", "agent_manage"] }),
            )
            .await;
        assert!(!r2.ok, "全剔除的白名单应报错");
    }

    /// 超时：执行器睡眠超过 timeoutSecs → 超时终止说明
    #[tokio::test]
    async fn spawn工具_子任务超时终止() {
        let gw = gw_with_approval("off", 0);
        let exec: SpawnExec = Arc::new(|_, _, _, _| {
            Box::pin(async {
                tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                Ok("不该到这".into())
            })
        });
        gw.set_spawn_executor(exec);
        let r = gw
            .execute(
                "a1",
                "s1",
                &[ToolName::Spawn],
                ToolName::Spawn,
                &serde_json::json!({ "task": "慢任务", "timeoutSecs": 10 }),
            )
            .await;
        assert!(!r.ok);
        assert!(r.error.unwrap_or_default().contains("超时"), "应报超时说明");
    }

    /// 并发上限：占满信号量后再派生 → 明确拒绝
    #[tokio::test]
    async fn spawn工具_并发上限拒绝() {
        // 信号量是进程级 OnceLock：并发测试与其他测试互相干扰，用独立子进程语义不可行；
        // 此处改为验证「上限拒绝路径」——直接耗尽后触发。先并发占满（4 张许可）：
        let gw = Arc::new(gw_with_approval("off", 0));
        let exec: SpawnExec = Arc::new(|_, _, _, _| {
            Box::pin(async {
                tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                Ok("done".into())
            })
        });
        gw.set_spawn_executor(exec);
        let mut handles = Vec::new();
        for _ in 0..4 {
            let gw2 = gw.clone();
            handles.push(tokio::spawn(async move {
                gw2.execute(
                    "a1",
                    "s1",
                    &[ToolName::Spawn],
                    ToolName::Spawn,
                    &serde_json::json!({ "task": "占位", "timeoutSecs": 60 }),
                )
                .await
            }));
        }
        tokio::time::sleep(std::time::Duration::from_millis(80)).await;
        let r = gw
            .execute(
                "a1",
                "s1",
                &[ToolName::Spawn],
                ToolName::Spawn,
                &serde_json::json!({ "task": "第五个" }),
            )
            .await;
        // 注：信号量为进程级共享，若其他测试恰好占用则可能直接拒绝——同样满足断言
        assert!(!r.ok, "并发占满时应拒绝");
        assert!(
            r.error.unwrap_or_default().contains("并发"),
            "应报并发上限说明"
        );
        for h in handles {
            let _ = h.await;
        }
    }

    /// 计划模式：写操作拦截转计划条目，读操作放行
    #[tokio::test]
    async fn 计划模式_拦截写操作转计划条目() {
        let gw = gw_with_approval("off", 0);
        crate::plan::set_plan_mode("plan-s1", true);
        // 写操作（edit）被拦截
        let r = gw
            .execute(
                "a1",
                "plan-s1",
                &[ToolName::Edit],
                ToolName::Edit,
                &serde_json::json!({ "path": "a.rs", "old_string": "x", "new_string": "y" }),
            )
            .await;
        // 拦截返回 ok 文本（引导模型继续规划），非执行错误
        assert!(r.ok, "拦截应以引导文本返回：{:?}", r.error);
        assert!(r.output.contains("计划模式"), "应含计划模式说明");
        let items = crate::plan::list_items("plan-s1");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].tool, "edit");
        assert_eq!(items[0].target, "a.rs");
        // 读操作放行
        let r2 = gw
            .execute(
                "a1",
                "plan-s1",
                &[ToolName::Grep],
                ToolName::Grep,
                &serde_json::json!({ "pattern": "fn " }),
            )
            .await;
        assert!(r2.ok, "读操作应放行：{:?}", r2.error);
        // 清理
        crate::plan::approve("plan-s1");
    }

    /// 计划模式：批准后清单条目按序回放执行
    #[tokio::test]
    async fn 计划模式_批准后回放执行() {
        let dir = std::env::temp_dir().join(format!("exm-plan-{}", crate::types::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Arc::new(Store::open(dir.join("data")).unwrap());
        let registry = Arc::new(LocalRegistry::new(dir.join("agents")).unwrap());
        let (events, _rx) = tokio::sync::broadcast::channel(64);
        let gw = Arc::new(ToolGateway::new(
            &dir,
            store,
            registry,
            events,
            SecurityConfig::default(),
        ));
        // 计划模式拦截 filesystem write
        crate::plan::set_plan_mode("plan-s2", true);
        let r = gw
            .execute(
                "a1",
                "plan-s2",
                &[ToolName::Filesystem],
                ToolName::Filesystem,
                &serde_json::json!({ "op": "write", "path": "note.txt", "content": "planned" }),
            )
            .await;
        assert!(r.ok);
        assert_eq!(crate::plan::list_items("plan-s2").len(), 1);
        // 批准：切 execute 并回放
        let items = crate::plan::approve("plan-s2");
        assert_eq!(items.len(), 1);
        assert!(!crate::plan::is_plan_mode("plan-s2"));
        // 回放（此时非 plan 模式，真正执行）
        let item = &items[0];
        let t = ToolName::parse(&item.tool).unwrap();
        let applied = gw
            .execute("plan-replay", "plan-s2", &[t], t, &item.args)
            .await;
        assert!(applied.ok, "回放应执行成功：{:?}", applied.error);
        assert!(dir.join("note.txt").exists(), "文件应已写入");
    }

    /// git 测试仓：临时目录初始化仓库并提交一个文件
    fn git_test_repo(gw: &ToolGateway) -> PathBuf {
        let root = gw.effective_root();
        std::fs::create_dir_all(&root).unwrap();
        std::process::Command::new("git")
            .arg("init")
            .current_dir(&root)
            .output()
            .expect("git init");
        std::process::Command::new("git")
            .args(["config", "user.email", "test@exm.local"])
            .current_dir(&root)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["config", "user.name", "exm-test"])
            .current_dir(&root)
            .output()
            .unwrap();
        std::fs::write(root.join("a.txt"), "hello\n").unwrap();
        std::process::Command::new("git")
            .args(["add", "."])
            .current_dir(&root)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(&root)
            .output()
            .unwrap();
        root
    }

    /// 只读动作（status）直接执行，无需审批
    #[tokio::test]
    async fn git工具_status只读直行() {
        let gw = gw_with_approval("off", 0);
        git_test_repo(&gw);
        let r = gw
            .execute(
                "a1",
                "s1",
                &[ToolName::Git],
                ToolName::Git,
                &serde_json::json!({ "action": "status" }),
            )
            .await;
        assert!(r.ok, "status 只读应放行：{:?}", r.error);
        assert!(
            r.output.contains("##"),
            "status 输出应含分支行：{}",
            r.output
        );
    }

    /// 严格模式（risky + 立即超时）：写动作产生审批单而非执行
    #[tokio::test]
    async fn git工具_严格模式commit产生审批单() {
        let gw = gw_with_approval("always", 0);
        git_test_repo(&gw);
        std::fs::write(gw.effective_root().join("b.txt"), "new\n").unwrap();
        let r = gw
            .execute(
                "a1",
                "s1",
                &[ToolName::Git],
                ToolName::Git,
                &serde_json::json!({ "action": "commit", "message": "add b" }),
            )
            .await;
        // wait=0 → 受阻回流，但审批单已落库
        assert!(!r.ok, "审批模式下 commit 不应直接执行");
        let approvals = gw
            .store_for_test()
            .list_approvals(Some("pending"), 10)
            .unwrap_or_default();
        assert!(
            approvals.iter().any(|a| a.command.contains("git commit")),
            "应产生 git commit 审批单：{approvals:?}"
        );
    }

    /// push 动作不在动作面：明确报错说明
    #[tokio::test]
    async fn git工具_push返回不支持说明() {
        let gw = gw_with_approval("risky", 0);
        git_test_repo(&gw);
        let r = gw
            .execute(
                "a1",
                "s1",
                &[ToolName::Git],
                ToolName::Git,
                &serde_json::json!({ "action": "push" }),
            )
            .await;
        assert!(!r.ok);
        assert!(
            r.error.unwrap_or_default().contains("不在 git 工具动作面"),
            "push 应返回动作面说明"
        );
    }

    /// 非仓库工作区：git 工具明确报错
    #[tokio::test]
    async fn git工具_非仓库报错() {
        let gw = gw_with_approval("off", 0);
        // 不初始化仓库
        let r = gw
            .execute(
                "a1",
                "s1",
                &[ToolName::Git],
                ToolName::Git,
                &serde_json::json!({ "action": "status" }),
            )
            .await;
        assert!(!r.ok);
        assert!(r.error.unwrap_or_default().contains("不是 git 仓库"));
    }

    /// 参数白名单：非法参数（注入尝试）拒绝
    #[tokio::test]
    async fn git工具_非法参数拒绝() {
        let gw = gw_with_approval("off", 0);
        git_test_repo(&gw);
        for bad in [
            serde_json::json!({ "action": "show", "object": "--exec=evil" }),
            serde_json::json!({ "action": "commit", "message": "x' && rm -rf /" }),
            serde_json::json!({ "action": "checkout-b", "branch": "b1 && echo pwned" }),
        ] {
            let r = gw
                .execute("a1", "s1", &[ToolName::Git], ToolName::Git, &bad)
                .await;
            assert!(!r.ok, "非法参数应拒绝：{bad}");
        }
    }

    /// off 档：白名单外命令直接拒绝（历史行为保持）
    #[tokio::test]
    async fn 审批路径_off档_白名单外命令直接拒绝() {
        let gw = gw_with_approval("off", 0);
        let r = gw
            .execute(
                "a1",
                "s1",
                &[ToolName::Terminal],
                ToolName::Terminal,
                &serde_json::json!({
                    "command": "unknown-cmd --flag"
                }),
            )
            .await;
        assert!(!r.ok, "off 档白名单外应拒绝");
        assert!(r.error.unwrap_or_default().contains("不在白名单"));
    }

    /// risky 档：白名单外的日常命令转审批单（不等待，受阻回流但审批单已落库）
    /// ——修复前这条路径是硬拒，agent 无从开发
    #[tokio::test]
    async fn 审批路径_risky档_白名单外命令转审批() {
        let gw = gw_with_approval("risky", 0);
        let r = gw
            .execute(
                "a1",
                "s2",
                &[ToolName::Terminal],
                ToolName::Terminal,
                &serde_json::json!({
                    "command": "terraform apply"
                }),
            )
            .await;
        assert!(!r.ok, "未批准前不应放行");
        let err = r.error.unwrap_or_default();
        assert!(err.contains("审批"), "应转审批而非白名单硬拒: {err}");
        // 审批单确实落库（用户可在面板批准）
        let pending = gw
            .store
            .list_approvals(Some("pending"), 10)
            .unwrap_or_default();
        assert!(
            pending.iter().any(|p| p.command.contains("terraform")),
            "审批单应已落库"
        );
    }

    /// risky 档：白名单内安全命令不受影响，照常执行
    #[tokio::test]
    async fn 审批路径_risky档_白名单内命令照常执行() {
        let gw = gw_with_approval("risky", 0);
        let cmd = "echo hello";
        let r = gw
            .execute(
                "a1",
                "s3",
                &[ToolName::Terminal],
                ToolName::Terminal,
                &serde_json::json!({
                    "command": cmd
                }),
            )
            .await;
        assert!(
            r.ok,
            "白名单内命令应直接执行: {}",
            r.error.unwrap_or_default()
        );
        assert!(r.output.contains("hello"));
    }

    /// risky 档：白名单内但高危（git reset --hard）仍走审批
    #[tokio::test]
    async fn 审批路径_risky档_白名单内高危命令仍审批() {
        let gw = gw_with_approval("risky", 0);
        let r = gw
            .execute(
                "a1",
                "s4",
                &[ToolName::Terminal],
                ToolName::Terminal,
                &serde_json::json!({
                    "command": "git reset --hard HEAD"
                }),
            )
            .await;
        assert!(!r.ok);
        assert!(
            r.error.unwrap_or_default().contains("审批"),
            "白名单内高危命令应走审批闸门"
        );
    }

    // ---------------- 实验性开关（experiments） ----------------

    /// 动态压缩门控：开关关闭时 context_compact 不在 schema 面
    #[test]
    fn 实验开关_动态压缩关闭时schema剔除context_compact() {
        let allow = [ToolName::Terminal, ToolName::ContextCompact];
        let specs = ToolGateway::tool_specs_gated(&allow, false, false, false, true);
        let specs = ToolGateway::tool_specs_experiments(specs, false);
        assert!(
            specs.iter().all(|s| s.name != "context_compact"),
            "开关关闭时 context_compact 不应下发"
        );
        // 开启时保留
        let specs = ToolGateway::tool_specs_gated(&allow, false, false, false, true);
        let specs = ToolGateway::tool_specs_experiments(specs, true);
        assert!(specs.iter().any(|s| s.name == "context_compact"));
    }

    /// 动态压缩执行：装配压缩器后调用成功落库摘要；未装配时明确报错
    #[tokio::test]
    async fn 动态压缩_装配后执行并落库() {
        use crate::provider::{ChatRequest, LlmProvider};
        struct OkCompacter;
        #[async_trait::async_trait]
        impl LlmProvider for OkCompacter {
            fn name(&self) -> &'static str {
                "test-compacter"
            }
            async fn chat(
                &self,
                _req: ChatRequest,
            ) -> anyhow::Result<crate::provider::ChatResponse> {
                Ok(crate::provider::ChatResponse {
                    content: "目标 X 已完成；结论 Y；未决 Z".into(),
                    reasoning: String::new(),
                    prompt_tokens: 0,
                    completion_tokens: 0,
                    tool_calls: Vec::new(),
                })
            }
            async fn stream(
                &self,
                _req: ChatRequest,
                _tx: tokio::sync::mpsc::UnboundedSender<crate::provider::StreamDelta>,
            ) -> anyhow::Result<crate::provider::ChatResponse> {
                anyhow::bail!("测试替身不支持流式")
            }
        }

        let gw = gw_with_approval("off", 0);
        // 造 6 条历史，keep=2 → 压 4 条
        let sid = gw.store.create_session("压缩测试", "g1").unwrap().id;
        for i in 0..6 {
            gw.store
                .add_message(
                    &sid,
                    crate::types::MessageRole::User,
                    None,
                    vec![crate::types::Statement::new(
                        crate::types::SpeechTag::要求,
                        format!("第 {i} 条"),
                    )],
                )
                .unwrap();
        }
        // 未装配：明确报错（不是静默失败）
        let r = gw
            .execute(
                "a1",
                &sid,
                &[],
                ToolName::ContextCompact,
                &serde_json::json!({ "keepRecent": 2 }),
            )
            .await;
        assert!(!r.ok);

        // 装配后：压缩成功 + 会话滚动摘要落库
        gw.set_compactor(Arc::new(OkCompacter), "test-model".into());
        let r = gw
            .execute(
                "a1",
                &sid,
                &[],
                ToolName::ContextCompact,
                &serde_json::json!({ "keepRecent": 2, "note": "进入长任务阶段" }),
            )
            .await;
        assert!(r.ok, "压缩应成功: {:?}", r.error);
        let s = gw.store.get_session(&sid).unwrap().unwrap();
        assert_eq!(s.summary_upto, Some(4), "摘要应覆盖前 4 条");
        assert!(s.rolling_summary.is_some());

        // 幂等：窗口外已全部压过 → 无可压缩说明
        let r2 = gw
            .execute(
                "a1",
                &sid,
                &[],
                ToolName::ContextCompact,
                &serde_json::json!({ "keepRecent": 2 }),
            )
            .await;
        assert!(r2.ok);
        assert!(r2.output.contains("无需压缩"), "二次压缩应报无可压缩");
    }

    // ---------------- 插件声明式工具（T1：命名空间接入 custom 执行链）

    fn mk_custom(name: &str, target: &str) -> crate::config::CustomTool {
        crate::config::CustomTool {
            name: name.into(),
            description: format!("插件工具 {name}"),
            kind: "http".into(),
            method: "GET".into(),
            target: target.into(),
            headers: Default::default(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": { "q": { "type": "string" } },
                "required": ["q"]
            }),
            agents: vec![],
            response_max_chars: None,
            response_select: None,
        }
    }

    /// 下发面：插件工具以命名空间名出现在 schema 下发里
    #[test]
    fn 插件工具_schema下发含命名空间前缀() {
        let gw = gw_with_approval("off", 0);
        gw.append_plugin_tools(vec![mk_custom("demo__ping", "https://example.com")]);
        let specs = gw.custom_specs_for("a1");
        let name = specs.iter().map(|s| s.name.clone()).collect::<Vec<_>>();
        assert!(
            name.contains(&"demo__ping".to_string()),
            "下发应含 demo__ping"
        );
    }

    /// 冲突面：插件工具与 config CustomTool 同名 → 先注册者胜（custom 优先，插件被拒）
    #[test]
    fn 插件工具_与config同名冲突先注册者胜() {
        let dir = std::env::temp_dir().join(format!("exm-gate-{}", crate::types::new_id()));
        std::fs::create_dir_all(dir.join("data")).unwrap();
        let store = Arc::new(Store::open(dir.join("data")).unwrap());
        let registry = Arc::new(LocalRegistry::new(dir.join("agents")).unwrap());
        let (events, _rx) = tokio::sync::broadcast::channel(64);
        let gw = ToolGateway::new(&dir, store, registry, events, SecurityConfig::default())
            .with_custom_tools(vec![mk_custom("dupe", "https://config-first.example")]);
        gw.append_plugin_tools(vec![mk_custom("dupe", "https://plugin.example")]);
        // custom 优先：下发的是 config 版 target
        assert_eq!(gw.custom_specs_for("a1").len(), 1);
        assert!(gw.has_custom_tool("dupe"));
        assert!(gw.plugin_tools().is_empty(), "同名插件工具应被拒绝注册");
    }

    /// 执行面：命名空间名走 custom 执行链（HTTP 往返可见行为）。
    /// 用不可达地址验证错误路径也走通（不 panic、错误信息含工具名口径）。
    #[tokio::test]
    async fn 插件工具_执行走custom链路() {
        let gw = gw_with_approval("off", 0);
        gw.append_plugin_tools(vec![mk_custom("demo__ping", "https://127.0.0.1:1/nope")]);
        let sid = gw.store.create_session("s", "a1").unwrap().id;
        let r = gw
            .execute_named(
                "a1",
                &sid,
                &[],
                "demo__ping",
                &serde_json::json!({ "q": "hi" }),
            )
            .await;
        // 不可达地址 → 返回错误但 ok=false 而非 panic；错误面含工具名
        assert!(!r.ok, "不可达地址应为失败结果: {:?}", r.output);
        let text = format!("{}{}", r.output, r.error.unwrap_or_default());
        assert!(
            text.contains("demo__ping") || text.len() < 500,
            "错误面应含工具名或简洁"
        );
        // 不应泄露内部路径
        assert!(!text.contains("/home/"), "不应泄露内部路径");
    }

    // ---------------- 进程插件运行时工具（T2：握手发现面接入工具网关）

    /// 假进程工具 provider（模拟 supervisor 转发：specs 带命名空间、执行回显）
    struct FakeProcessTools;

    impl crate::plugin::ToolProvider for FakeProcessTools {
        fn tool_specs(&self) -> Vec<crate::config::CustomTool> {
            vec![crate::config::CustomTool {
                name: "fakep__hello".into(),
                description: "进程握手发现".into(),
                kind: "process".into(),
                method: "POST".into(),
                target: String::new(),
                headers: Default::default(),
                parameters: serde_json::json!({ "type": "object" }),
                agents: vec![],
                response_max_chars: None,
                response_select: None,
            }]
        }
        fn call_tool(&self, name: &str, args: &serde_json::Value) -> anyhow::Result<String> {
            if name == "fakep__hello" {
                Ok(format!(
                    "hello:{}",
                    args.get("name").and_then(|v| v.as_str()).unwrap_or("?")
                ))
            } else {
                anyhow::bail!("未知工具 {name}")
            }
        }
    }

    /// T2 工具面：注册 → specs 下发可见 → 执行转发 provider → 摘除后不可见
    #[tokio::test]
    async fn 进程插件工具_注册下发执行与摘除() {
        let gw = gw_with_approval("off", 0);
        // 注册前不可见
        assert!(!gw
            .custom_specs_for("a1")
            .iter()
            .any(|s| s.name == "fakep__hello"));
        gw.register_plugin_runtime_tools("fakep", Arc::new(FakeProcessTools));
        // specs 可见（进程形态工具进入下发面）
        let specs = gw.custom_specs_for("a1");
        assert!(
            specs.iter().any(|s| s.name == "fakep__hello"),
            "注册后应下发"
        );
        // 执行转发
        let sid = gw.store.create_session("s", "a1").unwrap().id;
        let r = gw
            .execute_named(
                "a1",
                &sid,
                &[],
                "fakep__hello",
                &serde_json::json!({ "name": "巡检" }),
            )
            .await;
        assert!(r.ok, "执行应成功: {:?}", r.error);
        assert!(
            r.output.contains("hello:巡检"),
            "输出应含 provider 回显: {}",
            r.output
        );
        // 未知工具仍走原错误路径
        let r2 = gw
            .execute_named("a1", &sid, &[], "fakep__nope", &serde_json::json!({}))
            .await;
        assert!(!r2.ok);
        // 摘除后不可见
        gw.unregister_plugin_runtime_tools("fakep");
        assert!(!gw
            .custom_specs_for("a1")
            .iter()
            .any(|s| s.name == "fakep__hello"));
        let r3 = gw
            .execute_named("a1", &sid, &[], "fakep__hello", &serde_json::json!({}))
            .await;
        assert!(!r3.ok, "摘除后执行应失败");
    }

    /// 响应治理（插件生态扩展 1.1）：两级裁剪纯函数面。
    fn shaped(max: Option<usize>, select: Option<&str>, _body: &str) -> crate::config::CustomTool {
        let mut t = mk_custom("demo__shaped", "https://example.com");
        t.response_max_chars = max;
        t.response_select = select.map(str::to_string);
        t
    }

    #[test]
    fn 响应治理_select摘取与截断() {
        let body = r#"{"data":{"items":[{"title":"a"},{"title":"b"}],"total":2}}"#;
        // select 摘取数组 → 序列化字符串
        let r = shape_response(&shaped(None, Some("$.data.items"), body), body);
        assert!(r.ok, "{:?}", r.error);
        assert!(
            r.output.contains("\"title\":\"a\""),
            "应含摘取内容: {}",
            r.output
        );
        // select + maxChars 两级
        let long = format!("{{\"msg\":\"{}\"}}", "x".repeat(5000));
        let r = shape_response(&shaped(Some(100), Some("$.msg"), &long), &long);
        assert!(r.ok);
        assert!(
            r.output.chars().count() <= 100 + "…（截断）".chars().count(),
            "应受 maxChars 限制"
        );
    }

    #[test]
    fn 响应治理_路径缺失明确报错() {
        let body = r#"{"data":1}"#;
        let r = shape_response(&shaped(None, Some("$.missing.path"), body), body);
        assert!(!r.ok, "路径不存在应失败");
        assert!(
            r.error.unwrap_or_default().contains("$.missing.path"),
            "错误应含路径"
        );
    }

    #[test]
    fn 响应治理_非json忽略select() {
        let body = "纯文本响应".repeat(20);
        let r = shape_response(&shaped(Some(50), Some("$.a"), &body), &body);
        assert!(r.ok, "非 JSON 应忽略 select 仅截断");
        assert!(r.output.contains("…（截断）"));
    }

    #[test]
    fn 响应治理_缺省口径不变() {
        // 未声明任何字段：8000 截断（与迁移前行为一致）
        let long = "y".repeat(9000);
        let r = shape_response(&shaped(None, None, &long), &long);
        assert!(r.ok);
        assert!(
            r.output.starts_with('y')
                && r.output.chars().count() <= 8000 + "…（截断）".chars().count()
        );
    }

    #[test]
    fn json_path_基本语法() {
        let v: serde_json::Value =
            serde_json::from_str(r#"{"a":{"b":[{"c":1},{"c":2}],"d":"x"}}"#).unwrap();
        assert_eq!(
            json_path(&v, "$.a.b[1].c").and_then(|x| x.as_i64()),
            Some(2)
        );
        assert_eq!(json_path(&v, "a.d").and_then(|x| x.as_str()), Some("x")); // 前导 $. 可省
        assert_eq!(json_path(&v, "$.a").map(|x| x.is_object()), Some(true));
        assert!(json_path(&v, "$.a.b[9]").is_none(), "数组越界");
        assert!(json_path(&v, "$.x").is_none(), "字段不存在");
        assert!(json_path(&v, "$.").is_none(), "尾随点非法");
    }

    /// 凭据变量（plugin-ecosystem 1.3）：注入往返 / 缺键拒绝 / config 工具无归属拒绝
    #[tokio::test]
    async fn 凭据变量_注入与缺键() {
        let gw = gw_with_approval("off", 0);
        let sid = gw.store.create_session("s", "a1").unwrap().id;
        // 注入凭据解析面
        gw.set_plugin_credentials_resolver(Box::new(|pid, key| {
            (pid == "demo" && key == "api_token").then(|| "sk-real-value".to_string())
        }));
        // 缺键：调用被拒、错误含键名与指引、不发请求
        let mut t = mk_custom("demo__secure", "https://example.com/api");
        t.headers.insert(
            "Authorization".into(),
            "Bearer ${credentials.api_token}".into(),
        );
        gw.append_plugin_tools(vec![t]);
        // 先不配置凭据（resolver 只认 demo.api_token——配置上后应通过；
        // 此处 resolver 已配置，验证注入路径：把 resolver 换成缺键版本再验拒绝）
        gw.set_plugin_credentials_resolver(Box::new(|_, _| None));
        let r = gw
            .execute_named("a1", &sid, &[], "demo__secure", &serde_json::json!({}))
            .await;
        assert!(!r.ok, "缺键应拒绝");
        let err = r.error.unwrap_or_default();
        assert!(
            err.contains("api_token") && err.contains("凭据未配置"),
            "错误应含键名与指引：{err}"
        );
    }

    #[test]
    fn strip_html_多字节字符不越界() {
        // 部署实测回归：旧字节级实现在多字节字符中间切片 panic（15177 边界）
        let html = "<html><body><p>你好世界，这是中文正文𝄞音符与 emoji 😀 混排</p>\
<script>var s = \"中文脚本内容不影响正文\";</script>\
<p>İstanbul 与 straße 的大小写归一也不崩</p></body></html>";
        let text = strip_html(html);
        assert!(text.contains("你好世界"), "正文保留：{text}");
        assert!(text.contains("😀"), "emoji 完整：{text}");
        assert!(!text.contains("中文脚本内容"), "script 块剥除：{text}");
        assert!(text.contains("İstanbul"), "特殊大小写字符完整：{text}");
        // 孤立多字节 + 未闭合标签：不 panic 即过
        assert!(!strip_html("标题<").is_empty());
        assert_eq!(strip_html("<script>永不闭合"), "");
    }

    #[test]
    fn 凭据变量_解析纯函数() {
        let gw = gw_with_approval("off", 0);
        // 无 resolver：全部视为缺键
        let (out, missing) = gw.resolve_credential_vars("p", "Bearer ${credentials.k1}");
        assert_eq!(out, "Bearer ${credentials.k1}");
        assert_eq!(missing, vec!["k1"]);
        // resolver 命中：明文注入
        gw.set_plugin_credentials_resolver(Box::new(|pid, key| {
            (pid == "p" && key == "k1").then(|| "V".to_string())
        }));
        let (out, missing) =
            gw.resolve_credential_vars("p", "Bearer ${credentials.k1} tail ${credentials.k2}");
        assert_eq!(out, "Bearer V tail ${credentials.k2}");
        assert_eq!(missing, vec!["k2"]);
        // 非法键名原样保留（不视为变量）
        let (out, missing) = gw.resolve_credential_vars("p", "${credentials.ba d}");
        assert_eq!(out, "${credentials.ba d}");
        assert!(missing.is_empty());
    }

    #[tokio::test]
    async fn 凭据变量_无插件归属拒绝() {
        let gw = gw_with_approval("off", 0);
        let sid = gw.store.create_session("s", "a1").unwrap().id;
        let t = mk_custom("plain_tool", "https://example.com/${credentials.k}");
        gw.append_plugin_tools(vec![t]);
        let r = gw
            .execute_named("a1", &sid, &[], "plain_tool", &serde_json::json!({}))
            .await;
        assert!(!r.ok, "config 工具无插件归属，引用变量应拒绝");
        assert!(r.error.unwrap_or_default().contains("无插件归属"));
    }
}

// ---------------------------------------------------------------- 会话级工具注入（video-watch-agent 组 2 单测）

#[cfg(test)]
mod session_tool_tests {
    use super::*;

    fn ct(name: &str) -> crate::config::CustomTool {
        crate::config::CustomTool {
            name: name.into(),
            description: "测试会话工具".into(),
            kind: "builtin".into(),
            method: String::new(),
            target: String::new(),
            headers: Default::default(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": { "q": { "type": "string" } }
            }),
            agents: vec![],
            response_max_chars: None,
            response_select: None,
        }
    }

    #[test]
    fn 会话注入_可见性收窄() {
        let dir = std::env::temp_dir().join(format!("exm-sess-tools-{}", crate::types::new_id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("data")).unwrap();
        let store = Arc::new(crate::store::Store::open(dir.join("data")).unwrap());
        let registry = Arc::new(crate::registry::LocalRegistry::new(dir.join("agents")).unwrap());
        let (events, _rx) = tokio::sync::broadcast::channel(64);
        let gw = ToolGateway::new(
            &dir,
            store,
            registry,
            events,
            crate::config::SecurityConfig::default(),
        );

        // 注入前：普通会话看不到
        assert!(gw.session_tool_specs("s-video").is_empty());

        // 注入 video__summary 到 s-video 会话
        let exec = dynamic_exec(|_a, _s, args| async move {
            Ok(format!(
                "看到了：{}",
                args.get("q").and_then(|v| v.as_str()).unwrap_or("?")
            ))
        });
        gw.inject_session_tools("s-video", vec![(ct("video__summary"), exec)]);

        // 本会话可见、别的会话不可见
        let specs = gw.session_tool_specs("s-video");
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].name, "video__summary");
        assert!(gw.session_tool_specs("s-other").is_empty());

        // 执行面同样收窄：非注入会话调不到（未知工具）
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        rt.block_on(async {
            // 命名空间工具名走 execute_named 动态/会话表查找
            let r = gw
                .execute_named(
                    "a1",
                    "s-video",
                    &[],
                    "video__summary",
                    &serde_json::json!({ "q": "测试" }),
                )
                .await;
            // 会话工具执行：ok 或业务错误均可，但不应是「未知工具」
            let text = r.output.clone();
            assert!(!text.contains("未知工具"), "会话注入会话应命中工具：{text}");

            let r2 = gw
                .execute_named(
                    "a1",
                    "s-other",
                    &[],
                    "video__summary",
                    &serde_json::json!({ "q": "测试" }),
                )
                .await;
            assert!(!r2.ok, "普通会话不可见应失败");
            let t2 = r2.error.clone().unwrap_or_default();
            assert!(t2.contains("未知工具"), "普通会话不可见应报未知工具：{t2}");
        });

        // 清理后再不可见
        gw.clear_session_tools("s-video");
        assert!(gw.session_tool_specs("s-video").is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
