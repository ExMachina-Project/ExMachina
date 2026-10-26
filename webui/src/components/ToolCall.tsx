/** 工具执行轨迹卡：编程软件式过程透明——文件操作 / 终端命令 / 截屏全程可见 */
import React, { useState } from "react";
import { Button, Tag, message } from "antd";
import {
  BugOutlined, CheckCircleOutlined, CloseCircleOutlined, CodeOutlined,
  FileTextOutlined, FolderOutlined, GlobalOutlined, Loading3QuartersOutlined,
  SearchOutlined, DesktopOutlined, ClockCircleOutlined, ThunderboltOutlined,
  BranchesOutlined, RocketOutlined, UndoOutlined,
} from "@ant-design/icons";
import { assetUrl } from "../api";
import { api } from "../api";
import type { ToolCallItem } from "../types";

const TOOL_ICON: Record<string, React.ReactNode> = {
  read: <FileTextOutlined />,
  edit: <CodeOutlined />,
  grep: <SearchOutlined />,
  glob: <SearchOutlined />,
  filesystem: <FolderOutlined />,
  terminal: <ThunderboltOutlined />,
  web_search: <GlobalOutlined />,
  web_fetch: <GlobalOutlined />,
  browser: <GlobalOutlined />,
  computer: <DesktopOutlined />,
  git: <BranchesOutlined />,
  spawn: <RocketOutlined />,
};

/** 工具调用的单行摘要（对齐主流编码智能体的过程行） */
function callTitle(c: ToolCallItem): string {
  const a = c.args as Record<string, string | undefined>;
  switch (c.tool) {
    case "read":
      return `${a.path ?? ""}`;
    case "edit":
      return `${a.path ?? ""}`;
    case "grep":
      return `${a.pattern ?? ""}`;
    case "glob":
      return `${a.pattern ?? ""}`;
    case "filesystem":
      return `${a.op ?? ""} ${a.path ?? ""}`;
    case "terminal":
      return `${a.command ?? a.taskId ?? a.op ?? ""}`;
    case "web_search":
      return `${a.query ?? ""}`;
    case "web_fetch":
      return `${a.url ?? ""}`;
    case "browser":
      return `${a.op ?? ""} ${a.url ?? ""}`;
    case "computer":
      return opTitle(a);
    case "git":
      return `${a.action ?? ""}${a.pathSpec ? ` ${a.pathSpec}` : ""}${a.branch ? ` ${a.branch}` : ""}`;
    case "spawn":
      return `${(a.task ?? "").slice(0, 60)}${(a.task ?? "").length > 60 ? "…" : ""}`;
    default:
      return Object.values(a)[0] !== undefined ? String(Object.values(a)[0]).slice(0, 80) : "";
  }
}

function opTitle(a: Record<string, string | undefined>): string {
  const op = a.op ?? "screenshot";
  switch (op) {
    case "click": return `click ${a.double ? "×2 " : ""}${a.button ?? "left"} @ (${a.x ?? "?"}, ${a.y ?? "?"})`;
    case "move": return `move → (${a.x ?? "?"}, ${a.y ?? "?"})`;
    case "scroll": return `scroll dx=${a.dx ?? 0} dy=${a.dy ?? 0}`;
    case "type": return `type「${String(a.text ?? "").slice(0, 30)}」`;
    case "key": return `key ${a.combo ?? ""}`;
    default: return `screenshot${a.monitor ? ` #${a.monitor}` : ""}`;
  }
}

/** edit 的简易 diff 视图（old_string → new_string，按行上色） */
function DiffView({ c }: { c: ToolCallItem }): React.ReactElement | null {
  const a = c.args as Record<string, string | undefined>;
  const oldS = typeof a.old_string === "string" ? a.old_string : "";
  const newS = typeof a.new_string === "string" ? a.new_string : "";
  if (!c.args.old_string && !c.args.new_string) return null;
  const maxLines = 400;
  const oldLines = oldS.split("\n").slice(0, maxLines);
  const newLines = newS.split("\n").slice(0, maxLines);
  return (
    <div className="tool-diff">
      {oldLines.map((l, i) => (
        <div key={`o${i}`} className="diff-line diff-del">- {l}</div>
      ))}
      {newLines.map((l, i) => (
        <div key={`n${i}`} className="diff-line diff-add">+ {l}</div>
      ))}
    </div>
  );
}

