//! channel 域分文件：入站闸门（身份+群聊唤醒）+ 旁听落库（ambient）+ 流会话定位。

use super::*;

// ---------------------------------------------------------------- 通道入站闸门（身份 + 群聊唤醒）

/// 入站消息上下文：由各适配器从平台消息归一化提取
pub(crate) struct InboundCtx<'a> {
    pub text: &'a str,
    pub external_id: &'a str,
    pub display_name: &'a str,
    /// 群聊 / 频道上下文（私聊为 false）
    pub is_group: bool,
    /// 平台归一化信号：被提及（@）或回复智能体消息
    pub mentioned: bool,
    /// 通道会话键（与会话标题 `channel:<id>:<key>` 一致；/stop 定位会话用）
    pub chat_key: &'a str,
    /// 流展示名（群名 / 对端昵称；适配器尽力提供，None = 未知）——
    /// 建流/补名用（旁听路径建流也有名可用），见 ensure_flow_named
    pub flow_name: Option<&'a str>,
    /// 本条消息自带的已落盘媒体（kind, 绝对路径, 文件名）——旁听路径的附件
    /// 回填只挂本条消息的媒体（napcat 闸门前预下载并携带）。主链注入媒体
    /// 不走此字段（各适配器自行下载暂存）。无旁听预下载的适配器传 `&[]`。
    pub media: &'a [(String, String, String)],
}

pub(crate) enum GateDecision {
    /// 放行，携带消毒后文本与「是否主动回复放行」标记——消毒在身份判定之后执行
    /// （inbound-sanitization 分层要求），携带于返回值使调用方无法绕过消毒结果。
    /// proactive=true（主动插话，非 @ 点名）时执行层降 Chat 档：插话是参与讨论，
    /// 不是接受任务——群友间闲聊不能被当成工作指令执行工具；需要干活的场景
    /// 由用户 @ 点名触发完整能力。
    Allow(String, bool),
    /// 群聊未唤醒：静默忽略
    Ignore,
    /// 拒绝执行并回复提示（含 /pair 结果与配对指引）
    Deny(String),
}

