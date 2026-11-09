/** 智能体组详情面板（信息架构合并）：组编成管理 / 组设置 / 子个体成员一体。
 *  原独立「智能体组」页的卡片总览已并入「智能体」页统一管理（agent-groups-merge）；
 *  本组件只负责单个组的详情视图，由上层（SinglesView）传入 groupId 挂载。
 *  子个体是组的成员：成员的新建 / 编辑 / 人设 / 经验优化 / 删除都在组详情内完成。 */
import React, { useCallback, useEffect, useState } from "react";
import {
  Button, Card, Form, Input, Modal, Popconfirm, Select, Space, Spin, Switch, Table, Tag, message,
} from "antd";
import { BrandEmpty } from "../components/BrandEmpty";
import {
  ApiOutlined, ArrowLeftOutlined, DeleteOutlined, EditOutlined, FormOutlined, PlusOutlined,
  SettingOutlined, UsergroupDeleteOutlined, UserOutlined,
} from "@ant-design/icons";
import { api, type GroupCapabilities, type GroupMeta, type GroupOverview, type LlmProfile, type TemplateInfo } from "../api";
import { PageHeader } from "../components/PageHeader";
import type { AgentDefinition } from "../types";
import { buildCapabilityOptions, buildModelOptions, groupIdEn, modelLabel } from "../models";
import { useExm } from "../store";
import { useT } from "../i18n/core";

