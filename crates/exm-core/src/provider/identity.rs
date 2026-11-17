//! Session identifiers stay scoped to the caller; explicit header overrides also shape metadata.
use super::OpenAiCompatibleProvider;
use crate::config::{ClientIdentityPreset as Preset, ClientRequestMode};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use std::hash::{Hash, Hasher};
use std::sync::OnceLock;

impl OpenAiCompatibleProvider {
    fn session_identity(&self) -> String {
        let Some(caller) = crate::model_routing::caller().filter(|c| !c.session_id.is_empty())
        else {
            return self.identity_session.clone();
        };
        if let Ok(id) = uuid::Uuid::parse_str(&caller.session_id) {
            return id.to_string();
        }
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        caller.session_id.hash(&mut hash);
        let first = hash.finish();
        "client-identity-session".hash(&mut hash);
        let mut bytes = (((first as u128) << 64) | hash.finish() as u128).to_be_bytes();
        bytes[6] = (bytes[6] & 0x0f) | 0x40;
        bytes[8] = (bytes[8] & 0x3f) | 0x80;
        uuid::Uuid::from_bytes(bytes).to_string()
    }

    pub fn request_identity_headers(&self) -> HeaderMap {
        let mut headers = self.identity_headers.clone();
        let session = self.session_identity();
        let names: &[&'static str] = match self.identity.preset {
            Preset::ClaudeCode => &["x-claude-code-session-id"],
            Preset::Codex => &["session_id"],
            Preset::Opencode => &[
                "x-opencode-session-id",
                "x-opencode-session",
                "x-session-id",
                "x-session-affinity",
            ],
            Preset::Cline => &["x-task-id"],
            _ => &[],
        };
        for &name in names {
            if !headers.contains_key(name) {
                headers.insert(
                    HeaderName::from_static(name),
                    HeaderValue::from_str(&session).expect("generated UUID"),
                );
            }
        }
        headers
    }

    pub(super) fn claude_metadata_enabled(&self) -> bool {
        self.identity.preset == Preset::ClaudeCode
            && self.identity.request_mode == ClientRequestMode::Native
            && self.api_format == "anthropic"
    }

    pub(super) fn native_identity_compatible(&self) -> bool {
        match self.identity.preset {
            Preset::ClaudeCode => self.api_format == "anthropic",
            Preset::Codex => self.api_format == "responses",
            _ => true,
        }
    }

    pub(super) fn claude_metadata(&self) -> serde_json::Value {
        static DEVICE: OnceLock<String> = OnceLock::new();
        let device = DEVICE.get_or_init(|| {
            format!(
                "{}{}",
                uuid::Uuid::new_v4().simple(),
                uuid::Uuid::new_v4().simple()
            )
        });
        let headers = self.request_identity_headers();
        let session = headers["x-claude-code-session-id"]
            .to_str()
            .unwrap_or_default();
        serde_json::json!({"user_id": serde_json::json!({"device_id": device, "account_uuid": "", "session_id": session}).to_string()})
    }
}