/// 入站统一闸门：① `/pair` 兑换拦截（未绑定也可用）→ ② 群聊唤醒门控 → ③ 身份管控裁决。
/// 位置前置于一切执行（ChannelRun::begin 之前由各适配器调用）。
pub(crate) async fn admit(
    core: &exm_core::Core,
    ch: &Channel,
    ctx: &InboundCtx<'_>,
) -> GateDecision {
    let identity_cfg = &core.config().identity;

    // ⓪ /stop 停止指令（先于一切校验）：直接终止该通道会话运行中的轮次并回执
    // 权限：仅管理员（通道 admins 清单或绑定身份 role=admin）——与 /reset 同口径。
    // 聊天流是共享上下文，任何成员都能中止运行中任务 = 一个人手滑全班陪葬。
    // 非管理员未 @ 静默忽略（不进旁听噪音）；@ 场景给拒绝回执（被点名不回应是失礼）。
    if ctx.text.trim() == "/stop" {
        if !is_channel_admin(core, ch, ctx.external_id) {
            if ctx.mentioned {
                return GateDecision::Deny(
                    "【报告】/stop 需要管理员权限：停止的是共享会话的运行任务，会影响所有人。请联系管理员操作。".into(),
                );
            }
            return GateDecision::Ignore;
        }
        let gid = ch.group.clone().unwrap_or_else(|| core.active_group());
        let title = format!("channel:{}:{}", ch.id, ctx.chat_key);
        let sid = core.flow_active_session(&ch.id, ctx.chat_key).or_else(|| {
            // 流无绑定（含升级前的存量数据）：回落旧 title 寻址
            core.list_sessions_in_group(&gid)
                .ok()
                .and_then(|list| list.into_iter().find(|s| s.title == title))
                .map(|s| s.id)
        });
        let receipt = match sid.map(|id| core.stop_session(&id)) {
            Some(Ok(n)) => format!("⏹ 已停止本轮（已执行 {n} 次工具调用）。"),
            Some(Err(_)) | None => "当前没有运行中的任务。".to_string(),
        };
        return GateDecision::Deny(receipt);
    }

    // ⓪' /queue 显式入队（capability-parity 3.4）：`/queue <指令>` 把指令加入
    // 流会话的后续任务队列——与普通消息的 steering 语义严格分离（普通文本
    // 绝不隐式转成队列任务，session-steering spec）。回执如实：queued+pending，
    // 不冒充已执行（「通道回执不冒充完成」）。
    // 会话定位与 /stop 同口径（流绑定优先，旧 title 回落）；流尚未建会话时
    // 不代建——queue 归属真实会话，避免空流产生孤儿队列项。
    // 权限：入队是普通操作（身份闸门在后续 ③ 统一裁决），此处只解析命令；
    // 空指令给用法提示。子命令 list 先于入队解析（否则 "list" 会被当任务文本）。
    let queue_rest = ctx.text.trim().strip_prefix("/queue").map(str::trim);
    if queue_rest == Some("list") {
        let gid = ch.group.clone().unwrap_or_else(|| core.active_group());
        let title = format!("channel:{}:{}", ch.id, ctx.chat_key);
        let sid = core.flow_active_session(&ch.id, ctx.chat_key).or_else(|| {
            core.list_sessions_in_group(&gid)
                .ok()
                .and_then(|list| list.into_iter().find(|s| s.title == title))
                .map(|s| s.id)
        });
        let Some(sid) = sid else {
            return GateDecision::Deny("【报告】当前聊天流还没有会话，队列自然为空。".into());
        };
        let items: Vec<String> = exm_core::task_queue::list(&sid)
            .into_iter()
            .filter(|t| t.status == "queued")
            .map(|t| format!("{}. {}", t.seq, t.task))
            .collect();
        return GateDecision::Deny(if items.is_empty() {
            "队列为空：没有排队中的后续任务。".into()
        } else {
            format!(
                "📋 后续任务队列（待处理 {} 条）：\n{}",
                items.len(),
                items.join("\n")
            )
        });
    }
    if queue_rest == Some("") {
        return GateDecision::Deny(
            "用法：/queue <指令> —— 把指令排入本聊天流的后续任务队列（当前任务完成后按序执行）；/queue list 查看队列。".into(),
        );
    }
    if let Some(task) = queue_rest.filter(|t| !t.is_empty()) {
        let gid = ch.group.clone().unwrap_or_else(|| core.active_group());
        let title = format!("channel:{}:{}", ch.id, ctx.chat_key);
        let sid = core.flow_active_session(&ch.id, ctx.chat_key).or_else(|| {
            core.list_sessions_in_group(&gid)
                .ok()
                .and_then(|list| list.into_iter().find(|s| s.title == title))
                .map(|s| s.id)
        });
        let Some(sid) = sid else {
            return GateDecision::Deny(
                "【报告】当前聊天流还没有会话，先发送一条普通消息建立上下文后再用 /queue 排队后续任务。".into(),
            );
        };
        // 入站消毒与主链一致：队列文本也会进未来轮次的模型上下文
        let (clean_task, findings) =
            crate::sanitize::sanitize_with(task, Some(&core.config().security.sanitize_rules));
        if !findings.is_empty() {
            crate::sanitize::note_findings(&findings);
            core.emit_sanitize_finding(serde_json::json!({
                "channel": ch.id,
                "externalId": ctx.external_id,
                "patterns": findings.iter().map(|f| f.rule).collect::<Vec<_>>(),
                "actions": findings.iter().map(|f| f.action).collect::<Vec<_>>(),
            }));
        }
        return match exm_core::task_queue::enqueue_scoped(
            &sid,
            &clean_task,
            "queued",
            "",
            "chat",
            None,
        ) {
            Ok(qid) => {
                let items = exm_core::task_queue::list(&sid);
                let seq = items
                    .iter()
                    .find(|t| t.id == qid)
                    .map(|t| t.seq)
                    .unwrap_or(0);
                let queued = items.iter().filter(|t| t.status == "queued").count();
                // 队列快照事件（与 REST 面同口径）：广播 + 落库可回放（3.5
                // 「WS 重连/刷新可从事件恢复队列」对聊天入口同样成立）
                let running = items.iter().filter(|t| t.status == "running").count();
                core.emit_session_event(
                    &sid,
                    "task_queue.updated",
                    serde_json::json!({
                        "action": "add",
                        "items": items,
                        "running": running,
                        "queued": queued,
                    }),
                );
                GateDecision::Deny(format!(
                    "📥 已加入后续队列（第 {seq} 条，待处理 {queued} 条）：当前任务完成后按序执行；可用 /queue list 查询。"
                ))
            }
            Err(e) => GateDecision::Deny(format!("【报告】加入队列失败：{e}")),
        };
    }
    // /queue 不带参数的用法提示已在新 ⓪' 分支统一处理（queue_rest 解析），
    // 此处不再重复。

    // ⓪'' /plan 只读规划（capability-parity 4.4）：聊天入口与 CLI/TUI/WebUI
    // 等价——status 查模式与清单、start <任务> 进计划模式并跑只读轮、approve
    // 逐项回放、reject 保留清单、execute 切回执行模式；裸 /plan 视同 status。
    // 事件面（plan.enabled/disabled/item/approved/rejected/applied）由 core
    // 统一发射（广播+落库），四入口一致。会话定位与 /queue 同口径。
    if let Some(rest) = ctx.text.trim().strip_prefix("/plan").map(str::trim) {
        let gid = ch.group.clone().unwrap_or_else(|| core.active_group());
        let title = format!("channel:{}:{}", ch.id, ctx.chat_key);
        let sid = core.flow_active_session(&ch.id, ctx.chat_key).or_else(|| {
            core.list_sessions_in_group(&gid)
                .ok()
                .and_then(|list| list.into_iter().find(|s| s.title == title))
                .map(|s| s.id)
        });
        let (sub, task) = match rest.split_once(char::is_whitespace) {
            Some((s, t)) => (s, t.trim()),
            None => (rest, ""),
        };
        // approve/reject 是裁决面：仅管理员（与 /stop 同口径——聊天流是共享上下文）
        if matches!(sub, "approve" | "reject") && !is_channel_admin(core, ch, ctx.external_id) {
            if ctx.mentioned {
                return GateDecision::Deny(
                    "【报告】/plan approve|reject 需要管理员权限：计划裁决影响共享会话的执行。"
                        .into(),
                );
            }
            return GateDecision::Ignore;
        }
        let reply = match (sid, sub, task) {
            (None, ..) => {
                "【报告】当前聊天流还没有会话，先发送一条普通消息建立上下文后再用 /plan。"
                    .to_string()
            }
            (Some(sid), "status" | "", _) => {
                let items = core.plan_list(&sid);
                let mode = if exm_core::plan::is_plan_mode(&sid) {
                    "plan（只读）"
                } else {
                    "execute"
                };
                if items.is_empty() {
                    format!("📋 计划模式：{mode}；暂无计划条目。")
                } else {
                    format!(
                        "📋 计划模式：{mode}；待决条目 {} 条：\n{}",
                        items.len(),
                        items
                            .iter()
                            .map(|i| format!("{} [{}] {} → {}", i.id, i.action, i.tool, i.target))
                            .collect::<Vec<_>>()
                            .join("\n")
                    )
                }
            }
            (Some(sid), "start", t) if !t.is_empty() => {
                core.set_plan_mode(&sid, true);
                // 只读规划轮走标准 chat 链路（写工具被转换为 plan.item）
                match core.chat(&sid, t).await {
                    Ok(()) => "📋 已进入计划模式并完成一轮只读规划；用 /plan status 查看条目，/plan approve 回放执行或 /plan reject 保留待改。".to_string(),
                    Err(e) => format!("【报告】规划轮失败：{e}"),
                }
            }
            (Some(sid), "start", _) => {
                core.set_plan_mode(&sid, true);
                "📋 已进入计划模式；后续写操作会转换为计划条目，/plan approve 回放执行。"
                    .to_string()
            }
            (Some(sid), "approve", _) => match core.plan_approve(&sid).await {
                Ok(results) => format!("✅ 已批准并回放 {} 个计划条目。", results.len()),
                Err(e) => format!("【报告】批准失败：{e}"),
            },
            (Some(sid), "reject", _) => {
                let items = core.plan_reject(&sid);
                format!("🚫 已拒绝计划；{} 个条目保留待修改。", items.len())
            }
            (Some(sid), "execute" | "off", _) => {
                core.set_plan_mode(&sid, false);
                "🔁 已切换回执行模式。".to_string()
            }
            (Some(_), other, _) => format!(
                "用法：/plan [status|start <任务>|approve|reject|execute]（未知子命令 `{other}`）"
            ),
        };
        return GateDecision::Deny(reply);
    }

    // ⓪''' /goal 与 /spec 会话工作流命令（capability-parity 4.6）：与 CLI/TUI
    // 等价，命令在 gate 拦截直答——绝不把命令文本交给模型猜测（命令不是对话）。
    // 读面（status/show）普通成员可用；写面（start/edit/pause/resume/complete/
    // block/clear/draft/export）仅管理员（goal/spec 是共享会话的持久工作流状态）。
    // 与 cron（定时任务）/todo 看板（轮内执行进度）/队列（后续任务排队）三者
    // 互不相干：goal/spec 只动 workflow.* 事件，不触碰 cron 表、todo 快照与
    // task_queue。会话定位与 /stop //queue //plan 同口径。
    if let Some(raw) = ctx.text.trim().strip_prefix("/goal") {
        // 未 trim 的剩余：空或以空白开头才属于 /goal（避免误吞 /goalx 之类命令）
        if raw.is_empty() || raw.starts_with(char::is_whitespace) {
            let rest = raw.trim();
            let gid = ch.group.clone().unwrap_or_else(|| core.active_group());
            let title = format!("channel:{}:{}", ch.id, ctx.chat_key);
            let sid = core.flow_active_session(&ch.id, ctx.chat_key).or_else(|| {
                core.list_sessions_in_group(&gid)
                    .ok()
                    .and_then(|list| list.into_iter().find(|s| s.title == title))
                    .map(|s| s.id)
            });
            let (sub, arg) = match rest.split_once(char::is_whitespace) {
                Some((s, a)) => (s, a.trim()),
                None => (rest, ""),
            };
            let sub = if sub.is_empty() { "status" } else { sub };
            // 写面仅管理员；未 @ 静默忽略（与 /stop //plan approve 同口径）。白名单判权：
            // 只对确知的写子命令判权——打错的子命令不是写操作，走用法提示。
            if matches!(
                sub,
                "start" | "edit" | "pause" | "resume" | "complete" | "block" | "clear"
            ) && !is_channel_admin(core, ch, ctx.external_id)
            {
                if ctx.mentioned {
                    return GateDecision::Deny(
                        "【报告】/goal 写操作需要管理员权限：goal 是共享会话的持久目标状态。"
                            .into(),
                    );
                }
                return GateDecision::Ignore;
            }
            let reply = match sid {
                None => "【报告】当前聊天流还没有会话，先发送一条普通消息建立上下文后再用 /goal。"
                    .to_string(),
                Some(sid) => {
                    let current_revision = core.goal_get(&sid).ok().flatten().map(|g| g.revision);
                    let outcome = match sub {
                        "status" | "show" => {
                            return GateDecision::Deny(match core.goal_get(&sid) {
                                Ok(Some(g)) => format!(
                                    "🎯 goal {}（rev {}）\n目标：{}\n验收：{}\n验证：{}",
                                    format!("{:?}", g.status).to_lowercase(),
                                    g.revision,
                                    g.objective,
                                    if g.acceptance.is_empty() {
                                        "（未声明）".into()
                                    } else {
                                        g.acceptance.join("；")
                                    },
                                    if g.verification.is_empty() {
                                        "（未验证）".into()
                                    } else {
                                        g.verification.join("；")
                                    },
                                ),
                                Ok(None) => {
                                    "🎯 当前会话没有 goal；用 /goal start <目标> 创建。".into()
                                }
                                Err(e) => format!("【报告】读取 goal 失败：{e}"),
                            });
                        }
                        "start" if !arg.is_empty() => {
                            core.goal_update(&sid, "start", Some(arg), None, None, None)
                        }
                        "edit" if !arg.is_empty() => {
                            core.goal_update(&sid, "edit", Some(arg), None, None, current_revision)
                        }
                        "pause" => {
                            core.goal_update(&sid, "pause", None, None, None, current_revision)
                        }
                        "resume" => {
                            core.goal_update(&sid, "resume", None, None, None, current_revision)
                        }
                        "complete" if !arg.is_empty() => core.goal_update(
                            &sid,
                            "complete",
                            None,
                            None,
                            Some(arg),
                            current_revision,
                        ),
                        "block" if !arg.is_empty() => {
                            core.goal_update(&sid, "block", None, Some(arg), None, current_revision)
                        }
                        "block" => core.goal_update(
                            &sid,
                            "block",
                            None,
                            Some("聊天通道手动受阻"),
                            None,
                            current_revision,
                        ),
                        "clear" => {
                            core.goal_update(&sid, "clear", None, None, None, current_revision)
                        }
                        _ => {
                            return GateDecision::Deny(
                            "用法：/goal [status|start <目标>|edit <目标>|pause|resume|complete <验证证据>|block [原因]|clear]（complete 必须带真实验证证据）".into(),
                        );
                        }
                    };
                    match outcome {
                        Ok(g) => format!(
                            "🎯 goal {}（rev {}）：{}",
                            format!("{:?}", g.status).to_lowercase(),
                            g.revision,
                            g.objective
                        ),
                        Err(e) => format!("【报告】goal 操作失败：{e}"),
                    }
                }
            };
            return GateDecision::Deny(reply);
        }
    }

    if let Some(raw) = ctx.text.trim().strip_prefix("/context") {
        // 未 trim 的剩余：空或以空白开头才属于 /context（避免误吞 /contextx）
        if raw.is_empty() || raw.starts_with(char::is_whitespace) {
            let rest = raw.trim();
            let gid = ch.group.clone().unwrap_or_else(|| core.active_group());
            let title = format!("channel:{}:{}", ch.id, ctx.chat_key);
            let sid = core.flow_active_session(&ch.id, ctx.chat_key).or_else(|| {
                core.list_sessions_in_group(&gid)
                    .ok()
                    .and_then(|list| list.into_iter().find(|s| s.title == title))
                    .map(|s| s.id)
            });
            let (sub, arg) = match rest.split_once(char::is_whitespace) {
                Some((s, a)) => (s, a.trim()),
                None => (rest, ""),
            };
            let sub = if sub.is_empty() { "status" } else { sub };
            // 写面仅管理员（与 /plan 同口径——**白名单**：只对确知的写子命令判权）。
            // 不能用「非只读即写」的黑名单：那会把打错的子命令当成写操作，非管理员
            // 得到「需要管理员权限」——像是权限问题，其实是用法错误，还暗示存在
            // 这样一个子命令。未知子命令走下面的用法提示（对所有人一致）。
            if matches!(sub, "disable" | "enable") && !is_channel_admin(core, ch, ctx.external_id) {
                if ctx.mentioned {
                    return GateDecision::Deny(
                        "【报告】/context disable|enable 需要管理员权限：来源禁用影响共享会话的上下文注入。".into(),
                    );
                }
                return GateDecision::Ignore;
            }
            let reply = match sid {
                None => {
                    "【报告】当前聊天流还没有会话，先发送一条普通消息建立上下文后再用 /context。"
                        .to_string()
                }
                Some(sid) => {
                    match sub {
                        "status" | "show" | "list" => render_context_sources(core, &sid),
                        // detail：单个来源的状态与缺失原因（验收「各来源及缺失原因」）
                        "detail" if !arg.is_empty() => render_context_detail(core, &sid, arg),
                        "disable" if !arg.is_empty() => apply_context_source(core, &sid, arg, true),
                        "enable" if !arg.is_empty() => apply_context_source(core, &sid, arg, false),
                        // refresh：带 key 只重读该来源（失败只影响它自己）；不带 key 重读整表
                        "refresh" if !arg.is_empty() => refresh_context_source(core, &sid, arg),
                        "refresh" => render_context_sources(core, &sid),
                        _ => {
                            return GateDecision::Deny(
                                "用法：/context [status|detail <key>|disable <key>|enable <key>|refresh [key]]（key 形如 file:AGENTS.md / memory / retrieval / skills）".into(),
                            );
                        }
                    }
                }
            };
            return GateDecision::Deny(reply);
        }
    }

    if let Some(raw) = ctx.text.trim().strip_prefix("/learn") {
        // 未 trim 的剩余：空或以空白开头才属于 /learn
        if raw.is_empty() || raw.starts_with(char::is_whitespace) {
            let rest = raw.trim();
            let (sub, arg) = match rest.split_once(char::is_whitespace) {
                Some((s, a)) => (s, a.trim()),
                None => (rest, ""),
            };
            let sub = if sub.is_empty() { "list" } else { sub };
            // 裁决是治理写面：仅管理员（候选转正影响共享技能/记忆）。
            // 与 /context 同口径——白名单判权：打错的子命令不是写操作，走用法提示。
            if matches!(sub, "approve" | "reject") && !is_channel_admin(core, ch, ctx.external_id) {
                if ctx.mentioned {
                    return GateDecision::Deny(
                        "【报告】/learn approve|reject 需要管理员权限：候选转正影响共享技能与记忆。".into(),
                    );
                }
                return GateDecision::Ignore;
            }
            let reply = match sub {
                "list" | "show" | "status" => render_learning_candidates(core, arg),
                "approve" if !arg.is_empty() => {
                    decide_learning(core, arg, true, ctx.external_id, None)
                }
                "reject" if arg.is_empty() => {
                    return GateDecision::Deny(
                        "用法：/learn reject <id> <原因>（拒绝建议带原因）".into(),
                    );
                }
                "reject" => {
                    let (id, reason) = match arg.split_once(char::is_whitespace) {
                        Some((i, r)) => (i.trim(), r.trim()),
                        None => (arg, ""),
                    };
                    let reason = if reason.is_empty() {
                        None
                    } else {
                        Some(reason)
                    };
                    decide_learning(core, id, false, ctx.external_id, reason)
                }
                _ => {
                    return GateDecision::Deny(
                        "用法：/learn [list [pending|approved|rejected]|approve <id>|reject <id> <原因>]（候选提交由智能体在任务后经证据校验发起）".into(),
                    );
                }
            };
            return GateDecision::Deny(reply);
        }
    }

    if let Some(raw) = ctx.text.trim().strip_prefix("/spec") {
        // 未 trim 的剩余：空或以空白开头才属于 /spec（避免误吞 /specx 之类命令）
        if raw.is_empty() || raw.starts_with(char::is_whitespace) {
            let rest = raw.trim();
            let gid = ch.group.clone().unwrap_or_else(|| core.active_group());
            let title = format!("channel:{}:{}", ch.id, ctx.chat_key);
            let sid = core.flow_active_session(&ch.id, ctx.chat_key).or_else(|| {
                core.list_sessions_in_group(&gid)
                    .ok()
                    .and_then(|list| list.into_iter().find(|s| s.title == title))
                    .map(|s| s.id)
            });
            let (sub, arg) = match rest.split_once(char::is_whitespace) {
                Some((s, a)) => (s, a.trim()),
                None => (rest, ""),
            };
            let sub = if sub.is_empty() { "show" } else { sub };
            // 写面仅管理员；未 @ 静默忽略。白名单判权（与 /goal 同口径）：`export`
            // 是只读的口径说明（见下），不在写面之内。
            if matches!(sub, "draft" | "edit" | "clear")
                && !is_channel_admin(core, ch, ctx.external_id)
            {
                if ctx.mentioned {
                    return GateDecision::Deny(
                        "【报告】/spec 写操作需要管理员权限：spec 是共享会话的编程规格状态。"
                            .into(),
                    );
                }
                return GateDecision::Ignore;
            }
            let sid =
                match sid {
                    Some(sid) => sid,
                    None => return GateDecision::Deny(
                        "【报告】当前聊天流还没有会话，先发送一条普通消息建立上下文后再用 /spec。"
                            .into(),
                    ),
                };
            let reply = match sub {
            "status" | "show" => match core.spec_get(&sid) {
                Ok(Some(s)) => format!(
                    "📄 spec（rev {}，关联 goal {}）\n目标：{}\n范围：{}\n约束：{}\n验收：{}\n来源：{}",
                    s.revision,
                    s.goal_id.as_deref().unwrap_or("未关联"),
                    s.objective,
                    if s.scope.is_empty() { "（未声明）".into() } else { s.scope.join("、") },
                    if s.constraints.is_empty() { "（未声明）".into() } else { s.constraints.join("；") },
                    if s.acceptance.is_empty() { "（未声明）".into() } else { s.acceptance.join("；") },
                    s.source,
                ),
                Ok(None) => "📄 当前会话没有 spec；用 /spec draft <目标> 草拟。".into(),
                Err(e) => format!("【报告】读取 spec 失败：{e}"),
            },
            "draft" | "edit" if !arg.is_empty() => {
                let expected = core.spec_get(&sid).ok().flatten().map(|s| s.revision);
                match core.spec_update(&sid, arg, vec![], vec![], vec![], vec![], expected) {
                    Ok(s) => format!("📄 spec 已{}（rev {}）：{}", if sub == "draft" { "草拟" } else { "修订" }, s.revision, s.objective),
                    Err(e) => format!("【报告】spec 操作失败：{e}"),
                }
            }
            "clear" => {
                let expected = core.spec_get(&sid).ok().flatten().map(|s| s.revision);
                match core.spec_clear(&sid, expected) {
                    Ok(()) => "📄 spec 已清除（事实保留在事件轨迹）。".into(),
                    Err(e) => format!("【报告】spec 清除失败：{e}"),
                }
            }
            // 显式导出涉及工作区写——聊天通道不开放（用 CLI/WebUI 显式操作），
            // 这里给出口径说明而不是静默不支持。
            "export" => "【报告】/spec export 涉及工作区写入，请在 CLI（exm spec export）或 WebUI 操作；聊天通道只做会话内草拟/修订/清除。".into(),
            _ => {
                "用法：/spec [show|draft <目标>|edit <目标>|clear]（修改不自动批准 plan；导出走 CLI/WebUI）".into()
            }
        };
            return GateDecision::Deny(reply);
        }
    }

    // ① 配对码兑换（控制台签发短码 → 通道内 /pair <码> 绑定；单次有效、限期）
    // 防爆破（inbound-sanitization）：单通道单账号连续失败 N 次（默认 5）进入锁定
    // 窗口（默认 15 分钟）——锁定期内正确码也拒绝并提示剩余时间；锁定有审计事件
    let trimmed = ctx.text.trim();
    if let Some(code) = trimmed
        .strip_prefix("/pair ")
        .map(str::trim)
        .filter(|c| !c.is_empty())
    {
        let idcfg = &core.config().identity;
        let pkey = format!("pair:{}:{}", ch.id, ctx.external_id);
        let now = exm_core::types::now_ms();
        let lockout_ms = idcfg.pairing_lockout_secs * 1000;
        let already = crate::limits::RateLimiter::count(&pkey, lockout_ms, now);
        if idcfg.pairing_max_failures > 0 && already >= idcfg.pairing_max_failures as usize {
            core.emit_sanitize_finding(serde_json::json!({
                "kind": "pairing.lockout",
                "channel": ch.id,
                "externalId": ctx.external_id,
                "failures": already,
                "lockoutRemainSecs": lockout_ms / 1000,
            }));
            return GateDecision::Deny(format!(
                "【报告】尝试过于频繁：已锁定，请约 {} 分钟后再试。",
                lockout_ms / 60000
            ));
        }
        return match core.redeem_pairing(code, &ch.id, ctx.external_id, ctx.display_name, "member")
        {
            Ok(idn) => {
                // 兑换成功清空失败计数（成功后不应残留历史失败导致误锁）
                crate::limits::RateLimiter::reset(&pkey);
                GateDecision::Deny(format!(
                    "【报告】绑定成功：{}（角色 {}）。现在可以正常对话。",
                    idn.display_name, idn.role
                ))
            }
            Err(e) => {
                let allowed = crate::limits::RateLimiter::check(
                    &pkey,
                    lockout_ms,
                    idcfg.pairing_max_failures.max(1),
                    now,
                );
                if !allowed {
                    core.emit_sanitize_finding(serde_json::json!({
                        "kind": "pairing.lockout",
                        "channel": ch.id,
                        "externalId": ctx.external_id,
                        "failures": idcfg.pairing_max_failures,
                        "lockoutRemainSecs": lockout_ms / 1000,
                    }));
                    GateDecision::Deny(format!(
                        "【报告】连续失败 {} 次，已锁定约 {} 分钟：期间即使码正确也不受理。",
                        idcfg.pairing_max_failures,
                        lockout_ms / 60000
                    ))
                } else {
                    GateDecision::Deny(format!("【报告】绑定失败：{e}"))
                }
            }
        };
    }

    // ①' 聊天流（chat flow）会话管理：一个群 / 一个私聊各是一条流，流固定复用
    // 一个活跃会话；不切换就一直在同一会话里聊，跨轮上下文自然延续。
    if let Some(reply) = flow_command(core, ch, ctx).await {
        return GateDecision::Deny(reply);
    }

    // ② 群聊唤醒门控：群内仅提及 / 回复触发；私聊豁免。
    // 提及必回（mention_reply，默认开）：@ 机器人本体 / 引用回复机器人消息
    // 属「明确点名」，门控直接放行（不经主动回复漏斗）。关闭后提及消息
    // 同样进漏斗判断（纯陪聊场景：连 @ 也可被决策模型按语境拒掉）。
    let mut proactive_pass = false;
    if ch.gate_enabled(core) && ctx.is_group && ctx.mentioned && ch.mention_reply_enabled() {
        // 放行：直接落到后续闸门（身份/限流），不进漏斗
    } else if ch.gate_enabled(core) && ctx.is_group {
        // 主动回复（实验性，通道级开关）：未 @（或提及必回已关）时经漏斗
        // （概率软权重 → 探活词表 → 决策模型）判断是否主动回应
        // （proactive.rs；决策失败保守忽略——主动回复是增益不是义务）。
        // 被拦下的消息不再完全丢弃：落流会话作为「旁听」上下文（imctx: 标记，
        // 不触发轮次）——「仅 @ 回复」模式下 Agent 被点名时仍能接住群聊话题。
        let proactive = ch.proactive_enabled()
            && super::proactive::should_proactively_reply(core, ch, ctx).await;
        if !proactive {
            record_ambient(core, ch, ctx).await;
            return GateDecision::Ignore;
        }
        proactive_pass = true;
    }

    // ③ 身份管控：开启后未绑定用户拒绝执行并给配对指引。管理者账号清单命中豁免——
    // 按外部用户 ID 跨通道匹配（人在任一通道的管理者清单即凭证；判权跟人走，
    // 用户可能在此通道说话、经别的通道执行动作，故不限于本通道清单）
    if identity_cfg.identity_required
        && core.identity_of(&ch.id, ctx.external_id).is_none()
        && !load_channels(core)
            .iter()
            .any(|c| c.admins.iter().any(|a| a.trim() == ctx.external_id))
    {
        return GateDecision::Deny(
            "【报告】未绑定身份：请在控制台「通道身份」页签发配对码，然后在此发送 /pair <配对码> 完成绑定。".into(),
        );
    }

    // ④ 用量治理：通道主体滑动窗口限流 + 周期请求数配额（仅外部入口；组内派发不经此处）
    let limits = &core.config().limits;
    if limits.enabled {
        let subject = match core.identity_of(&ch.id, ctx.external_id) {
            Some(i) => format!("user:{}", i.id),
            None => format!("ch:{}:{}", ch.id, ctx.external_id),
        };
        let now = exm_core::types::now_ms();
        if limits.max_requests > 0
            && !crate::limits::RateLimiter::check(
                &format!("ch:{subject}"),
                limits.window_secs * 1000,
                limits.max_requests,
                now,
            )
        {
            return GateDecision::Deny(format!(
                "【报告】请求过于频繁：受用量限制，请在 {} 秒后重试。",
                limits.window_secs
            ));
        }
        if limits.quota_requests > 0 {
            let (qkey, qwin) = crate::limits::quota_key(&subject, &limits.quota_period, now);
            if !crate::limits::RateLimiter::check(&qkey, qwin, limits.quota_requests, now) {
                return GateDecision::Deny(
                    "【报告】周期请求数配额已耗尽，请等待周期刷新或联系管理员调整配额。".into(),
                );
            }
        }
    }
    // ⑤ 入站消毒（inbound-sanitization）：身份判定之后、放行之前——
    // 检出注入模式即中性化（Finding 仅审计不阻断）。此时流尚未绑定会话
    //（ChannelRun::begin 才发生），审计事件 payload 内嵌通道/账号定位；
    // session_id 留空走广播（WS 实时可见），不落事件库（无所属会话可挂）。
    // 规则开关随 config（security.sanitizeRules 单项可关）
    let (clean_text, findings) =
        crate::sanitize::sanitize_with(ctx.text, Some(&core.config().security.sanitize_rules));
    if !findings.is_empty() {
        crate::sanitize::note_findings(&findings);
        core.emit_sanitize_finding(serde_json::json!({
            "channel": ch.id,
            "externalId": ctx.external_id,
            "patterns": findings.iter().map(|f| f.rule).collect::<Vec<_>>(),
            "actions": findings.iter().map(|f| f.action).collect::<Vec<_>>(),
        }));
    }
    GateDecision::Allow(clean_text, proactive_pass)
}

