import { useCallback, useEffect, useRef, useState } from "react";
import { Alert, App, Button, Empty, Input, Select, Spin, Tabs, Tree } from "antd";
import type { DataNode } from "antd/es/tree";
import { ReloadOutlined, SaveOutlined } from "@ant-design/icons";
import CodeMirror from "@uiw/react-codemirror";
import { javascript } from "@codemirror/lang-javascript";
import { rust } from "@codemirror/lang-rust";
import { json } from "@codemirror/lang-json";
import type { EditorFile, WorkspaceChanges, GitOverview, GitOperation } from "../../client/src/api/editor";
import { useProjectClient } from "./projects";

export interface OpenFile extends EditorFile { draft: string }
export function CodeEditor({ file, onChange, onSaved, onReload }: { file: OpenFile; onChange: (text: string) => void; onSaved: (path: string, content: string) => void; onReload: () => void }) {
  const { client: { editorApi } } = useProjectClient();
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState("");
  const busy = useRef(false);
  const readonly = file.binary || file.truncated;
  const save = useCallback(async () => {
    if (busy.current || readonly || file.content === file.draft) return;
    const { path, draft, content } = file;
    busy.current = true; setSaving(true); setError("");
    try { await editorApi.save(path, draft, content, file.workspace); onSaved(path, draft); }
    catch (e) { setError(String(e)); }
    finally { busy.current = false; setSaving(false); }
  }, [editorApi, file, readonly, onSaved]);
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => { if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "s") { e.preventDefault(); void save(); } };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [save]);
  const ext = file.path.split(".").pop();
  const extensions = ext === "rs" ? [rust()] : ext === "json" ? [json()] : ["js", "jsx", "ts", "tsx", "mjs", "cjs"].includes(ext ?? "") ? [javascript({ typescript: ext === "ts" || ext === "tsx", jsx: ext === "jsx" || ext === "tsx" })] : [];
  return <section className="desk-editor">
    <header><span>{file.path}{file.content !== file.draft ? " · 未保存" : ""}</span><Button aria-label="保存文件" icon={<SaveOutlined />} loading={saving} disabled={readonly || file.content === file.draft} onClick={() => void save()}>保存</Button></header>
    {error && <Alert type="error" title={error} description="本地修改已保留。重新加载前可先复制需要保留的内容。" action={<Button size="small" onClick={onReload}>重新加载文件</Button>} closable onClose={() => setError("")} />}
    {readonly && <Alert type="info" title="只读预览：文件过大或包含二进制数据" />}
    <CodeMirror key={file.path} className="desk-code-mirror" aria-label={`编辑 ${file.path}`} value={file.draft} theme="dark" extensions={extensions} editable={!readonly} onChange={onChange} height="100%" basicSetup={{ lineNumbers: true, foldGutter: true }} />
  </section>;
}

