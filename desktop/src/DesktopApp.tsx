import { useCallback, useEffect, useRef, useState } from "react";
import { Alert, App, Button, Dropdown, Input, Modal, Skeleton, Tooltip } from "antd";
import {
  BranchesOutlined, CloseOutlined, CodeOutlined, FolderOpenOutlined, MenuFoldOutlined,
  MenuUnfoldOutlined, MinusOutlined, PlusOutlined, SettingOutlined, MessageOutlined, BorderOutlined,
} from "@ant-design/icons";
import { open as openDirectory } from "@tauri-apps/plugin-dialog";
import { useExm } from "../../webui/src/store";
import { api } from "../../webui/src/api";
import { editorApi } from "../../webui/src/api/editor";
import { desktopInvoke, type DesktopConfig } from "../../webui/src/desktop";
import { isTaskSideTitle } from "../../webui/src/session";
import { DesktopChat } from "./DesktopChat";
import { DesktopSettings } from "./DesktopSettings";
import { CodeEditor, ProjectPanel, type OpenFile, type ProjectTab } from "./CodeWorkspace";
import type { Session } from "../../webui/src/types";

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

function SessionSidebar({ collapsed, onToggle, onSelected }: { collapsed: boolean; onToggle: () => void; onSelected: () => void }) {
  const { sessions, sessionId, selectSession, newSession, running } = useExm();
  const { message, modal } = App.useApp();
  const [query, setQuery] = useState("");
  const [renaming, setRenaming] = useState<Session | null>(null);
  const [title, setTitle] = useState("");
  const [saving, setSaving] = useState(false);
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
    <div className="desk-sessions-head"><b>会话</b><Tooltip title="收起会话栏"><Button type="text" size="small" aria-label="收起会话栏" icon={<MenuFoldOutlined />} onClick={onToggle} /></Tooltip></div>
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
    <Modal open={!!renaming} title="重命名会话" confirmLoading={saving} onCancel={() => setRenaming(null)} onOk={() => void rename()}>
      <Input autoFocus aria-label="会话名称" value={title} onChange={(event) => setTitle(event.target.value)} onPressEnter={() => void rename()} />
    </Modal>
  </aside>;
}

function ContextRail({ onClose }: { onClose: () => void }) {
  const { sessions, sessionId, messages, running, graph, runToolCalls, ledger, todo, timeline } = useExm();
  const session = sessions.find((item) => item.id === sessionId);
  return <aside className="desk-context" aria-label="执行上下文">
    <header><b>执行过程</b><Button type="text" size="small" icon={<MenuUnfoldOutlined />} aria-label="收起右栏" onClick={onClose} /></header>
    <section><small>当前任务</small><p>{session?.title || "未选择会话"}</p>{running && <em>执行中</em>}</section>
    <section><small>本轮</small><dl><dt>消息</dt><dd>{messages.length}</dd><dt>工具调用</dt><dd>{runToolCalls.length}</dd><dt>任务节点</dt><dd>{graph?.nodes.length ?? 0}</dd></dl></section>
    {ledger?.task.goal && <section><small>目标</small><p>{ledger.task.goal}</p></section>}
    {!!todo?.items.length && <section><small>任务清单 · {todo.doneCount}/{todo.count}</small>{todo.items.map((item) => <p key={item.id}>{item.status === "done" ? "✓" : "○"} {item.content}</p>)}</section>}
    {!!runToolCalls.length && <section><small>工具</small>{runToolCalls.map((call) => <p key={call.callId}>{call.status === "running" ? "◌" : call.status === "ok" ? "✓" : "×"} {call.tool}</p>)}</section>}
    {!!timeline.length && <section><small>最近事件</small>{timeline.slice(-6).map((item, index) => <p key={index}>{item.text}</p>)}</section>}
  </aside>;
}

