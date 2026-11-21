#!/usr/bin/env node
import { execFileSync } from "node:child_process";

// New long-lived documents must be reviewed with the index and ignore rules.
const allowed = new Set([
  "docs/README.md",
  "docs/开发规范.md",
  "docs/安装指南.md",
  "docs/架构与设计.md",
  "docs/协议与契约.md",
  "docs/安全模型.md",
  "docs/插件开发指南.md",
  "docs/元智能体.md",
  "docs/客户端兼容身份.md",
  "docs/智能体模型分配.md",
  "docs/command-directory.md",
  "docs/command-entry-matrix.md",
]);
const args = process.argv.slice(2);
if (args.some((arg) => !["--staged", "--history"].includes(arg)) || args.length > 1) {
  console.error("Usage: node scripts/check-docs.mjs [--staged|--history]");
  process.exit(2);
}
const gitArgs = args.includes("--history")
  ? ["log", "--all", "--format=", "--name-only", "-z", "--", "docs/"]
  : args.includes("--staged")
    ? ["diff", "--cached", "--name-only", "--diff-filter=ACMR", "-z", "--", "docs/"]
    : ["ls-files", "--cached", "-z", "--", "docs/"];
const paths = new Set(execFileSync("git", gitArgs, { encoding: "utf8", maxBuffer: 16 * 1024 * 1024 })
  .split("\0").map((name) => name.replace(/^\n+/, "")).filter(Boolean));
const rejected = [...paths].filter((name) => !allowed.has(name)).sort();
if (rejected.length) {
  console.error("docs/ only accepts maintained development/design documents. Rejected paths:");
  for (const name of rejected) console.error(`  ${name}`);
  console.error("Keep screenshots, audit/acceptance reports and execution artifacts outside the repository.");
  process.exit(1);
}
console.log(`Documentation policy passed (${paths.size} paths${args.includes("--history") ? ", all local refs" : ""}).`);
