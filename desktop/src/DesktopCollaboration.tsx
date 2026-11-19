import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { Alert, App, Button, Checkbox, Empty, Input, Modal, Select, Spin, Tag } from "antd";
import { ArrowUpOutlined, DeleteOutlined, PlusOutlined } from "@ant-design/icons";
import { COLLABORATION_FEATURE, type CollaborationMember, type CollaborationPage, type CollaborationRoom, type CollaborationTurn, type CollaborationApproval } from "../../client/src/api/collaborations";
import { createClient } from "../../client/src/api/client";
import { Markdown } from "../../client/src/components/Markdown";
import { MessageProcess } from "../../client/src/components/ProcessSteps";
import { desktopTranslate, newProjectId, useProjectClient, type Project } from "./projects";

function memberName(member: CollaborationMember, projects: Project[]) {
  const project = projects.find((item) => item.workspace === member.workspace)?.name
    ?? (member.workspace.split(/[\\/]/).filter(Boolean).at(-1) || member.workspace);
  return `${project} · ${member.target.startsWith("single:") ? "智能体" : "组"} · ${member.label}`;
}

type Option = { value: string; label: string };

function CreateCollaboration({ projects, onClose, onCreate }: { projects: Project[]; onClose: () => void; onCreate: (room: CollaborationRoom) => void }) {
  const { client, project } = useProjectClient();
  const [title, setTitle] = useState("");
  const [members, setMembers] = useState(() => [
    { key: 0, projectId: project.id, target: "" },
    { key: 1, projectId: projects.find((item) => item.id !== project.id)?.id ?? project.id, target: "" },
  ]);
  const sequence = useRef(2);
  const [targets, setTargets] = useState<Option[]>([]);
  const [error, setError] = useState("");
  const [loading, setLoading] = useState(true);
  const busy = useRef(false);
  const [saving, setSaving] = useState(false);
  useEffect(() => {
    let cancelled = false;
    void Promise.all([client.api.groups(), client.api.listSingles()]).then(([groups, singles]) => {
      if (cancelled) return;
      setTargets([
        ...singles.singles.map((item) => ({ value: `single:${item.identifier}`, label: `智能体 · ${item.name}（${item.identifier}）` })),
        ...groups.groups.map((item) => ({ value: item.id, label: `组 · ${item.name}（${item.id}）` })),
      ]);
    }).catch((e: unknown) => { if (!cancelled) setError(String(e)); }).finally(() => { if (!cancelled) setLoading(false); });
    return () => { cancelled = true; };
  }, [client]);
  const valid = members.every((row) => row.target && projects.some((item) => item.id === row.projectId));
  const duplicate = new Set(members.map(({ projectId, target }) => JSON.stringify([projectId, target]))).size < members.length && valid;
  const create = async () => {
    if (busy.current || !valid || duplicate || !title.trim()) return;
    busy.current = true; setSaving(true); setError("");
    try {
      const room = await client.collaborations.create(title.trim(), members.map((row) => ({
        workspace: projects.find((item) => item.id === row.projectId)!.workspace, target: row.target,
      })));
      onCreate(room);
    } catch (e) { setError(String(e)); }
    finally { busy.current = false; setSaving(false); }
  };
  const update = (key: number, field: "projectId" | "target", value: string) => setMembers((rows) => rows.map((row) => row.key === key ? { ...row, [field]: value } : row));
  return <Modal open title="新建协作群聊" className="desk-project-manager" okText="创建群聊" cancelText="取消" confirmLoading={saving}
    okButtonProps={{ disabled: !title.trim() || !valid || duplicate || loading }} onCancel={() => { if (!busy.current) onClose(); }} onOk={() => void create()}>
    <div className="desk-project-form">
      <p className="desk-collab-note">群聊保存在「{project.name}」。可加入同一连接下不同项目的智能体和组；成员创建后固定。</p>
      <label>群聊名称<Input aria-label="群聊名称" value={title} maxLength={100} onChange={(event) => setTitle(event.target.value)} disabled={saving} /></label>
      <div className="desk-collab-roster">
        {members.map((row, index) => <div key={row.key} className="desk-collab-member-form">
          <label>项目 {index + 1}<select aria-label={`成员 ${index + 1} 项目`} value={row.projectId} disabled={saving} onChange={(event) => update(row.key, "projectId", event.target.value)}>
            {projects.map((item) => <option key={item.id} value={item.id}>{item.name} · {item.workspace}</option>)}
          </select></label>
          <label>对话对象<Select aria-label={`成员 ${index + 1} 对象`} value={row.target || undefined} placeholder="智能体或组" options={targets} loading={loading} disabled={saving} onChange={(value) => update(row.key, "target", value)} /></label>
          <Button type="text" icon={<DeleteOutlined />} aria-label={`移除成员 ${index + 1}`} disabled={members.length <= 2 || saving} onClick={() => setMembers((rows) => rows.filter((item) => item.key !== row.key))} />
        </div>)}
      </div>
      <Button icon={<PlusOutlined />} disabled={members.length >= 8 || saving} onClick={() => setMembers((rows) => [...rows, { key: sequence.current++, projectId: project.id, target: "" }])}>添加成员</Button>
      {duplicate && <Alert type="warning" title="同一项目内请选择不同的对象" />}
      {error && <Alert type="error" title={error} />}
    </div>
  </Modal>;
}

