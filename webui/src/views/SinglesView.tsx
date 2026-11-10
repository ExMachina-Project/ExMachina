/** 智能体页（信息架构合并）：独立智能体 + 智能体组 统一管理——
 *  独立智能体卡片网格 + 组卡片网格；点组卡进入组详情（原「智能体组」页详情视图，
 *  见 GroupsView 的 GroupDetailPanel）。原独立组页的卡片总览废止（agent-groups-merge）。 */
import React, { useCallback, useEffect, useState } from "react";
import { Button, Card, Dropdown, Form, Input, Modal, Popconfirm, Select, Space, Spin, Switch, Tag, message } from "antd";
import { BrandEmpty } from "../components/BrandEmpty";
import { ApiOutlined, MoreOutlined, PlusOutlined, ReloadOutlined, TeamOutlined, UndoOutlined, UserOutlined } from "@ant-design/icons";
import { api, type GroupMeta, type LlmProfile, type PersonaInfo } from "../api";
import { PageHeader } from "../components/PageHeader";
import { SkeletonList } from "../components/SkeletonList";
import { GroupDetailPanel } from "./GroupsView";
import { buildModelOptions, groupIdEn, modelLabel } from "../models";
import { useExm } from "../store";
import { useT } from "../i18n/core";

interface SingleInfo {
  identifier: string;
  name: string;
  description: string;
  domain: string;
  capabilities?: string[];
  modelHint?: string | null;
}