export function GroupDetailPanel({
  groupId,
  onBack,
  onDeleted,
}: {
  groupId: string;
  /** 返回统一页总览（原「组列表」返回语义） */
  onBack: () => void;
  /** 组被删除后回调（上层同步退出详情态） */
  onDeleted?: () => void;
}): React.ReactElement {
  const t = useT();
  const { groups, activeGroup, refreshAgents, loadGroups } = useExm();
  const [members, setMembers] = useState<AgentDefinition[]>([]);
  const [loading, setLoading] = useState(false);
  const [overview, setOverview] = useState<GroupOverview | null>(null);
  const [profiles, setProfiles] = useState<LlmProfile[]>([]);
  const [modelDraft, setModelDraft] = useState<string>("");
  const [agentModal, setAgentModal] = useState(false);
  const [saving, setSaving] = useState<"" | "agent" | "edit">("");
  const [templates, setTemplates] = useState<TemplateInfo[]>([]);
  const [wsDraft, setWsDraft] = useState("");
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [capsDraft, setCapsDraft] = useState<GroupCapabilities>({});
  const [agentForm] = Form.useForm();
  // 成员编辑 / 人设（原「子个体」页能力并入组详情）
  const [editing, setEditing] = useState<AgentDefinition | null>(null);
  const [editForm] = Form.useForm();
  const [editFormProfiles, setEditFormProfiles] = useState<LlmProfile[]>([]);
  const [promptLoading, setPromptLoading] = useState(false);
  // 连结体代理绑定（智能连结网络）：编辑表单下拉数据源
  const [nexusLinks, setNexusLinks] = useState<import("../api").NexusLink[]>([]);
  // 成员 SOUL.md（人设）编辑
  const [personaOf, setPersonaOf] = useState<AgentDefinition | null>(null);
  const [personaInfo, setPersonaInfo] = useState<import("../api").PersonaInfo | null>(null);
  const [personaDraft, setPersonaDraft] = useState("");
  const [personaSaving, setPersonaSaving] = useState(false);

  const meta: GroupMeta | undefined = groups.find((g) => g.id === groupId) ?? undefined;
  // 内置组的出厂人格受保护（后端禁重置：出厂 SOUL 无副本可复原）——按钮同步置灰并给出说明
  const personaProtected = meta?.builtin ?? false;
  // 主智能体（orchestrator-first）：组详情页的头等公民，独立区块呈现
  const primaryAgent = members.find((a) => a.identifier === meta?.primary) ?? null;

  const loadMembers = useCallback(async (gid: string) => {
    if (!gid) return;
    setLoading(true);
    try {
      setMembers(await api.groupAgents(gid));
      setOverview(await api.groupOverview(gid));
    } finally {
      setLoading(false);
    }
  }, []);

  // 连结体清单（编辑弹窗打开时拉取；失败静默——空清单只是没有可绑定项）
  const loadNexusLinks = useCallback(async () => {
    try {
      const r = await api.nexusLinks();
      setNexusLinks(r.links);
    } catch {
      /* 静默 */
    }
  }, []);

  useEffect(() => {
    if (groupId) {
      void loadMembers(groupId);
      // 组列表可能尚未装载（直达详情态）：确保 meta 可用
      if (!groups.length) void loadGroups();
    }
  }, [groupId, loadMembers, loadGroups, groups.length]);

  useEffect(() => {
    if (editing) void loadNexusLinks();
  }, [editing, loadNexusLinks]);

  useEffect(() => {
    void (async () => {
      try {
        const p = (await api.llmProfiles()).profiles;
        setProfiles(p);
        setEditFormProfiles(p);
      } catch { /* noop */ }
    })();
    void (async () => {
      try {
        setTemplates((await api.listTemplates()).templates);
      } catch { /* noop */ }
    })();
  }, []);

  const submitAgent = async () => {
    if (!meta) return;
    const v = await agentForm.validateFields();
    setSaving("agent");
    try {
      await api.createAgent({
        name: v.name || "",
        identifier: v.identifier,
        description: v.description || "",
        prompt: v.prompt || undefined,
        group: meta.id,
        template: v.template || undefined,
      });
      message.success(t("groups.agentCreated", { id: v.identifier }));
      setAgentModal(false);
      agentForm.resetFields();
      await loadMembers(meta.id);
      await refreshAgents();
    } catch (e) {
      message.error(t("groups.createFailed", { err: String(e) }));
    } finally {
      setSaving("");
    }
  };

  const doDeleteGroup = async (gid: string) => {
    try {
      await api.deleteGroup(gid);
      message.success(t("groups.groupDeleted", { id: gid }));
      await loadGroups();
      onDeleted?.(); // 上层同步退出详情态（卡片列表已不含该组）
    } catch (e) {
      message.error(t("groups.deleteFailed", { err: String(e) }));
    }
  };

  const doRemoveAgent = async (identifier: string) => {
    if (!meta) return;
    try {
      await api.removeGroupAgent(meta.id, identifier);
      await loadMembers(meta.id);
      await refreshAgents();
      message.success(t("groups.agentDeleted", { id: identifier }));
    } catch (e) {
      message.error(t("groups.deleteFailed", { err: String(e) }));
    }
  };

  const isBuiltin = meta?.builtin ?? true;

  // ---- 组设置（名称 / 工作区 / 默认模型 / 能力覆盖 / 自我优化） ----
  const [nameDraft, setNameDraft] = useState("");
  const [selfOptDraft, setSelfOptDraft] = useState(false);
  const openSettings = () => {
    setNameDraft(meta?.name ?? "");
    setWsDraft(meta?.workspace ?? "");
    setModelDraft(meta?.model ?? "");
    setCapsDraft(meta?.capabilities ?? {});
    setSelfOptDraft(meta?.selfOptimize ?? false);
    setSettingsOpen(true);
  };

  const saveSettings = async () => {
    if (!meta) return;
    const name = nameDraft.trim();
    if (!name) {
      message.warning(t("groups.f.nameRequired"));
      return;
    }
    try {
      // 组名随设置弹窗一并保存（group-rename 修复：此前组名无处可改）
      await api.updateGroup(meta.id, { name });
      await api.setGroupWorkspace(meta.id, wsDraft.trim());
      await api.setGroupModel(meta.id, modelDraft);
      await api.setGroupCapabilities(meta.id, capsDraft);
      if (selfOptDraft !== (meta.selfOptimize ?? false)) {
        await api.setGroupSelfOptimize(meta.id, selfOptDraft);
      }
      message.success(t("groups.settingsSaved"));
      setSettingsOpen(false);
      await loadMembers(meta.id);
      await loadGroups(); // 组名/开关同步到组卡片
    } catch (e) {
      message.error(t("common.saveFailed", { err: String(e) }));
    }
  };

  const capField = (key: keyof GroupCapabilities, label: string) => (
    <div className="cap-row" key={key}>
      <div className="cap-label">{label}</div>
      <Select
        allowClear
        showSearch
        value={capsDraft[key] || undefined}
        options={buildCapabilityOptions(profiles)}
        placeholder={t("groups.capsFollow")}
        onChange={(v) => setCapsDraft((c) => ({ ...c, [key]: v ?? "" }))}
      />
    </div>
  );

  // ---- 成员编辑 / 人设（并入原「子个体」页能力） ----
  const openPersona = async (a: AgentDefinition) => {
    setPersonaOf(a);
    setPersonaInfo(null);
    setPersonaDraft("");
    try {
      const info = await api.getPersona(a.identifier);
      setPersonaInfo(info);
      setPersonaDraft(info.persona);
    } catch (e) {
      message.error(t("agents.personaReadFailed", { err: String(e).slice(0, 120) }));
      setPersonaOf(null);
    }
  };

  const savePersona = async () => {
    if (!personaOf) return;
    setPersonaSaving(true);
    try {
      await api.putPersona(personaOf.identifier, personaDraft);
      message.success(t("agents.personaSaved"));
      setPersonaInfo(await api.getPersona(personaOf.identifier));
    } catch (e) {
      message.error(t("common.saveFailed", { err: String(e).slice(0, 120) }));
    } finally {
      setPersonaSaving(false);
    }
  };

  const resetPersona = async () => {
    if (!personaOf) return;
    try {
      await api.resetPersona(personaOf.identifier);
      setPersonaInfo(await api.getPersona(personaOf.identifier));
      setPersonaDraft("");
      message.success(t("agents.personaReset"));
    } catch (e) {
      message.error(t("agents.resetFailed", { err: String(e).slice(0, 120) }));
    }
  };

  const openEdit = async (a: AgentDefinition) => {
    setEditing(a);
    editForm.setFieldsValue({
      name: a.name,
      description: a.description,
      modelHint: a.modelHint ?? undefined,
      link: a.link ?? undefined,
      prompt: "",
    });
    setPromptLoading(true);
    try {
      const info = await api.getAgentPrompt(meta!.id, a.identifier);
      editForm.setFieldsValue({ prompt: info.prompt });
    } catch {
      editForm.setFieldsValue({ prompt: "" });
      message.info(t("agents.promptMissing"));
    } finally {
      setPromptLoading(false);
    }
  };

  const submitEdit = async () => {
    if (!editing || !meta) return;
    const v = await editForm.validateFields();
    setSaving("edit");
    try {
      await api.updateAgent(editing.identifier, {
        name: v.name,
        description: v.description,
        modelHint: v.modelHint ?? "",
        link: v.link ?? "",
      });
      if ((v.prompt ?? "").trim()) {
        await api.putAgentPrompt(meta.id, editing.identifier, v.prompt);
      }
      message.success(t("agents.editSaved", { name: v.name }));
      setEditing(null);
      await loadMembers(meta.id);
      await refreshAgents();
    } catch (e) {
      message.error(t("common.saveFailed", { err: String(e) }));
    } finally {
      setSaving("");
    }
  };

  // 在途个体（编排图运行中）：成员表状态列
  const busyAgents = new Set(
    (useExm((s) => s.graph)?.nodes ?? [])
      .filter((n) => ["running", "dispatched", "syncing"].includes(n.status))
      .map((n) => n.agentIdentifier),
  );

  // 组不存在（被删/直达失效）：空态兜底
  if (!meta) {
    return (
      <div className="pane-wrap">
        <PageHeader
          en="GROUP"
          title={t("nav.groups")}
          desc={
            <Space size={6} wrap>
              <Button size="small" icon={<ArrowLeftOutlined />} onClick={onBack}>
                {t("groups.back")}
              </Button>
            </Space>
          }
        />
        <BrandEmpty description={t("groups.none")} className="pane-empty" />
      </div>
    );
  }

  return (
    <div className="pane-wrap">
      <PageHeader
        en="GROUP"
        title={meta.name}
        desc={
          <Space size={6} wrap>
            <Button size="small" icon={<ArrowLeftOutlined />} onClick={onBack}>
              {t("groups.back")}
            </Button>
            <span className="mono dim">{groupIdEn(meta.id)}</span>
            {meta.builtin ? <Tag>{t("groups.builtinProtected")}</Tag> : <Tag color="purple">{t("groups.customGroup")}</Tag>}
            {meta.id === activeGroup && <Tag>{t("groups.activeTag")}</Tag>}
          </Space>
        }
        actions={
          <Space>
            <Button icon={<SettingOutlined />} onClick={() => { openSettings(); }}>
              {t("groups.settings")}
            </Button>
            <Popconfirm
              title={t("groups.groupDeleteConfirm", { name: meta.name })}
              disabled={isBuiltin || meta.id === activeGroup}
              onConfirm={() => void doDeleteGroup(meta.id)}
            >
              <Button danger icon={<UsergroupDeleteOutlined />} disabled={isBuiltin || meta.id === activeGroup}>
                {t("groups.del")}
              </Button>
            </Popconfirm>
            {!isBuiltin && (
              <Button type="primary" icon={<PlusOutlined />} onClick={() => setAgentModal(true)}>
                {t("groups.newAgent")}
              </Button>
            )}
          </Space>
        }
      />

      <div className="pane-body">
        {overview && (
          <div className="overview-chips">
            <div className="chip"><span className="chip-num">{overview.agents}</span><span className="chip-label">{t("groups.chipAgents")}</span></div>
            <div className="chip"><span className="chip-num">{overview.playbooks}</span><span className="chip-label">{t("groups.chipPlaybooks")}</span></div>
            <div className="chip"><span className="chip-num">{overview.sessions}</span><span className="chip-label">{t("groups.chipSessions")}</span></div>
            <div className="chip"><span className="chip-num">{overview.memory.group}</span><span className="chip-label">{t("groups.chipGroupMem")}</span></div>
            <div className="chip"><span className="chip-num">{overview.memory.shared}</span><span className="chip-label">{t("groups.chipShared")}</span></div>
          </div>
        )}
        {overview && overview.agentStats.length > 0 && (
          <div className="member-stats">
            <div className="rail-label">{t("groups.reliability")}</div>
            {overview.agentStats.slice(0, 6).map((s) => (
              <div key={s.agentId} className="member-stat-row">
                <span className="mono">{s.agentId}</span>
                <span className="dim">
                  {t("groups.stat", { runs: s.runs, done: s.done, blocked: s.blocked, conf: s.avgConfidence.toFixed(2) })}
                </span>
              </div>
            ))}
          </div>
        )}

        {/* 主智能体独立区块（orchestrator-first）：与用户对话、对交付负责的个体。
            大部分情况只需要修改主智能体——单独置顶呈现全功能入口。 */}
        {primaryAgent && (
          <Card
            size="small"
            className="hud"
            title={
              <Space>
                <UserOutlined />
                <span>{t("groups.primaryTitle")}</span>
                <span className="mono dim">{primaryAgent.identifier}</span>
                {(meta.selfOptimize ?? false) && (
                  <Tag color="geekblue">
                    <ApiOutlined /> {t("groups.selfOptTag")}
                  </Tag>
                )}
              </Space>
            }
          >
            <div className="primary-panel">
              <div className="primary-desc dim">{primaryAgent.description}</div>
              <div className="skill-line">
                {t("singles.model")}<Tag>{modelLabel(primaryAgent.modelHint, profiles)}</Tag>
                <span className="dim" style={{ marginLeft: 12 }}>{t("groups.primaryHint")}</span>
              </div>
              <Space style={{ marginTop: 10 }}>
                <Button size="small" icon={<FormOutlined />} onClick={() => void openEdit(primaryAgent)}>
                  {t("agents.edit")}
                </Button>
                <Button size="small" icon={<EditOutlined />} onClick={() => void openPersona(primaryAgent)}>
                  {t("agents.soulBtn")}
                </Button>
              </Space>
            </div>
          </Card>
        )}

        <Card size="small" className="hud" title={t("groups.unitsTitle")}>
          <Spin spinning={loading}>
            <Table
              size="small"
              rowKey="identifier"
              pagination={false}
              tableLayout="fixed"
              dataSource={[...members].filter((a) => a.identifier !== meta.primary)}
              columns={[
                {
                  title: t("agents.colUnit"),
                  key: "name",
                  width: 220,
                  render: (_, a: AgentDefinition) => (
                    <span className="member-cell">
                      <span className="member-line1">
                        <b>{a.name}</b>
                        <code className="member-id">{a.identifier}</code>
                      </span>
                    </span>
                  ),
                },
                { title: t("agents.colDuty"), dataIndex: "description", key: "desc", ellipsis: true },
                {
                  title: t("agents.colTools"),
                  dataIndex: "tools",
                  key: "tools",
                  width: 300,
                  // 生效工具面（agent-tool-surface）：声明清单 Tag 展示；
                  // 空清单 = 未声明（默认全量），显式标注而非留白
                  render: (tools: string[]) =>
                    tools?.length
                      ? tools.map((tl) => <Tag key={tl}>{tl}</Tag>)
                      : <Tag color="geekblue">{t("agents.toolsFull")}</Tag>,
                },
                {
                  title: t("agents.colState"),
                  key: "state",
                  width: 72,
                  render: (_, a: AgentDefinition) =>
                    busyAgents.has(a.identifier) ? <Tag color="processing">{t("agents.busy")}</Tag> : <Tag>{t("agents.idle")}</Tag>,
                },
                {
                  title: t("agents.colOps"),
                  key: "ops",
                  width: 220,
                  render: (_, a: AgentDefinition) => (
                    <Space size={4}>
                      <Button size="small" icon={<FormOutlined />} onClick={() => void openEdit(a)}>
                        {t("agents.edit")}
                      </Button>
                      <Button size="small" icon={<EditOutlined />} onClick={() => void openPersona(a)}>
                        {t("agents.soulBtn")}
                      </Button>
                      {!isBuiltin && meta.primary !== a.identifier && (
                        <Popconfirm title={t("groups.agentDeleteConfirm")} onConfirm={() => void doRemoveAgent(a.identifier)}>
                          <Button size="small" danger icon={<DeleteOutlined />} />
                        </Popconfirm>
                      )}
                    </Space>
                  ),
                },
              ]}
            />
            {members.filter((a) => a.identifier !== meta.primary).length === 0 && (
              <BrandEmpty description={t("groups.emptyUnits")} className="pane-empty" />
            )}
          </Spin>
        </Card>
      </div>

      <Modal
        open={agentModal}
        title={t("groups.modalNewAgent", { name: meta.name })}
        onCancel={() => setAgentModal(false)}
        onOk={() => void submitAgent()}
        confirmLoading={saving === "agent"}
        okText={t("groups.create")}
        destroyOnHidden
      >
        <Form form={agentForm} layout="vertical" onFinish={() => void submitAgent()}>
          <Form.Item name="template" label={t("groups.f.template")} initialValue="">
            <Select
              allowClear
              options={[
                { value: "", label: t("groups.f.templateBlank") },
                ...templates.filter((x) => x.kind === "unit").map((x) => ({ value: x.id, label: x.name })),
              ]}
            />
          </Form.Item>
          <Form.Item name="name" label={t("groups.f.agentName")} rules={[{ required: false }]}>
            <Input placeholder={t("groups.f.agentNamePh")} />
          </Form.Item>
          <Form.Item name="identifier" label={t("groups.f.identifier")} rules={[{ required: true }, { pattern: /^[A-Za-z0-9_-]{1,48}$/, message: t("groups.idPattern") }]}>
            <Input placeholder={t("groups.f.identifierPh")} />
          </Form.Item>
          <Form.Item name="description" label={t("groups.duty")} rules={[{ required: false }]}>
            <Input.TextArea rows={2} placeholder={t("groups.f.dutyPh")} />
          </Form.Item>
          <Form.Item name="prompt" label={t("agents.f.prompt")} extra={t("agents.f.promptExtra")}>
            <Input.TextArea rows={4} placeholder={t("agents.f.promptPh")} />
          </Form.Item>
        </Form>
      </Modal>

      {/* 成员编辑弹窗 */}
      <Modal
        open={editing !== null}
        title={editing ? t("agents.editTitle", { name: editing.name }) : ""}
        onCancel={() => setEditing(null)}
        onOk={() => void submitEdit()}
        confirmLoading={saving === "edit"}
        okText={t("common.save")}
        destroyOnHidden
        width={620}
      >
        <Form form={editForm} layout="vertical" onFinish={() => void submitEdit()}>
          <Form.Item name="name" label={t("agents.f.name")} rules={[{ required: true }]}>
            <Input />
          </Form.Item>
          <Form.Item name="description" label={t("agents.f.duty")} rules={[{ required: true }]}>
            <Input.TextArea rows={2} />
          </Form.Item>
          <Spin spinning={promptLoading}>
            <Form.Item
              name="prompt"
              label={t("agents.f.promptTitle")}
              extra={t("agents.f.promptEditExtra")}
              rules={[{ required: true, whitespace: true, message: t("agents.f.promptRequired") }]}
            >
              <Input.TextArea rows={12} className="prompt-editor" spellCheck={false} />
            </Form.Item>
          </Spin>
          <Form.Item name="modelHint" label={t("agents.f.model")} extra={t("agents.f.modelExtra")}>
            <Select
              allowClear
              showSearch
              style={{ width: "100%" }}
              options={buildModelOptions(editFormProfiles)}
              placeholder={t("agents.f.modelPh")}
            />
          </Form.Item>
          <Form.Item name="link" label={t("agents.f.link")} extra={t("agents.f.linkExtra")}>
            <Select
              allowClear
              style={{ width: "100%" }}
              options={nexusLinks.map((l) => ({ value: l.id, label: `${l.name}（${l.id}）`, disabled: !l.enabled }))}
              placeholder={t("agents.f.linkPh")}
              notFoundContent={t("agents.f.linkEmpty")}
            />
          </Form.Item>
        </Form>
      </Modal>

      {/* 成员 SOUL.md 编辑弹窗 */}
      <Modal
        open={personaOf !== null}
        title={personaOf ? t("agents.soulTitle", { name: personaOf.name }) : ""}
        onCancel={() => setPersonaOf(null)}
        width={640}
        footer={
          <Space>
            <Popconfirm
              title={t("agents.resetConfirm")}
              onConfirm={() => void resetPersona()}
              disabled={personaProtected || (personaInfo ? !personaInfo.custom : false)}
            >
              <Button disabled={personaProtected || (personaInfo ? !personaInfo.custom : false)}>{t("agents.soulReset")}</Button>
            </Popconfirm>
            <Button onClick={() => setPersonaOf(null)}>{t("common.close")}</Button>
            <Button type="primary" loading={personaSaving} onClick={() => void savePersona()}>
              {t("agents.saveHot")}
            </Button>
          </Space>
        }
      >
        {personaInfo && (
          <div style={{ marginBottom: 8 }}>
            {personaInfo.custom ? <Tag color="purple">{t("agents.soulCustom")}</Tag> : <Tag>{t("agents.soulDefault")}</Tag>}
          </div>
        )}
        {personaProtected && <div className="persona-hint">{t("agents.soulProtectedHint")}</div>}
        <Input.TextArea
          rows={14}
          className="prompt-editor"
          spellCheck={false}
          value={personaDraft}
          onChange={(e) => setPersonaDraft(e.target.value)}
        />
      </Modal>

      {/* 组设置弹窗 */}
      <Modal
        open={settingsOpen}
        title={t("groups.modalSettings", { name: meta.name })}
        onCancel={() => setSettingsOpen(false)}
        onOk={() => void saveSettings()}
        okText={t("common.save")}
        width={520}
      >
        <Form layout="vertical">
          <Form.Item label={t("groups.f.name")} help={t("groups.f.nameHelp")} style={{ marginBottom: 12 }}>
            <Input value={nameDraft} onChange={(e) => setNameDraft(e.target.value)} placeholder={t("groups.f.namePh")} />
          </Form.Item>
          <Form.Item label={t("groups.f.workspace")} help={t("groups.f.workspaceHelp")}>
            <Input value={wsDraft} onChange={(e) => setWsDraft(e.target.value)} placeholder={t("groups.f.wsPh")} />
          </Form.Item>
          <Form.Item label={t("groups.f.model")} help={t("groups.f.modelHelp")}>
            <Select value={modelDraft || undefined} onChange={setModelDraft} options={buildModelOptions(profiles)} allowClear />
          </Form.Item>
          <Form.Item label={t("groups.selfOptLabel")} help={t("groups.selfOptHint")} style={{ marginBottom: 8 }}>
            <Space align="center">
              <Switch checked={selfOptDraft} onChange={setSelfOptDraft} />
              <span className="dim">{selfOptDraft ? t("common.on") : t("common.off")}</span>
            </Space>
          </Form.Item>
          <Form.Item label={t("groups.capsTitle")} help={t("groups.capsHint")} style={{ marginBottom: 8 }}>
            <div className="cap-grid" style={{ gap: "8px 14px" }}>
              {capField("speech", t("models.capSpeech"))}
              {capField("transcribe", t("models.capStt"))}
              {capField("visionRelay", t("models.capVisionRelay"))}
              {capField("embedding", t("models.capEmbed"))}
            </div>
          </Form.Item>
        </Form>
      </Modal>
    </div>
  );
}
