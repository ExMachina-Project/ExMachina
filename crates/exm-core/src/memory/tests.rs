//! memory 域测试（自 memory.rs 迁出，纯搬移）。

use super::*;


#[cfg(test)]
mod tests {
    use super::*;

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
}
