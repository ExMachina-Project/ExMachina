//! views/chat/ChatComposer.tsx —— 输入区（codebase-refactor 7.2 拆分）：
//! 文件提及弹层、附件缩略图行、文本框（提及检测/历史召回/发送）、
//! 工具条（附件/模型选择）与发送/停止按钮。纯搬移：状态流转零改动。

import { Button, Input, Popconfirm, Select, message } from "antd";
import { ArrowUpOutlined, CloseOutlined, PauseCircleOutlined, PlusOutlined, RobotOutlined } from "@ant-design/icons";
import type React from "react";
import { useRef } from "react";
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
  const fileInputRef = useRef<HTMLInputElement>(null);
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
              <img src={src} alt={t("chat.attachAlt", { n: String(i + 1) })} />
              <Button
                size="small"
                type="text"
                className="attach-del"
                icon={<CloseOutlined />}
                aria-label={t("chat.removeAttachment", { n: String(i + 1) })}
                onClick={() => removeImage(i)}
              />
            </span>
          ))}
        </div>
      )}
      <Input.TextArea
        className="composer-textarea"
        value={text}
        aria-label={t("chat.messageLabel")}
        disabled={isSubSession}
        onChange={(e) => {
          setText(e.target.value);
          detectMention(e.target.value, e.target.selectionStart ?? e.target.value.length);
          if (sessionId) setDraft(sessionId, e.target.value);
        }}
        onBlur={onBlurMention}
        autoSize={{ minRows: 1, maxRows: 6 }}
        onPressEnter={(e) => {
          if (e.nativeEvent.isComposing || e.repeat || e.keyCode === 229) return;
          if (mention.open) return; // 提及弹层接管 Enter
          if (!e.shiftKey) {
            e.preventDefault();
            // running 时同样允许发送（capability-parity 3.5）：/chat 已分流
            // steering——运行中补充的普通消息引导当前任务，不取消不开新轮。
            if (text.trim() && !isSubSession) {
              doSend();
            }
          }
        }}
        onKeyDown={(e) => {
          if (e.nativeEvent.isComposing || e.keyCode === 229) return;
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
              if (sessionId) setDraft(sessionId, "");
              return;
            }
          }
          recallIdxRef.current = idx;
          const recalled = userMsgs[idx]?.statements.map((st) => st.text).join("\n") ?? "";
          setText(recalled);
          if (sessionId) setDraft(sessionId, recalled);
        }}
      />
      <div className="composer-bar">
        <div className="composer-tools">
          <Button
            type="text"
            className="attach-btn composer-attach"
            title={t("chat.attachTip")}
            aria-label={t("chat.attachTip")}
            icon={<PlusOutlined />}
            disabled={isSubSession}
            onClick={() => fileInputRef.current?.click()}
          />
          <input
            ref={fileInputRef}
            type="file"
            accept="image/*"
            multiple
            style={{ display: "none" }}
            onChange={(e) => {
              Array.from(e.target.files ?? []).slice(0, 4).forEach(addImage);
              e.target.value = "";
            }}
          />
          <span className="composer-model">
            <RobotOutlined />
            <Select
              value={currentModel}
              onChange={(v) => changeModel(v)}
              options={modelOptions}
              variant="borderless"
              size="small"
              className="composer-model-select"
              aria-label={t("chat.modelLabel")}
              popupMatchSelectWidth={false}
              disabled={isSubSession || profiles.length === 0}
            />
          </span>
        </div>
        <div className="composer-actions">
          <span className="composer-hint">
            {running && !isSubSession ? t("chat.steeringHint") : t("chat.enterHint")}
          </span>
          {/* 加入队列（capability-parity 3.5）：显式排队为后续独立任务——与普通
              发送的 steering 语义严格分离（session-steering spec「应用界面显式
              加入队列」）；提交与 CLI/聊天走同一 REST 面与规范请求体。 */}
          <Button
            size="small"
            className="composer-queue"
            title={t("chat.queueTip")}
            aria-label={t("chat.queue")}
            disabled={isSubSession || !text.trim() || !sessionId}
            onClick={() => {
              const task = text.trim();
              if (!sessionId || !task) return;
              api.queueAdd(sessionId, task)
                .then((r) => {
                  // 回执如实（queued+pending，不冒充完成）；pending 计数后端未回，
                  // 以 seq 呈现序号即可，待处理数留给队列条实时呈现
                  message.success(t("chat.queueDone", { seq: String(r.seq) }));
                  setText("");
                  if (sessionId) setDraft(sessionId, "");
                })
                .catch((e) => message.error(t("chat.queueFail", { err: String(e).slice(0, 120) })));
            }}
          >
            {t("chat.queue")}
          </Button>
          {/* 停止按钮（独立控制操作）与发送按钮并存：运行中普通发送 = steering
              （不取消运行），停止仍走显式确认——两种操作语义分离（3.4/3.5）。 */}
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
          ) : null}
          <Button
            className="composer-send"
            shape="circle"
            icon={<ArrowUpOutlined />}
            disabled={isSubSession || !text.trim()}
            onClick={doSend}
            aria-label={t("chat.send")}
          />
        </div>
      </div>
    </div>
  );
}
