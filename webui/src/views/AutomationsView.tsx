/** 自动化管理：定时任务（cron / 一次性）的创建、启停、手动执行与运行记录 */
import React, { useCallback, useEffect, useState } from "react";
import { Segmented, Collapse, Button, Drawer, Form, Input, Modal, Popconfirm, Select, Space, Spin, Switch, Table, Tag, message } from "antd";
import { BrandEmpty } from "../components/BrandEmpty";
import { CaretRightOutlined, PlusOutlined, ReloadOutlined } from "@ant-design/icons";
import { api, type Channel, type CronJob, type CronRun } from "../api";
import { PageHeader } from "../components/PageHeader";
import { useExm } from "../store";
import { useT } from "../i18n/core";

/** 常用 cron 模板（4.4）：一键填入表达式 */
const CRON_TEMPLATES: { expr: string; labelKey: "cron.tpl.daily" | "cron.tpl.weekday" | "cron.tpl.hourly" | "cron.tpl.weekly" | "cron.tpl.monthly" }[] = [
  { expr: "0 9 * * *", labelKey: "cron.tpl.daily" },
  { expr: "0 9 * * 1-5", labelKey: "cron.tpl.weekday" },
  { expr: "0 * * * *", labelKey: "cron.tpl.hourly" },
  { expr: "0 9 * * 1", labelKey: "cron.tpl.weekly" },
  { expr: "0 9 1 * *", labelKey: "cron.tpl.monthly" },
];

