//! channel 域分文件：回帖发言权（latest-wins）+ 入站媒体下载/暂存。

use super::*;

// ---------------------------------------------------------------- 回帖发言权（latest-wins）
//
// 重复回复根治（生产实测）：同一会话上并发挂多个回帖任务时（上一轮还在跑，
// 用户又发来新消息——每条入站消息各挂一个 spawn_reply），旧任务不会退出
// （退出只认 run.finished / run.error），旧轮的收束事件会被新旧两个任务
// 同时收到、各发一遍——用户视角「回复发两遍」。
//
// 规则：同会话只允许「最新」回帖任务发言。新任务诞生即接管发言权；被取代
// 的旧任务继续消化事件（保持 channel 容量回收）但一律不发送，直至自己
// 等到收束事件退出。这样每轮回复恰好由一个任务发出一次。
//
// 退出配对（防丢回复，生产实测）：回帖任务的「自己的收束事件」判定 =
// run() 已返回（round_done）后到达的 run.finished/run.error；此时广播里
// 可能还有在途事件（自己的收束紧随 run() 返回，微秒级乱序），故进入
// 「排空模式」把队列里剩余事件全部消化（作为发言人正常发送）再退出。
// 更早到达的收束事件属于前序轮次：代发其回复后继续等自己那轮——旧版
// 在此直接退出，导致追问轮次的回复无人发送（长轮次期间用户追问必现）。
//
// 注：不改收束事件的产生语义，也不合并轮次（排队窗口合并逻辑在 core 侧
// collect_pending_inputs，靠会话串行锁保证），只修「谁有资格发、何时退」。

