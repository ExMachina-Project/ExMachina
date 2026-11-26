//! panel 域测试（自 panel.rs 迁出，纯搬移）。

use super::*;

#[cfg(test)]
#[allow(clippy::module_inception)] // 文件名即 tests.rs，内层 mod 保持搬移前结构
mod tests {
    use super::*;

    #[test]
    fn 分词_支持引号与普通词() {
        assert_eq!(
            tokenize("create \"研发 组\" --id dev"),
            vec!["create", "研发 组", "--id", "dev"]
        );
        assert!(tokenize("   ").is_empty());
    }

    #[test]
    fn 参数解析_键值_布尔_位置() {
        let t = tokenize("add --kind fact \"标题\" \"内容\" --pin --agent scout --importance 0.8");
        let a = parse_args(&t);
        assert_eq!(a.pos, vec!["add", "标题", "内容"]);
        assert_eq!(a.kv.get("kind").map(String::as_str), Some("fact"));
        assert_eq!(a.kv.get("agent").map(String::as_str), Some("scout"));
        assert_eq!(a.kv.get("importance").map(String::as_str), Some("0.8"));
        assert!(a.bools.contains("pin"));
    }

    #[test]
    fn 参数解析_等号写法() {
        let a = parse_args(&tokenize("--base-url=https://x/v1 --model m"));
        assert_eq!(
            a.kv.get("base-url").map(String::as_str),
            Some("https://x/v1")
        );
        assert_eq!(a.kv.get("model").map(String::as_str), Some("m"));
    }

    #[test]
    fn 宽度_跳过_ansi与全角() {
        let colored = "\x1b[36m你好\x1b[0m";
        assert_eq!(disp_width(colored), 4);
        assert_eq!(fit(colored, 6), format!("{colored}  "));
    }

    #[test]
    fn 宽度_超长截断补省略号() {
        let s = fit("abcdefgh", 4);
        assert_eq!(disp_width(&s), 4);
        assert!(s.starts_with("abc"));
        assert!(s.contains('…'));
    }

    #[test]
    fn 主题解析_中英别名() {
        assert!(matches!(Theme::parse("cyan"), Some(Theme::Cyan)));
        assert!(matches!(Theme::parse("紫芒"), Some(Theme::Violet)));
        assert!(matches!(Theme::parse("ICE"), Some(Theme::Ice)));
        assert!(Theme::parse("rainbow").is_none());
    }

    #[test]
    fn 横幅_各行列宽严格一致() {
        let rows = banner_rows();
        assert_eq!(rows.len(), 7, "字标口径为 7 行");
        let w = disp_width(&rows[0]);
        assert_eq!(w, 62, "EXMACHINA = 9 字母 × 6 列 + 8 个间隔");
        for r in &rows {
            assert_eq!(disp_width(r), w, "行宽不一致：{r}");
        }
    }

    #[test]
    fn 画框_含中文与空串时仍等宽() {
        let p = Panel::new(PathBuf::from("."), PathBuf::from("."), false);
        let w = p.width;
        assert_eq!(disp_width(&p.rule_top("运行状态")), w);
        assert_eq!(disp_width(&p.rule_mid("中文内容与 ascii 混排")), w);
        assert_eq!(disp_width(&p.rule_mid("")), w);
        assert_eq!(disp_width(&p.rule_bottom()), w);
    }

    #[test]
    fn 编辑距离_基本口径() {
        assert_eq!(levenshtein("", "abc"), 3);
        assert_eq!(levenshtein("abc", "abc"), 0);
        assert_eq!(levenshtein("kitten", "sitting"), 3);
    }

