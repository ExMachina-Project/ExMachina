//! hooks/useChatStream.ts —— 聊天流跟随（codebase-refactor 7.2 拆分）：
//! 贴底跟随（流式增量不抢滚动条）、新消息强制跟随、审批卡抢视、
//! token 用量刷新。

import { useEffect, useRef, useState } from "react";
import { api } from "../api";
import type { ChatMessage } from "../types";

/** 聊天流跟随：返回（底部锚 ref、贴底 ref、是否显示回底按钮、token 用量）。 */
export function useChatStream(
  sessionId: string | undefined,
  messages: ChatMessage[],
  running: boolean,
  approvals: { approvalId: string; sessionId?: string | null }[],
  streamKey: unknown,
) {
  const stickRef = useRef(true);
  const bottomRef = useRef<HTMLDivElement>(null);
  const [showBottomBtn, setShowBottomBtn] = useState(false);
  const [usage, setUsage] = useState<{
    estimate: number;
    budget: number;
    unlimited: boolean;
    /** provider 上报的真实用量可用（否则为字符估算） */
    measured?: boolean;
  } | null>(null);
  // Only a user scroll away from the bottom disables streaming follow.
  useEffect(() => {
    const el = document.querySelector(".chat-scroll");
    if (!el) return;
    const onScroll = () => {
      const atBottom = el.scrollHeight - el.scrollTop - el.clientHeight <= 80;
      stickRef.current = atBottom;
      // 用户主动回底 → 恢复跟随（按钮点击滚到底同样触发此路径）
      setShowBottomBtn(!atBottom);
    };
    el.addEventListener("scroll", onScroll, { passive: true });
    return () => el.removeEventListener("scroll", onScroll);
  }, [sessionId]);
  useEffect(() => {
    // 新消息（非流式增量）：重置跟随（新轮次开始视为用户关注最新）
    stickRef.current = true;
    if (messages.length === 0 && !running) {
      const scroll = bottomRef.current?.closest(".chat-scroll");
      if (scroll) scroll.scrollTop = 0;
    } else {
      bottomRef.current?.scrollIntoView({ behavior: "smooth" });
    }
    setShowBottomBtn(false);
  }, [sessionId, messages.length, running]);
  // 流式增量（live/token/timeline）：仅贴底时跟随，不抢滚动条
  useEffect(() => {
    if (messages.length === 0 && !running) return;
    if (!stickRef.current) return;
    bottomRef.current?.scrollIntoView({ behavior: "auto" });
  }, [messages, streamKey, running]);
  // 审批卡到达：强制拉到卡位（审批是阻塞等待，时效性强——
  // 用户上翻回看时也该被打断，错过 = 工具在等待窗内一直挂着）。
  // 只对本会话单滚动（approval-session-scope 同口径）：跨会话单不弹卡也不抢视线。
  const ownApprovals = approvals.filter((a) => !a.sessionId || a.sessionId === sessionId).map((a) => a.approvalId).join("\0");
  useEffect(() => {
    if (!ownApprovals) return;
    stickRef.current = true;
    bottomRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [ownApprovals, sessionId]);

  useEffect(() => {
    let active = true;
    setUsage(null);
    if (!sessionId) return;
    void (async () => {
      try {
        const next = await api.sessionTokens(sessionId);
        if (active) setUsage(next);
      } catch {
        // 用量接口不可用时静默
      }
    })();
    return () => { active = false; };
  }, [sessionId, messages.length, running]);

  return { bottomRef, stickRef, showBottomBtn, usage };
}
