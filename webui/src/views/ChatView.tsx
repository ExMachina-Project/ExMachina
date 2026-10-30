/** 对话视图（整合控制台）：左栏 = 组切换 + 会话列表；右侧 = 消息流 + 实时流 + 输入 */
import React, { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Alert, Button, Card, Dropdown, Input, Modal, Popconfirm, Segmented, Select, Space, Spin, Tag, message } from "antd";
import { ArrowLeftOutlined, ArrowUpOutlined, CheckOutlined, CloudUploadOutlined, CloseOutlined, EditOutlined, ExportOutlined, FolderOpenOutlined, MessageOutlined, MoreOutlined, PauseCircleOutlined, PlusOutlined, RobotOutlined, SearchOutlined, TeamOutlined, UserOutlined } from "@ant-design/icons";
import { useIsNarrow, NARROW_QUERY } from "../useIsNarrow";
import { api, type LlmProfile } from "../api";
import { agentIdEn } from "../models";
import { useExm, type TimelineItem } from "../store";
import { useT } from "../i18n/core";

interface TargetInfo { mode: "group" | "single"; id: string; name?: string }
import { StatementList } from "../components/Statements";
import { PlanCard } from "../components/PlanCard";
import { Markdown } from "../components/Markdown";
import { AsciiMeter } from "../components/Ascii";
import { ToolCallList } from "../components/ToolCall";
import { FileMentionPopup, mentionCandidates, type MentionState } from "../components/FileMention";
import { workbenchTree, type WorkbenchNode } from "../api";
import { copyText } from "../components/Markdown";
import { UsageBar } from "../components/UsageBar";
import { ProcessSteps } from "../components/ProcessSteps";
import { TimelineTag } from "../components/TimelineTag";
import { SkeletonList } from "../components/SkeletonList";
import { OrbitRings } from "../components/OrbitRings";
import { TurnOps } from "../components/HistoryOps";
import { Emblem } from "../components/Emblem";
import type { ChatMessage } from "../types";