/// 群聊旁听落库（ambient context）：门控拦下的未 @ 群消息以 `imctx:` 标记
/// 追加到流会话——不触发轮次，仅作为「仅 @ 回复」模式下被点名时的群聊上下文。
///
/// 语义约定（docs/协议与契约.md）：
/// - 落 User 角色但标记 `imctx:发送者`，与真实输入（`im:`）区分；
/// - collect 不并入 goal（群聊旁听是背景，不是本轮任务输入）；
/// - 模型侧渲染「群聊·发送者」，展示层署名 + 「群聊」标签；
/// - 文本仍经入站消毒——旁听会进入未来轮次的模型上下文，不能成为注入面。
///
/// 媒体附件（ambient-media-attach）：适配器在闸门前已把旁听媒体下载落盘
/// （napcat 旁听窗口等），此处把附件元数据随占位消息回填——部署实测缺陷：
/// 群里未 @ 的纯图走旁听路径，图片文件落了盘（inbox 有文件）、占位文本
/// 落了库（「（用户发来附件：图片 x1）」），但附件元数据从未随消息存储，
/// 聊天流里永远看不到图。只回填**本条消息自带**的媒体（`ctx.media`）；
/// 窗口条目不 take（TTL 窗口仍供后续 @ 轮次取走注入模型）。
pub(crate) async fn record_ambient(core: &exm_core::Core, ch: &Channel, ctx: &InboundCtx<'_>) {
    let text = ctx.text.trim();
    if text.is_empty() {
        return;
    }
    let Some(sid) = ensure_flow_session(core, ch, ctx.chat_key, ctx.flow_name, ctx.is_group) else {
        return;
    };
    let (clean, findings) =
        crate::sanitize::sanitize_with(text, Some(&core.config().security.sanitize_rules));
    if !findings.is_empty() {
        crate::sanitize::note_findings(&findings);
        core.emit_sanitize_finding(serde_json::json!({
            "channel": ch.id,
            "externalId": ctx.external_id,
            "patterns": findings.iter().map(|f| f.rule).collect::<Vec<_>>(),
            "actions": findings.iter().map(|f| f.action).collect::<Vec<_>>(),
            "ambient": true,
        }));
    }
    // 发送者标记沿用六适配器同口径（im:名(id) → imctx:名(id)）；
    // 无署名时仍保留 imctx: 前缀（前缀本身就是旁听语义，不可丢）
    let sender =
        crate::channel_util::sender_origin(ctx.is_group, ctx.display_name, ctx.external_id)
            .map(|m| m.trim_start_matches("im:").to_string())
            .unwrap_or_default();
    let origin = format!("imctx:{sender}");
    let msg = core.store.add_message(
        &sid,
        exm_core::types::MessageRole::User,
        Some(origin.as_str()),
        vec![exm_core::types::Statement::new(
            exm_core::types::SpeechTag::要求,
            clean.as_str(),
        )],
    );
    // 旁听媒体随占位消息回填展示层元数据（不注入模型——旁听不触发轮次；
    // 模型注入走 @ 轮次 take 旁听媒体窗口的既有链路）。
    // 只挂本条消息自带的媒体（ctx.media，napcat 闸门前预下载携带）——
    // 曾用全窗快照回填：窗口内每条新旁听消息（含纯文本）都复读全部近期
    // 图片，聊天流里同图在每条消息上重复出现（部署实测缺陷）。
    if let Ok(m) = &msg {
        let rels: Vec<exm_core::types::InboundAttachment> = ctx
            .media
            .iter()
            .map(|a| {
                let rel = std::path::Path::new(&a.1)
                    .strip_prefix(&core.config().workspace_root)
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|_| a.1.clone());
                exm_core::types::InboundAttachment {
                    kind: a.0.clone(),
                    path: rel,
                    name: a.2.clone(),
                }
            })
            .collect();
        if !rels.is_empty() {
            core.store.attach_inbound(&sid, &m.id, rels);
        }
    }
    // 流预览跟随最新群消息（控制台流列表展示「群里刚说了什么」）
    let _ = core.touch_flow_message(&ch.id, ctx.chat_key, &clean);
}

