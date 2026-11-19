import { useCallback, useEffect, useRef, useState } from "react";
import { Alert, App, Button, Dropdown, Input, Modal, Skeleton, Tooltip } from "antd";
import {
  BranchesOutlined, CloseOutlined, CodeOutlined, FolderOpenOutlined, MenuFoldOutlined,
  MenuUnfoldOutlined, MinusOutlined, PlusOutlined, SettingOutlined, MessageOutlined, BorderOutlined, UnorderedListOutlined, TeamOutlined,
} from "@ant-design/icons";
import { desktopInvoke } from "./native";
import { isTaskSideTitle } from "../../client/src/session";
import { useProjectClient, type Project } from "./projects";
import { DesktopChat } from "./DesktopChat";
import { DesktopCollaboration } from "./DesktopCollaboration";
import { CodeEditor, ProjectPanel, type OpenFile, type ProjectTab } from "./CodeWorkspace";
import type { Session } from "../../client/src/types";
import { ProjectSessionsDrawer } from "./ProjectSessionsDrawer";
import { AgentWorkDrawer } from "../../client/src/components/AgentWorkDrawer";

const fileView = (path: string) => `file:${path}`;

function TitleBar({ onSettings }: { onSettings: () => void }) {
  const { message } = App.useApp();
  const control = (command: string) => void desktopInvoke(command).catch((error: unknown) => message.error(String(error)));
  return <header className="desk-titlebar" data-tauri-drag-region>
    <span className="desk-brand" data-tauri-drag-region><i />EXMACHINA</span>
    <span className="desk-titlebar-grow" data-tauri-drag-region />
    <Button type="text" size="small" icon={<SettingOutlined />} aria-label="设置" onClick={onSettings} />
    {window.__TAURI__ && <div className="desk-window-actions">
      <button aria-label="最小化" onClick={() => control("min_window")}><MinusOutlined /></button>
      <button aria-label="最大化" onClick={() => control("toggle_max_window")}><BorderOutlined /></button>
      <button aria-label="关闭" onClick={() => control("close_window")}><CloseOutlined /></button>
    </div>}
  </header>;
}

