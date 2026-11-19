import { useEffect, useState } from "react";
import { Alert, App, Button, Empty, Input, Modal, Select, Tabs } from "antd";
import { open as openDirectory } from "@tauri-apps/plugin-dialog";
import { connectProject, connectionKey, newProjectId, normalizeServer, setConnectionKey, type Connection, type Project, type ProjectCatalog } from "./projects";
import type { ExmClient } from "../../client/src/api/client";

export function ProjectManager({ open, catalog, onClose, onSave, onOpen, onRemove, onCredentialsChanged }: {
  open: boolean; catalog: ProjectCatalog; onClose: () => void;
  onSave: (catalog: ProjectCatalog) => void;
  onOpen: (project: Project, client: ExmClient, mobileUrl?: string) => void;
  onRemove: (project: Project) => Promise<void>;
  onCredentialsChanged: (id: string) => void;
}) {
  const { message, modal } = App.useApp();
  const [tab, setTab] = useState("projects");
  const [connectionId, setConnectionId] = useState("");
  const [projectName, setProjectName] = useState("");
  const [path, setPath] = useState("");
  const [name, setName] = useState("");
  const [url, setUrl] = useState("");
  const [key, setKey] = useState("");
  const [editing, setEditing] = useState<string>();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  useEffect(() => { if (open) { setError(""); setConnectionId((id) => catalog.connections.some((c) => c.id === id) ? id : catalog.connections[0]?.id ?? ""); } }, [open, catalog.connections]);
  const connection = catalog.connections.find((c) => c.id === connectionId);
  const saveConnection = async () => {
    if (busy) return;
    setBusy(true); setError("");
    try {
      const id = editing ?? newProjectId();
      const next: Connection = { id, name: name.trim() || "远程实例", kind: "remote", url: normalizeServer(url) };
      // Replacing a live connection is explicit: projects continue using their captured transport.
      if (editing && catalog.projects.some((p) => p.connectionId === editing) && next.url !== catalog.connections.find((c) => c.id === editing)?.url) {
        throw new Error("此连接已绑定项目。更换地址请新增连接，避免原项目指向另一实例。");
      }
      const previousKey = connectionKey(id);
      setConnectionKey(id, key.trim());
      try { await connectProject(next); } catch (e) { setConnectionKey(id, previousKey); throw e; }
      onSave({ ...catalog, connections: [...catalog.connections.filter((c) => c.id !== id), next] });
      if (editing) onCredentialsChanged(id);
      setConnectionId(id); setEditing(undefined); setName(""); setUrl(""); setKey(""); setTab("projects");
      message.success("连接验证成功；密钥仅保留在本次客户端会话中");
    } catch (e) { setError(String(e)); } finally { setBusy(false); }
  };
  const addProject = async () => {
    if (!connection || busy || !path.trim()) return;
    setBusy(true); setError("");
    try {
      const result = await connectProject(connection, path.trim());
      const duplicate = catalog.projects.find((p) => p.connectionId === connection.id && p.workspace === result.workspace);
      const project = duplicate ?? { id: newProjectId(), connectionId: connection.id, workspace: result.workspace, name: projectName.trim() || result.workspace.split(/[\\/]/).filter(Boolean).at(-1) || "项目" };
      if (!duplicate) onSave({ ...catalog, projects: [...catalog.projects, project], activeId: project.id });
      onOpen(project, result.client, result.mobileUrl); setPath(""); setProjectName(""); onClose();
    } catch (e) { setError(String(e)); } finally { setBusy(false); }
  };
  return <Modal open={open} onCancel={() => !busy && onClose()} title="项目与连接" footer={null} width={680} className="desk-project-manager" destroyOnHidden>
    {error && <Alert type="error" title={error} closable onClose={() => setError("")} />}
    <Tabs activeKey={tab} onChange={setTab} items={[
      { key: "projects", label: "项目", children: <>
        <p className="desk-muted">每个项目独立保留会话、草稿与文件编辑。切换不会停止后台任务；实例级智能体和配置仍共享。</p>
        <div className="desk-project-list">{catalog.projects.map((p) => <div key={p.id}><span><b>{p.name}</b><small>{catalog.connections.find((c) => c.id === p.connectionId)?.name} · {p.workspace}</small></span><Button danger size="small" onClick={() => void onRemove(p)}>移除</Button></div>)}</div>
        {!catalog.connections.length ? <Empty description="先添加一个实例连接" /> : <div className="desk-project-form">
          <label>实例<Select aria-label="项目实例" value={connectionId} options={catalog.connections.map((c) => ({ value: c.id, label: `${c.kind === "local" ? "本机" : "远程"} · ${c.name}` }))} onChange={setConnectionId} /></label>
          <label>项目名称<Input aria-label="项目名称" placeholder="可选，默认使用目录名" value={projectName} onChange={(e) => setProjectName(e.target.value)} /></label>
          <label>项目目录<Input aria-label="项目目录" placeholder="网关所在机器上的绝对目录" value={path} onChange={(e) => setPath(e.target.value)} onPressEnter={() => void addProject()} /></label>
          {connection?.kind === "local" && window.__TAURI__ && <Button onClick={() => void openDirectory({ directory: true, multiple: false, title: "打开项目" }).then((value) => { if (typeof value === "string") setPath(value); }).catch((e: unknown) => setError(String(e)))}>选择本机目录</Button>}
          <Button type="primary" loading={busy} disabled={!path.trim()} onClick={() => void addProject()}>打开项目</Button>
        </div>}
      </> },
      { key: "connections", label: "连接", children: <>
        <div className="desk-project-list">{catalog.connections.map((c) => <div key={c.id}><span><b>{c.name}</b><small>{c.kind === "local" ? "本机托管实例，按需启动" : c.url}</small></span>{c.kind === "remote" && <>
          <Button size="small" onClick={() => { setEditing(c.id); setName(c.name); setUrl(c.url); setKey(connectionKey(c.id)); }}>更新密钥</Button>
          <Button size="small" danger onClick={() => modal.confirm({ title: `移除连接「${c.name}」？`, content: "仅移除本机保存的连接，不删除实例数据。请先移除该连接下的项目。", onOk: () => {
            if (catalog.projects.some((p) => p.connectionId === c.id)) throw new Error("请先移除该连接下的项目");
            onSave({ ...catalog, connections: catalog.connections.filter((item) => item.id !== c.id) }); setConnectionKey(c.id, "");
          } })}>移除</Button>
        </>}</div>)}</div>
        <div className="desk-project-form">
          <label>连接名称<Input aria-label="连接名称" value={name} onChange={(e) => setName(e.target.value)} /></label>
          <label>实例地址<Input aria-label="实例地址" placeholder="192.168.1.20:4173 或 https://host/exm" value={url} onChange={(e) => setUrl(e.target.value)} /></label>
          <label>访问密钥<Input.Password aria-label="访问密钥" value={key} onChange={(e) => setKey(e.target.value)} autoComplete="off" /></label>
          <small className="desk-muted">地址和项目保存在本机；密钥只保留在当前客户端会话，不写入连接列表。非可信局域网请使用 HTTPS 或安全隧道。</small>
          <Button type="primary" loading={busy} onClick={() => void saveConnection()}>{editing ? "验证并更新密钥" : "验证并保存连接"}</Button>
          {editing && <Button onClick={() => { setEditing(undefined); setName(""); setUrl(""); setKey(""); }}>取消更新</Button>}
        </div>
      </> },
    ]} />
  </Modal>;
}
