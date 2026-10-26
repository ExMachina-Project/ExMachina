/** 代码工作台（spec: webui-code-workbench）：文件树 + 内容分页浏览 + 在会话中引用 */
import React, { useCallback, useEffect, useMemo, useState } from "react";
import { Button, Empty, Input, Pagination, Spin, Tag, Tree, Typography, message } from "antd";
import {
  FileOutlined,
  FolderOutlined,
  CodeOutlined,
  SendOutlined,
  ReloadOutlined,
} from "@ant-design/icons";
import type { DataNode } from "antd/es/tree";
import { workbenchFile, workbenchTree, type WorkbenchFile, type WorkbenchNode } from "../api";
import { useExm } from "../store";

const { Text, Paragraph } = Typography;

/** 平铺节点 → antd Tree DataNode（按目录分组嵌套） */
function buildTreeData(nodes: WorkbenchNode[]): DataNode[] {
  // 按路径段构造嵌套：dir 节点带空 children 占位，file 为叶子
  const dirMap = new Map<string, DataNode & { children: DataNode[] }>();
  const roots: (DataNode & { children: DataNode[] })[] = [];
  const ensureDir = (segs: string[], full: string) => {
    let list = roots;
    let prefix = "";
    for (const seg of segs) {
      prefix = prefix ? `${prefix}/${seg}` : seg;
      let node = dirMap.get(prefix);
      if (!node) {
        node = {
          key: `d:${prefix}`,
          title: (
            <span>
              <FolderOutlined /> {seg}
            </span>
          ),
          children: [],
        };
        dirMap.set(prefix, node);
        list.push(node);
      }
      list = node.children as (DataNode & { children: DataNode[] })[];
    }
    return list;
  };
  // 目录优先（服务端已排序），文件挂到对应目录
  for (const n of nodes) {
    if (n.type === "dir") {
      ensureDir(n.path.split("/"), n.path);
    }
  }
  for (const n of nodes) {
    if (n.type === "file") {
      const segs = n.path.split("/");
      const name = segs.pop()!;
      const list = segs.length ? ensureDir(segs, segs.join("/")) : roots;
      list.push({
        key: `f:${n.path}`,
        title: (
          <span>
            <FileOutlined /> {name}
          </span>
        ),
        isLeaf: true,
        children: [],
      });
    }
  }
  return roots;
}