/// 流会话定位/建立（旁听用，与 `ChannelRun::begin` 同口径）：
/// 有效绑定优先；未绑定则按 title 认领旧会话（升级迁移）或新建并回写绑定。
/// 不切全局交互上下文——旁听只是记账，不应翻转打开中的工作目标。
fn ensure_flow_session(
    core: &exm_core::Core,
    ch: &Channel,
    chat_key: &str,
    flow_name: Option<&str>,
    is_group: bool,
) -> Option<String> {
    if let Some(sid) = core.flow_active_session(&ch.id, chat_key) {
        // 流名回填（空填不覆盖）：流存在但名为空时补（begin_named 同口径）
        if let Some(n) = flow_name.filter(|n| !n.trim().is_empty()) {
            core.ensure_flow_named(&ch.id, chat_key, n, is_group);
        }
        return Some(sid);
    }
    let target = ch
        .group
        .clone()
        .unwrap_or_else(|| format!("{SINGLE_PREFIX}{}", default_single_id(core)));
    let (single, group) = parse_target(&target);
    let scope = match (single, group) {
        (Some(id), _) => format!("{SINGLE_PREFIX}{id}"),
        (_, Some(g)) => g,
        _ => core.active_group(),
    };
    let title = format!("channel:{}:{chat_key}", ch.id);
    // 原子版（竞态 P1-3）：查绑定→建会话→回写绑定整体临界区——
    // 并发同流消息不再各建会话（孤儿+绑定互抹）
    core.ensure_flow_session_atomic(&ch.id, chat_key, &scope, &title, flow_name, is_group)
}

