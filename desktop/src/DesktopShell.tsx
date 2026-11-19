import { useCallback, useEffect, useRef, useState } from "react";
import { Alert, App, Button, Empty, Spin } from "antd";
import { PlusOutlined, SettingOutlined } from "@ant-design/icons";
import { ProjectWorkspace } from "./DesktopApp";
import { DesktopSettings } from "./DesktopSettings";
import { ProjectManager } from "./ProjectManager";
import { ClientServices } from "../../client/src/services";
import { connectionKey, connectProject, desktopTranslate, loadCatalog, makeRuntime, newProjectId, ProjectContext, saveCatalog, setConnectionKey, type Project, type ProjectCatalog, type ProjectRuntime } from "./projects";
import { createClient, type ExmClient } from "../../client/src/api/client";
import { desktopInvoke, type DesktopConfig } from "./native";

function ProjectTab({ runtime, active, onSelect }: { runtime: ProjectRuntime; active: boolean; onSelect: () => void }) {
  const running = runtime.store((s) => s.running || s.sessions.some((item) => item.running));
  const approvals = runtime.store((s) => s.pendingApprovals);
  return <button type="button" aria-current={active ? "page" : undefined} onClick={onSelect} title={runtime.project.workspace}>
    <span className={`desk-led ${running ? "working" : "ok"}`} />{runtime.project.name}{approvals ? ` · ${approvals} 待审批` : ""}
  </button>;
}

