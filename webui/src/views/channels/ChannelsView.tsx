/** 通道网关：多平台多账号接入（QQ 官方机器人 / NapCat / Telegram / Discord / Slack / Matrix / webhook 桥接），每个账号可绑定不同智能体组 */
import React, { useCallback, useEffect, useState } from "react";
import {
  Button, Collapse, Form, Input, Modal, Popconfirm, Segmented, Select, Slider, Space, Spin, Switch, Table, Tag, Tooltip, message,
} from "antd";
import { BrandEmpty } from "../../components/BrandEmpty";
import { PlusOutlined, QrcodeOutlined, ReloadOutlined } from "@ant-design/icons";
import { api, type Channel, type ChannelStatus } from "../../api";
import { PageHeader } from "../../components/PageHeader";
import { IdentityPanel } from "../../components/IdentityPanel";
import { MaskedPasswordInput } from "../../components/MaskedInput";
import { BiliQrLoginModal } from "../../components/BiliQrLoginModal";
import { QQBotBindModal } from "../../components/QQBotBindModal";
import { useExm } from "../../store";
import { useT, type TKey } from "../../i18n/core";
import { useIsNarrow } from "../../useIsNarrow";
import { fullTime, relativeTime } from "../../time";

import { PLATFORMS, BRIDGE_TYPES, platformDefaults, type ChanPlat, type ChanField } from "./catalog";

