//! memory 域测试（自 memory.rs 迁出，纯搬移）。

use super::*;

#[cfg(test)]
#[allow(clippy::module_inception)] // 文件名即 tests.rs，内层 mod 保持搬移前结构
mod tests {
    use super::*;

    #[test]
    fn embedding_models_never_share_vector_space() {
        let dir = std::env::temp_dir().join(format!("exm-vector-space-{}", uuid::Uuid::new_v4()));
        let store = MemoryStore::open(&dir).unwrap();
        let entry = store
            .remember(&MemoryDraft::new(
                MemoryKind::Fact,
                "vector fixture",
                "semantic evidence",
            ))
            .unwrap();
        store
            .set_model_embedding(&entry.id, "p/model-a", vec![1.0, 0.0])
            .unwrap();
        store
            .set_model_embedding(&entry.id, "p/model-b", vec![0.0, 1.0])
            .unwrap();
        let query = vec![1.0, 0.0];
        let a = store
            .recall_with_model(None, "semantic", 5, None, Some(("p/model-a", &query)))
            .unwrap();
        let b = store
            .recall_with_model(None, "semantic", 5, None, Some(("p/model-b", &query)))
            .unwrap();
        let unknown = store
            .recall_with_model(None, "semantic", 5, None, Some(("p/model-c", &query)))
            .unwrap();
        assert_eq!(a[0].semantic_score, Some(1.0));
        assert_eq!(b[0].semantic_score, Some(0.0));
        assert_eq!(unknown[0].semantic_score, None);
        assert!(MemoryStore::cosine(&[1.0, 0.0], &[1.0]).is_none());
        store
            .update(&entry.id, None, Some("new body"), None, None)
            .unwrap();
        assert!(store
            .get_entry_public(&entry.id)
            .unwrap()
            .unwrap()
            .model_embeddings
            .is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn 词项切分_中文二元与英文词() {
        let t = tokens("EXMACHINA 记忆系统 memory");
        assert!(t.contains(&"记忆".to_string()));
        assert!(t.contains(&"忆系".to_string()));
        assert!(t.contains(&"memory".to_string()));
    }

    #[test]
    fn 写入去重_检索排序_衰减豁免() {
        let dir = std::env::temp_dir().join(format!("exm-mem-{}", uuid::Uuid::new_v4()));
        let store = MemoryStore::open(&dir).unwrap();

        store
            .remember(
                &MemoryDraft::new(MemoryKind::Preference, "用户偏好", "偏好中文汇报，风格简短")
                    .importance(0.9),
            )
            .unwrap();
        store
            .remember(
                &MemoryDraft::new(MemoryKind::Digest, "会话摘要", "讨论了无关话题").importance(0.3),
            )
            .unwrap();
        // 去重：同内容再写不新增，重要性提升
        let dup = store
            .remember(&MemoryDraft::new(
                MemoryKind::Preference,
                "用户偏好",
                "偏好中文汇报，风格简短",
            ))
            .unwrap();
        assert!(dup.importance > 0.9, "重复写入应提升重要性");

        let list = store.list(None, 10).unwrap();
        assert_eq!(list.len(), 2, "去重后应只有 2 条");

        let hits = store.recall("汇报风格偏好", 5, None, None, None).unwrap();
        assert!(!hits.is_empty());
        assert!(
            hits[0].entry.title.contains("偏好"),
            "偏好条目应排第一，实际: {:?}",
            hits[0].entry.title
        );

        // 衰减：固定项豁免
        store.pin(&hits[0].entry.id, true).unwrap();
        let before = store.list(Some(MemoryKind::Preference), 1).unwrap()[0].importance;
        let _ = store.decay(30.0, 0.05).unwrap();
        let after = store.list(Some(MemoryKind::Preference), 1).unwrap()[0].importance;
        assert!((before - after).abs() < 1e-9, "固定记忆不应被衰减");

        // 遗忘
        store.forget(&hits[0].entry.id).unwrap();
        assert_eq!(store.list(None, 10).unwrap().len(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 个体统计累加() {
        let dir = std::env::temp_dir().join(format!("exm-stat-{}", uuid::Uuid::new_v4()));
        let store = MemoryStore::open(&dir).unwrap();
        store
            .record_agent_outcome("context-agent", "done", 0.9)
            .unwrap();
        store
            .record_agent_outcome("context-agent", "blocked", 0.5)
            .unwrap();
        let stats = store.agent_stats(10).unwrap();
        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0].runs, 2);
        assert_eq!(stats[0].done, 1);
        assert_eq!(stats[0].blocked, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 个体记忆与群体记忆分层() {
        let dir = std::env::temp_dir().join(format!("exm-layer-{}", uuid::Uuid::new_v4()));
        let store = MemoryStore::open(&dir).unwrap();

        // 个体记忆：归属 context-agent
        let mut draft =
            MemoryDraft::new(MemoryKind::Lesson, "上下文体教训", "重构配置前先跑全量测试")
                .importance(0.8);
        draft.agent_id = Some("context-agent".into());
        store.remember(&draft).unwrap();

        // 群体记忆：无归属，人人可见
        store
            .remember(&MemoryDraft::new(
                MemoryKind::Decision,
                "共享决策",
                "构建入口统一为 cargo build",
            ))
            .unwrap();

        // 归属者：个体 + 群体都可见，且个体条目带"个体记忆"标记
        let hits = store
            .recall_for_agent("context-agent", "测试 构建", 10, None, None)
            .unwrap();
        let owners: Vec<bool> = hits.iter().map(|h| h.entry.agent_id.is_some()).collect();
        assert!(owners.contains(&true), "归属者应看到个体记忆");
        assert!(owners.contains(&false), "归属者也应看到群体记忆");
        let ind = hits.iter().find(|h| h.entry.agent_id.is_some()).unwrap();
        assert!(ind.reasons.iter().any(|r| r == "个体记忆"));

        // 其他个体：只能看到群体记忆，看不到他人私有
        let hits2 = store
            .recall_for_agent("scout-agent", "测试 构建", 10, None, None)
            .unwrap();
        assert!(
            hits2.iter().all(|h| h.entry.agent_id.is_none()),
            "其他个体不应看到私有记忆，实际: {:?}",
            hits2
                .iter()
                .map(|h| h.entry.title.clone())
                .collect::<Vec<_>>()
        );

        // 群体检索（指挥体视角）：仅群体记忆
        let shared = store.recall("测试 构建", 10, None, None, None).unwrap();
        assert!(shared.iter().all(|h| h.entry.agent_id.is_none()));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 组级记忆隔离与全局条目() {
        let dir = std::env::temp_dir().join(format!("exm-group-{}", uuid::Uuid::new_v4()));
        let store = MemoryStore::open(&dir).unwrap();

        // A 组条目 / B 组条目 / 全局条目（None = 处处可见）
        store
            .remember(
                &MemoryDraft::new(
                    MemoryKind::Lesson,
                    "翻译组教训",
                    "术语表必须先于正文翻译建立",
                )
                .group("gA"),
            )
            .unwrap();
        store
            .remember(
                &MemoryDraft::new(MemoryKind::Lesson, "游戏组教训", "数值改动必须附模拟对比")
                    .group("gB"),
            )
            .unwrap();
        store
            .remember(&MemoryDraft::new(
                MemoryKind::Fact,
                "全局事实",
                "构建入口统一为 cargo build",
            ))
            .unwrap();

        // A 组视角：自己的 + 全局；看不到 B 组
        let a_hits = store
            .recall("教训 构建", 10, None, Some("gA"), None)
            .unwrap();
        let titles: Vec<&str> = a_hits.iter().map(|h| h.entry.title.as_str()).collect();
        assert!(titles.contains(&"翻译组教训"), "A 组应看到本组条目");
        assert!(titles.contains(&"全局事实"), "A 组应看到全局条目");
        assert!(!titles.contains(&"游戏组教训"), "A 组不应看到 B 组条目");

        // B 组视角对称
        let b_hits = store
            .recall("教训 数值", 10, None, Some("gB"), None)
            .unwrap();
        assert!(b_hits
            .iter()
            .all(|h| h.entry.group_id.as_deref() != Some("gA")));

        // 个体检索同样受组约束：同一 identifier 的私有记忆不跨组泄漏
        let mut draft = MemoryDraft::new(MemoryKind::Lesson, "A组私有教训", "该组内教训仅组内可见");
        draft.agent_id = Some("shared-agent".into());
        draft.group_id = Some("gA".into());
        store.remember(&draft).unwrap();
        let in_ga = store
            .recall_for_agent("shared-agent", "私有教训", 10, Some("gA"), None)
            .unwrap();
        assert!(in_ga.iter().any(|h| h.entry.title == "A组私有教训"));
        let in_gb = store
            .recall_for_agent("shared-agent", "私有教训", 10, Some("gB"), None)
            .unwrap();
        assert!(
            in_gb.iter().all(|h| h.entry.title != "A组私有教训"),
            "组外不应检索到他人组内私有记忆，实际: {:?}",
            in_gb
                .iter()
                .map(|h| h.entry.title.clone())
                .collect::<Vec<_>>()
        );

        // 去重哈希按组隔离：同内容跨组各自成条
        let same_title = "同内容条目";
        let same_body = "内容完全一致";
        store
            .remember(&MemoryDraft::new(MemoryKind::Fact, same_title, same_body).group("gA"))
            .unwrap();
        store
            .remember(&MemoryDraft::new(MemoryKind::Fact, same_title, same_body).group("gB"))
            .unwrap();
        store
            .remember(&MemoryDraft::new(MemoryKind::Fact, same_title, same_body))
            .unwrap();
        let all = store.list(None, 50).unwrap();
        let count = all.iter().filter(|e| e.title == same_title).count();
        assert_eq!(
            count, 3,
            "同内容在 gA/gB/全局 应为 3 条独立条目，实际 {count}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 空查询回退近期_分层不越权() {
        let dir = std::env::temp_dir().join(format!("exm-mem-emptyq-{}", uuid::Uuid::new_v4()));
        let store = MemoryStore::open(&dir).unwrap();

        // 甲的私有记忆 + 乙的私有记忆 + 一条群体记忆
        let mut a = MemoryDraft::new(MemoryKind::Lesson, "甲的私有教训", "只属于 agent-a");
        a.agent_id = Some("agent-a".into());
        store.remember(&a).unwrap();
        let mut b = MemoryDraft::new(MemoryKind::Lesson, "乙的私有教训", "只属于 agent-b");
        b.agent_id = Some("agent-b".into());
        store.remember(&b).unwrap();
        store
            .remember(&MemoryDraft::new(
                MemoryKind::Decision,
                "群体决策",
                "无归属共享",
            ))
            .unwrap();

        // 空查询回退近期：个体视角只见「自己私有 + 群体」，他人的私有不得混入
        let hits_a = store
            .recall_for_agent("agent-a", "", 10, None, None)
            .unwrap();
        let titles: Vec<&str> = hits_a.iter().map(|h| h.entry.title.as_str()).collect();
        assert!(titles.contains(&"甲的私有教训"), "本人私有可见");
        assert!(titles.contains(&"群体决策"), "群体共享可见");
        assert!(
            !titles.contains(&"乙的私有教训"),
            "空查询不得泄漏他人私有，实际 {titles:?}"
        );

        // 群体检索（SharedOnly）空查询：仅群体
        let shared = store.recall("", 10, None, None, None).unwrap();
        assert!(
            shared.iter().all(|h| h.entry.agent_id.is_none()),
            "群体视角空查询不得带出个体私有"
        );

        // 全量视角（管理）空查询：全部可见
        let all = store.recall_all("   ", 10, None, None).unwrap();
        assert_eq!(all.len(), 3, "管理视角空查询可见全部");

        // 纯符号查询（无有效词项）与空串同路径，分层同样生效
        let sym = store
            .recall_for_agent("agent-b", "!!!@@@###", 10, None, None)
            .unwrap();
        assert!(sym
            .iter()
            .all(|h| h.entry.agent_id.as_deref() != Some("agent-a")));

        // 列示入口的分层口径：recent_in_layer
        let list_b = store.recent_in_layer(Some("agent-b"), 10, None).unwrap();
        assert!(list_b
            .iter()
            .all(|h| h.entry.agent_id.as_deref() != Some("agent-a")));
        let list_all = store.recent_in_layer(None, 10, None).unwrap();
        assert_eq!(list_all.len(), 3);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 衰减数学_边界自洽() {
        // 曲线自洽：一个半衰期恰好减半，两个半衰期剩四分之一
        assert!((decay_factor(0.0, 30.0) - 1.0).abs() < 1e-9, "零龄不衰减");
        assert!((decay_factor(30.0, 30.0) - 0.5).abs() < 1e-9);
        assert!((decay_factor(60.0, 30.0) - 0.25).abs() < 1e-9);
        // 0 天 / 负数 / NaN：无数学意义，按 1 天兜底（与 1 天曲线重合）
        for bad in [0.0, -3.0, f64::NAN] {
            assert!(
                (decay_factor(10.0, bad) - decay_factor(10.0, 1.0)).abs() < 1e-12,
                "非法半衰期 {bad} 应按 1 天处理"
            );
        }
        // 极大半衰期：等于不衰减（永不遗忘）
        assert!((decay_factor(365.0, 1e300) - 1.0).abs() < 1e-9);
        // 未来时间戳：不增寿（因子恒 1）
        assert!(
            (decay_factor(-7.0, 30.0) - 1.0).abs() < 1e-12,
            "未来时间戳不得反向增寿"
        );
        // 下限保底与上限钳制
        let decayed = decayed_importance(0.5, 40.0, 30.0, 0.05);
        assert!(
            (decayed - 0.5 * 0.5f64.powf(40.0 / 30.0)).abs() < 1e-9,
            "应严格落在半衰曲线上"
        );
        assert_eq!(
            decayed_importance(0.01, 1000.0, 30.0, 0.05),
            0.05,
            "跌破下限保到 floor"
        );
        assert_eq!(
            decayed_importance(0.9, 0.0, 30.0, 1.5),
            1.0,
            "floor 越界不得把重要性抬出 [0,1]"
        );
        assert_eq!(
            decayed_importance(0.8, -100.0, 30.0, 0.05),
            0.8,
            "未来时间戳下重要性原样保留"
        );
    }

    /// 衰减整链路：过期条目严格落在半衰曲线上；未来时间戳不被增寿
    #[test]
    fn 衰减整链路_过期变淡_未来不增寿() {
        let dir = std::env::temp_dir().join(format!("exm-mem-decay-{}", uuid::Uuid::new_v4()));
        let store = MemoryStore::open(&dir).unwrap();
        let stale = store
            .remember(
                &MemoryDraft::new(MemoryKind::Fact, "过期记忆", "四十天前的旧事").importance(0.8),
            )
            .unwrap();
        let future = store
            .remember(
                &MemoryDraft::new(MemoryKind::Fact, "未来记忆", "时钟回拨产生的脏数据")
                    .importance(0.6),
            )
            .unwrap();

        // 直改条目时间戳模拟「40 天前」与「未来 1 天」（造存储态，避免睡真实时间）
        let stale_ts = (chrono::Utc::now() - chrono::Duration::days(40)).to_rfc3339();
        let future_ts = (chrono::Utc::now() + chrono::Duration::days(1)).to_rfc3339();
        patch_updated_at(&store, &stale.id, &stale_ts);
        patch_updated_at(&store, &future.id, &future_ts);

        let changed = store.decay(30.0, 0.05).unwrap();
        assert_eq!(changed, 1, "只有过期条目需要改写");

        let stale_after = store.get_entry_public(&stale.id).unwrap().unwrap();
        let expect = 0.8 * 0.5f64.powf(40.0 / 30.0);
        assert!(
            (stale_after.importance - expect).abs() < 1e-9,
            "应严格落在半衰曲线上：{} vs {expect}",
            stale_after.importance
        );

        let future_after = store.get_entry_public(&future.id).unwrap().unwrap();
        assert!(
            (future_after.importance - 0.6).abs() < 1e-12,
            "未来时间戳不得被衰减反向增寿"
        );
        assert!(future_after.importance <= 1.0, "重要性不得越出 [0,1]");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 测试辅助：直改条目 updated_at（制造时间态，不睡真实时钟）
    fn patch_updated_at(store: &MemoryStore, id: &str, ts: &str) {
        let mut e = store.get_entry_public(id).unwrap().unwrap();
        e.updated_at = ts.to_string();
        store.save_entry(&e).unwrap();
    }

    #[test]
    fn 并发写入_同内容去重不裂条() {
        let dir = std::env::temp_dir().join(format!("exm-mem-race-{}", uuid::Uuid::new_v4()));
        let store = std::sync::Arc::new(MemoryStore::open(&dir).unwrap());

        // 同内容并发：哈希查重与写入必须原子，否则各建一条（去重契约被破坏）
        // （词项刻意与其他条目不重叠，避免召回阶段按词项撞上）
        let mut handles = Vec::new();
        for _ in 0..16 {
            let s = store.clone();
            let d = MemoryDraft::new(MemoryKind::Fact, "原子去重约定", "十六线程同记一事只落一条");
            handles.push(std::thread::spawn(move || s.remember(&d).unwrap()));
        }
        for h in handles {
            h.join().unwrap();
        }
        let all = store.list(None, 100).unwrap();
        assert_eq!(all.len(), 1, "并发同内容应只落 1 条，实际 {} 条", all.len());
        assert!(
            (all[0].importance - 1.0).abs() < 1e-9,
            "16 次写入的重要性提升应封顶 1.0，实际 {}",
            all[0].importance
        );

        // 不同内容并发：互不丢失
        let mut handles = Vec::new();
        for i in 0..8 {
            let s = store.clone();
            handles.push(std::thread::spawn(move || {
                s.remember(&MemoryDraft::new(
                    MemoryKind::Fact,
                    format!("独立条目{i}"),
                    format!("第 {i} 条独立内容"),
                ))
                .unwrap();
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(
            store.list(None, 100).unwrap().len(),
            9,
            "8 条不同内容应全部落库"
        );

        // 去重后的哈希索引仍可用：并发写入的条目可被正常召回
        let hits = store.recall("原子去重", 5, None, None, None).unwrap();
        assert_eq!(hits.len(), 1, "哈希索引未裂，召回应恰好 1 条");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 写入边界_超长截断与特殊字符() {
        let dir = std::env::temp_dir().join(format!("exm-mem-cap-{}", uuid::Uuid::new_v4()));
        let store = MemoryStore::open(&dir).unwrap();

        // 超长正文/标题：按字符截断（不劈 UTF-8），返回值即落库内容
        let long_body = "长".repeat(50_000);
        let long_title = format!("{}超长标题", "标".repeat(300));
        let e = store
            .remember(&MemoryDraft::new(MemoryKind::Fact, long_title, long_body))
            .unwrap();
        assert_eq!(
            e.body.chars().count(),
            MemoryStore::BODY_CAP_CHARS,
            "正文应截到上限"
        );
        assert_eq!(
            e.title.chars().count(),
            MemoryStore::TITLE_CAP_CHARS,
            "标题应截到上限"
        );

        // 特殊字符：emoji / 换行 / 引号 / 路径分隔符——原样存取、去重键稳定
        let weird = "含\"引号\"与\\斜杠、/path:分隔符*与\n换行 和 emoji 🦾🤖";
        let w1 = store
            .remember(&MemoryDraft::new(MemoryKind::Lesson, weird, weird))
            .unwrap();
        let w2 = store
            .remember(&MemoryDraft::new(MemoryKind::Lesson, weird, weird))
            .unwrap();
        assert_eq!(w1.id, w2.id, "特殊字符内容应稳定去重（同一哈希）");
        let back = store.get_entry_public(&w1.id).unwrap().unwrap();
        assert_eq!(back.body, weird, "特殊字符正文应原样存取");

        // 首尾空白规整后等价去重：同一句话多打了空格不裂条
        let padded = store
            .remember(&MemoryDraft::new(
                MemoryKind::Lesson,
                format!("{weird} "),
                format!("{weird} "),
            ))
            .unwrap();
        assert_eq!(padded.id, w1.id, "仅首尾空白差异应视为同一条");

        // 截断后的条目仍可被正常召回（索引按落库内容建立）
        let hits = store.recall("超长标题", 5, None, None, None).unwrap();
        assert!(
            hits.iter()
                .any(|h| h.entry.title.chars().count() == MemoryStore::TITLE_CAP_CHARS),
            "截断条目应在索引内，实际: {:?}",
            hits.iter()
                .map(|h| h.entry.title.clone())
                .collect::<Vec<_>>()
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 条目编辑_哈希迁移与索引重建() {
        let dir = std::env::temp_dir().join(format!("exm-mem-edit-{}", uuid::Uuid::new_v4()));
        let store = MemoryStore::open(&dir).unwrap();

        let e = store
            .remember(&MemoryDraft::new(MemoryKind::Fact, "旧标题", "旧正文 alpha").importance(0.4))
            .unwrap();
        let created = e.created_at.clone();

        // 改正文 + 改类型：检索命中新内容，created_at 不变
        let updated = store
            .update(
                &e.id,
                None,
                Some("新正文 beta 检索词"),
                None,
                Some("lesson"),
            )
            .unwrap()
            .expect("条目应存在");
        assert_eq!(updated.kind, MemoryKind::Lesson);
        assert_eq!(updated.created_at, created, "编辑不应改变创建时间");
        assert_eq!(updated.body, "新正文 beta 检索词");
        let hits = store.recall("beta 检索词", 5, None, None, None).unwrap();
        assert!(
            hits.iter().any(|h| h.entry.id == e.id),
            "检索应命中编辑后的正文"
        );

        // 哈希迁移：同新内容再写命中去重（同 id、不新增）
        let dup = store
            .remember(&MemoryDraft::new(
                MemoryKind::Fact,
                "旧标题",
                "新正文 beta 检索词",
            ))
            .unwrap();
        assert_eq!(dup.id, e.id, "同内容二次写入应命中编辑后的条目");
        assert_eq!(store.list(None, 10).unwrap().len(), 1);

        // 旧哈希已删除：旧内容不再与编辑后的条目去重，可重新成条
        store
            .remember(&MemoryDraft::new(
                MemoryKind::Fact,
                "旧标题",
                "旧正文 alpha",
            ))
            .unwrap();
        assert_eq!(store.list(None, 10).unwrap().len(), 2);

        // 非法 kind 拒绝；不存在的 id 返回 None
        assert!(
            store
                .update(&e.id, None, None, None, Some("bogus"))
                .is_err(),
            "未知类型应报错"
        );
        assert!(store
            .update("missing-id", Some("x"), None, None, None)
            .unwrap()
            .is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---------------- 版本链（capability-parity 6.5，spec: agent-learning-loop「受控写入和版本回滚」） ----------------

    fn open_store(tag: &str) -> (MemoryStore, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("exm-memver-{tag}-{}", uuid::Uuid::new_v4()));
        (MemoryStore::open(&dir).unwrap(), dir)
    }

    /// 版本号单调递增，回滚不是倒退而是「以旧内容写新版本」；每一步的历史行与内容对得上。
    /// 期望值独立推算：created=v1 → updated=v2 → updated=v3 → 回滚到 v1 = v4 → 遗忘行 v5 →
    /// 撤销遗忘 = v6。
    #[test]
    fn 版本链_创建更新回滚撤销遗忘_版本单调且内容对应() {
        let (store, dir) = open_store("chain");
        let e1 = store
            .remember(&MemoryDraft::new(MemoryKind::Fact, "标题", "第一版正文").group("g1"))
            .unwrap();
        assert_eq!(e1.version_label(), 1, "新建即 v1");

        let e2 = store
            .update(&e1.id, None, Some("第二版正文"), None, None)
            .unwrap()
            .unwrap();
        let e3 = store
            .update(&e1.id, None, Some("第三版正文"), None, None)
            .unwrap()
            .unwrap();
        assert_eq!((e2.version_label(), e3.version_label()), (2, 3));

        let e4 = store.rollback(&e1.id, 1).unwrap();
        assert_eq!(
            e4.version_label(),
            4,
            "回滚到 v1 产生的是 v4，不是把版本号改回 1"
        );
        assert_eq!(e4.body, "第一版正文", "内容恢复为 v1");

        let hist = store.history(&e1.id).unwrap();
        let rows: Vec<(u64, &str, &str)> = hist
            .iter()
            .map(|r| (r.version, r.change.as_str(), r.body.as_str()))
            .collect();
        assert_eq!(
            rows,
            vec![
                (1, "created", "第一版正文"),
                (2, "updated", "第二版正文"),
                (3, "updated", "第三版正文"),
                (4, "rolled_back", "第一版正文"),
            ],
            "每个版本号恰好一行，行内存该版本的内容"
        );

        store.forget(&e1.id).unwrap();
        assert!(
            store.get_entry_public(&e1.id).unwrap().is_none(),
            "遗忘后条目不再存在"
        );
        assert!(store.is_forgotten(&e1.id).unwrap());
        let last = store.history(&e1.id).unwrap().pop().unwrap();
        assert_eq!(
            (last.version, last.change.as_str()),
            (5, "forgotten"),
            "遗忘是版本链上的一行"
        );
        assert_eq!(
            last.body, "第一版正文",
            "遗忘行记的是被遗忘那一刻的内容（v4 的内容）"
        );

        let e6 = store.restore(&e1.id).unwrap();
        assert_eq!(e6.version_label(), 6, "撤销遗忘接着版本链走");
        assert_eq!(e6.body, "第一版正文");
        assert_eq!(e6.group_id.as_deref(), Some("g1"), "归属恢复原样");
        assert!(!store.is_forgotten(&e1.id).unwrap(), "撤销后墓碑清除");
        let hits = store.recall("第一版", 5, None, Some("g1"), None).unwrap();
        assert!(
            hits.iter().any(|h| h.entry.id == e1.id),
            "撤销遗忘后重新可被召回"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 注入块标注来源与版本（spec：注入结果 MUST 标注来源和版本）。
    /// 版本号必须跟随真实版本链（新建 v1 → 编辑 v2 → 回滚产生 v4），不是恒定的 v1。
    #[test]
    fn 注入块_标注来源与版本_且跟随版本链() {
        let (store, dir) = open_store("inject-label");
        let e1 = store
            .remember(&MemoryDraft::new(
                MemoryKind::Decision,
                "部署窗口",
                "第一版正文",
            ))
            .unwrap();
        let hits = |store: &MemoryStore, id: &str| -> Vec<RecallHit> {
            store
                .recall("部署窗口", 5, None, None, None)
                .unwrap()
                .into_iter()
                .filter(|h| h.entry.id == id)
                .collect()
        };

        let block_v1 = MemoryStore::render_prompt_block(&hits(&store, &e1.id));
        assert!(
            block_v1.contains(&format!("[来源：记忆 {} v1]", e1.id)),
            "v1 注入行应带来源与版本标注 —— {block_v1}"
        );

        store
            .update(&e1.id, None, Some("第二版正文"), None, None)
            .unwrap()
            .unwrap();
        let block_v2 = MemoryStore::render_prompt_block(&hits(&store, &e1.id));
        assert!(
            block_v2.contains(&format!("[来源：记忆 {} v2]", e1.id)),
            "编辑后注入行应标 v2 —— {block_v2}"
        );

        store.rollback(&e1.id, 1).unwrap();
        let block_v3 = MemoryStore::render_prompt_block(&hits(&store, &e1.id));
        assert!(
            block_v3.contains(&format!("[来源：记忆 {} v3]", e1.id)),
            "回滚（以旧内容写新版本）后注入行应标 v3 —— {block_v3}"
        );
        assert!(
            block_v3.contains("第一版正文"),
            "回滚后注入的是恢复的内容 —— {block_v3}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 格式错误或越界的更新不改变旧版本，也不留下半成品历史。
    #[test]
    fn 版本链_格式错误或越界_旧版本不变且无半成品() {
        let (store, dir) = open_store("reject");
        let e = store
            .remember(&MemoryDraft::new(MemoryKind::Fact, "有效标题", "有效正文"))
            .unwrap();
        let before_entry = store.get_entry_public(&e.id).unwrap().unwrap();
        let before_hist = store.history(&e.id).unwrap();

        let err = store
            .update(&e.id, Some("   "), None, None, None)
            .unwrap_err();
        assert!(err.to_string().contains("title 不能为空"), "{err}");
        let err = store.update(&e.id, None, Some(""), None, None).unwrap_err();
        assert!(err.to_string().contains("body 不能为空"), "{err}");
        let err = store
            .update(&e.id, None, None, None, Some("bogus"))
            .unwrap_err();
        assert!(err.to_string().contains("未知记忆类型"), "{err}");

        let after = store.get_entry_public(&e.id).unwrap().unwrap();
        assert_eq!(after.version_label(), before_entry.version_label());
        assert_eq!(after.title, before_entry.title);
        assert_eq!(after.body, before_entry.body);
        assert_eq!(after.kind, before_entry.kind);
        assert_eq!(
            after.updated_at, before_entry.updated_at,
            "被拒的更新不刷新时间"
        );
        assert_eq!(
            store.history(&e.id).unwrap().len(),
            before_hist.len(),
            "被拒的更新不写历史"
        );

        // 越界：与另一条条目内容相同（会夺走对方的去重索引）→ 拒绝，双方都不变
        let other = store
            .remember(&MemoryDraft::new(
                MemoryKind::Fact,
                "另一条",
                "独有内容 omega",
            ))
            .unwrap();
        let err = store
            .update(&e.id, Some("另一条"), Some("独有内容 omega"), None, None)
            .unwrap_err();
        assert!(err.to_string().contains("已存在相同内容"), "{err}");
        assert_eq!(
            store.get_entry_public(&e.id).unwrap().unwrap().body,
            "有效正文"
        );
        assert_eq!(
            store
                .get_entry_public(&other.id)
                .unwrap()
                .unwrap()
                .version_label(),
            1
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 内容完全没变的更新不是变更：不升版、不写历史。
    #[test]
    fn 版本链_无变化的更新_不升版不记历史() {
        let (store, dir) = open_store("noop");
        let e = store
            .remember(&MemoryDraft::new(MemoryKind::Fact, "标题", "正文"))
            .unwrap();
        let same = store
            .update(&e.id, Some("标题"), Some("正文"), None, None)
            .unwrap()
            .unwrap();
        assert_eq!(same.version_label(), 1);
        assert_eq!(store.history(&e.id).unwrap().len(), 1, "只有 created 一行");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 回滚到不存在的版本 / 撤销未遗忘的条目 → 具体拒绝原因，且什么都不改。
    #[test]
    fn 版本链_回滚不存在版本与撤销未遗忘_具体拒绝() {
        let (store, dir) = open_store("missing");
        let e = store
            .remember(&MemoryDraft::new(MemoryKind::Fact, "标题", "正文"))
            .unwrap();
        let err = store.rollback(&e.id, 9).unwrap_err();
        assert!(err.to_string().contains("历史版本不存在"), "{err}");
        assert_eq!(
            store
                .get_entry_public(&e.id)
                .unwrap()
                .unwrap()
                .version_label(),
            1
        );

        let err = store.restore(&e.id).unwrap_err();
        assert!(err.to_string().contains("仍存在"), "{err}");

        let err = store.restore("never-existed").unwrap_err();
        assert!(err.to_string().contains("不存在"), "{err}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 旧条目（字段引入前写入，version 缺省为 0）按 v1 陈述，第一次更新得到 v2。
    #[test]
    fn 版本链_旧数据无版本字段_按v1陈述且可升版() {
        let (store, dir) = open_store("legacy");
        let e = store
            .remember(&MemoryDraft::new(MemoryKind::Fact, "旧数据", "正文"))
            .unwrap();
        // 模拟引入版本字段之前落盘的条目：文件里没有 version 键
        let path = dir.join("memory/entries").join(format!("{}.json", e.id));
        let mut v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        v.as_object_mut().unwrap().remove("version");
        std::fs::write(&path, serde_json::to_string(&v).unwrap()).unwrap();
        let legacy = store.get_entry_public(&e.id).unwrap().unwrap();
        assert_eq!(legacy.version, 0, "旧数据读出为 0");
        assert_eq!(legacy.version_label(), 1, "对外按 v1 陈述");
        let e2 = store
            .update(&e.id, None, Some("新正文"), None, None)
            .unwrap()
            .unwrap();
        assert_eq!(e2.version_label(), 2);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 记忆「过期」= 已被遗忘（或被新版本取代）：不再进入召回结果——这是注入的上游。
    ///
    /// 注意断言口径：`recall` 在查询词项**无命中**时会回退到「近期列表」（reasons =
    /// 「按重要性/时间排序」），所以不能用「结果为空」来判断旧内容是否可见——库里只要
    /// 还有别的条目，回退就会把它列出来。真正的判据是：旧内容的词项不再命中倒排索引，
    /// 即命中的 reasons 里不出现「词项覆盖」。
    #[test]
    fn 版本链_遗忘后不再召回_旧版本内容不再可见() {
        let (store, dir) = open_store("expired");
        let e = store
            .remember(&MemoryDraft::new(
                MemoryKind::Fact,
                "部署步骤",
                "先跑迁移再发布",
            ))
            .unwrap();
        let term_hit = |q: &str| {
            store
                .recall(q, 5, None, None, None)
                .unwrap()
                .into_iter()
                .any(|h| h.entry.id == e.id && h.reasons.iter().any(|r| r.starts_with("词项覆盖")))
        };
        assert!(term_hit("迁移"), "基线：旧内容的词项可被命中");

        store.forget(&e.id).unwrap();
        assert!(!term_hit("迁移"), "遗忘后不可召回");
        assert!(store.list(None, 50).unwrap().is_empty(), "遗忘后不在列表里");

        // 旧版本内容同样不可见：更新掉的内容不会经词项索引回流
        let e2 = store.restore(&e.id).unwrap();
        store
            .update(&e2.id, None, Some("完全不同的新内容 zeta"), None, None)
            .unwrap();
        assert!(!term_hit("迁移"), "被新版本取代的旧内容不再被词项命中");
        assert!(
            term_hit("zeta"),
            "新内容的词项照常命中（证明探测器在读同一个条目）"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 回归（先于 capability-parity 存在的缺陷）：`update` 过去先覆盖 title/body 再
    /// `unindex_entry`，删的是尚不存在的新词项，旧词项永久留在倒排索引里——编辑过的
    /// 记忆仍会被旧内容的关键词召回。此测试不经过版本链的回滚/撤销，只钉 update 本身。
    #[test]
    fn 条目编辑_旧词项从倒排索引移除_新词项可命中() {
        let (store, dir) = open_store("unindex");
        let e = store
            .remember(&MemoryDraft::new(
                MemoryKind::Fact,
                "部署步骤",
                "先跑迁移再发布",
            ))
            .unwrap();
        let term_hit = |q: &str| {
            store
                .recall(q, 5, None, None, None)
                .unwrap()
                .into_iter()
                .any(|h| h.entry.id == e.id && h.reasons.iter().any(|r| r.starts_with("词项覆盖")))
        };
        assert!(term_hit("迁移"), "基线：旧词项可命中");
        store
            .update(&e.id, None, Some("完全不同的新内容 zeta"), None, None)
            .unwrap();
        assert!(!term_hit("迁移"), "编辑后旧词项不再命中");
        assert!(term_hit("zeta"), "编辑后新词项命中");

        // 只改 tags 同理：旧 tag 的词项要清掉
        let t = store
            .remember(
                &MemoryDraft::new(MemoryKind::Fact, "标签条目", "正文 alpha").tags(&["kubernetes"]),
            )
            .unwrap();
        assert!(store
            .recall("kubernetes", 5, None, None, None)
            .unwrap()
            .iter()
            .any(|h| h.entry.id == t.id && h.reasons.iter().any(|r| r.starts_with("词项覆盖"))));
        store
            .update(&t.id, None, None, Some(vec!["docker".into()]), None)
            .unwrap();
        let hits = store.recall("kubernetes", 5, None, None, None).unwrap();
        assert!(
            !hits
                .iter()
                .any(|h| h.entry.id == t.id && h.reasons.iter().any(|r| r.starts_with("词项覆盖"))),
            "改掉 tag 后旧 tag 不再命中"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
    /// 已遗忘清单（capability-parity 6.6）：治理面「撤销遗忘」入口的数据源。
    /// 墓碑保留归属字段 → 清单里读出来就是遗忘当时的条目；撤销遗忘后从清单消失。
    #[test]
    fn 已遗忘清单_保留归属_撤销后消失_顺序确定() {
        let (store, dir) = open_store("forgotten-list");
        assert!(
            store.list_forgotten().unwrap().is_empty(),
            "空库：无已遗忘条目"
        );

        let a = store
            .remember(&MemoryDraft::new(MemoryKind::Fact, "甲", "正文甲").group("g1"))
            .unwrap();
        let b = store
            .remember(
                &MemoryDraft::new(MemoryKind::Lesson, "乙", "正文乙")
                    .group("g1")
                    .agent("agent-x"),
            )
            .unwrap();

        // 未遗忘的条目不在清单里
        assert!(
            store.list_forgotten().unwrap().is_empty(),
            "未遗忘不得出现在已遗忘清单"
        );

        // 遗忘 a、b。清单的顺序口径：按墓碑里的 updated_at（= 遗忘时刻）倒序，平局按 id。
        // updated_at 是秒级精度，测试无法可靠制造时间差，所以不假设谁先谁后——
        // 而是独立复述口径，验证返回序列自身已按该键排好（且因此每次读出来都一样）。
        store.forget(&a.id).unwrap();
        store.forget(&b.id).unwrap();
        let key = |e: &MemoryEntry| (e.updated_at.clone(), e.id.clone());
        let is_ordered = |rows: &[MemoryEntry]| {
            rows.windows(2).all(|w| {
                let (x, y) = (key(&w[0]), key(&w[1]));
                // 前一项的遗忘时刻更晚，或时刻相同且 id 更小
                x.0 > y.0 || (x.0 == y.0 && x.1 < y.1)
            })
        };
        let rows = store.list_forgotten().unwrap();
        let mut sorted_ids: Vec<&str> = rows.iter().map(|e| e.id.as_str()).collect();
        sorted_ids.sort_unstable();
        let mut want: Vec<String> = vec![a.id.clone(), b.id.clone()];
        want.sort_unstable();
        assert_eq!(
            sorted_ids,
            want.iter().map(String::as_str).collect::<Vec<_>>(),
            "恰好这两条已遗忘条目在清单里"
        );
        assert!(
            is_ordered(&rows),
            "清单按遗忘时刻倒序、平局按 id：{:?}",
            rows.iter().map(key).collect::<Vec<_>>()
        );
        // 再读一次：顺序稳定（不随底层迭代序漂移）
        let again = store.list_forgotten().unwrap();
        assert_eq!(
            rows.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
            again.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
            "重复读取顺序一致"
        );

        // 归属原样保留：撤销遗忘后不能「换了主人」
        let b_row = rows.iter().find(|e| e.id == b.id).unwrap();
        assert_eq!(b_row.agent_id.as_deref(), Some("agent-x"), "个体归属保留");
        assert_eq!(b_row.group_id.as_deref(), Some("g1"), "群体归属保留");
        assert!(matches!(b_row.kind, MemoryKind::Lesson), "类型保留");
        assert_eq!(b_row.title, "乙", "标题保留");

        // 撤销遗忘 b → 只剩 a
        store.restore(&b.id).unwrap();
        let after_restore = store.list_forgotten().unwrap();
        let ids: Vec<&str> = after_restore.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(
            ids,
            vec![a.id.as_str()],
            "撤销遗忘后从清单消失，且恢复的条目不再算已遗忘"
        );
        assert!(
            store.get_entry(&b.id).unwrap().is_some(),
            "b 已回到常规列表"
        );

        // 再遗忘一条：清单随之增长，且依然满足顺序口径
        let c = store
            .remember(&MemoryDraft::new(MemoryKind::Fact, "丙", "正文丙").group("g1"))
            .unwrap();
        store.forget(&c.id).unwrap();
        let rows = store.list_forgotten().unwrap();
        let mut sorted_ids: Vec<&str> = rows.iter().map(|e| e.id.as_str()).collect();
        sorted_ids.sort_unstable();
        let mut want: Vec<String> = vec![a.id.clone(), c.id.clone()];
        want.sort_unstable();
        assert_eq!(
            sorted_ids,
            want.iter().map(String::as_str).collect::<Vec<_>>(),
            "撤销 b 后再遗忘 c：清单应恰为 a 与 c 两条"
        );
        assert!(
            is_ordered(&rows),
            "清单依然有序：{:?}",
            rows.iter().map(key).collect::<Vec<_>>()
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