export function DesktopApp() {
  const { message, modal } = App.useApp();
  const [catalog, setCatalog] = useState<ProjectCatalog>(loadCatalog);
  const catalogRef = useRef(catalog);
  catalogRef.current = catalog;
  const [runtimes, setRuntimes] = useState<ProjectRuntime[]>([]);
  const runtimeRef = useRef(runtimes);
  runtimeRef.current = runtimes;
  const [activeId, setActiveId] = useState(catalog.activeId ?? "");
  const [manager, setManager] = useState(false);
  const [settings, setSettings] = useState(false);
  const [connecting, setConnecting] = useState(false);
  const [error, setError] = useState("");
  const started = useRef(false);
  const request = useRef(0);
  const dirty = useRef<Record<string, number>>({});
  const onDirty = useCallback((id: string, count: number) => { dirty.current[id] = count; }, []);
  const update = useCallback((next: ProjectCatalog) => { saveCatalog(next); catalogRef.current = next; setCatalog(next); }, []);

  const open = useCallback((project: Project, client: ExmClient, mobileUrl?: string) => {
    let runtime = runtimeRef.current.find((r) => r.project.id === project.id);
    if (!runtime) {
      const connection = catalogRef.current.connections.find((c) => c.id === project.connectionId);
      runtime = makeRuntime(project, client, connection?.kind === "local", connection?.kind === "local" ? connection.url : client.serverBase(), mobileUrl);
      runtimeRef.current = [...runtimeRef.current, runtime];
      setRuntimes(runtimeRef.current);
    }
    if (window.__TAURI__) {
      const connection = catalogRef.current.connections.find((c) => c.id === project.connectionId);
      if (connection) void desktopInvoke("set_event_gateway", {
        url: client.serverBase(), key: client.accessKey(), remote: connection.kind === "remote", workspace: project.workspace,
      }).catch((error: unknown) => message.error(String(error)));
    }
    setActiveId(project.id); setError("");
    update({ ...catalogRef.current, activeId: project.id });
  }, [message, update]);

  const select = useCallback(async (project: Project) => {
    const sequence = ++request.current;
    const opened = runtimeRef.current.find((r) => r.project.id === project.id);
    if (opened) {
      open(project, opened.client, opened.mobileUrl);
      setConnecting(false); setError(""); return;
    }
    const connection = catalogRef.current.connections.find((c) => c.id === project.connectionId);
    if (!connection) return;
    setConnecting(true); setError("");
    try {
      const result = await connectProject(connection, project.workspace);
      if (result.workspace !== project.workspace) throw new Error("项目路径已变化，请将新的目录作为项目添加");
      if (sequence === request.current) open(project, result.client, result.mobileUrl);
    } catch (e) { if (sequence === request.current) { setError(String(e)); setManager(true); } }
    finally { if (sequence === request.current) setConnecting(false); }
  }, [open]);

  useEffect(() => {
    if (started.current) return;
    started.current = true;
    void (async () => {
      try {
        let next = catalogRef.current;
        const launch = window.__EXM_GATEWAY__;
        if (launch) {
          const kind = launch.remote ? "remote" as const : "local" as const;
          const existing = next.connections.find((c) => c.kind === kind && (kind === "local" || c.url === launch.url));
          const connection = existing ?? { id: kind === "local" ? "local" : newProjectId(), name: kind === "local" ? "本机实例" : "启动连接", kind, url: kind === "local" ? "" : launch.url };
          if (kind === "remote" && launch.key && !connectionKey(connection.id)) setConnectionKey(connection.id, launch.key);
          next = { ...next, connections: [...next.connections.filter((c) => c.id !== connection.id), connection] };
          if (window.__TAURI__ && !next.connections.some((c) => c.kind === "local")) next.connections.push({ id: "local", name: "本机实例", kind: "local", url: "" });
          update(next);
          if (!next.projects.length) {
            const result = await connectProject(connection);
            const project: Project = { id: newProjectId(), name: result.workspace.split(/[\\/]/).filter(Boolean).at(-1) || "默认项目", workspace: result.workspace, connectionId: connection.id };
            update({ ...next, projects: [project], activeId: project.id }); open(project, result.client, result.mobileUrl); return;
          }
        }
        const project = next.projects.find((p) => p.id === next.activeId) ?? next.projects[0];
        if (project) await select(project); else setManager(true);
      } catch (e) { setError(String(e)); setManager(true); }
    })();
  }, [open, select, update]);

  useEffect(() => {
    if (!window.__TAURI__) return;
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void import("@tauri-apps/api/window").then(async ({ getCurrentWindow }) => {
      const current = getCurrentWindow();
      const stop = await current.onCloseRequested(async (event) => {
        if (!Object.values(dirty.current).some(Boolean)) return;
        event.preventDefault();
        const config = await desktopInvoke<DesktopConfig>("get_desktop_config");
        if (!config.exitOnClose) { await desktopInvoke("hide_window"); return; }
        modal.confirm({ title: "退出前丢弃所有项目的未保存修改？", content: "包括当前未显示项目的文件修改。", okText: "丢弃并退出", cancelText: "继续编辑", onOk: () => current.destroy() });
      });
      if (disposed) stop(); else unlisten = stop;
    }).catch((e: unknown) => message.error(String(e)));
    return () => { disposed = true; unlisten?.(); };
  }, [message, modal]);

  const remove = async (project: Project) => {
    const runtime = runtimeRef.current.find((r) => r.project.id === project.id);
    const running = runtime?.store.getState().running;
    const confirmed = await new Promise<boolean>((resolve) => modal.confirm({ title: `移除项目「${project.name}」？`, content: `${dirty.current[project.id] ? "未保存的文件修改将丢失。" : ""}${running ? "后台任务不会被停止。" : ""}仅移除本机入口，不删除远端目录、会话或数据。`, okText: "移除项目", cancelText: "保留", okButtonProps: { danger: true }, onOk: () => resolve(true), onCancel: () => resolve(false) }));
    if (!confirmed) return;
    const next = { ...catalogRef.current, projects: catalogRef.current.projects.filter((p) => p.id !== project.id) };
    update(next); runtime?.store.dispose();
    delete dirty.current[project.id];
    runtimeRef.current = runtimeRef.current.filter((r) => r.project.id !== project.id); setRuntimes(runtimeRef.current);
    if (activeId === project.id) { setActiveId(""); if (next.projects[0]) await select(next.projects[0]); }
  };
  const active = runtimes.find((r) => r.project.id === activeId);
  if (import.meta.env.DEV) {
    window.__EXM_DESKTOP_ACTIVE_PROJECT__ = activeId;
    window.__EXM_DESKTOP_TEST_STORES__ = runtimes.map((runtime) => ({ projectId: runtime.project.id, store: runtime.store }));
  }
  return <div className="desk-shell">
    <nav className="desk-project-tabs" aria-label="项目切换">
      {catalog.projects.map((project) => {
        const runtime = runtimes.find((r) => r.project.id === project.id);
        return runtime ? <ProjectTab key={project.id} runtime={runtime} active={project.id === activeId} onSelect={() => void select(project)} /> : <button type="button" key={project.id} title={project.workspace} onClick={() => void select(project)}>{project.name}</button>;
      })}
      <Button type="text" icon={<PlusOutlined />} aria-label="项目与连接" onClick={() => setManager(true)} />
      <span className="desk-grow" />{connecting && <Spin size="small" />}
      {!active && <Button type="text" icon={<SettingOutlined />} aria-label="设置" onClick={() => setSettings(true)} />}
    </nav>
    {error && <Alert type="error" title={error} closable onClose={() => setError("")} />}
    <div className="desk-project-host">
      {!active && <div className="desk-boot"><Empty description="连接实例，打开你的项目" /><Button type="primary" onClick={() => setManager(true)}>项目与连接</Button></div>}
      {runtimes.map((runtime) => <div className="desk-project-instance" key={runtime.project.id} hidden={runtime.project.id !== activeId}>
      <ProjectContext.Provider value={runtime}><ClientServices.Provider value={{ api: runtime.client.api, assetUrl: runtime.client.assetUrl, downloadUrl: runtime.client.downloadUrl, dark: true, t: desktopTranslate }}>
          <ProjectWorkspace active={runtime.project.id === activeId} projects={catalog.projects} onSettings={() => setSettings(true)} onOpenProject={() => setManager(true)} onDirty={onDirty} />
        </ClientServices.Provider></ProjectContext.Provider>
      </div>)}
    </div>
    <ProjectManager open={manager} catalog={catalog} onClose={() => setManager(false)} onSave={update} onOpen={open} onRemove={remove} onCredentialsChanged={(id) => {
      for (const runtime of runtimeRef.current.filter((r) => r.project.connectionId === id)) {
        runtime.store.reconnect();
        const connection = catalogRef.current.connections.find((item) => item.id === id);
        if (connection) void desktopInvoke("set_event_gateway", {
          url: runtime.client.serverBase(), key: runtime.client.accessKey(), remote: connection.kind === "remote", workspace: runtime.project.workspace,
        }).catch((error: unknown) => message.error(String(error)));
      }
    }} />
    <DesktopSettings open={settings} onClose={() => setSettings(false)} consoleUrl={active?.consoleUrl} mobileUrl={active?.mobileUrl} accessKey={active?.client.accessKey()} client={active?.client}
      onRestartGateway={active?.local ? async (config) => {
        const instance = createClient({ url: active.client.serverBase(), accessKey: active.client.accessKey }, desktopTranslate);
        const check = await instance.req<{ runningSessions: number; pendingApprovals: number }>("/health");
        if (check.runningSessions) throw new Error(`${check.runningSessions} 个 Agent 任务仍在运行，请等待任务完成后再重启`);
        if (check.pendingApprovals) throw new Error(`${check.pendingApprovals} 个审批仍待处理，请先完成审批后再重启`);
        await desktopInvoke("set_desktop_config", { ...config });
        const gateway = await desktopInvoke<{ lanUrl?: string; consoleUrl: string }>("gateway_restart");
        const refreshed = runtimeRef.current.map((runtime) => runtime.project.connectionId === active.project.connectionId
          ? { ...runtime, mobileUrl: gateway.lanUrl, consoleUrl: runtime.local ? runtime.client.serverBase() : gateway.consoleUrl }
          : runtime);
        runtimeRef.current = refreshed; setRuntimes(refreshed);
      } : undefined} />
  </div>;
}