type Draft = { text: string; tasks: Record<string, string>; requestId?: string };
const blankDraft: Draft = { text: "", tasks: {} };
function readDraft(key: string): Draft {
  try {
    const value = JSON.parse(sessionStorage.getItem(key) ?? "null") as Draft | null;
    if (value && typeof value.text === "string" && value.tasks && typeof value.tasks === "object") return value;
  } catch { /* unavailable storage */ }
  return blankDraft;
}

// getRandomValues is also available on WebViews without a secure-context randomUUID().
const turnId = () => newProjectId().replace(/^(........)(....)(....)(....)(............)$/, "$1-$2-$3-$4-$5");
const states = { running: "执行中 / 等待执行", done: "已完成", failed: "失败", stopped: "已停止", interrupted: "已中断 · 未自动重跑" };

function mergeLatest(previous: CollaborationPage | null, latest: CollaborationPage): CollaborationPage {
  const overlap = previous?.turns.findIndex((turn) => turn.id === latest.turns[0]?.id) ?? -1;
  return previous && overlap > 0
    ? { ...latest, nextBefore: previous.nextBefore, turns: [...previous.turns.slice(0, overlap), ...latest.turns] }
    : latest;
}

function RoomChat({ room, projects, active }: { room: CollaborationRoom; projects: Project[]; active: boolean }) {
  const { client, project } = useProjectClient();
  const { message } = App.useApp();
  const storageKey = `exm.desktop.collaboration.draft.${project.id}.${room.id}`;
  const [draft, setDraft] = useState(() => readDraft(storageKey));
  const [page, setPage] = useState<CollaborationPage | null>(null);
  const [refresh, setRefresh] = useState(0);
  const [error, setError] = useState("");
  const [sending, setSending] = useState(false);
  const submit = useRef(false);
  const [awaitingTurn, setAwaitingTurn] = useState<string>();
  const [loadingOlder, setLoadingOlder] = useState(false);
  const [stopping, setStopping] = useState<string>();
  const [deciding, setDeciding] = useState<string>();
  const stream = useRef<HTMLDivElement>(null);
  const stick = useRef(true);
  const roster = new Map(room.members.map((member) => [member.sessionId, member]));
  const recipients = room.members.flatMap((m) => typeof draft.tasks[m.sessionId] === "string" ? [{ sessionId: m.sessionId, instruction: draft.tasks[m.sessionId].trim() }] : []);
  const canSend = !!page && !error && !page.activeTurnId && !awaitingTurn && !sending && recipients.length > 0 && recipients.every((item) => draft.text.trim() || item.instruction);
  const reload = () => setRefresh((value) => value + 1);
  const updateDraft = (value: Draft) => {
    setDraft(value);
    try { sessionStorage.setItem(storageKey, JSON.stringify(value)); } catch { /* storage unavailable */ }
  };
  useEffect(() => {
    if (!active) return;
    const controller = new AbortController();
    let timer: number;
    const load = async () => {
      let interval = 6_000;
      try {
        const latest = await client.collaborations.get(room.id, undefined, controller.signal);
        if (controller.signal.aborted) return;
        setPage((current) => mergeLatest(current, latest)); setError("");
        if (awaitingTurn && latest.turns.some((turn) => turn.id === awaitingTurn)) setAwaitingTurn(undefined);
        if (latest.activeTurnId || awaitingTurn) interval = 1_000;
      } catch (e) {
        if (!controller.signal.aborted) setError(String(e));
      }
      if (!controller.signal.aborted) timer = window.setTimeout(() => void load(), interval);
    };
    void load();
    return () => { controller.abort(); window.clearTimeout(timer); };
  }, [active, awaitingTurn, client, refresh, room.id]);
  useLayoutEffect(() => {
    const node = stream.current;
    if (active && node && stick.current) node.scrollTop = node.scrollHeight;
  }, [active, page]);
  const loadEarlier = async () => {
    const before = page?.nextBefore;
    if (!before || loadingOlder) return;
    setLoadingOlder(true); stick.current = false;
    try {
      const earlier = await client.collaborations.get(room.id, before);
      setPage((current) => current?.nextBefore === before ? { ...current, nextBefore: earlier.nextBefore, turns: [...earlier.turns, ...current.turns] } : current);
    } catch (e) { message.error(String(e)); }
    finally { setLoadingOlder(false); }
  };
  const send = async () => {
    if (!canSend || submit.current) return;
    submit.current = true; setSending(true);
    const requestId = draft.requestId ?? turnId();
    updateDraft({ ...draft, requestId });
    try {
      const accepted = await client.collaborations.send(room.id, { requestId, text: draft.text.trim(), recipients });
      setAwaitingTurn(accepted.id); updateDraft(blankDraft); stick.current = true;
    } catch (e) {
      message.error(String(e)); reload();
    } finally { submit.current = false; setSending(false); }
  };
  const stop = async (turn: string, sessionId?: string) => {
    if (stopping) return;
    setStopping(sessionId ?? turn);
    try { await client.collaborations.stop(room.id, turn, sessionId); message.info("已请求停止，不会回滚已完成的操作"); }
    catch (e) { message.error(String(e)); }
    finally { setStopping(undefined); reload(); }
  };
  const decide = async (approval: CollaborationApproval, member: CollaborationMember, approve: boolean) => {
    if (deciding) return;
    setDeciding(approval.id);
    try {
      const scoped = createClient({ url: client.serverBase(), accessKey: client.accessKey, workspace: member.workspace }, desktopTranslate);
      const result = await scoped.api.decideApproval(approval.id, approve, "once");
      if (result.status === "failed") message.error(result.result ?? "审批操作执行失败");
    } catch (e) { message.error(String(e)); }
    finally { setDeciding(undefined); reload(); }
  };
  const renderTurn = (turn: CollaborationTurn) => <article key={turn.id} className="desk-collab-turn">
    <header><b>你</b><time dateTime={turn.createdAt}>{new Date(turn.createdAt).toLocaleString()}</time></header>
    {turn.text && <div className="desk-collab-user-text">{turn.text}</div>}
    {turn.deliveries.map((delivery) => {
      const member = roster.get(delivery.sessionId);
      if (!member) return null;
      const label = memberName(member, projects);
      const latestReply = delivery.messages?.filter((m) => m.role !== "system").at(-1);
      return <section className="desk-collab-delivery" key={member.sessionId}>
        <header><b title={member.workspace}>{label}</b><Tag color={delivery.status === "running" ? "processing" : delivery.status === "failed" ? "error" : undefined}>{states[delivery.status]}</Tag>
          {turn.id === page?.activeTurnId && delivery.status === "running" && <Button size="small" type="text" loading={stopping === member.sessionId} disabled={!!stopping} onClick={() => void stop(turn.id, member.sessionId)} aria-label={`停止 ${label}`}>停止</Button>}
        </header>
        {delivery.instruction && <div className="desk-collab-user-text"><small>分工</small> {delivery.instruction}</div>}
        {delivery.error && <Alert type="error" title={delivery.error} />}
        {delivery.messages?.map((item) => <div className="desk-collab-reply" key={item.id}>
          {item.role === "unit" && item.agentId && <small>{item.agentId}</small>}
          <MessageProcess thinking={item.thinking} steps={item.steps} toolCalls={item.toolCalls} sessionId={item.sessionId} turn={item.turn ?? 0} labelThinking={(n) => `思考过程 · ${n} 字`} labelTools={(n) => `工具调用 · ${n}`} />
          <Markdown text={item.statements.map((s) => s.text).join("\n\n")} />
        </div>)}
        {latestReply && <Button type="link" size="small" disabled={sending} onClick={() => {
          const excerpt = latestReply.statements.map((item) => item.text).join("\n").slice(0, 4_000);
          const quote = excerpt.split("\n").map((line) => `> ${line}`).join("\n");
          updateDraft({ tasks: draft.tasks, text: `${draft.text ? `${draft.text}\n\n` : ""}引用自 ${label}：\n${quote}` });
          message.info("已引用到草稿；发送前请选择收件人并确认内容");
        }}>引用到草稿</Button>}
      </section>;
    })}
  </article>;
  return <div className="desk-chat desk-collab-chat">
    {error && <Alert type="error" title="群聊状态暂未同步" description={error} action={<Button size="small" onClick={reload}>重试</Button>} />}
    <div className="desk-stream desk-collab-stream" ref={stream} onScroll={(event) => {
      const el = event.currentTarget; stick.current = el.scrollHeight - el.scrollTop - el.clientHeight < 80;
    }}>
      {!page && !error && <Spin size="small" />}
      {page && !page.turns.length && <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description="选择收件人后发送消息，或分别下达任务。" />}
      {page?.nextBefore && <Button size="small" loading={loadingOlder} onClick={() => void loadEarlier()}>更早的群聊记录</Button>}
      {page?.turns.map(renderTurn)}
      {page?.approvals.map((approval) => {
        const member = roster.get(approval.memberSessionId);
        if (!member) return null;
        return <article className="approval-card" key={approval.id}>
          <header><b>待审批</b><span title={member.workspace}>{memberName(member, projects)} · {approval.agentId}</span></header>
          <pre>{approval.command}</pre>
          <div><Button disabled={!!deciding || !!error} loading={deciding === approval.id} onClick={() => void decide(approval, member, true)}>批准一次</Button>
            <Button danger disabled={!!deciding || !!error} onClick={() => void decide(approval, member, false)}>拒绝</Button></div>
        </article>;
      })}
    </div>
    <footer className="desk-composer desk-collab-composer">
      <Input.TextArea value={draft.text} aria-label="群聊消息" placeholder="给选中成员的公共消息（也可以只写分工）" autoSize={{ minRows: 2, maxRows: 5 }} maxLength={32_000} disabled={sending}
        onChange={(event) => updateDraft({ text: event.target.value, tasks: draft.tasks })} />
      <div className="desk-collab-recipients" role="group" aria-label="本轮收件人及分工">
        {room.members.map((member) => {
          const label = memberName(member, projects);
          const checked = Object.hasOwn(draft.tasks, member.sessionId);
          return <div key={member.sessionId} className="desk-collab-recipient">
            <Checkbox checked={checked} disabled={sending} title={member.workspace} onChange={(event) => {
              const tasks = { ...draft.tasks };
              if (event.target.checked) tasks[member.sessionId] = ""; else delete tasks[member.sessionId];
              updateDraft({ text: draft.text, tasks });
            }}>{label}</Checkbox>
            {checked && <Input.TextArea value={draft.tasks[member.sessionId]} aria-label={`${label} 的分工`} placeholder="只发给此成员的分工（选填）" autoSize={{ minRows: 1, maxRows: 3 }} maxLength={16_000} disabled={sending}
              onChange={(event) => updateDraft({ text: draft.text, tasks: { ...draft.tasks, [member.sessionId]: event.target.value } })} />}
          </div>;
        })}
      </div>
      <div className="desk-composer-tools"><small>已选 {recipients.length} 位 · 仅转发本轮内容</small><span className="desk-grow" />
        {page?.activeTurnId && <Button size="small" danger loading={stopping === page.activeTurnId} disabled={!!stopping} onClick={() => void stop(page.activeTurnId!)}>停止本轮</Button>}
        <Button type="primary" icon={<ArrowUpOutlined />} aria-label="发送给所选成员" loading={sending || !!awaitingTurn} disabled={!canSend} onClick={() => void send()}>发送给所选成员</Button>
      </div>
    </footer>
  </div>;
}

