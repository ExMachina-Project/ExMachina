//! channel 域分文件：通道消息统一执行（ChannelRun）+ 快捷指令 + 交互上下文还原。

use super::*;

// ---------------------------------------------------------------- 通道消息统一执行

/// 通道入站消息的执行上下文：**组绑定 → 会话复用 → 事件订阅 → 执行 → 组还原**。
/// 六个内置适配器（telegram / qqbot / napcat / discord / slack / matrix）共用这一段，
/// 避免「切组」与「会话键复用」两处语义在各自的适配器里各写一遍而慢慢漂移。
///
/// 用法：`begin` 建立上下文并拿到事件订阅（**先订阅再执行**，确保不漏收束事件）
/// → `spawn_reply` 挂回帖 → `run` 执行本轮 → await 回帖任务。
///
/// 注意：上下文本身可 Clone（回帖任务与执行方各持一份），
/// 但 `broadcast::Receiver` 不可 Clone，故它由 `begin` 单独交出。
#[derive(Clone)]
pub(crate) struct ChannelRun {
    pub(crate) core: std::sync::Arc<exm_core::Core>,
    pub session_id: String,
    pub(crate) prev_group: String,
    pub(crate) prev_single: Option<String>,
    pub(crate) switched: bool,
    /// 平台 kind（能力矩阵判定用）
    pub kind: String,
    /// 本轮能力边界（通道配置在 begin 时快照，执行期不变）
    pub(crate) scope: exm_core::types::ToolScope,
    /// 本轮消息来源标记（None = 人工触发的平台消息；"auto:video" = 主动观看等系统自动内容；
    /// "im:<名字|id>" = 群聊发送者——落库进消息 agent_id，展示层据此在消息头显示发送者，
    /// 模型侧上下文渲染为「[用户·名字]」区分群聊多人发言）
    pub origin: Option<String>,
    /// 发送者平台外部 ID（approval-ergonomics 通道裁决权限判定：
    /// origin 用户本人或跨通道 admins 才能裁 ok:/no:；非 flow 入口为 None）
    pub external_id: Option<String>,
    /// 聊天流定位（begin 时快照；run 落库后回写流预览用）
    pub(crate) flow: Option<(String, String)>,
    /// 本轮执行完成标记（`run()` 返回时置位）：回帖任务的退出判定输入之一。
    /// 收束事件（run.finished/run.error）在 emit 后微秒级到达，可能先于本标记
    /// 置位——因此回帖任务用「事件 + 定时轮询」双通道观察，绝不对该标记做
    /// 单次等待（notify 单发会因观察时机错过而永久悬挂，测试实测踩过）。
    /// Clone 共享同一实例（回帖任务与执行方各持一份）。
    pub(crate) round_done: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl ChannelRun {
    /// 账号绑定目标切换（单智能体或组）+ 会话键复用（`channel:<通道id>:<会话键>`）；
    /// 返回 (上下文, 运行事件订阅)
    pub(crate) async fn begin(
        core: &std::sync::Arc<exm_core::Core>,
        ch: &Channel,
        chat_key: &str,
    ) -> Option<(Self, tokio::sync::broadcast::Receiver<CoreEvent>)> {
        Self::begin_named(core, ch, chat_key, None, None).await
    }

    /// begin 带流名变体（会话管理可用性修复）：适配器把入站事件自带的
    /// 展示名（群名 / 对端昵称）传进来，建流/补名用——流列表不再显示裸
    /// chat_key（QQ 号）。名称只空填不覆盖（ensure_flow_named 语义）。
    pub(crate) async fn begin_named(
        core: &std::sync::Arc<exm_core::Core>,
        ch: &Channel,
        chat_key: &str,
        flow_name: Option<&str>,
        is_group: Option<bool>,
    ) -> Option<(Self, tokio::sync::broadcast::Receiver<CoreEvent>)> {
        let prev_group = core.active_group();
        let prev_single = core.registry().active_single();
        // 绑定目标缺省回落单智能体默认体（machina），未配置单智能体时维持当前上下文
        let target = ch
            .group
            .clone()
            .unwrap_or_else(|| format!("{SINGLE_PREFIX}{}", default_single_id(core)));
        // 切换后校验 + 有限重试（竞态 P1-2 短期缓解）：激活组/单体是进程级全局态，
        // 并发 begin（多通道同时来消息）会互相切走上下文——switch 后立即快照 scope，
        // 与目标不符说明被他人切走：重试抢占（最多 3 次，仍不符则用当前 scope 继续，
        // 消息不丢，只可能落错组——比挂死好）。中期方案：scope 做成 ChannelRun
        // 显式参数，消灭全局切换。
        let mut switched = false;
        let mut scope = core.registry().active_scope();
        for _ in 0..3 {
            switched = switch_context_to(core, &target);
            scope = core.registry().active_scope();
            let expected = expected_scope_of(core, &target);
            if expected.as_deref() == Some(scope.as_str()) {
                break;
            }
            // 被并发切换抢走：重试（下轮 switch 回自己的目标）
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        // 聊天流（chat flow）会话定位：流绑定优先——不主动切换时固定复用
        // 同一会话（跨轮上下文与记忆自然延续）。旧数据（按 title 寻址的存量
        // 通道会话）自动认领迁移到流上，行为无缝升级。
        // 原子版（竞态 P1-3）：查绑定→建会话→回写绑定整体临界区——并发同流
        // 消息不再各建会话（孤儿+绑定互抹）。组过滤在外层做（换组后旧绑定
        // 会话不跨组复用，原子版内组不符时按 None 处理重建）。
        let title = format!("channel:{}:{chat_key}", ch.id);
        let session = {
            let sid = core.ensure_flow_session_atomic(
                &ch.id,
                chat_key,
                &scope,
                &title,
                flow_name,
                is_group.unwrap_or(false),
            )?;
            core.store
                .get_session(&sid)
                .ok()
                .flatten()
                .filter(|s| s.group_id == scope)
        };
        let Some(session) = session else {
            // 绑定会话跨组不符：原子版内已复用绑定，但 scope 过滤后无效——
            // 新开本组会话并改绑（保守路径：罕见，通道换组才触发）。
            // create_session 落当前激活组（上面 switch 后即 scope）。
            let s = core.create_session(&title).ok()?;
            let _ = core.bind_flow_session_named(&ch.id, chat_key, &s.id, flow_name, is_group);
            // 建失败也别把交互上下文留在别人身上
            if core.store.get_session(&s.id).ok().flatten().is_none() {
                if switched {
                    restore_context(core, &prev_group, &prev_single);
                }
                return None;
            }
            return Self::finish_begin(
                core,
                ch,
                chat_key,
                s,
                flow_name,
                is_group.unwrap_or(false),
                prev_group,
                prev_single,
                switched,
            )
            .await;
        };
        Self::finish_begin(
            core,
            ch,
            chat_key,
            session,
            flow_name,
            is_group.unwrap_or(false),
            prev_group,
            prev_single,
            switched,
        )
        .await
    }

    /// begin 尾段（会话已定位）：公共装配（流名回填/IM 登记/预算档位/记忆分区/订阅）
    #[allow(clippy::too_many_arguments)] // 装配参数天然多；收拢成结构体反增噪音
    async fn finish_begin(
        core: &std::sync::Arc<exm_core::Core>,
        ch: &Channel,
        chat_key: &str,
        session: exm_core::types::Session,
        flow_name: Option<&str>,
        is_group: bool,
        prev_group: String,
        prev_single: Option<String>,
        switched: bool,
    ) -> Option<(Self, tokio::sync::broadcast::Receiver<CoreEvent>)> {
        let scope = core.registry().active_scope();
        // 流名回填（空填不覆盖）：流已存在但名为空（更早版本建的）时补上
        if let Some(n) = flow_name.filter(|n| !n.trim().is_empty()) {
            core.ensure_flow_named(&ch.id, chat_key, n, is_group);
        }
        // IM 通道会话登记：轮次组装系统提示时注入简洁表达纪律
        // （治理「一次性回复大量内容」：提示层约束 + 通道层截断双保险）
        exm_core::orchestrator::mark_im_session(&session.id);
        // 会话作用域快照（agent-budget-audit）：工具预算档位按通道 capability
        // 联动（chat < tools < computer）;begin 时写入,轮次内不变。
        core.orchestrator()
            .tools
            .set_session_scope(&session.id, ch.tool_scope());
        // 记忆隔离即时生效：通道每次执行前把分区键同步到会话；
        // 关闭隔离时清空分区，立即回到同组共享记忆。
        let partition = ch.memory_partition(&scope);
        let _ = core
            .store
            .set_session_memory_partition(&session.id, partition.as_deref());
        let rx = core.subscribe();
        Some((
            ChannelRun {
                core: core.clone(),
                session_id: session.id,
                prev_group,
                prev_single,
                switched,
                kind: ch.kind.clone(),
                scope: ch.tool_scope(),
                origin: None,
                external_id: None,
                flow: Some((ch.id.clone(), chat_key.to_string())),
                round_done: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            },
            rx,
        ))
    }

    /// 标记本轮消息来源（auto:xxx 系统自动内容 / im:xxx 群聊发送者；
    /// 落库进消息 agent_id，展示层与模型侧按前缀区分语义）
    pub(crate) fn with_origin(mut self, origin: impl Into<String>) -> Self {
        self.origin = Some(origin.into());
        self
    }

    /// 标记发送者平台外部 ID（approval-ergonomics：通道裁决 ok:/no: 权限判定）。
    /// 六适配器在 stamp_session_origin 同位调用（同一个人的两个标记）。
    pub(crate) fn with_external_id(mut self, external_id: impl Into<String>) -> Self {
        self.external_id = Some(external_id.into());
        self
    }

    /// 收束事件 → 回帖文本（非收束事件返回 None；已按平台上限截断）
    pub(crate) fn take_reply(&self, evt: &CoreEvent, max_chars: usize) -> Option<String> {
        match evt.kind.as_str() {
            "run.finished" => {
                let text = flatten_statements(&evt.payload, max_chars);
                // 沉默约定（IM silence）：模型宣告无话可说 → None（不投递），
                // 所有直调方（spawn_reply / rss / video 插件）统一免发送。
                if text.trim().is_empty() {
                    None
                } else {
                    Some(text)
                }
            }
            "run.error" => {
                let msg = evt
                    .payload
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("运行失败");
                // 消息自带句式前缀（【报告】/【警告】等）时不再包裹——
                // emit_run_error 的调用方文案已按语言纪律定稿，二次包裹会产生
                // 「【警告】【报告】…」双层前缀（部署实测）
                let already = msg.starts_with('【')
                    && msg.chars().skip(1).take(4).any(|c| c == '】');
                let text = if already {
                    msg.to_string()
                } else {
                    format!("【警告】{msg}")
                };
                Some(text.chars().take(max_chars).collect())
            }
            // 工具预算熔断内联提示（agent-budget-audit 3.2）：预算耗尽的首次拦截
            // 内联一条警告（用户感知「为什么停了」）；后续同轮再拦截不重复推——
            // 模型收束前可能连续多次尝试，逐条推送会刷屏。
            "tool.budget" => {
                let reason = evt
                    .payload
                    .get("reason")
                    .and_then(|v| v.as_str())
                    .unwrap_or("工具调用预算已耗尽");
                let key = format!("budget-notified:{}", evt.session_id);
                if self.core.consume_once_flag(&key) {
                    Some(format!("【警告】{reason}").chars().take(max_chars).collect())
                } else {
                    None
                }
            }
            // 计划待审批（spec plan-mode 跨端呈现）：文本摘要 + 快捷审批指引。
            // 各 IM 适配器把本回复推回通道，即完成「推送文本摘要 + 快捷审批回复方式」
            "plan.pending" => {                let count = evt
                    .payload
                    .get("count")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let items = self.core.plan_list(&self.session_id);
                let mut lines = vec![format!("📋 计划待审批（{count} 条）：")];
                for (i, it) in items.iter().take(8).enumerate() {
                    let target: String = it.target.chars().take(60).collect();
                    lines.push(format!("{}. [{}] {} {}", i + 1, it.action, it.tool, target));
                }
                if items.len() > 8 {
                    lines.push(format!("… 共 {} 条", items.len()));
                }
                lines.push("回复「plan:yes」批准执行，「plan:no」拒绝保留".into());
                Some(lines.join("\n").chars().take(max_chars).collect())
            }
            // 审批单待裁决（approval-ergonomics 跨端呈现）：与 plan.pending 同位。
            // 推单号 + 命令摘要 + 裁决用法;单号支持前缀匹配（完整 8 位或其唯一前缀）。
            // 通道裁决只走 once 档（设计决策:档位在文本界面无法可靠表达,升档去控制台）。
            "approval.required" => {
                let id = evt
                    .payload
                    .get("approvalId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let cmd = evt
                    .payload
                    .get("command")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let wait = self
                    .core
                    .config()
                    .security
                    .approval_wait_secs;
                let cmd_short: String = cmd.chars().take(120).collect();
                let mut lines = vec![
                    format!("⛔ 审批请求（单号 {id}）："),
                    cmd_short,
                    format!(
                        "{wait} 秒内回复 ok:{id} 批准 / no:{id} 拒绝，或到控制台裁决"
                    ),
                ];
                if cmd.len() > 120 {
                    lines.insert(2, "…（命令已截断）".into());
                }
                Some(lines.join("\n").chars().take(max_chars).collect())
            }
            "plan.applied" => {
                let ok = evt
                    .payload
                    .get("ok")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let target = evt
                    .payload
                    .get("target")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                Some(format!(
                    "{} 计划条目已执行：{target}",
                    if ok { "✅" } else { "❌" }
                ))
            }
            _ => None,
        }
    }

    /// 执行本轮；返回后交互上下文已还原（Drop guard 覆盖提前返回 / 任务中止）
    pub(crate) async fn run(self, text: &str) {
        // 聊天流预览回写（列表展示最新一条；失败不影响执行）
        if let Some((ch_id, chat_key)) = &self.flow {
            let _ = self
                .core
                .touch_flow_message(ch_id, chat_key, text.trim());
        }
        let _restore = GroupRestore {
            core: self.core.clone(),
            prev_group: self.prev_group.clone(),
            prev_single: self.prev_single.clone(),
            switched: self.switched,
        };
        // 计划/审批快捷指令（plan-mode / approval-ergonomics）：plan:yes / plan:no /
        // ok:<单号> / no:<单号> 直达裁决，不进模型轮次。
        // 适配器已在 begin 前调用 quick_directive_reply 直发回帖；这里保留同一入口
        // 兜底（非适配器调用方 / 旧路径），行为与直发一致。
        if let Some(reply) = quick_directive_reply(
            &self.core,
            Some(&self.session_id),
            self.external_id.as_deref(),
            text,
        )
        .await
        {
            self.core.emit_run_error(&self.session_id, &reply);
            return;
        }
        // 周期 token 配额（外部来源会话）：耗尽即拒绝新轮次并经 run.error 回帖提示；
        // 组内派发会话无来源登记，天然不受限（设计 D10）
        let limits = self.core.config().limits.clone();
        if limits.enabled && limits.quota_tokens > 0 {
            if let Some((_subject, _role)) = self.core.session_origin(&self.session_id) {
                let used = self.core.session_tokens_estimate(&self.session_id);
                if used >= limits.quota_tokens {
                    self.core.emit_run_error(
                        &self.session_id,
                        &format!(
                            "【报告】周期 token 配额已耗尽（已用 {used} / 上限 {}）：本轮拒绝执行，请联系管理员调整配额。",
                            limits.quota_tokens
                        ),
                    );
                    return;
                }
            }
        }
        let _ = limits;
        if let Err(e) = self
            .core
            .chat_scoped_origin(&self.session_id, text, self.scope, self.origin.as_deref())
            .await
        {
            eprintln!("[channel:{}] 执行失败：{e}", self.session_id);
        }
        // 本轮执行完成（含失败路径）：回帖任务据此在「无收束事件」的异常路径
        //（如预算闸门 bail!）下也能退出（轮询 round_done + 排空广播，见 spawn_reply）。
        self.round_done
            .store(true, std::sync::atomic::Ordering::Release);
    }
}

/// 交互上下文兜底还原：任务正常结束、提前返回、甚至被 abort 时都执行 Drop，
/// 避免视频轮询等后台任务超时中止后把全局激活组/单智能体留在通道绑定目标上。
struct GroupRestore {
    core: std::sync::Arc<exm_core::Core>,
    prev_group: String,
    prev_single: Option<String>,
    switched: bool,
}

impl Drop for GroupRestore {
    fn drop(&mut self) {
        if self.switched {
            restore_context(&self.core, &self.prev_group, &self.prev_single);
        }
    }
}

/// 审批快捷指令解析（approval-ergonomics）：`ok:<单号>` / `no:<单号>`。
/// 单号 = 8 位审批单 id（approval.required 推送文本所带）。返回 (是否批准, 单号)。
/// 大小写不敏感；单号非空即受理（不存在/已处理在裁决点报告,不在解析点挡）。
pub(crate) fn parse_approval_directive(text: &str) -> Option<(bool, String)> {
    let lower = text.trim().to_lowercase();
    let (approve, rest) = if let Some(r) = lower.strip_prefix("ok:") {
        (true, r)
    } else if let Some(r) = lower.strip_prefix("no:") {
        (false, r)
    } else {
        return None;
    };
    let id = rest.trim();
    if id.is_empty() {
        return None;
    }
    Some((approve, id.to_string()))
}

/// 是否快捷裁决指令（plan:yes / plan:no / ok:<单号> / no:<单号>）。
/// 适配器用它在建会话/回帖任务前分流，避免为裁决消息启动模型轮次。
pub(crate) fn is_quick_directive(text: &str) -> bool {
    let t = text.trim();
    t == "plan:yes" || t == "plan:no" || parse_approval_directive(t).is_some()
}

/// 快捷指令直裁（plan-mode / approval-ergonomics 跨端审批）：
/// `plan:yes` / `plan:no` / `ok:<单号>` / `no:<单号>` 直达裁决，不进模型轮次。
/// 返回 Some(回帖文本) = 已处理；None = 非指令。
///
/// 适配器应在 `ChannelRun::begin` / `spawn_reply` 之前调用：同一会话可能仍有
/// 原轮次在跑（审批等待中），再建一个回帖任务会与原任务争抢同一事件流——
/// 裁决回帖被重复投递，且原任务提前退出会吞掉原轮最终答复。
pub(crate) async fn quick_directive_reply(
    core: &std::sync::Arc<exm_core::Core>,
    session_id: Option<&str>,
    external_id: Option<&str>,
    text: &str,
) -> Option<String> {
    let trimmed = text.trim();
    // 计划快捷审批（spec plan-mode 跨端审批）：无待审清单时提示当前状态
    if trimmed == "plan:yes" || trimmed == "plan:no" {
        let approve = trimmed == "plan:yes";
        let Some(session_id) = session_id else {
            return Some("【报告】当前无待审批计划（清单为空或已处理）。".into());
        };
        if core.plan_list(session_id).is_empty() {
            return Some("【报告】当前无待审批计划（清单为空或已处理）。".into());
        }
        if approve {
            return Some(match core.plan_approve(session_id).await {
                Ok(rs) => {
                    let ok_n = rs.iter().filter(|(_, r)| r.ok).count();
                    let fail_n = rs.len() - ok_n;
                    format!("【报告】计划已批准并回放：{ok_n} 条成功，{fail_n} 条失败。")
                }
                Err(e) => format!("【警告】计划回放失败：{e}"),
            });
        }
        core.plan_reject(session_id);
        return Some("【报告】计划已拒绝（清单保留，可继续修改后再次生成）。".into());
    }
    // 审批快捷裁决（approval-ergonomics 跨端审批）：单号支持前缀匹配
    //（完整 8 位或唯一前缀）。权限口径：审批单 origin 用户本人，或任一通道
    // admins 命中（判权跟人走，跨通道）。通道裁决只走 once 档（档位在文本
    // 界面无法可靠表达，升档去控制台）。
    let (approve, id_input) = parse_approval_directive(trimmed)?;
    let pendings = core.approval_list(Some("pending"), 500).unwrap_or_default();
    let hits: Vec<exm_core::types::ApprovalRequest> = pendings
        .into_iter()
        .filter(|a| a.id.starts_with(&id_input))
        .collect();
    let ap = match hits.as_slice() {
        [one] => one.clone(),
        [] => return Some(format!("【报告】审批单 {id_input} 不存在或已处理。")),
        _ => {
            return Some(format!(
                "【报告】单号前缀 {} 命中 {} 张待审单（{}），请补全单号。",
                id_input,
                hits.len(),
                hits.iter().map(|a| a.id.as_str()).collect::<Vec<_>>().join(" / ")
            ))
        }
    };
    let id = ap.id.clone();
    let caller = external_id.unwrap_or("");
    let is_origin_user = ap
        .origin_user
        .as_deref()
        .is_some_and(|u| !u.is_empty() && caller == u);
    let is_admin = !caller.is_empty()
        && super::channels::load_channels(core)
            .iter()
            .any(|c| c.admins.iter().any(|a| a.trim() == caller));
    if !is_origin_user && !is_admin {
        return Some(format!(
            "【报告】审批裁决被拒绝：发送者无权裁决审批单 {id}（仅发起用户或管理员）。"
        ));
    }
    Some(
        match core
            .approval_decide_scoped(
                &id,
                if approve {
                    exm_core::types::ApprovalScope::Once { approve: true }
                } else {
                    exm_core::types::ApprovalScope::Once { approve: false }
                },
            )
            .await
        {
            Ok(r) => {
                let head = if approve { "已批准并执行" } else { "已拒绝" };
                let detail = r.result.as_deref().unwrap_or("");
                let note: String = detail.chars().take(300).collect();
                format!("【报告】审批单 {id} {head}。{note}")
            }
            Err(e) => format!("【警告】审批单 {id} 裁决失败：{e}"),
        },
    )
}

/// 还原到之前的交互上下文（组 + 单智能体快照）
pub(crate) fn restore_context(
    core: &exm_core::Core,
    prev_group: &str,
    prev_single: &Option<String>,
) {
    if let Some(id) = prev_single {
        let _ = core.registry().set_active_single(Some(id));
    } else {
        if core.registry().single_mode() {
            let _ = core.registry().set_active_single(None);
        }
        let _ = core.registry().set_active_group(prev_group);
    }
}

/// 通道缺省绑定目标：默认单智能体 machina；不存在时取任意首个单智能体；都无则回落组模式
pub(crate) fn default_single_id(core: &exm_core::Core) -> String {
    if core.registry().single("machina").is_some() {
        return "machina".into();
    }
    core.registry()
        .list_singles()
        .first()
        .map(|d| d.identifier.clone())
        .unwrap_or_default()
}

/// 挂本轮回帖任务：订阅运行事件，收束即用 `send` 把文本发回原会话（发完或流断即退出）。
/// 各适配器只需给出发送闭包（平台 API 差异全部封在这里面）。
/// 媒体产物条目（run.finished artifacts → 平台投递）
#[derive(Debug, Clone)]
pub(crate) struct MediaItem {
    /// image | file | voice
    pub kind: String,
    /// 绝对路径（spawn_reply 已从工作区相对路径解析）
    pub path: std::path::PathBuf,
    pub caption: String,
    /// 字节大小（降级注记展示用；登记时未知则 0）
    pub size: u64,
}

/// 审批推送的富消息钩子载荷（approval-ergonomics 平台交互）：文本摘要 + 审批单号。
/// 平台适配器可据此附加原生交互控件（如 QQ 官方机器人按钮）；钩子返回 false
/// 表示富消息发送失败，调用方回落普通文本（审批单不丢）。
pub(crate) struct ApprovalPrompt {
    pub approval_id: String,
    pub text: String,
}

/// 审批富消息发送钩子：返回是否已成功送达（false = 回落普通文本）。
pub(crate) type ApprovalHook = Box<
    dyn Fn(ApprovalPrompt) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send>>
        + Send,
>;

/// 媒体发送钩子（Box 化便于适配器侧多处克隆捕获）：返回是否已成功送达
///（false = 回落降级注记）。
pub(crate) type MediaHook = Box<
    dyn Fn(MediaItem) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send>> + Send,
>;