export function SinglesView(): React.ReactElement {
  const t = useT();
  const { groups, loadGroups } = useExm();
  const [singles, setSingles] = useState<SingleInfo[]>([]);
  const [profiles, setProfiles] = useState<LlmProfile[]>([]);
  const [loading, setLoading] = useState(false);
  const [modal, setModal] = useState(false);
  const [saving, setSaving] = useState(false);
  const [modelTarget, setModelTarget] = useState<SingleInfo | null>(null);
  const [modelDraft, setModelDraft] = useState<string>("");
  const [descTarget, setDescTarget] = useState<SingleInfo | null>(null);
  const [descDraft, setDescDraft] = useState<string>("");
  /** 已进入的组详情（信息架构合并）：非空 = 整页渲染 GroupDetailPanel */
  const [enteredGroup, setEnteredGroup] = useState<string | null>(null);
  // 组卡片菜单（group-card-menu）：编辑信息（改名/改 id）/ 自我优化开关
  const [groupEditTarget, setGroupEditTarget] = useState<GroupMeta | null>(null);
  const [groupEditForm] = Form.useForm();
  const [groupEditSaving, setGroupEditSaving] = useState(false);
  const [groupOptTarget, setGroupOptTarget] = useState<GroupMeta | null>(null);
  const [groupOptSaving, setGroupOptSaving] = useState(false);

  // 连结体代理绑定（智能连结网络）：创建表单下拉数据源
  const [nexusLinks, setNexusLinks] = useState<import("../api").NexusLink[]>([]);
  const [soulOf, setSoulOf] = useState<SingleInfo | null>(null);
  const [soulInfo, setSoulInfo] = useState<PersonaInfo | null>(null);
  const [soulDraft, setSoulDraft] = useState<string>("");
  const [soulSaving, setSoulSaving] = useState(false);
  const [form] = Form.useForm();

  const load = useCallback(async () => {
    setLoading(true);
    try {
      const r = await api.listSingles();
      setSingles(r.singles);
      setProfiles((await api.llmProfiles()).profiles);
      // 组清单（store 全局态）：统一页的组卡片段用
      void loadGroups().catch(() => undefined);
      // 连结体清单（失败静默——空清单只是没有可绑定项）
      try {
        setNexusLinks((await api.nexusLinks()).links);
      } catch {
        /* 静默 */
      }
    } finally {
      setLoading(false);
    }
  }, [loadGroups]);

  useEffect(() => {
    void load();
  }, [load]);

  const submit = async () => {
    const v = await form.validateFields();
    setSaving(true);
    try {
      await api.createSingle({
        name: v.name,
        identifier: v.identifier,
        description: v.description,
        prompt: v.prompt || undefined,
        link: v.link || undefined,
      });
      message.success(t("singles.created", { id: v.identifier }));
      setModal(false);
      form.resetFields();
      await load();
    } catch (e) {
      message.error(t("singles.createFailed", { err: String(e) }));
    } finally {
      setSaving(false);
    }
  };

  const remove = async (id: string) => {
    try {
      await api.deleteSingle(id);
      message.success(t("singles.removed", { id }));
      await load();
    } catch (e) {
      message.error(t("singles.deleteFailed", { err: String(e) }));
    }
  };

  const saveModel = async () => {
    if (!modelTarget) return;
    try {
      await api.setAgentModel(modelTarget.identifier, modelDraft);
      message.success(t("singles.modelUpdated", { name: modelTarget.name }));
      setModelTarget(null);
      await load();
    } catch (e) {
      message.error(t("singles.setFailed", { err: String(e) }));
    }
  };

  const saveDesc = async () => {
    if (!descTarget) return;
    if (!descDraft.trim()) {
      message.warning(t("singles.f.descPh"));
      return;
    }
    try {
      await api.updateSingle(descTarget.identifier, { description: descDraft.trim() });
      message.success(t("singles.descSaved", { name: descTarget.name }));
      setDescTarget(null);
      await load();
    } catch (e) {
      message.error(t("common.saveFailed", { err: String(e) }));
    }
  };

  // 改名（显示名 name；identifier 不可改）：智能体页此前无改名入口，
  // 默认智能体（machina）也一并放行——后端 update_agent 对内置体无限制（部署实测反馈）
  const [renameTarget, setRenameTarget] = useState<SingleInfo | null>(null);
  const [renameDraft, setRenameDraft] = useState("");
  const saveRename = async () => {
    if (!renameTarget) return;
    const name = renameDraft.trim();
    if (!name) {
      message.warning(t("singles.f.nameRequired"));
      return;
    }
    try {
      await api.updateSingle(renameTarget.identifier, { name });
      message.success(t("singles.renamedOne", { name }));
      setRenameTarget(null);
      await load();
    } catch (e) {
      message.error(t("common.saveFailed", { err: String(e) }));
    }
  };

  const [promptOf, setPromptOf] = useState<SingleInfo | null>(null);
  const [promptDraft, setPromptDraft] = useState("");
  const [promptSaving, setPromptSaving] = useState(false);

  // ---- 组卡片菜单（group-card-menu）----
  /** 组信息保存：改名 + 可选改 id（后端迁移会话/记忆/任务/触发器引用），
   *  并一并保存工作区与组默认模型（基本设置不再需要进组详情才能改）。 */
  const saveGroupEdit = async () => {
    if (!groupEditTarget) return;
    const v = await groupEditForm.validateFields();
    const name = (v.name ?? "").trim();
    const nid = (v.identifier ?? "").trim();
    if (!name) {
      message.warning(t("groups.f.nameRequired"));
      return;
    }
    setGroupEditSaving(true);
    try {
      const meta = await api.updateGroup(groupEditTarget.id, {
        name,
        id: nid && nid !== groupEditTarget.id ? nid : undefined,
      });
      // 改 id 已由后端迁移引用：后续设置按新 id 落盘
      const targetId = meta.id;
      // 中途失败重试时也用最新 id/名称（避免改 id 成功后设置失败，重试仍打旧 id）
      setGroupEditTarget(meta);
      const workspace = (v.workspace ?? "").trim();
      if (workspace !== (groupEditTarget.workspace ?? "").trim()) {
        await api.setGroupWorkspace(targetId, workspace);
      }
      const model = v.model ?? "";
      if (model !== (groupEditTarget.model ?? "")) {
        await api.setGroupModel(targetId, model);
      }
      message.success(t("groups.infoSaved", { name: meta.name }));
      setGroupEditTarget(null);
      if (enteredGroup === groupEditTarget.id) {
        setEnteredGroup(targetId); // 改 id 后详情态跟随新 id
      }
      await loadGroups();
    } catch (e) {
      message.error(t("common.saveFailed", { err: String(e) }));
    } finally {
      setGroupEditSaving(false);
    }
  };

  /** 自我优化开关保存（group-self-optimize）：提交 Switch 的当前草稿值。
   *  注意不能取反——Switch 的 onChange 已把 groupOptTarget.selfOptimize 改成目标值，
   *  取反会导致「关掉后保存又变回开启」。 */
  const toggleGroupOpt = async () => {
    if (!groupOptTarget) return;
    const enabling = groupOptTarget.selfOptimize ?? false;
    setGroupOptSaving(true);
    try {
      await api.setGroupSelfOptimize(groupOptTarget.id, enabling);
      message.success(t(enabling ? "groups.selfOptOn" : "groups.selfOptOff"));
      setGroupOptTarget(null);
      await loadGroups();
    } catch (e) {
      message.error(t("common.saveFailed", { err: String(e) }));
    } finally {
      setGroupOptSaving(false);
    }
  };

  const groupCardMenu = (g: GroupMeta) => [
    {
      key: "edit",
      label: t("groups.menuEditInfo"),
      onClick: () => {
        setGroupEditTarget(g);
        groupEditForm.setFieldsValue({
          name: g.name,
          identifier: g.id,
          workspace: g.workspace ?? "",
          model: g.model || undefined,
        });
      },
    },
    {
      key: "selfOpt",
      label: t("groups.menuSelfOpt"),
      icon: <ApiOutlined />,
      onClick: () => setGroupOptTarget(g),
    },
  ];

  const openPrompt = async (s: SingleInfo) => {
    setPromptOf(s);
    setPromptDraft("");
    try {
      const info = await api.getSinglePrompt(s.identifier);
      setPromptDraft(info.prompt);
    } catch {
      message.info(t("agents.promptMissing"));
      setPromptOf(null);
      return;
    }
  };

  const savePrompt = async () => {
    if (!promptOf) return;
    setPromptSaving(true);
    try {
      await api.putSinglePrompt(promptOf.identifier, promptDraft);
      message.success(t("agents.promptSaved"));
      setPromptOf(null);
    } catch (e) {
      message.error(t("common.saveFailed", { err: String(e) }));
    } finally {
      setPromptSaving(false);
    }
  };

  const openSoul = async (s: SingleInfo) => {
    setSoulOf(s);
    setSoulInfo(null);
    setSoulDraft("");
    try {
      const info = await api.singlePersona(s.identifier);
      setSoulInfo(info);
      setSoulDraft(info.persona);
    } catch (e) {
      message.error(t("agents.personaReadFailed", { err: String(e) }));
      setSoulOf(null);
    }
  };

  const saveSoul = async () => {
    if (!soulOf) return;
    setSoulSaving(true);
    try {
      await api.singlesSetPersona(soulOf.identifier, soulDraft);
      message.success(t("singles.soulSaved"));
      setSoulInfo(await api.singlePersona(soulOf.identifier));
    } catch (e) {
      message.error(t("common.saveFailed", { err: String(e) }));
    } finally {
      setSoulSaving(false);
    }
  };

  const resetSoul = async () => {
    if (!soulOf) return;
    try {
      await api.singlesResetPersona(soulOf.identifier);
      setSoulInfo(await api.singlePersona(soulOf.identifier));
      setSoulDraft((await api.singlePersona(soulOf.identifier)).persona);
      message.success(t("agents.resetBtn"));
    } catch (e) {
      message.error(t("common.saveFailed", { err: String(e) }));
    }
  };

  const [delTarget, setDelTarget] = useState<SingleInfo | null>(null);

  const cardMenu = (s: SingleInfo) => [
    {
      key: "rename",
      label: t("common.rename"),
      onClick: () => {
        setRenameTarget(s);
        setRenameDraft(s.name);
      },
    },
    {
      key: "model",
      label: t("singles.menuModel"),
      onClick: () => {
        setModelTarget(s);
        setModelDraft(s.modelHint ?? "");
      },
    },
    {
      key: "desc",
      label: t("singles.menuDesc"),
      onClick: () => {
        setDescTarget(s);
        setDescDraft(s.description);
      },
    },
    {
      key: "prompt",
      label: t("singles.menuPrompt"),
      onClick: () => void openPrompt(s),
    },
    {
      key: "soul",
      label: t("singles.menuSoul"),
      onClick: () => void openSoul(s),
    },
    { type: "divider" as const },
    {
      key: "del",
      label: t("common.delete"),
      danger: true,
      onClick: () => setDelTarget(s),
    },
  ];

  // 组详情态：整页替换为组详情面板（原「智能体组」页详情视图）
  if (enteredGroup) {
    return (
      <GroupDetailPanel
        groupId={enteredGroup}
        onBack={() => setEnteredGroup(null)}
        onDeleted={() => setEnteredGroup(null)}
      />
    );
  }

  return (
    <div className="pane-wrap">
      <PageHeader
        en="AGENTS"
        title={t("nav.agent")}
        desc={t("singles.desc")}
        actions={
          <>
            <Button type="primary" icon={<PlusOutlined />} onClick={() => setModal(true)}>
              {t("singles.new")}
            </Button>
            <Button icon={<ReloadOutlined />} onClick={() => void load()}>
              {t("common.refresh")}
            </Button>
          </>
        }
      />

      <Spin spinning={loading}>
        {loading && singles.length === 0 ? (
          <SkeletonList rows={4} variant="card" />
        ) : (
          <>
            <div className="rail-label" style={{ marginBottom: 10 }}>{t("singles.sectionSingles")}</div>
            <div className="skill-grid">
              {singles.map((s) => (
                <Card
                  key={s.identifier}
                  size="small"
                  className="hud model-card"
                  title={
                    <Space>
                      <UserOutlined />
                      <span>{s.name}</span>
                      <span className="mono dim">{s.identifier}</span>
                    </Space>
                  }
                  extra={
                    <Dropdown menu={{ items: cardMenu(s) }} trigger={["click"]} placement="bottomRight">
                      <Button size="small" icon={<MoreOutlined />} aria-label={t("singles.cardMenu")} />
                    </Dropdown>
                  }
                >
                  <div className="dim">{s.description}</div>
                  <div className="skill-line">{t("singles.domain", { domain: s.domain || t("singles.solo") })}</div>
                  <div className="skill-line">
                    {t("singles.model")}<Tag>{modelLabel(s.modelHint, profiles)}</Tag>
                  </div>
                </Card>
              ))}
            </div>
            {!loading && singles.length === 0 && (
              <BrandEmpty description={t("singles.empty")} className="graph-empty" />
            )}

            {/* 智能体组段（信息架构合并）：点卡片进入组详情；三点菜单直接改组信息 */}
            <div className="rail-label" style={{ margin: "18px 0 10px" }}>{t("singles.sectionGroups")}</div>
            <div className="skill-grid">
              {groups.map((g) => (
                <Card
                  key={g.id}
                  size="small"
                  className="hud model-card group-card"
                  hoverable
                  onClick={() => setEnteredGroup(g.id)}
                  title={
                    <Space>
                      <TeamOutlined />
                      <span>{g.name}</span>
                      <span className="mono dim">{groupIdEn(g.id)}</span>
                    </Space>
                  }
                  extra={
                    <Dropdown
                      menu={{ items: groupCardMenu(g) }}
                      trigger={["click"]}
                      placement="bottomRight"
                      // 菜单在 portal 中渲染，React 事件仍会沿组件树冒泡到卡片 onClick；
                      // 在弹层根部截断，避免点「编辑组信息 / 自我优化」时误进入组详情。
                      popupRender={(menu) => <div onClick={(e) => e.stopPropagation()}>{menu}</div>}
                    >
                      <Button
                        size="small"
                        icon={<MoreOutlined />}
                        aria-label={t("singles.cardMenu")}
                        onClick={(e) => e.stopPropagation()}
                      />
                    </Dropdown>
                  }
                >
                  <div className="dim">{g.description}</div>
                  {(g.selfOptimize ?? false) && (
                    <div className="skill-line">
                      <Tag color="geekblue">
                        <ApiOutlined /> {t("groups.selfOptTag")}
                      </Tag>
                    </div>
                  )}
                </Card>
              ))}
            </div>
            {!loading && groups.length === 0 && (
              <BrandEmpty description={t("groups.none")} className="graph-empty" />
            )}
          </>
        )}
      </Spin>

      {/* 切换模型（跟随全局 / 指定提供商与模型）——卡片菜单唯一默认模型入口 */}
      <Modal
        open={modelTarget !== null}
        title={t("singles.modelTitle", { name: modelTarget?.name ?? "" })}
        onCancel={() => setModelTarget(null)}
        onOk={() => void saveModel()}
        okText={t("common.save")}
        width={480}
      >
        <div className="pane-hint" style={{ marginBottom: 12 }}>
          {t("singles.modelHint")}
        </div>
        <Select
          value={modelDraft}
          onChange={setModelDraft}
          style={{ width: "100%" }}
          options={buildModelOptions(profiles)}
        />
      </Modal>

      {/* 编辑描述（轻量：仅简介字段） */}
      <Modal
        open={descTarget !== null}
        title={descTarget ? t("singles.descTitle", { name: descTarget.name }) : ""}
        onCancel={() => setDescTarget(null)}
        onOk={() => void saveDesc()}
        okText={t("common.save")}
        width={480}
      >
        <Input.TextArea
          rows={3}
          value={descDraft}
          onChange={(e) => setDescDraft(e.target.value)}
          placeholder={t("singles.f.descPh")}
        />
      </Modal>

      {/* 改名（显示名）：所有智能体含默认智能体均可改，identifier 恒定 */}
      <Modal
        open={renameTarget !== null}
        title={renameTarget ? t("common.rename") : ""}
        onCancel={() => setRenameTarget(null)}
        onOk={() => void saveRename()}
        okText={t("common.save")}
        width={480}
      >
        <Input
          value={renameDraft}
          onChange={(e) => setRenameDraft(e.target.value)}
          onPressEnter={() => void saveRename()}
          placeholder={t("singles.f.namePh")}
        />
        {renameTarget && (
          <div className="pane-hint" style={{ marginTop: 8 }}>
            {t("singles.renameIdHint", { id: renameTarget.identifier })}
          </div>
        )}
      </Modal>

      {/* 编辑 SOUL.MD（人设：自定义状态 + 恢复默认 + 文本编辑） */}
      <Modal
        open={delTarget !== null}
        title={t("singles.deleteConfirm")}
        onCancel={() => setDelTarget(null)}
        onOk={() => {
          if (delTarget) void remove(delTarget.identifier);
          setDelTarget(null);
        }}
        okText={t("common.delete")}
        okButtonProps={{ danger: true }}
      >
        <span className="dim">{t("singles.deleteHint", { name: delTarget?.name ?? "", id: delTarget?.identifier ?? "" })}</span>
      </Modal>

      <Modal
        open={promptOf !== null}
        title={promptOf ? t("singles.promptTitle", { name: promptOf.name }) : ""}
        onCancel={() => setPromptOf(null)}
        width={720}
        footer={
          <Space>
            <Button onClick={() => setPromptOf(null)}>{t("common.close")}</Button>
            <Button type="primary" loading={promptSaving} onClick={() => void savePrompt()}>
              {t("agents.saveHot")}
            </Button>
          </Space>
        }
      >
        <Input.TextArea
          rows={18}
          className="prompt-editor"
          spellCheck={false}
          value={promptDraft}
          onChange={(e) => setPromptDraft(e.target.value)}
          placeholder={t("singles.promptPh")}
        />
      </Modal>

      <Modal
        open={soulOf !== null}
        title={soulOf ? t("singles.soulTitle", { name: soulOf.name }) : ""}
        onCancel={() => setSoulOf(null)}
        width={640}
        footer={
          <Space>
            <Popconfirm title={t("agents.resetConfirm")} onConfirm={() => void resetSoul()} disabled={soulInfo ? !soulInfo.custom : false}>
              <Button icon={<UndoOutlined />} disabled={soulInfo ? !soulInfo.custom : false}>
                {t("agents.resetBtn")}
              </Button>
            </Popconfirm>
            <Button onClick={() => setSoulOf(null)}>{t("common.close")}</Button>
            <Button type="primary" loading={soulSaving} onClick={() => void saveSoul()}>
              {t("agents.saveHot")}
            </Button>
          </Space>
        }
      >
        <Space orientation="vertical" style={{ width: "100%" }} size="small">
          {soulInfo && (
            <div className="persona-status">
              {soulInfo.custom ? (
                <Tag color="purple">{t("agents.customPersona")}</Tag>
              ) : (
                <Tag>{t("agents.defaultPersona")}</Tag>
              )}
              <span className="persona-hint">{t("singles.soulHint", { id: soulOf?.identifier ?? "" })}</span>
            </div>
          )}
          <Input.TextArea
            rows={10}
            value={soulDraft}
            onChange={(e) => setSoulDraft(e.target.value)}
            placeholder={t("singles.soulPh")}
          />
        </Space>
      </Modal>

      <Modal
        open={modal}
        title={t("singles.new")}
        onCancel={() => setModal(false)}
        onOk={() => void submit()}
        confirmLoading={saving}
        okText={t("singles.create")}
        destroyOnHidden
      >
        <Form form={form} layout="vertical" onFinish={() => void submit()}>
          <Form.Item name="name" label={t("singles.f.name")} rules={[{ required: true }]}>
            <Input placeholder={t("singles.f.namePh")} />
          </Form.Item>
          <Form.Item
            name="identifier"
            label={t("singles.f.identifier")}
            rules={[{ required: true }, { pattern: /^[A-Za-z0-9_-]{1,48}$/, message: t("singles.idPattern") }]}
          >
            <Input placeholder={t("singles.f.identifierPh")} />
          </Form.Item>
          <Form.Item name="description" label={t("singles.f.desc")} rules={[{ required: true }]}>
            <Input.TextArea rows={2} placeholder={t("singles.f.descPh")} />
          </Form.Item>
          <Form.Item name="prompt" label={t("singles.f.prompt")}>
            <Input.TextArea rows={4} placeholder={t("singles.f.promptPh")} />
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

      {/* 组信息编辑（group-card-menu）：改名 / 改 id / 工作区 / 默认模型（基本设置入口） */}
      <Modal
        open={groupEditTarget !== null}
        title={groupEditTarget ? t("groups.editInfoTitle", { name: groupEditTarget.name }) : ""}
        onCancel={() => setGroupEditTarget(null)}
        onOk={() => void saveGroupEdit()}
        confirmLoading={groupEditSaving}
        okText={t("common.save")}
        destroyOnHidden
      >
        <Form form={groupEditForm} layout="vertical">
          <Form.Item
            name="name"
            label={t("groups.f.name")}
            rules={[{ required: true, whitespace: true, message: t("groups.f.nameRequired") }]}
          >
            <Input placeholder={t("groups.f.namePh")} />
          </Form.Item>
          <Form.Item name="workspace" label={t("groups.f.workspace")} extra={t("groups.f.workspaceHelp")}>
            <Input placeholder={t("groups.f.wsPh")} />
          </Form.Item>
          <Form.Item name="model" label={t("groups.f.model")} extra={t("groups.f.modelHelp")}>
            <Select allowClear options={buildModelOptions(profiles)} placeholder={t("groups.capsFollow")} />
          </Form.Item>
          <Form.Item
            name="identifier"
            label={t("groups.f.idLabel")}
            extra={t("groups.f.idEditExtra")}
            rules={[
              { required: true },
              { pattern: /^[A-Za-z0-9_-]{1,48}$/, message: t("groups.idPattern") },
            ]}
          >
            <Input placeholder="research" />
          </Form.Item>
        </Form>
      </Modal>

      {/* 自我优化设置（group-self-optimize）：Switch 是目标态草稿，保存提交草稿值；内置组放开编成保护是显式授权 */}
      <Modal
        open={groupOptTarget !== null}
        title={t("groups.selfOptTitle", { name: groupOptTarget?.name ?? "" })}
        onCancel={() => setGroupOptTarget(null)}
        onOk={() => void toggleGroupOpt()}
        confirmLoading={groupOptSaving}
        okText={t("common.save")}
        destroyOnHidden
      >
        <div className="pane-hint" style={{ display: "grid", gap: 10 }}>
          <span>{t("groups.selfOptHint")}</span>
          {groupOptTarget?.builtin && <span>{t("groups.selfOptBuiltinHint")}</span>}
          <Space align="center">
            <Switch
              checked={groupOptTarget ? (groupOptTarget.selfOptimize ?? false) : false}
              onChange={(v) => setGroupOptTarget(groupOptTarget ? { ...groupOptTarget, selfOptimize: v } : groupOptTarget)}
            />
            <span>{(groupOptTarget?.selfOptimize ?? false) ? t("common.on") : t("common.off")}</span>
          </Space>
        </div>
      </Modal>
    </div>
  );
}
