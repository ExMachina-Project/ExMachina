/** 主框架：纯导航侧栏——组切换在对话页内，组管理独立成页 */
import React, { lazy, Suspense, useEffect, useRef, useState } from "react";
import { message, Menu, Tag } from "antd";
import {
  ApartmentOutlined,
  BellOutlined,
  ClusterOutlined,
  CloseOutlined,
  DatabaseOutlined,
  FileTextOutlined,
  MenuOutlined,
  MessageOutlined,
  ApiOutlined,
  SettingOutlined,
  UserOutlined,
  FundOutlined,
  ClockCircleOutlined,
  CloudUploadOutlined,
  UnorderedListOutlined,
  AppstoreAddOutlined,
  DeploymentUnitOutlined,
  CommentOutlined,
  ThunderboltOutlined,
} from "@ant-design/icons";
import { api } from "./api";
import { isTaskSideTitle } from "./session";
import { SubbarSlotContext } from "./components/Subbar";
import { useExm } from "./store";
import { useT, type TKey } from "./i18n/core";
import { ACCENTS, accentColor, THEMES, useTheme, type ThemeKey } from "./theme";
import { CharStream } from "./components/CharStream";
import { CommandPalette } from "./components/CommandPalette";
import { DataStream } from "./components/DataStream";
import { Emblem } from "./components/Emblem";
import { OrbitRings } from "./components/OrbitRings";
import { TelemetryBar } from "./components/TelemetryBar";
/* 视图懒加载（React.lazy + 动态 import）：按视图切分 chunk，首屏只拉 shell + 当前视图，
 * 其余视图在导航到时才加载（vendor 三方库另行在 vite.config.ts 的 manualChunks 分组缓存） */
const LoginView = lazy(() => import("./views/LoginView").then((m) => ({ default: m.LoginView })));
const SetupKeyView = lazy(() => import("./views/SetupKeyView").then((m) => ({ default: m.SetupKeyView })));
const ChatView = lazy(() => import("./views/ChatView").then((m) => ({ default: m.ChatView })));
const FlowsView = lazy(() => import("./views/FlowsView").then((m) => ({ default: m.FlowsView })));
const TasksView = lazy(() => import("./views/TasksView").then((m) => ({ default: m.TasksView })));
const SessionsView = lazy(() => import("./views/SessionsView").then((m) => ({ default: m.SessionsView })));
const ModelsView = lazy(() => import("./views/ModelsView").then((m) => ({ default: m.ModelsView })));
const GraphView = lazy(() => import("./views/GraphView").then((m) => ({ default: m.GraphView })));
const LedgerView = lazy(() => import("./views/LedgerView").then((m) => ({ default: m.LedgerView })));
const MemoryView = lazy(() => import("./views/MemoryView").then((m) => ({ default: m.MemoryView })));
const SettingsView = lazy(() => import("./views/settings/SettingsView").then((m) => ({ default: m.SettingsView })));
const NexusView = lazy(() => import("./views/NexusView").then((m) => ({ default: m.NexusView })));
const SinglesView = lazy(() => import("./views/SinglesView").then((m) => ({ default: m.SinglesView })));
const SkillsView = lazy(() => import("./views/SkillsView").then((m) => ({ default: m.SkillsView })));
const PluginsView = lazy(() => import("./views/PluginsView").then((m) => ({ default: m.PluginsView })));
const AutomationsView = lazy(() => import("./views/AutomationsView").then((m) => ({ default: m.AutomationsView })));
const ApprovalsView = lazy(() => import("./views/ApprovalsView").then((m) => ({ default: m.ApprovalsView })));
const ChannelsView = lazy(() => import("./views/channels/ChannelsView").then((m) => ({ default: m.ChannelsView })));
const ActivityView = lazy(() => import("./views/ActivityView").then((m) => ({ default: m.ActivityView })));

/** 懒加载 chunk 未就绪时的兜底：品牌徽记 + 等宽字状态行（替代裸 Spin） */
function ViewSuspense({ children }: { children: React.ReactNode }): React.ReactElement {
  return (
    <Suspense
      fallback={
        <div className="view-loading">
          <Emblem size={56} charging />
          <span className="view-loading-text">LOADING</span>
        </div>
      }
    >
      {children}
    </Suspense>
  );
}

