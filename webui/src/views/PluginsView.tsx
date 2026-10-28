/** 插件管理：四层扩展模型（数据/声明式/进程/内置）的统一管理面 + 索引目录安装流 */
import React, { useCallback, useEffect, useState } from "react";
import {
  Button,
  Card,
  Descriptions,
  Drawer,
  Empty,
  Input,
  Popconfirm,
  Space,
  Spin,
  Switch,
  Table,
  Tag,
  Typography,
  message,
} from "antd";
import { CloudDownloadOutlined, ReloadOutlined, SearchOutlined } from "@ant-design/icons";
import { api, type PluginDetail, type PluginSummary } from "../api";
import { PageHeader } from "../components/PageHeader";
import { useT } from "../i18n/core";

/** 形态 → 标签色 */
const FORM_COLOR: Record<string, string> = {
  builtin: "blue",
  process: "purple",
  declarative: "cyan",
  virtual: "default",
  unknown: "default",
};

const FORM_LABEL: Record<string, string> = {
  builtin: "内置",
  process: "进程",
  declarative: "声明式",
  virtual: "虚拟",
  unknown: "未知",
};

/** 索引状态 → 文案/色 */
/** 索引状态 → i18n key / 色 */
const INDEX_STATUS: Record<string, { key: string; color?: string }> = {
  available: { key: "plugins.catalog.available", color: "green" },
  installed: { key: "plugins.catalog.installed", color: "blue" },
  updatable: { key: "plugins.catalog.updatable", color: "orange" },
  incompatible: { key: "plugins.catalog.incompatible", color: "red" },
};

