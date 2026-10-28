//! RSS/Atom 解析（quick-xml）：极简事件流解析，取标题/链接/发布时间/摘要。
//!
//! 口径（spec rss-reader「解析」）：
//! - RSS 2.0：`<item>` 的 `<title>/<link>/<pubDate>/<description>`
//! - Atom：`<entry>` 的 `<title>/<link href>/`<updated>`/`<summary>`
//! - 缺 link 的条目跳过（去重键无处安放）；其余字段缺省降级
//! - 解析失败整源报错（调用方逐源失败不阻塞）

use serde::{Deserialize, Serialize};

/// 单条 feed 条目（解析产物的规整形状）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FeedItem {
    pub title: String,
    pub link: String,
    /// 原文发布时间（原样字符串；缺省空）
    pub published: String,
    /// 摘要（HTML 剥标签后纯文本；截 500 字）
    pub summary: String,
}

/// 解析 RSS2/Atom 文本 → 条目列表（按文档序；调用方自行排序/截断）。
/// 无任何条目（含解析彻底失败）返回 Err；有条目但个别缺字段按降级口径跳过。
pub fn parse_feed(text: &str) -> anyhow::Result<Vec<FeedItem>> {
    let mut reader = quick_xml::Reader::from_str(text);
    reader.config_mut().trim_text(true);
    let mut items = Vec::new();
    // 解析状态机：item/entry 内收集字段
    #[derive(Default)]
    struct Cur {
        in_item: bool,
        title: String,
        link: String,
        published: String,
        summary: String,
        cur_text: String,
        cur_field: &'static str,
    }
    let mut cur = Cur::default();
    let mut depth_in_item = 0usize;
    let mut buf = Vec::new();
    loop {
        use quick_xml::events::Event;
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let name = e.name();
                let tag = String::from_utf8_lossy(name.as_ref()).to_string();
                let local = tag.rsplit(':').next().unwrap_or(&tag).to_string();
                if cur.in_item {
                    depth_in_item += 1;
                    cur.cur_field = match local.as_str() {
                        "title" => "title",
                        "link" => {
                            // Atom：<link href="..."/>；RSS：<link>文本</link>
                            for a in e.attributes().flatten() {
                                if a.key.as_ref() == b"href" {
                                    cur.link =
                                        String::from_utf8_lossy(&a.value).to_string();
                                }
                            }
                            "link"
                        }
                        "pubDate" | "published" | "updated" | "date" => "published",
                        "description" | "summary" | "content" | "encoded" => "summary",
                        _ => "",
                    };
                    cur.cur_text.clear();
                } else if local == "item" || local == "entry" {
                    cur = Cur { in_item: true, ..Default::default() };
                    depth_in_item = 0;
                }
            }
            Ok(Event::Empty(e)) => {
                // 自闭合（Atom link 常见形态）：直接赋值（无 End 事件兜底）
                if cur.in_item {
                    let tag = String::from_utf8_lossy(e.name().as_ref()).to_string();
                    let local = tag.rsplit(':').next().unwrap_or(&tag).to_string();
                    if local == "link" {
                        for a in e.attributes().flatten() {
                            if a.key.as_ref() == b"href" {
                                cur.link = String::from_utf8_lossy(&a.value).to_string();
                            }
                        }
                    }
                }
            }
            Ok(Event::Text(t)) => {
                if cur.in_item && !cur.cur_field.is_empty() {
                    cur.cur_text.push_str(&t.unescape()?);
                }
            }
            Ok(Event::CData(t)) => {
                if cur.in_item && !cur.cur_field.is_empty() {
                    cur.cur_text.push_str(&String::from_utf8_lossy(t.as_ref()));
                }
            }
            Ok(Event::End(e)) => {
                let tag = String::from_utf8_lossy(e.name().as_ref()).to_string();
                let local = tag.rsplit(':').next().unwrap_or(&tag).to_string();
                if cur.in_item {
                    match cur.cur_field {
                        "title" => cur.title = cur.cur_text.clone(),
                        "link" => {
                            // 文本形态（RSS）优先；属性形态已在 Start/Empty 时直赋
                            if !cur.cur_text.trim().is_empty() {
                                cur.link = cur.cur_text.clone();
                            }
                        }
                        "published" => cur.published = cur.cur_text.clone(),
                        "summary" => cur.summary = cur.cur_text.clone(),
                        _ => {}
                    }
                    cur.cur_field = "";
                    cur.cur_text.clear();
                    if depth_in_item == 0 && (local == "item" || local == "entry") {
                        // 条目收束：缺 link 跳过；title 缺省用 link；summary 剥标签截断
                        let link = cur.link.trim().to_string();
                        if !link.is_empty() {
                            items.push(FeedItem {
                                title: if cur.title.trim().is_empty() {
                                    link.clone()
                                } else {
                                    cur.title.trim().to_string()
                                },
                                link,
                                published: cur.published.trim().to_string(),
                                summary: strip_html(&cur.summary),
                            });
                        }
                        cur = Cur::default();
                    }
                    depth_in_item = depth_in_item.saturating_sub(1);
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => anyhow::bail!("feed 解析失败：{e}"),
            _ => {}
        }
        buf.clear();
    }
    if items.is_empty() {
        anyhow::bail!("feed 中未找到任何条目（item/entry）");
    }
    Ok(items)
}

