//! core/identity.rs —— Core 通道身份与流绑定域（codebase-refactor 拆分）：
//! 用户身份（identity CRUD）、配对码（签发/核销/过期清理）、聊天流（flow）与
//! 会话绑定、流未读计数、一次性标记。纯搬移：方法签名与实现零改动。

use super::super::Core;
use crate::types::*;

impl Core {
    pub fn identity_of(&self, channel: &str, external_id: &str) -> Option<UserIdentity> {
        self.store.get_identity(channel, external_id).ok().flatten()
    }
    pub fn identity_of_id(&self, id: &str) -> Option<UserIdentity> {
        self.store.get_identity_by_id(id).ok().flatten()
    }
    pub fn identities(&self) -> Vec<UserIdentity> {
        self.store.list_identities().unwrap_or_default()
    }
    pub fn save_identity(&self, identity: &UserIdentity) -> anyhow::Result<()> {
        self.store.put_identity(identity)
    }
    pub fn drop_identity(&self, id: &str) -> anyhow::Result<()> {
        self.store.delete_identity(id)
    }
    /// 撤销配对码（控制台删除；幂等：不存在也成功）
    pub fn drop_pairing_code(&self, code: &str) -> anyhow::Result<()> {
        self.store.delete_pairing_code(code)
    }
    /// 清理全部过期配对码，返回清理数（周期任务与控制台「一键清理」共用）
    pub fn purge_expired_pairing_codes(&self) -> usize {
        let now = now_ms();
        let mut n = 0usize;
        for c in self.pairing_codes() {
            if now > c.expires_at_ms && self.drop_pairing_code(&c.code).is_ok() {
                n += 1;
            }
        }
        n
    }
    /// 流 id 口径：`{channelId}:{chatKey}`
    pub fn flow_id(channel_id: &str, chat_key: &str) -> String {
        format!("{channel_id}:{chat_key}")
    }
    /// 原子版「查绑定→(缺绑定)建会话→回写绑定」（竞态 P1-3）：
    /// ensure_flow_session 的 check-then-act 序列整体进同一临界区——
    /// 并发两条同流消息不再各建会话（孤儿+绑定互抹）。
    /// scope 由调用方解析（组 id）；title 为会话标题；legacy 认领在锁内。
    pub fn ensure_flow_session_atomic(
        &self,
        channel_id: &str,
        chat_key: &str,
        scope: &str,
        title: &str,
        flow_name: Option<&str>,
        is_group: bool,
    ) -> Option<String> {
        let flock = self.flow_lock_arc(channel_id, chat_key);
        let _g = flock.lock();
        // 锁内直接读流文档（不走 flow_active_session——那会重复拿同一把锁，死锁）：
        // 绑定有效且会话仍存在 → 复用（他人可能已在此临界区建好绑定）
        if let Some(flow) = self.channel_flow(channel_id, chat_key) {
            if let Some(sid) = flow.active_session.clone() {
                if self.store.get_session(&sid).ok().flatten().is_some() {
                    // 流名回填（空填不覆盖；与 ensure_flow_named 同语义但在锁内）
                    if let Some(n) = flow_name.filter(|n| !n.trim().is_empty()) {
                        if flow.display_name.trim().is_empty() {
                            let mut f = flow.clone();
                            f.display_name = n.trim().to_string();
                            f.is_group = f.is_group || is_group;
                            let _ = self.store.put_channel_flow(&f);
                        }
                    }
                    return Some(sid);
                }
            }
        }
        // 旧 title 会话认领（升级迁移），否则新建
        let legacy = self
            .store
            .list_sessions_in_group(scope)
            .ok()
            .and_then(|list| list.into_iter().find(|s| s.title == title));
        let sid = match legacy {
            Some(s) => s.id,
            None => self.store.create_session(title, scope).ok()?.id,
        };
        // 回写绑定也直接操作（bind_flow_session_named 内部重复拿锁）：
        // 读流→合并名→写回，单临界区内完成
        let mut flow = self.channel_flow(channel_id, chat_key).unwrap_or_else(|| ChannelFlow {
            id: Self::flow_id(channel_id, chat_key),
            channel_id: channel_id.to_string(),
            chat_key: chat_key.to_string(),
            display_name: flow_name.unwrap_or("").trim().to_string(),
            is_group,
            active_session: None,
            last_message: String::new(),
            last_active_at: String::new(),
            created_at: now_iso(),
        });
        if flow.display_name.trim().is_empty() {
            if let Some(n) = flow_name {
                flow.display_name = n.trim().to_string();
            }
        }
        flow.is_group = flow.is_group || is_group;
        flow.active_session = Some(sid.clone());
        flow.last_active_at = now_iso();
        let _ = self.store.put_channel_flow(&flow);
        Some(sid)
    }
    /// 取流（不存在返回 None）
    pub fn channel_flow(&self, channel_id: &str, chat_key: &str) -> Option<ChannelFlow> {
        self.store
            .get_channel_flow(&Self::flow_id(channel_id, chat_key))
            .ok()
            .flatten()
    }
    pub fn channel_flows(&self, channel_id: Option<&str>) -> Vec<ChannelFlow> {
        self.store.list_channel_flows(channel_id).unwrap_or_default()
    }
    /// 保存流（upsert）
    pub fn save_channel_flow(&self, flow: &ChannelFlow) -> anyhow::Result<()> {
        self.store.put_channel_flow(flow)
    }
    pub fn drop_channel_flow(&self, id: &str) -> anyhow::Result<()> {
        self.store.delete_channel_flow(id)
    }
    /// 流未读 +1（智能体在流绑定会话产出回复时调用；spawn_reply 收束点）。
    /// 流不存在时忽略（未绑定流的会话无未读语义）。
    /// 一次性标记消费（agent-budget-audit 内联提示去重）：首次返回 true 并置位，
    /// 此后 false。进程内存态。
    pub fn consume_once_flag(&self, key: &str) -> bool {
        self.once_flags.lock().insert(key.to_string())
    }
    pub fn bump_flow_unread(&self, channel_id: &str, chat_key: &str) {
        let id = Self::flow_id(channel_id, chat_key);
        if self.store.get_channel_flow(&id).ok().flatten().is_none() {
            return;
        }
        let mut m = self.flow_unread.lock();
        let c = m.entry(id).or_insert(0);
        *c = c.saturating_add(1);
    }
    /// 流未读清零（用户打开该流绑定会话时调用；REST /flows/:id/read）
    pub fn clear_flow_unread(&self, channel_id: &str, chat_key: &str) {
        self.flow_unread
            .lock()
            .remove(&Self::flow_id(channel_id, chat_key));
    }
    /// 单条流未读数（列表输出用）
    pub fn flow_unread_count(&self, channel_id: &str, chat_key: &str) -> u32 {
        self.flow_unread
            .lock()
            .get(&Self::flow_id(channel_id, chat_key))
            .copied()
            .unwrap_or(0)
    }
    /// 流绑定的活跃会话；不存在或已删 → None（调用方开新会话后回写绑定）
    pub fn flow_active_session(&self, channel_id: &str, chat_key: &str) -> Option<String> {
        // 临界区（竞态 P1-3）：查绑定 + 失效清理原子化——与 bind/touch 互斥，
        // 防止清理与他方回写交错抹掉刚落的绑定
        let lock = self.flow_lock_arc(channel_id, chat_key);
        let _g = lock.lock();
        let flow = self.channel_flow(channel_id, chat_key)?;
        let sid = flow.active_session?;
        // 会话可能已被用户在控制台删除：失效绑定顺带清理，下次重建
        if self.store.get_session(&sid).ok().flatten().is_none() {
            let _ = self.drop_channel_flow(&flow.id);
            return None;
        }
        Some(sid)
    }
    /// 把流绑定到会话（流不存在则创建；顺带刷最后活跃）
    pub fn bind_flow_session(
        &self,
        channel_id: &str,
        chat_key: &str,
        session_id: &str,
    ) -> anyhow::Result<()> {
        self.bind_flow_session_named(channel_id, chat_key, session_id, None, None)
    }
    /// 流名维护（会话管理可用性修复 2026-11）：流的展示名（群名 / 对端昵称）
    /// 只在为空时回填——不覆盖既有值（可能是用户改过的自定义名，或更早来源
    /// 的更好名称）。适配器每次入站都带最新名调用，首次到达即落名。
    pub fn ensure_flow_named(
        &self,
        channel_id: &str,
        chat_key: &str,
        display_name: &str,
        is_group: bool,
    ) {
        if display_name.trim().is_empty() {
            return;
        }
        let Some(mut flow) = self.channel_flow(channel_id, chat_key) else {
            return;
        };
        if flow.display_name.trim().is_empty() {
            flow.display_name = display_name.trim().to_string();
            flow.is_group = flow.is_group || is_group;
            let _ = self.store.put_channel_flow(&flow);
        }
    }
    /// bind_flow_session 带名变体：建流时直接落展示名（避免先建空名流再补名
    /// 的窗口期——控制台流列表显示裸 chat_key 不可辨认）
    pub fn bind_flow_session_named(
        &self,
        channel_id: &str,
        chat_key: &str,
        session_id: &str,
        display_name: Option<&str>,
        is_group: Option<bool>,
    ) -> anyhow::Result<()> {
        // 临界区（竞态 P1-3）：读流→建/改→回写原子化——并发 bind（同流两条消息
        // 各建会话）不再互抹绑定、不再双建流文档
        let flock = self.flow_lock_arc(channel_id, chat_key);
        let _g = flock.lock();
        let id = Self::flow_id(channel_id, chat_key);
        let mut flow = self
            .channel_flow(channel_id, chat_key)
            .unwrap_or(ChannelFlow {
                id: id.clone(),
                channel_id: channel_id.to_string(),
                chat_key: chat_key.to_string(),
                display_name: display_name.unwrap_or("").trim().to_string(),
                is_group: is_group.unwrap_or(false),
                active_session: None,
                last_message: String::new(),
                last_active_at: String::new(),
                created_at: now_iso(),
            });
        // 既有流只补空名（不覆盖用户自定义）
        if flow.display_name.trim().is_empty() {
            if let Some(n) = display_name {
                flow.display_name = n.trim().to_string();
            }
        }
        if let Some(g) = is_group {
            flow.is_group = flow.is_group || g;
        }
        flow.active_session = Some(session_id.to_string());
        flow.last_active_at = now_iso();
        self.store.put_channel_flow(&flow)
    }
    /// 流的消息预览回写（入站消息落库后调用；不建流——只有 begin 才建）
    pub fn touch_flow_message(
        &self,
        channel_id: &str,
        chat_key: &str,
        preview: &str,
    ) -> anyhow::Result<()> {
        // 临界区（竞态 P1-3）：读改写原子化——touch 的整流写回不再覆盖
        // 期间他方落的 bind（active_session）
        let flock = self.flow_lock_arc(channel_id, chat_key);
        let _g = flock.lock();
        if let Some(mut flow) = self.channel_flow(channel_id, chat_key) {
            flow.last_message = preview.chars().take(120).collect();
            flow.last_active_at = now_iso();
            self.store.put_channel_flow(&flow)?;
        }
        Ok(())
    }
    /// 签发一次性配对码（8 位短码，TTL 由调用方给）
    pub fn issue_pairing_code(
        &self,
        note: &str,
        platform: Option<&str>,
        ttl_secs: u64,
    ) -> anyhow::Result<PairingCode> {
        self.issue_pairing_code_role(note, platform, ttl_secs, "member")
    }
    /// 带预设角色签发（admin = 管理者配对码；兑换即落 admin）
    pub fn issue_pairing_code_role(
        &self,
        note: &str,
        platform: Option<&str>,
        ttl_secs: u64,
        role: &str,
    ) -> anyhow::Result<PairingCode> {
        let code = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
        let c = PairingCode {
            code: code.clone(),
            note: note.to_string(),
            channel_platform: platform.map(|s| s.to_string()),
            created_by: "console".into(),
            created_at: now_iso(),
            expires_at_ms: now_ms() + ttl_secs * 1000,
            role: if role == "admin" {
                "admin".into()
            } else {
                "member".into()
            },
        };
        self.store.put_pairing_code(&c)?;
        Ok(c)
    }
    pub fn pairing_codes(&self) -> Vec<PairingCode> {
        self.store.list_pairing_codes().unwrap_or_default()
    }
    /// 直接落一张配对码（迁移 / 测试 / 控制台定向写入）
    pub fn save_pairing_code(&self, code: &PairingCode) -> anyhow::Result<()> {
        self.store.put_pairing_code(code)
    }
    /// 兑换配对码 → 绑定身份（单次有效 + 过期校验；channel 为通道账号 id）
    pub fn redeem_pairing(
        &self,
        code: &str,
        channel: &str,
        external_id: &str,
        display_name: &str,
        default_role: &str,
    ) -> anyhow::Result<UserIdentity> {
        let c = self
            .store
            .consume_pairing_code(code.trim())
            .map_err(|e| anyhow::anyhow!("{e}"))?
            .ok_or_else(|| anyhow::anyhow!("配对码无效或已被使用"))?;
        if now_ms() > c.expires_at_ms {
            anyhow::bail!("配对码已过期，请重新签发");
        }
        if let Some(p) = &c.channel_platform {
            if !channel.starts_with(p.as_str()) {
                anyhow::bail!("配对码限定平台 {p}，当前通道不可用");
            }
        }
        let identity = UserIdentity {
            id: format!("{channel}:{external_id}"),
            channel: channel.to_string(),
            external_id: external_id.to_string(),
            display_name: display_name.to_string(),
            // 预设角色优先（签发时指定）；存量码无角色落 default_role（member）
            role: if c.role.trim().is_empty() {
                default_role.to_string()
            } else {
                c.role.clone()
            },
            paired_at: now_iso(),
            note: c.note.clone(),
        };
        self.store.put_identity(&identity)?;
        Ok(identity)
    }

}
