/**
 * 知识库视图：实例级共享知识的清单 / 检索 / 新增 / 删除
 * 与记忆的分工：记忆 = 本组经历（分区隔离）；知识 = 全智能体通用客观结论
 */
import React, { useCallback, useEffect, useState } from "react";
import {
  Button,
  Card,
  Input,
  List,
  message,
  Modal,
  Popconfirm,
  Space,
  Tag,
  Tooltip,
} from "antd";
import { BrandEmpty } from "../components/BrandEmpty";
import {
  BookOutlined,
  DeleteOutlined,
  PlusOutlined,
  ReloadOutlined,
  SearchOutlined,
} from "@ant-design/icons";
import { api, type KnowledgeEntry } from "../api";
import { PageHeader } from "../components/PageHeader";
import { useT } from "../i18n/core";

export function KnowledgeView(): React.ReactElement {
  const t = useT();
  const [entries, setEntries] = useState<KnowledgeEntry[]>([]);
  const [query, setQuery] = useState("");
  const [searching, setSearching] = useState(false);
  // 新增弹窗
  const [adding, setAdding] = useState(false);
  const [addTitle, setAddTitle] = useState("");
  const [addBody, setAddBody] = useState("");
  const [addTags, setAddTags] = useState("");
  const [saving, setSaving] = useState(false);

  const reload = useCallback(async () => {
    try {
      if (query.trim()) {
        const r = await api.listKnowledge({ q: query.trim(), limit: 50 });
        setEntries(r.items);
      } else {
        const r = await api.listKnowledge({ limit: 50 });
        setEntries(r.items);
      }
    } catch (e) {
      message.error(String(e));
    }
  }, [query]);

  useEffect(() => {
    void reload();
  }, [reload]);

  const doSearch = async () => {
    setSearching(true);
    try {
      await reload();
    } finally {
      setSearching(false);
    }
  };

  const doAdd = async () => {
    if (!addTitle.trim() || !addBody.trim()) {
      message.warning(t("knowledge.addRequired"));
      return;
    }
    setSaving(true);
    try {
      const tags = addTags
        .split(",")
        .map((s) => s.trim())
        .filter(Boolean);
      const r = await api.addKnowledge({ title: addTitle.trim(), body: addBody.trim(), tags });
      message.success(
        r.deduped ? t("knowledge.addDeduped", { id: r.entry.id }) : t("knowledge.added"),
      );
      setAdding(false);
      setAddTitle("");
      setAddBody("");
      setAddTags("");
      void reload();
    } catch (e) {
      message.error(String(e));
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className="memory-wrap">
      <PageHeader en="KNOWLEDGE" title={t("nav.knowledge")} desc={t("knowledge.desc")} />
      <Card
        size="small"
        className="memory-card"
        title={t("knowledge.searchCard")}
        extra={
          <Space size={[6, 6]}>
            <Button size="small" icon={<ReloadOutlined />} onClick={() => void reload()}>
              {t("common.refresh")}
            </Button>
            <Button size="small" type="primary" icon={<PlusOutlined />} onClick={() => setAdding(true)}>
              {t("knowledge.add")}
            </Button>
          </Space>
        }
      >
        <Space.Compact style={{ width: "100%" }}>
          <Input
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder={t("knowledge.searchPh")}
            onPressEnter={() => void doSearch()}
          />
          <Button
            type="primary"
            icon={<SearchOutlined />}
            loading={searching}
            onClick={() => void doSearch()}
          >
            {t("knowledge.search")}
          </Button>
        </Space.Compact>
        <List
          className="memory-hits"
          size="small"
          dataSource={entries}
          locale={{ emptyText: <BrandEmpty description={t("knowledge.empty")} /> }}
          renderItem={(e) => (
            <List.Item
              actions={[
                <Popconfirm
                  key="del"
                  title={t("knowledge.delConfirm")}
                  onConfirm={async () => {
                    try {
                      await api.deleteKnowledge(e.id);
                      void reload();
                    } catch (err) {
                      message.error(String(err));
                    }
                  }}
                >
                  <Button size="small" type="text" danger icon={<DeleteOutlined />} />
                </Popconfirm>,
              ]}
            >
              <Space orientation="vertical" size={0} style={{ width: "100%" }}>
                <Space wrap>
                  <BookOutlined />
                  <b>{e.title}</b>
                  <Tag>hits {e.hits}</Tag>
                  <Tooltip title={`${t("knowledge.source")}: ${e.sourceAgent}`}>
                    <span className="memory-reason">{e.sourceAgent}</span>
                  </Tooltip>
                  {e.tags.map((tg) => (
                    <Tag key={tg}>{tg}</Tag>
                  ))}
                </Space>
                <span className="memory-body">{e.body.slice(0, 200)}</span>
                <span className="memory-reason">
                  {e.id} · {e.updatedAt}
                </span>
              </Space>
            </List.Item>
          )}
        />
      </Card>

      <Modal
        title={t("knowledge.addTitle")}
        open={adding}
        onCancel={() => setAdding(false)}
        onOk={() => void doAdd()}
        confirmLoading={saving}
        okText={t("common.save")}
        cancelText={t("common.cancel")}
        destroyOnHidden
      >
        <Space orientation="vertical" style={{ width: "100%" }}>
          <div>
            <div className="dim">{t("knowledge.fieldTitle")}</div>
            <Input
              value={addTitle}
              onChange={(e) => setAddTitle(e.target.value)}
              maxLength={200}
              placeholder={t("knowledge.titlePh")}
            />
          </div>
          <div>
            <div className="dim">{t("knowledge.fieldBody")}</div>
            <Input.TextArea
              value={addBody}
              onChange={(e) => setAddBody(e.target.value)}
              rows={8}
              maxLength={8000}
              placeholder={t("knowledge.bodyPh")}
            />
          </div>
          <div>
            <div className="dim">{t("knowledge.fieldTags")}</div>
            <Input
              value={addTags}
              onChange={(e) => setAddTags(e.target.value)}
              placeholder={t("knowledge.tagsPh")}
            />
          </div>
        </Space>
      </Modal>
    </div>
  );
}