export function AutomationsView(): React.ReactElement {
  const t = useT();
  const { groups, activeGroup, sessions } = useExm();
  const [jobs, setJobs] = useState<CronJob[]>([]);
  const [channels, setChannels] = useState<Channel[]>([]);
  const [loading, setLoading] = useState(false);
  const [modal, setModal] = useState(false);
  const [saving, setSaving] = useState(false);
  const [runsFor, setRunsFor] = useState<CronJob | null>(null);
  const [runs, setRuns] = useState<CronRun[]>([]);
  const [form] = Form.useForm();

  const load = useCallback(async () => {
    setLoading(true);
    try {
      const [jobs, chs] = await Promise.all([api.listCron(), api.listChannels()]);
      setJobs(jobs);
      setChannels(chs);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const submit = async () => {
    const v = await form.validateFields();
    setSaving(true);
    try {
      const mode = (v.mode ?? "cron") as string;
      const oneOff = mode === "at";
      const queue = mode === "queue";
      await api.createCron({
        name: v.name,
        prompt: v.prompt,
        kind: mode,
        cron: oneOff || queue ? undefined : (v.cron as string | undefined),
        at: oneOff ? (v.at as string | undefined) : undefined,
        // 连续任务：按序执行的任务 id 列表（后端要求至少一项）
        steps: queue
          ? ((v.steps ?? []) as string[]).map((s) => s.trim()).filter(Boolean)
          : undefined,
        // 一次性任务可投递到已有会话；留空则按 sessionTitle 新建/复用
        targetSession: oneOff ? (v.targetSession || undefined) : undefined,
        group: v.group || undefined,
        sessionTitle: v.sessionTitle || undefined,
        notifyChannels: ((v.notifyChannels ?? []) as string[]).map((s) => s.trim()).filter(Boolean),
      });
      message.success(t("cron.created"));
      setModal(false);
      form.resetFields();
      await load();
    } catch (e) {
      message.error(t("cron.createFailed", { err: String(e) }));
    } finally {
      setSaving(false);
    }
  };

  const toggle = async (job: CronJob, enabled: boolean) => {
    await api.updateCron(job.id, { enabled });
    await load();
  };

  const runNow = async (job: CronJob) => {
    try {
      const run = await api.runCron(job.id);
      message.success(t("cron.ran", { status: run.status, id: run.sessionId.slice(0, 8) }));
    } catch (e) {
      message.error(t("cron.runFailed", { err: String(e) }));
    }
  };

  const openRuns = async (job: CronJob) => {
    setRunsFor(job);
    setRuns(await api.cronRuns(job.id, 30));
  };

  return (
    <div className="pane-wrap">
      <PageHeader
        en="CRON"
        title={t("nav.cron")}
        desc={t("cron.desc")}
        actions={
          <>
            <Button type="primary" icon={<PlusOutlined />} onClick={() => setModal(true)}>
              {t("cron.new")}
            </Button>
            <Button icon={<ReloadOutlined />} onClick={() => void load()}>
              {t("common.refresh")}
            </Button>
          </>
        }
      />
      <Spin spinning={loading}>
        <Table<CronJob>
          size="small"
          rowKey="id"
          pagination={false}
          dataSource={jobs}
          locale={{ emptyText: <BrandEmpty description={t("cron.empty")} className="pane-empty" /> }}
          columns={[
            {
              title: t("cron.colJob"),
              render: (_, j) => (
                <Space direction="vertical" size={0}>
                  <b>{j.name}</b>
                  <span className="mono dim">{j.id}</span>
                </Space>
              ),
            },
            {
              title: t("cron.colSchedule"),
              render: (_, j) => {
                // 任务类型（v0.4.0 三类型）：缺省按 cron/at 有无推断，与后端 job_kind 同口径
                const kind = j.kind ?? (j.cron ? "cron" : "at");
                return (
                  <Space direction="vertical" size={2}>
                    {kind === "queue" ? (
                      <Tag>{t("cron.kind.queue")}</Tag>
                    ) : kind === "at" ? (
                      <Tag>{t("cron.oneOff", { at: j.at ?? "" })}</Tag>
                    ) : (
                      <Tag>cron[{j.cron ?? ""}]</Tag>
                    )}
                    {kind === "queue" && j.steps && j.steps.length > 0 && (
                      <span className="mono dim">
                        {t("cron.f.stepsShort")}: {j.steps.join(" → ")}
                      </span>
                    )}
                    {kind === "at" && j.targetSession && (
                      <span className="mono dim">
                        → {j.targetSession.slice(0, 8)}…
                      </span>
                    )}
                    {j.enabled && j.nextRunAt && (
                      <span className="dim mono">{t("cron.nextRun", { at: new Date(j.nextRunAt).toLocaleString() })}</span>
                    )}
                  </Space>
                );
              },
            },
            {
              title: t("cron.colGroup"),
              render: (_, j) => (
                <Space direction="vertical" size={0}>
                  <span className="mono">{j.group ?? t("cron.currentGroup")}</span>
                  {j.notifyChannels && j.notifyChannels.length > 0 && (
                    <span className="mono dim">
                      {t("cron.f.notifyChannels")}: {j.notifyChannels.join(", ")}
                    </span>
                  )}
                </Space>
              ),
            },
            {
              title: t("cron.colEnabled"),
              render: (_, j) => <Switch size="small" checked={j.enabled} onChange={(v) => void toggle(j, v)} />,
            },
            {
              title: t("cron.colLastRun"),
              render: (_, j) => (
                <span className="mono dim">
                  {j.lastRunAt ?? t("cron.notRun")}
                  {j.lastStatus ? ` · ${j.lastStatus}` : ""}
                </span>
              ),
            },
            {
              title: t("cron.colOps"),
              render: (_, j) => (
                <Space>
                  <Button size="small" icon={<CaretRightOutlined />} onClick={() => void runNow(j)}>
                    {t("cron.run")}
                  </Button>
                  <Button size="small" onClick={() => void openRuns(j)}>
                    {t("cron.runs")}
                  </Button>
                  <Popconfirm title={t("cron.deleteConfirm")} onConfirm={() => void api.deleteCron(j.id).then(load)}>
                    <Button size="small" danger>
                      {t("common.delete")}
                    </Button>
                  </Popconfirm>
                </Space>
              ),
            },
          ]}
        />
      </Spin>

      <Drawer title={t("cron.runsTitle", { name: runsFor?.name ?? "" })} open={runsFor !== null} onClose={() => setRunsFor(null)} width={520}>
        {runs.length === 0 && <BrandEmpty description={t("cron.noRuns")} />}
        {runs.map((r) => (
          <div key={r.id} className="run-row">
            <div>
              <Tag color={r.status === "done" ? "success" : "error"}>{r.status}</Tag>
              <span className="mono dim">{r.startedAt}</span>
            </div>
            <div className="dim">
              {t("cron.sessionLabel")} <span className="mono">{r.sessionId.slice(0, 8)}</span> · {r.summary}
            </div>
          </div>
        ))}
      </Drawer>

      <Modal
        open={modal}
        title={t("cron.modalNew")}
        onCancel={() => setModal(false)}
        onOk={() => void submit()}
        confirmLoading={saving}
        okText={t("cron.create")}
        destroyOnHidden
      >
        <Form form={form} layout="vertical" onFinish={() => void submit()} initialValues={{ mode: "cron" }}>
          <Form.Item name="name" label={t("cron.f.name")} rules={[{ required: true }]}>
            <Input placeholder={t("cron.f.namePh")} />
          </Form.Item>
          <Form.Item name="prompt" label={t("cron.f.prompt")} rules={[{ required: true }]}>
            <Input.TextArea rows={3} placeholder={t("cron.f.promptPh")} />
          </Form.Item>
          <Form.Item name="mode" label={t("cron.f.mode")}>
            <Segmented
              options={[
                { value: "cron", label: t("cron.mode.cron") },
                { value: "at", label: t("cron.mode.at") },
                { value: "queue", label: t("cron.mode.queue") },
              ]}
            />
          </Form.Item>
          <Form.Item noStyle shouldUpdate={(a, b) => a.mode !== b.mode}>
            {({ getFieldValue }) =>
              getFieldValue("mode") === "at" ? (
                <>
                  <Form.Item name="at" label={t("cron.f.at")} rules={[{ required: true }]}>
                    <Input placeholder="2026-09-13T09:00:00Z" />
                  </Form.Item>
                  <Form.Item name="targetSession" label={t("cron.f.targetSession")} extra={t("cron.f.targetSessionExtra")}>
                    <Select
                      allowClear
                      showSearch
                      placeholder={t("cron.f.targetSessionPh")}
                      options={sessions.map((s) => ({ value: s.id, label: `${s.title}（${s.id.slice(0, 8)}…）` }))}
                    />
                  </Form.Item>
                </>
              ) : getFieldValue("mode") === "queue" ? (
                /* 连续任务：按序执行的步骤任务 id（后端要求至少一项） */
                <Form.Item
                  name="steps"
                  label={t("cron.f.steps")}
                  extra={t("cron.f.stepsExtra")}
                  rules={[
                    {
                      validator: (_, value: string[]) =>
                        Array.isArray(value) && value.map((s) => s.trim()).filter(Boolean).length > 0
                          ? Promise.resolve()
                          : Promise.reject(new Error(t("cron.f.stepsRequired"))),
                    },
                  ]}
                >
                  <Select
                    mode="tags"
                    allowClear
                    placeholder={t("cron.f.stepsPh")}
                    tokenSeparators={[",", " ", "\n"]}
                  />
                </Form.Item>
              ) : (
                <>
                  <Form.Item label={t("cron.templates")} style={{ marginBottom: 6 }}>
                    <Space size={4} wrap>
                      {CRON_TEMPLATES.map((tpl) => (
                        <Button
                          key={tpl.expr}
                          size="small"
                          onClick={() => form.setFieldsValue({ cron: tpl.expr })}
                          title={tpl.expr}
                        >
                          {t(tpl.labelKey)}
                        </Button>
                      ))}
                    </Space>
                  </Form.Item>
                  <Form.Item
                    name="cron"
                    label={t("cron.f.cron")}
                    rules={[{ required: true }, { pattern: /^(\S+\s+){4}\S+$/, message: t("cron.cronPattern") }]}
                  >
                    <Input placeholder="0 9 * * *" />
                  </Form.Item>
                </>
              )
            }
          </Form.Item>
          <Form.Item name="group" label={t("cron.f.group")} initialValue={activeGroup}>
            <Select
              allowClear
              placeholder={t("cron.f.groupPh")}
              options={groups.map((g) => ({ value: g.id, label: `${g.name}（${g.id}）` }))}
            />
          </Form.Item>

          <Collapse
            ghost
            className="form-advanced"
            items={[
              {
                key: "adv",
                label: t("cron.adv"),
                children: (
                  <>
                    <Form.Item
                      name="sessionTitle"
                      label={t("cron.f.sessionTitle")}
                      extra={t("cron.f.sessionTitleExtra")}
                    >
                      <Input placeholder={t("cron.f.sessionTitlePh")} />
                    </Form.Item>
                    <Form.Item
                      name="notifyChannels"
                      label={t("cron.f.notifyChannels")}
                      extra={t("cron.f.notifyChannelsExtra")}
                    >
                      <Select
                        mode="multiple"
                        allowClear
                        placeholder={t("cron.f.notifyChannelsPh")}
                        options={channels.map((c) => ({
                          value: c.id,
                          label: c.account ? `${c.account}（${c.id}）` : c.id,
                        }))}
                      />
                    </Form.Item>
                  </>
                ),
              },
            ]}
          />
        </Form>
      </Modal>
    </div>
  );
}