export function DesktopApp() {
  const { message, modal } = App.useApp();
  const [booting, setBooting] = useState(true);
  const [bootError, setBootError] = useState("");
  const [settings, setSettings] = useState(false);
  const [center, setCenter] = useState("chat");
  const [files, setFiles] = useState<Record<string, OpenFile>>({});
  const [workspace, setWorkspace] = useState("");
  const [projectInput, setProjectInput] = useState<string | null>(null);
  const [projectBusy, setProjectBusy] = useState(false);
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
  const workspaceGeneration = useRef(0);
  const fileRequest = useRef(0);
  const started = useRef(false);
  const dirtyFiles = Object.values(files).filter((file) => file.content !== file.draft);

  const initialize = useCallback(async () => {
    setBooting(true); setBootError("");
    try {
      await useExm.getState().init();
      const result = await editorApi.workspace();
      setWorkspace(result.workspace);
    } catch (error) { setBootError(String(error)); }
    finally { setBooting(false); }
  }, []);
  useEffect(() => {
    if (started.current) return;
    started.current = true;
    void initialize();
  }, [initialize]);
  useEffect(() => {
    if (!window.__TAURI__) return;
    let disposed = false;
    const stops: (() => void)[] = [];
    const listen = async (event: string, callback: () => void) => {
      try {
        const stop = await window.__TAURI__!.event.listen(event, callback);
        if (disposed) stop(); else stops.push(stop);
      } catch (error) { if (!disposed) message.error(String(error)); }
    };
    void listen("gateway://down", () => setGatewayDown(true));
    void listen("gateway://up", () => { setGatewayDown(false); void useExm.getState().refreshPendingApprovals(); });
    return () => { disposed = true; stops.forEach((stop) => stop()); };
  }, [message]);
  useEffect(() => {
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
  }, []);
  useEffect(() => {
    if (!navRequest) return;
    useExm.setState({ navRequest: null });
    if (navRequest.sessionId) void useExm.getState().selectSession(navRequest.sessionId).catch((error: unknown) => message.error(String(error)));
    setCenter("chat");
    setSettings(false);
  }, [navRequest, message]);
  useEffect(() => {
    if (!dirtyFiles.length) return;
    const protect = (event: BeforeUnloadEvent) => { event.preventDefault(); event.returnValue = ""; };
    window.addEventListener("beforeunload", protect);
    return () => window.removeEventListener("beforeunload", protect);
  }, [dirtyFiles.length]);
  useEffect(() => {
    if (!window.__TAURI__ || !dirtyFiles.length) return;
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void import("@tauri-apps/api/window").then(async ({ getCurrentWindow }) => {
      const currentWindow = getCurrentWindow();
      const stop = await currentWindow.onCloseRequested(async (event) => {
        event.preventDefault();
        const config = await desktopInvoke<DesktopConfig>("get_desktop_config");
        if (!config.exitOnClose) { await desktopInvoke("hide_window"); return; }
        modal.confirm({ title: "退出前丢弃未保存修改？", content: "文件中的本地修改尚未保存。", okText: "丢弃并退出", cancelText: "继续编辑", onOk: () => currentWindow.destroy() });
      });
      if (disposed) stop(); else unlisten = stop;
    }).catch((error: unknown) => message.error(String(error)));
    return () => { disposed = true; unlisten?.(); };
  }, [dirtyFiles.length, message, modal]);

  const confirmDiscard = (title: string) => new Promise<boolean>((resolve) => modal.confirm({
    title, content: "未保存的文件修改将被丢弃。", okText: "丢弃修改", cancelText: "继续编辑", okButtonProps: { danger: true },
    onOk: () => resolve(true), onCancel: () => resolve(false),
  }));
  const changeWorkspace = async (path: string) => {
    if (!path.trim() || projectBusy) return;
    if (path === workspace) { setProjectInput(null); return; }
    if (useExm.getState().running) { message.info("请先停止当前任务，再切换项目。"); return; }
    if (dirtyFiles.length && !await confirmDiscard("切换项目前丢弃未保存修改？")) return;
    setProjectBusy(true);
    workspaceGeneration.current += 1;
    fileRequest.current += 1;
    try {
      const result = await editorApi.setWorkspace(path.trim());
      setWorkspace(result.workspace); setFiles({}); setCenter("project"); setProjectTab("files");
      setProjectInput(null); setRefresh((value) => value + 1);
    } catch (error) { message.error(String(error)); }
    finally { setProjectBusy(false); }
  };
  const chooseWorkspace = async () => {
    if (!window.__TAURI__ || window.__EXM_GATEWAY__?.remote) { setProjectInput(workspace); return; }
    try {
      const path = await openDirectory({ directory: true, multiple: false, title: "打开项目" });
      if (typeof path === "string") await changeWorkspace(path);
    } catch (error) { message.error(String(error)); }
  };
  const openFile = async (path: string) => {
    const request = ++fileRequest.current;
    const generation = workspaceGeneration.current;
    if (fileState.current[path]) { setCenter(fileView(path)); return; }
    try {
      const file = await editorApi.read(path);
      if (generation !== workspaceGeneration.current) return;
      setFiles((current) => current[path] ? current : { ...current, [path]: { ...file, workspace: file.workspace ?? workspace, draft: file.content } });
      if (request === fileRequest.current) setCenter(fileView(path));
    } catch (error) { if (generation === workspaceGeneration.current) message.error(String(error)); }
  };
  const closeFile = async (path: string) => {
    const file = fileState.current[path];
    if (file.content !== file.draft && !await confirmDiscard(`关闭 ${path} 前丢弃修改？`)) return;
    setFiles((current) => { const next = { ...current }; delete next[path]; return next; });
    if (center === fileView(path)) setCenter("project");
  };
  const reloadFile = async (path: string) => {
    if (fileState.current[path]?.draft !== fileState.current[path]?.content && !await confirmDiscard(`重新加载 ${path}？`)) return;
    const generation = workspaceGeneration.current;
    try {
      const file = await editorApi.read(path);
      if (generation === workspaceGeneration.current) setFiles((current) => ({ ...current, [path]: { ...file, workspace: file.workspace ?? workspace, draft: file.content } }));
    } catch (error) { message.error(String(error)); }
  };
  const showProject = (tab: ProjectTab) => { fileRequest.current += 1; setProjectTab(tab); setCenter("project"); };
  const open = center.startsWith("file:") ? files[center.slice(5)] : undefined;
  if (booting) return <div className="desk-boot"><Skeleton active paragraph={{ rows: 5 }} /><span>正在连接工作区…</span></div>;
  if (bootError) return <div className="desk-boot"><Alert type="error" title="无法连接网关" description={bootError} /><Button onClick={() => void initialize()}>重新连接</Button></div>;
  return <div className="desk-app">
    <TitleBar onSettings={() => setSettings(true)} />
    <div className="desk-statusbar">
      <Button type="text" size="small" icon={sessionsCollapsed ? <MenuUnfoldOutlined /> : <MenuFoldOutlined />} aria-label={sessionsCollapsed ? "显示会话" : "隐藏会话"} onClick={() => setSessionsCollapsed(!sessionsCollapsed)} />
      <Tooltip title={wsConnected ? "网关已连接" : "连接中，自动重试"}><span className={`desk-led ${wsConnected ? "ok" : "bad"}`} /></Tooltip>
      <Button type="text" size="small" className="desk-workspace-button" aria-label="打开项目" title={workspace} icon={<FolderOpenOutlined />} onClick={() => void chooseWorkspace()}>{workspace.split(/[\\/]/).filter(Boolean).at(-1) || "打开项目"}</Button>
      <span className="desk-status-grow" />
      <Button type="text" size="small" aria-label="查看对话" icon={<MessageOutlined />} onClick={() => { fileRequest.current += 1; setCenter("chat"); }}>对话{pendingApprovals ? ` · ${pendingApprovals}` : ""}</Button>
      <Button type="text" size="small" aria-label="查看代码" icon={<CodeOutlined />} onClick={() => showProject("files")}>代码</Button>
      <Button type="text" size="small" aria-label="查看变更" icon={<BranchesOutlined />} onClick={() => showProject("changes")}>变更</Button>
      <Button type="text" size="small" icon={<MenuUnfoldOutlined />} aria-label="显示执行过程" onClick={() => setContext(!context)} />
    </div>
    <div className="desk-body">
      <SessionSidebar collapsed={sessionsCollapsed} onToggle={() => setSessionsCollapsed(true)} onSelected={() => { setCenter("chat"); if (window.innerWidth < 760) setSessionsCollapsed(true); }} />
      <main className="desk-main">
        {gatewayDown && <Alert type="error" title="本地网关已停止" action={<Button size="small" onClick={() => void desktopInvoke("gateway_restart").catch((error: unknown) => message.error(String(error)))}>重启网关</Button>} />}
        <nav className="desk-file-tabs" aria-label="打开的文件">
          <button aria-current={center === "chat" ? "page" : undefined} onClick={() => setCenter("chat")}><MessageOutlined /> 对话</button>
          {Object.values(files).map((file) => <div className="desk-file-tab" key={file.path}>
            <button aria-current={center === fileView(file.path) ? "page" : undefined} title={file.path} onClick={() => { fileRequest.current += 1; setCenter(fileView(file.path)); }}>{file.path.split("/").at(-1)}{file.content !== file.draft ? " ●" : ""}</button>
            <Button type="text" size="small" icon={<CloseOutlined />} aria-label={`关闭 ${file.path}`} onClick={() => void closeFile(file.path)} />
          </div>)}
        </nav>
        <div className="desk-center-pane" hidden={center !== "chat"}>
          <DesktopChat workspace={workspace} onProject={() => void chooseWorkspace()} active={center === "chat"} contextVersion={settings ? 0 : refresh} />
        </div>
        <div className="desk-center-pane" hidden={center !== "project"}>
          <ProjectPanel workspace={workspace} onOpen={(path) => void openFile(path)} refresh={refresh} tab={projectTab} onTabChange={setProjectTab} onApproval={() => { void useExm.getState().refreshPendingApprovals(); setCenter("chat"); }} />
        </div>
        {open && <CodeEditor key={`${workspace}:${open.path}`} file={open}
          onChange={(draft) => setFiles((current) => ({ ...current, [open.path]: { ...current[open.path], draft } }))}
          onReload={() => void reloadFile(open.path)}
          onSaved={(path, content) => {
            setFiles((current) => current[path]?.workspace === open.workspace ? { ...current, [path]: { ...current[path], content } } : current);
            setRefresh((value) => value + 1);
          }} />}
      </main>
      {context && <ContextRail onClose={() => setContext(false)} />}
    </div>
    <DesktopSettings open={settings} onClose={() => { setSettings(false); setRefresh((value) => value + 1); }} />
    <Modal open={projectInput !== null} title="打开项目" okText="打开" confirmLoading={projectBusy} onCancel={() => setProjectInput(null)} onOk={() => void changeWorkspace(projectInput ?? "")}>
      <p>{window.__EXM_GATEWAY__?.remote ? "输入网关所在机器上的项目目录。" : "输入项目目录。"}</p>
      <Input autoFocus aria-label="项目目录" value={projectInput ?? ""} onChange={(event) => setProjectInput(event.target.value)} onPressEnter={() => void changeWorkspace(projectInput ?? "")} />
    </Modal>
  </div>;
}
