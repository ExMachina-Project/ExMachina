/** 首启密钥设置页：无密钥部署的首次启动引导 —— 与登录门共用同一套主题与壳，
 *  仅信息不同：设置访问密钥（主）+ 下次再说 / 不再提醒（次）。
 *
 *  语义：设置 → 密钥落配置并本地记住，直接进入；「下次再说」进程内不再提示
 *  （网关重启恢复提醒）；「不再提醒」持久化关闭引导（config.security.keySetupDismissed）。
 */
import React, { useEffect, useRef, useState } from "react";
import { Button, Input, type InputRef } from "antd";
import { KeyOutlined } from "@ant-design/icons";
import { api } from "../api";
import { Emblem } from "../components/Emblem";
import { OrbitRings } from "../components/OrbitRings";
import { DataStream } from "../components/DataStream";
import { useT } from "../i18n/core";

export function SetupKeyView({ onEnter }: { onEnter: () => void }): React.ReactElement {
  const [key, setKey] = useState("");
  const [busy, setBusy] = useState<"" | "set" | "later" | "never">("");
  const [error, setError] = useState("");
  const inputRef = useRef<InputRef>(null);
  const t = useT();

  useEffect(() => {
    inputRef.current?.focus();
  }, []);

  const save = async () => {
    if (busy) return;
    const k = key.trim();
    if (!k) {
      setError(t("setup.empty"));
      return;
    }
    setBusy("set");
    setError("");
    try {
      await api.putConfig({ security: { authKey: k } });
      localStorage.setItem("exm.key", k);
      onEnter();
    } catch (e) {
      setError(String(e).slice(0, 120));
    } finally {
      setBusy("");
    }
  };

  const skip = async (mode: "later" | "never") => {
    if (busy) return;
    setBusy(mode);
    setError("");
    try {
      if (mode === "later") await api.snoozeKeySetup();
      else await api.dismissKeySetup();
      onEnter();
    } catch (e) {
      setError(String(e).slice(0, 120));
    } finally {
      setBusy("");
    }
  };

  return (
    <div className="login-wrap">
      {/* 资产 hero 轨道环（底层）：与登录门同一套壳 */}
      <OrbitRings />
      {/* 背景十六进制字节雨：与登录门同一套主题，仅信息不同 */}
      <DataStream side="left" />
      <DataStream side="right" />
      <div className="login-core">
        <div className="login-figure">
          <Emblem size={132} animated />
        </div>
        <h1 className="login-title">EX-MACHINA</h1>
        <div className="title-rule" />
        <div className="login-sub">
          {t("setup.sub")} <span className="mono">[INITIAL SETUP]</span>
        </div>
        <div className="login-box">
          <div className="login-label">
            <KeyOutlined /> {t("setup.keyLabel")} <span className="label-en">[ACCESS KEY]</span>
          </div>
          <Input.Password
            ref={inputRef}
            value={key}
            onChange={(e) => setKey(e.target.value)}
            placeholder={t("setup.keyPlaceholder")}
            onPressEnter={() => void save()}
            status={error ? "error" : undefined}
          />
          <Button type="primary" block loading={busy === "set"} onClick={() => void save()} className="login-btn">
            {t("setup.submit")} <span className="label-en">{"[SET & ENTER]"}</span>
          </Button>
          {error && <div className="login-error">{error}</div>}
          <div className="setup-actions">
            <Button type="text" size="small" disabled={!!busy} loading={busy === "later"} onClick={() => void skip("later")}>
              {t("setup.later")}
            </Button>
            <Button type="text" size="small" disabled={!!busy} loading={busy === "never"} onClick={() => void skip("never")}>
              {t("setup.never")}
            </Button>
          </div>
          <div className="setup-hint">{t("setup.hint")}</div>
        </div>
        <div className="login-foot mono">{t("login.foot")}</div>
      </div>
    </div>
  );
}
