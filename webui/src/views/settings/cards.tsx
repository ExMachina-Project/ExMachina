// settings 域文件：统计与偏好卡片（消毒/限额/通知/版本；自 SettingsView.tsx 拆出）。
import React, { useEffect, useState } from "react";
import { Alert, Button, Card, InputNumber, Select, Switch, Tag, message } from "antd";
import { CloudDownloadOutlined } from "@ant-design/icons";
import { api, limitsStats, sanitizeStats, type LimitsStats } from "../../api";
import { loadNotifyPrefs, saveNotifyPrefs, type NotifyPrefs } from "../../store";
import { useT } from "../../i18n/core";

export function SanitizeStatsCard(): React.ReactElement {
  const t = useT();
  const [stats, setStats] = useState<{ pattern: string; count: number }[] | null>(null);
  useEffect(() => {
    void (async () => {
      try {
        const r = await sanitizeStats();
        setStats(r.stats ?? []);
      } catch {
        setStats(null);
      }
    })();
  }, []);
  if (stats === null) return <></>;
  return (
    <Card size="small" className="hud" style={{ marginBottom: 12 }}>
      <div className="dim" style={{ marginBottom: 6 }}>{t("cfg.sanitizeStatsTitle")}</div>
      {stats.length === 0 ? (
        <div className="dim">{t("cfg.sanitizeStatsEmpty")}</div>
      ) : (
        <table className="plain-table">
          <thead><tr><th>{t("cfg.sanitizeStatsPattern")}</th><th>{t("cfg.sanitizeStatsCount")}</th></tr></thead>
          <tbody>
            {stats.map((s) => (
              <tr key={s.pattern}><td className="mono">{s.pattern}</td><td className="mono">{s.count}</td></tr>
            ))}
          </tbody>
        </table>
      )}
    </Card>
  );
}

export function LimitsStatsCard(): React.ReactElement {  const t = useT();
  const [stats, setStats] = useState<LimitsStats | null>(null);
  useEffect(() => {
    void (async () => {
      try { setStats(await limitsStats()); } catch { setStats(null); }
    })();
  }, []);
  if (!stats) return <></>;
  return (
    <Card size="small" className="hud" style={{ marginBottom: 12 }}>
      {stats.enabled ? (
        <>
          <div className="dim" style={{ marginBottom: 6 }}>
            {t("cfg.limitsStatsEnabled", { window: stats.windowSecs, max: stats.maxRequests > 0 ? stats.maxRequests : "∞", period: stats.quotaPeriod })}
          </div>
          {stats.counters.length === 0 ? (
            <div className="dim">{t("cfg.limitsNoCounters")}</div>
          ) : (
            <table className="plain-table">
              <thead><tr><th>{t("cfg.limitsSubject")}</th><th>{t("cfg.limitsCount")}</th></tr></thead>
              <tbody>
                {stats.counters.map((c) => (
                  <tr key={c.key}><td className="mono">{c.key}</td><td className="mono">{c.count}</td></tr>
                ))}
              </tbody>
            </table>
          )}
        </>
      ) : (
        <div className="dim">{t("cfg.limitsDisabled")}</div>
      )}
    </Card>
  );
}

/** 实验运行记录卡（assistant-parity 7.2 实验可观测）：全局事件流聚合，
 *  实验区每开关的最近触发时间与事件摘要；无运行记录明示空态。
 *  事件面复用会话事件溯源端点（id=global = 全局事件流，无新端点）。 */
