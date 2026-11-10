/** 全局会话速查（Ctrl/Cmd+K，session-search）：
 *  任意视图唤起，检索会话标题与最近 200 条消息；↑/↓ 选择、Enter 打开并定位、
 *  Esc 关闭。与管理页共用 useSessionSearch / Highlight / requestNav 定位链路。 */
import React, { useEffect, useRef, useState } from "react";
import { Input, Modal, Spin } from "antd";
import type { InputRef } from "antd";
import { SearchOutlined } from "@ant-design/icons";
import { useExm } from "../store";
import { useT } from "../i18n/core";
import { relativeTime } from "../time";
import { Highlight, useSessionSearch } from "./sessionSearch";

export function CommandPalette(): React.ReactElement {
  const t = useT();
  const [open, setOpen] = useState(false);
  const [sel, setSel] = useState(0);
  const search = useSessionSearch({ limit: 12 });
  const selectSession = useExm((st) => st.selectSession);
  const requestNav = useExm((st) => st.requestNav);
  const inputRef = useRef<InputRef>(null);

  // 全局快捷键：Ctrl/Cmd+K（桌面壳/部分浏览器）+「/」（浏览器保留 Ctrl+K 为地址栏搜索，
  // 页面无法拦截时的兜底）；在输入框内不触发「/」，避免打断正常输入。
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") {
        e.preventDefault();
        setOpen((v) => !v);
        return;
      }
      const target = e.target as HTMLElement | null;
      const typing =
        !!target &&
        (target.tagName === "INPUT" || target.tagName === "TEXTAREA" || target.isContentEditable);
      if (!typing && e.key === "/" && !e.metaKey && !e.ctrlKey && !e.altKey) {
        e.preventDefault();
        setOpen(true);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  const close = () => {
    setOpen(false);
    setSel(0);
    search.clear();
  };

  // 打开后聚焦输入框（Modal 动画结束后再聚焦，避免被遮罩抢走）
  useEffect(() => {
    if (!open) return;
    const id = setTimeout(() => inputRef.current?.focus(), 60);
    return () => clearTimeout(id);
  }, [open]);

  // 结果变化/关键字变化时回到首项
  useEffect(() => {
    setSel(0);
  }, [search.hits]);

  const hits = search.hits ?? [];
  const openHit = (idx: number) => {
    const h = hits[idx];
    if (!h) return;
    void selectSession(h.sessionId);
    requestNav("chat", h.sessionId, h.snippets[0]?.messageId);
    close();
  };

  return (
    <Modal
      open={open}
      onCancel={close}
      footer={null}
      width={560}
      destroyOnHidden
      className="cmd-palette"
      styles={{ body: { padding: 0 } }}
    >
      <div className="cmd-head">
        <SearchOutlined className="cmd-icon" />
        <Input
          ref={inputRef}
          variant="borderless"
          placeholder={t("palette.placeholder")}
          aria-label={t("palette.title")}
          value={search.query}
          onChange={(e) => search.setQuery(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "ArrowDown") {
              e.preventDefault();
              setSel((i) => Math.min(Math.max(0, hits.length - 1), i + 1));
            } else if (e.key === "ArrowUp") {
              e.preventDefault();
              setSel((i) => Math.max(0, i - 1));
            } else if (e.key === "Enter" && hits.length > 0) {
              e.preventDefault();
              openHit(sel);
            } else if (e.key === "Escape") {
              e.preventDefault();
              close();
            }
          }}
        />
        <span className="cmd-scope mono">{t("palette.scope")}</span>
      </div>
      <div className="cmd-body">
        {search.loading && hits.length === 0 ? (
          <div className="cmd-status"><Spin size="small" /> {t("palette.loading")}</div>
        ) : search.error ? (
          <div className="cmd-status">{t("palette.error", { err: search.error })}</div>
        ) : !search.query.trim() ? (
          <div className="cmd-status">{t("palette.hint")}</div>
        ) : hits.length === 0 ? (
          <div className="cmd-status">{t("palette.empty")}</div>
        ) : (
          hits.map((h, i) => (
            <button
              key={h.sessionId}
              type="button"
              className={`cmd-item ${i === sel ? "on" : ""}`}
              onMouseEnter={() => setSel(i)}
              onClick={() => openHit(i)}
            >
              <span className="cmd-item-title">
                <Highlight text={h.title} query={search.query} />
              </span>
              <span className="cmd-item-meta mono">
                {h.messageMatches > 0
                  ? t("sessions.searchHitCount", { n: h.messageMatches })
                  : t("sessions.searchTitleHit")}
                {" · "}
                {relativeTime(h.lastActiveAt)}
              </span>
              {h.snippets[0] && (
                <span className="cmd-item-snippet">
                  <Highlight text={h.snippets[0].text} query={search.query} />
                </span>
              )}
            </button>
          ))
        )}
      </div>
      <div className="cmd-foot mono">{t("palette.hint")}</div>
    </Modal>
  );
}

export default CommandPalette;
