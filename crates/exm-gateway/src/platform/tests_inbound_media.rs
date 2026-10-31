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
        let dir = std::env::temp_dir().join(format!("exm-inb-f-{}", uuid::Uuid::new_v4()));
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
}
