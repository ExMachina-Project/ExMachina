//! 会话来源解析（session-management）：title 约定 → 结构化来源。
//!
//! 通道流会话的 title 一直是唯一寻址事实源（`channel:{channelId}:{chatKey}[:label]`，
//! channel_runtime / channels.rs / e2e 多处依赖）。本模块把该约定集中解析为结构化
//! 来源（origin / chat_key / label），管理页分组视图与闲置归档共用同一口径。
//! 存量会话读时派生，零迁移、零写入。

/// 会话来源（解析结果）
#[derive(Debug, Clone, PartialEq)]
pub enum SessionOrigin {
    /// 控制台会话（WebUI / CLI 创建，title 即展示名）
    Console,
    /// 通道流会话：channelId + chatKey + 会话标签（/new 后缀，缺省「默认会话」语义）
    Channel {
        channel_id: String,
        chat_key: String,
        label: Option<String>,
    },
}

impl SessionOrigin {
    /// 会话列表附加字段口径（API `origin` 字段值）
    pub fn key(&self) -> String {
        match self {
            SessionOrigin::Console => "console".into(),
            SessionOrigin::Channel { channel_id, chat_key, .. } => {
                format!("channel:{channel_id}:{chat_key}")
            }
        }
    }
}

/// 解析会话来源。title 形态：
/// - `channel:{channelId}:{chatKey}` —— 流首个会话（裸标题）
/// - `channel:{channelId}:{chatKey}:{label}` —— /new 或指定标签开的会话
/// - 其它 —— 控制台会话
///
/// chatKey 内部不含 `:`（各适配器以平台 id / 群号 / 固定键生成），
/// 因此按前 3 段切分是安全的；label 允许含 `:`（取第 4 段起全部）。
pub fn parse_session_origin(title: &str) -> SessionOrigin {
    let Some(rest) = title.strip_prefix("channel:") else {
        return SessionOrigin::Console;
    };
    // 至少需要 channelId 与 chatKey 两段；切分后余下（若有）整体作为 label
    let mut parts = rest.splitn(3, ':');
    let (Some(channel_id), Some(chat_key)) = (parts.next(), parts.next()) else {
        return SessionOrigin::Console;
    };
    if channel_id.is_empty() || chat_key.is_empty() {
        return SessionOrigin::Console;
    }
    SessionOrigin::Channel {
        channel_id: channel_id.to_string(),
        chat_key: chat_key.to_string(),
        label: parts.next().map(str::to_string),
    }
}

/// 会话归档范围（批量归档端点口径）
#[derive(Debug, Clone, PartialEq)]
pub enum ArchiveScope {
    /// 单流：channelId + chatKey
    Flow { channel_id: String, chat_key: String },
    /// 整通道：channelId
    Channel { channel_id: String },
}

impl ArchiveScope {
    /// 会话是否落在归档范围内（title 前缀判定）
    pub fn matches(&self, origin: &SessionOrigin) -> bool {
        match (self, origin) {
            (
                ArchiveScope::Flow { channel_id, chat_key },
                SessionOrigin::Channel { channel_id: c, chat_key: k, .. },
            ) => channel_id == c && chat_key == k,
            (
                ArchiveScope::Channel { channel_id },
                SessionOrigin::Channel { channel_id: c, .. },
            ) => channel_id == c,
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 解析_裸流标题() {
        assert_eq!(
            parse_session_origin("channel:c-tg:123456"),
            SessionOrigin::Channel {
                channel_id: "c-tg".into(),
                chat_key: "123456".into(),
                label: None
            }
        );
    }

    #[test]
    fn 解析_带标签标题() {
        assert_eq!(
            parse_session_origin("channel:c-qq:group_100:20251101"),
            SessionOrigin::Channel {
                channel_id: "c-qq".into(),
                chat_key: "group_100".into(),
                label: Some("20251101".into())
            }
        );
    }

    #[test]
    fn 解析_标签可含冒号() {
        assert_eq!(
            parse_session_origin("channel:c-x:k1:lab:el"),
            SessionOrigin::Channel {
                channel_id: "c-x".into(),
                chat_key: "k1".into(),
                label: Some("lab:el".into())
            }
        );
    }

    #[test]
    fn 解析_控制台与残缺形态() {
        assert_eq!(parse_session_origin("随便聊"), SessionOrigin::Console);
        assert_eq!(parse_session_origin(""), SessionOrigin::Console);
        // 前缀命中但段缺失 / 空段 = 控制台（防御：脏数据不进通道组）
        assert_eq!(parse_session_origin("channel:"), SessionOrigin::Console);
        assert_eq!(parse_session_origin("channel:c-tg:"), SessionOrigin::Console);
        assert_eq!(parse_session_origin("channel::k"), SessionOrigin::Console);
    }

    #[test]
    fn origin键_口径() {
        assert_eq!(parse_session_origin("channel:c-tg:1").key(), "channel:c-tg:1");
        assert_eq!(SessionOrigin::Console.key(), "console");
    }

    #[test]
    fn 归档范围_匹配() {
        let o = parse_session_origin("channel:c-tg:grp:lab");
        assert!(ArchiveScope::Flow { channel_id: "c-tg".into(), chat_key: "grp".into() }.matches(&o));
        assert!(!ArchiveScope::Flow { channel_id: "c-tg".into(), chat_key: "other".into() }.matches(&o));
        assert!(ArchiveScope::Channel { channel_id: "c-tg".into() }.matches(&o));
        assert!(!ArchiveScope::Channel { channel_id: "c-qq".into() }.matches(&o));
        assert!(!ArchiveScope::Channel { channel_id: "c-tg".into() }.matches(&SessionOrigin::Console));
    }
}
