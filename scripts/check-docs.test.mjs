import assert from "node:assert/strict";
import { execFileSync, spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const scripts = path.dirname(fileURLToPath(import.meta.url));
const checker = path.join(scripts, "check-docs.mjs");
function fixture(t) {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), "exm-docs-policy-"));
  t.after(() => fs.rmSync(directory, { recursive: true, force: true }));
  const git = (...args) => execFileSync("git", args, { cwd: directory, encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] });
  git("init", "-q");
  git("config", "user.name", "Documentation policy fixture");
  git("config", "user.email", "fixture@example.invalid");
  const add = (name) => {
    const file = path.join(directory, name);
    fs.mkdirSync(path.dirname(file), { recursive: true });
    fs.writeFileSync(file, "fixture\n");
    git("add", "-f", "--", name);
  };
  const check = (...args) => spawnSync(process.execPath, [checker, ...args], { cwd: directory, encoding: "utf8" });
  return { directory, git, add, check };
}

test("maintained development/design documents are accepted", (t) => {
  const f = fixture(t);
  for (const name of ["docs/README.md", "docs/开发规范.md", "docs/架构与设计.md", "docs/协议与契约.md"]) f.add(name);
  assert.equal(f.check().status, 0);
  assert.equal(f.check("--staged").status, 0);
});

test("forced screenshot additions are rejected", (t) => {
  const f = fixture(t);
  f.add("docs/审计/截图/chat.png");
  const result = f.check("--staged");
  assert.equal(result.status, 1);
  assert.match(result.stderr, /docs\/审计\/截图\/chat\.png/);
  assert.equal(f.check().status, 1);
});

test("markdown audit and acceptance reports are not treated as guides", (t) => {
  const f = fixture(t);
  f.add("docs/审计报告.md");
  f.add("docs/验收-模型.md");
  assert.equal(f.check("--staged").status, 1);
});

test("new documentation requires an explicit inclusion decision", (t) => {
  const f = fixture(t);
  f.add("docs/random notes.md");
  assert.equal(f.check("--staged").status, 1);
});

test("deleting existing artifacts is allowed, but does not count as history cleanup", (t) => {
  const f = fixture(t);
  f.add("docs/审计/old.json");
  f.git("commit", "-qm", "fixture artifact");
  f.git("rm", "-q", "--", "docs/审计/old.json");
  assert.equal(f.check("--staged").status, 0);
  assert.equal(f.check().status, 0);
  assert.equal(f.check("--history").status, 1);
});

test("untracked local artifacts are not confused with staged content", (t) => {
  const f = fixture(t);
  f.add("docs/开发规范.md");
  fs.writeFileSync(path.join(f.directory, "docs/local-result.json"), "{}");
  assert.equal(f.check("--staged").status, 0);
});

test("pre-commit rejects forced artifact staging without creating a commit", (t) => {
  const f = fixture(t);
  f.git("config", "core.hooksPath", path.resolve(scripts, "../.githooks"));
  f.add("docs/截图.png");
  const result = spawnSync("git", ["commit", "-qm", "must be rejected"], { cwd: f.directory, encoding: "utf8" });
  assert.equal(result.status, 1);
  assert.match(result.stderr, /Rejected paths/);
  assert.equal(spawnSync("git", ["rev-parse", "--verify", "HEAD"], { cwd: f.directory }).status, 128);
});

test("the screenshot tool refuses docs output before loading browser dependencies", (t) => {
  const f = fixture(t);
  const result = spawnSync(process.execPath, [path.join(scripts, "ui-shot.mjs"), "--out", path.resolve(scripts, "../docs/截图")], { cwd: f.directory, encoding: "utf8" });
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /UI artifacts must be written outside docs/);
});