    #[test]
    fn 状态栏_按宽度自适应装填() {
        let sep = " │ ";
        let sep_w = disp_width(sep);
        let chips: Vec<String> = vec![
            "TARGET 组 智能连结 · 40体".to_string(),
            "MODEL deepseek-chat".to_string(),
            "SESS e4e9c9e0 · 0轮".to_string(),
            "TOK ≈1900".to_string(),
        ];
        // 窄终端：装不下的靠后芯片被丢弃，且绝不超宽（不换行）
        let narrow = fit_chips(chips.clone(), 48, sep_w);
        assert!(narrow.len() < chips.len(), "窄终端应丢弃部分芯片");
        assert!(disp_width(&narrow.join(sep)) <= 48);
        // 宽终端：全部保留
        let wide = fit_chips(chips.clone(), 100, sep_w);
        assert_eq!(wide.len(), 4);
        assert!(disp_width(&wide.join(sep)) <= 100);
        // 单枚即超宽（组名极长）→ 截断保留而非整体消失
        let one = fit_chips(vec![format!("TARGET {}", "超".repeat(60))], 40, sep_w);
        assert_eq!(one.len(), 1);
        assert!(disp_width(&one[0]) <= 40);
    }

    #[test]
    fn 命令建议_前缀优先再编辑距离() {
        assert_eq!(suggest_command("sess").as_deref(), Some("session"));
        assert_eq!(suggest_command("sessons").as_deref(), Some("sessions"));
        assert_eq!(suggest_command("targt").as_deref(), Some("target"));
        assert_eq!(suggest_command("zzzzzz"), None);
        assert_eq!(suggest_command(""), None);
    }

    #[test]
    fn 面板纠错命令来自统一目录并保留本地兼容命令() {
        let names = panel_command_names();
        assert!(names.iter().any(|name| name == "model"));
        assert!(names.iter().any(|name| name == "config"));
        assert!(names.iter().any(|name| name == "queue"));
        assert!(names.iter().any(|name| name == "steer"));
        assert!(names.iter().any(|name| name == "goal"));
        assert!(names.iter().any(|name| name == "copy"));
        assert_eq!(suggest_command("qeuue").as_deref(), Some("queue"));
    }

