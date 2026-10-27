/** B 站扫码登录弹窗：出码 → 渲染二维码 → 轮询确认 → 回填 Cookie 到通道表单。
 *  码本身是 B 站 passport 的授权链接，手机 B 站 App 扫码确认；
 *  轮询节奏 2s；86083 过期自动换码重试一轮，连续网络错误静默退避。 */
import React, { useEffect, useRef, useState } from "react";
import { Alert, Modal, Spin, Typography, message } from "antd";
import QRCode from "qrcode";
import { api } from "../api";
import { useT } from "../i18n/core";

export function BiliQrLoginModal({
  open,
  onClose,
  onConfirmed,
}: {
  open: boolean;
  onClose: () => void;
  /** 登录成功：把 sessdata / biliJct / uid 回填表单 */
  onConfirmed: (login: { sessdata: string; biliJct: string; uid: string; uname: string }) => void;
}): React.ReactElement | null {
  const t = useT();
  const [qrDataUrl, setQrDataUrl] = useState<string>("");
  const [phase, setPhase] = useState<"loading" | "scanned" | "confirming">("loading");
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
      const r = await api.biliQrStart();
      const url = await QRCode.toDataURL(r.url, { width: 240, margin: 1 });
      if (!aliveRef.current) return;
      setQrDataUrl(url);
      void poll(r.qrcodeKey, 0);
    } catch (e) {
      if (!aliveRef.current) return;
      setError(String(e).slice(0, 160));
    }
  };

  const poll = async (key: string, fails: number) => {
    if (!aliveRef.current) return;
    try {
      const r = await api.biliQrPoll(key);
      if (!aliveRef.current) return;
      if (r.state === "confirmed") {
        setPhase("confirming");
        onConfirmed({ sessdata: r.sessdata, biliJct: r.biliJct, uid: r.uid, uname: r.uname });
        message.success(t("channels.biliqr.done", { name: r.uname }));
        // 回填即完成：等用户在表单里保存，弹窗自动关
        setTimeout(() => aliveRef.current && onClose(), 600);
        return;
      }
      // 86101 未扫 → 继续；86090 已扫待确认 → 提示；86083 过期 → 换码
      if (r.code === 86090) setPhase("scanned");
      else if (r.code === 86083) {
        void start(); // 过期自动换码
        return;
      }
      timerRef.current = setTimeout(() => void poll(key, 0), 2000);
    } catch (e) {
      if (!aliveRef.current) return;
      if (fails >= 2) {
        setError(String(e).slice(0, 160));
        return;
      }
      timerRef.current = setTimeout(() => void poll(key, fails + 1), 3000);
    }
  };

  if (!open) return null;
  return (
    <Modal
      open={open}
      title={t("channels.biliqr.title")}
      footer={null}
      onCancel={() => {
        stop();
        onClose();
      }}
      width={340}
    >
      <div className="biliqr-body">
        {error ? (
          <Alert type="error" showIcon message={error} action={<a onClick={() => void start()}>{t("common.retry")}</a>} />
        ) : qrDataUrl ? (
          <>
            <div className={`biliqr-img${phase === "scanned" ? " scanned" : ""}`}>
              <img src={qrDataUrl} alt="Bilibili login QR" width={240} height={240} />
              {phase === "scanned" && (
                <div className="biliqr-veil">
                  <Spin />
                  <Typography.Text style={{ color: "#fff" }}>{t("channels.biliqr.scanned")}</Typography.Text>
                </div>
              )}
            </div>
            <Typography.Text type="secondary" className="biliqr-hint">
              {t("channels.biliqr.hint")}
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
