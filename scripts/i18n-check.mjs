/**
 * i18n 词条体检：抓「源码引用了但词典没有收录」的 key。
 *
 * 为什么需要它：i18n 的取词函数签名是 `t(TArg)`，而
 * `TArg = TKey | (string & {})` 为了容纳后端下发的动态 key 放宽了类型，
 * 拼错的静态 key 在编译期不会被发现；未收录的 key 又会静默返回 key 原文，
 * 于是界面会直接显示 `approvals.colSession` 这样的裸键。
 * （回归：2026-10 审批页列头与通道页 toast 均曾如此）
 *
 * 用法：node scripts/i18n-check.mjs        缺失即以非零码退出
 */
import { readFileSync, readdirSync, statSync } from "node:fs";
import { join, extname } from "node:path";
import { fileURLToPath } from "node:url";
import { dirname } from "node:path";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
const SRC = join(ROOT, "webui/src");

function walk(dir, out = []) {
  for (const e of readdirSync(dir)) {
    const p = join(dir, e);
    if (statSync(p).isDirectory()) walk(p, out);
    else if ([".ts", ".tsx"].includes(extname(p))) out.push(p);
  }
  return out;
}

/**
 * 从取词调用里抓字面量 key：`t("a.b")` / `tr("a.b")` / `tb("a.b")`。
 *
 * 必须锚定**取词函数名**：早期版本只匹配任意 `("a.b")`，于是
 * `kind.startsWith("run.finished")`、`kind.includes("memory.write")` 这类
 * 业务字符串被判成缺失词条 —— 假阳性会让检查不可信，真缺失反而被淹没。
 */
const TAKE = String.raw`(?:t|tr|tb|translate)\s*\(\s*"([a-z][A-Za-z0-9]*(?:\.[A-Za-z0-9_]+)+)"`;
function keysIn(text) {
  const keys = new Set();
  const re = new RegExp(TAKE, "g");
  let m;
  while ((m = re.exec(text))) keys.add(m[1]);
  return keys;
}

function dictKeys(file) {
  const text = readFileSync(file, "utf8");
  const set = new Set();
  let m;
  const re = /^\s*"([A-Za-z0-9_.]+)":/gm;
  while ((m = re.exec(text))) set.add(m[1]);
  return set;
}

const files = walk(SRC).filter((f) => !f.includes(`${join("i18n", "langs")}`) && !f.endsWith(`${join("i18n", "core.ts")}`));
const used = new Map(); // key -> [file:line]
for (const f of files) {
  const rel = f.replace(SRC + "/", "");
  readFileSync(f, "utf8").split("\n").forEach((line, i) => {
    for (const k of keysIn(line)) {
      if (!used.has(k)) used.set(k, []);
      used.get(k).push(`${rel}:${i + 1}`);
    }
  });
}

const zh = dictKeys(join(SRC, "i18n/langs/zh.ts"));
const en = dictKeys(join(SRC, "i18n/langs/en.ts"));

// 前缀取词锚定取词函数后不再需要「哪些前缀不是词条」的例外名单：
// 校验过一遍收紧后依然零缺失（配置字段名等都在 `t()` 之外，不进 used）。
const missingZh = [];
for (const [key, uses] of used) {
  if (!zh.has(key) && !key.startsWith("exm.")) missingZh.push({ key, uses });
}
const missingEn = [...en].length === 0 ? [] : [];
for (const k of zh) if (!en.has(k)) missingEn.push(k);

console.log(`引用 key ${used.size} 个 · zh 词条 ${zh.size} · en 词条 ${en.size}`);
if (missingZh.length) {
  console.error(`\n✗ zh 缺失 ${missingZh.length} 个（界面会显示 key 原文）：`);
  for (const m of missingZh.sort((a, b) => a.key.localeCompare(b.key))) {
    console.error(`   ${m.key}  ← ${m.uses.slice(0, 3).join(", ")}`);
  }
}
if (missingEn.length) {
  console.error(`\n✗ en 缺失 ${missingEn.length} 个（英文界面回落中文）：`);
  for (const k of missingEn.sort()) console.error(`   ${k}`);
}
if (!missingZh.length && !missingEn.length) console.log("✓ 词条齐备");
process.exit(missingZh.length || missingEn.length ? 1 : 0);