function SessionSidebar({ active, collapsed, onToggle, onSelected }: { active: boolean; collapsed: boolean; onToggle: () => void; onSelected: () => void }) {
  const { store: useExm, client: { api } } = useProjectClient();
  const { sessions, sessionId, selectSession, newSession, running } = useExm();
  const { message, modal } = App.useApp();
  const [query, setQuery] = useState("");
  const [renaming, setRenaming] = useState<Session | null>(null);
  const [title, setTitle] = useState("");
  const [saving, setSaving] = useState(false);
  const [managerOpen, setManagerOpen] = useState(false);
  const filtered = sessions.filter((session) => !session.archivedAt && !session.title.startsWith("channel:") && !isTaskSideTitle(session.title)
    && session.title.toLowerCase().includes(query.toLowerCase()));
  const select = async (id: string) => {
    try { await selectSession(id); onSelected(); }
    catch (error) { message.error(String(error)); }
  };
  const rename = async () => {
    if (!renaming || !title.trim() || saving) return;
    setSaving(true);
    try {
      await api.renameSession(renaming.id, title.trim());
      useExm.setState((state) => ({ sessions: state.sessions.map((session) => session.id === renaming.id ? { ...session, title: title.trim() } : session) }));
      setRenaming(null);
    } catch (error) { message.error(String(error)); }
    finally { setSaving(false); }
  };
  const remove = (session: Session) => modal.confirm({
    title: `删除「${session.title}」？`, content: "会话及其记录将被删除。", okText: "删除", okButtonProps: { danger: true },
    onOk: async () => {
      await api.deleteSession(session.id);
      const remaining = useExm.getState().sessions.filter((item) => item.id !== session.id);
      useExm.setState({ sessions: remaining });
      if (useExm.getState().sessionId === session.id) {
        const next = remaining.find((item) => !isTaskSideTitle(item.title));
        if (next) await selectSession(next.id); else await newSession();
      }
    },
  });
  return <aside className={`desk-sessions ${collapsed ? "collapsed" : ""}`} aria-label="会话列表">
    <div className="desk-sessions-head"><b>会话</b><span><Tooltip title="管理会话"><Button type="text" size="small" aria-label="管理会话" icon={<UnorderedListOutlined />} onClick={() => setManagerOpen(true)} /></Tooltip><Tooltip title="收起会话栏"><Button type="text" size="small" aria-label="收起会话栏" icon={<MenuFoldOutlined />} onClick={onToggle} /></Tooltip></span></div>
    <Button className="desk-new-session" icon={<PlusOutlined />} onClick={() => void newSession().then(onSelected).catch((error: unknown) => message.error(String(error)))}>新任务</Button>
    <Input.Search allowClear placeholder="搜索会话" value={query} onChange={(event) => setQuery(event.target.value)} />
    <nav>{filtered.map((session) => <div key={session.id} className={`desk-session ${session.id === sessionId ? "active" : ""}`}>
      <button className="desk-session-select" aria-current={session.id === sessionId ? "page" : undefined} onClick={() => void select(session.id)}>
        <MessageOutlined /><span>{session.title || "未命名任务"}</span>
        {session.id === sessionId && running && <i className="desk-run-dot" />}
      </button>
      <Dropdown trigger={["click"]} menu={{ items: [
        { key: "rename", label: "重命名" }, { key: "delete", label: "删除", danger: true },
      ], onClick: ({ key }) => {
        if (key === "rename") { setRenaming(session); setTitle(session.title); }
        if (key === "delete") remove(session);
      } }}>
        <Button className="desk-session-menu" type="text" size="small" aria-label={`${session.title} 的操作`}>···</Button>
      </Dropdown>
    </div>)}</nav>
    <Modal open={active && !!renaming} title="重命名会话" confirmLoading={saving} onCancel={() => setRenaming(null)} onOk={() => void rename()}>
      <Input autoFocus aria-label="会话名称" value={title} onChange={(event) => setTitle(event.target.value)} onPressEnter={() => void rename()} />
    </Modal>
    {active && managerOpen && <ProjectSessionsDrawer onClose={() => setManagerOpen(false)} onSelected={onSelected} />}
  </aside>;
}

function ContextRail({ active, onClose }: { active: boolean; onClose: () => void }) {
  const { store: useExm } = useProjectClient();
  const state = useExm();
  const { sessions, sessionId, messages, running, graph, runToolCalls, ledger, todo, timeline, liveParts, agents, activeGroup } = state;
  const [agentWorkOpen, setAgentWorkOpen] = useState(false);
  const session = sessions.find((item) => item.id === sessionId);
  return <aside className="desk-context" aria-label="执行上下文">
    <header><b>执行过程</b><span><Button type="text" size="small" icon={<UnorderedListOutlined />} aria-label="Agent 工作态势" title="Agent 工作态势" disabled={!sessionId} onClick={() => setAgentWorkOpen(true)} /><Button type="text" size="small" icon={<MenuUnfoldOutlined />} aria-label="收起右栏" onClick={onClose} /></span></header>
    <section><small>当前任务</small><p>{session?.title || "未选择会话"}</p>{running && <em>执行中</em>}</section>
    <section><small>本轮</small><dl><dt>消息</dt><dd>{messages.length}</dd><dt>工具调用</dt><dd>{runToolCalls.length}</dd><dt>任务节点</dt><dd>{graph?.nodes.length ?? 0}</dd></dl></section>
    {ledger?.task.goal && <section><small>目标</small><p>{ledger.task.goal}</p></section>}
    {!!todo?.items.length && <section><small>任务清单 · {todo.doneCount}/{todo.count}</small>{todo.items.map((item) => <p key={item.id}>{item.status === "done" ? "✓" : "○"} {item.content}</p>)}</section>}
    {!!runToolCalls.length && <section><small>工具</small>{runToolCalls.map((call) => <p key={call.callId}>{call.status === "running" ? "◌" : call.status === "ok" ? "✓" : "×"} {call.tool}</p>)}</section>}
    {!!timeline.length && <section><small>最近事件</small>{timeline.slice(-6).map((item, index) => <p key={index}>{item.text}</p>)}</section>}
    {active && agentWorkOpen && <AgentWorkDrawer open onClose={() => setAgentWorkOpen(false)} sessionId={sessionId} sessionTitle={session?.title ?? ""}
      running={running} graph={graph} liveParts={liveParts} toolCalls={runToolCalls} messages={messages} timeline={timeline} agents={agents} targetLabel={session?.groupId ?? activeGroup} />}
  </aside>;
}

