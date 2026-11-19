//! Explicit, user-directed fan-out on one gateway. A room is only a transcript
//! and a roster; model history, tools, memory and approvals stay in each member's
//! ordinary (but room-owned) project session.

use crate::{
    round_trace::RoundGuard,
    types::{new_id, now_iso, ApprovalRequest, ChatMessage, MessageRole, ToolScope},
    Core,
};
use futures_util::{stream::FuturesUnordered, StreamExt};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

const MAX_MEMBERS: usize = 8;
const PAGE_SIZE: usize = 8;

#[derive(Debug, thiserror::Error)]
pub enum CollaborationError {
    #[error("{0}")]
    Invalid(String),
    #[error("协作群聊或轮次不存在")]
    NotFound,
    #[error("{0}")]
    Conflict(String),
    #[error(transparent)]
    Storage(#[from] anyhow::Error),
}

type Result<T> = std::result::Result<T, CollaborationError>;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MemberInput {
    pub target: String,
    pub workspace: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CollaborationMember {
    pub session_id: String,
    pub target: String,
    pub label: String,
    pub workspace: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CollaborationRoom {
    pub id: String,
    pub title: String,
    pub workspace: String,
    pub members: Vec<CollaborationMember>,
    pub updated_at: String,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct CollaborationRecord {
    pub room: CollaborationRoom,
    /// Append order independent of clock skew; pages need not scan all turn files.
    pub turns: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Recipient {
    pub session_id: String,
    #[serde(default)]
    pub instruction: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TurnInput {
    pub request_id: String,
    #[serde(default)]
    pub text: String,
    pub recipients: Vec<Recipient>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DeliveryStatus {
    Running,
    Done,
    Failed,
    Stopped,
    Interrupted,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CollaborationDelivery {
    pub session_id: String,
    pub instruction: String,
    pub status: DeliveryStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub messages: Vec<ChatMessage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CollaborationTurn {
    pub id: String,
    pub text: String,
    pub created_at: String,
    pub deliveries: Vec<CollaborationDelivery>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CollaborationApproval {
    pub member_session_id: String,
    pub id: String,
    pub command: String,
    pub agent_id: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CollaborationPage {
    pub room: CollaborationRoom,
    pub turns: Vec<CollaborationTurn>,
    pub next_before: Option<String>,
    pub active_turn_id: Option<String>,
    pub approvals: Vec<CollaborationApproval>,
}

struct LiveTurn {
    core: Arc<Core>,
    room_id: String,
}

impl Drop for LiveTurn {
    fn drop(&mut self) {
        self.core.collaboration_runs.lock().remove(&self.room_id);
    }
}

fn id_is_canonical(id: &str) -> bool {
    uuid::Uuid::parse_str(id).is_ok_and(|value| value.to_string() == id)
}

fn invalid(message: &str) -> CollaborationError {
    CollaborationError::Invalid(message.into())
}

impl Core {
    pub fn create_collaboration(
        &self,
        workspace: &str,
        title: &str,
        members: Vec<MemberInput>,
    ) -> Result<CollaborationRoom> {
        let title = title.trim();
        if title.is_empty() || title.chars().count() > 100 {
            return Err(invalid("群聊名称需要 1–100 个字符"));
        }
        if !(2..=MAX_MEMBERS).contains(&members.len()) {
            return Err(invalid("请选择 2–8 个群聊成员"));
        }
        // Validate the entire roster before creating any sessions. No client-supplied
        // session IDs or labels, and no fallback to the active target or global directory.
        let workspace = canonical_workspace(workspace)?;
        let mut roster = Vec::with_capacity(members.len());
        let mut seen = HashSet::new();
        for member in members {
            let root = canonical_workspace(&member.workspace)?;
            let label = if let Some(id) = member.target.strip_prefix("single:") {
                self.registry.single(id).map(|agent| agent.name)
            } else {
                self.registry
                    .group_meta(&member.target)
                    .map(|group| group.name)
            }
            .ok_or_else(|| invalid("有成员的对话对象不存在"))?;
            if !seen.insert((root.clone(), member.target.clone())) {
                return Err(invalid("同一项目中的同一对象只能加入一次"));
            }
            roster.push((root, member.target, label));
        }
        let mut room = CollaborationRoom {
            id: new_id(),
            title: title.into(),
            workspace,
            members: Vec::with_capacity(roster.len()),
            updated_at: now_iso(),
        };
        let created = (|| -> anyhow::Result<()> {
            for (root, target, label) in roster {
                let session = self.store.create_bound_session(
                    &format!("{title} · {label}"),
                    &target,
                    Some(root.clone()),
                    Some(room.id.clone()),
                )?;
                room.members.push(CollaborationMember {
                    session_id: session.id,
                    target,
                    label,
                    workspace: root,
                });
            }
            self.store.put_collaboration(&CollaborationRecord {
                room: room.clone(),
                turns: vec![],
            })
        })();
        if let Err(error) = created {
            for member in &room.members {
                let _ = self.store.delete_session(&member.session_id);
            }
            return Err(error.into());
        }
        Ok(room)
    }

    pub fn list_collaborations(&self, workspace: &str) -> Result<Vec<CollaborationRoom>> {
        let mut rooms: Vec<_> = self
            .store
            .list_collaborations()?
            .into_iter()
            .map(|record| record.room)
            .filter(|room| room.workspace == workspace)
            .collect();
        rooms.sort_by(|a, b| b.updated_at.cmp(&a.updated_at).then(a.id.cmp(&b.id)));
        Ok(rooms)
    }

    fn collaboration_record(&self, workspace: &str, id: &str) -> Result<CollaborationRecord> {
        if !id_is_canonical(id) {
            return Err(CollaborationError::NotFound);
        }
        self.store
            .get_collaboration(id)?
            .filter(|record| record.room.workspace == workspace)
            .ok_or(CollaborationError::NotFound)
    }

    pub fn collaboration_page(
        &self,
        workspace: &str,
        id: &str,
        before: Option<&str>,
    ) -> Result<CollaborationPage> {
        // Snapshot the index and active ID on the same side of admission/finish.
        let (record, active_turn_id) = {
            let running = self.collaboration_runs.lock();
            (
                self.collaboration_record(workspace, id)?,
                running.get(id).cloned(),
            )
        };
        let end = match before {
            Some(cursor) => record
                .turns
                .iter()
                .position(|turn| turn == cursor)
                .ok_or(CollaborationError::NotFound)?,
            None => record.turns.len(),
        };
        let start = end.saturating_sub(PAGE_SIZE);
        let mut turns = Vec::with_capacity(end - start);
        for id in &record.turns[start..end] {
            let mut turn = self.store.get_collaboration_turn(&record.room.id, id)?;
            if active_turn_id.as_ref() != Some(id) {
                for delivery in &mut turn.deliveries {
                    if delivery.status == DeliveryStatus::Running {
                        delivery.status = DeliveryStatus::Interrupted;
                    }
                }
            }
            turns.push(turn);
        }
        let mut approvals = Vec::new();
        if before.is_none() && !record.turns.is_empty() {
            for approval in self.approval_list(Some("pending"), usize::MAX)? {
                if let Some(member) = self.collaboration_approval_owner(&record.room, &approval)? {
                    approvals.push(CollaborationApproval {
                        member_session_id: member.session_id.clone(),
                        id: approval.id,
                        command: approval.command,
                        agent_id: approval.agent_id,
                    });
                }
            }
        }
        Ok(CollaborationPage {
            room: record.room,
            turns,
            next_before: (start > 0).then(|| record.turns[start].clone()),
            active_turn_id,
            approvals,
        })
    }

    fn collaboration_approval_owner<'a>(
        &self,
        room: &'a CollaborationRoom,
        approval: &ApprovalRequest,
    ) -> Result<Option<&'a CollaborationMember>> {
        let Some(workspace) = self.approval_workspace(approval).ok().flatten() else {
            return Ok(None);
        };
        let mut id = approval.session_id.clone();
        let mut visited = HashSet::new();
        while visited.insert(id.clone()) {
            let Some(session) = self.store.get_session(&id)? else {
                break;
            };
            if session.collaboration_id.as_deref() != Some(&room.id)
                || session.workspace.as_deref() != Some(&workspace)
            {
                break;
            }
            if let Some(member) = room
                .members
                .iter()
                .find(|m| m.session_id == id && m.workspace == workspace)
            {
                return Ok(Some(member));
            }
            let Some(parent) = session.parent_session else {
                break;
            };
            id = parent;
        }
        Ok(None)
    }

    /// The request ID enables a client to retry an ambiguous network failure without
    /// repeating a tool-using turn. Only one turn per room may be active at a time.
    pub fn start_collaboration(
        self: &Arc<Self>,
        workspace: &str,
        room_id: &str,
        mut input: TurnInput,
    ) -> Result<String> {
        if !id_is_canonical(&input.request_id) {
            return Err(invalid("requestId 必须是规范 UUID"));
        }
        input.text = input.text.trim().to_string();
        if input.text.len() > 32_000 || !(1..=MAX_MEMBERS).contains(&input.recipients.len()) {
            return Err(invalid("请选择 1–8 个收件人，公共消息不得超过 32000 字节"));
        }
        for recipient in &mut input.recipients {
            recipient.instruction = recipient.instruction.trim().to_string();
            if recipient.instruction.len() > 16_000
                || (input.text.is_empty() && recipient.instruction.is_empty())
            {
                return Err(invalid(
                    "每位收件人必须有消息或分工；分工不得超过 16000 字节",
                ));
            }
        }

        let mut running = self.collaboration_runs.lock();
        let mut record = self.collaboration_record(workspace, room_id)?;
        if record.turns.contains(&input.request_id) {
            let existing = self
                .store
                .get_collaboration_turn(room_id, &input.request_id)?;
            if existing.text != input.text
                || !existing
                    .deliveries
                    .iter()
                    .map(|d| (&d.session_id, &d.instruction))
                    .eq(input
                        .recipients
                        .iter()
                        .map(|r| (&r.session_id, &r.instruction)))
            {
                return Err(CollaborationError::Conflict(
                    "此 requestId 已被另一条消息使用".into(),
                ));
            }
            return Ok(input.request_id);
        }
        if running.contains_key(room_id) {
            return Err(CollaborationError::Conflict(
                "上一轮仍在执行，请等待或停止该轮".into(),
            ));
        }

        let mut roots = HashMap::<String, Arc<tokio::sync::Mutex<()>>>::new();
        let mut jobs = Vec::with_capacity(input.recipients.len());
        let mut seen = HashSet::new();
        for recipient in &input.recipients {
            let member = record
                .room
                .members
                .iter()
                .find(|m| m.session_id == recipient.session_id)
                .ok_or_else(|| invalid("收件人不在这个群聊中"))?;
            if !seen.insert(&member.session_id) {
                return Err(invalid("收件人不能重复"));
            }
            let lock = self
                .run_lock(&member.session_id)
                .try_lock_owned()
                .map_err(|_| CollaborationError::Conflict("该成员的会话仍在执行".into()))?;
            let session = self.store.get_session(&member.session_id)?;
            let session = session.filter(|s| {
                s.collaboration_id.as_deref() == Some(room_id)
                    && s.workspace.as_ref() == Some(&member.workspace)
                    && s.group_id == member.target
            });
            let root_lock = roots.entry(member.workspace.clone()).or_default().clone();
            let text = match (input.text.as_str(), recipient.instruction.as_str()) {
                ("", task) => task.to_string(),
                (common, "") => common.to_string(),
                (common, task) => format!("{common}\n\n你的分工：\n{task}"),
            };
            jobs.push((session, lock, root_lock, text));
        }

        let turn = CollaborationTurn {
            id: input.request_id.clone(),
            text: input.text,
            created_at: now_iso(),
            deliveries: input
                .recipients
                .into_iter()
                .map(|recipient| CollaborationDelivery {
                    session_id: recipient.session_id,
                    instruction: recipient.instruction,
                    status: DeliveryStatus::Running,
                    messages: vec![],
                    error: None,
                })
                .collect(),
        };
        // Nothing runs until both records are durable. An unindexed turn written
        // before a failed index write is inert and can be overwritten by a retry.
        self.store.put_collaboration_turn(room_id, &turn)?;
        record.room.updated_at = turn.created_at.clone();
        record.turns.push(turn.id.clone());
        self.store.put_collaboration(&record)?;

        let rounds = jobs
            .into_iter()
            .zip(&turn.deliveries)
            .enumerate()
            .map(|(index, ((session, lock, root_lock, text), delivery))| {
                let core = self.clone();
                let session_id = delivery.session_id.clone();
                let guard = RoundGuard::begin(&session_id);
                async move {
                    let (_lock, _guard) = (lock, guard);
                    // Members of one exact project execute serially within this turn.
                    let _project = root_lock.lock().await;
                    if crate::round_trace::is_cancelled(&session_id) {
                        return (index, DeliveryStatus::Stopped, Vec::new(), None);
                    }
                    let before = core.store.message_count(&session_id);
                    let result = match session {
                        Some(session) => {
                            core.execute_chat_round(&session, &text, ToolScope::Tools, None, None)
                                .await
                        }
                        None => Err(anyhow::anyhow!("该成员的会话已不存在或归属已变化")),
                    };
                    let messages = core.store.list_messages(&session_id, 0).map(|rows| {
                        rows.into_iter()
                            .skip(before)
                            .filter(|m| m.role != MessageRole::User)
                            .collect()
                    });
                    let (status, messages, error) = if crate::round_trace::is_cancelled(&session_id)
                    {
                        (DeliveryStatus::Stopped, messages.unwrap_or_default(), None)
                    } else {
                        match (result, messages) {
                            (Ok(()), Ok(rows)) => (DeliveryStatus::Done, rows, None),
                            (Err(error), rows) => (
                                DeliveryStatus::Failed,
                                rows.unwrap_or_default(),
                                Some(error.to_string()),
                            ),
                            (_, Err(error)) => {
                                (DeliveryStatus::Failed, vec![], Some(error.to_string()))
                            }
                        }
                    };
                    (index, status, messages, error)
                }
            })
            .collect::<FuturesUnordered<_>>();
        running.insert(room_id.into(), turn.id.clone());
        let live = LiveTurn {
            core: self.clone(),
            room_id: room_id.into(),
        };
        drop(running);
        tokio::spawn(async move {
            let _live = live;
            let mut turn = turn;
            let mut rounds = rounds;
            while let Some((index, status, messages, error)) = rounds.next().await {
                let delivery = &mut turn.deliveries[index];
                delivery.status = status;
                delivery.messages = messages;
                delivery.error = error;
                if let Err(error) = _live
                    .core
                    .store
                    .put_collaboration_turn(&_live.room_id, &turn)
                {
                    eprintln!("[collaboration] 轮次 {} 保存失败: {error}", turn.id);
                }
            }
        });
        Ok(input.request_id)
    }

    pub fn stop_collaboration(
        &self,
        workspace: &str,
        room_id: &str,
        turn_id: &str,
        member: Option<&str>,
    ) -> Result<usize> {
        let running = self.collaboration_runs.lock();
        self.collaboration_record(workspace, room_id)?;
        // Keep the generation lock until cancellation is signalled; an old stop
        // request must not affect a newer turn of the same member sessions.
        if running.get(room_id).is_none_or(|id| id != turn_id) {
            return Err(CollaborationError::Conflict("该轮已不在执行".into()));
        }
        let turn = self.store.get_collaboration_turn(room_id, turn_id)?;
        if member.is_some_and(|id| !turn.deliveries.iter().any(|d| d.session_id == id)) {
            return Err(CollaborationError::NotFound);
        }
        let stopped = turn
            .deliveries
            .iter()
            .filter(|d| member.is_none_or(|id| id == d.session_id))
            .filter(|d| self.stop_session(&d.session_id).is_ok())
            .count();
        if stopped == 0 {
            return Err(CollaborationError::Conflict(
                "没有可以停止的成员任务".into(),
            ));
        }
        Ok(stopped)
    }
}

fn canonical_workspace(path: &str) -> Result<String> {
    let root = crate::workspace::pinned(path)
        .map_err(|error| CollaborationError::Invalid(error.to_string()))?;
    // pinned rejects changed/symlinked roots, and resolve already verifies UTF-8.
    Ok(root.to_string_lossy().into_owned())
}
