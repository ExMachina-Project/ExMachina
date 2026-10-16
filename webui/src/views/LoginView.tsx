/** 登录门：访问密钥鉴权 —— 品牌徽记 + 标题 + 极简面板
 *
 *  视觉口径：中性底色、圆角面板、柔和阴影，强调色只做点缀；无扫描线 / 字符雨 / 角框。
 *  动效保留（transform/opacity）：徽记缓呼吸、入场浮升；prefers-reduced-motion 全局降级。
 */
import React, { useEffect, useRef, useState } from "react";
import { Button, Input, type InputRef } from "antd";
import { KeyOutlined } from "@ant-design/icons";
import { api } from "../api";
import { Emblem } from "../components/Emblem";
import { DataStream } from "../components/DataStream";
import { useT } from "../i18n/core";

export function LoginView({ onUnlock }: { onUnlock: () => void }): React.ReactElement {
  const [key, setKey] = useState("");
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");
  const inputRef = useRef<InputRef>(null);
  const t = useT();

  useEffect(() => {
    inputRef.current?.focus();
  }, []);

  const unlock = async () => {
    if (!key.trim() || loading) return;
    setLoading(true);
    setError("");
    try {
      const r = await api.verifyAuth(key.trim());
      if (r.ok) {
        localStorage.setItem("exm.key", key.trim());
        onUnlock();
      } else {
        setError(r.error ?? t("login.badKey"));
      }
    } catch (e) {
      setError(String(e).slice(0, 120));
    } finally {
      setLoading(false);
    }
  };

  return (
    <div className="login-wrap">
      {/* 背景十六进制字节雨（左右两列）：放在 login-wrap 内部（它是定位祖先），z-index 低于 login-core */}
      <DataStream side="left" />
      <DataStream side="right" />
      <div className="login-core">
        <div className="login-figure">
          <Emblem size={132} animated />
        </div>
        <h1 className="login-title">EX-MACHINA</h1>
        <div className="title-rule" />
        <div className="login-sub">
          {t("login.sub")} <span className="mono">[SECURE ACCESS]</span>
        </div>
        <div className="login-box">
          <div className="login-label">
            <KeyOutlined /> {t("login.keyLabel")} <span className="label-en">[ACCESS KEY]</span>
          </div>
          <Input.Password
            ref={inputRef}
            value={key}
            onChange={(e) => setKey(e.target.value)}
            placeholder={t("login.keyPlaceholder")}
            onPressEnter={() => void unlock()}
            status={error ? "error" : undefined}
          />
          <Button type="primary" block loading={loading} onClick={() => void unlock()} className="login-btn">
            {t("login.unlock")} <span className="label-en">[UNLOCK]</span>
          </Button>
          {error && <div className="login-error">{error}</div>}
        </div>
        <div className="login-foot mono">{t("login.foot")}</div>
      </div>
    </div>
  );
}
