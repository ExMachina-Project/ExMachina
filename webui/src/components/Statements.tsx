/** 陈述渲染：句式前缀着色 + 证据等级徽标 */
import React from "react";
import { Tag } from "antd";
import type { SpeechTag, Statement } from "../types";
import { useT } from "../i18n/core";

const TAG_COLORS: Record<SpeechTag, string> = {
  肯定: "green",
  否定: "red",
  疑问: "gold",
  报告: "cyan",
  提案: "purple",
  警告: "volcano",
  要求: "geekblue",
  观测: "default",
};

const LEVEL_COLORS: Record<string, string> = { A: "red", B: "orange", C: "blue", D: "default" };

export function StatementLine({ s }: { s: Statement }): React.ReactElement {
  const t = useT();
  return (
    <div className="stmt-line">
      <Tag color={TAG_COLORS[s.tag]} className="stmt-tag">
        {s.tag}
      </Tag>
      <span>{s.text}</span>
      {s.evidenceLevel && (
        <Tag color={LEVEL_COLORS[s.evidenceLevel]} className="stmt-level">
          {t("statements.evidence", { s: s.evidenceLevel })}
        </Tag>
      )}
    </div>
  );
}

export function StatementList({ statements, plain }: { statements: Statement[]; plain?: boolean }): React.ReactElement {
  // plain（聊天流只读视角）：正文直排，不带句式 tag / 证据徽标——流内每条
  // user 消息落库统一是「要求」标签，逐条徽标是满屏噪音（部署实测反馈）
  if (plain) {
    return (
      <div>
        {statements.map((s, i) => (
          <div key={i} className="stmt-line stmt-plain">
            <span>{s.text}</span>
          </div>
        ))}
      </div>
    );
  }
  return (
    <div>
      {statements.map((s, i) => (
        <StatementLine key={i} s={s} />
      ))}
    </div>
  );
}