export function ExperimentRunsCard(): React.ReactElement {
  const t = useT();
  const [rows, setRows] = useState<{ label: string; at: string; summary: string }[] | null>(null);
  useEffect(() => {
    void (async () => {
      try {
        const evts = await api.sessionEvents("global", 500);
        // 实验事件 → 开关映射（与 7.1 落库事件对齐）
        const MAP: Record<string, string> = {
          "super.halt": "superAgent",
          "auto.eval": "autoAgent",
          "auto.act": "autoAgent",
          "meta.proposal": "metaAgent",
          "meta.applied": "metaAgent",
          "memory.digest": "autoMemoryDigest",
        };
        const latest: Record<string, { at: string; summary: string }> = {};
        for (const e of evts) {
          const kind = e.payload && typeof e.payload === "object"
            ? String((e.payload as { type?: string }).type ?? "")
            : String(e.kind ?? "");
          const sw = MAP[kind];
          if (!sw || latest[sw]) continue;
          const p = (e.payload as { payload?: Record<string, unknown> })?.payload ?? {};
          const bits: string[] = [kind];
          for (const k of ["haltKind", "act", "kind", "target", "sessions", "entriesProduced", "changed", "phase"]) {
            if (p[k] !== undefined) bits.push(`${k}=${String(p[k])}`);
          }
          latest[sw] = { at: String(e.createdAt ?? ""), summary: bits.join(" · ").slice(0, 120) };
        }
        const LABEL: Record<string, string> = {
          superAgent: t("cfg.expSuperAgent"),
          autoAgent: t("cfg.expAutoAgent"),
          metaAgent: t("cfg.expMetaAgent"),
          autoMemoryDigest: t("cfg.expAutoMemoryDigest"),
        };
        setRows(
          Object.keys(LABEL).map((sw) => ({
            label: LABEL[sw],
            at: latest[sw]?.at ?? "",
            summary: latest[sw]?.summary ?? "",
          }))
        );
      } catch {
        setRows([]);
      }
    })();
  }, [t]);
  if (rows === null) return <></>;
  return (
    <Card size="small" className="hud" style={{ marginBottom: 12 }}>
      <div className="dim" style={{ marginBottom: 6 }}>{t("cfg.expRunsTitle")}</div>
      <table className="plain-table">
        <thead><tr><th>{t("cfg.expRunsSwitch")}</th><th>{t("cfg.expRunsLast")}</th><th>{t("cfg.expRunsSummary")}</th></tr></thead>
        <tbody>
          {rows.map((r) => (
            <tr key={r.label}>
              <td>{r.label}</td>
              <td className="mono">{r.at ? new Date(r.at).toLocaleString() : "—"}</td>
              <td className="mono">{r.summary || t("cfg.expRunsEmpty")}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </Card>
  );
}

/** 浏览器通知偏好卡（web-notifications）：授权 + 类别开关，本地持久化 */
export function NotifyPrefsCard(): React.ReactElement {
  const t = useT();
  const [prefs, setPrefs] = useState<NotifyPrefs>(() => loadNotifyPrefs());
  const [denied, setDenied] = useState(false);

  useEffect(() => {
    if (typeof Notification !== "undefined" && Notification.permission === "denied") setDenied(true);
  }, []);

  const setEnabled = async (v: boolean) => {
    if (v && typeof Notification !== "undefined" && Notification.permission !== "granted") {
      const perm = await Notification.requestPermission();
      if (perm !== "granted") {
        setDenied(true);
        return;
      }
    }
    setDenied(typeof Notification !== "undefined" && Notification.permission === "denied");
    const next = { ...prefs, enabled: v };
    setPrefs(next);
    saveNotifyPrefs(next);
  };

  const setCat = (key: "approval" | "roundDone" | "cronDone", v: boolean) => {
    const next = { ...prefs, [key]: v };
    setPrefs(next);
    saveNotifyPrefs(next);
  };

  const unsupported = typeof Notification === "undefined";

  return (
    <Card size="small" className="hud" style={{ marginBottom: 12 }}>
      <div className="dim" style={{ marginBottom: 8 }}>{t("notify.desc")}</div>
      {unsupported ? (
        <div className="dim">{t("notify.unsupported")}</div>
      ) : (
        <>
          {denied && <Alert type="warning" showIcon title={t("notify.blocked")} style={{ marginBottom: 8 }} />}
          <div className="cfg2-row">
            <div className="cfg2-label">
              <div className="cfg2-name">{t("notify.enabled")}</div>
              <div className="cfg2-help">{t("notify.enabledHelp")}</div>
            </div>
            <Switch checked={prefs.enabled} onChange={(v) => void setEnabled(v)} />
          </div>
          <div className="cfg2-row">
            <div className="cfg2-name">{t("notify.catApproval")}</div>
            <Switch size="small" disabled={!prefs.enabled} checked={prefs.approval} onChange={(v) => setCat("approval", v)} />
          </div>
          <div className="cfg2-row">
            <div className="cfg2-name">{t("notify.catRound")}</div>
            <Switch size="small" disabled={!prefs.enabled} checked={prefs.roundDone} onChange={(v) => setCat("roundDone", v)} />
          </div>
          <div className="cfg2-row">
            <div className="cfg2-name">{t("notify.catCron")}</div>
            <Switch size="small" disabled={!prefs.enabled} checked={prefs.cronDone} onChange={(v) => setCat("cronDone", v)} />
          </div>
        </>
      )}
    </Card>
  );
}

// ---------------------------------------------------------------- 版本与更新

export function VersionPanel() {
  const t = useT();
  const [info, setInfo] = useState<{ version: string; gitHash: string } | null>(null);
  const [checking, setChecking] = useState(false);
  const [result, setResult] = useState<Awaited<ReturnType<typeof api.versionCheck>> | null>(null);
  const [updatePhase, setUpdatePhase] = useState("");

  useEffect(() => {
    void (async () => {
      try {
        setInfo(await api.versionInfo());
      } catch { /* noop */ }
    })();
  }, []);

  const check = async () => {
    setChecking(true);
    try {
      setResult(await api.versionCheck());
    } catch {
      setResult(null);
    } finally {
      setChecking(false);
    }
  };

  const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

  /** 一键更新:触发 → 轮询脚本日志 → 容器重启失联 → 等恢复并核对 gitHash 变化 */
  const applyUpdate = async () => {
    const before = info?.gitHash ?? "";
    try {
      await api.applyUpdate();
    } catch (e) {
      message.error(String(e));
      return;
    }
    setUpdatingFlag(true);
    let sawRestart = false;
    for (let i = 0; i < 240; i++) {
      await sleep(2500);
      try {
        const st = await api.updateStatus();
        if (st.running) {
          // 阶段优先取状态机 phase(pulling/building/recreating),日志行兜底
          const phaseText =
            st.phase === "pulling"
              ? t("settings.version.phasePulling")
              : st.phase === "building"
                ? t("settings.version.phaseBuilding")
                : st.phase === "recreating"
                  ? t("settings.version.phaseRecreating")
                  : "";
          const lines = st.log.trim().split("\n");
          setUpdatePhase(phaseText || lines[lines.length - 1] || t("settings.version.phaseBuilding"));
          continue;
        }
        if (!sawRestart) {
          // 脚本已退出:phase/exitCode 判定成败(exitCode 非零或日志含失败特征)
          const failed = st.exitCode !== null && st.exitCode !== 0;
          const failLog = /error|失败|fatal|conflict/i.test(st.log);
          setUpdatingFlag(false);
          setUpdatePhase("");
          if (failed || (failLog && !/更新完成/.test(st.log))) {
            message.error(`${t("settings.version.failed")}:${st.log.slice(-300)}`);
          } else {
            message.success(t("settings.version.doneNoRestart"));
          }
          void refreshInfo();
          return;
        }
      } catch {
        // 失联 = 容器重启中
        if (!sawRestart) {
          sawRestart = true;
          setUpdatePhase(t("settings.version.phaseRestarting"));
        }
      }
      // 容器恢复后核对 gitHash
      try {
        const v = await api.versionInfo();
        if (before && v.gitHash && v.gitHash !== before) {
          setUpdatingFlag(false);
          setUpdatePhase("");
          setInfo(v);
          message.success(t("settings.version.success", { hash: v.gitHash }));
          return;
        }
      } catch { /* 仍在重启 */ }
    }
    setUpdatingFlag(false);
    setUpdatePhase("");
    message.warning(t("settings.version.timeout"));
  };

  const refreshInfo = async () => {
    try {
      setInfo(await api.versionInfo());
    } catch { /* noop */ }
  };
  const [updatingFlag, setUpdatingFlag] = useState(false);

  return (
    <>
      <header className="cfg-panel-head">
        <h3>{t("sec.version")}</h3>
        <p>{t("sec.version.desc")}。</p>
      </header>
      <div className="cfg-list">
        <div className="cfg2-row">
          <div className="cfg2-label">
            <div className="cfg2-name">{t("settings.version.current")}</div>
            <div className="cfg2-help">{t("settings.version.currentHelp")}</div>
          </div>
          <div className="cfg2-ctrl mono">
            v{info?.version ?? "…"} · {info?.gitHash ?? "…"}
            {result?.deployMode && (
              <Tag
                color={result.deployMode === "git" ? "blue" : result.deployMode === "image" ? "cyan" : "default"}
                style={{ marginLeft: 8 }}
              >
                {t(`settings.version.mode.${result.deployMode}`)}
              </Tag>
            )}
          </div>
        </div>
        <div className="cfg2-row">
          <div className="cfg2-label">
            <div className="cfg2-name">{t("settings.version.check")}</div>
            <div className="cfg2-help">{t("settings.version.checkHelp")}</div>
          </div>
          <div className="cfg2-ctrl">
            <Button size="small" loading={checking} onClick={() => void check()}>
              {t("settings.version.checkBtn")}
            </Button>
          </div>
        </div>
        <div className="cfg2-row">
          <div className="cfg2-label">
            <div className="cfg2-name">{t("settings.version.apply")}</div>
            <div className="cfg2-help">
              {result?.deployMode === "image"
                ? t("settings.version.applyHelpImage")
                : result?.deployMode === "git"
                  ? t("settings.version.applyHelpGit")
                  : t("settings.version.applyHelp")}
            </div>
          </div>
          <div className="cfg2-ctrl">
            {updatePhase ? (
              <span className="dim">{updatePhase}</span>
            ) : (
              <Button size="small" type="primary" danger loading={updatingFlag} onClick={() => void applyUpdate()}>
                {t("settings.version.applyBtn")}
              </Button>
            )}
          </div>
        </div>
        {result && (
          <div className="cfg2-row">
            <div className="cfg2-label">
              <div className="cfg2-name">
                {result.updateAvailable === true && <Tag color="warning">{t("settings.version.available")}</Tag>}
                {result.updateAvailable === false && <Tag color="success">{t("settings.version.uptodate")}</Tag>}
                {result.updateAvailable == null && <Tag>{t("settings.version.unknown")}</Tag>}
              </div>
              <div className="cfg2-help">
                {result.updateAvailable === true && result.upstream && (
                  <>
                    {t("settings.version.latest")}: {result.upstream.ref} @ {result.upstream.short} · {result.upstream.date} ·{" "}
                    <a href={result.upstream.url} target="_blank" rel="noreferrer">
                      {t("settings.version.viewCommit")}
                    </a>
                  </>
                )}
                {result.updateAvailable === false && t("settings.version.uptodateHelp")}
                {result.updateAvailable == null && <>{result.note ? `${t("settings.version.unknownHelp")}(${result.note})` : t("settings.version.unknownHelp")}</>}
              </div>
            </div>
          </div>
        )}
      </div>
      <div className="cfg2-foot">
        <span className="dim">{t("settings.version.guide")}</span>
      </div>
    </>
  );
}
