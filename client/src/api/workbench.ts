//! Workbench file-tree types shared by client applications.
export interface WorkbenchNode {
  path: string;
  name: string;
  type: "dir" | "file";
  size?: number;
}

/** Workspace file tree used by file-mention pickers. */
export interface WorkbenchTree {
  root: string;
  count: number;
  nodes: WorkbenchNode[];
}