function MessageBubble({ m, turn, ops, orchLabel }: { m: ChatMessage; turn?: number; ops?: React.ReactNode; orchLabel?: string }): React.ReactElement {
  const tb = useT();
  const isUser = m.role === "user";
  // 自动任务标记：agentId 带 auto: 前缀的 user 消息是系统自动内容（视频观看等），非真人输入
  const autoOrigin = isUser && m.agentId?.startsWith("auto:") ? m.agentId.slice(5) : null;
  const title = isUser
    ? autoOrigin
      ? tb("chat.role.auto")
      : tb("chat.role.user")
    : m.role === "orchestrator"
      ? (orchLabel ?? tb("chat.role.orchestrator"))
      : (m.agentId ?? tb("chat.role.system"));
  const hasThinking = Boolean(m.thinking && m.thinking.trim());
  const hasTools = Boolean(m.toolCalls && m.toolCalls.length > 0);
  const steps = m.steps ?? [];
  const hasProcess = hasThinking || hasTools || steps.length > 0;
  const time = relativeTime(m.createdAt);
  return (
    <Card size="small" className={`msg-bubble ${isUser ? "msg-user" : "msg-agent"}`}>
      <div className="msg-head">
        {isUser ? <UserOutlined /> : <RobotOutlined />} <b>{title}</b>
        {autoOrigin && <span className="auto-tag">{autoOrigin}</span>}
        {turn != null && <span className="msg-turn mono">#{turn}</span>}
        {time && <span className="msg-time mono">{time}</span>}
        {!isUser && (
          <button
            className="msg-copy"
            title="复制"
            onClick={async () => {
              const text = m.statements.map((st) => st.text).join("\n\n");
              if (await copyText(text)) {
                message.success("已复制");
              }
            }}
          >
            ⧉
          </button>
        )}
        {ops && <span className="msg-ops">{ops}</span>}
      </div>
      {hasProcess &&
        (steps.length > 0 ? (
          // 有序步骤：思考/工具按发生顺序交错逐次显示（服务端留存，刷新后仍在）
          <ProcessSteps
            parts={steps.map((s) => ({ kind: s.kind, text: s.text, callId: s.callId }))}
            toolCalls={m.toolCalls ?? []}
            labelThinking={(n) => tUsage(tb, n)}
            labelTools={(n) => tb("chat.usedTools", { n: String(n) })}
            sessionId={m.sessionId}
            turn={m.turn ?? turn}
          />
        ) : (
          <div className="msg-process">
            {hasThinking && (
              <UsageBar label={tUsage(tb, m.thinking!.length)}>
                <pre className="usage-think">{m.thinking}</pre>
              </UsageBar>
            )}
            {hasTools && (
              <UsageBar label={tb("chat.usedTools", { n: m.toolCalls!.length })}>
                <ToolCallList calls={m.toolCalls!} sessionId={m.sessionId} turn={m.turn ?? turn} />
              </UsageBar>
            )}
          </div>
        ))}
      <Markdown text={m.statements.map((s) => s.text).join("\n\n")} />
    </Card>
  );
}

/** 思考栏摘要文案（OpenCode 口径：使用了 N 个思考；本地思维流以字数计量） */
function tUsage(tb: (k: string, v?: Record<string, string>) => string, chars: number): string {
  return tb("chat.usedThinking", { n: String(chars) });
}

/** 时间线行（派发/回流/记忆/裁决…）：派发条目带「查看子会话」跳转（子个体过程流入口） */
function TimelineRows({ items, tb, onOpenSub }: { items: TimelineItem[]; tb: (k: string, v?: Record<string, string>) => string; onOpenSub: (id: string) => void }): React.ReactElement {
  return (
    <>
      {items.map((tl, i) => (
        <div key={`tl-${i}`} className={`timeline-item tl-${tl.kind}`}>
          <TimelineTag kind={tl.kind} ns="chat" />
          <span className="tl-text">{tl.text}</span>
          {tl.childSessionId && (
            <button
              className="tl-sub-link"
              onClick={(e) => {
                e.stopPropagation();
                onOpenSub(tl.childSessionId!);
              }}
            >
              {tb("chat.openSubSession")} →
            </button>
          )}
        </div>
      ))}
    </>
  );
}

/** 相对时间（chat-ergonomics）：刚刚 / N 分钟前 / N 小时前 / 昨天 / N 天前 / 日期 */
export function relativeTime(iso?: string): string {
  if (!iso) return "";
  const t = new Date(iso).getTime();
  if (Number.isNaN(t)) return "";
  const diff = Date.now() - t;
  const min = Math.floor(diff / 60000);
  if (min < 1) return "刚刚";
  if (min < 60) return `${min} 分钟前`;
  const hour = Math.floor(min / 60);
  if (hour < 24) return `${hour} 小时前`;
  const day = Math.floor(hour / 24);
  if (day === 1) return "昨天";
  if (day < 7) return `${day} 天前`;
  return iso.slice(0, 10);
}

export function ChatView(): React.ReactElement {
  const t = useT();
  const {     messages, liveOrch, liveThinking, liveParts, runToolCalls, approvals,
    timeline, timelineSession, running, send, wsConnected, decideApproval,
    sessions, sessionId, selectSession, newSession,
    groups, activeGroup, setTarget,
    refreshAgents,
    loadGroups,
    sessionsLoaded,
    plan, planApplied, planPending, decidePlan, setPlanMode,
    todo,
    spawns, clearSpawns,
  } = useExm();
  const [target, setTargetInfo] = useState<TargetInfo>({ mode: "group", id: activeGroup });
  const [singles, setSinglesList] = useState<{ identifier: string; name: string; modelHint?: string | null }[]>([]);
  const [profiles, setProfiles] = useState<LlmProfile[]>([]);
  const [activeProfileId, setActiveProfileId] = useState("");
  // 确认弹窗走 useModal 演出器：静态 Modal.confirm 渲染在 React 树外、吃不到 ConfigProvider，
  // 暗色主题下会落回默认白底（与全局自绘样式打架）；演出器版本随主题/语言/令牌同步。
  const [modal, modalContextHolder] = Modal.useModal();

  const loadTarget = useCallback(async () => {
    try {
      const t = await api.getTarget();
      setTargetInfo({ mode: t.mode, id: t.id, name: t.name });
      const s = await api.listSingles();
      setSinglesList(s.singles.map((x) => ({ identifier: x.identifier, name: x.name, modelHint: x.modelHint })));
    } catch {
      // 目标接口不可用时退回组模式展示
    }
  }, []);

  useEffect(() => {
    void loadTarget();
  }, [loadTarget, activeGroup]);
  useEffect(() => {
    api.llmProfiles().then((r) => {
      setProfiles(r.profiles);
      setActiveProfileId(r.active);
    }).catch(() => undefined);
  }, []);

  const changeTarget = async (value: string) => {
    const [mode, id] = value.startsWith("single:") ? (["single", value.slice(7)] as const) : (["group", value.slice(6)] as const);
    await setTarget(mode, id);
    message.success(mode === "single" ? t("chat.switchedSingle", { id }) : t("chat.switchedGroup", { id }));
    await loadTarget();
  };
  const [text, setText] = useState("");
  const [images, setImages] = useState<string[]>([]);
  const [expandedSpawns, setExpandedSpawns] = useState<string[]>([]);
  /** `@` 文件提及（对话即编码）：输入 @ 触发候选弹层，选中插入路径 */
  const [mention, setMention] = useState<MentionState>({ open: false, query: "", start: 0 });
  const [mentionIdx, setMentionIdx] = useState(0);
  const [mentionNodes, setMentionNodes] = useState<WorkbenchNode[]>([]);
  const [mentionLoading, setMentionLoading] = useState(false);
  const mentionSeqRef = useRef(0);
  const [sessionFilter, setSessionFilter] = useState("");
  /** 竖屏导航 = 列表页 / 会话页 两个独立页面（对标主流移动端）：
   *  进入应用先落在会话列表，点某条会话才进入该会话页，会话页有返回按钮回到列表。
   *  带深链（#/chat/<id>）时直接进会话页；桌面端两栏并排，screen 恒为 thread。 */
  const narrow = useIsNarrow();
  // 首屏偏好：窄屏默认落在列表页（深链 #/chat/<id> 直接进会话页）。
  // 注意不要用 effect 在首帧纠正 screen——matchMedia 在懒加载 chunk 落地时可能
  // 还没稳定，一次 setScreen("thread") 就会把用户永远留在会话页（实测复现）。
  const [screenPref, setScreenPref] = useState<"list" | "thread">(() => {
    if (typeof window === "undefined" || !window.matchMedia(NARROW_QUERY).matches) return "thread";
    return /^#\/chat\/[^/]/.test(window.location.hash) ? "thread" : "list";
  });
  const screen: "list" | "thread" = narrow ? screenPref : "thread";
  const setScreen = setScreenPref;
  /** 会话类型过滤（2.2）：all / normal / channel */
  const [sessionKind, setSessionKind] = useState("all");
  const [renaming, setRenaming] = useState<{ id: string; title: string } | null>(null);
  const [usage, setUsage] = useState<{
    estimate: number;
    budget: number;
    unlimited: boolean;
    /** provider 上报的真实用量可用（否则为字符估算） */
    measured?: boolean;
  } | null>(null);
  /** 上箭头召回的游标（null = 未处于召回态） */
  const recallIdxRef = useRef<number | null>(null);
  const bottomRef = useRef<HTMLDivElement>(null);

  // 附件压缩：Canvas 缩到 ≤1280px、JPEG 82%（控制多模态请求体尺寸）
  const addImage = (file: File) => {
    const reader = new FileReader();
    reader.onload = () => {
      const img = new Image();
      img.onload = () => {
        const scale = Math.min(1, 1280 / Math.max(img.width, img.height));
        const canvas = document.createElement("canvas");
        canvas.width = Math.round(img.width * scale);
        canvas.height = Math.round(img.height * scale);
        canvas.getContext("2d")?.drawImage(img, 0, 0, canvas.width, canvas.height);
        const dataUrl = canvas.toDataURL("image/jpeg", 0.82);
        setImages((prev) => [...prev, dataUrl].slice(0, 4));
      };
      img.src = String(reader.result);
    };
    reader.readAsDataURL(file);
  };

  // 子会话 = 派发运行台账：只读查看子个体过程流（不允许在子会话里开工新轮次）
  const isSubSession = Boolean(sessions.find((s) => s.id === sessionId)?.title.startsWith("sub:"));

  const doSend = () => {
    if (isSubSession) return;
    if (text.trim()) {
      void send(text, images);
      setText("");
      setImages([]);
    }
  };

  // 切会话恢复草稿（chat-ergonomics）
  useEffect(() => {
    if (!sessionId) return;
    setText(useExm.getState().drafts[sessionId] ?? "");
    recallIdxRef.current = null;
  }, [sessionId]);

  // 是否已滚离底部（chat-ergonomics：显示「回到底部」按钮）
  const [showBottomBtn, setShowBottomBtn] = useState(false);

  /** 检测输入末尾的 `@提及` 片段：光标前最近一个未被空格隔断的 @ 起段即触发 */
  const detectMention = useCallback((value: string, caret: number) => {
    const before = value.slice(0, caret);
    const at = before.lastIndexOf("@");
    if (at < 0) {
      setMention((m) => ({ ...m, open: false }));
      return;
    }
    const seg = before.slice(at + 1);
    // 触发词不含空白与第二个 @；超长视为普通文本（误触保护）
    if (seg.length > 48 || /[\s@]/.test(seg)) {
      setMention((m) => ({ ...m, open: false }));
      return;
    }
    setMention({ open: true, query: seg, start: at });
  }, []);

  /** 文件树懒加载：首次触发 @ 时拉取一次（会话期内缓存；深层目录由模型 read 工具承担） */
  useEffect(() => {
    if (!mention.open || mentionNodes.length > 0 || mentionLoading) return;
    const seq = ++mentionSeqRef.current;
    setMentionLoading(true);
    void workbenchTree("", 6)
      .then((r) => {
        if (seq !== mentionSeqRef.current) return;
        setMentionNodes(r.nodes);
      })
      .catch(() => {
        if (seq === mentionSeqRef.current) setMention((m) => ({ ...m, open: false }));
      })
      .finally(() => {
        if (seq === mentionSeqRef.current) setMentionLoading(false);
      });
  }, [mention.open, mentionNodes.length, mentionLoading]);

  /** 选中候选：把 @片段 替换为 `路径 `（占位插入，路径中的 @/空格风险由过滤端点保证不存在） */
  const applyMention = useCallback((path: string) => {
    setText((prev) => {
      const after = prev.slice(mention.start).replace(/@[^\s@]*/, "");
      return prev.slice(0, mention.start) + path + after + " ";
    });
    setMention({ open: false, query: "", start: 0 });
    setMentionIdx(0);
    // 焦点回输入框（弹层点击会抢焦点）
    requestAnimationFrame(() => {
      const el = document.querySelector<HTMLTextAreaElement>(".composer-textarea textarea");
      el?.focus();
      const pos = mention.start + path.length + 1;
      el?.setSelectionRange(pos, pos);
    });
  }, [mention]);

  /** 提及弹层键盘导航（textarea onKeyDown 转发）：↑↓ 移动、Enter/Tab 选中、Esc 关闭。
   *  返回 true = 已接管该键，宿主不得再触发发送。 */
  const onMentionKey = useCallback(
    (e: React.KeyboardEvent<HTMLTextAreaElement>): boolean => {
      if (!mention.open) return false;
      const cands = mentionCandidates(mentionNodes, mention.query);
      if (e.key === "ArrowDown") {
        e.preventDefault();
        setMentionIdx((i) => Math.min(i + 1, cands.length - 1));
        return true;
      }
      if (e.key === "ArrowUp") {
        e.preventDefault();
        setMentionIdx((i) => Math.max(i - 1, 0));
        return true;
      }
      if (e.key === "Enter" || e.key === "Tab") {
        e.preventDefault();
        const c = cands[mentionIdx];
        if (c) applyMention(c.path);
        return true;
      }
      if (e.key === "Escape") {
        e.preventDefault();
        setMention((m) => ({ ...m, open: false }));
        return true;
      }
      return false;
    },
    [mention, mentionNodes, mentionIdx, applyMention],
  );

  // 流式跟随只在「用户本来就贴底」时进行：中途上翻回看不打断。
  // 距底阈值 80px 兼容流式渲染的轻微抖动；用 ref 记录贴底态避免重渲染竞态。
  const stickRef = React.useRef(true);
  useEffect(() => {
    const el = document.querySelector(".chat-scroll");
    if (!el) return;
    const onScroll = () => {
      const atBottom = el.scrollHeight - el.scrollTop - el.clientHeight <= 80;
      stickRef.current = atBottom;
      // 用户主动回底 → 恢复跟随（按钮点击滚到底同样触发此路径）
      setShowBottomBtn(!atBottom);
    };
    el.addEventListener("scroll", onScroll, { passive: true });
    return () => el.removeEventListener("scroll", onScroll);
  }, []);
  useEffect(() => {
    // 新消息（非流式增量）：重置跟随（新轮次开始视为用户关注最新）
    stickRef.current = true;
    bottomRef.current?.scrollIntoView({ behavior: "smooth" });
    setShowBottomBtn(false);
  }, [messages.length]);
  useEffect(() => {
    // 流式增量（live/token/timeline）：仅贴底时跟随，不抢滚动条
    if (!stickRef.current) return;
    bottomRef.current?.scrollIntoView({ behavior: "auto" });
  }, [messages, liveOrch, liveParts, runToolCalls, timeline]);
  // 审批卡到达：强制拉到卡位（审批是阻塞等待，时效性强——
  // 用户上翻回看时也该被打断，错过 = 工具在等待窗内一直挂着）
  useEffect(() => {
    if (approvals.length === 0) return;
    stickRef.current = true;
    bottomRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [approvals.length]);

  useEffect(() => {
    if (!sessionId) return;
    void (async () => {
      try {
        setUsage(await api.sessionTokens(sessionId));
      } catch {
        // 用量接口不可用时静默
      }
    })();
  }, [sessionId, messages.length, running]);

  // 单智能体模式：交互主体 = 该个体，过程（工具/思考）与正文同源——
  // liveOrch 卡已聚合 thinking 与工具栏（runToolCalls），liveParts 若再建卡
  // 就是同一个体被拆成两张分裂卡（identifier 一大一小写两张头像）。过滤掉。
  const liveUnitEntries = Object.entries(liveParts).filter(
    ([agent, v]) => v.length > 0 && !(target.mode === "single" && agent === target.id),
  );
  const activeMeta = groups.find((g) => g.id === activeGroup);
  // 指挥体显示名（10）：智能体模式显示该智能体名；组模式显示「指挥体 [组名]」
  const orchLabel = target.mode === "single"
    ? (singles.find((x) => x.identifier === target.id)?.name ?? target.id)
    : t("chat.orchGroup", { group: activeMeta?.name ?? target.id });
  const targetValue = target.mode === "single" ? `single:${target.id}` : `group:${target.id}`;
  const currentModel = target.mode === "single"
    ? (singles.find((x) => x.identifier === target.id)?.modelHint ?? "")
    : (groups.find((g) => g.id === target.id)?.model ?? "");
  const activeProfile = profiles.find((p) => p.id === activeProfileId);
  const modelOptions = [
    {
      value: "",
      label: activeProfile?.model
        ? `${t("models.followGlobalShort")} · ${activeProfile.model}`
        : t("models.followGlobalShort"),
    },
    ...profiles.flatMap((p) => [
      ...(p.model ? [{ value: p.id, label: `${p.model} · ${p.name}` }] : []),
      ...p.models
        .filter((m) => m.model !== p.model)
        .map((m) => ({ value: `${p.id}/${m.model}`, label: `${m.model} · ${p.name}` })),
    ]),
  ];
  const changeModel = async (value: string) => {
    try {
      if (target.mode === "single") {
        await api.setAgentModel(target.id, value);
      } else {
        await api.setGroupModel(target.id, value);
      }
      await Promise.all([loadTarget(), loadGroups()]);
      message.success(t("common.saved"));
    } catch (e) {
      message.error(t("common.saveFailed", { err: String(e).slice(0, 120) }));
    }
  };
  const targetOptions = [
    {
      label: t("chat.groupLabel"),
      options: groups.map((g) => ({
        value: `group:${g.id}`,
        label: `${g.name}（${g.id}${g.builtin ? ` · ${t("chat.builtin")}` : ""}）`,
      })),
    },
    {
      label: t("chat.singlesLabel"),
      options: singles.map((s) => ({ value: `single:${s.identifier}`, label: s.name })),
    },
  ];

  /** 会话归属的组名（列表副标题） */
  const groupNameOf = useCallback(
    (gid: string) => groups.find((g) => g.id === gid)?.name ?? gid,
    [groups],
  );

  /** 会话列表（按类型与关键字过滤） */
  const visibleSessions = useMemo(
    () =>
      sessions
        .filter((s) =>
          sessionKind === "all" ? true : sessionKind === "channel" ? s.title.startsWith("channel:") : !s.title.startsWith("channel:"),
        )
        .filter((s) => !sessionFilter || s.title.toLowerCase().includes(sessionFilter.toLowerCase())),
    [sessions, sessionKind, sessionFilter],
  );

  /** 按活跃时间分组：今天 / 昨天 / 更早（移动端列表页的段落结构） */
  const sessionGroups = useMemo(() => {
    const startOfToday = new Date();
    startOfToday.setHours(0, 0, 0, 0);
    const startOfYesterday = new Date(startOfToday);
    startOfYesterday.setDate(startOfYesterday.getDate() - 1);
    const buckets: { key: "today" | "yesterday" | "earlier"; labelKey: string; items: typeof sessions }[] = [
      { key: "today", labelKey: "chat.groupToday", items: [] },
      { key: "yesterday", labelKey: "chat.groupYesterday", items: [] },
      { key: "earlier", labelKey: "chat.groupEarlier", items: [] },
    ];
    for (const s of visibleSessions) {
      const ts = new Date(s.lastActiveAt ?? s.updatedAt ?? s.createdAt).getTime();
      const bucket = Number.isNaN(ts)
        ? buckets[2]
        : ts >= startOfToday.getTime()
          ? buckets[0]
          : ts >= startOfYesterday.getTime()
            ? buckets[1]
            : buckets[2];
      bucket.items.push(s);
    }
    return buckets.filter((b) => b.items.length > 0);
  }, [visibleSessions]);

  const removeSession = async (id: string) => {
    try {
      await api.deleteSession(id);
      // 从最新状态构建列表（不用渲染闭包的旧快照），无论删的是否为当前会话都先落列表
      const rest = useExm.getState().sessions.filter((s) => s.id !== id);
      useExm.setState({ sessions: rest });
      if (id === sessionId) {
        useExm.setState({ sessionId: undefined, messages: [], graph: null });
        if (rest.length > 0) {
          await selectSession(rest[0].id);
        } else {
          await newSession();
        }
      }
      // 列表删空后留在列表页（窄屏），否则会落在一个空会话页上
      if (rest.length === 0 && narrow) setScreen("list");
    } catch {
      // 删除失败静默：会话可能在运行中被网关拒绝
    }
  };

  // ---- 会话导出 Markdown（2.3）：角色前缀正文 + 工具摘要 + 思维链折叠引用 ----
  const exportSession = async (id: string, title: string) => {
    try {
      const msgs = await api.messages(id);
      const lines: string[] = [`# ${title}`, "", `> 导出时间：${new Date().toLocaleString()}`, ""];
      for (const m of msgs) {
        const role = m.role === "user" ? (m.agentId?.startsWith("auto:") ? "⚙️ 自动任务" : "👤 用户") : m.role === "orchestrator" ? (m.agentId ? `🤖 ${m.agentId}` : "🤖 指挥体") : "⚙️ 系统";
        lines.push(`## ${role} · ${m.createdAt}`, "");
        if (m.thinking) {
          lines.push("<details><summary>思维过程</summary>", "", "```", m.thinking, "```", "", "</details>", "");
        }
        if (m.toolCalls && m.toolCalls.length > 0) {
          lines.push("<details><summary>工具调用</summary>", "");
          for (const tc of m.toolCalls) {
            lines.push(`- \`${tc.tool}\` ${tc.status === "ok" ? "✅" : tc.status === "error" ? "❌" : "⏳"} ${tc.summary || ""}`);
          }
          lines.push("", "</details>", "");
        }
        lines.push(m.statements.map((st) => st.text).join("\n\n"), "");
      }
      const blob = new Blob([lines.join("\n")], { type: "text/markdown;charset=utf-8" });
      const url = URL.createObjectURL(blob);
      const a = document.createElement("a");
      a.href = url;
      a.download = `${title.replace(/[\\/:*?"<>|]/g, "_") || "session"}.md`;
      a.click();
      URL.revokeObjectURL(url);
      message.success(t("chat.exportDone"));
    } catch (e) {
      message.error(t("chat.exportFailed", { err: String(e).slice(0, 120) }));
    }
  };

  // ---- 会话历史管控（组 9.5）：撤销 / 编辑重发 / 分叉 ----
  const [editing, setEditing] = useState<{ turn: number; text: string } | null>(null);
  const [historyBusy, setHistoryBusy] = useState(false);

  const refreshSession = async () => {
    if (sessionId) await selectSession(sessionId);
  };

  const doUndo = async (turn: number) => {
    if (!sessionId) return;
    setHistoryBusy(true);
    try {
      const info = await api.undoSession(sessionId, turn - 1, true);
      message.success(t("chat.turn.undoDone", { n: String(info.archived) }));
      await refreshSession();
    } catch (e) {
      message.error(t("chat.turn.failed", { err: String(e) }));
    } finally {
      setHistoryBusy(false);
    }
  };

  const openEdit = (turn: number) => {
    // 原文 = 该轮的用户消息文本（本轮内第一条 user 陈述拼接）
    let n = 0;
    const msg = messages.find((m) => {
      if (m.role === "user") n += 1;
      return n === turn;
    });
    setEditing({ turn, text: msg ? msg.statements.map((s) => s.text).join("\n") : "" });
  };

  const doEdit = async () => {
    if (!sessionId || !editing) return;
    setHistoryBusy(true);
    try {
      await api.editSession(sessionId, editing.turn, editing.text);
      message.success(t("chat.turn.resent"));
      setEditing(null);
      await refreshSession();
    } catch (e) {
      message.error(t("chat.turn.failed", { err: String(e) }));
    } finally {
      setHistoryBusy(false);
    }
  };

  const doFork = async (turn: number) => {
    if (!sessionId) return;
    setHistoryBusy(true);
    try {
      const fork = await api.forkSession(sessionId, turn);
      useExm.setState({ sessions: [{ id: fork.id, title: fork.title } as never, ...useExm.getState().sessions] });
      message.success(t("chat.turn.forkDone"));
      await selectSession(fork.id);
    } catch (e) {
      message.error(t("chat.turn.failed", { err: String(e) }));
    } finally {
      setHistoryBusy(false);
    }
  };

  // 消息 → 轮次映射：用户消息序号即轮次（1 起）
  let userTurn = 0;
  const renderMessages = messages.map((m) => {
    if (m.role !== "user") return { m, turn: undefined as number | undefined };
    userTurn += 1;
    return { m, turn: userTurn };
  });

  return (
    <div className={`chat-shell screen-${screen}`} data-screen={screen}>
      {modalContextHolder}
      {/* 左栏 = 移动端「会话列表页」，桌面端是常驻侧栏（见 styles.css 的屏幕切换） */}
      <aside className="chat-rail">
        {/* 列表页标题栏（窄屏显示）：标题 + 会话计数 */}
        <div className="rail-head">
          <span className="rail-head-title">{t("chat.sessions")}</span>
          <span className="rail-head-count">{visibleSessions.length}</span>
        </div>
        <div className="rail-section">
          <div className="rail-label">
            <span className="rail-no">01</span> {t("chat.target")} [TARGET]
          </div>
          <Select
            value={targetValue}
            style={{ width: "100%" }}
            className="target-select"
            onChange={(v) => void changeTarget(v)}
            options={targetOptions}
          />
          {target.mode === "single" ? (
            <div className="rail-hint">{t("chat.soloMode")} · <span className="mono">{agentIdEn(target.id)}</span></div>
          ) : (
            activeMeta && (
              <div className="rail-hint">{t("chat.primary")}：<span className="mono">{activeMeta.primary ? agentIdEn(activeMeta.primary) : t("chat.unset")}</span></div>
            )
          )}
        </div>
        <div className="rail-section grow">
          {/* 工具条：搜索 + 类型过滤（同行紧凑排布，见样式表） */}
          <div className="rail-toolbar">
            <Input
              size="small"
              allowClear
              prefix={<SearchOutlined className="rail-search-icon" />}
              placeholder={t("chat.searchSessions")}
              className="session-search"
              value={sessionFilter}
              onChange={(e) => setSessionFilter(e.target.value)}
            />
            {/* 会话类型过滤（2.2）：全部 / 普通 / 通道（segment 紧凑形态，宽度自适应） */}
            <Select
              size="small"
              value={sessionKind}
              onChange={setSessionKind}
              className="session-kind"
              popupMatchSelectWidth={false}
              options={[
                { value: "all", label: t("chat.kind.all") },
                { value: "normal", label: t("chat.kind.normal") },
                { value: "channel", label: t("chat.kind.channel") },
              ]}
            />
          </div>
          <div className="session-list">
            {sessions.length === 0 && !sessionsLoaded && <SkeletonList rows={6} />}
            {sessionGroups.map((g, gi) => (
              <section className="session-group" key={g.key}>
                <header className="session-group-head">
                  <span className="session-group-left">
                    <span className="session-group-label">{t(g.labelKey)}</span>
                    <span className="session-group-count">{g.items.length}</span>
                  </span>
                  {/* 新建入口跟着第一段走：省一整行控制条，也符合移动端列表的惯例 */}
                  {gi === 0 && (
                    <Button
                      type="text"
                      size="small"
                      icon={<PlusOutlined />}
                      className="rail-new-session"
                      onClick={() => {
                        void newSession();
                        setScreen("thread"); // 竖屏：新建后直接进会话页
                      }}
                    >
                      {t("session.new")}
                    </Button>
                  )}
                </header>
                {g.items.map((s) => (
                  <div
                    key={s.id}
                    className={`session-row ${s.id === sessionId ? "active" : ""}`}
                    onClick={() => {
                      void selectSession(s.id);
                      if (narrow) setScreen("thread"); // 竖屏：点会话才进入该会话页
                    }}
                  >
                    <span className="session-bar" />
                    {/* 头像承载「会话类型」而非组名首字母：副标题已经写了组名，
                     * 首字母与之一一对应（整组同字母）＝零信息；类型恰好是列表里可筛的维度。 */}
                    {(() => {
                      const isChannel = s.title.startsWith("channel:");
                      const isSub = s.title.startsWith("sub:");
                      const subAgent = isSub ? s.title.split(":")[1] : "";
                      const kind = isChannel ? "channel" : isSub ? "sub" : "normal";
                      return (
                        <>
                          <span className={`session-avatar kind-${kind}`} aria-hidden="true">
                            {isChannel ? <CloudUploadOutlined /> : isSub ? <TeamOutlined /> : <MessageOutlined />}
                          </span>
                          <span className="session-main">
                            <span className="session-title">
                              {isChannel
                                ? s.title.replace(/^channel:[^:]+:/, "↗ ")
                                : isSub
                                  ? s.title.replace(/^sub:[^:]+:/, "")
                                  : s.title}
                            </span>
                            <span className="session-sub">
                              {isSub ? `@${subAgent} · ${t("chat.subSession")}` : groupNameOf(s.groupId)}
                            </span>
                          </span>
                        </>
                      );
                    })()}
                    {/* 操作收进「⋮」菜单：此前三个图标常驻，窄屏上既挤占标题又换行 */}
                    <span className="session-ops" onClick={(e) => e.stopPropagation()}>
                      <Dropdown
                        trigger={["click"]}
                        menu={{
                          items: [
                            { key: "export", icon: <ExportOutlined />, label: t("chat.export") },
                            { key: "rename", icon: <EditOutlined />, label: t("common.rename") },
                            { type: "divider" },
                            { key: "delete", icon: <CloseOutlined />, label: t("chat.delete"), danger: true },
                          ],
                          onClick: ({ key, domEvent }) => {
                            domEvent.stopPropagation();
                            if (key === "export") void exportSession(s.id, s.title);
                            else if (key === "rename") setRenaming({ id: s.id, title: s.title });
                            else {
                              modal.confirm({
                                title: t("chat.deleteConfirm"),
                                okText: t("common.confirm"),
                                okButtonProps: { danger: true },
                                cancelText: t("common.cancel"),
                                onOk: () => removeSession(s.id),
                              });
                            }
                          },
                        }}
                      >
                        <Button
                          size="small"
                          type="text"
                          className="session-more"
                          icon={<MoreOutlined />}
                          aria-label={t("chat.moreOps")}
                          onClick={(e) => e.stopPropagation()}
                        />
                      </Dropdown>
                    </span>
                    <span className="session-time">{relativeTime(s.lastActiveAt ?? s.createdAt)}</span>
                  </div>
                ))}
              </section>
            ))}
            {visibleSessions.length === 0 && (
              <div className="rail-empty">
                <p className="dim">{sessionFilter ? t("chat.noMatch") : t("chat.noSessions")}</p>
                {!sessionFilter && (
                  <Button
                    type="primary"
                    icon={<PlusOutlined />}
                    onClick={() => {
                      void newSession();
                      setScreen("thread");
                    }}
                  >
                    {t("session.new")}
                  </Button>
                )}
              </div>
            )}
          </div>
        </div>
      </aside>

      {/* 右侧：消息流 + 输入 */}
      <div className="chat-wrap">
        <div className="console-bar">
          <span className="console-title">{t("nav.chat")}</span>
          <span className="page-en">CHAT</span>
          <span className="console-sep" />
          <span className="readout"><span className="k">MODE</span> <span className="v">{target.mode === "single" ? "SOLO" : "GROUP"}</span></span>
          <span className="readout"><span className="k">STATE</span> <span className={`v${running ? " v-live" : ""}`}>{running ? "RUNNING" : "IDLE"}</span></span>

          {usage && (
            <span className="readout console-usage">
              <span className="k">TOKENS</span>
              {/* 字符进度条：比数字更直观地表达「还剩多少预算」；实测用量不带 ~ 前缀 */}
              <AsciiMeter value={usage.estimate} total={usage.unlimited ? 0 : usage.budget} cells={14} />
              <span className="v">{usage.measured ? usage.estimate : `~${usage.estimate}`}</span>
            </span>
          )}
          {/* 竖屏：返回会话列表页（列表页与本页是两个页面，不是叠加抽屉） */}
          {narrow && (
            <button className="rail-back" onClick={() => setScreen("list")} aria-label={t("chat.sessions")}>
              <ArrowLeftOutlined />
              <span className="rail-back-label">{t("chat.sessionsShort")}</span>
            </button>
          )}
          <span className={`status-led ${wsConnected ? "ok" : "bad"}`} style={{ marginLeft: "auto" }} />
        </div>
        {!wsConnected && <Alert type="warning" message={t("chat.wsDown")} showIcon className="ws-alert app-banner" />}
        {/* 计划模式（spec plan-mode：WebUI 跨端审批呈现）——模式开关 + 计划卡片 */}
        <div className="plan-mode-bar">
          <Segmented
            size="small"
            value={plan?.mode ?? "execute"}
            onChange={(v) => void setPlanMode(v === "plan")}
            options={[
              { value: "execute", label: t("plan.modeExecute") },
              { value: "plan", label: t("plan.modePlan") },
            ]}
          />
          {plan?.mode === "plan" && <span className="plan-mode-hint">{t("plan.modeHint")}</span>}
          {/* 任务清单进度（spec agent-todo）：todo.updated 实时刷新 */}
          {todo && todo.count > 0 && (
            <span className={`todo-progress${todo.allDone ? " todo-all-done" : ""}`}>
              <span className="todo-progress-label">📋 {t("todo.progress", { done: String(todo.doneCount), total: String(todo.count) })}</span>
              <span className="todo-bar">
                <span
                  className="todo-bar-fill"
                  style={{ width: `${Math.round((todo.doneCount / todo.count) * 100)}%` }}
                />
              </span>
            </span>
          )}
        </div>
        {todo && todo.count > 0 && (
          <div className="todo-strip">
            {todo.items.map((it) => (
              <span key={it.id} className={`todo-chip todo-${it.status}`}>
                {it.status === "done" ? "✅" : it.status === "in_progress" ? "🔄" : "⬜"} {it.content}
              </span>
            ))}
          </div>
        )}
        {/* 后台子任务卡（spec subagent-spawn WebUI 呈现）：运行中 spinner + 终态展开 + 清除 */}
        {spawns.length > 0 && (
          <div className="spawns-strip">
            {spawns.map((s) => (
              <div key={s.spawnId} className={`spawn-card spawn-${s.status}`}>
                <div
                  className="spawn-card-head"
                  onClick={() => setExpandedSpawns((prev) => (prev.includes(s.spawnId) ? prev.filter((x) => x !== s.spawnId) : [...prev, s.spawnId]))}
                >
                  <span className="spawn-status-icon">
                    {s.status === "running" ? (
                      <Spin size="small" />
                    ) : s.status === "done" ? (
                      "✅"
                    ) : s.status === "timeout" ? (
                      "⏱"
                    ) : (
                      "❌"
                    )}
                  </span>
                  <span className="spawn-preview">{s.preview || s.spawnId}</span>
                  {s.status !== "running" && s.resultPreview && (
                    <span className="spawn-expand-hint">
                      {expandedSpawns.includes(s.spawnId) ? t("spawn.foldLess") : t("spawn.foldMore")}
                    </span>
                  )}
                </div>
                {s.status !== "running" && s.resultPreview && expandedSpawns.includes(s.spawnId) && (
                  <pre className="spawn-result">{s.resultPreview}</pre>
                )}
              </div>
            ))}
            <Button type="link" size="small" className="spawn-clear" onClick={clearSpawns}>
              {t("spawn.clear")}
            </Button>
          </div>
        )}
        {plan && (plan.items.length > 0 || planPending) && (
          <PlanCard
            info={plan}
            applied={planApplied}
            pending={planPending}
            onApprove={() => void decidePlan("approve")}
            onReject={() => void decidePlan("reject")}
          />
        )}
        <div className="chat-scroll">
          {/* 空态：当前会话尚无消息且未在运行 —— 目标捕获盘（与桌面端空态同源母题） */}
          {renderMessages.length === 0 && !running && (
            <div className="chat-acquire">
              <span className="capture-frame chat-acquire-figure">
                <OrbitRings />
                <Emblem size={224} />
              </span>
              <div className="chat-acquire-title">{t("chat.acquireTitle")}</div>
              <div className="chat-acquire-desc">{t("chat.acquireDesc")}</div>
              <div className="chat-acquire-readouts">
                <span className="readout"><span className="k">TGT</span><span className="v">{orchLabel}</span></span>
                <span className="readout"><span className="k">GROUP</span><span className="v">{activeMeta?.name ?? activeGroup}</span></span>
                <span className="readout"><span className="k">LINK</span><span className={`v ${wsConnected ? "ok" : "err"}`}>{wsConnected ? "ONLINE" : "RECONN"}</span></span>
              </div>
            </div>
          )}
          {renderMessages.map(({ m, turn }) => (
            <MessageBubble
              key={m.id}
              m={m}
              turn={turn}
              orchLabel={orchLabel}
              ops={
                turn != null && !running ? (
                  <TurnOps turn={turn} onEdit={openEdit} onUndo={doUndo} onFork={doFork} />
                ) : undefined
              }
            />
          ))}

          {/* 等待首个输出：加载卡（一旦有流式输出/子个体产出即让位，避免与输出卡并存） */}
          {running && !liveOrch && liveUnitEntries.length === 0 && (
            <Card size="small" className="msg-bubble msg-agent">
              <Space direction="vertical" className="full-width">
                <div>
                  <Spin size="small" /> <b>{`${orchLabel} …`}</b>
                </div>
                {(liveThinking.trim() || timeline.length > 0 || runToolCalls.length > 0) && (
                  <div className="msg-process">
                    {liveThinking.trim() && (
                      <UsageBar label={t("chat.usedThinking", { n: String(liveThinking.length) })} live>
                        <pre className="usage-think">{liveThinking}</pre>
                      </UsageBar>
                    )}
                    {(timeline.length > 0 || runToolCalls.length > 0) && (
                      <UsageBar label={t("chat.usedTools", { n: runToolCalls.length + timeline.length })} live>
                        <TimelineRows items={timeline} tb={t} onOpenSub={(id) => void selectSession(id)} />
                        {runToolCalls.length > 0 && <ToolCallList calls={runToolCalls} />}
                      </UsageBar>
                    )}
                  </div>
                )}
              </Space>
            </Card>
          )}

          {/* 本轮调度轨迹（收束后保留）：派发/回流条目 + 子会话入口，直到下一次发送或切会话 */}
          {!running && timeline.length > 0 && timelineSession === sessionId && (
            <Card size="small" className="msg-bubble msg-agent turn-timeline-card">
              <div className="msg-process">
                <UsageBar label={t("chat.turnTimeline", { n: String(timeline.length) })} defaultOpen>
                  <TimelineRows items={timeline} tb={t} onOpenSub={(id) => void selectSession(id)} />
                </UsageBar>
              </div>
            </Card>
          )}

          {liveUnitEntries.map(([agent, parts]) => (
            <Card key={agent} size="small" className="msg-bubble msg-agent live-card">
              <div className="msg-head">
                <RobotOutlined /> <b>{agent}</b> <Tag color="processing">{t("chat.executing")}</Tag>
              </div>
              {/* 思考/工具/输出按发生顺序交错展示（连续工具调用聚合为同一列表） */}
              <ProcessSteps
                parts={parts}
                toolCalls={runToolCalls}
                live
                labelThinking={(n) => t("chat.usedThinking", { n: String(n) })}
                labelTools={(n) => t("chat.usedTools", { n: String(n) })}
              />
            </Card>
          ))}

          {liveOrch && (
            <Card size="small" className="msg-bubble msg-agent live-card">
              <div className="msg-head">
                <RobotOutlined /> <b>{orchLabel}</b> <Tag color="processing">{t("chat.streaming")}</Tag>
              </div>
              {(liveThinking.trim() || timeline.length > 0 || runToolCalls.length > 0) && (
                <div className="msg-process">
                  {liveThinking.trim() && (
                    <UsageBar label={t("chat.usedThinking", { n: String(liveThinking.length) })} live>
                      <pre className="usage-think">{liveThinking}</pre>
                    </UsageBar>
                  )}
                  {(timeline.length > 0 || runToolCalls.length > 0) && (
                    <UsageBar label={t("chat.usedTools", { n: runToolCalls.length + timeline.length })} live>
                      <TimelineRows items={timeline} tb={t} onOpenSub={(id) => void selectSession(id)} />
                      {runToolCalls.length > 0 && <ToolCallList calls={runToolCalls} />}
                    </UsageBar>
                  )}
                </div>
              )}
              <pre className="live-pre">{liveOrch}</pre>
            </Card>
          )}
          {/* 内联审批：渲染在消息流内（live 区之后、贴底锚之前）——
              审批到达时随消息流滚动进视线；原先渲染在 chat-scroll 外的静态区，
              流式输出期间用户盯着底部，上方的卡根本看不到（审批交互不可见 bug） */}
          {approvals.map((a) => (
            <Card key={a.approvalId} size="small" className="approval-card">
              <div className="approval-head">
                <span className="approval-title">⏸ {t("chat.approvalTitle")}</span>
                <span className="approval-agent mono">{a.agentId}</span>
              </div>
              <pre className="approval-cmd">{a.command}</pre>
              <div className="approval-ops">
                <Button
                  type="primary"
                  size="small"
                  icon={<CheckOutlined />}
                  onClick={() => void decideApproval(a.approvalId, true)}
                >
                  {t("chat.approvalApprove")}
                </Button>
                <Button size="small" danger onClick={() => void decideApproval(a.approvalId, false)}>
                  {t("chat.approvalDeny")}
                </Button>
                {a.sessionId && a.sessionId !== sessionId && (
                  <Button size="small" type="link" onClick={() => void selectSession(a.sessionId!)}>
                    {t("chat.openSubSession")}
                  </Button>
                )}
                <span className="approval-hint">{t("chat.approvalHint")}</span>
              </div>
            </Card>
          ))}
          {showBottomBtn && (
            <button
              className="scroll-bottom-btn"
              aria-label={t("chat.scrollToBottom")}
              onClick={() => {
                stickRef.current = true;
                bottomRef.current?.scrollIntoView({ behavior: "smooth" });
              }}
            >
              ↓
            </button>
          )}
          <div ref={bottomRef} />
        </div>

        <div className="chat-composer">
          <FileMentionPopup
            nodes={mentionNodes}
            loading={mentionLoading}
            state={mention}
            idx={mentionIdx}
            onIdxChange={setMentionIdx}
            onPick={applyMention}
          />
          {images.length > 0 && (
            <div className="attach-row">
              {images.map((src, i) => (
                <span key={i} className="attach-thumb">
                  <img src={src} alt={`附件${i + 1}`} />
                  <Button
                    size="small"
                    type="text"
                    className="attach-del"
                    icon={<CloseOutlined />}
                    onClick={() => setImages(images.filter((_, j) => j !== i))}
                  />
                </span>
              ))}
            </div>
          )}
          <Input.TextArea
            className="composer-textarea"
            value={text}
            disabled={isSubSession}
            onChange={(e) => {
              setText(e.target.value);
              detectMention(e.target.value, e.target.selectionStart ?? e.target.value.length);
              if (sessionId) useExm.getState().setDraft(sessionId, e.target.value);
            }}
            onBlur={() => {
              // 延迟关闭：给 onMouseDown 选中留出事件窗口
              setTimeout(() => setMention((m) => ({ ...m, open: false })), 150);
            }}
            autoSize={{ minRows: 1, maxRows: 6 }}
            onPressEnter={(e) => {
              if (mention.open) return; // 提及弹层接管 Enter
              if (!e.shiftKey) {
                e.preventDefault();
                if (text.trim() && !running && !isSubSession) {
                  doSend();
                }
              }
            }}
            onKeyDown={(e) => {
              if (onMentionKey(e)) return; // 提及弹层优先接管方向键/确认/关闭
              // 上/下箭头召回历史用户消息（chat-ergonomics）：仅在空输入时接管
              if (e.key !== "ArrowUp" && e.key !== "ArrowDown") return;
              if (text.trim()) return;
              const userMsgs = messages.filter((m) => m.role === "user");
              if (userMsgs.length === 0) return;
              e.preventDefault();
              let idx = recallIdxRef.current;
              if (e.key === "ArrowUp") {
                idx = idx === null ? userMsgs.length - 1 : Math.max(0, idx - 1);
              } else {
                if (idx === null) return;
                idx = idx + 1;
                if (idx >= userMsgs.length) {
                  recallIdxRef.current = null;
                  setText("");
                  return;
                }
              }
              recallIdxRef.current = idx;
              const recalled = userMsgs[idx]?.statements.map((st) => st.text).join("\n") ?? "";
              setText(recalled);
            }}
          />
          <div className="composer-bar">
            <div className="composer-tools">
              <label className="attach-btn composer-attach" title={t("chat.attachTip")}>
                <PlusOutlined />
                <input
                  type="file"
                  accept="image/*"
                  multiple
                  style={{ display: "none" }}
                  onChange={(e) => {
                    Array.from(e.target.files ?? []).slice(0, 4).forEach(addImage);
                    e.target.value = "";
                  }}
                />
              </label>
              <span className="composer-model">
                <RobotOutlined />
                <Select
                  value={currentModel}
                  onChange={(v) => void changeModel(v)}
                  options={modelOptions}
                  variant="borderless"
                  size="small"
                  className="composer-model-select"
                  popupMatchSelectWidth={false}
                  disabled={isSubSession || profiles.length === 0}
                />
              </span>
            </div>
            <div className="composer-actions">
              <span className="composer-hint">{t("chat.enterHint")}</span>
              {running && sessionId ? (
                <Popconfirm
                  title={t("chat.stopConfirm")}
                  onConfirm={() => {
                    if (!sessionId) return;
                    api.stopSession(sessionId)
                      .then((r) => message.success(t("chat.stopDone", { n: String(r.toolsExecuted) })))
                      .catch((e) => message.error(t("chat.stopFail", { err: String(e).slice(0, 120) })));
                  }}
                >
                  <Button
                    className="composer-send composer-stop"
                    shape="circle"
                    danger
                    icon={<PauseCircleOutlined />}
                    aria-label={t("chat.stop")}
                  />
                </Popconfirm>
              ) : (
                <Button
                  className="composer-send"
                  shape="circle"
                  icon={<ArrowUpOutlined />}
                  disabled={isSubSession || running || !text.trim()}
                  onClick={doSend}
                  aria-label={t("chat.send")}
                />
              )}
            </div>
          </div>
        </div>
        {renaming && (
          <Modal
            open
            title={t("chat.renameTitle")}
            onCancel={() => setRenaming(null)}
            onOk={async () => {
              if (!renaming.title.trim()) return;
              await api.renameSession(renaming.id, renaming.title);
              const store = useExm.getState();
              useExm.setState({ sessions: store.sessions.map((s) => s.id === renaming.id ? { ...s, title: renaming.title } : s) });
              setRenaming(null);
            }}
            okText={t("common.rename")}
          >
            <Input
              value={renaming.title}
              onChange={(e) => setRenaming({ ...renaming, title: e.target.value })}
              placeholder={t("chat.newTitle")}
            />
          </Modal>
        )}
        {editing && (
          <Modal
            open
            title={t("chat.turn.editTitle", { turn: String(editing.turn) })}
            onCancel={() => setEditing(null)}
            onOk={() => void doEdit()}
            okText={t("chat.turn.resend")}
            confirmLoading={historyBusy}
          >
            <Alert
              type="warning"
              showIcon
              message={t("chat.turn.editWarn")}
              className="edit-warn"
            />
            <Input.TextArea
              value={editing.text}
              onChange={(e) => setEditing({ ...editing, text: e.target.value })}
              autoSize={{ minRows: 3, maxRows: 10 }}
            />
          </Modal>
        )}
      </div>
    </div>
  );
}