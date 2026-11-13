/**
 * Mermaid 图表渲染块（chart-rendering）：```mermaid 围栏 → SVG。
 * 动态 import mermaid（仅首个图表触发加载，独立 vendor chunk 不进主 bundle）；
 * 语法错误回落原代码块并附错误提示；主题跟随（深/浅）。
 */
import React, { useEffect, useRef, useState } from "react";
import { useTheme } from "../theme";
import { themeOf } from "../theme";

type MermaidState =
  | { phase: "loading" }
  | { phase: "ok"; svg: string }
  | { phase: "error"; message: string };

/** mermaid 单例初始化（幂等）：主题按当前预设切换深浅 */
let mermaidReady: Promise<{
  render: (id: string, text: string) => Promise<{ svg: string }>;
}> | null = null;

async function getMermaid(dark: boolean) {
  const mermaid = await import("mermaid");
  mermaid.default.initialize({
    startOnLoad: false,
    theme: dark ? "dark" : "default",
    securityLevel: "strict",
    fontFamily: "inherit",
  });
  return mermaid.default;
}

export function MermaidBlock({ code }: { code: string }): React.ReactElement {
  const preset = useTheme((s) => s.preset);
  const dark = themeOf(preset).mode === "dark";
  const [state, setState] = useState<MermaidState>({ phase: "loading" });
  const seqRef = useRef(0);

  useEffect(() => {
    const seq = ++seqRef.current;
    let cancelled = false;
    (async () => {
      try {
        const mermaid = await getMermaid(dark);
        if (cancelled || seq !== seqRef.current) return;
        const id = `exm-mermaid-${seq}`;
        const { svg } = await mermaid.render(id, code);
        if (!cancelled && seq === seqRef.current) {
          setState({ phase: "ok", svg });
        }
      } catch (e) {
        if (!cancelled && seq === seqRef.current) {
          setState({
            phase: "error",
            message: e instanceof Error ? e.message : String(e),
          });
        }
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [code, dark]);

  if (state.phase === "ok") {
    // SVG 注入：securityLevel=strict 已禁用事件绑定；容器 overflow 滚动防溢出
    return (
      <div
        className="mermaid-block"
        dangerouslySetInnerHTML={{ __html: state.svg }}
      />
    );
  }
  // 语法错误 / 加载中：回落原代码块（错误时附提示行，图源码可复制修正）
  return (
    <div className="code-block-wrap">
      <div className="code-block-meta">
        <span className="code-lang mono">mermaid</span>
        {state.phase === "error" && (
          <span className="mermaid-err mono">图表语法错误：{state.message.slice(0, 120)}</span>
        )}
      </div>
      <pre>
        <code>{code}</code>
      </pre>
    </div>
  );
}