/** terminal 产出面板：命令 / 输出（stdout-stderr 合流按源着色）/ 退出码分区，长输出折叠 */
function TerminalView({ c }: { c: ToolCallItem }): React.ReactElement | null {
  const a = c.args as Record<string, unknown>;
  const op = String(a.op ?? "run");
  const cmd = typeof a.command === "string" ? a.command : "";
  const taskId = typeof a.taskId === "string" ? a.taskId : "";
  // 输出来源：结果文本（summary 已含运行输出；工具层 stdout/stderr 合流标注）
  const out = c.summary ?? "";
  if (!cmd && !taskId && !out) return null;
  const isBg = a.background === true;
  const lines = out.split("\n");
  const MAX = 40;
  const folded = lines.length > MAX;
  return (
    <div className="tool-terminal">
      <div className="term-row">
        <span className="term-label">命令</span>
        <code className="term-cmd">{cmd || `terminal ${op} ${taskId}`}</code>
        {isBg && <Tag color="blue">后台</Tag>}
        {c.status === "ok" && !isBg && <Tag color="green">exit 0</Tag>}
        {c.status === "error" && <Tag color="red">失败</Tag>}
      </div>
      {out && (
        <pre className="term-out">
          {folded ? lines.slice(0, MAX).join("\n") : out}
          {folded && `\n…（共 ${lines.length} 行，已折叠；完整输出见工具结果）`}
        </pre>
      )}
      {isBg && taskId && (
        <div className="term-row">
          <span className="term-label">后台任务</span>
          <code>{taskId}</code>
          <span style={{ fontSize: 12, color: "#888" }}>模型经 terminal(poll/kill) 轮询与终止</span>
        </div>
      )}
    </div>
  );
}

/** 单条工具卡：状态图标 + 摘要行 + 可展开详情（参数 / 结果 / diff / 截图 / 回滚） */
export function ToolCallCard({ c, sessionId, turn }: { c: ToolCallItem; sessionId?: string; turn?: number }): React.ReactElement {
  const [open, setOpen] = useState(false);
  const [rolling, setRolling] = useState(false);
  const [rolled, setRolled] = useState(false);
  const icon = TOOL_ICON[c.tool] ?? <BugOutlined />;
  const statusNode =
    c.status === "running" ? (
      <Loading3QuartersOutlined spin className="tool-spin" />
    ) : c.status === "ok" ? (
      <CheckCircleOutlined className="tool-ok" />
    ) : (
      <CloseCircleOutlined className="tool-err" />
    );
  const title = callTitle(c);
  const hasDetail =
    !!c.summary || Object.keys(c.args ?? {}).length > 0 || (c.images?.length ?? 0) > 0;
  const isEdit = c.tool === "edit" || (c.tool === "filesystem" && c.args.op === "write");
  // 可回滚类：文件修改（edit/patch/filesystem.write）与 git 写动作——
  // 检查点在动作执行前登记，undo 恢复到该轮开始前状态（spec: webui-code-workbench）
  const rollbackable =
    sessionId != null &&
    typeof turn === "number" &&
    c.status === "ok" &&
    (c.tool === "edit" ||
      c.tool === "patch" ||
      (c.tool === "filesystem" && (c.args as Record<string, unknown>).op === "write") ||
      (c.tool === "git" &&
        ["add", "commit", "restore", "stash-push", "stash-pop", "checkout-b"].includes(
          String((c.args as Record<string, unknown>).action ?? ""),
        )));
  const onRollback = async (e: React.MouseEvent) => {
    e.stopPropagation(); // 不触发卡片展开
    if (!sessionId || typeof turn !== "number" || rolling) return;
    setRolling(true);
    try {
      const r = await api.undoSession(sessionId, turn, true);
      setRolled(true);
      void message.success(`已回滚到第 ${r.uptoTurn} 轮前（恢复 ${r.restored.length} 个文件）`);
    } catch (err) {
      void message.error(`回滚失败：${err}`);
    } finally {
      setRolling(false);
    }
  };
  return (
    <div className={`tool-card tool-${c.status}`} onClick={() => hasDetail && setOpen(!open)}>
      <div className="tool-row">
        {statusNode}
        <span className="tool-icon">{icon}</span>
        <span className="tool-name">{c.tool}</span>
        <span className="tool-title">{title}</span>
        {typeof c.durationMs === "number" && c.status !== "running" && (
          <Tag className="tool-ms" icon={<ClockCircleOutlined />}>{c.durationMs}ms</Tag>
        )}
        {rollbackable && !rolled && (
          <Button
            size="small"
            danger
            icon={<UndoOutlined />}
            loading={rolling}
            onClick={onRollback}
            title={`回滚到第 ${turn} 轮开始前（恢复检查点登记的文件）`}
          >
            回滚
          </Button>
        )}
        {rolled && <Tag color="orange">已回滚</Tag>}
      </div>
      {open && (
        <div className="tool-detail">
          {isEdit && <DiffView c={c} />}
          {c.tool === "terminal" && <TerminalView c={c} />}
          {c.images && c.images.length > 0 && (
            <div className="tool-shots">
              {c.images.map((p, i) => (
                <img key={i} src={assetUrl(p)} alt={`screenshot-${i + 1}`} className="tool-shot" loading="lazy" />
              ))}
            </div>
          )}
          {Object.keys(c.args ?? {}).length > 0 && !isEdit && (
            <pre className="tool-args">{JSON.stringify(c.args, null, 2).slice(0, 2000)}</pre>
          )}
          {c.summary && <pre className="tool-summary">{c.summary}</pre>}
        </div>
      )}
    </div>
  );
}

/** 工具轨迹列表 */
export function ToolCallList({ calls, sessionId, turn }: { calls: ToolCallItem[]; sessionId?: string; turn?: number }): React.ReactElement | null {
  if (!calls.length) return null;
  return (
    <div className="tool-list">
      {calls.map((c) => (
        <ToolCallCard key={c.callId} c={c} sessionId={sessionId} turn={turn} />
      ))}
    </div>
  );
}
