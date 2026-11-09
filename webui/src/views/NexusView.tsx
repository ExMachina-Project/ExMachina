/** 智能连结网络（实验性）：外部连结体管理——远程 EXMACHINA 网关 / 本机 CLI 智能体的接入与维护 */
import React, { useCallback, useEffect, useState } from "react";
import { Alert, Button, Card, Form, Input, Modal, Popconfirm, Segmented, Select, Space, Spin, Switch, Tag, message } from "antd";
import { BrandEmpty } from "../components/BrandEmpty";
import { ApiOutlined, PlusOutlined, SettingOutlined, UsergroupDeleteOutlined } from "@ant-design/icons";
import { api, type NexusLink } from "../api";
import { PageHeader } from "../components/PageHeader";
import { NexusTopology, type TopoEdge, type TopoNode } from "../components/NexusTopology";
import { useT } from "../i18n/core";

// 连结体类别 → 资产灰阶语汇：仅 opencode（自研外部连结）保留品牌紫点缀，其余中性
const KIND_COLORS: Record<string, string | undefined> = {
  exmachina: undefined,
  opencode: "purple",
  codex: undefined,
  claude: undefined,
  custom: undefined,
};

const CLI_DEFAULTS: Record<string, string> = {
  opencode: "opencode run",
  codex: "codex exec",
  claude: "claude -p",
};

