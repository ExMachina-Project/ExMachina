/** Markdown 渲染器（GFM），用于消息与回流面板。
 *  代码块带语言标签 + 一键复制 + 长块折叠（chat-ergonomics / chat-render）；
 *  复制走 Clipboard API + execCommand 降级。 */
import React, { useState } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";

/** 复制文本：优先 Clipboard API，非安全上下文降级 execCommand */
export async function copyText(text: string): Promise<boolean> {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    try {
      const ta = document.createElement("textarea");
      ta.value = text;
      ta.style.position = "fixed";
      ta.style.opacity = "0";
      document.body.appendChild(ta);
      ta.select();
      const ok = document.execCommand("copy");
      document.body.removeChild(ta);
      return ok;
    } catch {
      return false;
    }
  }
}

/** 从代码块 children 树提取 (代码文本, 语言标签)。
 *  react-markdown v10 的 pre 子节点是 <code className="language-xx">。 */
function codeInfo(node: React.ReactNode): { text: string; lang: string } {
  let lang = "";
  const visit = (n: React.ReactNode) => {
    if (React.isValidElement(n)) {
      const cls = (n.props as { className?: string }).className ?? "";
      const m = cls.match(/language-([\w+-]+)/);
      if (m) lang = m[1];
      visit((n.props as { children?: React.ReactNode }).children);
    } else if (Array.isArray(n)) {
      n.forEach(visit);
    }
  };
  visit(node);
  return { text: extractText(node), lang };
}

/** 超过该行数/字节的代码块默认折叠，只露头尾 */
const FOLD_LINES = 14;

/** 代码块容器：语言标签 + 复制按钮；超长默认折叠（点击展开/收起） */
function CodeBlock(props: React.HTMLAttributes<HTMLPreElement>): React.ReactElement {
  const [copied, setCopied] = useState(false);
  const { text, lang } = codeInfo(props.children);
  const lineCount = text ? text.replace(/\n$/, "").split("\n").length : 0;
  const long = lineCount > FOLD_LINES;
  const [expanded, setExpanded] = useState(false);
  const showFolded = long && !expanded;
  return (
    <div className={`code-block-wrap${long ? " code-block-long" : ""}`}>
      {(lang || long) && (
        <div className="code-block-meta">
          {lang && <span className="code-lang mono">{lang}</span>}
          <span className="code-lines mono">{lineCount} 行</span>
          {long && (
            <button className="code-fold-btn" onClick={() => setExpanded((v) => !v)}>
              {expanded ? "收起" : `展开 ${lineCount} 行`}
            </button>
          )}
        </div>
      )}
      <button
        className="code-copy-btn"
        onClick={async () => {
          if (await copyText(text)) {
            setCopied(true);
            setTimeout(() => setCopied(false), 1200);
          }
        }}
        aria-label="copy"
      >
        {copied ? "已复制" : "复制"}
      </button>
      {showFolded ? (
        <pre className="code-folded">
          <code>{text.split("\n").slice(0, FOLD_LINES).join("\n")}</code>
          <span className="code-fold-ellipsis">…</span>
        </pre>
      ) : (
        <pre {...props}>{props.children}</pre>
      )}
    </div>
  );
}

/** 递归抽取子节点的纯文本（代码块内容在 children 树里） */
function extractText(node: React.ReactNode): string {
  if (node == null || typeof node === "boolean") return "";
  if (typeof node === "string" || typeof node === "number") return String(node);
  if (Array.isArray(node)) return node.map(extractText).join("");
  if (React.isValidElement(node)) {
    return extractText((node.props as { children?: React.ReactNode }).children);
  }
  return "";
}

/** 裸 JSON 检测与美化：智能体的结构化回复（无围栏）若直接当段落渲染会一行拉满。
 *  整段是合法 JSON（对象/数组）时包成 ```json 围栏，让代码块管线接管（复制/折叠/语言标签）。 */
function wrapBareJson(text: string): string {
  const s = text.trim();
  if ((!s.startsWith("{") && !s.startsWith("[")) || s.length < 40) return text;
  if (text.includes("```")) return text; // 已含围栏的不动
  try {
    const parsed: unknown = JSON.parse(s);
    if (parsed !== null && typeof parsed === "object") {
      return "```json\n" + JSON.stringify(parsed, null, 2) + "\n```";
    }
  } catch {
    /* 不是合法 JSON，按原文本渲染 */
  }
  return text;
}

export function Markdown({ text }: { text: string }): React.ReactElement {
  return (
    <div className="md">
      <ReactMarkdown
        remarkPlugins={[remarkGfm]}
        components={{
          pre: (props) => <CodeBlock {...(props as React.HTMLAttributes<HTMLPreElement>)} />,
        }}
      >
        {wrapBareJson(text)}
      </ReactMarkdown>
    </div>
  );
}