/// HTML 粗剥：去标签 + 常见实体 + 空白折叠；截 500 字
pub fn strip_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    for (ent, ch) in [("&amp;", "&"), ("&lt;", "<"), ("&gt;", ">"), ("&quot;", "\""), ("&#39;", "'"), ("&nbsp;", " ")] {
        out = out.replace(ent, ch);
    }
    // 空白折叠
    let mut collapsed = String::with_capacity(out.len());
    let mut prev_space = false;
    for c in out.chars() {
        if c.is_whitespace() {
            if !prev_space {
                collapsed.push(' ');
            }
            prev_space = true;
        } else {
            collapsed.push(c);
            prev_space = false;
        }
    }
    collapsed.trim().chars().take(500).collect()
}

// ---------------------------------------------------------------- 单测

#[cfg(test)]
mod tests {
    use super::*;

    const RSS: &str = r#"<?xml version="1.0"?>
<rss version="2.0"><channel>
<title>示例源</title>
<item>
  <title>第一条</title>
  <link>https://example.com/1</link>
  <pubDate>Thu, 29 Oct 2026 08:00:00 GMT</pubDate>
  <description>&lt;p&gt;正文 &amp;amp; 摘要&lt;/p&gt;</description>
</item>
<item>
  <title>缺链接条目</title>
  <description>应被跳过</description>
</item>
<item>
  <link>https://example.com/2</link>
  <description>标题缺省用链接</description>
</item>
</channel></rss>"#;

    const ATOM: &str = r#"<?xml version="1.0"?>
<feed xmlns="http://www.w3.org/2005/Atom">
<title>Atom 源</title>
<entry>
  <title type="html">Atom 条目</title>
  <link href="https://example.com/a1"/>
  <updated>2026-10-29T08:00:00Z</updated>
  <summary>Atom 摘要</summary>
</entry>
</feed>"#;

    #[test]
    fn 解析_rss2() {
        let items = parse_feed(RSS).unwrap();
        assert_eq!(items.len(), 2, "缺 link 条目应跳过：{:?}", items);
        assert_eq!(items[0].title, "第一条");
        assert_eq!(items[0].link, "https://example.com/1");
        assert!(items[0].published.contains("Oct"), "发布时间：{}", items[0].published);
        assert_eq!(items[0].summary, "正文 &amp; 摘要".replace("&amp;", "&"), "实体应还原：{}", items[0].summary);
        assert_eq!(items[1].title, "https://example.com/2", "缺标题降级用链接");
    }

    #[test]
    fn 解析_atom() {
        let items = parse_feed(ATOM).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "Atom 条目");
        assert_eq!(items[0].link, "https://example.com/a1");
        assert_eq!(items[0].summary, "Atom 摘要");
    }

    #[test]
    fn 解析_无条目报错() {
        assert!(parse_feed("<rss><channel><title>x</title></channel></rss>").is_err());
        assert!(parse_feed("not xml at all").is_err());
    }

    #[test]
    fn 剥标签与实体() {
        assert_eq!(strip_html("<p>Hello&nbsp;<b>World</b></p>"), "Hello World");
        assert_eq!(strip_html("a&lt;b&gt;c"), "a<b>c");
        let long = format!("<p>{}</p>", "x".repeat(1000));
        assert!(strip_html(&long).chars().count() <= 500, "应截 500 字");
    }
}