export function NexusView(): React.ReactElement {
  const t = useT();
  const [links, setLinks] = useState<NexusLink[]>([]);
  const [loading, setLoading] = useState(false);
  const [modal, setModal] = useState<{ link: NexusLink | null } | null>(null);
  const [probing, setProbing] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  // 拓扑/列表视图切换（nexus-topology-admin）：同一事实源（graph 聚合端点）
  const [view, setView] = useState<"topo" | "list">("topo");
  const [topo, setTopo] = useState<{ nodes: TopoNode[]; edges: TopoEdge[] }>({ nodes: [], edges: [] });
  const [edgeBusy, setEdgeBusy] = useState(false);
  const [form] = Form.useForm();

  const load = useCallback(async () => {
    setLoading(true);
    try {
      setLinks((await api.nexusLinks()).links);
      // 拓扑聚合失败（旧网关无端点）静默空图：列表视图不受影响，不弹错误
      try {
        setTopo(await api.nexusGraph());
      } catch {
        setTopo({ nodes: [], edges: [] });
      }
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  /** 拓扑详情面板解绑（PUT /nexus/edges unbind）——图与列表同源刷新 */
  const unbind = async (agentId: string, linkId: string) => {
    setEdgeBusy(true);
    try {
      await api.editNexusEdge("unbind", agentId, linkId);
      message.success(t("nexus.topoUnbound"));
      await load();
    } catch (e) {
      message.error(t("common.saveFailed", { err: String(e).slice(0, 140) }));
    } finally {
      setEdgeBusy(false);
    }
  };

  const submit = async () => {
    const v = await form.validateFields();
    setSaving(true);
    try {
      const r = await api.saveNexusLink({
        id: v.id.trim(),
        name: v.name.trim(),
        kind: v.kind,
        endpoint: (v.endpoint ?? "").trim(),
        apiKey: (v.apiKey ?? "").trim(),
        command: (v.command ?? "").trim(),
        enabled: v.enabled ?? true,
      });
      message.success(t("nexus.saved", { name: r.link.name }));
      setModal(null);
      await load();
    } catch (e) {
      message.error(t("common.saveFailed", { err: String(e).slice(0, 140) }));
    } finally {
      setSaving(false);
    }
  };

  const remove = async (id: string) => {
    try {
      await api.deleteNexusLink(id);
      message.success(t("nexus.deleted"));
      await load();
    } catch (e) {
      message.error(t("common.saveFailed", { err: String(e).slice(0, 140) }));
    }
  };

  const probe = async (id: string) => {
    setProbing(id);
    try {
      const r = await api.probeNexusLink(id);
      if (r.ok) {
        message.success(t("nexus.probeOk"));
      } else {
        message.error(t("nexus.probeFail", { msg: r.message.slice(0, 160) }));
      }
    } catch (e) {
      message.error(t("common.saveFailed", { err: String(e).slice(0, 140) }));
    } finally {
      setProbing(null);
    }
  };

  return (
    <div className="pane-wrap">
      <PageHeader
        en="NEXUS"
        title={t("nav.nexus")}
        desc={t("nexus.desc")}
        actions={
          <Button type="primary" icon={<PlusOutlined />} onClick={() => { setModal({ link: null }); }}>
            {t("nexus.add")}
          </Button>
        }
      />
      <Alert
        type="warning"
        showIcon
        className="nexus-exp-alert app-banner"
        title={t("nexus.expTitle")}
        description={t("nexus.expDesc")}
        style={{ marginBottom: 12 }}
      />
      <div className="pane-body">
        <Spin spinning={loading}>
          {/* 拓扑/列表切换（nexus-topology-admin）：默认拓扑（结构可读），列表保留管理操作 */}
          <Segmented
            value={view}
            onChange={(v) => setView(v as "topo" | "list")}
            options={[
              { value: "topo", label: t("nexus.viewTopo") },
              { value: "list", label: t("nexus.viewList") },
            ]}
            style={{ marginBottom: 12 }}
          />
          {view === "topo" ? (
            <NexusTopology nodes={topo.nodes} edges={topo.edges} onUnbind={(a, l) => void unbind(a, l)} busy={edgeBusy} />
          ) : links.length === 0 ? (
            <BrandEmpty description={t("nexus.empty")} className="pane-empty" />
          ) : (
            <div className="nexus-links">
              {links.map((l) => (
                <Card key={l.id} size="small" className="hud nexus-link-card">
                  <div className="nexus-link-row">
                    <Tag color={l.enabled ? KIND_COLORS[l.kind] : undefined}>{t("nexus.kind." + l.kind)}</Tag>
                    <b>{l.name}</b>
                    <span className="mono dim">{l.id}</span>
                    {(l.endpoint || l.command || CLI_DEFAULTS[l.kind]) && (
                      <span className="dim mono nexus-endpoint">{l.endpoint || l.command || CLI_DEFAULTS[l.kind]}</span>
                    )}
                    <Space size={4} style={{ marginLeft: "auto" }}>
                      {!l.enabled && <Tag>disabled</Tag>}
                      <Button
                        size="small"
                        loading={probing === l.id}
                        icon={<ApiOutlined />}
                        onClick={() => void probe(l.id)}
                        title={t("nexus.probe")}
                      />
                      <Button
                        size="small"
                        icon={<SettingOutlined />}
                        onClick={() => {
                          // forceRender 常驻挂载：编辑回填必须在打开弹窗前写入表单
                          form.setFieldsValue({ ...l, apiKey: undefined });
                          setModal({ link: l });
                        }}
                      />
                      <Popconfirm title={t("nexus.delConfirm", { name: l.name })} onConfirm={() => void remove(l.id)}>
                        <Button size="small" danger icon={<UsergroupDeleteOutlined />} />
                      </Popconfirm>
                    </Space>
                  </div>
                </Card>
              ))}
            </div>
          )}
        </Spin>
      </div>

      <Modal
        open={modal !== null}
        title={modal?.link ? t("nexus.editTitle", { name: modal.link.name }) : t("nexus.add")}
        onCancel={() => setModal(null)}
        onOk={() => void submit()}
        confirmLoading={saving}
        okText={t("common.save")}
        destroyOnHidden
        forceRender
      >
        <Form form={form} layout="vertical" initialValues={{ kind: "exmachina" }} onFinish={() => void submit()}>
          <Form.Item name="id" label={t("nexus.f.id")} rules={[{ required: true }, { pattern: /^[A-Za-z0-9_-]{1,48}$/, message: t("groups.idPattern") }]}>
            <Input placeholder={t("nexus.f.idPh")} disabled={modal?.link != null} />
          </Form.Item>
          <Form.Item name="name" label={t("nexus.f.name")} rules={[{ required: true }]}>
            <Input placeholder={t("nexus.f.namePh")} />
          </Form.Item>
          <Form.Item name="kind" label={t("nexus.f.kind")} rules={[{ required: true }]}>
            <Select
              options={[
                { value: "exmachina", label: t("nexus.kind.exmachina") },
                { value: "opencode", label: t("nexus.kind.opencode") },
                { value: "codex", label: t("nexus.kind.codex") },
                { value: "claude", label: t("nexus.kind.claude") },
                { value: "custom", label: t("nexus.kind.custom") },
              ]}
            />
          </Form.Item>
          <Form.Item noStyle shouldUpdate={(a, b) => a.kind !== b.kind}>
            {({ getFieldValue }) => {
              const kind = getFieldValue("kind") as string;
              if (kind === "exmachina") {
                return (
                  <>
                    <Form.Item name="endpoint" label={t("nexus.f.endpoint")} rules={[{ required: true, whitespace: true, message: t("nexus.f.endpointReq") }]}>
                      <Input placeholder="http://192.168.2.10:4173" />
                    </Form.Item>
                    <Form.Item name="apiKey" label={t("nexus.f.apiKey")} extra={t("nexus.f.apiKeyExtra")}>
                      <Input.Password placeholder="******" />
                    </Form.Item>
                  </>
                );
              }
              if (kind === "custom") {
                return (
                  <Form.Item name="command" label={t("nexus.f.command")} extra={t("nexus.f.commandExtra")} rules={[{ required: true, whitespace: true, message: t("nexus.f.commandReq") }]}>
                    <Input placeholder="my-agent --task" />
                  </Form.Item>
                );
              }
              const defaultCommand = CLI_DEFAULTS[kind];
              if (!defaultCommand) return null;
              return (
                <>
                  <Alert
                    type="info"
                    showIcon
                    className="nexus-cli-hint"
                    title={t("nexus.cliHint", { cmd: defaultCommand })}
                  />
                  <Form.Item
                    name="command"
                    label={t("nexus.f.commandOverride")}
                    extra={t("nexus.f.commandOverrideExtra", { cmd: defaultCommand })}
                  >
                    <Input placeholder={defaultCommand} />
                  </Form.Item>
                </>
              );
            }}
          </Form.Item>
          <Form.Item name="enabled" label={t("nexus.f.enabled")} valuePropName="checked" extra={t("nexus.f.enabledExtra")} initialValue={true}>
            <Switch />
          </Form.Item>
        </Form>
      </Modal>
    </div>
  );
}
