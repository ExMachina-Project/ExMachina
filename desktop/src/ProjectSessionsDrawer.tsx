import { useCallback, useEffect, useRef, useState } from "react";
import { Alert, App, Button, Checkbox, Drawer, Empty, Input, Segmented, Spin } from "antd";
import type { Session, SessionSearchHit } from "../../client/src/types";
import { useProjectClient } from "./projects";

/** Project-local management; never loads another project's or instance's session list. */
export function ProjectSessionsDrawer({ onClose, onSelected }: { onClose: () => void; onSelected: () => void }) {
  const { project, client: { api }, store } = useProjectClient();
  const { modal } = App.useApp();
  const [archived, setArchived] = useState(false);
  const [sessions, setSessions] = useState<Session[]>([]);
  const [hits, setHits] = useState<SessionSearchHit[] | null>(null);
  const [query, setQuery] = useState("");
  const [selected, setSelected] = useState<string[]>([]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [rename, setRename] = useState<{ id: string; title: string } | null>(null);
  const request = useRef(0);
  const load = useCallback(async () => {
    const sequence = ++request.current; setBusy(true); setError("");
    try {
      const list = archived ? await api.listArchivedSessions() : await api.listAllSessions();
      if (sequence !== request.current) return;
      setSessions(list); setSelected([]);
      if (!archived) store.setState({ sessions: list });
    } catch (e) { if (sequence === request.current) setError(String(e)); }
    finally { if (sequence === request.current) setBusy(false); }
  }, [api, archived, store]);
  useEffect(() => { void load(); return () => { request.current += 1; }; }, [load]);
  useEffect(() => {
    let cancelled = false;
    setHits(null);
    if (!query.trim() || archived) return;
    const timer = window.setTimeout(() => void api.searchSessions(query, 50).then((result) => {
      if (!cancelled) setHits(result.hits);
    }).catch((e: unknown) => { if (!cancelled) setError(String(e)); }), 250);
    return () => { cancelled = true; clearTimeout(timer); };
  }, [api, archived, query]);
  const visible = sessions.filter((s) => hits ? hits.some((h) => h.sessionId === s.id) : !query.trim() || `${s.title} ${s.lastMessagePreview ?? ""}`.toLowerCase().includes(query.trim().toLowerCase()));
  const batchIds = visible.filter((s) => selected.includes(s.id)).map((s) => s.id);
  const batch = async (action: "archive" | "unarchive" | "delete") => {
    setBusy(true); setError("");
    try {
      const result = await api.batchSessions(action, batchIds);
      await load();
      if (result.failed.length) setError(`部分操作未完成：${result.failed.map((f) => f.error).join("；")}`);
      const list = await api.listAllSessions(); store.setState({ sessions: list });
      if (!list.some((s) => s.id === store.getState().sessionId)) {
        const next = list.find((s) => !s.title.startsWith("sub:"));
        if (next) await store.getState().selectSession(next.id); else await store.getState().newSession();
      }
    } catch (e) { setError(String(e)); } finally { setBusy(false); }
  };
  return <Drawer open onClose={onClose} title={`${project.name} · 管理会话`} width="min(740px, 100vw)">
    <div className="desk-session-manager">
      {error && <Alert type="error" title={error} />}
      <Segmented value={archived ? "归档" : "会话"} options={["会话", "归档"]} onChange={(value) => { setArchived(value === "归档"); setQuery(""); }} />
      <Input.Search aria-label="搜索项目会话" placeholder={archived ? "搜索归档标题" : "搜索标题与消息"} value={query} onChange={(e) => setQuery(e.target.value)} allowClear />
      <div className="desk-session-batch"><Checkbox checked={!!visible.length && visible.every((s) => selected.includes(s.id))} onChange={(e) => setSelected(e.target.checked ? visible.map((s) => s.id) : [])}>全选</Checkbox>
        <Button disabled={!batchIds.length || busy} onClick={() => void batch(archived ? "unarchive" : "archive")}>{archived ? "恢复" : "归档"}</Button>
        <Button danger disabled={!batchIds.length || busy} onClick={() => modal.confirm({ title: `删除 ${batchIds.length} 个会话？`, content: "会话和消息记录将永久删除。", okButtonProps: { danger: true }, onOk: () => batch("delete") })}>删除</Button>
      </div>
      <Spin spinning={busy}>{!visible.length ? <Empty description="没有匹配的会话" /> : visible.map((s) => <article key={s.id} className="desk-managed-session">
        <Checkbox aria-label={`选择 ${s.title}`} checked={selected.includes(s.id)} onChange={(e) => setSelected((ids) => e.target.checked ? [...ids, s.id] : ids.filter((id) => id !== s.id))} />
        <button className="desk-file-link" disabled={archived} title={archived ? "恢复后可打开会话" : undefined} onClick={() => void store.getState().selectSession(s.id).then(() => { onSelected(); onClose(); }).catch((e: unknown) => setError(String(e)))}>{s.pinned ? "★ " : ""}{s.title}<small>{hits?.find((h) => h.sessionId === s.id)?.snippets[0]?.text ?? s.lastMessagePreview ?? s.groupId}</small></button>
        <Button size="small" onClick={() => setRename({ id: s.id, title: s.title })}>重命名</Button>
        <Button size="small" onClick={() => void api.pinSession(s.id, !s.pinned).then(load).catch((e: unknown) => setError(String(e)))}>{s.pinned ? "取消置顶" : "置顶"}</Button>
      </article>)}</Spin>
      {rename && <form onSubmit={(e) => { e.preventDefault(); if (!rename.title.trim() || busy) return; setBusy(true); void api.renameSession(rename.id, rename.title.trim()).then(() => { setRename(null); return load(); }).catch((e: unknown) => setError(String(e))).finally(() => setBusy(false)); }}>
        <Input aria-label="会话名称" value={rename.title} onChange={(e) => setRename({ ...rename, title: e.target.value })} /><Button htmlType="submit" loading={busy}>保存名称</Button><Button onClick={() => setRename(null)}>取消</Button>
      </form>}
    </div>
  </Drawer>;
}