/// 聊天流（chat flow）固定单会话：一条流恒定复用同一个会话，不再有流内
/// 会话区分（`/new` `/sessions` `/use` 已退役——多会话切换单独治理记忆与
/// 上下文，与「聊天流 = 一个外部会话的镜像」的定位冲突；需要全新上下文
/// 时由管理者在控制台归档/重建，通道侧不提供切换入口）。
///
/// 旧指令仍拦截并回执说明（群里发了不能没反应），但不做任何状态变更。
/// 返回 Some(reply) = 指令已处理（Deny 语义回帖）；None = 非流指令，放行后续闸门。
async fn flow_command(core: &exm_core::Core, ch: &Channel, ctx: &InboundCtx<'_>) -> Option<String> {
    let text = ctx.text.trim();
    // /reset：重置聊天上下文——旧会话归档保留（控制台归档区可回看），流绑定
    // 切到新会话，后续消息从零开始。「聊天流固定单会话」退役了 /new /sessions
    // /use，但「干净重开」是真实高频诉求（上下文被长对话灌满 / 话题彻底换轨），
    // 只靠控制台手动归档对通道侧用户不可达。
    // 权限：仅管理员（通道 admins 清单或绑定身份 role=admin）——群聊流是共享
    // 上下文，任何成员都能归档会话/中止运行任务 = 一个人手滑全班陪葬。
    // 非管理员静默忽略（不回执，避免「权限拒绝」刷屏）；@ 场景（mentioned）
    // 给拒绝回执——被点名不回应是失礼，说明原因即可。
    if text == "/reset" {
        if !is_channel_admin(core, ch, ctx.external_id) {
            if ctx.mentioned {
                return Some(
                    "【报告】/reset 需要管理员权限：聊天流是共享上下文，重置影响所有人（会话归档 + 运行任务中止）。请联系管理员操作。".into(),
                );
            }
            return None; // 未 @ 的非管理员：静默忽略（不进后续闸门，也不落旁听噪音）
        }
        return Some(reset_flow_session(core, ch, ctx).await);
    }
    let retired = [
        (
            "/new",
            "本聊天流固定使用一个会话，不再开新会话。如需干净重开请发 /reset（旧对话归档保留）。",
        ),
        ("/sessions", "本聊天流固定使用一个会话，无流内会话列表。"),
        ("/use", "本聊天流固定使用一个会话，无需切换。"),
    ];
    for (cmd, reply) in retired {
        if text == cmd || text.starts_with(&format!("{cmd} ")) {
            // 附当前绑定会话短码，管理者可在控制台按此定位
            let cur = core
                .flow_active_session(&ch.id, ctx.chat_key)
                .map(|sid| sid.chars().take(8).collect::<String>())
                .unwrap_or_else(|| "（尚未创建）".into());
            return Some(format!("{reply}当前会话：{cur}"));
        }
    }
    None
}

