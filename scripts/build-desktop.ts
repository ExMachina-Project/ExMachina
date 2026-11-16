import { spawnSync } from "node:child_process";
import path from "node:path";
import { fileURLToPath } from "node:url";

async function main(): Promise<void> {
  const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
  const check = process.argv.includes("--check");
  const result = spawnSync(process.execPath, [
    path.join(root, "node_modules/typescript/bin/tsc"),
    "-p", path.join(root, "desktop/tsconfig.json"),
  ], { cwd: root, stdio: "inherit" });
  if (result.error) throw result.error;
  if (result.status !== 0) { process.exitCode = result.status ?? 1; return; }
  if (check) return;
  const build = spawnSync(process.execPath, [path.join(root, "node_modules/vite/bin/vite.js"), "build", "--config", "desktop/vite.config.ts"], { cwd: root, stdio: "inherit" });
  if (build.error) throw build.error;
  process.exitCode = build.status ?? 1;
}
void main().catch((error: unknown) => { console.error(error); process.exitCode = 1; });
