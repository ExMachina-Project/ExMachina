/** 插件管理：四层扩展模型（数据/声明式/进程/内置）的统一管理面 */
import React, { useCallback, useEffect, useState } from "react";
import {
  Button,
  Card,
  Descriptions,
  Drawer,
  Empty,
  Popconfirm,
  Space,
  Spin,
  Switch,
  Tag,
  Typography,
  message,
} from "antd";
import { ReloadOutlined } from "@ant-design/icons";
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

export function PluginsView(): React.ReactElement {
  const t = useT();
  const [plugins, setPlugins] = useState<PluginSummary[]>([]);
  const [loading, setLoading] = useState(false);
  const [detail, setDetail] = useState<PluginDetail | null>(null);
  const [detailOpen, setDetailOpen] = useState(false);
  const [toggling, setToggling] = useState<string | null>(null);

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

  useEffect(() => {
    void load();
  }, [load]);

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
      await load();
    } catch (e) {
      message.error(String(e));
    }
  };

  const openDetail = async (id: string) => {
    try {
      const d = await api.pluginDetail(id);
      setDetail(d);
      setDetailOpen(true);
    } catch (e) {
      message.error(String(e));
    }
  };

  /** 能力计数徽标 */
  const capabilityTags = (p: PluginSummary): React.ReactNode[] => {
    const tags: React.ReactNode[] = [];
    if (p.capabilities?.tools) tags.push(<Tag key="tools">{`工具 ×${p.capabilities.tools}`}</Tag>);
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

  return (
    <div className="view-scroll">
      <PageHeader
        title={t("plugins.title")}
        actions={
          <Button icon={<ReloadOutlined />} onClick={() => void load()} loading={loading}>
            {t("common.refresh")}
          </Button>
        }
      />
      <Spin spinning={loading}>
        {plugins.length === 0 && !loading ? (
          <Empty description="plugins/ 目录为空——放入插件包或查看文档" />
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
                    {(p.form === "declarative" || p.form === "process") && (
                      <Popconfirm
                        title={`卸载 ${p.id}？`}
                        description="将删除 plugins/ 下的插件目录"
                        onConfirm={() => void remove(p.id)}
                      >
                        <Button size="small" danger>
                          卸载
                        </Button>
                      </Popconfirm>
                    )}
                  </Space>
                }
              >
                <Typography.Paragraph type="secondary" ellipsis={{ rows: 2 }}>
                  {p.description || p.id}
                </Typography.Paragraph>
                <Space wrap size={[4, 8]}>
                  <>{capabilityTags(p)}</>
                  {p.version && <Tag bordered={false}>{`v${p.version}`}</Tag>}
                </Space>
                {p.error && (
                  <Typography.Paragraph type="danger" style={{ marginTop: 8, marginBottom: 0 }}>
                    {p.error}
                  </Typography.Paragraph>
                )}
              </Card>
            ))}
          </div>
        )}
      </Spin>

      <Drawer
        title={detail ? `${detail.name || detail.id}（${FORM_LABEL[detail.form] ?? detail.form}）` : ""}
        width={520}
        open={detailOpen}
        destroyOnHidden
        onClose={() => setDetailOpen(false)}
      >
        {detail && (
          <>
            <Descriptions column={1} size="small" bordered>
              <Descriptions.Item label="id">{detail.id}</Descriptions.Item>
              <Descriptions.Item label="版本">{detail.version || "—"}</Descriptions.Item>
              <Descriptions.Item label="状态">
                <Tag color={detail.enabled ? "green" : "default"}>
                  {detail.enabled ? "启用" : "禁用"}
                </Tag>
                <Tag color={detail.health === "ok" ? "green" : "red"}>{detail.health}</Tag>
              </Descriptions.Item>
              {detail.error && (
                <Descriptions.Item label="错误">
                  <Typography.Text type="danger">{detail.error}</Typography.Text>
                </Descriptions.Item>
              )}
            </Descriptions>
            <Typography.Title level={5} style={{ marginTop: 16 }}>
              清单（plugin.json）
            </Typography.Title>
            <pre className="code-block" style={{ maxHeight: 360, overflow: "auto" }}>
              {JSON.stringify(detail.manifest, null, 2)}
            </pre>
          </>
        )}
      </Drawer>
    </div>
  );
}