/// 学习候选清单渲染（capability-parity 6.4 /learn list）：
/// 按状态过滤（缺省 pending——待裁决的最相关）。
fn render_learning_candidates(core: &exm_core::Core, status_arg: &str) -> String {
    let status = match status_arg {
        "pending" | "approved" | "rejected" => Some(status_arg),
        _ => None,
    };
    match core.learning_candidates(status) {
        Ok(list) if list.is_empty() => {
            "🎓 当前没有待处理的学习候选（智能体任务后凭真实证据自动提交）。".into()
        }
        Ok(list) => {
            let lines: Vec<String> = list
                .iter()
                .take(10)
                .map(|c| {
                    let evidence: Vec<String> = c
                        .evidence
                        .iter()
                        .map(|e| format!("{}·{}", e.kind.key(), e.verification))
                        .collect();
                    format!(
                        "• [{}] {}（{}/{}，置信 {:.0}%，{} 条证据：{}）\n  {}",
                        c.status,
                        c.title,
                        c.target,
                        c.scope,
                        c.confidence * 100.0,
                        c.evidence.len(),
                        evidence.join("、"),
                        c.id
                    )
                })
                .collect();
            format!(
                "🎓 学习候选（{} 项）：\n{}\n裁决：/learn approve <id> / /learn reject <id> <原因>",
                list.len(),
                lines.join("\n")
            )
        }
        Err(e) => format!("【报告】读取学习候选失败：{e}"),
    }
}

/// 学习候选裁决（capability-parity 6.4 / 6.6）：approve/reject 一次定局。
/// 批准回执带转正落点与版本号——通道治理操作与 REST/CLI/WebUI 同一口径，
/// 版本事实源只有一条（技能/记忆版本链），这里只是把它如实转述。
fn decide_learning(
    core: &exm_core::Core,
    id: &str,
    approve: bool,
    decided_by: &str,
    reason: Option<&str>,
) -> String {
    let verb = if approve { "批准" } else { "拒绝" };
    match core.learning_decide(id, approve, decided_by, reason) {
        Ok(c) => {
            // 转正失败不吞：批准 ≠ 生效，用户必须看到真实落点
            if let Some(p) = &c.promotion {
                if !p.ok {
                    return format!(
                        "🎓 已{verb}候选「{}」（{}），但转正失败：{}",
                        c.title,
                        c.id,
                        p.error.as_deref().unwrap_or("未知原因")
                    );
                }
                let v = p
                    .version
                    .map(|n| format!(" v{n}"))
                    .unwrap_or_else(String::new);
                return format!(
                    "🎓 已{verb}候选「{}」（{}）：已写入 {}{v}，后续治理走对应版本链",
                    c.title, c.id, p.ref_id
                );
            }
            format!(
                "🎓 已{verb}候选「{}」（{}）：转正与版本化由技能/记忆写入链承接",
                c.title, c.id
            )
        }
        Err(e) => format!("【报告】{verb}候选失败：{e}"),
    }
}

/// 层级的通道可读名称。API/事件里是 snake_case 外显名（project / user），照搬到
/// 中文回执里会夹生；只在本渲染层映射，不动契约。
fn layer_label(layer: &str) -> &'static str {
    match layer {
        "project" => "项目",
        "user" => "用户",
        "agent_prompt" => "智能体提示",
        "system_safety" => "系统安全",
        _ => "未知",
    }
}

/// 一条来源的可读状态：禁用时带上「原本是什么状态」，未就绪时带上原因。
fn context_source_line(s: &serde_json::Value) -> String {
    let kind = s.get("kind").and_then(|v| v.as_str()).unwrap_or("?");
    let key = s.get("key").and_then(|v| v.as_str()).unwrap_or("?");
    let status = s.get("status").and_then(|v| v.as_str()).unwrap_or("?");
    let layer = s
        .get("layer")
        .and_then(|v| v.as_str())
        .map(|l| format!("（{} 层）", layer_label(l)))
        .unwrap_or_default();
    // 禁用是叠加位：原状态留在 baseStatus，用户要能看出「启用后会是什么」
    let base = s
        .get("baseStatus")
        .and_then(|v| v.as_str())
        .filter(|b| *b != status)
        .map(|b| format!("，原状态 {b}"))
        .unwrap_or_default();
    let chars = s
        .get("chars")
        .and_then(|v| v.as_u64())
        .map(|c| format!("，{c} 字"))
        .unwrap_or_default();
    let reason = s
        .get("reason")
        .and_then(|v| v.as_str())
        .map(|r| format!("：{r}"))
        .unwrap_or_default();
    let modified = s
        .get("modifiedAt")
        .and_then(|v| v.as_u64())
        .filter(|t| *t > 0)
        .and_then(fmt_modified)
        .map(|t| format!("，修改于 {t}"))
        .unwrap_or_default();
    format!("• [{kind}] {key}{layer} — {status}{base}{chars}{modified}{reason}")
}

