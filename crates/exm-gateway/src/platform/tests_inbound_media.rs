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
    /// 占位消息应带**本条消息自带**的媒体元数据（InboundCtx.media）——旧缺陷：
    /// 图片文件落盘、占位文本落库，但 attachments 永远为空，聊天流看不到图。
    /// 更旧的缺陷：回填曾用全窗快照，窗口内每条新旁听消息（含纯文本）都复读
    /// 全部近期图片——本测试锚定「只挂本条的」语义。
    /// 窗口条目不被回填取走（@ 轮次 take 注入模型的链路保持原语义）。
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
            mention_reply: None,
            capability: "computer".into(),
            memory_isolation: false,
            created_at: exm_core::types::now_iso(),
        };
        // 旁听媒体已入窗（napcat 闸门前的预下载：既入窗又随 ctx.media 携带）
        let img_dir = core.config().workspace_root.join("inbox").join("napcat-t");
        std::fs::create_dir_all(&img_dir).unwrap();
        let img = img_dir.join("pic.png");
        std::fs::write(&img, b"png-bytes").unwrap();
        let img2 = img_dir.join("old.png");
        std::fs::write(&img2, b"png-bytes-2").unwrap();
        // 窗口里已有一张**旧图**（此前消息留下的）
        crate::ambient_media::push(
            &ch.id,
            "996979629",
            vec![(
                "image".into(),
                img2.display().to_string(),
                "old.png".into(),
            )],
        );
        // 旁听占位消息（未 @ 的纯图 → 占位文本）；本条消息自带的媒体是 pic.png
        let ctx = InboundCtx {
            text: "（用户发来附件：图片 x1，附件细节见消息注入）",
            external_id: "2515681159",
            display_name: "群友",
            is_group: true,
            mentioned: false,
            chat_key: "996979629",
            flow_name: Some("测试群"),
            media: &[(
                "image".into(),
                img.display().to_string(),
                "pic.png".into(),
            )],
        };
        super::super::record_ambient(&core, &ch, &ctx).await;
        // 占位消息应带附件元数据（工作区相对路径），且**只有本条的 pic.png**
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
            "只挂本条消息自带的媒体: {:?}",
            m.attachments
        );
        assert!(!m.attachments[0].path.starts_with('/'), "应为工作区相对路径");
        assert!(m.attachments[0].path.contains("pic.png"));
        assert!(
            !m.attachments.iter().any(|a| a.path.contains("old.png")),
            "窗口里的旧图不该复读到本条消息"
        );
        // 纯文本旁听消息不带附件（复读回归锚点）
        let plain_ctx = InboundCtx {
            text: "群里闲聊的一句话",
            external_id: "2515681159",
            display_name: "群友",
            is_group: true,
            mentioned: false,
            chat_key: "996979629",
            flow_name: Some("测试群"),
            media: &[],
        };
        super::super::record_ambient(&core, &ch, &plain_ctx).await;
        let msgs = core.store.list_messages(&sid, 10).unwrap();
        let m = msgs
            .iter()
            .rev()
            .find(|m| m.statements.iter().any(|s| s.text.contains("闲聊")))
            .expect("纯文本旁听消息应已落库");
        assert!(
            m.attachments.is_empty(),
            "纯文本旁听消息不该挂窗口里的图: {:?}",
            m.attachments
        );
        // 窗口不被回填取走——@ 轮次仍可 take 注入模型（old + pic 两条都在，
        // pic 与 ctx.media 同源：真实链路 napcat 预下载既入窗又携带）
        crate::ambient_media::push(
            &ch.id,
            "996979629",
            vec![(
                "image".into(),
                img.display().to_string(),
                "pic.png".into(),
            )],
        );
        let remain = crate::ambient_media::take(&ch.id, "996979629");
        assert_eq!(remain.len(), 2, "窗口条目应保留给 @ 轮次");
    }
}

/// 指纹去重（inbound-media-dedup 6.1）
mod dedup_tests {
    use super::*;

    fn test_core() -> exm_core::Core {
        let dir = std::env::temp_dir().join(format!("exm-ded-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("agents")).unwrap();
        exm_core::Core::create(&dir).expect("创建 Core 失败")
    }

    fn ch() -> crate::platform::Channel {
        Channel {
            id: "qq-test".into(),
            kind: "qqbot".into(),
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
            mention_reply: None,
            capability: "computer".into(),
            memory_isolation: false,
            created_at: exm_core::types::now_iso(),
        }
    }

    /// 同轮重复附件：同字节两条 → 只落盘 1 份
    #[tokio::test]
    async fn 同轮重复_只留一份() {
        let core = test_core();
        let ch = ch();
        let png = vec![0x89, b'P', b'N', b'G', 1, 2, 3];
        let p1 = save_inbound_media(&core, &ch, "a.png", png.clone()).await.unwrap();
        let p2 = save_inbound_media(&core, &ch, "b.png", png.clone()).await.unwrap();
        assert_eq!(p1, p2, "同字节不同名 → 同指纹路径");
        // 同轮收集层语义（qqbot 侧 seen_fp 判重）也在指纹上：
        let fp = crate::platform::media_fingerprint(&png);
        assert!(p1.contains(&fp), "路径含指纹: {p1}");
        // 目录里只有一个文件
        let dir = std::path::Path::new(&p1).parent().unwrap();
        let n = std::fs::read_dir(dir).unwrap().count();
        assert_eq!(n, 1, "磁盘只留一份");
    }

    /// 跨消息重推复用：先後两轮（不同展示名）→ 复用同一路径，磁盘一份
    #[tokio::test]
    async fn 跨消息重推_复用路径() {
        let core = test_core();
        let ch = ch();
        let jpg = vec![0xFF, 0xD8, 0xFF, 0xE0, 7, 8, 9];
        let first = save_inbound_media(&core, &ch, "photo-1234.jpg", jpg.clone()).await.unwrap();
        // 平台重推改名：文件名字段变化，字节相同
        let second = save_inbound_media(&core, &ch, "photo-9999.jpg", jpg.clone()).await.unwrap();
        assert_eq!(first, second, "重推复用已落盘路径");
        let bytes = tokio::fs::read(&first).await.unwrap();
        assert_eq!(bytes, jpg, "复用文件内容一致");
        let dir = std::path::Path::new(&first).parent().unwrap();
        assert_eq!(std::fs::read_dir(dir).unwrap().count(), 1, "磁盘一份");
    }

    /// 指纹不依赖文件名；不同字节不同指纹（不误伤正常多图）
    #[tokio::test]
    async fn 不同内容_各自落盘() {
        let core = test_core();
        let ch = ch();
        let a = save_inbound_media(&core, &ch, "1.png", vec![1, 2, 3]).await.unwrap();
        let b = save_inbound_media(&core, &ch, "2.png", vec![4, 5, 6]).await.unwrap();
        assert_ne!(a, b, "不同字节不同指纹路径");
    }

    /// 纯媒体占位在去重后清单上计算：去重剩 1 条仍按 image 生成占位
    #[test]
    fn 纯媒体占位_去重后不误判() {
        // collect 侧（qqbot/napcat 同口径）：kinds 来自去重后的清单
        let deduped: Vec<(String, Vec<u8>)> = vec![("image".into(), vec![1, 2, 3])];
        let kinds: Vec<&str> = deduped.iter().map(|(k, _)| k.as_str()).collect();
        let placeholder = crate::channel_util::media_placeholder(&kinds);
        assert!(!placeholder.trim().is_empty(), "占位正文正常生成: {placeholder}");
    }
}