export function DesktopCollaboration({ active, projects }: { active: boolean; projects: Project[] }) {
  const { client, project } = useProjectClient();
  const sameInstance = projects.filter((item) => item.connectionId === project.connectionId);
  const [rooms, setRooms] = useState<CollaborationRoom[] | null>(null);
  const [id, setId] = useState("");
  const [unsupported, setUnsupported] = useState(false);
  const [error, setError] = useState("");
  const [reload, setReload] = useState(0);
  const [creating, setCreating] = useState(false);
  useEffect(() => {
    if (!active) return;
    const controller = new AbortController();
    void (async () => {
      try {
        const health = await client.req<{ features?: string[] }>("/health", { signal: controller.signal, cache: "no-store" });
        if (controller.signal.aborted) return;
        const supported = health.features?.includes(COLLABORATION_FEATURE) ?? false;
        setUnsupported(!supported);
        if (!supported) { setError(""); return; }
        const list = await client.collaborations.list(controller.signal);
        if (controller.signal.aborted) return;
        setRooms(list); setError("");
        setId((current) => list.some((room) => room.id === current) ? current : list[0]?.id ?? "");
      } catch (e) { if (!controller.signal.aborted) setError(String(e)); }
    })();
    return () => controller.abort();
  }, [active, client, reload]);
  const room = rooms?.find((item) => item.id === id);
  if (unsupported) return <Alert type="info" title="该网关不支持协作群聊" description="升级网关后可在同一实例内组织跨项目协作。普通对话仍可使用。" action={<Button size="small" onClick={() => setReload((n) => n + 1)}>重新检测</Button>} />;
  return <section className="desk-collaboration" aria-label="项目协作群聊">
    <header className="desk-collab-nav">
      <Select aria-label="选择协作群聊" placeholder="选择协作群聊" value={id || undefined} options={rooms?.map((item) => ({ value: item.id, label: item.title }))} onChange={setId} disabled={!rooms?.length} />
      <Button icon={<PlusOutlined />} aria-label="新建协作群聊" disabled={!rooms || !!error} onClick={() => setCreating(true)}>新建协作群聊</Button>
    </header>
    {error && <Alert type="error" title={error} action={<Button size="small" onClick={() => setReload((n) => n + 1)}>重试</Button>} />}
    {room ? <RoomChat key={room.id} room={room} projects={sameInstance} active={active} /> : !error && (rooms ? <Empty description="添加同一实例中的项目，再选择智能体或组，一起工作。" /> : <Spin />)}
    {active && creating && <CreateCollaboration projects={sameInstance} onClose={() => setCreating(false)} onCreate={(created) => {
      setRooms((current) => [created, ...(current ?? [])]); setId(created.id); setCreating(false);
    }} />}
  </section>;
}