export function ChannelsView(): React.ReactElement {
  const t = useT();
  // 窄屏降级（移动端查漏）：次要列 md+ 才显示，主列收窄
  const narrow = useIsNarrow();
  // 插件清单动态并入平台目录：插件声明的 channel kinds（凭据字段按清单渲染）
  const [extraPlats, setExtraPlats] = useState<ChanPlat[]>([]);
  useEffect(() => {
    void (async () => {
      try {
        const r = await api.plugins();
        const plugs = r.plugins ?? [];
        const extra: ChanPlat[] = [];
        for (const p of plugs) {
          if (!p.enabled || !p.capabilities?.channel) continue;
          const kinds = p.capabilities.channel.kinds ?? [];
          // 静态表已有的 kind：凭据字段仍由静态表精细维护（帮助文案/扫码登录等），跳过
          const newKinds = kinds.filter((k) => !PLATFORMS.some((sp) => sp.key === k));
          for (const k of newKinds) {
            extra.push({
              key: k,
              color: undefined,
              tag: p.name || p.id,
              labelKey: "channels.plat.plugin" as TKey,
              helpKey: "channels.platHelp.plugin" as TKey,
              proactive: true,
              fields: [], // 凭据字段在详情处按清单动态渲染（下方 pluginCredentialFields）
            });
          }
        }
        setExtraPlats(extra);
      } catch {
        /* 插件面不可用时不阻塞静态目录 */
      }
    })();
  }, []);
  const catalog = React.useMemo<ChanPlat[]>(
    () => [...PLATFORMS, ...extraPlats],
    [extraPlats],
  );
  const { groups } = useExm();
  const [channels, setChannels] = useState<Channel[]>([]);
  const [tab, setTab] = useState("accounts");
  const [singles, setSingles] = useState<{ identifier: string; name: string }[]>([]);
  const [status, setStatus] = useState<Record<string, ChannelStatus>>({});
  const [testing, setTesting] = useState<string>("");
  const [testResult, setTestResult] = useState<Record<string, { ok: boolean; category?: string; identity?: string; message?: string; error?: string }>>({});
  const [loading, setLoading] = useState(false);
  const [modal, setModal] = useState(false);
  const [saving, setSaving] = useState(false);
  const [qrOpen, setQrOpen] = useState(false);
  const [qqBindOpen, setQqBindOpen] = useState(false);
  const [editing, setEditing] = useState<Channel | null>(null);
  const [form] = Form.useForm();

  const load = useCallback(async () => {
    setLoading(true);
    try {
      setChannels(await api.listChannels());
      // 绑定目标可选项：单智能体（默认形态，在前）+ 智能体组（次要）
      api.listSingles().then((r) => setSingles(r.singles ?? [])).catch(() => {});
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  // 运行状态轮询：适配器对账周期 5s，这里同步 5s 拉一次总览
  useEffect(() => {
    let alive = true;
    const pull = () =>
      api
        .channelStatus()
        .then((s) => {
          if (alive) setStatus(s);
        })
        .catch(() => {});
    pull();
    const timer = setInterval(pull, 5000);
    return () => {
      alive = false;
      clearInterval(timer);
    };
  }, []);

  const openCreate = () => {
    setEditing(null);
    form.resetFields();
    form.setFieldsValue({
      platform: "qqbot",
      capability: "computer",
      memoryIsolation: false,
      proactiveReply: false,
      config: platformDefaults("qqbot"),
    });
    setModal(true);
  };

  const openEdit = (c: Channel) => {
    setEditing(c);
    const plat = catalog.find((p) => p.key === c.type);
    // 扩展 config（目录之外键）反序列化为 JSON 预填，保存时随 config 一并回传
    const catalogKeys = new Set((plat?.fields ?? []).map((f) => f.key));
    const extra: Record<string, string> = {};
    for (const [k, v] of Object.entries(c.config ?? {})) {
      if (!catalogKeys.has(k)) extra[k] = v;
    }
    const cfg: Record<string, unknown> = { ...(c.config ?? {}) };
    plat?.fields
      .filter((f) => f.toggle)
      .forEach((f) => {
        const raw = c.config?.[f.key];
        cfg[f.key] = raw === undefined ? Boolean(f.default) : raw === "true";
      });
    form.setFieldsValue({
      platform: c.type,
      account: c.account,
      group: c.group,
      secret: c.secret,
      replyWebhook: c.replyWebhook,
      token: c.token,
      allowedChats: (c.allowedChats ?? []).join(", "),
      admins: (c.admins ?? []).join(", "),
      capability: c.capability ?? "computer",
      memoryIsolation: c.memoryIsolation ?? false,
      proactiveReply: c.proactiveReply ?? false,
      proactiveRate: c.proactiveRate ?? 100,
      mentionReply: c.mentionReply ?? true,
      config: cfg,
      configJson: Object.keys(extra).length > 0 ? JSON.stringify(extra, null, 2) : "",
    });
    setModal(true);
  };

  /** 解析 config 扩展 JSON：空串 = 空对象；合法对象（标量值）→ 键值表；否则返回错误文案 */
  const parseConfigJson = (raw: unknown): { value: Record<string, string>; error?: string } => {
    const s = String(raw ?? "").trim();
    if (!s) return { value: {} };
    let parsed: unknown;
    try {
      parsed = JSON.parse(s);
    } catch {
      return { value: {}, error: t("channels.f.configJsonBad") };
    }
    if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) {
      return { value: {}, error: t("channels.f.configJsonObj") };
    }
    const out: Record<string, string> = {};
    for (const [k, v] of Object.entries(parsed as Record<string, unknown>)) {
      if (!["string", "number", "boolean"].includes(typeof v)) {
        return { value: {}, error: t("channels.f.configJsonObj") };
      }
      out[k] = String(v);
    }
    return { value: out };
  };

  const submit = async () => {
    const v = await form.validateFields();
    setSaving(true);
    const plat = catalog.find((p) => p.key === v.platform) ?? catalog[0];
    // config 扩展 JSON：面板未展开时字段未注册，从 form store 直读并手动校验（合法才允许保存）
    const parsedJson = parseConfigJson(form.getFieldValue("configJson"));
    if (parsedJson.error) {
      message.error(parsedJson.error);
      setSaving(false);
      return;
    }
    // 目录字段优先：剔除扩展 JSON 里的同名键，避免「清空目录字段却被扩展 JSON 复活」
    for (const f of plat.fields) {
      delete parsedJson.value[f.key];
    }
    const config: Record<string, string> = { ...parsedJson.value };
    for (const f of plat.fields) {
      if (f.top) continue;
      // 布尔开关写 "true"/"false"：视频平台的 watch/dryRun 默认 true，
      // 用「空 = 删除键」会让「关掉」被后端默认值重新打开。
      const val = f.toggle ? (v.config?.[f.key] ? "true" : "false") : String(v.config?.[f.key] ?? "").trim();
      if (val) config[f.key] = val; // 其余空值不入库（清空敏感键即删除，与后端口径一致）
    }
    const body = {
      platform: v.platform,
      account: v.account || undefined,
      group: v.group || undefined,
      secret: v.secret || undefined,
      replyWebhook: v.replyWebhook || undefined,
      token: v.token || undefined,
      config,
      allowedChats: (v.allowedChats ?? "")
        .split(/[,,\s]+/)
        .map((s: string) => s.trim())
        .filter(Boolean),
      admins: (v.admins ?? "")
        .split(/[,,\s]+/)
        .map((s: string) => s.trim())
        .filter(Boolean),
      capability: v.capability ?? "computer",
      memoryIsolation: Boolean(v.memoryIsolation),
      proactiveReply: Boolean(v.proactiveReply),
      proactiveRate: v.proactiveReply ? Number(v.proactiveRate ?? 100) : undefined,
      mentionReply: v.mentionReply === undefined ? undefined : Boolean(v.mentionReply),
    };
    try {
      if (editing) {
        await api.updateChannel(editing.id, body);
        message.success(t("channels.updated", { id: editing.id }));
      } else {
        await api.createChannel({ id: v.id, enabled: true, ...body });
        message.success(t("channels.created", { id: v.id }));
      }
      setModal(false);
      await load();
    } catch (e) {
      message.error(t("common.saveFailed", { err: String(e) }));
    } finally {
      setSaving(false);
    }
  };

  const toggle = async (c: Channel, enabled: boolean) => {
    await api.updateChannel(c.id, { enabled });
    await load();
  };

  const remove = async (id: string) => {
    try {
      await api.deleteChannel(id);
      message.success(t("channels.del", { id }));
      await load();
    } catch (e) {
      message.error(t("channels.deleteFailed", { err: String(e) }));
    }
  };

  const origin = typeof window !== "undefined" ? window.location.origin : "";

  const platformTag = (p: string) => {
    const plat = catalog.find((x) => x.key === p);
    if (!plat) return <Tag>{p}</Tag>;
    return <Tag color={plat.color || undefined}>{plat.tag ?? (plat.tagKey ? t(plat.tagKey) : p)}</Tag>;
  };

  const fieldSet = (c: Channel, f: ChanField) =>
    f.top === "token" ? !!c.token : f.top === "secret" ? !!c.secret : !!c.config?.[f.key];

  const credTag = (c: Channel) => {
    const plat = catalog.find((x) => x.key === c.type);
    if (!plat) return <Tag>{c.type}</Tag>;
    const missing = plat.fields.filter((f) => f.required && !fieldSet(c, f));
    if (missing.length > 0) {
      return (
        <Tag color="error">
          {t("channels.credMissing", { fields: missing.map((f) => t(f.labelKey)).join(" / ") })}
        </Tag>
      );
    }
    const any = plat.fields.some((f) => (f.required || f.secret) && fieldSet(c, f));
    return any ? <Tag color="success">{t("channels.credOk")}</Tag> : <Tag>{t("channels.noCred")}</Tag>;
  };

  /** 运行状态：桥接类型无运行时；内置适配器以最近一次上报为准 */
  const statusCell = (c: Channel) => {
    if (BRIDGE_TYPES.includes(c.type) || !c.enabled) return <span className="dim">—</span>;
    const st = status[c.id];
    if (!st) return <Tag>{t("channels.stWait")}</Tag>;
    return (
      <Space orientation="vertical" size={0} style={{ display: "flex" }}>
        <Tooltip title={`${st.detail} · ${st.at}`}>
          <Tag color={st.state === "ok" ? "success" : "error"} style={{ cursor: "default", marginRight: 0, alignSelf: "flex-start" }}>
            {st.state === "ok" ? t("channels.stOk") : t("channels.stErr")}
          </Tag>
        </Tooltip>
        <span
          className="mono dim"
          style={{ display: "block", maxWidth: 260, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}
        >
          {st.detail}
        </span>
      </Space>
    );
  };

  return (
    <div className="pane-wrap">
      <PageHeader
        en="CHANNELS"
        title={t("nav.channels")}
        desc={t("channels.desc")}
        actions={
          <>
            <Segmented
              value={tab}
              onChange={(v) => setTab(v as string)}
              options={[
                { value: "accounts", label: t("channels.tabAccounts") },
                { value: "identity", label: t("channels.tabIdentity") },
              ]}
            />
            {tab === "accounts" ? (
              <Button type="primary" icon={<PlusOutlined />} onClick={openCreate}>
                {t("channels.add")}
              </Button>
            ) : null}
            <Button icon={<ReloadOutlined />} onClick={() => void load()}>
              {t("common.refresh")}
            </Button>
          </>
        }
      />

      <Spin spinning={loading}>
        {tab === "identity" ? (
          <IdentityPanel expanded />
        ) : (
          <div className={narrow ? "tbl-fit" : undefined}>
          <Table<Channel>
          size="small"
          rowKey="id"
          pagination={false}
          scroll={{ x: narrow ? "max-content" : "max-content" }}
          dataSource={channels}
          locale={{ emptyText: <BrandEmpty description={t("channels.empty")} className="pane-empty" /> }}
          columns={[
            {
              title: t("channels.colAccount"),
              render: (_, c) => (
                <Space orientation="vertical" size={0}>
                  <b>
                    {c.account ?? c.id} {c.account ? <span className="mono dim">{c.id}</span> : null}
                  </b>
                  <Tooltip title={fullTime(c.createdAt)}>
                    <span className="mono dim">{relativeTime(c.createdAt) || c.createdAt}</span>
                  </Tooltip>
                </Space>
              ),
            },
            { title: t("channels.colPlatform"), render: (_, c) => platformTag(c.type) },
            // 窄屏降级：绑定目标的标签组信息密度高但非首要，md+ 显示
            {
              title: t("channels.colGroup"),
              responsive: ["md"],
              render: (_, c) => {
                const cap = c.capability ?? "computer";
                const isSingle = c.group?.startsWith("single:");
                return (
                  <Space orientation="vertical" size={2}>
                    {c.group ? (
                      isSingle ? (
                        <Tag>
                          {t("channels.f.singleTag")} · {singles.find((s) => s.identifier === c.group!.slice(7))?.name ?? c.group.slice(7)}
                        </Tag>
                      ) : (
                        <Tag color="purple">{c.group}</Tag>
                      )
                    ) : (
                      <Tag>{t("channels.followSingle")}</Tag>
                    )}
                    <Space size={4} wrap>
                      <Tag color={cap === "chat" ? undefined : "purple"}>
                        {t(`channels.cap.${cap}` as TKey)}
                      </Tag>
                      {c.memoryIsolation ? (
                        <Tag>{t("channels.memIsolated")}</Tag>
                      ) : (
                        <Tag>{t("channels.memShared")}</Tag>
                      )}
                    </Space>
                  </Space>
                );
              },
            },
            { title: t("channels.colSecret"), responsive: ["md"], render: (_, c) => credTag(c) },
            // 窄屏状态列也收进 md+（150px 文案列 + 4 列在 390px 装不下；启用/编辑/删除优先）
            { title: t("channels.colStatus"), responsive: ["md"], width: narrow ? 150 : 280, render: (_, c) => statusCell(c) },
            // 窄屏降级：测试/回复方式/启用开关收进 md+；操作列常驻（编辑/删除可达性优先）
            {
              title: t("channels.colTest"),
              responsive: ["md"],
              width: 110,
              render: (_, c) => {
                const st = testResult[c.id];
                const testText = st
                  ? st.ok
                    ? st.identity || t("channels.testOkShort")
                    : t("channels.testCat." + st.category, { msg: (st.message ?? "").slice(0, 60) })
                  : "";
                return (
                  <Space size={4} orientation="vertical">
                    <Button
                      size="small"
                      loading={testing === c.id}
                      onClick={() => {
                        setTesting(c.id);
                        api.channelTest(c.id)
                          .then((r) => {
                            setTestResult((m) => ({ ...m, [c.id]: r }));
                            if (r.ok) message.success(t("channels.testOk", { id: r.identity ?? "" }));
                            else message.warning(t("channels.testFailC", { cat: r.category ?? "", msg: (r.message ?? r.error ?? "").slice(0, 80) }));
                          })
                          .catch((e) => message.error(t("channels.testFailC", { cat: "", msg: String(e).slice(0, 80) })));
                      }}
                    >
                      {t("channels.testBtn")}
                    </Button>
                    {st && (
                      /* 长错误文案限宽省略 + Tooltip 全文：antd Tag 不换行会撑破列宽，
                         连接类报错（IO error: Connection refused 等）曾把表格挤出视口 */
                      <Tooltip title={testText}>
                        <Tag
                          color={st.ok ? undefined : st.category === "auth" ? "error" : st.category === "missing" ? "warning" : undefined}
                          style={{ maxWidth: 160, whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis", marginInlineEnd: 0 }}
                        >
                          {testText}
                        </Tag>
                      </Tooltip>
                    )}
                  </Space>
                );
              },
            },
            {
              title: t("channels.colReply"),
              responsive: ["md"],
              render: (_, c) => {
                const plat = catalog.find((x) => x.key === c.type);
                if (plat?.proactive) return <Tag color="purple">{t("channels.replyProactive")}</Tag>;
                return plat?.replyBuiltin ? (
                  <Tag>{t("channels.replyBuiltin")}</Tag>
                ) : (
                  <span className="mono dim">{c.replyWebhook ?? "—"}</span>
                );
              },
            },
            {
              title: t("channels.colEnabled"),
              render: (_, c) => <Switch size="small" checked={c.enabled} onChange={(v) => void toggle(c, v)} />,
            },
            {
              title: t("channels.colOps"),
              width: 140,
              // 窄屏固定右侧（移动端查漏）：嵌套横滚下「删除」曾滚到底仍出视口，
              // fixed right 保证编辑/删除恒可达（antd 固定列 + scroll.x 标准方案）
              fixed: narrow ? "right" : undefined,
              render: (_, c) => (
                <Space>
                  <Button size="small" onClick={() => openEdit(c)}>
                    {t("common.edit")}
                  </Button>
                  <Popconfirm title={t("channels.deleteConfirm")} onConfirm={() => void remove(c.id)}>
                    <Button size="small" danger>
                      {t("common.delete")}
                    </Button>
                  </Popconfirm>
                </Space>
              ),
            },
          ]}
        />
        </div>
        )}
      </Spin>

      {channels.some((c) => BRIDGE_TYPES.includes(c.type)) && (
        <div className="inbound-hint hud">
          <div className="rail-label">{t("channels.webhookTitle")}</div>
          <pre className="live-pre">{t("channels.inboundSample", { origin })}</pre>
          <div className="dim">{t("channels.bridgeHint")}</div>
        </div>
      )}

      <Modal
        open={modal}
        title={editing ? t("channels.editTitle", { id: editing.id }) : t("channels.add")}
        onCancel={() => setModal(false)}
        onOk={() => void submit()}
        confirmLoading={saving}
        okText={t("common.save")}
        cancelText={t("common.cancel")}
        destroyOnHidden
        width={640}
      >
        <Form form={form} layout="vertical" onFinish={() => void submit()}>
          <Form.Item noStyle shouldUpdate={(a, b) => a.platform !== b.platform}>
            {({ getFieldValue }) => {
              const plat = catalog.find((p) => p.key === getFieldValue("platform")) ?? catalog[0];
              return (
                <>
                  <Form.Item
                    name="platform"
                    label={t("channels.f.platform")}
                    rules={[{ required: true }]}
                    extra={t(plat.helpKey)}
                  >
                    <Select
                      disabled={editing !== null}
                      options={catalog.map((p) => ({ value: p.key, label: p.tag ? `${t(p.labelKey)} · ${p.tag}` : t(p.labelKey) }))}
                      onChange={(kind) =>
                        form.setFieldsValue({
                          token: undefined,
                          secret: undefined,
                          config: platformDefaults(kind),
                          configJson: undefined,
                        })
                      }
                    />
                  </Form.Item>
                  {!editing && (
                    <Form.Item
                      name="id"
                      label={t("channels.f.id")}
                      rules={[{ required: true }, { pattern: /^[A-Za-z0-9_-]{1,48}$/, message: t("channels.idPattern") }]}
                    >
                      <Input placeholder={t("channels.f.idPh")} />
                    </Form.Item>
                  )}
                  {plat.fields.map((f) =>
                    f.toggle ? (
                      <Form.Item
                        key={f.key}
                        name={f.top ? f.key : ["config", f.key]}
                        valuePropName="checked"
                        label={t(f.labelKey)}
                        extra={f.helpKey ? t(f.helpKey) : undefined}
                      >
                        <Switch size="small" />
                      </Form.Item>
                    ) : (
                      <React.Fragment key={f.key}>
                        {f.qrLogin && (
                          <Form.Item label={t("channels.biliqr.btn")} extra={t("channels.biliqr.btnHelp")}>
                            <Button block icon={<QrcodeOutlined />} onClick={() => setQrOpen(true)}>
                              {t("channels.biliqr.btn")}
                            </Button>
                          </Form.Item>
                        )}
                        {f.qqBind && (
                          <Form.Item label={t("channels.qqbind.btn")} extra={t("channels.qqbind.btnHelp")}>
                            <Button block icon={<QrcodeOutlined />} onClick={() => setQqBindOpen(true)}>
                              {t("channels.qqbind.btn")}
                            </Button>
                          </Form.Item>
                        )}
                        <Form.Item
                          name={f.top ? f.key : ["config", f.key]}
                          label={t(f.labelKey)}
                          rules={
                            f.required && !editing
                              ? [{ required: true, message: t("channels.f.required") }]
                              : undefined
                          }
                          extra={f.helpKey ? t(f.helpKey) : undefined}
                        >
                          {f.options ? (
                            <Select options={f.options.map((o) => ({ value: o.value, label: t(o.labelKey) }))} />
                          ) : f.multiline ? (
                            <Input.TextArea rows={3} placeholder={f.ph?.startsWith("channels.") ? t(f.ph as TKey) : f.ph} />
                          ) : f.secret ? (
                            <MaskedPasswordInput autoComplete="new-password" placeholder={f.ph?.startsWith("channels.") ? t(f.ph as TKey) : f.ph} />
                          ) : (
                            <Input placeholder={f.ph?.startsWith("channels.") ? t(f.ph as TKey) : f.ph} />
                          )}
                        </Form.Item>
                      </React.Fragment>
                    ),
                  )}
                  <Form.Item name="group" label={t("channels.f.group")}>
                    <Select
                      allowClear
                      placeholder={t("channels.f.groupPh")}
                      options={[
                        {
                          label: t("channels.f.singleGroup"),
                          options: singles.map((s) => ({
                            value: `single:${s.identifier}`,
                            label: `${s.name}（${t("channels.f.singleTag")}）`,
                          })),
                        },
                        {
                          label: t("channels.f.groupTag"),
                          options: groups.map((g) => ({ value: g.id, label: `${g.name}（${g.id}）` })),
                        },
                      ]}
                    />
                  </Form.Item>
                  <Form.Item
                    name="capability"
                    label={t("channels.f.capability")}
                    extra={t("channels.f.capabilityHelp")}
                    rules={[{ required: true }]}
                  >
                    <Select
                      options={[
                        { value: "chat", label: t("channels.cap.chat") },
                        { value: "tools", label: t("channels.cap.tools") },
                        { value: "computer", label: t("channels.cap.computer") },
                      ]}
                    />
                  </Form.Item>
                  <Form.Item
                    name="memoryIsolation"
                    valuePropName="checked"
                    label={t("channels.f.memoryIsolation")}
                    extra={t("channels.f.memoryIsolationHelp")}
                  >
                    <Switch size="small" />
                  </Form.Item>
                  {/* 主动回复（群聊免 @）：开关 + 回复率（回复率仅开启时有意义） */}
                  <Form.Item
                    noStyle
                    shouldUpdate={(a, b) => a.proactiveReply !== b.proactiveReply}
                  >
                    {({ getFieldValue }) => (
                      <>
                        <Form.Item
                          name="proactiveReply"
                          valuePropName="checked"
                          label={t("channels.f.proactiveReply")}
                          extra={t("channels.f.proactiveReplyHelp")}
                        >
                          <Switch size="small" />
                        </Form.Item>
                        {getFieldValue("proactiveReply") && (
                          <Form.Item
                            name="proactiveRate"
                            label={t("channels.f.proactiveRate")}
                            extra={t("channels.f.proactiveRateHelp")}
                            initialValue={100}
                          >
                            <Slider min={0} max={100} step={5} marks={{ 0: "0", 50: "50", 100: "100" }} />
                          </Form.Item>
                        )}
                        {/* 提及必回（默认开）：@ / 引用回复机器人直接放行，不经漏斗 */}
                        <Form.Item
                          name="mentionReply"
                          valuePropName="checked"
                          label={t("channels.f.mentionReply")}
                          extra={t("channels.f.mentionReplyHelp")}
                          initialValue={true}
                        >
                          <Switch size="small" />
                        </Form.Item>
                      </>
                    )}
                  </Form.Item>
                  {/* 管理者账号：主体区直接可见（判权跟人走，任何通道配了都全局生效） */}
                  <Form.Item
                    name="admins"
                    label={t("channels.f.admins")}
                    extra={t("channels.f.adminsExtra")}
                  >
                    <Input placeholder={t("channels.f.adminsPh")} />
                  </Form.Item>

                  <Collapse
                    ghost
                    className="form-advanced"
                    items={[
                      {
                        key: "adv",
                        label: plat.replyBuiltin ? t("channels.advNative") : t("channels.adv"),
                        children: (
                          <>
                            <Form.Item name="account" label={t("channels.f.account")}>
                              <Input placeholder={t("channels.f.accountPh")} />
                            </Form.Item>
                            <Form.Item
                              name="allowedChats"
                              label={t("channels.f.allowedChats")}
                              extra={t("channels.f.allowedChatsExtra")}
                            >
                              <Input placeholder={t("channels.f.allowedChatsPh")} />
                            </Form.Item>
                            {!plat.replyBuiltin && (
                              <Form.Item name="replyWebhook" label={t("channels.f.replyWebhook")}>
                                <Input placeholder={t("channels.f.replyWebhookPh")} />
                              </Form.Item>
                            )}
                          </>
                        ),
                      },
                      {
                        key: "cfgjson",
                        label: t("channels.advConfig"),
                        children: (
                          <Form.Item
                            name="configJson"
                            label={t("channels.f.configJsonLabel")}
                            extra={t("channels.f.configJsonExtra")}
                            rules={[
                              {
                                validator: (_rule, value) => {
                                  const r = parseConfigJson(value);
                                  return r.error ? Promise.reject(new Error(r.error)) : Promise.resolve();
                                },
                              },
                            ]}
                          >
                            <Input.TextArea rows={4} className="mono" placeholder={t("channels.f.configJsonPh")} />
                          </Form.Item>
                        ),
                      },
                    ]}
                  />
                </>
              );
            }}
          </Form.Item>
        </Form>
      </Modal>
      <BiliQrLoginModal
        open={qrOpen}
        onClose={() => setQrOpen(false)}
        onConfirmed={({ sessdata, biliJct, uid }) => {
          // 回填 Cookie 字段：uid 非空才写（老账号可能不下发）
          form.setFieldsValue({ config: { ...form.getFieldValue("config"), sessdata, biliJct, ...(uid ? { uid } : {}) } });
        }}
      />
      <QQBotBindModal
        open={qqBindOpen}
        onClose={() => setQqBindOpen(false)}
        onConfirmed={({ appId, appSecret }) => {
          // 回填官方机器人凭据：扫码绑定后由平台下发
          form.setFieldsValue({ config: { ...form.getFieldValue("config"), appId, appSecret } });
        }}
      />
    </div>
  );
}