type ViewKey =
  | "chat"
  | "flows"
  | "tasks"
  | "sessions"
  | "graph"
  | "agent"
  | "nexus"
  | "skills"
  | "plugins"
  | "models"
  | "automations"
  | "approvals"
  | "channels"
  | "ledger"
  | "memory"
  | "activity"
  | "settings";

/** 合法视图键（hash 路由白名单校验） */
const VIEW_KEYS: ViewKey[] = [
  "chat", "flows", "tasks", "sessions", "agent", "nexus", "graph", "automations",
  "skills", "plugins", "models", "approvals", "channels", "ledger", "memory", "activity", "settings",
];

/** 旧路由兼容（agent-groups-merge）：独立组页已并入「智能体」页，#/groups 落到 #/agent */
const LEGACY_VIEW_ALIASES: Record<string, ViewKey> = { groups: "agent" };

/** 从 hash 解析视图：#/chat/<sessionId> → { view: chat, session }; 非法回落 chat */
function parseHash(): { view: ViewKey; session?: string } {
  const h = location.hash.replace(/^#\/?/, "");
  if (!h) return { view: "chat" };
  const [head, sub] = h.split("/");
  // 旧路由别名先行（groups → agent），再匹配白名单；非法回落 chat
  const view = LEGACY_VIEW_ALIASES[head] ?? VIEW_KEYS.find((k) => k === head);
  if (!view) return { view: "chat" };
  return { view, session: sub };
}

/** 侧栏清单 */
export const NAV_ITEMS: {
  key: ViewKey;
  en: string;
  labelKey: string;
  icon: React.ReactNode;
  /** 实验性功能标记：侧栏显示「实验」角标 */
  experimental?: boolean;
}[] = [
  { key: "chat", en: "CHAT", labelKey: "nav.chat", icon: <MessageOutlined /> },
  { key: "flows", en: "FLOWS", labelKey: "nav.flows", icon: <CommentOutlined /> },
  { key: "tasks", en: "TASKS", labelKey: "nav.tasks", icon: <ThunderboltOutlined /> },
  { key: "agent", en: "AGENTS", labelKey: "nav.agent", icon: <UserOutlined /> },
  { key: "nexus", en: "NEXUS", labelKey: "nav.nexus", icon: <ClusterOutlined />, experimental: true },
  { key: "sessions", en: "SESSIONS", labelKey: "nav.sessions", icon: <UnorderedListOutlined /> },
  { key: "graph", en: "DAG", labelKey: "nav.dag", icon: <ApartmentOutlined /> },
  { key: "automations", en: "CRON", labelKey: "nav.cron", icon: <ClockCircleOutlined /> },
  { key: "skills", en: "SKILLS", labelKey: "nav.skills", icon: <DeploymentUnitOutlined /> },
  { key: "plugins", en: "PLUGINS", labelKey: "nav.plugins", icon: <AppstoreAddOutlined /> },
  { key: "models", en: "MODELS", labelKey: "nav.models", icon: <ApiOutlined /> },
  { key: "approvals", en: "APPROVALS", labelKey: "nav.approvals", icon: <BellOutlined /> },
  { key: "channels", en: "CHANNELS", labelKey: "nav.channels", icon: <CloudUploadOutlined /> },
  { key: "ledger", en: "LEDGER", labelKey: "nav.ledger", icon: <FundOutlined /> },
  { key: "memory", en: "MEMORY", labelKey: "nav.memory", icon: <DatabaseOutlined /> },
  { key: "activity", en: "EVENTS", labelKey: "nav.events", icon: <FileTextOutlined /> },
  { key: "settings", en: "CONFIG", labelKey: "nav.settings", icon: <SettingOutlined /> },
];

export default function App(): React.ReactElement {
  const config = useExm((s) => s.config);
  const wsConnected = useExm((s) => s.wsConnected);
  const init = useExm((s) => s.init);
  const pendingApprovals = useExm((s) => s.pendingApprovals);
  // 聊天未读徽标：通道流未读总数（30s 轮询 + run.finished 事件驱动的刷新链路）
  const [flowUnread, setFlowUnread] = useState(0);
  useEffect(() => {
    let dead = false;
    const tick = () =>
      void api
        .listChannelFlows()
        .then((fs) => !dead && setFlowUnread(fs.reduce((n, f) => n + (f.unread ?? 0), 0)))
        .catch(() => undefined);
    void tick();
    const h = () => void tick();
    const iv = setInterval(h, 60000);
    window.addEventListener("exm:flows-refresh", h);
    return () => {
      dead = true;
      clearInterval(iv);
      window.removeEventListener("exm:flows-refresh", h);
    };
  }, []);
  const refreshPendingApprovals = useExm((s) => s.refreshPendingApprovals);
  // 审批待办轮询（60s 兜底）：WS 丢事件/页面刷新后角标仍可恢复——
  // 审批等待窗内（approvalWaitSecs）错过提示 = 工具受阻，必须有可靠提醒面
  useEffect(() => {
    void refreshPendingApprovals();
    const h = setInterval(() => void refreshPendingApprovals(), 60000);
    return () => clearInterval(h);
  }, [refreshPendingApprovals]);
  const accent = useTheme((s) => s.accent);
  const setAccent = useTheme((s) => s.setAccent);
  const preset = useTheme((s) => s.preset);
  const setPreset = useTheme((s) => s.setPreset);
  const initial = parseHash();
  const [view, setViewState] = useState<ViewKey>(initial.view);
  /** 切视图并同步 hash（app-shell-routing）
   *  保留 `#/chat/<sessionId>` 的会话段：此前三元恒为空串，深链进来第一跳就被抹平，
   *  移动端无法判断「是否深链直入会话页」，只能一律落在列表页。 */
  const setView = (v: ViewKey) => {
    setViewState(v);
    const seg = location.hash.startsWith("#/chat/") ? location.hash.slice("#/chat/".length) : "";
    const target = "#/" + v + (v === "chat" && seg ? "/" + seg : "");
    if (location.hash !== target) {
      history.replaceState(null, "", target);
    }
  };
  // 二级侧边栏（上下文）：当前 view 有页面侧栏内容才提供入口
  const ctxKind = view === "chat" ? "chat" : view === "flows" ? "flows" : view === "tasks" ? "tasks" : view === "settings" ? "settings" : null;
  const ctxTitleKey = ctxKind === "chat" ? "ctx.chat" : ctxKind === "flows" ? "ctx.flows" : ctxKind === "settings" ? "nav.settings" : "ctx.tasks";
  /** 当前页在 NAV_ITEMS 中的条目（页面手风琴行尾显示当前页名） */
  const currentNav = NAV_ITEMS.find((it) => it.key === view) ?? NAV_ITEMS[0];
  const navRequest = useExm((st) => st.navRequest);
  useEffect(() => {
    if (!navRequest) return;
    if (navRequest.view === "chat" && navRequest.sessionId) {
      // 路由感知（信息架构 2026-11）：目标会话若不是用户直连会话，落到对应页——
      // 通道流会话 → 「聊天」页；任务会话（job-* / 心跳巡检）→ 「任务」页。
      // 否则对话页左栏（已过滤这两类）看不到跳转目标，呈现「跳了但没到」。
      const sid = navRequest.sessionId;
      void api
        .getSession(sid)
        .then((s) => {
          const dest = s.title.startsWith("channel:")
            ? "flows"
            : isTaskSideTitle(s.title)
              ? "tasks"
              : "chat";
          setView(dest as ViewKey);
        })
        .catch(() => setView("chat"));
      void useExm.getState().selectSession(sid);
    } else {
      setView(navRequest.view as ViewKey);
    }
    useExm.setState({ navRequest: null });
  }, [navRequest]);
  // hashchange：前进/后退导航（hashchange 时不回写 hash，避免回环）
  useEffect(() => {
    const onHash = () => {
      const { view: v } = parseHash();
      setViewState(v);
    };
    window.addEventListener("hashchange", onHash);
    return () => window.removeEventListener("hashchange", onHash);
  }, []);

  // 键盘跳页（kbd-nav）：G 武装后接页首字母（gc=对话 gf=聊天 gt=任务 gs=会话
  // 管理 ga=智能体 gn=智能连结 gd=编排 gm=模型 gp=审批），或 1-9 直达前九项。
  // 输入框/文本域聚焦与修饰键按下时不触发，不干扰正常输入与浏览器快捷键。
  // 映射手选不自动派生：首字母冲突页（graph/automations 等）一律走数字直达。
  useEffect(() => {
    let armed = false;
    const order: ViewKey[] = NAV_ITEMS.map((i) => i.key);
    const goMap: Record<string, ViewKey> = {
      c: "chat",
      f: "flows",
      t: "tasks",
      s: "sessions",
      a: "agent",
      n: "nexus",
      d: "graph",
      m: "models",
      p: "approvals",
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.metaKey || e.ctrlKey || e.altKey) return;
      const el = e.target as HTMLElement | null;
      if (el && (el.tagName === "INPUT" || el.tagName === "TEXTAREA" || el.isContentEditable)) return;
      if (e.key === "g" || e.key === "G") {
        armed = true;
        return;
      }
      if (!armed) return;
      armed = false;
      const key = e.key.toLowerCase();
      const dest = /^[1-9]$/.test(key) ? order[Number(key) - 1] : goMap[key];
      if (dest && dest !== view) {
        setViewState(dest);
        history.replaceState(null, "", "#/" + dest);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [view]);
  /** 页面导航：全量平铺（2026-11 三次调整：手风琴展开收起被指多余，直接平铺）。
   *  二级侧边栏已彻底迁至右栏常驻，与左栏无关。 */
  const navMenuOpen = true;
  /** 二级侧边栏插槽节点（页面侧栏 portal 目标） */
  const [subbarSlot, setSubbarSlot] = useState<HTMLElement | null>(null);
  /** 底部主题手风琴（预设 + 强调色合并入口）：独立开关 */
  const [themeOpen, setThemeOpen] = useState(false);
  /** 移动端侧栏抽屉（≤860px 时侧栏覆盖呈现） */
  const [navOpen, setNavOpen] = useState(false);
  // Esc 关闭展开中的面板（输入框聚焦时除外——不干扰会话搜索等本地输入）。
  // 主题面板收起；页面导航已平铺无常开态，右栏独立，不受 Esc 影响。
  useEffect(() => {
    if (!themeOpen) return;
    const onKey = (e: KeyboardEvent) => {
      const el = e.target as HTMLElement | null;
      if (e.key === "Escape" && el?.tagName !== "INPUT" && el?.tagName !== "TEXTAREA") {
        setThemeOpen(false);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [themeOpen]);
  const t = useT();
  // 登录门：checking = 鉴权中；setup = 首启未设密钥（引导设置）；locked = 未解锁；open = 已进入
  const [gate, setGate] = useState<"checking" | "setup" | "locked" | "open">("checking");
  /** 通道就绪 = 全局端点已配置密钥（apiKey 由服务端掩码回显，非空即已配置） */
  const llmReady = !!config?.llm?.apiKey;

  useEffect(() => {
    void (async () => {
      try {
        // 首启引导（部署后首次启动）：未配置访问密钥且未「跳过 / 关闭」提醒 → 设置密钥页
        const st = await api.authStatus();
        if (!st.keyConfigured && !st.setupSnoozed && !st.setupDismissed) {
          setGate("setup");
          return;
        }
        const key = localStorage.getItem("exm.key") ?? "";
        const r = st.keyConfigured ? await api.verifyAuth(key) : { ok: true };
        setGate(r.ok ? "open" : "locked");
        if (r.ok) {
          await init();
          // 会话深链（app-shell-routing）：#/chat/<sessionId> → 选中该会话；
          // 目标若是通道/任务会话，视图同步落到「聊天」「任务」页（与 navRequest 同口径）
          const { view, session } = parseHash();
          if (view === "chat" && session) {
            const exists = useExm.getState().sessions.some((s) => s.id === session);
            if (exists) {
              const s = await api.getSession(session).catch(() => null);
              await useExm.getState().selectSession(session);
              if (s?.title.startsWith("channel:")) setViewState("flows");
              else if (s && isTaskSideTitle(s.title)) setViewState("tasks");
            } else {
              message.info("深链会话不存在，已切换到最近会话");
            }
          }
        }
      } catch {
        setGate("locked");
      }
    })();
  }, [init]);

  if (gate === "checking") {
    return (
      <div className="login-wrap">
        <OrbitRings />
        <DataStream side="left" />
        <DataStream side="right" />
        <div className="login-core">
          <div className="login-figure">
            <Emblem size={92} charging />
          </div>
          <span className="brand-name login-brand">EX·MACHINA</span>
          <div className="title-rule" style={{ marginTop: 12 }} />
          <div className="login-sub" style={{ textAlign: "center" }}>
            {t("status.connecting")} <span className="mono">[CONNECTING]</span>
          </div>
        </div>
      </div>
    );
  }
  if (gate === "setup") {
    return (
      <ViewSuspense>
        <SetupKeyView
          onEnter={() => {
            setGate("open");
            void init();
          }}
        />
      </ViewSuspense>
    );
  }
  if (gate === "locked") {
    return (
      <ViewSuspense>
        <LoginView
          onUnlock={() => {
            setGate("open");
            void init();
          }}
        />
      </ViewSuspense>
    );
  }

  return (
    <SubbarSlotContext.Provider value={subbarSlot}>
      <div className="app-shell">
      {/* 移动端抽屉开关（桌面隐藏） */}
      <button className="mobile-nav-btn" onClick={() => setNavOpen(!navOpen)} aria-label="menu">
        {navOpen ? <CloseOutlined /> : <MenuOutlined />}
      </button>
      {navOpen && <div className="nav-mask" onClick={() => setNavOpen(false)} />}
      <aside className={`app-sider ${navOpen ? "open" : ""} top-open`}>
        {/* 字符流放在侧栏背景里（见 styles.css 的层级说明） */}
        <CharStream />
        {/* 品牌区：LED + 名称一行，副标语保留独立第二行（2026-11 回退：
            副标语并行尾挤成一行被斥「谁叫你压成这样的」——只减 padding 不并排） */}
        <div className="brand">
          <div className="brand-row">
            <span className="brand-led" />
            <span className="brand-name">EX·MACHINA</span>
          </div>
          <div className="brand-sub">{t("brand.sub")}</div>
        </div>

        {/* ── 页面导航：全量平铺（NAV_ITEMS 全量入口 + 角标；与原菜单同款行）──
            标题头已删（2026-11：平铺形态下「页面」标题无信息量，被指留着干嘛） */}
        <div className="theme-switcher acc-nav open">
          <div className="theme-panel">
            <Menu
              mode="inline"
              selectedKeys={[view]}
              className="nav-menu"
              items={NAV_ITEMS.map((it) => ({
                key: it.key,
                icon: it.icon,
                label: (
                  <>
                    <span>{t(it.labelKey)}</span> <span className="label-en">[{it.en}]</span>
                    {it.key === "approvals" && pendingApprovals > 0 && (
                      <span className="nav-badge" title={`${pendingApprovals}`}>{pendingApprovals > 99 ? "99+" : pendingApprovals}</span>
                    )}
                    {it.key === "flows" && flowUnread > 0 && (
                      <span className="nav-badge" title={`${flowUnread}`}>{flowUnread > 99 ? "99+" : flowUnread}</span>
                    )}
                  </>
                ),
              }))}
              onClick={({ key }) => {
                setView(key as ViewKey);
                history.replaceState(null, "", "#/" + key);
                setNavOpen(false);
              }}
            />
          </div>
        </div>

        {/* ── 二级侧边栏已彻底迁至主内容右栏（subbar-rail，常驻）：
            左主侧栏不再保留入口与面板（2026-11 二次调整）── */}

        {/* 弹性占位：主题压到底 */}
        <div className="sider-grow" />

        {/* ── 底部手风琴：主题（预设 + 强调色合并为单一入口，独立开关）──
            分隔线已删（2026-11：菜单平铺后与主题区无缝衔接，分割线纯占位） */}
        <div className={`theme-switcher theme-merged ${themeOpen ? "open" : ""}`}>
          <button className="theme-toggle" onClick={() => setThemeOpen(!themeOpen)} aria-expanded={themeOpen}>
            <span className="theme-label">{t("theme.label")}</span>
            <span className="theme-current" style={{ color: accentColor(preset, accent) }}>
              {t(`theme.preset.${preset}` as TKey)} · {t(`theme.accent.${accent}` as TKey)}
            </span>
            <span className={`theme-caret ${themeOpen ? "up" : ""}`}>▸</span>
          </button>
          <div className="theme-panel">
            <div className="theme-block">
              <div className="theme-block-label">{t("theme.presetTitle")}</div>
              {THEMES.map((th) => (
                <button
                  key={th.key}
                  className={`theme-option ${preset === th.key ? "active" : ""}`}
                  onClick={() => setPreset(th.key as ThemeKey)}
                >
                  <span className="theme-swatch" style={{ background: th.mode === "light" ? "#ffffff" : "#101218" }} />
                  <span>{t(`theme.preset.${th.key}` as TKey)}</span>
                  <span className="label-en">[{th.key.toUpperCase()}]</span>
                </button>
              ))}
            </div>
            <div className="theme-block">
              <div className="theme-block-label">{t("theme.accentTitle")}</div>
              {ACCENTS.map((a) => (
                <button
                  key={a.key}
                  className={`theme-option ${accent === a.key ? "active" : ""}`}
                  onClick={() => setAccent(a.key)}
                >
                  <span className="theme-swatch" style={{ background: accentColor(preset, a.key) }} />
                  <span>{t(`theme.accent.${a.key}` as TKey)}</span>
                  <span className="label-en">[{a.key.toUpperCase()}]</span>
                </button>
              ))}
            </div>
          </div>
        </div>

        <div className="nav-footer">
          <span className={`status-led ${wsConnected ? "ok" : "bad"}`} />
          <span className="status-text">{wsConnected ? t("status.connected") : t("status.reconnecting")}</span>
          {/* 通道状态：是否已配置模型端点。未配置即无法推理，提示去「模型设置」页补齐。 */}
          <Tag color={llmReady ? "success" : "warning"} className="brand-tag">
            {llmReady ? t("status.link") : t("status.unlinked")}
          </Tag>
        </div>
      </aside>
      <main className="app-content">
        <ViewSuspense>
          {/* key 驱动：切视图即重挂载 → 入场动效（淡入 + 上移）每次都完整播放 */}
          <div className="view-stage" key={view}>
            {view === "chat" && <ChatView />}
            {view === "flows" && <FlowsView />}
            {view === "tasks" && <TasksView />}
            {view === "sessions" && <SessionsView />}
            {view === "graph" && <GraphView />}
            {view === "agent" && <SinglesView />}
            {view === "nexus" && <NexusView />}
            {view === "skills" && <SkillsView />}
            {view === "plugins" && <PluginsView />}
            {view === "models" && <ModelsView />}
            {view === "automations" && <AutomationsView />}
            {view === "approvals" && <ApprovalsView />}
            {view === "channels" && <ChannelsView />}
            {view === "ledger" && <LedgerView />}
            {view === "memory" && <MemoryView />}
            {view === "activity" && <ActivityView />}
            {view === "settings" && <SettingsView />}
          </div>
        </ViewSuspense>
      </main>
      {/* ── 二级侧边栏（右栏，常驻）：页面自己的侧栏内容（会话/聊天流/任务会话/
          设置分类导航）portal 到这里。独立于左主侧栏，无开关——进页即现。── */}
      {ctxKind && (
        <aside className={`subbar-rail subbar-rail-${ctxKind}`}>
          <div className="sider-subpanel" ref={setSubbarSlot} />
        </aside>
      )}
      {/* 底边遥测条：position:fixed 通栏贴窗口底，不参与 .app-shell 的 flex 布局 */}
      <TelemetryBar />
      {/* 全局会话速查（Ctrl/Cmd+K）：Modal 自带 portal，挂根部即可 */}
      <CommandPalette />
      </div>
    </SubbarSlotContext.Provider>
  );
}