export type ProjectTab = "files" | "changes" | "git";
export function ProjectPanel({ workspace, onOpen, refresh, tab, onTabChange, onApproval }: { workspace: string; onOpen: (path: string) => void; refresh: number; tab: ProjectTab; onTabChange: (tab: ProjectTab) => void; onApproval: () => void }) {
  const { client: { editorApi } } = useProjectClient();
  const { message, modal } = App.useApp();
  const [tree, setTree] = useState<DataNode[]>([]);
  const [changes, setChanges] = useState<WorkspaceChanges | null>(null);
  const [git, setGit] = useState<GitOverview | null>(null);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const [loading, setLoading] = useState(true);
  const [commit, setCommit] = useState("");
  const generation = useRef(0);
  const operating = useRef(false);
  const load = useCallback(async () => {
    const seq = ++generation.current;
    setError(""); setLoading(true);
    try {
      const [files, nextChanges, overview] = await Promise.all([editorApi.list(), editorApi.changes(), editorApi.git()]);
      if (seq !== generation.current) return;
      setTree(files.entries.map((f) => ({ key: f.path, title: f.name, isLeaf: !f.dir })));
      setChanges(nextChanges); setGit(overview);
    } catch (e) { if (seq === generation.current) setError(String(e)); }
    finally { if (seq === generation.current) setLoading(false); }
  }, [editorApi]);
  useEffect(() => { setTree([]); void load(); return () => { generation.current += 1; }; }, [workspace, refresh, load]);
  const loadChildren = async ({ key }: DataNode) => {
    const seq = generation.current;
    try {
      const result = await editorApi.list(String(key));
      if (seq !== generation.current) return;
      const append = (nodes: DataNode[]): DataNode[] => nodes.map((node) => node.key === key
        ? { ...node, children: result.entries.map((f) => ({ key: f.path, title: f.name, isLeaf: !f.dir })) }
        : { ...node, children: node.children ? append(node.children) : undefined });
      setTree(append);
    } catch (e) { if (seq === generation.current) setError(String(e)); }
  };
  const run = async (op: GitOperation) => {
    if (operating.current) return;
    operating.current = true;
    setBusy(true); setError("");
    try {
      const result = await editorApi.operation(op);
      if (result.pending) { message.info("已创建审批，请确认后执行"); onApproval(); }
      else { message.success("Git 操作完成"); if (op.op === "commit") setCommit(""); }
      await load();
    } catch (e) { setError(String(e)); } finally { operating.current = false; setBusy(false); }
  };
  const discard = (path: string) => modal.confirm({ title: `丢弃 ${path} 的修改？`, content: "此操作将创建审批单，批准后执行。", okText: "创建审批", onOk: () => run({ op: "discard", path }) });
  return <section className="desk-project">
    <header><b title={workspace}>项目工作区</b><Button aria-label="刷新项目" type="text" icon={<ReloadOutlined />} onClick={() => void load()} /></header>
    {error && <Alert type="error" title={error} action={<Button size="small" onClick={() => void load()}>重试</Button>} />}
    <Spin spinning={busy || loading}><Tabs activeKey={tab} onChange={(key) => onTabChange(key as ProjectTab)} items={[
      { key: "files", label: "文件", children: tree.length ? <Tree treeData={tree} loadData={loadChildren} onSelect={(_, info) => { if (info.node.isLeaf) onOpen(String(info.node.key)); }} /> : <Empty description="目录为空" /> },
      { key: "changes", label: `变更 ${changes?.files.length ?? ""}`, children: <>
        {changes?.files.length && git?.repo ? <Button disabled={busy} onClick={() => void run({ op: "stage", path: "." })}>暂存全部</Button> : !changes?.files.length && <Empty description="工作区暂无变更" />}
        {changes?.files.map((f) => <article className="desk-change" key={f.path}>
          <button className="desk-file-link" onClick={() => onOpen(f.path)}>{f.status} {f.path}</button>
          {git?.repo && <div className="desk-change-actions"><Button size="small" onClick={() => void run({ op: f.staged ? "unstage" : "stage", path: f.path })}>{f.staged ? "取消暂存" : "暂存"}</Button><Button size="small" danger aria-label="丢弃修改" onClick={() => discard(f.path)}>丢弃</Button></div>}
          {f.diff && <details><summary>查看 diff</summary><pre>{f.diff.text.split("\n").map((line, i) => <span key={i} className={line.startsWith("+") ? "diff-add" : line.startsWith("-") ? "diff-del" : ""}>{line}{"\n"}</span>)}</pre></details>}
        </article>)}
        {git?.repo && <div className="desk-commit"><Input aria-label="提交信息" placeholder="提交信息" value={commit} onChange={(e) => setCommit(e.target.value)} /><Button disabled={!commit.trim() || busy} onClick={() => void run({ op: "commit", message: commit })}>提交已暂存文件</Button></div>}
      </> },
      { key: "git", label: "Git", children: git?.repo ? <>
        <Select aria-label="Git 分支" value={git.branch} options={git.branches?.map((b) => ({ value: b, label: b }))} onChange={(branch) => void run({ op: "switch", branch })} />
        <pre className="desk-git-log">{git.log?.join("\n") || "暂无提交"}</pre>
      </> : <Empty description="所选目录不是 Git 仓库" /> },
    ]} /></Spin>
  </section>;
}