export function PluginsView(): React.ReactElement {
  const t = useT();
  const [plugins, setPlugins] = useState<PluginSummary[]>([]);
  const [loading, setLoading] = useState(false);
  const [detail, setDetail] = useState<PluginDetail | null>(null);
  const [detailOpen, setDetailOpen] = useState(false);
  const [toggling, setToggling] = useState<string | null>(null);
  // 索引目录（plugin-ecosystem 3.4）
  const [indexEntries, setIndexEntries] = useState<
    { id: string; name: string; version: string; description: string; status: string; localVersion?: string; exmMin?: string }[]
  >([]);
  const [indexLoading, setIndexLoading] = useState(false);
  const [indexError, setIndexError] = useState<string | null>(null);
  const [search, setSearch] = useState("");
  const [installing, setInstalling] = useState<string | null>(null);
  // 凭据表单（详情抽屉内）
  const [credForm, setCredForm] = useState<Record<string, string>>({});
  const [credFields, setCredFields] = useState<{ key: string; label: string; sensitive: boolean }[]>([]);
  const [savingCreds, setSavingCreds] = useState(false);

  const load = useCallback(async () => {
    setLoading(true);
    try {
      const r = await api.plugins();
      setPlugins(r.plugins ?? []);
    } catch (e) {
      message.error(String(e));
    } finally {
      setLoading(false);
    }
  }, []);

  const loadIndex = useCallback(async () => {
    setIndexLoading(true);
    setIndexError(null);
    try {
      const r = await api.pluginIndex();
      setIndexEntries(r.entries ?? []);
    } catch (e) {
      setIndexError(String(e));
    } finally {
      setIndexLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
    void loadIndex();
  }, [load, loadIndex]);

  const toggle = async (id: string, enabled: boolean) => {
    setToggling(id);
    try {
      await api.setPluginEnabled(id, enabled);
      message.success(enabled ? `已启用 ${id}` : `已禁用 ${id}`);
      await load();
    } catch (e) {
      message.error(String(e));
    } finally {
      setToggling(null);
    }
  };

  const remove = async (id: string) => {
    try {
      await api.deletePlugin(id);
      message.success(`已卸载 ${id}`);
      setDetailOpen(false);
      await Promise.all([load(), loadIndex()]);
    } catch (e) {
      message.error(String(e));
    }
  };

  // 索引安装/更新（source = 索引 id；updatable 走 overwrite 语义）
  const installFromIndex = async (id: string, overwrite: boolean) => {
    setInstalling(id);
    try {
      const r = await api.installPlugin(id, overwrite);
      message.success(t("plugins.installed", { id: r.id, v: r.version }));
      await Promise.all([load(), loadIndex()]);
    } catch (e) {
      message.error(String(e));
    } finally {
      setInstalling(null);
    }
  };

  const updateFromIndex = async (id: string) => {
    setInstalling(id);
    try {
      const r = await api.updatePlugin(id);
      if (r.updated) message.success(r.message);
      else message.info(r.message);
      await Promise.all([load(), loadIndex()]);
    } catch (e) {
      message.error(String(e));
    } finally {
      setInstalling(null);
    }
  };

  // 详情抽屉打开：拉详情 + 凭据区（含清单声明的键位）
  const openDetail = async (id: string) => {
    try {
      const d = await api.pluginDetail(id);
      setDetail(d);
      setDetailOpen(true);
      // 凭据键位：清单 channel.credentials（或 manifest 凭据声明）+ 已配置键
      const fields: { key: string; label: string; sensitive: boolean }[] = [];
      const m = d.manifest as {
        channel?: { credentials?: { key: string; label?: string; sensitive?: boolean }[] };
      };
      for (const c of m.channel?.credentials ?? []) {
        fields.push({ key: c.key, label: c.label || c.key, sensitive: !!c.sensitive });
      }
      setCredFields(fields);
      // 已配置凭据回显（掩码值直接展示——提交时掩码串不覆盖真值）
      try {
        const cr = await api.pluginCredentials(id);
        setCredForm({ ...(cr.credentials ?? {}) });
        // 配置了但清单未声明的键也展示（数据面完整）
        for (const k of Object.keys(cr.credentials ?? {})) {
          if (!fields.some((f) => f.key === k)) {
            fields.push({ key: k, label: k, sensitive: true });
          }
        }
      } catch {
        setCredForm({});
      }
    } catch (e) {
      message.error(String(e));
    }
  };

  const saveCreds = async (id: string) => {
    setSavingCreds(true);
    try {
      await api.setPluginCredentials(id, credForm);
      message.success(t("plugins.credentials.saved"));
      setDetailOpen(false);
    } catch (e) {
      message.error(String(e));
    } finally {
      setSavingCreds(false);
    }
  };

  const capabilityTags = (p: PluginSummary) => {
    const tags: React.ReactNode[] = [];
    if (p.capabilities?.tools)
      tags.push(
        <Tag key="t" color="geekblue">
          {`工具 ×${p.capabilities.tools}`}
        </Tag>,
      );
    if (p.capabilities?.channel)
      tags.push(
        <Tag key="ch" color="geekblue">
          {`通道 ${p.capabilities.channel.kinds.join("/")}`}
        </Tag>,
      );
    if (p.capabilities?.watch) tags.push(<Tag key="watch" color="green">轮询</Tag>);
    if (p.capabilities?.resources)
      tags.push(<Tag key="res">{`资源 ×${p.capabilities.resources}`}</Tag>);
    if (p.capabilities?.process) tags.push(<Tag key="proc" color="orange">独立进程</Tag>);
    return tags;
  };

  const filteredIndex = indexEntries.filter((e) => {
    const q = search.trim().toLowerCase();
    if (!q) return true;
    return (
      e.id.toLowerCase().includes(q) ||
      e.name.toLowerCase().includes(q) ||
      e.description.toLowerCase().includes(q)
    );
  });

  return (
    <div className="view-scroll">
      <PageHeader
        title={t("plugins.title")}
        actions={
          <Button icon={<ReloadOutlined />} onClick={() => { void load(); void loadIndex(); }} loading={loading}>
            {t("common.refresh")}
          </Button>
        }
      />
      <Spin spinning={loading}>
        {plugins.length === 0 && !loading ? (
          <Empty description="plugins/ 目录为空——从下方目录安装，或放入插件包" />
        ) : (
          <div className="card-grid">
            {plugins.map((p) => (
              <Card
                key={p.id}
                title={
                  <Space>
                    <span>{p.name || p.id}</span>
                    <Tag color={FORM_COLOR[p.form] ?? "default"}>
                      {FORM_LABEL[p.form] ?? p.form}
                    </Tag>
                    {!p.enabled && <Tag>已禁用</Tag>}
                    {p.error && <Tag color="red">错误</Tag>}
                  </Space>
                }
                extra={
                  <Space>
                    <Switch
                      checked={p.enabled}
                      loading={toggling === p.id}
                      disabled={p.form === "virtual"}
                      onChange={(v) => void toggle(p.id, v)}
                    />
                    <Button size="small" onClick={() => void openDetail(p.id)}>
                      详情
                    </Button>
                  </Space>
                }
                onClick={() => void openDetail(p.id)}
                style={{ cursor: "pointer" }}
              >
                <Space direction="vertical" size={4}>
                  <Typography.Text type="secondary">{p.description || p.id}</Typography.Text>
                  <Space size={4} wrap>
                    {p.version && <Tag>{`v${p.version}`}</Tag>}
                    <>{capabilityTags(p)}</>
                  </Space>
                  {p.error && (
                    <Typography.Text type="danger" style={{ fontSize: 12 }}>
                      {p.error}
                    </Typography.Text>
                  )}
                </Space>
              </Card>
            ))}
          </div>
        )}
      </Spin>

      {/* 索引目录（plugin-ecosystem）：拉取/搜索/一键安装更新 */}
      <div style={{ marginTop: 24 }}>
        <PageHeader
          title={t("plugins.catalog")}
          actions={
            <Space>
              <Input
                allowClear
                prefix={<SearchOutlined />}
                placeholder={t("plugins.catalog.searchPh")}
                value={search}
                onChange={(e) => setSearch(e.target.value)}
                style={{ width: 220 }}
              />
              <Button
                icon={<CloudDownloadOutlined />}
                onClick={() => void loadIndex()}
                loading={indexLoading}
              >
                {t("plugins.catalog.refresh")}
              </Button>
            </Space>
          }
        />
        {indexError ? (
          <Empty description={t("plugins.catalog.fetchFail", { err: indexError })} />
        ) : filteredIndex.length === 0 && !indexLoading ? (
          <Empty description={search ? t("plugins.catalog.noMatch") : t("plugins.catalog.empty")} />
        ) : (
          <Table
            rowKey="id"
            size="small"
            loading={indexLoading}
            dataSource={filteredIndex}
            pagination={false}
            columns={[
              {
                title: "插件",
                key: "plugin",
                render: (_, e) => (
                  <Space direction="vertical" size={0}>
                    <Space size={8}>
                      <Typography.Text strong>{e.name || e.id}</Typography.Text>
                      <Typography.Text type="secondary" style={{ fontSize: 12 }}>
                        {e.id}
                      </Typography.Text>
                    </Space>
                    {e.description && (
                      <Typography.Text type="secondary" style={{ fontSize: 12 }}>
                        {e.description}
                      </Typography.Text>
                    )}
                  </Space>
                ),
              },
              {
                title: "版本",
                key: "version",
                width: 140,
                render: (_, e) => (
                  <Space direction="vertical" size={0}>
                    <span>{`v${e.version}`}</span>
                    {e.localVersion && (
                      <Typography.Text type="secondary" style={{ fontSize: 12 }}>
                        {t("plugins.catalog.localVer", { v: e.localVersion })}
                      </Typography.Text>
                    )}
                    {e.exmMin && (
                      <Typography.Text type="secondary" style={{ fontSize: 12 }}>
                        {t("plugins.catalog.exmMin", { v: e.exmMin })}
                      </Typography.Text>
                    )}
                  </Space>
                ),
              },
              {
                title: "状态",
                dataIndex: "status",
                width: 110,
                render: (s: string) => {
                  const meta = INDEX_STATUS[s];
                  return meta ? (
                    <Tag color={meta.color}>{t(meta.key)}</Tag>
                  ) : (
                    <Tag>{s}</Tag>
                  );
                },
              },
              {
                title: "操作",
                key: "actions",
                width: 140,
                render: (_, e) => {
                  if (e.status === "installed")
                    return (
                      <Button size="small" disabled>
                        {t("plugins.catalog.latest")}
                      </Button>
                    );
                  if (e.status === "incompatible")
                    return (
                      <Button size="small" disabled>
                        {t("plugins.catalog.hostLow")}
                      </Button>
                    );
                  if (e.status === "updatable")
                    return (
                      <Button
                        size="small"
                        type="primary"
                        loading={installing === e.id}
                        onClick={() => void updateFromIndex(e.id)}
                      >
                        {t("plugins.catalog.update")}
                      </Button>
                    );
                  return (
                    <Button
                      size="small"
                      type="primary"
                      loading={installing === e.id}
                      onClick={() => void installFromIndex(e.id, false)}
                    >
                      {t("plugins.catalog.install")}
                    </Button>
                  );
                },
              },
            ]}
          />
        )}
      </div>

      {/* 详情抽屉：清单/健康/快照/来源 + 凭据表单 */}
      <Drawer
        title={
          detail ? (
            <Space>
              <span>{detail.name || detail.id}</span>
              <Tag color={FORM_COLOR[detail.form] ?? "default"}>
                {FORM_LABEL[detail.form] ?? detail.form}
              </Tag>
            </Space>
          ) : (
            ""
          )
        }
        open={detailOpen}
        onClose={() => setDetailOpen(false)}
        width={520}
        destroyOnClose
      >
        {detail && (
          <Space direction="vertical" size={16} style={{ width: "100%" }}>
            <Descriptions
              column={1}
              size="small"
              bordered
              items={[
                { key: "id", label: "id", children: detail.id },
                { key: "ver", label: "版本", children: detail.version || "-" },
                { key: "form", label: "形态", children: FORM_LABEL[detail.form] ?? detail.form },
                { key: "health", label: "健康", children: detail.health },
                ...(detail.error
                  ? [{ key: "err", label: "错误", children: detail.error }]
                  : []),
                ...(detail.toolsSnapshot && detail.toolsSnapshot.length > 0
                  ? [
                      {
                        key: "snap",
                        label: t("plugins.detail.snapshot"),
                        children: detail.toolsSnapshot.join("、"),
                      },
                    ]
                  : []),
                ...(detail.installation
                  ? [
                      {
                        key: "src",
                        label: t("plugins.detail.source"),
                        children: `${detail.installation.kind}：${detail.installation.url}${
                          detail.installation.subdir ? `（子目录 ${detail.installation.subdir}）` : ""
                        }`,
                      },
                    ]
                  : []),
              ]}
            />
            {/* 凭据表单：键位来自清单声明 + 已配置键；敏感值回显掩码、掩码直存不覆盖 */}
            <Card
              size="small"
              title={t("plugins.credentials")}
              extra={
                detail.credentialsConfigured ? (
                  <Tag color="green">{t("plugins.credentials.configured")}</Tag>
                ) : (
                  <Tag>{t("plugins.credentials.none")}</Tag>
                )
              }
            >
              {credFields.length === 0 ? (
                <Typography.Text type="secondary">
                  {t("plugins.credentials.noFields")}
                </Typography.Text>
              ) : (
                <Space direction="vertical" size={8} style={{ width: "100%" }}>
                  {credFields.map((f) => (
                    <Space key={f.key} direction="vertical" size={2} style={{ width: "100%" }}>
                      <Typography.Text style={{ fontSize: 12 }}>
                        {f.label}
                        {f.sensitive ? t("plugins.credentials.sensitive") : ""}
                      </Typography.Text>
                      <Input.Password
                        value={credForm[f.key] ?? ""}
                        placeholder={f.sensitive ? t("plugins.credentials.phSensitive") : ""}
                        onChange={(e) => setCredForm({ ...credForm, [f.key]: e.target.value })}
                      />
                    </Space>
                  ))}
                  <Button
                    type="primary"
                    size="small"
                    loading={savingCreds}
                    onClick={() => void saveCreds(detail.id)}
                  >
                    {t("plugins.credentials.save")}
                  </Button>
                  <Typography.Text type="secondary" style={{ fontSize: 12 }}>
                    {t("plugins.credentials.hint")}
                  </Typography.Text>
                </Space>
              )}
            </Card>
            <Space>
              {detail.form !== "builtin" && detail.form !== "virtual" && (
                <Popconfirm title={`确认卸载 ${detail.id}？目录与来源记录将一并清理。`} onConfirm={() => void remove(detail.id)}>
                  <Button danger>
                    卸载
                  </Button>
                </Popconfirm>
              )}
              {detail.installation && (
                <Button loading={installing === detail.id} onClick={() => void updateFromIndex(detail.id)}>
                  {t("plugins.detail.checkUpdate")}
                </Button>
              )}
            </Space>
          </Space>
        )}
      </Drawer>
    </div>
  );
}