/// Unix 秒 → 本地可读时间（`%Y-%m-%d %H:%M`）。0 / 解析不了返回 None：
/// 不打印一个假的「1970-01-01」。
fn fmt_modified(secs: u64) -> Option<String> {
    chrono::DateTime::from_timestamp(secs as i64, 0).map(|dt| {
        dt.with_timezone(&chrono::Local)
            .format("%Y-%m-%d %H:%M")
            .to_string()
    })
}

/// 上下文来源清单渲染（capability-parity 6.3 /context status）：
/// 文件/记忆/检索/技能四类，含状态、禁用位与缺失原因——「用户可查看各来源及缺失原因」。
///
/// 整层缺席的文件来源（未绑定项目 / 项目目录不可用 / 没有 ~/.exmachina）不在清单里，
/// 由 Core 给出原因附在末尾——这里不猜「为什么没有文件来源」。
fn render_context_sources(core: &exm_core::Core, sid: &str) -> String {
    let sources = match core.context_sources(sid) {
        Ok(s) => s,
        Err(e) => return format!("【报告】读取上下文来源失败：{e}"),
    };
    // 清单已拿到：缺席说明取不到就不附（不为一句说明让整张清单失败），也不编造
    let gaps = core.context_source_gaps(sid).unwrap_or_default();
    let gap_note = context_gaps_note(&gaps);
    if sources.is_empty() {
        return format!("📦 当前会话没有可列出的上下文来源。{gap_note}");
    }
    let lines: Vec<String> = sources.iter().map(context_source_line).collect();
    let disabled = sources
        .iter()
        .filter(|s| s.get("disabled").and_then(|v| v.as_bool()) == Some(true))
        .count();
    let note = if disabled == 0 {
        String::new()
    } else {
        format!("\n（{disabled} 项已停用自动注入——停用只管自动注入，模型主动的记忆召回与用户显式 /skill <id> 不受影响）")
    };
    format!(
        "📦 上下文来源（{} 项）：\n{}{note}{gap_note}\n操作：/context detail <key> 看单项、/context disable <key> 停用自动注入、/context enable <key> 恢复、/context refresh [key] 重读文件",
        sources.len(),
        lines.join("\n")
    )
}

/// 缺席说明的呈现段（Core 给出的原因原样呈现；没有缺席就没有这一段）。
fn context_gaps_note(gaps: &[serde_json::Value]) -> String {
    if gaps.is_empty() {
        return String::new();
    }
    let lines: Vec<String> = gaps
        .iter()
        .map(|g| {
            let layer = layer_label(g.get("layer").and_then(|v| v.as_str()).unwrap_or("未知"));
            format!(
                "· {layer}层：{}",
                g.get("reason")
                    .and_then(|v| v.as_str())
                    .unwrap_or("（未给出原因）")
            )
        })
        .collect();
    format!(
        "\n未加载的文件来源（说明，不是可操作的来源）：\n{}",
        lines.join("\n")
    )
}

/// 清单里找一项；找不到时给出可用键（detail / refresh 共用同一口径：
/// 未知键是**用法错误**，不是「读取失败」——不能包装成刷新失败）。
fn find_context_source<'a>(
    sources: &'a [serde_json::Value],
    key: &str,
) -> Result<&'a serde_json::Value, String> {
    sources
        .iter()
        .find(|s| s.get("key").and_then(|v| v.as_str()) == Some(key))
        .ok_or_else(|| {
            let keys: Vec<String> = sources
                .iter()
                .filter_map(|s| s.get("key").and_then(|v| v.as_str()).map(str::to_string))
                .collect();
            if keys.is_empty() {
                "当前会话没有任何上下文来源（无项目上下文文件）".to_string()
            } else {
                format!("可用：{}", keys.join(" / "))
            }
        })
}

/// 单个来源的详情（/context detail <key>）：状态、禁用位、原状态、缺失原因、
/// 修改时间与字数（文件来源）。未知 key 给出可用键。
fn render_context_detail(core: &exm_core::Core, sid: &str, key: &str) -> String {
    let key = key.trim();
    let sources = match core.context_sources(sid) {
        Ok(s) => s,
        Err(e) => return format!("【报告】读取上下文来源失败：{e}"),
    };
    match find_context_source(&sources, key) {
        Ok(s) => {
            let mut lines = vec![context_source_line(s)];
            match s.get("disabled").and_then(|v| v.as_bool()) {
                Some(true) => lines.push(
                    "  自动注入已停用（下一轮对话起不再注入）；/context enable 可恢复。模型主动的记忆召回与用户显式 /skill <id> 不受影响。".into(),
                ),
                _ => lines.push(
                    "  自动注入开启中（下一轮对话按预算注入）；/context disable 可停用自动注入。".into(),
                ),
            }
            if s.get("status").and_then(|v| v.as_str()) == Some("disabled") {
                // 已停用时也如实提示原状态：启用后它可能仍是 missing / 未就绪
                let base = s.get("baseStatus").and_then(|v| v.as_str()).unwrap_or("?");
                let why = s.get("reason").and_then(|v| v.as_str()).unwrap_or("");
                lines.push(format!(
                    "  恢复后回到原状态 {base}{}",
                    if why.is_empty() {
                        String::new()
                    } else {
                        format!("（{why}）")
                    }
                ));
            }
            format!("📦 来源详情：\n{}", lines.join("\n"))
        }
        Err(hint) => format!("【报告】没有上下文来源 {key}。{hint}"),
    }
}

/// 单来源刷新（/context refresh <key>）：如实回报该来源重读的结果。失败只体现在
/// 这一个来源上，并明确说明其他来源未受影响。
///
/// 文案按来源种类说不同的话，因为发生的事不同：
/// - 文件来源：真的重新读了文件 →「已重读」；读不出来 →「重读失败」+ 原因。
/// - 运行时来源（记忆/检索/技能）：没有文件可重读，刷新 = 重新观测一次当前
///   状态。观测本身不会失败，但观测结果可能是「未就绪」——这不能报成 ✅。
fn refresh_context_source(core: &exm_core::Core, sid: &str, key: &str) -> String {
    let key = key.trim();
    // 未知键是用法错误，不是「刷新失败」：先对清单判定，不靠解析 Core 的错误文本。
    let sources = match core.context_sources(sid) {
        Ok(s) => s,
        Err(e) => return format!("【报告】读取上下文来源失败：{e}"),
    };
    if let Err(hint) = find_context_source(&sources, key) {
        return format!("【报告】没有上下文来源 {key}，无从刷新。{hint}");
    }
    let report = match core.context_source_refresh(sid, key) {
        Ok(report) => report,
        Err(e) => return format!("【报告】刷新来源 {key} 失败：{e}（其他来源不受影响）"),
    };
    let others = report["others"].as_array().map(Vec::len).unwrap_or(0);
    let line = context_source_line(&report["source"]);
    let is_file = report["source"]["kind"].as_str() == Some("file");
    match (report["ok"].as_bool(), is_file) {
        (Some(false), _) => {
            let err = report["error"].as_str().unwrap_or("未知原因");
            format!(
                "⚠️ 重读来源 {key} 失败：{err}\n{line}\n其他 {others} 个来源未受影响（未重读、状态不变）；本来源的禁用设置也未改动。"
            )
        }
        (Some(true), true) => {
            format!("✅ 已重读来源 {key}。\n{line}\n其他 {others} 个来源未重读、状态不变。")
        }
        _ => {
            let ready = report["source"]["status"].as_str() == Some("ready");
            let verdict = if ready {
                "已就绪，按预算参与自动注入".to_string()
            } else {
                format!(
                    "未就绪：{}",
                    report["source"]["reason"].as_str().unwrap_or("原因未上报")
                )
            };
            format!(
                "🔄 来源 {key} 没有可重读的文件（运行时来源），已重新观测：{verdict}\n{line}\n其他 {others} 个来源未重读、状态不变。"
            )
        }
    }
}