export function CodeWorkbenchView(): React.ReactElement {
  const [tree, setTree] = useState<WorkbenchNode[] | null>(null);
  const [treeErr, setTreeErr] = useState("");
  const [loadingTree, setLoadingTree] = useState(false);
  const [file, setFile] = useState<WorkbenchFile | null>(null);
  const [filePath, setFilePath] = useState("");
  const [loadingFile, setLoadingFile] = useState(false);
  const [page, setPage] = useState(1);
  const [filter, setFilter] = useState("");
  const [expanded, setExpanded] = useState<React.Key[]>([]);
  const requestNav = useExm((s) => s.requestNav);
  const drafts = useExm((s) => s.drafts);
  const setDraft = useExm((s) => s.setDraft);
  const sessionId = useExm((s) => s.sessionId);

  const loadTree = useCallback(async () => {
    setLoadingTree(true);
    setTreeErr("");
    try {
      const t = await workbenchTree("", 6);
      setTree(t.nodes);
    } catch (e) {
      setTreeErr(String(e));
    } finally {
      setLoadingTree(false);
    }
  }, []);

  useEffect(() => {
    void loadTree();
  }, [loadTree]);

  const openFile = useCallback(async (p: string, pg = 1) => {
    setLoadingFile(true);
    setFilePath(p);
    setPage(pg);
    try {
      const f = await workbenchFile(p, pg, 400);
      setFile(f);
    } catch (e) {
      setFile(null);
      void message.error(`读取失败：${e}`);
    } finally {
      setLoadingFile(false);
    }
  }, []);

  /** 在会话中引用：把文件路径插入当前会话输入草稿并跳会话页 */
  const insertRef = useCallback(
    (p: string) => {
      if (!sessionId) {
        void message.warning("无活动会话：先到对话页开启会话");
        return;
      }
      const cur = drafts[sessionId] ?? "";
      const ref = `请查看 ${p} `;
      setDraft(sessionId, cur ? `${cur}\n${ref}` : ref);
      void message.success("已插入会话输入区");
      requestNav("chat", sessionId);
    },
    [sessionId, drafts, setDraft, requestNav],
  );

  const treeData = useMemo(() => (tree ? buildTreeData(tree) : []), [tree]);

  const filtered = useMemo(() => {
    if (!filter.trim() || !tree) return null;
    const kw = filter.trim().toLowerCase();
    return tree.filter((n) => n.path.toLowerCase().includes(kw));
  }, [filter, tree]);

  return (
    <div style={{ display: "flex", gap: 12, height: "100%", minHeight: 0 }}>
      {/* 文件树 */}
      <div
        style={{
          width: 300,
          minWidth: 240,
          borderRight: "1px solid var(--ant-color-border-secondary, #eee)",
          paddingRight: 8,
          display: "flex",
          flexDirection: "column",
          minHeight: 0,
        }}
      >
        <div style={{ display: "flex", gap: 6, marginBottom: 8 }}>
          <Input
            size="small"
            placeholder="过滤路径…"
            value={filter}
            onChange={(e) => setFilter(e.target.value)}
            allowClear
          />
          <Button size="small" icon={<ReloadOutlined />} onClick={() => void loadTree()} title="刷新" />
        </div>
        <div style={{ flex: 1, overflow: "auto", minHeight: 0 }}>
          {loadingTree ? (
            <Spin size="small" />
          ) : treeErr ? (
            <Text type="danger">树加载失败：{treeErr}</Text>
          ) : filtered ? (
            filtered.length === 0 ? (
              <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description="无匹配" />
            ) : (
              filtered.slice(0, 200).map((n) => (
                <div
                  key={n.path}
                  style={{ cursor: "pointer", padding: "2px 4px", whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis" }}
                  onClick={() => n.type === "file" && void openFile(n.path)}
                >
                  {n.type === "dir" ? <FolderOutlined style={{ color: "#faad14" }} /> : <FileOutlined />}{" "}
                  <Text style={{ fontSize: 12 }}>{n.path}</Text>
                </div>
              ))
            )
          ) : (
            <Tree
              showLine={{ showLeafIcon: false }}
              treeData={treeData}
              expandedKeys={expanded}
              onExpand={(keys) => setExpanded(keys)}
              onSelect={(keys) => {
                const k = String(keys[0] ?? "");
                if (k.startsWith("f:")) void openFile(k.slice(2));
              }}
            />
          )}
        </div>
      </div>

      {/* 内容区 */}
      <div style={{ flex: 1, display: "flex", flexDirection: "column", minWidth: 0 }}>
        {filePath ? (
          <>
            <div style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 8 }}>
              <CodeOutlined />
              <Text strong style={{ fontFamily: "monospace" }}>{filePath}</Text>
              {file && <Tag>{file.totalLines} 行</Tag>}
              <Button size="small" icon={<SendOutlined />} onClick={() => insertRef(filePath)}>
                在会话中引用
              </Button>
            </div>
            <div style={{ flex: 1, overflow: "auto", minHeight: 0, background: "var(--ant-color-bg-layout, #fafafa)", borderRadius: 6, padding: 8 }}>
              {loadingFile ? (
                <Spin />
              ) : file ? (
                <pre style={{ margin: 0, fontSize: 12, fontFamily: "ui-monospace, monospace", whiteSpace: "pre" }}>
                  {file.content}
                </pre>
              ) : (
                <Text type="secondary">读取失败</Text>
              )}
            </div>
            {file && file.totalLines > file.limit && (
              <div style={{ marginTop: 8, textAlign: "right" }}>
                <Pagination
                  size="small"
                  current={file.offset}
                  pageSize={file.limit}
                  total={file.totalLines}
                  showSizeChanger={false}
                  onChange={(pg) => void openFile(filePath, pg)}
                />
              </div>
            )}
          </>
        ) : (
          <div style={{ flex: 1, display: "flex", alignItems: "center", justifyContent: "center" }}>
            <Empty
              image={Empty.PRESENTED_IMAGE_SIMPLE}
              description={
                <span>
                  选择左侧文件浏览
                  <br />
                  <Text type="secondary" style={{ fontSize: 12 }}>
                    工作区文件树（.gitignore 口径过滤），选中文件分页查看，可一键引用到会话
                  </Text>
                </span>
              }
            />
          </div>
        )}
      </div>
    </div>
  );
}
