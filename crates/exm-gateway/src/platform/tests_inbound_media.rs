//! gateway/platform/tests_inbound_media —— 自 platform.rs 按资源域拆出（纯搬移，零行为变更）
#[cfg(test)]
use super::*;

mod inbound_media_tests {
    use super::*;

    fn test_core() -> exm_core::Core {
        let dir = std::env::temp_dir().join(format!("exm-inb-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("agents")).unwrap();
        exm_core::Core::create(&dir).expect("创建 Core 失败")
    }

    /// 入站注入：图片进多模态暂存（data URL），文件注入路径说明
    #[tokio::test]
    async fn 入站媒体_注入与暂存() {
        let core = test_core();
        let png: Vec<u8> = vec![0x89, b'P', b'N', b'G', 1, 2, 3, 4];
        // 媒体落工作区内（save_inbound_media 实际行为）：附件元数据相对化才有意义
        let dir = core.config().workspace_root.join("media");
        std::fs::create_dir_all(&dir).unwrap();
        let img = dir.join("shot.png");
        std::fs::write(&img, &png).unwrap();
        let doc = dir.join("report.txt");
        std::fs::write(&doc, b"hello").unwrap();

        let saved = vec![
            ("image".into(), img.display().to_string(), "shot.png".into()),
            (
                "file".into(),
                doc.display().to_string(),
                "report.txt".into(),
            ),
        ];
        let note = stage_inbound_media(&core, "ses-inb", &saved);
        assert!(note.contains("图片附件 shot.png 已注入"), "{note}");
        assert!(note.contains("文件附件 report.txt 已保存"), "{note}");
        // 图片确实进入多模态暂存（session 维度）
        let staged = exm_core::image_stash::take("ses-inb");
        assert_eq!(staged.len(), 1, "应有一条暂存图片");
        assert!(staged[0].starts_with("data:image/png;base64,"));
        // 附件元数据已登记进入站暂存（im-inbound-attach：随 user 消息落库，
        // WebUI 聊天流据此渲染缩略图）；路径应为工作区相对路径
        let rels = exm_core::inbox_stash::take("ses-inb");
        assert_eq!(rels.len(), 2, "图片+文件都应登记");
        assert_eq!(rels[0].kind, "image");
        assert!(!rels[0].path.starts_with('/'), "应为工作区相对路径：{}", rels[0].path);
        assert!(rels[0].path.contains("shot.png"), "{}", rels[0].path);
        assert_eq!(rels[1].name, "report.txt");
    }

    /// 失败回退：文件缺失 → 注入失败说明而非 panic/丢弃
    #[tokio::test]
    async fn 入站媒体_读取失败回退() {
        let core = test_core();
        let saved = vec![("image".into(), "/nonexistent/x.png".into(), "x.png".into())];
        let note = stage_inbound_media(&core, "ses-fail", &saved);
        assert!(note.contains("读取失败"), "{note}");
        assert!(
            exm_core::image_stash::take("ses-fail").is_empty(),
            "失败不应产生暂存"
        );
    }

    /// 旁听媒体回填（ambient-media-attach 回归）：群聊未 @ 的图片走旁听路径，
    /// 占位消息落库后应从旁听窗口快照拿到附件展示元数据——旧缺陷：图片文件
    /// 落盘、占位文本落库，但 attachments 永远为空，聊天流看不到图。
    /// 窗口条目不被快照取走（@ 轮次 take 注入模型的链路保持原语义）。
    #[tokio::test]
    async fn 旁听媒体_占位消息回填附件() {
        let core = test_core();
        let ch = Channel {
            id: "napcat-t".into(),
            kind: "napcat".into(),
            enabled: true,
            group: None,
            allowed_chats: vec![],
            admins: vec![],
            account: None,
            secret: None,
            reply_webhook: None,
            token: None,
            config: Default::default(),
            group_gate: None,
            proactive_reply: None,
            proactive_rate: None,
            capability: "computer".into(),
            memory_isolation: false,
            created_at: exm_core::types::now_iso(),
        };
        // 旁听媒体已入窗（napcat 闸门前的预下载）
        let img_dir = core.config().workspace_root.join("inbox").join("napcat-t");
        std::fs::create_dir_all(&img_dir).unwrap();
        let img = img_dir.join("pic.png");
        std::fs::write(&img, b"png-bytes").unwrap();
        crate::ambient_media::push(
            &ch.id,
            "996979629",
            vec![(
                "image".into(),
                img.display().to_string(),
                "pic.png".into(),
            )],
        );
        // 旁听占位消息（未 @ 的纯图 → 占位文本）
        let ctx = InboundCtx {
            text: "（用户发来附件：图片 x1，附件细节见消息注入）",
            external_id: "2515681159",
            display_name: "群友",
            is_group: true,
            mentioned: false,
            chat_key: "996979629",
            flow_name: Some("测试群"),
        };
        super::super::record_ambient(&core, &ch, &ctx).await;
        // 占位消息应带附件元数据（工作区相对路径）
        let sid = core
            .flow_active_session(&ch.id, "996979629")
            .expect("旁听应已建流会话");
        let msgs = core.store.list_messages(&sid, 10).unwrap();
        let m = msgs
            .iter()
            .rev()
            .find(|m| m.statements.iter().any(|s| s.text.contains("用户发来附件")))
            .expect("占位消息应已落库");
        assert_eq!(
            m.attachments.len(),
            1,
            "旁听图片应回填附件: {:?}",
            m.attachments
        );
        assert!(!m.attachments[0].path.starts_with('/'), "应为工作区相对路径");
        assert!(m.attachments[0].path.contains("pic.png"));
        // 窗口不被快照取走——@ 轮次仍可 take 注入模型
        let remain = crate::ambient_media::take(&ch.id, "996979629");
        assert_eq!(remain.len(), 1, "窗口条目应保留给 @ 轮次");
    }
}
