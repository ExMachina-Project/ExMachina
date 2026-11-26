/** 技能包管理：派发时按触发词自动携带的指令包（数据化技能，热装载） */
import React, { useCallback, useEffect, useState } from "react";
import {
  Button,
  Card,
  Form,
  Input,
  Modal,
  Popconfirm,
  Space,
  Spin,
  Table,
  Tag,
  Tooltip,
  message,
} from "antd";
import { BrandEmpty } from "../components/BrandEmpty";
import { HistoryOutlined, PlusOutlined, ReloadOutlined } from "@ant-design/icons";
import {
  api,
  type DeletedSkill,
  type SkillDef,
  type SkillHistoryRow,
} from "../api";
import { PageHeader } from "../components/PageHeader";
import { tr, useT } from "../i18n/core";

export function SkillsView(): React.ReactElement {
  const t = useT();
  const [skills, setSkills] = useState<SkillDef[]>([]);
  const [loading, setLoading] = useState(false);
  const [modal, setModal] = useState(false);
  const [saving, setSaving] = useState(false);
  const [form] = Form.useForm();
  // 版本链（capability-parity 6.5 / 6.6）：删除后技能从列表消失，「回滚恢复」入口靠已删除清单
  const [deleted, setDeleted] = useState<DeletedSkill[]>([]);
  const [historyFor, setHistoryFor] = useState<{ id: string; name: string } | null>(null);
  const [history, setHistory] = useState<SkillHistoryRow[]>([]);
  const [historyCurrent, setHistoryCurrent] = useState<number | null>(null);
  const [historyLoading, setHistoryLoading] = useState(false);
  const [busyId, setBusyId] = useState<string | null>(null);

  const load = useCallback(async () => {
    setLoading(true);
    try {
      // 与技能列表同等对待：清单读失败就是加载失败，不静默降级成「没有已删除技能」
      const [list, del] = await Promise.all([api.skills(), api.deletedSkills()]);
      setSkills(list);
      setDeleted(del.skills);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const openHistory = async (id: string, name: string) => {
    setHistoryFor({ id, name });
    setHistory([]);
    setHistoryCurrent(null);
    setHistoryLoading(true);
    try {
      const r = await api.skillVersions(id);
      setHistory(r.versions);
      setHistoryCurrent(r.current);
      setHistoryFor({ id, name: r.versions.at(-1)?.name ?? name });
    } catch (e) {
      message.error(tr("common.loadFailed", { err: String(e) }));
      setHistoryFor(null);
    } finally {
      setHistoryLoading(false);
    }
  };

  const rollback = async (id: string, to: number) => {
    setBusyId(id);
    try {
      const r = await api.rollbackSkill(id, to);
      message.success(tr("skills.rollbackDone", { to, v: r.version }));
      const [h] = await Promise.all([api.skillVersions(id), load()]);
      setHistory(h.versions);
      setHistoryCurrent(h.current);
    } catch (e) {
      message.error(tr("common.opFailed", { err: String(e) }));
    } finally {
      setBusyId(null);
    }
  };

  const submit = async () => {
    const v = await form.validateFields();
    setSaving(true);
    try {
      await api.createSkill({
        id: v.id,
        name: v.name,
        description: v.description ?? "",
        triggers: (v.triggers ?? "").split(/[,，]/).map((s: string) => s.trim()).filter(Boolean),
        instructions: v.instructions,
        agents: (v.agents ?? "").split(/[,，]/).map((s: string) => s.trim()).filter(Boolean),
      });
      message.success(t("skills.created", { id: v.id }));
      setModal(false);
      form.resetFields();
      await load();
    } catch (e) {
      message.error(t("skills.createFailed", { err: String(e) }));
    } finally {
      setSaving(false);
    }
  };

  const remove = async (id: string) => {
    try {
      await api.deleteSkill(id);
      message.success(t("skills.deleted", { id }));
      await load();
    } catch (e) {
      message.error(t("skills.deleteFailed", { err: String(e) }));
    }
  };

  return (
    <div className="pane-wrap">
      <PageHeader
        en="SKILLS"
        title={t("nav.skills")}
        desc={t("skills.desc")}
        actions={
          <>
            <Button type="primary" icon={<PlusOutlined />} onClick={() => setModal(true)}>
              {t("skills.new")}
            </Button>
            <Button icon={<ReloadOutlined />} onClick={() => void load()}>
              {t("common.refresh")}
            </Button>
          </>
        }
      />
      <Spin spinning={loading}>
        <div className="skill-grid">
          {skills.map((s) => (
            <Card
              key={s.id}
              size="small"
              className="hud skill-card"
              title={
                <Space>
                  <span>{s.name}</span>
                  {/* 照实显示版本号（legacy 技能为 v0）：与 CLI / REST 同一口径，回滚命令里的 --to 才对得上 */}
                  <Tag>{tr("skills.versionTag", { v: s.version ?? 0 })}</Tag>
                  <span className="mono dim">{s.id}</span>
                </Space>
              }
              extra={
                <Space size={4}>
                  <Tooltip title={t("skills.historyTip")}>
                    <Button
                      size="small"
                      type="text"
                      icon={<HistoryOutlined />}
                      disabled={busyId === s.id}
                      onClick={() => void openHistory(s.id, s.name)}
                    />
                  </Tooltip>
                  <Popconfirm title={t("skills.deleteConfirm")} onConfirm={() => void remove(s.id)}>
                    <Button size="small" danger disabled={busyId === s.id}>
                      {t("common.delete")}
                    </Button>
                  </Popconfirm>
                </Space>
              }
            >
              {s.description && <div className="dim">{s.description}</div>}
              <div className="skill-line">
                {t("skills.triggerLabel")}
                {s.triggers.length ? (
                  s.triggers.map((tl) => (
                    <Tag key={tl}>{tl}</Tag>
                  ))
                ) : (
                  <span className="dim">—</span>
                )}
              </div>
              <div className="skill-line">
                {t("skills.applyLabel")}{s.agents.length ? s.agents.map((a) => <Tag key={a}>{a}</Tag>) : <Tag color="purple">{t("skills.allAgents")}</Tag>}
              </div>
              <div className="skill-line dim">
                {t("skills.usage", { used: s.useCount ?? 0, ok: s.successCount ?? 0 })}
                {s.source && s.source !== "manual" ? ` · ${s.source}` : ""}
              </div>
              <details className="skill-details"><summary>{t("skills.f.instructions")}</summary><div className="skill-instructions">{s.instructions}</div></details>
            </Card>
          ))}
        </div>
        {!loading && skills.length === 0 && (
          <BrandEmpty description={t("skills.empty")} className="graph-empty" />
        )}
      </Spin>

      <Card
        size="small"
        className="hud"
        style={{ marginTop: 16 }}
        title={
          <Space>
            <span>{t("skills.deletedCard")}</span>
            <Tag>{tr("skills.deletedCount", { n: deleted.length })}</Tag>
          </Space>
        }
      >
        {deleted.length === 0 ? (
          <BrandEmpty description={t("skills.deletedEmpty")} />
        ) : (
          <Space orientation="vertical" size={8} style={{ width: "100%" }}>
            {deleted.map((d) => (
              <div key={d.id} className="skill-line">
                <Tag color="red">{t("skills.deletedBadge")}</Tag>
                <span className="mono">{d.id}</span>
                <span className="dim">
                  {d.deletedAt ? ` · ${d.deletedAt}` : ""}
                </span>
                <Button
                  size="small"
                  type="link"
                  icon={<HistoryOutlined />}
                  disabled={busyId === d.id}
                  onClick={() => void openHistory(d.id, d.id)}
                >
                  {t("skills.historyTip")}
                </Button>
              </div>
            ))}
          </Space>
        )}
      </Card>

      <Modal
        open={historyFor !== null}
        title={
          historyFor
            ? tr("skills.historyTitle", { name: historyFor.name })
            : t("skills.historyTip")
        }
        footer={null}
        width={760}
        onCancel={() => setHistoryFor(null)}
        destroyOnHidden
      >
        {historyLoading ? null : history.length === 0 ? (
          <BrandEmpty description={t("skills.historyEmpty")} />
        ) : (
          <Table<SkillHistoryRow>
            size="small"
            rowKey="version"
            dataSource={history}
            pagination={false}
            columns={[
              {
                title: t("skills.colVersion"),
                dataIndex: "version",
                width: 110,
                render: (v: number) => (
                  <Space size={4}>
                    <Tag color={v === historyCurrent ? "blue" : "default"}>
                      {tr("skills.versionTag", { v })}
                    </Tag>
                    {v === historyCurrent && <Tag color="blue">{t("skills.currentVersion")}</Tag>}
                  </Space>
                ),
              },
              {
                title: t("skills.instructionsPreview"),
                key: "instructions",
                render: (_, r) => (
                  <Space orientation="vertical" size={0}>
                    <span>
                      <b>{r.name}</b>
                      {r.description && <span className="dim"> · {r.description}</span>}
                    </span>
                    {/* 每版写了什么：名字各版相同，光看名字无从选回滚目标 */}
                    <span className="memory-body">{r.instructions.slice(0, 200)}</span>
                  </Space>
                ),
              },
              {
                key: "ops",
                width: 160,
                render: (_, r) =>
                  r.version === historyCurrent ? null : (
                    <Popconfirm
                      title={tr("skills.rollbackConfirm", { to: r.version })}
                      onConfirm={() => rollback(historyFor?.id ?? "", r.version)}
                    >
                      <Button
                        size="small"
                        type="link"
                        icon={<ReloadOutlined />}
                        disabled={busyId === historyFor?.id}
                      >
                        {t("skills.rollbackTip")}
                      </Button>
                    </Popconfirm>
                  ),
              },
            ]}
          />
        )}
      </Modal>

      <Modal
        open={modal}
        title={t("skills.new")}
        onCancel={() => setModal(false)}
        onOk={() => void submit()}
        confirmLoading={saving}
        okText={t("skills.create")}
        destroyOnHidden
      >
        <Form form={form} layout="vertical" onFinish={() => void submit()}>
          <Form.Item name="id" label="ID" rules={[{ required: true }, { pattern: /^[A-Za-z0-9_-]{1,48}$/, message: t("skills.idPattern") }]}>
            <Input placeholder={t("skills.f.idPh")} />
          </Form.Item>
          <Form.Item name="name" label={t("skills.f.name")} rules={[{ required: true }]}>
            <Input placeholder={t("skills.f.namePh")} />
          </Form.Item>
          <Form.Item name="description" label={t("skills.f.desc")}>
            <Input />
          </Form.Item>
          <Form.Item name="triggers" label={t("skills.f.triggers")}>
            <Input placeholder="git push, git reset, git clean" />
          </Form.Item>
          <Form.Item name="agents" label={t("skills.f.agents")}>
            <Input placeholder="coding-agent, ops-agent" />
          </Form.Item>
          <Form.Item name="instructions" label={t("skills.f.instructions")} rules={[{ required: true }]}>
            <Input.TextArea rows={4} placeholder={t("skills.f.instructionsPh")} />
          </Form.Item>
        </Form>
      </Modal>
    </div>
  );
}