    #[test]
    fn 偏好_落盘读回与缺省() {
        let dir = std::env::temp_dir().join(format!("exm-prefs-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // 文件不存在 → 缺省
        let empty = load_prefs(&dir);
        assert_eq!(empty.theme, "");
        assert!(!empty.quiet);
        // 落盘 → 读回
        save_prefs(
            &dir,
            &PanelPrefs {
                theme: "violet".into(),
                quiet: true,
            },
        );
        let back = load_prefs(&dir);
        assert_eq!(back.theme, "violet");
        assert!(back.quiet);
        assert!(matches!(Theme::parse(&back.theme), Some(Theme::Violet)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 会话短id_截断到八位() {
        assert_eq!(short_id("0123456789abcdef"), "01234567");
        assert_eq!(short_id("abc"), "abc");
    }

    #[test]
    fn 路径_去verbatim前缀并按显示宽截断() {
        assert_eq!(
            shorten_path(Path::new(r"\\?\D:\Projects\ExMachina"), 40),
            r"D:\Projects\ExMachina"
        );
        assert_eq!(
            shorten_path(Path::new(r"\\?\UNC\srv\share\x"), 40),
            r"\\srv\share\x"
        );
        let long = Path::new(r"D:\a\b\c\d\e\f\g\h\i\j\k\l\m\n\o\p\q\r\s\t\u\v");
        let out = shorten_path(long, 20);
        assert!(disp_width(&out) <= 20, "截断后不应超宽：{out}");
        assert!(out.starts_with('…'));
        // 中文路径按显示列算宽
        let cjk = Path::new(r"D:\项目\智能连结\工作区");
        assert!(disp_width(&shorten_path(cjk, 12)) <= 12);
    }

    fn help_row_width(row: &HelpRow) -> usize {
        match row {
            HelpRow::Title(t) => disp_width(t),
            HelpRow::Entry { head, tail } => {
                disp_width(head) + tail.as_deref().map(disp_width).unwrap_or(0)
            }
        }
    }

    /// 真实命令表在多种终端宽度下都必须**逐行不超宽**（否则终端硬折行、缩进错位）
    #[test]
    fn 帮助渲染_每行不超面板宽度() {
        // 覆盖真实表格 + 极端宽度：窄终端下折行、宽终端下仍不超宽
        for width in [30usize, 40, 60, 72, 100, 200] {
            for row in render_help(HELP_GROUPS, width) {
                let w = help_row_width(&row);
                assert!(w <= width, "宽度 {width} 下超宽（{w}）：{row:?}");
            }
        }
    }

    /// 命令表必须自洽：命令以 `/` 开头、组标题以 `//` 开头、无重复命令
    #[test]
    fn 帮助表_结构自洽() {
        let mut seen = std::collections::HashSet::new();
        for (title, items) in HELP_GROUPS {
            assert!(title.starts_with("//"), "组标题应以 `//` 开头：{title}");
            assert!(!items.is_empty(), "组 {title} 不应为空");
            for (cmd, desc) in items.iter() {
                assert!(cmd.starts_with('/'), "命令应以 `/` 开头：{cmd}");
                assert!(!desc.trim().is_empty(), "命令 {cmd} 缺少说明");
                assert!(seen.insert(*cmd), "命令重复：{cmd}");
            }
        }
    }

    #[tokio::test]
    async fn spec_export_tui_requires_explicit_export_and_writes_project_file() {
        let root = std::env::temp_dir().join(format!("exm-panel-spec-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let core = std::sync::Arc::new(exm_core::Core::create(root.clone()).unwrap());
        let session = core.create_session("panel spec export").unwrap();
        let mut panel = Panel::new(root.clone(), root.join(".exmachina"), true);
        let mut state = PanelState {
            session_id: session.id.clone(),
            session_title: session.title,
            turns: 0,
        };

        panel
            .dispatch(&core, &mut state, "/spec draft TUI 导出验收")
            .await
            .unwrap();
        assert!(!root.join("docs/tui-spec.md").exists());
        panel
            .dispatch(&core, &mut state, "/spec export docs/tui-spec.md")
            .await
            .unwrap();
        let content = std::fs::read_to_string(root.join("docs/tui-spec.md")).unwrap();
        assert!(content.contains("TUI 导出验收"));
        let _ = std::fs::remove_dir_all(root);
    }

    /// 折行必须按显示列算：中文占 2 列，否则中文行会实际超宽
    #[test]
    fn 折行_按显示宽度且不丢字符() {
        // 10 个汉字 = 20 列，按 10 列折行 → 每行 5 字、共 2 行
        let src = "深层记忆检索固定统计";
        let out = wrap_disp(src, 10);
        assert_eq!(out.len(), 2, "{out:?}");
        for l in &out {
            assert!(disp_width(l) <= 10, "超宽：{l}");
        }
        // 折行不丢字符（忽略空白后与原文一致：空格折行处的空格即断点）
        assert_eq!(out.join("").replace(' ', ""), src.replace(' ', ""));
        // 优先在空格处断，不把英文单词劈开
        assert_eq!(
            wrap_disp("read config schema now", 12),
            vec!["read config".to_string(), "schema now".to_string()]
        );
        // 放不下时也至少产出一行，不能空手而归
        assert_eq!(wrap_disp("abc", 0), vec!["abc".to_string()]);
        // 单行内放得下就原样返回
        assert_eq!(wrap_disp("short", 20), vec!["short".to_string()]);
        // 行尾不留待取值参数：`--key <key>` 这类成对被挪到下一行，不劈开
        let cmd = "/model add <id> --name <名称> --base-url <url> --key <key> --model <模型>";
        let out = wrap_disp(cmd, 60);
        assert!(out.iter().all(|l| disp_width(l) <= 60), "超宽：{out:?}");
        for l in &out[..out.len() - 1] {
            let last = l.rsplit(' ').next().unwrap_or_default();
            assert!(!dangling_value(last), "行尾是被劈开的参数：{l}");
        }
        assert_eq!(out.join("").replace(' ', ""), cmd.replace(' ', ""));
    }

    #[test]
    fn 流列表排序_最后活跃倒序() {
        // /flows 序号寻址一致性：sorted_flows 固定按 last_active_at 倒序
        // （与 REST /channels/flows 同口径）；此处验证排序纯函数行为
        // （ChannelFlow 构造最小字段，经 serde 完整性由 core 侧测试覆盖）
        let mk = |id: &str, at: &str| exm_core::types::ChannelFlow {
            id: id.into(),
            channel_id: "ch".into(),
            chat_key: id.into(),
            display_name: String::new(),
            is_group: false,
            active_session: None,
            last_message: String::new(),
            last_active_at: at.into(),
            created_at: at.into(),
        };
        let mut v = [
            mk("a", "2026-11-06T01:00:00Z"),
            mk("b", "2026-11-06T03:00:00Z"),
            mk("c", "2026-11-05T00:00:00Z"),
        ];
        v.sort_by(|x, y| y.last_active_at.cmp(&x.last_active_at));
        assert_eq!(
            v.iter().map(|f| f.chat_key.clone()).collect::<Vec<_>>(),
            vec!["b", "a", "c"]
        );
    }

    #[test]
    fn 流scope_通道组优先_缺省回落单机() {
        // flow_scope 口径：channels.json 有组 → 组；无 → machina 单机；无档案 → 激活组
        // （此处只验证无 channels.json 环境回落链不死；带通道文件的行为由 e2e 流面覆盖）
        let base = std::env::temp_dir().join(format!("exm-flows-{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        let mut cfg = exm_core::config::ExmConfig::load(&base);
        cfg.data_dir = base.join("data");
        let core = exm_core::Core::with_config(cfg).expect("Core 创建失败");
        let scope = flow_scope(&core, "no-such-channel");
        assert!(
            !scope.is_empty(),
            "回落链必须给出非空 scope（实际 {scope}）"
        );
        assert!(
            scope.starts_with("single:") || !scope.contains(':'),
            "回落应为单智能体 scope 或组 id（实际 {scope}）"
        );
    }

    /// capability-parity 6.5/6.6：面板的版本命令不是「只写在帮助表里」——走真实
    /// `Panel::dispatch`，对同一个 Core 断言版本链事实。与 CLI/REST 共用同一函数与同一
    /// 版本事实源（技能版本链 / memory/history），没有第二套存储。
    ///
    /// 期望值独立推导：回滚与撤销遗忘都以旧内容**追加新版本行**，版本号单调递增。
    #[tokio::test]
    async fn 面板版本命令_走分发改同一版本链() {
        let home = std::env::temp_dir().join(format!("exm-panel-ver-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&home).unwrap();
        // 断言都在内层函数里返回 Result：失败时外层仍清理临时目录
        let out = panel_version_chain(&home).await;
        let _ = std::fs::remove_dir_all(&home);
        out.unwrap();
    }

    async fn panel_version_chain(home: &std::path::Path) -> Result<(), String> {
        use exm_core::memory::MemoryKind;
        fn e(what: &str, err: impl std::fmt::Display) -> String {
            format!("{what}：{err}")
        }
        fn need(what: &str, ok: bool, detail: impl std::fmt::Display) -> Result<(), String> {
            if ok {
                Ok(())
            } else {
                Err(format!("{what}：{detail}"))
            }
        }
        let core = std::sync::Arc::new(exm_core::Core::create(home).map_err(|x| e("建 Core", x))?);
        let session = core
            .create_session("panel versions")
            .map_err(|x| e("建会话", x))?;
        let mut panel = Panel::new(home.to_path_buf(), home.join(".exmachina"), true);
        let mut state = PanelState {
            session_id: session.id.clone(),
            session_title: session.title,
            turns: 0,
        };
        // 走真实分发：行内容由调用处给出，错误原样上抛（带命令行便于定位）
        macro_rules! run {
            ($line:expr) => {{
                let line: &str = &$line;
                panel
                    .dispatch(&core, &mut state, line)
                    .await
                    .map_err(|x| e(line, x))?
            }};
        }
        let hist_of = |id: &str| -> Result<Vec<(u64, String, String)>, String> {
            core.memory_history(id)
                .map(|h| {
                    h.iter()
                        .map(|r| (r.version, r.change.clone(), r.body.clone()))
                        .collect()
                })
                .map_err(|x| e("记忆版本史", x))
        };

        // ---------- 记忆 ----------
        // 版本链期望（独立推导）：v1 created → 面板回滚到 v1 = v2 rolled_back
        // → 面板遗忘 = v3 forgotten（遗忘本身是版本链上一行，可审计）
        // → 面板撤销遗忘 = v4 restored。每一步都追加新版本行，版本号单调递增。
        let entry = core
            .memory_remember(exm_core::memory::MemoryDraft::new(
                MemoryKind::Fact,
                "面板版本验证",
                "初版正文",
            ))
            .map_err(|x| e("写入记忆", x))?;
        let id = entry.id.clone();
        need("新写入应为 v1", entry.version == 1, entry.version)?;

        run!(format!("/memory versions {id}"));
        run!(format!("/memory rollback {id} --to 1"));
        let hist = hist_of(&id)?;
        need(
            "面板回滚应在同一版本链追加 rolled_back 行",
            hist.iter().map(|r| (r.0, r.1.as_str())).collect::<Vec<_>>()
                == vec![(1, "created"), (2, "rolled_back")],
            format!("{hist:?}"),
        )?;
        need(
            "回滚行内容应是被回滚到的版本",
            hist[1].2 == "初版正文",
            &hist[1].2,
        )?;

        // 越界回滚：dispatch 把错误交给调用者，且旧版本不变
        let oob = panel
            .dispatch(&core, &mut state, &format!("/memory rollback {id} --to 99"))
            .await;
        need("越界回滚应返回错误（实际 Ok）", oob.is_err(), "")?;
        need(
            "越界回滚不得改变版本链",
            hist_of(&id)?.len() == hist.len(),
            "",
        )?;

        run!(format!("/memory forget {id}"));
        need(
            "遗忘后不应在常规列表",
            core.memory_list(None, 50)
                .map_err(|x| e("记忆列表", x))?
                .iter()
                .all(|x| x.id != id),
            "",
        )?;
        run!(format!("/memory restore {id}"));
        let hist = hist_of(&id)?;
        need(
            "遗忘占一个版本号，撤销遗忘再追加一行",
            hist.iter().map(|r| (r.0, r.1.as_str())).collect::<Vec<_>>()
                == vec![
                    (1, "created"),
                    (2, "rolled_back"),
                    (3, "forgotten"),
                    (4, "restored"),
                ],
            format!("{hist:?}"),
        )?;
        need(
            "撤销遗忘后应回到常规列表",
            core.memory_list(None, 50)
                .map_err(|x| e("记忆列表", x))?
                .iter()
                .any(|x| x.id == id),
            "",
        )?;

        // ---------- 技能 ----------
        // v1 → 更新 v2 → 面板回滚到 v1 = v3（内容回到 v1）
        let mut skill = exm_core::types::SkillDef {
            id: "panel-ver".into(),
            name: "面板版本验证".into(),
            description: String::new(),
            triggers: vec!["面板专属触发词QZ".into()],
            instructions: "初版指令".into(),
            agents: vec![],
            created_at: String::new(),
            use_count: 0,
            success_count: 0,
            last_used_at: String::new(),
            source: String::new(),
            version: 0,
            updated_at: String::new(),
        };
        let v1 = core
            .skill_write_versioned(&skill)
            .map_err(|x| e("写技能 v1", x))?;
        need("技能首写应为 v1", v1.version == 1, v1.version)?;
        skill.instructions = "第二版指令".into();
        let v2 = core
            .skill_write_versioned(&skill)
            .map_err(|x| e("写技能 v2", x))?;
        need("技能二次写应为 v2", v2.version == 2, v2.version)?;

        run!("/skills versions panel-ver");
        run!("/skills rollback panel-ver --to 1");
        let hist = core
            .skill_history_detailed("panel-ver")
            .map_err(|x| e("技能版本史", x))?;
        let vers: Vec<u64> = hist.iter().map(|r| r.version).collect();
        need(
            "面板回滚应在技能版本链追加 v3",
            vers == vec![1, 2, 3],
            format!("{vers:?}"),
        )?;
        need(
            "v3 内容应为被回滚到的 v1",
            hist[2].instructions == "初版指令",
            &hist[2].instructions,
        )?;
        Ok(())
    }
}
