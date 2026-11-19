import { useCallback, useEffect, useState } from "react";
import { Alert, App, Button, Form, Input, Modal, Select, Switch, Tabs } from "antd";
import { desktopInvoke, type DesktopConfig } from "./native";
import { CopyOutlined, ReloadOutlined } from "@ant-design/icons";
import type { ExmClient } from "../../client/src/api/client";

function NativeSettings({ onRestartGateway }: { onRestartGateway?: (config: DesktopConfig) => Promise<void> }) {
  const { message } = App.useApp();
  const [form] = Form.useForm<DesktopConfig>();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  useEffect(() => {
    if (window.__TAURI__) void desktopInvoke<DesktopConfig>("get_desktop_config").then((cfg) => form.setFieldsValue(cfg)).catch((e: unknown) => setError(String(e)));
  }, [form]);
  if (!window.__TAURI__) return <Alert type="info" title="窗口行为、系统通知和快捷键在原生桌面客户端中设置。" />;
  return <Form form={form} layout="vertical" onFinish={async (values) => {
    setBusy(true); setError("");
    try { await desktopInvoke("set_desktop_config", { ...values }); message.success("桌面设置已保存"); }
    catch (e) { setError(String(e)); } finally { setBusy(false); }
  }}>
      {error && <Alert type="error" title={error} />}
      <Form.Item name="allowLan" label="允许局域网手机连接" extra="重启本机实例后生效。仅在可信局域网或 VPN 内使用；连接仍需要实例访问密钥。" valuePropName="checked"><Switch /></Form.Item>
    <Form.Item name="exitOnClose" label="关闭窗口时退出应用" extra="关闭此选项后，窗口关闭时会驻留托盘。" valuePropName="checked"><Switch /></Form.Item>
    <Form.Item name="shortcut" label="唤起 / 隐藏快捷键" rules={[{ required: true }]}><Input /></Form.Item>
    <Form.Item name="notifyApproval" label="审批通知" valuePropName="checked"><Switch /></Form.Item>
    <Form.Item name="notifyCron" label="自动化通知" valuePropName="checked"><Switch /></Form.Item>
    <Form.Item name="notifyTaskDone" label="任务完成通知" valuePropName="checked"><Switch /></Form.Item>
    <Button htmlType="submit" loading={busy} type="primary">保存桌面设置</Button>
    {onRestartGateway && <Button style={{ marginLeft: 8 }} icon={<ReloadOutlined />} disabled={busy} onClick={async () => {
      setBusy(true); setError("");
      try {
        await form.validateFields();
        const config = form.getFieldsValue(true);
        await onRestartGateway(config);
        message.success("本机实例已重启");
      }
      catch (e) { setError(String(e)); }
      finally { setBusy(false); }
    }}>应用网络设置并重启</Button>}
  </Form>;
}
export function DesktopSettings({ open, onClose, consoleUrl, mobileUrl, accessKey, onRestartGateway, client }: {
  open: boolean; onClose: () => void; consoleUrl?: string; mobileUrl?: string; accessKey?: string; onRestartGateway?: (config: DesktopConfig) => Promise<void>; client?: ExmClient;
}) {
  const { message } = App.useApp();
  const [profiles, setProfiles] = useState<{ id: string; name: string; baseUrl: string; model: string }[]>([]);
  const [selected, setSelected] = useState("");
  const [providerName, setProviderName] = useState("");
  const [baseUrl, setBaseUrl] = useState("");
  const [model, setModel] = useState("");
  const [apiKey, setApiKey] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const refreshProfiles = useCallback(async () => {
    if (!client) return;
    try {
      const result = await client.api.llmProfiles();
      setProfiles(result.profiles);
      setSelected(result.active);
      const active = result.profiles.find((profile) => profile.id === result.active);
      if (active) { setProviderName(active.name); setBaseUrl(active.baseUrl); setModel(active.model); }
    } catch (e) { setError(String(e)); }
  }, [client]);
  useEffect(() => { if (open) void refreshProfiles(); }, [open, refreshProfiles]);
  const saveProfile = async () => {
    if (!client || busy || !providerName.trim() || !baseUrl.trim() || !model.trim()) return;
    setBusy(true); setError("");
    try {
      const result = await client.api.saveLlmProfile({
        ...(selected ? { id: selected } : {}), name: providerName.trim(), baseUrl: baseUrl.trim(), model: model.trim(),
        ...(apiKey.trim() ? { apiKey: apiKey.trim() } : {}),
      });
      await client.api.activateLlmProfile(result.id);
      setApiKey(""); await refreshProfiles(); message.success("模型连接已保存并设为当前默认");
    } catch (e) { setError(String(e)); }
    finally { setBusy(false); }
  };
  const testProfile = async () => {
    if (!client || !selected || busy) return;
    setBusy(true); setError("");
    try {
      const result = await client.api.testLlmProfile(selected);
      if (!result.ok) throw new Error(result.error ?? result.message ?? "连接测试失败");
      message.success(`连接成功${result.latencyMs ? ` · ${result.latencyMs} ms` : ""}`);
    } catch (e) { setError(String(e)); }
    finally { setBusy(false); }
  };
  return <Modal open={open} onCancel={onClose} footer={null} title="桌面设置" width={640} className="desk-settings" destroyOnHidden>
    <Tabs items={[
      { key: "native", label: "桌面", children: <NativeSettings onRestartGateway={onRestartGateway} /> },
      { key: "mobile", label: "手机连接", children: mobileUrl && accessKey ? <div className="desk-mobile-connect">
        <Alert type="warning" title="仅在可信局域网或 VPN 使用；访问密钥为管理凭证。请勿通过不可信 HTTP 网络分享。" />
        <label>网关地址<Input readOnly value={mobileUrl} /></label>
        <Button icon={<CopyOutlined />} onClick={() => void navigator.clipboard.writeText(mobileUrl)}>复制地址</Button>
        <label>访问密钥<Input.Password readOnly value={accessKey} /></label>
        <Button icon={<CopyOutlined />} onClick={() => void navigator.clipboard.writeText(accessKey)}>复制密钥</Button>
        <small>独立移动应用使用此地址和密钥连接实例后端（移动应用仍在开发中）。浏览器打开此地址进入 WebUI 管理台；公网连接请配置 HTTPS 或 VPN。</small>
      </div> : <Alert type="info" title="开启「允许局域网手机连接」并重启本机实例后，这里会显示连接信息。" /> },
      { key: "instance", label: "实例配置", children: <>
        <p>为当前实例配置默认模型连接；供应商密钥仅发送并保存在此实例，不写入桌面项目列表。其它编成、通道和安全策略在 Web 控制台维护。</p>
        {client && <div className="desk-model-setup">
          {error && <Alert type="error" title={error} closable onClose={() => setError("")} />}
          <label>已有模型连接<Select aria-label="已有模型连接" allowClear placeholder="添加新的模型连接" value={selected || undefined} options={profiles.map((profile) => ({ value: profile.id, label: `${profile.name} · ${profile.model}` }))} onChange={(id) => {
            setSelected(id ?? ""); const profile = profiles.find((item) => item.id === id);
            setProviderName(profile?.name ?? ""); setBaseUrl(profile?.baseUrl ?? ""); setModel(profile?.model ?? ""); setApiKey("");
          }} /></label>
          <label>连接名称<Input aria-label="连接名称" value={providerName} onChange={(e) => setProviderName(e.target.value)} /></label>
          <label>API 基址<Input aria-label="API 基址" placeholder="https://api.example.com/v1" value={baseUrl} onChange={(e) => setBaseUrl(e.target.value)} /></label>
          <label>默认模型<Input aria-label="默认模型" placeholder="模型 ID" value={model} onChange={(e) => setModel(e.target.value)} /></label>
          <label>API 密钥<Input.Password aria-label="模型 API 密钥" autoComplete="new-password" value={apiKey} onChange={(e) => setApiKey(e.target.value)} placeholder={selected ? "留空表示沿用实例中已保存的密钥" : "输入模型 API 密钥"} /></label>
          <div><Button type="primary" loading={busy} disabled={!providerName.trim() || !baseUrl.trim() || !model.trim()} onClick={() => void saveProfile()}>保存并设为默认</Button>
            <Button style={{ marginLeft: 8 }} loading={busy} disabled={!selected} onClick={() => void testProfile()}>测试连接</Button></div>
        </div>}
        {consoleUrl ? <Button onClick={() => {
          if (window.__TAURI__) void desktopInvoke("open_console", { url: consoleUrl });
          else window.open(consoleUrl, "_blank", "noopener,noreferrer");
        }}>打开实例控制台</Button> : <Alert type="info" title="请先连接实例" />}
      </> },
    ]} />
  </Modal>;
}
