//! api/workbench.ts —— 工作台域（7.1 拆分）：文件树（对话内 @ 文件提及数据源）。纯搬移。


// ---------------- 文件提及（对话即编码：chat 内 @ 引用） ----------------

export interface WorkbenchNode {
  path: string;
  name: string;
  type: "dir" | "file";
  size?: number;
}

export interface WorkbenchTree {
  root: string;
  count: number;
  nodes: WorkbenchNode[];
}

/** 工作台文件树（.gitignore 口径过滤；工作区边界校验在服务端）：对话内 @ 文件提及的数据源 */
