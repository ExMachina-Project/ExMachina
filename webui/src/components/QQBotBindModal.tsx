/** QQ 官方机器人扫码绑定弹窗：出绑定任务 → 渲染二维码 → 轮询确认 → 回填 AppID/AppSecret。
 *  二维码是 q.qq.com 的绑定页，手机 QQ 扫码确认后平台下发 AES-256-GCM 加密的 AppSecret；
 *  解密在网关完成（密钥只存在于本弹窗内存，不落盘）。
 *  轮询节奏 2s；过期需重新出码；连续网络错误静默退避一次。 */
import React, { useEffect, useRef, useState } from "react";
import { Alert, Modal, Spin, Typography, message } from "antd";
import QRCode from "qrcode";
import { api } from "../api";
import { useT } from "../i18n/core";

export function QQBotBindModal({
  open,
  onClose,
  onConfirmed,
}: {
  open: boolean;
  onClose: () => void;
  /** 绑定成功：把 AppID / AppSecret 回填通道表单 */
  onConfirmed: (creds: { appId: string; appSecret: string }) => void;
}): React.ReactElement | null {
  const t = useT();
  const [qrDataUrl, setQrDataUrl] = useState<string>("");
  const [phase, setPhase] = useState<"loading" | "waiting" | "scanned" | "expired" | "confirming">("loading");
  const [error, setError] = useState<string>("");
  const timerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const aliveRef = useRef(true);

  // 生命周期：open 时出码，关闭时停轮询
  useEffect(() => {
    aliveRef.current = open;
    if (open) void start();
    return () => {
      aliveRef.current = false;
      if (timerRef.current) clearTimeout(timerRef.current);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open]);

  const stop = () => {
    if (timerRef.current) clearTimeout(timerRef.current);
    timerRef.current = null;
  };

  const start = async () => {
    stop();
    setQrDataUrl("");
    setPhase("loading");
    setError("");
    try {
      const r = await api.qqbotBindStart();
      const url = await QRCode.toDataURL(r.url, { width: 240, margin: 1 });
      if (!aliveRef.current) return;
      setQrDataUrl(url);
      setPhase("waiting");
      void poll(r.taskId, r.key, 0);
    } catch (e) {
      if (!aliveRef.current) return;
      setError(String(e).slice(0, 160));
    }
  };

  const poll = async (taskId: string, key: string, fails: number) => {
    if (!aliveRef.current) return;
    try {
      const r = await api.qqbotBindPoll(taskId, key);
      if (!aliveRef.current) return;
      if (r.state === "confirmed") {
        setPhase("confirming");
        onConfirmed({ appId: r.appId, appSecret: r.appSecret });
        message.success(t("channels.qqbind.done", { id: r.appId }));
        // 回填即完成：等用户在表单里保存，弹窗自动关
        setTimeout(() => aliveRef.current && onClose(), 600);
        return;
      }
      if (r.state === "expired") {
        setPhase("expired");
        return;
      }
      if (r.state === "scanned") setPhase("scanned");
      timerRef.current = setTimeout(() => void poll(taskId, key, 0), 2000);
    } catch (e) {
      if (!aliveRef.current) return;
      if (fails >= 2) {
        setError(String(e).slice(0, 160));
        return;
      }
      timerRef.current = setTimeout(() => void poll(taskId, key, fails + 1), 3000);
    }
  };

  if (!open) return null;
  return (
    <Modal
      open={open}
      title={t("channels.qqbind.title")}
      footer={null}
      onCancel={() => {
        stop();
        onClose();
      }}
      width={340}
    >
      <div className="biliqr-body">
        {error ? (
          <Alert type="error" showIcon title={error} action={<a onClick={() => void start()}>{t("common.retry")}</a>} />
        ) : phase === "expired" ? (
          <Alert
            type="warning"
            showIcon
            title={t("channels.qqbind.expired")}
            action={<a onClick={() => void start()}>{t("common.retry")}</a>}
          />
        ) : qrDataUrl ? (
          <>
            <div className={`biliqr-img${phase === "scanned" ? " scanned" : ""}`}>
              <img src={qrDataUrl} alt="QQ bot bind QR" width={240} height={240} />
              {phase === "scanned" && (
                <div className="biliqr-veil">
                  <Spin />
                  <Typography.Text style={{ color: "#fff" }}>{t("channels.qqbind.scanned")}</Typography.Text>
                </div>
              )}
            </div>
            <Typography.Text type="secondary" className="biliqr-hint">
              {t("channels.qqbind.hint")}
            </Typography.Text>
          </>
        ) : (
          <div className="biliqr-loading">
            <Spin />
          </div>
        )}
      </div>
    </Modal>
  );
}
