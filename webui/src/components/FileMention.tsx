/**
 * 输入框 `@` 文件提及（对话即编码，spec: chat-as-coding）：
 * 在 composer 输入 `@` 触发文件树搜索弹层，选中后把 `路径` 插入消息文本。
 * 复用 workbench 树端点（.gitignore 口径 + 工作区边界校验），无独立编码页面。
 */
import React, { useEffect, useMemo, useRef } from "react";
import { FileOutlined, FolderOutlined } from "@ant-design/icons";
import type { WorkbenchNode } from "../api";
import { useT } from "../i18n/core";

export interface MentionState {
  open: boolean;
  /** 触发词（@ 之后的过滤文本） */
  query: string;
  /** @ 在文本中的起始位置（替换用） */
  start: number;
}

/** 扁平节点 → 提及候选（文件优先、目录其次，按 query 子串过滤） */
export function mentionCandidates(
  nodes: WorkbenchNode[],
  query: string,
  limit = 12,
): WorkbenchNode[] {
  const q = query.trim().toLowerCase();
  const pool = nodes.filter((n) => n.type === "file" || n.type === "dir");
  const hit = q
    ? pool.filter((n) => n.path.toLowerCase().includes(q))
    : pool;
  // 文件在前、路径短在前（顶层文件最可能被引用）
  return hit
    .sort((a, b) => {
      if (a.type !== b.type) return a.type === "file" ? -1 : 1;
      return a.path.length - b.path.length;
    })
    .slice(0, limit);
}

/** 文件提及弹层：候选列表展示与点击选中（键盘导航由宿主 textarea 的 onKeyDown 驱动，
 *  通过受控 idx/onIdxChange 接管 ↑↓/Enter/Tab/Esc——弹层自身不可聚焦） */
export function FileMentionPopup({
  nodes,
  loading,
  state,
  idx,
  onIdxChange,
  onPick,
}: {
  nodes: WorkbenchNode[];
  loading: boolean;
  state: MentionState;
  idx: number;
  onIdxChange: (i: number) => void;
  onPick: (path: string) => void;
}): React.ReactElement | null {
  const t = useT();
  const listRef = useRef<HTMLDivElement>(null);
  const cands = useMemo(() => mentionCandidates(nodes, state.query), [nodes, state.query]);

  useEffect(() => {
    const el = listRef.current?.children[idx] as HTMLElement | undefined;
    el?.scrollIntoView({ block: "nearest" });
  }, [idx]);

  useEffect(() => {
    onIdxChange(0);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [state.query]);

  if (!state.open) return null;

  return (
    <div className="mention-pop" role="listbox">
      <div className="mention-pop-head">{t("chat.mentionTitle")}</div>
      <div className="mention-pop-list" ref={listRef}>
        {loading && <div className="mention-pop-item dim">…</div>}
        {!loading && cands.length === 0 && (
          <div className="mention-pop-item dim">{t("chat.mentionEmpty")}</div>
        )}
        {cands.map((c, i) => (
          <div
            key={c.path}
            className={`mention-pop-item ${i === idx ? "on" : ""}`}
            onMouseEnter={() => onIdxChange(i)}
            onMouseDown={(e) => {
              e.preventDefault(); // 不抢 textarea 焦点
              onPick(c.path);
            }}
          >
            {c.type === "dir" ? <FolderOutlined /> : <FileOutlined />}
            <span className="mono">{c.path}</span>
          </div>
        ))}
      </div>
      <div className="mention-pop-foot dim">{t("chat.mentionHint")}</div>
    </div>
  );
}