/// 上下文来源禁用/启用（capability-parity 6.3）：单来源操作，
/// 失败不影响其他来源（清单整体重读后回报）。
///
/// 文案只陈述已经验证过的事：开关管的是**自动注入**（规划/派发/智能体直连的各
/// 注入点下一轮起生效）；模型主动发起的记忆召回、用户显式 `/skill <id>` 不受它管。
fn apply_context_source(core: &exm_core::Core, sid: &str, key: &str, disable: bool) -> String {
    let key = key.trim();
    let verb = if disable { "停用" } else { "恢复" };
    match core.context_source_set(sid, key, disable) {
        Ok(disabled) => {
            let state = if disabled.is_empty() {
                "（当前没有停用的来源）".to_string()
            } else {
                format!("（已停用自动注入：{}）", disabled.join("、"))
            };
            format!("✅ 已{verb}来源 {key} 的自动注入，下一轮对话起生效{state}。")
        }
        // Core 层在任何失败路径上都先于写清单 bail（会话不存在 / 未知键 / 缺键），
        // 所以失败 = 没有任何来源被改动。Core 的报错文本里已带原因与可用键，
        // 原样转述即可——不解析错误文本来分类（那是脆弱做法），也不叠两层「失败：」
        Err(e) => format!("【报告】无法{verb}来源 {key}，未做任何改动。{e}"),
    }
}

/// 通道管理员判权（/reset、/stop 等管理指令共用）：通道 admins 清单命中，
/// 或绑定身份 role=admin（判权跟人走——人在本通道说话，身份登记在别的通道）。
fn is_channel_admin(core: &exm_core::Core, ch: &Channel, external_id: &str) -> bool {
    ch.admins.iter().any(|a| a.trim() == external_id)
        || core
            .identity_of(&ch.id, external_id)
            .map(|i| i.role == "admin")
            .unwrap_or(false)
}

/// /reset 落地：归档旧会话 → 新建会话 → 流绑定切换。
/// 会话内消息不动（归档 = archived_at 标记 + 移出默认列表，历史在归档区完整可回看）。
async fn reset_flow_session(core: &exm_core::Core, ch: &Channel, ctx: &InboundCtx<'_>) -> String {
    let old = core.flow_active_session(&ch.id, ctx.chat_key);
    // 无会话可直接开新的（首次消息前 /reset 也成立）
    let Some(old_sid) = old else {
        return match ensure_flow_session(core, ch, ctx.chat_key, ctx.flow_name, ctx.is_group) {
            Some(_) => "✅ 已重置：新建会话就绪，发消息即从零开始。".into(),
            None => "❌ 重置失败：会话创建失败，请稍后重试。".into(),
        };
    };
    // 运行中的轮次先停（旧会话可能在长任务里，不停会把回复落进归档会话）
    let stop_note = match core.stop_session(&old_sid) {
        Ok(n) => format!("（已停止运行中任务，{n} 次工具调用中止）"),
        Err(_) => String::new(),
    };
    // 归档旧会话（幂等；失败不阻断——旧会话保持原样，新会话照常开）
    let archived = core.store.set_session_archived(&old_sid, true).is_ok();
    // 新建同 scope 会话并切换绑定（ensure_flow_session_atomic 会复用绑定，
    // 这里需显式 create + bind：归档会话不再是活跃目标）
    let scope = core
        .store
        .get_session(&old_sid)
        .ok()
        .flatten()
        .map(|s| s.group_id)
        .unwrap_or_else(|| {
            ch.group
                .clone()
                .unwrap_or_else(|| format!("{SINGLE_PREFIX}{}", default_single_id(core)))
        });
    let title = format!("channel:{}:{}", ch.id, ctx.chat_key);
    match core.store.create_session(&title, &scope) {
        Ok(new_s) => {
            let bind = core.bind_flow_session_named(
                &ch.id,
                ctx.chat_key,
                &new_s.id,
                ctx.flow_name,
                Some(ctx.is_group),
            );
            let archive_note = if archived {
                "旧对话已归档（控制台归档区可回看）"
            } else {
                "⚠️ 旧会话归档失败（保留原样），但已切换新会话"
            };
            match bind {
                Ok(()) => format!(
                    "✅ 已重置{stop_note}。{archive_note}，新会话从零开始。"
                ),
                Err(e) => format!("❌ 重置失败：新会话已建但绑定切换失败（{e}）。旧会话保持归档态，可到控制台恢复。"),
            }
        }
        Err(e) => format!(
            "❌ 重置失败：新会话创建失败（{e}）。{}",
            if archived {
                "旧会话已归档，可到控制台恢复后重试。"
            } else {
                "旧会话未受影响。"
            }
        ),
    }
}

/// 运行收束事件 → 纯文本回帖（telegram / napcat / qqbot 三个适配器共用）。
/// 收束语句平铺（IM 投递形态）：正文原样逐行拼接，**不添加任何句式前缀**——
/// 是否使用【报告】等标记由模型自己决定，投递层只做拼接与截断。
///
/// 沉默约定（IM silence）：模型按旁听纪律输出 `<沉默/>`（可能独立成行或与
/// 少量文字混行）= 本轮无话可说——投递层整轮丢弃（返回空串，由调用方
/// 决定不发送 / 不记未读），群聊里不再出现「不参与群聊闲聊」这类废话。
///
/// 不在此处截断（防截断丢内容）：分段职责归 `split_reply`（按段落/代码块
/// 边界切），段数封顶归回帖循环。这里仅设单轮总预算天花板（一段上限 ×
/// 24 段）防异常超长（如工具输出裸拼）打爆通道侧。
pub(crate) fn flatten_statements(payload: &serde_json::Value, max_chars: usize) -> String {
    let mut lines: Vec<String> = Vec::new();
    if let Some(list) = payload.get("statements").and_then(|v| v.as_array()) {
        for s in list {
            let text = s.get("text").and_then(|v| v.as_str()).unwrap_or("");
            if text.trim().is_empty() {
                continue;
            }
            lines.push(text.to_string());
        }
    }
    let mut text = lines.join("\n");
    if text.trim().is_empty() {
        text = "（本轮无收束输出）".into();
    }
    if is_silence(&text) {
        return String::new();
    }
    let budget = max_chars.saturating_mul(MAX_REPLY_SEGMENTS.max(24));
    text.chars().take(budget).collect()
}

/// 单轮回帖段数默认封顶（IM 防轰炸）：超出部分截断并注明去处。
/// 不再像旧版一刀切 3 段——分段前先经完整预算（见 flatten_statements），
/// 常规长文（数段）可完整送达，只有异常超长才触顶。
pub(crate) const MAX_REPLY_SEGMENTS: usize = 24;

/// 沉默标记判定：语句里出现 `<沉默/>`（允许全角变体与空白）即整轮沉默。
/// 保守口径——只在标记明确出现时丢轮，避免误杀正常回复。
pub(crate) fn is_silence(text: &str) -> bool {
    let t = text.trim();
    t.contains("<沉默/>")
        || t.contains("<沉默 />")
        || t.contains("＜沉默/＞")
        || t.contains("＜沉默 ／＞")
        || t.contains("〈沉默/〉")
}
