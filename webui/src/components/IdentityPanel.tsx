/** 通道身份面板：配对码签发 / 绑定清单管理（capability-completion 组 3.4）
 *  数据源：/api/identity/list · /api/identity/code · /api/identity/role · /api/identity/:id
 *  两种形态：tab 页内联（expanded，恒展开）；旧嵌入位（缺省）保留折叠交互。 */
import React, { useCallback, useEffect, useState } from "react";
import { Badge, Button, Card, Input, Popconfirm, Space, Table, Tag, Typography, message } from "antd";
import { CodeOutlined, ReloadOutlined } from "@ant-design/icons";
import { identityIssue, identityList, identityPurgeCodes, identityRevokeCode, identitySetRole, identityUnbind } from "../api";
import { useT } from "../i18n/core";

interface IdentityItem {
  id: string;
  channel: string;
  externalId: string;
  displayName: string;
  role: string;
  pairedAt: string;
  note: string;
}
interface CodeItem {
  code: string;
  note: string;
  platform?: string | null;
  createdAt: string;
  expiresAtMs: number;
  expired: boolean;
  role?: string;
}
interface IdentityList {
  identityRequired: boolean;
  identities: IdentityItem[];
  codes: CodeItem[];
}

export function IdentityPanel({ expanded = false }: { expanded?: boolean }): React.ReactElement {
  const t = useT();
  const [data, setData] = useState<IdentityList | null>(null);
  const [note, setNote] = useState("");
  const [asAdmin, setAsAdmin] = useState(false);
  const [issued, setIssued] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [open, setOpen] = useState(false);

  const load = useCallback(async () => {
    try {
      setData(await identityList());
    } catch {
      /* 静默 */
    }
  }, []);
  useEffect(() => {
    void load();
  }, [load]);

  const issue = async () => {
    setBusy(true);
    try {
      const r = await identityIssue(note.trim(), asAdmin ? "admin" : "member");
      setIssued(r.code);
      setNote("");
      setOpen(true); // 签发即展开：码限时有效，置顶可见
      message.success(asAdmin ? t("identity.issuedAdmin") : t("identity.issued"));
      await load();
    } catch (e) {
      message.error(String(e).slice(0, 160));
    } finally {
      setBusy(false);
    }
  };

  const setRole = async (id: string, role: "admin" | "member") => {
    try {
      await identitySetRole(id, role);
      await load();
    } catch (e) {
      message.error(String(e).slice(0, 160));
    }
  };

  const unbind = async (id: string) => {
    try {
      await identityUnbind(id);
      message.success(t("identity.unbound"));
      await load();
    } catch (e) {
      message.error(String(e).slice(0, 160));
    }
  };

  const revokeCode = async (code: string) => {
    try {
      await identityRevokeCode(code);
      message.success(t("identity.revoked"));
      await load();
    } catch (e) {
      message.error(String(e).slice(0, 160));
    }
  };

  const purgeExpired = async () => {
    try {
      const r = await identityPurgeCodes();
      message.success(t("identity.purged", { n: r.purged }));
      await load();
    } catch (e) {
      message.error(String(e).slice(0, 160));
    }
  };

  const expiredCount = (data?.codes ?? []).filter((c) => c.expired).length;

  const liveCodes = (data?.codes ?? []).filter((c) => !c.expired).length;
  const bound = data?.identities.length ?? 0;

  const body = (
    <>
      <Space.Compact style={{ width: "100%", marginBottom: 12 }}>
        <Input
          value={note}
          placeholder={t("identity.notePlaceholder")}
          onChange={(e) => setNote(e.target.value)}
          onPressEnter={() => void issue()}
        />
        <Button type="primary" loading={busy} onClick={() => void issue()}>
          {t("identity.issue")}
        </Button>
      </Space.Compact>
      {/* 预设角色：勾选后兑换即落 admin（管理者配对码），免二次手动提权 */}
      <label className="identity-admin-check" style={{ display: "flex", alignItems: "center", gap: 6, marginBottom: 12, cursor: "pointer" }}>
        <input type="checkbox" checked={asAdmin} onChange={(e) => setAsAdmin(e.target.checked)} />
        <span className="dim">{t("identity.asAdmin")}</span>
      </label>
      {issued && (
        <div className="identity-issued">
          <Tag color="green">/pair {issued}</Tag>
          <span className="dim">{t("identity.hint")}</span>
        </div>
      )}
          <Table
            size="small"
            rowKey="code"
            dataSource={data?.codes ?? []}
            pagination={false}
            columns={[
              {
                title: t("identity.code"),
                dataIndex: "code",
                render: (c: string, r: CodeItem) =>
                  r.expired ? <span className="dim">{c}</span> : <Tag color="blue">/pair {c}</Tag>,
              },
              { title: t("identity.note"), dataIndex: "note" },
              { title: t("identity.platform"), dataIndex: "platform", render: (p?: string) => p ?? t("identity.any") },
              {
                title: t("identity.role"),
                dataIndex: "role",
                render: (r?: string) =>
                  r === "admin" ? <Tag color="gold">admin</Tag> : <Tag>member</Tag>,
              },
              {
                title: t("identity.status"),
                dataIndex: "expired",
                render: (x: boolean) =>
                  x ? <Tag color="default">{t("identity.expired")}</Tag> : <Tag color="processing">{t("identity.active")}</Tag>,
              },
              {
                title: "",
                key: "op",
                width: 80,
                render: (_: unknown, r: CodeItem) => (
                  <Popconfirm
                    title={r.expired ? t("identity.revokeExpiredConfirm") : t("identity.revokeConfirm")}
                    onConfirm={() => void revokeCode(r.code)}
                  >
                    <Button size="small" type="text" danger>
                      {t("identity.revoke")}
                    </Button>
                  </Popconfirm>
                ),
              },
            ]}
          />
          <Table
            size="small"
            style={{ marginTop: 12 }}
            rowKey="id"
            dataSource={data?.identities ?? []}
            pagination={false}
            columns={[
              { title: t("identity.user"), dataIndex: "displayName" },
              { title: t("identity.channel"), dataIndex: "channel" },
              { title: t("identity.externalId"), dataIndex: "externalId" },
              {
                title: t("identity.role"),
                dataIndex: "role",
                render: (role: string, r: IdentityItem) => (
                  <Space>
                    {role === "admin" ? <Tag color="gold">admin</Tag> : <Tag>member</Tag>}
                    {role === "member" ? (
                      <Button size="small" type="link" onClick={() => void setRole(r.id, "admin")}>
                        {t("identity.toAdmin")}
                      </Button>
                    ) : (
                      <Button size="small" type="link" onClick={() => void setRole(r.id, "member")}>
                        {t("identity.toMember")}
                      </Button>
                    )}
                  </Space>
                ),
              },
              { title: t("identity.pairedAt"), dataIndex: "pairedAt" },
              {
                title: "",
                key: "op",
                render: (_: unknown, r: IdentityItem) => (
                  <Popconfirm title={t("identity.unbindConfirm")} onConfirm={() => void unbind(r.id)}>
                    <Button size="small" danger type="text">
                      {t("identity.unbind")}
                    </Button>
                  </Popconfirm>
                ),
              },
            ]}
          />
    </>
  );

  // tab 内联形态：无卡片壳，直接铺内容
  if (expanded) return <div className="identity-inline">{body}</div>;

  // 嵌入形态：卡片 + 折叠交互
  return (
    <Card
      size="small"
      className="identity-panel"
      title={
        <Space size={8} onDoubleClick={() => setOpen((v) => !v)}>
          <CodeOutlined />
          {t("identity.title")}
        </Space>
      }
      extra={
        <Space size={4}>
          {expiredCount > 0 && (
            <Popconfirm title={t("identity.purgeConfirm")} onConfirm={() => void purgeExpired()}>
              <Button size="small" type="text" danger>
                {t("identity.purge", { n: expiredCount })}
              </Button>
            </Popconfirm>
          )}
          {(liveCodes > 0 || bound > 0) && (
            <Badge count={liveCodes} title={t("identity.codeCount", { n: liveCodes })}>
              <Tag style={{ marginInlineEnd: 0 }} color={liveCodes ? "processing" : "default"}>
                {t("identity.summary", { codes: liveCodes, bound })}
              </Tag>
            </Badge>
          )}
          <Button size="small" type="text" icon={<ReloadOutlined />} onClick={() => void load()} />
        </Space>
      }
    >
      {open ? (
        body
      ) : (
        <div className="identity-collapsed">
          <Space.Compact style={{ flex: 1, minWidth: 0 }}>
            <Input
              value={note}
              placeholder={t("identity.notePlaceholder")}
              onChange={(e) => setNote(e.target.value)}
              onPressEnter={() => void issue()}
            />
            <Button type="primary" loading={busy} onClick={() => void issue()}>
              {t("identity.issue")}
            </Button>
          </Space.Compact>
          <Button type="link" size="small" onClick={() => setOpen(true)}>
            {t("identity.expand")}
          </Button>
        </div>
      )}
    </Card>
  );
}
