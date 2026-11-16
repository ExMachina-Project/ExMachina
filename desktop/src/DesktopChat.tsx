import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { TextAreaRef } from "antd/es/input/TextArea";
import { App, Button, Collapse, Input, Modal, Select, Tag } from "antd";
import { ArrowUpOutlined, PaperClipOutlined, PauseOutlined, CopyOutlined, EditOutlined, ForkOutlined, RollbackOutlined } from "@ant-design/icons";
import { useExm } from "../../webui/src/store";
import { api, assetUrl, downloadUrl, workbenchTree, isDestructiveCommand, type WorkbenchNode } from "../../webui/src/api";
import { Markdown, copyText } from "../../webui/src/components/Markdown";
import { ToolCallList } from "../../webui/src/components/ToolCall";
import { MessageProcess, ProcessSteps } from "../../webui/src/components/ProcessSteps";
import { FileMentionPopup, mentionCandidates, type MentionState } from "../../webui/src/components/FileMention";
import { PlanCard } from "../../webui/src/components/PlanCard";
import { useAttachments } from "../../webui/src/hooks/useAttachments";
import { useChatStream } from "../../webui/src/hooks/useChatStream";
import type { ChatMessage } from "../../webui/src/types";

function Message({ item, turn, onEdit, onUndo, onFork }: { item: ChatMessage; turn: number; onEdit?: () => void; onUndo?: () => void; onFork?: () => void }) {
  const text = item.statements.map((s) => s.text).join("\n\n");
  return <article className={`desk-message ${item.role}`} data-message-id={item.id}>
    <header><b>{item.role === "user" ? "你" : item.agentId || "Agent"}</b><Button type="text" size="small" icon={<CopyOutlined />} aria-label="复制消息" onClick={() => void copyText(text)} />
      {onEdit && <Button type="text" size="small" icon={<EditOutlined />} aria-label="编辑并重发" onClick={onEdit} />}
      {onUndo && <Button type="text" size="small" icon={<RollbackOutlined />} aria-label="撤销此轮" onClick={onUndo} />}
      {onFork && <Button type="text" size="small" icon={<ForkOutlined />} aria-label="从此处分叉" onClick={onFork} />}
    </header>
    <MessageProcess thinking={item.thinking} steps={item.steps} toolCalls={item.toolCalls} sessionId={item.sessionId} turn={turn} labelThinking={(n) => `思考过程 · ${n} 字`} labelTools={(n) => `工具调用 · ${n}`} />
    <Markdown text={text} />
    {item.attachments?.map((attachment, index) => <a key={index} className="desk-artifact" href={downloadUrl(attachment.path)} target="_blank" rel="noreferrer">{attachment.kind === "image" && <img src={assetUrl(attachment.path)} alt={attachment.name || "图片附件"} />}{attachment.name || attachment.path}</a>)}
    {item.artifacts?.map((a) => <a className="desk-artifact" key={a.path} href={downloadUrl(a.path)} target="_blank" rel="noreferrer">
      {a.kind === "image" && <img src={assetUrl(a.path)} alt={a.caption || a.path} />}{a.caption || a.path}
    </a>)}
  </article>;
}

