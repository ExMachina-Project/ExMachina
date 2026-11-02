/** 会话检索共用件（session-search）：防抖检索 hook + 关键词高亮。
 *
 *  管理页与全局速查共用同一链路：
 *  - 输入停止 250ms 后才发请求（空串不发）；
 *  - 请求序号守卫：快速输入时丢弃过期响应，避免旧结果覆盖新结果；
 *  - 高亮只用 React 文本节点切分（不做 HTML 注入）；Unicode 小写展开导致
 *    长度不一致时整段按纯文本渲染，避免下标错位。
 */
import React, { useEffect, useRef, useState } from "react";
import { api } from "../api";
import type { SessionSearchHit } from "../types";

export interface UseSessionSearchOptions {
  /** 关闭时不发请求（如归档视图走本地过滤） */
  enabled?: boolean;
  /** 结果会话上限（管理页 20、速查 12） */
  limit?: number;
  /** 防抖毫秒（默认 250） */
  debounceMs?: number;
}

export interface SessionSearchState {
  query: string;
  setQuery: (q: string) => void;
  hits: SessionSearchHit[] | null;
  loading: boolean;
  error: string | null;
  clear: () => void;
}

export function useSessionSearch(opts: UseSessionSearchOptions = {}): SessionSearchState {
  const enabled = opts.enabled ?? true;
  const limit = opts.limit ?? 20;
  const debounceMs = opts.debounceMs ?? 250;
  const [query, setQuery] = useState("");
  const [hits, setHits] = useState<SessionSearchHit[] | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const seq = useRef(0);

  useEffect(() => {
    const q = query.trim();
    seq.current += 1;
    const id = seq.current;
    if (!enabled || !q) {
      setHits(null);
      setLoading(false);
      setError(null);
      return;
    }
    setLoading(true);
    const timer = setTimeout(async () => {
      try {
        const r = await api.searchSessions(q, limit);
        if (seq.current === id) {
          setHits(r.hits);
          setError(null);
        }
      } catch (e) {
        if (seq.current === id) {
          setHits(null);
          setError(String(e));
        }
      } finally {
        if (seq.current === id) setLoading(false);
      }
    }, debounceMs);
    return () => clearTimeout(timer);
  }, [query, enabled, limit, debounceMs]);

  return {
    query,
    setQuery,
    hits,
    loading,
    error,
    clear: () => setQuery(""),
  };
}

/** 关键词高亮：大小写不敏感、保留原文大小写；非等长（Unicode 小写展开）退化为纯文本。 */
export function Highlight({ text, query }: { text: string; query: string }): React.ReactElement {
  const q = query.trim();
  if (!q || !text) return <>{text}</>;
  const lower = text.toLowerCase();
  const qLower = q.toLowerCase();
  if (lower.length !== text.length || qLower.length === 0) return <>{text}</>;
  const parts: React.ReactNode[] = [];
  let from = 0;
  let key = 0;
  for (;;) {
    const at = lower.indexOf(qLower, from);
    if (at < 0) {
      if (from < text.length) parts.push(text.slice(from));
      break;
    }
    if (at > from) parts.push(text.slice(from, at));
    parts.push(
      <mark key={key++} className="search-hit">
        {text.slice(at, at + qLower.length)}
      </mark>,
    );
    from = at + qLower.length;
  }
  return <>{parts}</>;
}