/// 会话 → 当前发言任务的序号（latest-wins：新任务恒接管；序号在 spawn 前
/// 分配，接管顺序 = 创建顺序 = 轮次排队顺序，与任务体调度顺序无关）。
fn reply_ledger() -> &'static Mutex<HashMap<String, u64>> {
    static SPEAKERS: OnceLock<Mutex<HashMap<String, u64>>> = OnceLock::new();
    SPEAKERS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn next_reply_seq() -> u64 {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

pub(crate) fn spawn_reply<F, Fut, M, MFut>(
    run: &ChannelRun,
    rx: tokio::sync::broadcast::Receiver<CoreEvent>,
    max_chars: usize,
    send: F,
    send_media: M,
) -> tokio::task::JoinHandle<()>
where
    F: Fn(String) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send,
    M: Fn(MediaItem) -> MFut + Send + 'static,
    MFut: std::future::Future<Output = bool> + Send,
{
    spawn_reply_with_approval(run, rx, max_chars, send, send_media, None)
}

/// `spawn_reply` 的审批富消息变体：审批事件优先交给 `approval_send` 钩子
/// （平台原生控件），失败再走普通文本分段。其余事件行为完全一致。
pub(crate) fn spawn_reply_with_approval<F, Fut, M, MFut>(
    run: &ChannelRun,
    rx: tokio::sync::broadcast::Receiver<CoreEvent>,
    max_chars: usize,
    send: F,
    send_media: M,
    approval_send: Option<ApprovalHook>,
) -> tokio::task::JoinHandle<()>
where
    F: Fn(String) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send,
    M: Fn(MediaItem) -> MFut + Send + 'static,
    MFut: std::future::Future<Output = bool> + Send,
{
    let run = run.clone();
    // 发言权接管（latest-wins）：spawn 前登记（顺序即创建顺序），
    // 本任务成为该会话当前唯一发言人；旧任务自此静默。
    let my_seq = next_reply_seq();
    reply_ledger()
        .lock()
        .insert(run.session_id.clone(), my_seq);
    tokio::spawn(async move {
        let mut rx = rx;
        let sid = run.session_id.clone();
        let is_speaker = || {
            reply_ledger().lock().get(&sid).copied() == Some(my_seq)
        };
        // round_done 兜底（见模块注释）：无收束事件的异常路径下防悬挂
        let round_done = || run.round_done.load(std::sync::atomic::Ordering::Acquire);
        // 收流容错（竞态 P1-4）：Lagged = 广播环溢出丢了一批旧事件，新事件仍在
        // 环里——必须继续收，退出会让本轮回复静默丢失。Closed = 发送端全关。
        //
        // 观察通道（无单发等待，杜绝错过通知的悬挂）：
        // - 主 select 等事件，同时等 250ms 心跳 tick；
        // - round_done 置位后（tick 观察到，或收束事件到达时已置位）：进入
        //   排空模式，try_recv 消化完在途广播后退出。
        //
        // 退出条件（三选一）：
        // ① 消化到收束事件且 round_done 已置位（进入排空，消化完退出）；
        // ② round_done 置位但无收束事件（异常路径兜底，排空后退出防悬挂）；
        // ③ 已被更新任务取代时到达的收束事件（静默让位退出）。
        let mut draining = false;
        loop {
            let evt = if draining {
                match rx.try_recv() {
                    Ok(evt) => evt,
                    Err(_) => break, // 排空完毕，无收束事件：兜底退出
                }
            } else {
                tokio::select! {
                    r = rx.recv() => match r {
                        Ok(evt) => evt,
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    },
                    _ = tokio::time::sleep(std::time::Duration::from_millis(250)) => {
                        if round_done() {
                            draining = true;
                        }
                        continue;
                    }
                }
            };
            if evt.session_id != run.session_id {
                continue;
            }
            // 已被更新的回帖任务取代：静默让位——不再发送，见到收束事件即退出
            //（自己的轮次与回复由接管的发言任务负责送达）。
            if !is_speaker() {
                if matches!(evt.kind.as_str(), "run.finished" | "run.error") {
                    break;
                }
                continue;
            }
            // 产物投递（组 6.2/6.3）：能力矩阵判定直发或降级并注
            let mut media_notes: Vec<String> = Vec::new();
            if let Some(list) = evt.payload.get("artifacts").and_then(|v| v.as_array()) {
                for a in list {
                    let kind = a
                        .get("kind")
                        .and_then(|x| x.as_str())
                        .unwrap_or("file")
                        .to_string();
                    let rel = a
                        .get("path")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string();
                    if rel.is_empty() {
                        continue;
                    }
                    let caption = a
                        .get("caption")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string();
                    let abs = run.core.config().workspace_root.join(&rel);
                    let item = MediaItem {
                        kind: kind.clone(),
                        path: abs,
                        caption,
                    };
                    match media_fallback(&run.kind, &kind) {
                        MediaFallback::Native => {
                            if !send_media(item.clone()).await {
                                media_notes.push(format!(
                                    "📎 {}：{rel}（直发失败，可在工作区查看）",
                                    item.caption
                                ));
                            }
                        }
                        MediaFallback::LinkNote => {
                            media_notes.push(format!("📎 {}：{rel}（工作区内）", item.caption));
                        }
                    }
                }
            }
            if let Some(text) = run.take_reply(&evt, max_chars) {
                // 流未读 +1：智能体在本流产出回复（spec experience-iteration 未读徽标；
                // 用户打开该会话时清零）。一次收束记一条未读（不是按分段计数）。
                if let Some((ch_id, chat_key)) = run.flow.as_ref() {
                    run.core.bump_flow_unread(ch_id, chat_key);
                }
                // 审批事件优先走平台富消息（approval-ergonomics 平台交互）：
                // 如 QQ 官方机器人按钮，点击即产生 ok:/no: 裁决指令；失败回落文本。
                if evt.kind == "approval.required" {
                    if let (Some(hook), Some(id)) = (
                        approval_send.as_ref(),
                        evt.payload.get("approvalId").and_then(|v| v.as_str()),
                    ) {
                        let prompt = ApprovalPrompt {
                            approval_id: id.to_string(),
                            text: text.clone(),
                        };
                        if hook(prompt).await {
                            continue;
                        }
                    }
                }
                // 长回复分段推送（组 6.6）：按平台上限逐段送达，顺序保持。
                // 段数封顶（IM 防轰炸）：异常超长触顶时截断并注明去处——
                // 需要完整长文的场景（文档/代码交付）应走文件产物而非刷屏。
                // 官方机器人被动回复每条入站消息限 ~5 次（群 5 次/5 分钟），
                // 审批/计划推送还共享同一额度——qqbot 单独收紧防后段静默失败。
                let segments = split_reply(&text, max_chars);
                let total = segments.len();
                let max_segments = if run.kind == "qqbot" { 4 } else { MAX_REPLY_SEGMENTS };
                if total <= max_segments {
                    for (i, seg) in segments.into_iter().enumerate() {
                        // 产物注记并入最后一段（文本主体保持整洁）
                        if i + 1 == total && !media_notes.is_empty() {
                            send(format!("{seg}\n{}", media_notes.join("\n"))).await;
                        } else {
                            send(seg).await;
                        }
                    }
                } else {
                    let capped = total - max_segments;
                    for seg in segments.into_iter().take(max_segments - 1) {
                        send(seg).await;
                    }
                    let mut note = media_notes.join("\n");
                    note.truncate(note.char_indices().nth(200).map(|(i, _)| i).unwrap_or(note.len()));
                    send(format!(
                        "（内容过长已截断，略去末尾 {capped} 段；完整回复见工作台会话记录）\n{note}"
                    ))
                    .await;
                }
                // 退出判定（见模块注释）：收束事件 + 本轮已执行完（round_done）
                // ⇒ 自己那轮收束事件已到达。广播里可能还有紧随的在途事件
                //（emit 与 store 微秒级乱序）：进入排空模式消化完再退出，
                // 防止把同批收束答复漏发。更早到达（round_done 未置位）的
                // 收束事件属于前序轮次：代发后继续等自己那轮。
                if matches!(evt.kind.as_str(), "run.finished" | "run.error") && round_done() {
                    draining = true;
                }
            }
        }
        // 退出清理发言权：仅当仍属自己（新任务已接管时不误删）
        {
            let mut l = reply_ledger().lock();
            if l.get(&sid).copied() == Some(my_seq) {
                l.remove(&sid);
            }
        }
    })
}

/// 入站媒体：下载保存到 `.exmachina/inbox/{通道id}/`，返回（绝对路径, 文件名）
pub(crate) async fn save_inbound_media(
    core: &exm_core::Core,
    ch: &Channel,
    name: &str,
    bytes: Vec<u8>,
) -> Option<String> {
    let dir = core
        .config()
        .workspace_root
        .join(".exmachina")
        .join("inbox")
        .join(&ch.id);
    tokio::fs::create_dir_all(&dir).await.ok()?;
    let safe: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || "._-".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    let file = dir.join(format!("{}-{safe}", exm_core::types::now_ms()));
    tokio::fs::write(&file, &bytes).await.ok()?;
    Some(file.display().to_string())
}

/// 通用下载（入站媒体用）：带超时；失败 None
pub(crate) async fn download_bytes(url: &str) -> Option<Vec<u8>> {
    reqwest::Client::new()
        .get(url)
        .timeout(std::time::Duration::from_secs(60))
        .send()
        .await
        .ok()?
        .bytes()
        .await
        .ok()
        .map(|b| b.to_vec())
}

/// 带鉴权下载（slack url_private 等）：Bearer token
pub(crate) async fn download_bytes_auth(url: &str, token: &str) -> Option<Vec<u8>> {
    reqwest::Client::new()
        .get(url)
        .bearer_auth(token)
        .timeout(std::time::Duration::from_secs(60))
        .send()
        .await
        .ok()?
        .bytes()
        .await
        .ok()
        .map(|b| b.to_vec())
}

/// 入站媒体注入会话：图片走多模态暂存（data URL），文件/语音注入路径说明。
/// 追加到消息文本后由模型一并感知。返回注入说明文本。
/// 同时把附件元数据（工作区相对路径）登记进会话暂存，随本轮 user 消息落库
/// （im-inbound-attach）——WebUI 聊天流据此渲染缩略图/下载卡片。
pub(crate) fn stage_inbound_media(
    core: &exm_core::Core,
    session_id: &str,
    saved: &[(String, String, String)], // (kind, 绝对路径, 文件名)
) -> String {
    let mut notes = String::new();
    let mut rels: Vec<exm_core::types::InboundAttachment> = Vec::new();
    for (kind, path, name) in saved {
        // 绝对路径 → 工作区相对路径（展示层经 /api/asset 预览；strip 失败保留原样兜底）
        let rel = std::path::Path::new(path)
            .strip_prefix(&core.config().workspace_root)
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| path.clone());
        rels.push(exm_core::types::InboundAttachment {
            kind: kind.clone(),
            path: rel,
            name: name.clone(),
        });
        match kind.as_str() {
            "image" => {
                if let Ok(bytes) = std::fs::read(path) {
                    use base64::Engine as _;
                    let data = format!(
                        "data:image/png;base64,{}",
                        base64::engine::general_purpose::STANDARD.encode(bytes)
                    );
                    exm_core::image_stash::stage(session_id, vec![data]);
                    notes.push_str(&format!("\n（图片附件 {name} 已注入，可直接描述画面内容）"));
                } else {
                    notes.push_str(&format!("\n（图片附件 {name} 读取失败）"));
                }
            }
            "voice" => {
                notes.push_str(&format!("\n（语音附件 {name} 已保存：{path}，可按需处理）"));
            }
            _ => {
                notes.push_str(&format!("\n（文件附件 {name} 已保存：{path}）"));
            }
        }
    }
    if !rels.is_empty() {
        exm_core::inbox_stash::stage(session_id, rels);
    }
    notes
}
