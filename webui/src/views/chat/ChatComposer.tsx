//! views/chat/ChatComposer.tsx —— 输入区（codebase-refactor 7.2 拆分）：
//! 文件提及弹层、附件缩略图行、文本框（提及检测/历史召回/发送）、
//! 工具条（附件/模型选择）与发送/停止按钮。纯搬移：状态流转零改动。

import { Button, Input, Popconfirm, Select, message } from "antd";
import { ArrowUpOutlined, CloseOutlined, PauseCircleOutlined, PlusOutlined, RobotOutlined } from "@ant-design/icons";
import type React from "react";
import { api } from "../../api";
import { FileMentionPopup, type MentionState } from "../../components/FileMention";
import type { WorkbenchNode } from "../../api";
import type { LlmProfile } from "../../api";
import type { ChatMessage } from "../../types";

export interface ComposerModelOption {
  value: string;
  label: string;
}

export interface ChatComposerProps {
  mention: MentionState;
  mentionNodes: WorkbenchNode[];
  mentionLoading: boolean;
  mentionIdx: number;
  setMentionIdx: React.Dispatch<React.SetStateAction<number>>;
  applyMention: (path: string) => void;
  images: string[];
  removeImage: (i: number) => void;
  text: string;
  setText: React.Dispatch<React.SetStateAction<string>>;
  detectMention: (value: string, caret: number) => void;
  onBlurMention: () => void;
  onMentionKey: (e: React.KeyboardEvent<HTMLTextAreaElement>) => boolean;
  sessionId: string | null;
  setDraft: (sessionId: string, text: string) => void;
  messages: ChatMessage[];
  recallIdxRef: React.MutableRefObject<number | null>;
  isSubSession: boolean;
  running: boolean;
  doSend: () => void;
  addImage: (file: File) => void;
  currentModel: string;
  changeModel: (value: string) => void;
  modelOptions: ComposerModelOption[];
  profiles: LlmProfile[];
  t: (key: string, params?: Record<string, string>) => string;
}

export function ChatComposer(p: ChatComposerProps): React.ReactElement {
  const {
    mention, mentionNodes, mentionLoading, mentionIdx, setMentionIdx, applyMention,
    images, removeImage, text, setText, detectMention, onBlurMention, onMentionKey,
    sessionId, setDraft, messages, recallIdxRef, isSubSession, running, doSend,
    addImage, currentModel, changeModel, modelOptions, profiles, t,
  } = p;
  return (
    <div className="chat-composer">
      <FileMentionPopup
        nodes={mentionNodes}
        loading={mentionLoading}
        state={mention}
        idx={mentionIdx}
        onIdxChange={setMentionIdx}
        onPick={applyMention}
      />
      {images.length > 0 && (
        <div className="attach-row">
          {images.map((src, i) => (
            <span key={i} className="attach-thumb">
              <img src={src} alt={`附件${i + 1}`} />
              <Button
                size="small"
                type="text"
                className="attach-del"
                icon={<CloseOutlined />}
                onClick={() => removeImage(i)}
              />
            </span>
          ))}
        </div>
      )}
      <Input.TextArea
        className="composer-textarea"
        value={text}
        disabled={isSubSession}
        onChange={(e) => {
          setText(e.target.value);
          detectMention(e.target.value, e.target.selectionStart ?? e.target.value.length);
          if (sessionId) setDraft(sessionId, e.target.value);
        }}
        onBlur={onBlurMention}
        autoSize={{ minRows: 1, maxRows: 6 }}
        onPressEnter={(e) => {
          if (mention.open) return; // 提及弹层接管 Enter
          if (!e.shiftKey) {
            e.preventDefault();
            if (text.trim() && !running && !isSubSession) {
              doSend();
            }
          }
        }}
        onKeyDown={(e) => {
          if (onMentionKey(e)) return; // 提及弹层优先接管方向键/确认/关闭
          // 上/下箭头召回历史用户消息（chat-ergonomics）：仅在空输入时接管
          if (e.key !== "ArrowUp" && e.key !== "ArrowDown") return;
          if (text.trim()) return;
          const userMsgs = messages.filter((m) => m.role === "user");
          if (userMsgs.length === 0) return;
          e.preventDefault();
          let idx = recallIdxRef.current;
          if (e.key === "ArrowUp") {
            idx = idx === null ? userMsgs.length - 1 : Math.max(0, idx - 1);
          } else {
            if (idx === null) return;
            idx = idx + 1;
            if (idx >= userMsgs.length) {
              recallIdxRef.current = null;
              setText("");
              return;
            }
          }
          recallIdxRef.current = idx;
          const recalled = userMsgs[idx]?.statements.map((st) => st.text).join("\n") ?? "";
          setText(recalled);
        }}
      />
      <div className="composer-bar">
        <div className="composer-tools">
          <label className="attach-btn composer-attach" title={t("chat.attachTip")}>
            <PlusOutlined />
            <input
              type="file"
              accept="image/*"
              multiple
              style={{ display: "none" }}
              onChange={(e) => {
                Array.from(e.target.files ?? []).slice(0, 4).forEach(addImage);
                e.target.value = "";
              }}
            />
          </label>
          <span className="composer-model">
            <RobotOutlined />
            <Select
              value={currentModel}
              onChange={(v) => changeModel(v)}
              options={modelOptions}
              variant="borderless"
              size="small"
              className="composer-model-select"
              popupMatchSelectWidth={false}
              disabled={isSubSession || profiles.length === 0}
            />
          </span>
        </div>
        <div className="composer-actions">
          <span className="composer-hint">{t("chat.enterHint")}</span>
          {running && sessionId ? (
            <Popconfirm
              title={t("chat.stopConfirm")}
              onConfirm={() => {
                if (!sessionId) return;
                api.stopSession(sessionId)
                  .then((r) => message.success(t("chat.stopDone", { n: String(r.toolsExecuted) })))
                  .catch((e) => message.error(t("chat.stopFail", { err: String(e).slice(0, 120) })));
              }}
            >
              <Button
                className="composer-send composer-stop"
                shape="circle"
                danger
                icon={<PauseCircleOutlined />}
                aria-label={t("chat.stop")}
              />
            </Popconfirm>
          ) : (
            <Button
              className="composer-send"
              shape="circle"
              icon={<ArrowUpOutlined />}
              disabled={isSubSession || running || !text.trim()}
              onClick={doSend}
              aria-label={t("chat.send")}
            />
          )}
        </div>
      </div>
    </div>
  );
}