export function ProjectWorkspace({ active, projects, onSettings, onOpenProject, onDirty }: { active: boolean; projects: Project[]; onSettings: () => void; onOpenProject: () => void; onDirty: (id: string, count: number) => void }) {
  const { project, local, store: useExm, client: { api, editorApi } } = useProjectClient();
  const { message, modal } = App.useApp();
  const [booting, setBooting] = useState(true);
  const [bootError, setBootError] = useState("");
  const [center, setCenter] = useState("chat");
  const [collaborationOpened, setCollaborationOpened] = useState(false);
  const [files, setFiles] = useState<Record<string, OpenFile>>({});
  const workspace = project.workspace;
  const [sessionsCollapsed, setSessionsCollapsed] = useState(() => window.innerWidth < 960);
  const [context, setContext] = useState(() => window.innerWidth >= 1100);
  const [projectTab, setProjectTab] = useState<ProjectTab>("files");
  const [refresh, setRefresh] = useState(0);
  const [gatewayDown, setGatewayDown] = useState(false);
  const wsConnected = useExm((state) => state.wsConnected);
  const pendingApprovals = useExm((state) => state.pendingApprovals);
  const navRequest = useExm((state) => state.navRequest);
  const fileState = useRef(files);
  fileState.current = files;
  const fileRequest = useRef(0);
  const started = useRef(false);
  const dirtyFiles = Object.values(files).filter((file) => file.content !== file.draft);
  useEffect(() => { onDirty(project.id, dirtyFiles.length); }, [project.id, dirtyFiles.length, onDirty]);

  const initialize = useCallback(async () => {
    setBooting(true); setBootError("");
    try {
      await useExm.getState().init();
    } catch (error) { setBootError(String(error)); }
    finally { setBooting(false); }
  }, [useExm]);
  useEffect(() => {
    if (started.current) return;
    started.current = true;
    void initialize();
  }, [initialize]);
  useEffect(() => {
    if (!window.__TAURI__ || !local) return;
    let disposed = false;
    const stops: (() => void)[] = [];
    const listen = async (event: string, callback: () => void) => {
      try {
        const stop = await window.__TAURI__!.event.listen(event, callback);
        if (disposed) stop(); else stops.push(stop);
      } catch (error) { if (!disposed) message.error(String(error)); }
    };
    void listen("gateway://down", () => setGatewayDown(true));
    void listen("gateway://up", () => { setGatewayDown(false); useExm.reconnect(); });
    return () => { disposed = true; stops.forEach((stop) => stop()); };
  }, [message, local, useExm]);
  useEffect(() => {
    if (!active || booting || bootError) return;
    const timer = window.setInterval(() => {
      setRefresh((value) => value + 1);
      void useExm.getState().refreshPendingApprovals();
    }, 30_000);
    const approvalsChanged = () => setRefresh((value) => value + 1);
    const sessionsChanged = () => void api.listSessions().then((sessions) => useExm.setState({ sessions })).catch(() => undefined);
    window.addEventListener("exm:approvals-refresh", approvalsChanged);
    window.addEventListener("exm:flows-refresh", sessionsChanged);
    return () => {
      window.clearInterval(timer);
      window.removeEventListener("exm:approvals-refresh", approvalsChanged);
      window.removeEventListener("exm:flows-refresh", sessionsChanged);
    };
    void useExm.getState().refreshPendingApprovals();
  }, [active, api, useExm, booting, bootError]);
  useEffect(() => {
    if (!navRequest) return;
    useExm.setState({ navRequest: null });
    if (navRequest.sessionId) void useExm.getState().selectSession(navRequest.sessionId).catch((error: unknown) => message.error(String(error)));
    setCenter("chat");
  }, [navRequest, message, useExm]);
  useEffect(() => {
    if (!dirtyFiles.length) return;
    const protect = (event: BeforeUnloadEvent) => { event.preventDefault(); event.returnValue = ""; };
    if (active) window.addEventListener("beforeunload", protect);
    return () => window.removeEventListener("beforeunload", protect);
  }, [dirtyFiles.length, active]);

  const confirmDiscard = (title: string) => new Promise<boolean>((resolve) => modal.confirm({
    title, content: "未保存的文件修改将被丢弃。", okText: "丢弃修改", cancelText: "继续编辑", okButtonProps: { danger: true },
    onOk: () => resolve(true), onCancel: () => resolve(false),
  }));
  const chooseWorkspace = onOpenProject;
  const openFile = async (path: string) => {
    const request = ++fileRequest.current;
    if (fileState.current[path]) { setCenter(fileView(path)); return; }
    try {
      const file = await editorApi.read(path);
      setFiles((current) => current[path] ? current : { ...current, [path]: { ...file, workspace: file.workspace ?? workspace, draft: file.content } });
      if (request === fileRequest.current) setCenter(fileView(path));
    } catch (error) { message.error(String(error)); }
  };
  const closeFile = async (path: string) => {
    const file = fileState.current[path];
    if (file.content !== file.draft && !await confirmDiscard(`关闭 ${path} 前丢弃修改？`)) return;
    setFiles((current) => { const next = { ...current }; delete next[path]; return next; });
    if (center === fileView(path)) setCenter("project");
  };
  const reloadFile = async (path: string) => {
    if (fileState.current[path]?.draft !== fileState.current[path]?.content && !await confirmDiscard(`重新加载 ${path}？`)) return;
    const original = fileState.current[path];
    try {
      const file = await editorApi.read(path);
      setFiles((current) => current[path] === original ? { ...current, [path]: { ...file, workspace: file.workspace ?? workspace, draft: file.content } } : current);
    } catch (error) { message.error(String(error)); }
  };
  const showProject = (tab: ProjectTab) => { fileRequest.current += 1; setProjectTab(tab); setCenter("project"); };
  const open = center.startsWith("file:") ? files[center.slice(5)] : undefined;
  if (booting) return <div className="desk-boot"><Skeleton active paragraph={{ rows: 5 }} /><span>正在连接工作区…</span></div>;
  if (bootError) return <div className="desk-boot"><Alert type="error" title="无法连接网关" description={bootError} /><Button onClick={() => void initialize()}>重新连接</Button></div>;
  return <div className="desk-app">
    <TitleBar onSettings={onSettings} />
    <div className="desk-statusbar">
      <Button type="text" size="small" icon={sessionsCollapsed ? <MenuUnfoldOutlined /> : <MenuFoldOutlined />} aria-label={sessionsCollapsed ? "显示会话" : "隐藏会话"} onClick={() => setSessionsCollapsed(!sessionsCollapsed)} />
      <Tooltip title={wsConnected ? "网关已连接" : "连接中，自动重试"}><span className={`desk-led ${wsConnected ? "ok" : "bad"}`} /></Tooltip>
      <Button type="text" size="small" className="desk-workspace-button" aria-label="打开项目" title={workspace} icon={<FolderOpenOutlined />} onClick={() => void chooseWorkspace()}>{workspace.split(/[\\/]/).filter(Boolean).at(-1) || "打开项目"}</Button>
      <span className="desk-status-grow" />
      <Button type="text" size="small" aria-label="查看对话" icon={<MessageOutlined />} onClick={() => { fileRequest.current += 1; setCenter("chat"); }}>对话{pendingApprovals ? ` · ${pendingApprovals}` : ""}</Button>
      <Button type="text" size="small" aria-label="查看代码" icon={<CodeOutlined />} onClick={() => showProject("files")}>代码</Button>
      <Button type="text" size="small" aria-label="查看变更" icon={<BranchesOutlined />} onClick={() => showProject("changes")}>变更</Button>
      <Button type="text" size="small" icon={<MenuUnfoldOutlined />} aria-label="显示执行过程" disabled={center === "collaboration"} onClick={() => setContext(!context)} />
    </div>
    <div className="desk-body">
      <SessionSidebar active={active} collapsed={sessionsCollapsed} onToggle={() => setSessionsCollapsed(true)} onSelected={() => { setCenter("chat"); if (window.innerWidth < 760) setSessionsCollapsed(true); }} />
      <main className="desk-main">
        {gatewayDown && <Alert type="error" title="本地网关已停止" action={<Button size="small" onClick={onSettings}>打开桌面设置</Button>} />}
        <nav className="desk-file-tabs" aria-label="打开的文件">
          <button aria-label="对话" aria-current={center === "chat" ? "page" : undefined} onClick={() => setCenter("chat")}><MessageOutlined /> 对话</button>
          <button aria-label="协作群聊" aria-current={center === "collaboration" ? "page" : undefined} onClick={() => { fileRequest.current += 1; setCollaborationOpened(true); setCenter("collaboration"); }}><TeamOutlined /> 协作群聊</button>
          {Object.values(files).map((file) => <div className="desk-file-tab" key={file.path}>
            <button aria-current={center === fileView(file.path) ? "page" : undefined} title={file.path} onClick={() => { fileRequest.current += 1; setCenter(fileView(file.path)); }}>{file.path.split("/").at(-1)}{file.content !== file.draft ? " ●" : ""}</button>
            <Button type="text" size="small" icon={<CloseOutlined />} aria-label={`关闭 ${file.path}`} onClick={() => void closeFile(file.path)} />
          </div>)}
        </nav>
        <div className="desk-center-pane" hidden={center !== "chat"}>
          <DesktopChat workspace={workspace} onProject={chooseWorkspace} active={active && center === "chat"} contextVersion={refresh} />
        </div>
        {collaborationOpened && <div className="desk-center-pane" hidden={center !== "collaboration"}>
          <DesktopCollaboration projects={projects} active={active && center === "collaboration"} />
        </div>}
        {active && center === "project" && <div className="desk-center-pane">
          <ProjectPanel workspace={workspace} onOpen={(path) => void openFile(path)} refresh={refresh} tab={projectTab} onTabChange={setProjectTab} onApproval={() => { void useExm.getState().refreshPendingApprovals(); setCenter("chat"); }} />
        </div>}
        {open && active && <CodeEditor key={`${workspace}:${open.path}`} file={open}
          onChange={(draft) => setFiles((current) => ({ ...current, [open.path]: { ...current[open.path], draft } }))}
          onReload={() => void reloadFile(open.path)}
          onSaved={(path, content) => {
            setFiles((current) => current[path]?.workspace === open.workspace ? { ...current, [path]: { ...current[path], content } } : current);
            setRefresh((value) => value + 1);
          }} />}
      </main>
      {context && center !== "collaboration" && <ContextRail active={active} onClose={() => setContext(false)} />}
    </div>
  </div>;
}
