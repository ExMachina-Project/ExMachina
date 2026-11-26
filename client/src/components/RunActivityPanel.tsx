import { useState } from "react";
import type React from "react";
import { Button, Spin, Tag } from "antd";
import { useClientServices } from "../services";
import type { RunProgress, TerminalJobSnapshot } from "../types";
import type { SpawnBgItem } from "../api";

export function RunActivityPanel({
  running,
  progress,
  terminalJobs,
  spawns,
  onClearSpawns,
}: {
  running: boolean;
  progress: RunProgress | null;
  terminalJobs: TerminalJobSnapshot[];
  spawns: SpawnBgItem[];
  onClearSpawns?: () => void;
}): React.ReactElement | null {
  const { t } = useClientServices();
  const [expanded, setExpanded] = useState<string[]>([]);
  if (!running && !progress && terminalJobs.length === 0 && spawns.length === 0) return null;
  const toggle = (id: string) => setExpanded((current) => current.includes(id) ? current.filter((item) => item !== id) : [...current, id]);
  const statusKey = (status: TerminalJobSnapshot["status"]) => `terminalJobs.status.${status}` as never;
  const shouldShowProgressMessage = progress && (progress.stage === "planning" || progress.stage === "working" || progress.stage === "model" || progress.announce === true);
  const progressLabel = progress?.stage === "preparing" && progress.announce !== true
    ? t("chat.executing" as never)
    : progress ? t(`runProgress.${progress.stage}` as never) : t("chat.executing" as never);
  const progressMessage = progress?.stage === "planning"
    ? t("runProgress.planning" as never)
    : shouldShowProgressMessage
      ? progress?.message || t("chat.executing" as never)
      : t("chat.executing" as never);

  return <section className="run-activity-panel" aria-live="polite">
    {running && <div className="run-progress-row">
      <Spin size="small" />
      <Tag color="processing">{progressLabel ?? t("chat.executing" as never)}</Tag>
      {progress?.agentId && <span className="mono run-progress-agent">{progress.agentId}</span>}
      <span className="run-progress-message">{progressMessage}</span>
    </div>}
    {terminalJobs.length > 0 && <div className="terminal-jobs-list" aria-label={t("terminalJobs.title" as never)}>
      {terminalJobs.map((job) => <article key={job.taskId} className={`terminal-job terminal-job-${job.status}`}>
        <button type="button" className="terminal-job-head" onClick={() => toggle(job.taskId)} aria-expanded={expanded.includes(job.taskId)}>
          <span className="terminal-job-indicator">{job.status === "running" ? <Spin size="small" /> : job.status === "exited" && job.exitCode === 0 ? "✓" : "●"}</span>
          <code className="terminal-job-command">{job.command || job.taskId}</code>
          <Tag color={job.status === "running" ? "processing" : job.status === "exited" && job.exitCode === 0 ? "success" : "default"}>{t(statusKey(job.status))}</Tag>
          <span className="terminal-job-size">{job.outputBytes} B{job.outputTruncated ? " · …" : ""}</span>
        </button>
        {expanded.includes(job.taskId) && <div className="terminal-job-detail">
          {job.outputPreview && <pre>{job.outputPreview}</pre>}
          {job.error && <p role="alert">{job.error}</p>}
          {typeof job.exitCode === "number" && <small>{t("terminalJobs.exitCode" as never)}: {job.exitCode}</small>}
        </div>}
      </article>)}
    </div>}
    {spawns.length > 0 && <div className="spawns-strip">
      {spawns.map((spawn) => <article key={spawn.spawnId} className={`spawn-card spawn-${spawn.status}`}>
        <button type="button" className="spawn-card-head" onClick={() => toggle(spawn.spawnId)} aria-expanded={expanded.includes(spawn.spawnId)}>
          <span className="spawn-status-icon">{spawn.status === "running" ? <Spin size="small" /> : spawn.status === "done" ? "✅" : spawn.status === "timeout" ? "⏱" : "❌"}</span>
          <span className="spawn-preview">{spawn.preview || spawn.spawnId}</span>
          {spawn.status !== "running" && spawn.resultPreview && <span className="spawn-expand-hint">{expanded.includes(spawn.spawnId) ? t("spawn.foldLess" as never) : t("spawn.foldMore" as never)}</span>}
        </button>
        {spawn.status !== "running" && spawn.resultPreview && expanded.includes(spawn.spawnId) && <pre className="spawn-result">{spawn.resultPreview}</pre>}
      </article>)}
      {onClearSpawns && <Button type="link" size="small" className="spawn-clear" onClick={onClearSpawns}>{t("spawn.clear" as never)}</Button>}
    </div>}
  </section>;
}
