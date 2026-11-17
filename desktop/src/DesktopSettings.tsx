import { lazy, Suspense, useEffect, useState } from "react";
import { Alert, App, Button, Form, Input, Modal, Spin, Switch, Tabs } from "antd";
import { desktopInvoke, type DesktopConfig } from "../../webui/src/desktop";
import { canLeaveSettings } from "../../webui/src/settingsNavigation";
const Models = lazy(() => import("../../webui/src/views/models/ModelsView").then((m) => ({ default: m.ModelsView })));
const Agents = lazy(() => import("../../webui/src/views/SinglesView").then((m) => ({ default: m.SinglesView })));
const Channels = lazy(() => import("../../webui/src/views/channels/ChannelsView").then((m) => ({ default: m.ChannelsView })));
const Settings = lazy(() => import("../../webui/src/views/settings/SettingsView").then((m) => ({ default: m.SettingsView })));
const Skills = lazy(() => import("../../webui/src/views/SkillsView").then((m) => ({ default: m.SkillsView })));
const Plugins = lazy(() => import("../../webui/src/views/PluginsView").then((m) => ({ default: m.PluginsView })));
const Memory = lazy(() => import("../../webui/src/views/MemoryView").then((m) => ({ default: m.MemoryView })));
const Automations = lazy(() => import("../../webui/src/views/AutomationsView").then((m) => ({ default: m.AutomationsView })));
const Sessions = lazy(() => import("../../webui/src/views/SessionsView").then((m) => ({ default: m.SessionsView })));
const Approvals = lazy(() => import("../../webui/src/views/ApprovalsView").then((m) => ({ default: m.ApprovalsView })));
const Knowledge = lazy(() => import("../../webui/src/views/KnowledgeView").then((m) => ({ default: m.KnowledgeView })));
const Activity = lazy(() => import("../../webui/src/views/ActivityView").then((m) => ({ default: m.ActivityView })));

function NativeSettings() {
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
    <Form.Item name="exitOnClose" label="关闭窗口时退出应用" extra="关闭此选项后，窗口关闭时会驻留托盘。" valuePropName="checked"><Switch /></Form.Item>
    <Form.Item name="shortcut" label="唤起 / 隐藏快捷键" rules={[{ required: true }]}><Input /></Form.Item>
    <Form.Item name="notifyApproval" label="审批通知" valuePropName="checked"><Switch /></Form.Item>
    <Form.Item name="notifyCron" label="自动化通知" valuePropName="checked"><Switch /></Form.Item>
    <Form.Item name="notifyTaskDone" label="任务完成通知" valuePropName="checked"><Switch /></Form.Item>
    <Button htmlType="submit" loading={busy} type="primary">保存桌面设置</Button>
  </Form>;
}
export function DesktopSettings({ open, onClose }: { open: boolean; onClose: () => void }) {
  const [active, setActive] = useState("model");
  return <Modal open={open} onCancel={() => { if (canLeaveSettings()) onClose(); }} footer={null} title="工作区设置" width={1180} className="desk-settings" destroyOnHidden>
    <Suspense fallback={<Spin />}><Tabs activeKey={active} onChange={setActive} items={[
      { key: "model", label: "模型", children: <Models active={active === "model"} /> },
      { key: "agent", label: "智能体", children: <Agents active={active === "agent"} /> },
      { key: "native", label: "桌面", children: <NativeSettings /> },
      { key: "skills", label: "技能", children: <Skills /> },
      { key: "plugins", label: "插件", children: <Plugins /> },
      { key: "sessions", label: "会话", children: <Sessions /> },
      { key: "approvals", label: "审批", children: <Approvals /> },
      { key: "memory", label: "记忆", children: <Memory /> },
      { key: "knowledge", label: "知识", children: <Knowledge /> },
      { key: "automation", label: "自动化", children: <Automations /> },
      { key: "channels", label: "连结", children: <Channels /> },
      { key: "config", label: "高级", children: <Settings /> },
      { key: "activity", label: "审计", children: <Activity /> },
    ]} /></Suspense>
  </Modal>;
}