export function DesktopChat({ onProject, workspace, active, contextVersion }: { onProject: () => void; workspace: string; active: boolean; contextVersion: number }) {
  const { message, modal } = App.useApp();
  const state = useExm();
  const { sessionId, messages, running, liveOrch, liveThinking, liveParts, runToolCalls, timeline, approvals } = state;
  const [text, setText] = useState("");
  const [sending, setSending] = useState(false);
  const [stopping, setStopping] = useState(false);
  const [agent, setAgent] = useState<string>();
  const [profile, setProfile] = useState("");
  const [profiles, setProfiles] = useState<{ value: string; label: string }[]>([]);
  const [target, setTarget] = useState<{ mode: "group" | "single"; id: string }>({ mode: "group", id: "default" });
  const [targets, setTargets] = useState<{ value: string; label: string }[]>([]);
  const submit = useRef(false);
  const pick = useRef<HTMLInputElement>(null);
  const input = useRef<TextAreaRef>(null);
  const contextRequest = useRef(0);
  const [changingTarget, setChangingTarget] = useState(false);
  const [editing, setEditing] = useState<{ sessionId: string; turn: number; text: string } | null>(null);
  const [historyBusy, setHistoryBusy] = useState(false);
  const [mention, setMention] = useState<MentionState>({ open: false, query: "", start: 0 });
  const [mentionIndex, setMentionIndex] = useState(0);
  const [mentionNodes, setMentionNodes] = useState<WorkbenchNode[]>([]);
  const [mentionLoading, setMentionLoading] = useState(false);
  const [mentionLoaded, setMentionLoaded] = useState(false);
  const mentionRequest = useRef(0);
  const attachments = useAttachments(sessionId);
  const stream = useMemo(() => ({ liveOrch, liveThinking, liveParts, runToolCalls, timeline }), [liveOrch, liveThinking, liveParts, runToolCalls, timeline]);
  const scroll = useChatStream(sessionId ?? undefined, messages, running, approvals, stream);
  const loadContext = useCallback(async () => {
    const request = ++contextRequest.current;
    const [t, groups, singles, llm, agents] = await Promise.all([api.getTarget(), api.groups(), api.listSingles(), api.llmProfiles(), api.agents()]);
    if (request !== contextRequest.current) return;
    setTarget(t);
    setTargets([...singles.singles.map((s) => ({ value: `single:${s.identifier}`, label: s.name })), ...groups.groups.map((g) => ({ value: `group:${g.id}`, label: `组 · ${g.name}` }))]);
    setProfile(t.mode === "single" ? singles.singles.find((s) => s.identifier === t.id)?.modelHint ?? "" : groups.groups.find((g) => g.id === t.id)?.model ?? "");
    setProfiles([{ value: "", label: "跟随默认模型" }, ...llm.profiles.flatMap((p) => [
      { value: p.id, label: `${p.model} · ${p.name}` },
      ...p.models.filter((model) => model.model !== p.model).map((model) => ({ value: `${p.id}/${model.model}`, label: `${model.model} · ${p.name}` })),
    ])]);
    useExm.setState({ agents });
  }, []);
  useEffect(() => { void loadContext().catch((error: unknown) => message.error(String(error))); return () => { contextRequest.current += 1; }; }, [loadContext, contextVersion, message]);
  useEffect(() => { setText(sessionId ? useExm.getState().drafts[sessionId] ?? "" : ""); setAgent(undefined); setEditing(null); setMention((current) => ({ ...current, open: false })); }, [sessionId]);
  useEffect(() => {
    if (active && scroll.stickRef.current) scroll.bottomRef.current?.scrollIntoView();
  }, [active, scroll.bottomRef, scroll.stickRef]);
  useEffect(() => {
    mentionRequest.current += 1; setMentionLoaded(false); setMentionLoading(false); setMentionNodes([]); setMention((current) => ({ ...current, open: false }));
  }, [workspace, target.id]);
  useEffect(() => {
    if (!mention.open || mentionLoaded || mentionLoading) return;
    const request = ++mentionRequest.current;
    setMentionLoading(true);
    void workbenchTree("", 6).then((result) => {
      if (request !== mentionRequest.current) return;
      setMentionNodes(result.nodes); setMentionLoaded(true);
    }).catch((error: unknown) => { if (request === mentionRequest.current) { message.error(String(error)); setMention((current) => ({ ...current, open: false })); } })
      .finally(() => { if (request === mentionRequest.current) setMentionLoading(false); });
  }, [mention.open, mentionLoaded, mentionLoading, message]);
  const draft = (value: string) => { setText(value); if (sessionId) state.setDraft(sessionId, value); };
  const detectMention = (value: string, caret: number) => {
    const before = value.slice(0, caret); const at = before.lastIndexOf("@"); const query = before.slice(at + 1);
    setMention({ open: at >= 0 && (at === 0 || /\s/.test(before[at - 1])) && query.length <= 48 && !/[\s@]/.test(query), start: at, query });
  };
  const applyMention = (path: string) => {
    const next = text.slice(0, mention.start) + path + " " + text.slice(mention.start).replace(/@[^\s@]*/, "").replace(/^ /, "");
    draft(next); setMention((current) => ({ ...current, open: false })); setMentionIndex(0);
    requestAnimationFrame(() => { input.current?.focus(); const caret = mention.start + path.length + 1; input.current?.resizableTextArea?.textArea.setSelectionRange(caret, caret); });
  };
  const send = async () => {
    if (!text.trim() || !sessionId || submit.current || changingTarget || running) return;
    submit.current = true; setSending(true);
    const id = sessionId; const submitted = text; const images = [...attachments.images];
    state.setDraft(id, submitted);
    try {
      if (await state.send(submitted, images, undefined, agent)) {
        if (useExm.getState().sessionId === id) setText((now) => now === submitted ? "" : now);
        attachments.setImages((now) => { const remaining = [...now]; for (const image of images) { const at = remaining.indexOf(image); if (at >= 0) remaining.splice(at, 1); } return remaining; });
      }
    } finally { submit.current = false; setSending(false); }
  };
  const stop = async () => {
    if (!sessionId || stopping) return;
    setStopping(true);
    try { await api.stopSession(sessionId); } catch (e) { message.error(String(e)); }
    finally { setStopping(false); }
  };
  const [deciding, setDeciding] = useState<string[]>([]);
  const decide = async (id: string, approve: boolean, scope: "once" | "session" | "allowlist" = "once") => {
    if (deciding.includes(id)) return;
    setDeciding((ids) => [...ids, id]);
    try { await state.decideApproval(id, approve, scope); } catch (e) { message.error(String(e)); }
    finally { setDeciding((ids) => ids.filter((s) => s !== id)); }
  };
  const chooseTarget = async (value: string) => {
    const mode = value.startsWith("single:") ? "single" : "group";
    const id = value.slice(mode === "single" ? 7 : 6);
    if (!id || changingTarget) return;
    setChangingTarget(true);
    try {
      await state.setTarget(mode, id);
      setTarget({ mode, id });
      setAgent(undefined);
      await loadContext();
    } catch (e) { message.error(String(e)); }
    finally { setChangingTarget(false); }
  };
  const undo = (turn: number) => modal.confirm({ title: "撤销此轮及后续消息？", content: "此轮文件修改也将恢复。", onOk: async () => { if (!sessionId) return; await api.undoSession(sessionId, turn - 1, true); if (useExm.getState().sessionId === sessionId) await state.selectSession(sessionId); } });
  const fork = async (turn: number) => {
    if (!sessionId || historyBusy) return;
    setHistoryBusy(true);
    try { const result = await api.forkSession(sessionId, turn); const session = await api.getSession(result.id); useExm.setState((current) => ({ sessions: [session, ...current.sessions] })); await state.selectSession(result.id); }
    catch (error) { message.error(String(error)); } finally { setHistoryBusy(false); }
  };
  const edit = async () => {
    if (!editing || historyBusy || !editing.text.trim()) return;
    setHistoryBusy(true);
    try { await api.editSession(editing.sessionId, editing.turn, editing.text); setEditing(null); if (useExm.getState().sessionId === editing.sessionId) await state.selectSession(editing.sessionId); }
    catch (error) { message.error(String(error)); } finally { setHistoryBusy(false); }
  };
  let userTurn = 0;
  const primaryAgent = agent || (target.mode === "single" ? target.id : "");
  const primaryParts = liveParts[primaryAgent] ?? [];
  return <section className="desk-chat">
    <div id="stream" className="chat-scroll desk-stream">
      {!messages.length && !running && <div className="desk-welcome">
        <div className="desk-eyebrow">EXMACHINA / CODING AGENT</div><h1>从你的项目开始</h1>
        <p>{workspace ? "分析代码、实施修改、检查结果。" : "打开项目目录，或先描述你想完成的任务。"}</p>
        <div className="suggest">{["梳理项目结构", "检查代码问题", "制定实施计划"].map((label) => <button key={label} onClick={() => { draft(`${label}，给出具体步骤和验收标准。`); input.current?.focus(); }}>{label}<span>↗</span></button>)}</div>
        {!workspace && <Button onClick={onProject}>打开项目</Button>}
      </div>}
      {messages.map((item) => {
        if (item.role === "user") userTurn += 1;
        const turn = item.turn ?? userTurn;
        const operations = item.role === "user" && !running && !historyBusy && !item.id.startsWith("local-");
        return <Message key={item.id} item={item} turn={turn}
          onEdit={operations && sessionId ? () => setEditing({ sessionId, turn, text: item.statements.map((statement) => statement.text).join("\n") }) : undefined}
          onUndo={operations ? () => undo(turn) : undefined} onFork={operations ? () => void fork(turn) : undefined} />;
      })}
      {(running || liveOrch || liveThinking) && <article className="desk-message assistant">
        <header><b>{agent || "Agent"}</b>{running && <Tag color="processing">执行中</Tag>}</header>
        {primaryParts.length ? <ProcessSteps parts={primaryParts} toolCalls={runToolCalls} live={running} labelThinking={(n) => `思考过程 · ${n} 字`} labelTools={(n) => `工具调用 · ${n}`} /> : <>
          {liveThinking && <details className="desk-thinking"><summary>思考过程</summary><pre>{liveThinking}</pre></details>}
          {!!runToolCalls.length && <ToolCallList calls={runToolCalls} />}
          {liveOrch ? <div className="live-pre"><Markdown text={liveOrch} /></div> : !runToolCalls.length && <span className="desk-muted">正在处理任务…</span>}
        </>}
      </article>}
      {Object.entries(liveParts).filter(([id, parts]) => parts.length && id !== primaryAgent).map(([id, parts]) => <details key={id} className="desk-message"><summary>{id} · 执行过程</summary><ProcessSteps parts={parts} toolCalls={runToolCalls} live={running} labelThinking={(n) => `思考过程 · ${n} 字`} labelTools={(n) => `工具调用 · ${n}`} /></details>)}
      {!!timeline.length && <Collapse size="small" items={[{ key: "timeline", label: "执行记录", children: timeline.map((item, i) => <p key={i}>{item.kind} · {item.text}{item.childSessionId && <Button type="link" size="small" onClick={() => void state.selectSession(item.childSessionId!).catch((error: unknown) => message.error(String(error)))}>查看子会话</Button>}</p>) }]} />}
      {state.plan && <PlanCard info={state.plan} applied={state.planApplied} pending={state.planPending} onApprove={() => void state.decidePlan("approve")} onReject={() => void state.decidePlan("reject")} />}
      {approvals.map((a) => <article className="approval-card" key={a.approvalId}>
        <header><b>待审批</b><span>{a.agentId}</span></header><pre>{a.command}</pre>
        {a.sessionId && a.sessionId !== sessionId && <button className="approval-session desk-file-link" onClick={() => void state.selectSession(a.sessionId!).catch((e: unknown) => message.error(String(e)))}>来源会话 · {state.sessions.find((s) => s.id === a.sessionId)?.title || a.sessionId} ↗</button>}
        <div><Button aria-label="批准" loading={deciding.includes(a.approvalId)} onClick={() => void decide(a.approvalId, true)}>批准</Button>
          {!a.command.startsWith("[") && !isDestructiveCommand(a.command) && <><Button disabled={deciding.includes(a.approvalId)} onClick={() => void decide(a.approvalId, true, "session")}>本会话允许</Button><Button disabled={deciding.includes(a.approvalId)} onClick={() => void decide(a.approvalId, true, "allowlist")}>加入白名单</Button></>}
          <Button aria-label="拒绝" disabled={deciding.includes(a.approvalId)} danger onClick={() => void decide(a.approvalId, false)}>拒绝</Button></div>
      </article>)}
      <div ref={scroll.bottomRef} />
    </div>
    {scroll.showBottomBtn && <Button className="desk-scroll-bottom" onClick={() => { scroll.stickRef.current = true; scroll.bottomRef.current?.scrollIntoView(); }}>回到最新消息 ↓</Button>}
    <footer className="desk-composer" id="composer">
      <FileMentionPopup nodes={mentionNodes} loading={mentionLoading} state={mention} idx={mentionIndex} onIdxChange={setMentionIndex} onPick={applyMention} />
      {!!attachments.images.length && <div className="attach-row">{attachments.images.map((src, i) => <div className="attach-thumb" key={`${i}-${src.slice(-20)}`}><img src={src} alt={`附件 ${i + 1}`} /><button aria-label={`移除附件 ${i + 1}`} onClick={() => attachments.removeImage(i)}>×</button></div>)}</div>}
      <Input.TextArea id="input" ref={input} value={text} aria-label="消息内容" placeholder="描述任务，@ 引用文件，或粘贴代码与图片…" autoSize={{ minRows: 2, maxRows: 8 }}
        onChange={(event) => { draft(event.target.value); detectMention(event.target.value, event.target.selectionStart); }}
        onPaste={(event) => { const images = Array.from(event.clipboardData.items).filter((item) => item.type.startsWith("image/")); if (images.length) { event.preventDefault(); images.forEach((item) => { const file = item.getAsFile(); if (file) attachments.addImage(file); }); } }}
        onKeyDown={(event) => {
          if (event.nativeEvent.isComposing || event.keyCode === 229) return;
          if (mention.open) {
            const candidates = mentionCandidates(mentionNodes, mention.query);
            if (event.key === "ArrowDown" || event.key === "ArrowUp") { event.preventDefault(); setMentionIndex((index) => Math.max(0, Math.min(index + (event.key === "ArrowDown" ? 1 : -1), candidates.length - 1))); return; }
            if (event.key === "Enter" || event.key === "Tab") { event.preventDefault(); if (candidates[mentionIndex]) applyMention(candidates[mentionIndex].path); return; }
            if (event.key === "Escape") { event.preventDefault(); setMention((current) => ({ ...current, open: false })); return; }
          }
          if (event.key === "Enter" && !event.shiftKey && !event.repeat) { event.preventDefault(); void send(); }
        }} />
      <div className="desk-composer-tools">
        <Button id="btn-plus" type="text" icon={<PaperClipOutlined />} aria-label="添加图片" onClick={() => pick.current?.click()} />
        <input ref={pick} id="file-pick" type="file" accept="image/*" hidden multiple onChange={(e) => { Array.from(e.target.files ?? []).forEach(attachments.addImage); e.target.value = ""; }} />
        <Select aria-label="对话对象" size="small" disabled={running || sending || changingTarget} value={`${target.mode}:${target.id}`} options={targets} onChange={(value) => void chooseTarget(value)} />
        <Select aria-label="模型" size="small" disabled={running || sending || changingTarget} value={profile} options={profiles} onChange={(model) => void (target.mode === "single" ? api.setAgentModel(target.id, model) : api.setGroupModel(target.id, model)).then(() => setProfile(model)).catch((e: unknown) => message.error(String(e)))} />
        {target.mode === "group" && <Select aria-label="子智能体直聊" size="small" allowClear disabled={running || sending || changingTarget} placeholder="编排执行" value={agent} options={state.agents.map((a) => ({ value: a.identifier, label: a.name }))} onChange={setAgent} />}
        <Select aria-label="执行模式" size="small" value={state.plan?.mode ?? "execute"} options={[{ value: "execute", label: "执行" }, { value: "plan", label: "计划" }]} disabled={running} onChange={(mode) => void state.decidePlan("setMode", mode === "plan")} />
        <span className="desk-grow" />
        {running && <Button id="btn-stop" danger icon={<PauseOutlined />} aria-label="停止" loading={stopping} onClick={() => void stop()} />}
        <Button id="btn-send" type="primary" icon={<ArrowUpOutlined />} aria-label="发送消息" loading={sending} disabled={!text.trim() || running || changingTarget} onClick={() => void send()} />
      </div>
    </footer>
    <Modal open={!!editing} title="编辑并重发" okText="重发" confirmLoading={historyBusy} onCancel={() => setEditing(null)} onOk={() => void edit()}>
      <Input.TextArea aria-label="编辑消息" value={editing?.text ?? ""} autoSize={{ minRows: 4, maxRows: 12 }} onChange={(event) => setEditing((current) => current ? { ...current, text: event.target.value } : current)} />
    </Modal>
  </section>;
}
