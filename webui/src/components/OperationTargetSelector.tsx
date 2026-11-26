import { useEffect, useMemo, useRef, useState, type ReactElement } from "react";
import { Button, Input, Popover, Spin } from "antd";
import { DownOutlined, ReloadOutlined, SearchOutlined, TeamOutlined, UserOutlined } from "@ant-design/icons";
import { targetAvailability, targetKey, type OperationTarget } from "../../../client/src/operationContext";
import { operationContext, refreshOperationDirectory, suggestBoundTarget, useOperationContext } from "../operationContext";
import { useExm } from "../store";
import { useT } from "../i18n/core";
import { useIsNarrow } from "../useIsNarrow";

export function OperationTargetSelector(): ReactElement {
  const t = useT();
  const narrow = useIsNarrow();
  const state = useOperationContext((value) => value);
  const binding = useExm((value) => value.selectedSession?.groupId);
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [cursor, setCursor] = useState(0);
  const trigger = useRef<HTMLButtonElement>(null);
  const search = useRef<import("antd").InputRef>(null);
  const availability = targetAvailability(state);
  useEffect(() => { void refreshOperationDirectory(); }, []);
  useEffect(() => { suggestBoundTarget(); }, [binding, state.status]);
  const options = useMemo(() => state.directory.filter((item) =>
    `${item.name} ${item.id} ${t(item.kind === "single" ? "operation.single" : "operation.group")}`.toLocaleLowerCase().includes(query.trim().toLocaleLowerCase())), [state.directory, query, t]);
  const close = () => { setOpen(false); requestAnimationFrame(() => trigger.current?.focus()); };
  const select = (target: OperationTarget | null) => { if (operationContext.getState().select(target)) close(); };
  const content = <div className="operation-popover" role="dialog" aria-label={t("operation.label")} onKeyDown={(event) => {
    if (event.key === "Escape") { event.preventDefault(); event.stopPropagation(); close(); }
  }}>
    <p className="operation-help">{t("operation.help")}</p>
    <Input ref={search} prefix={<SearchOutlined />} value={query} aria-label={t("operation.search")}
      onChange={(event) => { setQuery(event.target.value); setCursor(0); }}
      onKeyDown={(event) => {
        if (event.key === "ArrowDown" || event.key === "ArrowUp") {
          event.preventDefault(); setCursor((index) => Math.max(0, Math.min(options.length - 1, index + (event.key === "ArrowDown" ? 1 : -1))));
        } else if (event.key === "Enter" && options[cursor] && state.status === "ready") {
          event.preventDefault(); select(options[cursor]);
        }
      }} aria-controls="operation-options" aria-activedescendant={options[cursor] ? `operation-option-${cursor}` : undefined} role="combobox" aria-expanded={open} />
    <div className="operation-directory-state" aria-live="polite">
      {state.status === "loading" && <><Spin size="small" /> {t("operation.loading")}</>}
      {state.status === "error" && <span role="alert">{t("operation.failed", { error: state.error ?? "" })}</span>}
      {availability === "unavailable" && <span role="alert">{t("operation.unavailable")}</span>}
    </div>
    <div id="operation-options" className="operation-options" role="listbox" aria-label={t("operation.label")}>
      {options.map((item, index) => <button type="button" role="option" id={`operation-option-${index}`} key={targetKey(item)} data-target-key={targetKey(item)}
        className={`operation-option ${index === cursor ? "focused" : ""}`} disabled={state.status !== "ready"}
        aria-selected={state.target ? targetKey(state.target) === targetKey(item) : false} onClick={() => select(item)}>
        {item.kind === "single" ? <UserOutlined /> : <TeamOutlined />}
        <span><strong>{item.name}</strong><small>{t(item.kind === "single" ? "operation.single" : "operation.group")} · {item.id}</small></span>
      </button>)}
      {state.status === "ready" && options.length === 0 && <p role="status">{t(query ? "operation.noMatch" : "operation.empty")}</p>}
    </div>
    <div className="operation-actions">
      <Button type="text" size="small" onClick={() => select(null)}>{t("operation.none")}</Button>
      <Button size="small" aria-label={t("operation.refresh")} icon={<ReloadOutlined />} loading={state.status === "loading"} onClick={() => void refreshOperationDirectory()}>{t("operation.refresh")}</Button>
    </div>
  </div>;
  return <div className="operation-selector" data-availability={availability}>
    <Popover trigger="click" placement={narrow ? "bottomLeft" : "rightTop"} open={open} content={content} onOpenChange={(next) => {
      setOpen(next);
      if (next) { setQuery(""); setCursor(0); void refreshOperationDirectory(); }
    }} afterOpenChange={(next) => { if (next) search.current?.focus(); }}>
      <button ref={trigger} type="button" className="operation-target-trigger" aria-haspopup="dialog" aria-expanded={open}
        aria-label={`${t("operation.label")}：${state.target?.name ?? t("operation.none")}`} title={state.target ? `${state.target.name} · ${state.target.kind}:${state.target.id}` : t("operation.help")}>
        {state.target?.kind === "group" ? <TeamOutlined /> : <UserOutlined />}
        <span><small>{t("operation.label")}</small><strong>{state.target?.name ?? t("operation.none")}</strong></span>
        {state.status === "loading" ? <Spin size="small" /> : <DownOutlined />}
      </button>
    </Popover>
  </div>;
}

/** Closed mobile navigation still exposes the default and returns focus to the same picker. */
export function OperationTargetMobileButton({ openNavigation }: { openNavigation: () => void }): ReactElement {
  const t = useT();
  const target = useOperationContext((state) => state.target);
  return <button type="button" className="mobile-operation-target" aria-label={`${t("operation.label")}：${target?.name ?? t("operation.none")}`}
    title={target?.name ?? t("operation.none")} onClick={() => {
      openNavigation();
      requestAnimationFrame(() => document.querySelector<HTMLButtonElement>(".operation-target-trigger")?.click());
    }}><UserOutlined /><span>{target?.name ?? t("operation.none")}</span></button>;
}
