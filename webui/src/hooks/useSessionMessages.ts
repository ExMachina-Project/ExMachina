import { useEffect, useRef, useState } from "react";
import { api } from "../api";
import type { ChatMessage } from "../types";

/** Read-only session views share one request generation for initial and event refreshes. */
export function useSessionMessages(sessionId?: string, refreshIntervalMs?: number) {
  const [messages, setMessages] = useState<ChatMessage[]>([]);
  const [loading, setLoading] = useState(false);
  const generation = useRef(0);
  const visible = useRef(document.visibilityState === "visible");
  useEffect(() => {
    let disposed = false;
    setMessages([]);
    setLoading(Boolean(sessionId));
    const load = async () => {
      if (!sessionId || !visible.current) return;
      const request = ++generation.current;
      try {
        const next = await api.messages(sessionId);
        if (!disposed && request === generation.current) setMessages(next);
      } catch {
        // A failed refresh preserves the last successful history.
      } finally {
        if (!disposed && request === generation.current) setLoading(false);
      }
    };
    void load();
    const refresh = (event: Event) => {
      const id = (event as CustomEvent<string | undefined>).detail;
      if (!id || id === sessionId) void load();
    };
    window.addEventListener("exm:bound-refresh", refresh);
    const onVisibility = () => { visible.current = document.visibilityState === "visible"; if (visible.current) void load(); };
    document.addEventListener("visibilitychange", onVisibility);
    const timer = refreshIntervalMs ? window.setInterval(() => { if (visible.current) void load(); }, refreshIntervalMs) : undefined;
    return () => {
      disposed = true;
      generation.current += 1;
      if (timer) window.clearInterval(timer);
      window.removeEventListener("exm:bound-refresh", refresh);
      document.removeEventListener("visibilitychange", onVisibility);
    };
  }, [sessionId, refreshIntervalMs]);
  return { messages, loading };
}
