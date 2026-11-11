// store 域文件：WS 连接层（代际/退避/心跳/断连补偿；自 store.ts 拆出）。
import { api, serverBase } from "../api";
import type { WsEvent } from "./index";
import type { ExmState } from "./index";
import { useExm } from "./sessions";


let ws: WebSocket | null = null;
/** 连接代际号：每次 connectWs 递增。旧代际残留的 onclose/定时器一律失效，根治 A→B→A 快速切换的闭包竞态 */
let wsGen = 0;
/** 会话选择代际号：每次 selectSession 递增。快速切换时旧响应乱序到达不得覆盖新状态 */
let selectSeq = 0;
export function getSelectSeq(): number { return selectSeq; }
export function bumpSelectSeq(): number { return ++selectSeq; }
/** 连续失败退避（2s → 5s → 10s → 30s 封顶）：成功 open 即归零 */
let wsBackoff = 0;
/** 心跳看门狗：超过该时长未收到任何帧（含协议 Pong）即判死重连 */
const WS_STALE_MS = 45000;
let wsLastFrame = 0;
let wsWatch: ReturnType<typeof setInterval> | null = null;
/** 应用层心跳定时器句柄：与连接代际同生命周期，onclose 清理防叠加 */
let wsHeartbeat: ReturnType<typeof setInterval> | null = null;

export function connectWs(get: () => ExmState): void {
  const sessionId = get().sessionId;
  if (!sessionId) return;
  const gen = ++wsGen;
  // 主动关闭旧连接：其 onclose 携带旧 gen，不会触发重连（也不会清心跳——故在此先清）
  if (wsHeartbeat !== null) {
    clearInterval(wsHeartbeat);
    wsHeartbeat = null;
  }
  ws?.close();
  const base = serverBase();
  const proto = base.startsWith("https") || (base === "" && location.protocol === "https:") ? "wss" : "ws";
  const host = base ? base.replace(/^https?:\/\//, "") : location.host;
  ws = new WebSocket(`${proto}://${host}/ws?sessionId=${sessionId}&key=${encodeURIComponent(localStorage.getItem("exm.key") ?? "")}`);
  let reconnected = false; // 本代际是否经历过断连重连（成功后补偿拉取丢失的事件）
  wsLastFrame = Date.now();
  ws.onopen = () => {
    if (gen !== wsGen) return;
    wsBackoff = 0; // 连上即归零退避
    get().setWs(true);
    if (!reconnected) return;
    // 断连补偿：重拉消息与编排图恢复丢失事件；图无在途节点则解除 running 卡死
    const compSeq = selectSeq; // 补偿期间的选择切换同样不得被旧快照覆盖
    void (async () => {
      try {
        const [messages, graph] = await Promise.all([api.messages(sessionId), api.graph(sessionId)]);
        if (gen !== wsGen || useExm.getState().sessionId !== sessionId || selectSeq !== compSeq) return;
        const pending = (graph?.nodes ?? []).some((n) =>
          ["running", "dispatched", "syncing"].includes(String((n as { status?: string }).status ?? "")),
        );
        useExm.setState({ messages, graph: (graph?.nodes?.length ?? 0) > 0 ? graph : null, running: pending });
      } catch {
        /* 补偿失败静默：下轮断连重连时再次补偿 */
      }
      // 断连窗内产生的审批单补偿：pending 表全量对照（跨会话口径与直推/恢复一致）
      try {
        const live = await api.listApprovals("pending", 100);
        if (gen !== wsGen || useExm.getState().sessionId !== sessionId || selectSeq !== compSeq) return;
        useExm.setState((s) => ({
          approvals: [
            ...s.approvals,
            ...live
              .map((x) => ({ approvalId: x.id, agentId: x.agentId, command: x.command, sessionId: x.sessionId }))
              .filter((c) => !s.approvals.some((a) => a.approvalId === c.approvalId)),
          ],
        }));
      } catch { /* 审批补偿失败静默 */ }
    })();
  };
  ws.onclose = () => {
    if (gen !== wsGen) return; // 旧代际：静默退出，不碰当前连接、不重连
    if (wsHeartbeat !== null) {
      clearInterval(wsHeartbeat);
      wsHeartbeat = null;
    }
    get().setWs(false);
    reconnected = true;
    const delay = [2000, 5000, 10000, 30000][Math.min(wsBackoff, 3)];
    wsBackoff += 1;
    setTimeout(() => {
      if (gen !== wsGen || get().sessionId !== sessionId) return;
      connectWs(get);
    }, delay);
  };
  ws.onmessage = (m) => {
    wsLastFrame = Date.now();
    try {
      get().handleEvent(JSON.parse(String(m.data)) as WsEvent);
    } catch {
      /* 忽略坏帧 */
    }
  };
  // 应用层心跳：每 20s 发一条 ping 文本帧并把发送成功计为「活」。
  // 根因：服务端 25s 协议层 Ping 的 Pong 由浏览器自动回但不触发 onmessage，
  // 看门狗以收帧判活——空闲会话唯一流量是协议帧，45s 必误判链路死强拆重连
  // （「网关断连」弹条周期性出现）。发送成功即链路可用（缓冲满会抛异常），
  // 收方向由服务端事件流双保险。服务端对入站文本帧忽略（_ => {}），无副作用。
  wsHeartbeat = setInterval(() => {
    const cur = ws;
    if (!cur || cur.readyState !== WebSocket.OPEN) return;
    try {
      cur.send("ping");
      wsLastFrame = Date.now();
    } catch {
      /* 发送失败：缓冲满≈链路将死，交给看门狗 */
    }
  }, 20000);
  // 心跳看门狗（单例）：空闲超时强制重连（浏览器对协议 Pong 不暴露事件，以「收帧时刻」为准——
  // 服务端 25s 一跳且事件流通常活跃，45s 静默基本等于链路已死）
  if (wsWatch === null) {
    wsWatch = setInterval(() => {
      const cur = ws;
      if (!cur || cur.readyState !== WebSocket.OPEN) return;
      if (Date.now() - wsLastFrame > WS_STALE_MS) {
        cur.close(); // 触发 onclose → 指数退避重连
      }
    }, 10000);
  }
}

// 网络恢复 / 页面回前台：立即重连（不等退避计时器）
export function reconnectNow(): void {
  const cur = ws;
  if (cur && cur.readyState === WebSocket.OPEN) return;
  wsBackoff = 0;
  cur?.close(); // 触发 onclose → 立即档退避
  // onclose 携带旧代际时不会自愈（例如从未连上过），此处直接补一跳
  setTimeout(() => {
    if (!ws || ws.readyState === WebSocket.CLOSED) wsBackoff = 0;
  }, 0);
}

if (typeof window !== "undefined") {
  window.addEventListener("online", reconnectNow);
  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState === "visible") reconnectNow();
  });
}

