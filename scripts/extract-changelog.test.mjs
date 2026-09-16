#!/usr/bin/env node
/**
 * extract-changelog.test.mjs — extract-changelog.mjs 的单元测试（Node 内置 test runner）。
 *
 * 运行：node --test scripts/
 *
 * 覆盖：版本一节的正确定位与边界、日期/预发布标题、找不到版本时回退「未发布」、
 *       都没有时输出为空、CLI 参数（--file/--out/--help/非法参数）与 CLI 调用本身。
 *       全部在进程内调用导出的函数，不 spawn 子进程。
 */
import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";

import { extractSection, run } from "./extract-changelog.mjs";

const SAMPLE = [
  "# Changelog",
  "",
  "## 未发布",
  "",
  "### 修复",
  "- 未发布的修复条目",
  "",
  "## [0.1.8] - 2026-09-20",
  "",
  "### 新增",
  "- 0.1.8 的新增条目",
  "",
  "### 修复",
  "- 0.1.8 的修复条目",
  "",
  "## [0.1.8-alpha.1] - 2026-09-10",
  "",
  "- 预发布条目",
  "",
  "## [0.1.7] - 2026-09-14",
  "",
  "- 旧版本条目",
  "",
].join("\n");

function scratch(name) {
  const root = join(tmpdir(), `dsh-changelog-${name}-${process.pid}`);
  rmSync(root, { recursive: true, force: true });
  mkdirSync(root, { recursive: true });
  return root;
}

test("extractSection 取指定版本，边界停在下个 ## 标题", () => {
  const { notes, source } = extractSection(SAMPLE, "0.1.8");
  assert.equal(source, "version");
  assert.equal(notes, ["### 新增", "- 0.1.8 的新增条目", "", "### 修复", "- 0.1.8 的修复条目"].join("\n"));
});

test("extractSection 支持预发布版本号与无日期标题", () => {
  assert.equal(extractSection(SAMPLE, "0.1.8-alpha.1").notes, "- 预发布条目");
  const bare = "## 0.1.9\n\n- 无日期标题\n\n## [0.1.8]\n\n- 下一个\n";
  assert.equal(extractSection(bare, "0.1.9").notes, "- 无日期标题");
});

test("extractSection 找不到版本时回退「未发布」", () => {
  const { notes, source } = extractSection(SAMPLE, "0.1.9");
  assert.equal(source, "unreleased");
  assert.equal(notes, "### 修复\n- 未发布的修复条目");
});

test("extractSection 版本与「未发布」都没有时输出为空", () => {
  const { notes, source } = extractSection("# Changelog\n\n## [0.1.0]\n\n- only old\n", "9.9.9");
  assert.equal(source, null);
  assert.equal(notes, "");
});

test("extractSection 支持 Unreleased 拼写", () => {
  const { source } = extractSection("# Changelog\n\n## [Unreleased]\n\n- pending\n", "1.0.0");
  assert.equal(source, "unreleased");
});

test("版本一节的 ### 子标题不会被当作边界", () => {
  const text = "## [1.0.0]\n\n### 修复\n- a\n\n### 内部\n- b\n\n## [0.9.0]\n\n- c\n";
  const { notes } = extractSection(text, "1.0.0");
  assert.match(notes, /### 内部/);
  assert.doesNotMatch(notes, /- c/);
});

test("run 接受版本号与 --file/--out，状态信息走 log 而不是 stdout", () => {
  const root = scratch("cli");
  const changelog = join(root, "CHANGELOG.md");
  const out = join(root, "notes.md");
  writeFileSync(changelog, SAMPLE, "utf8");

  const messages = [];
  const result = run(["0.1.7", "--file", changelog, "--out", out], {
    cwd: root,
    log: (m) => messages.push(m),
  });

  assert.equal(result.source, "version");
  assert.equal(readFileSync(out, "utf8"), "- 旧版本条目\n");
  assert.ok(messages.some((m) => m.startsWith("[ok]")), messages.join("\n"));

  const missing = run(["9.9.9", "--file", changelog, "--out", join(root, "fallback.md")], {
    cwd: root,
    log: () => {},
  });
  assert.equal(missing.source, "unreleased", "有「未发布」时回退它");
  assert.match(readFileSync(join(root, "fallback.md"), "utf8"), /未发布的修复条目/);

  // 版本与「未发布」都没有：正文里给一句「未找到条目」的提醒，方便发布者补 CHANGELOG。
  const bare = join(root, "CHANGELOG-bare.md");
  writeFileSync(bare, "# Changelog\n\n## [0.0.1]\n\n- old only\n", "utf8");
  const nothing = run(["9.9.9", "--file", bare, "--out", join(root, "missing.md")], {
    cwd: root,
    log: () => {},
  });
  assert.equal(nothing.source, null);
  assert.match(
    readFileSync(join(root, "missing.md"), "utf8"),
    /未在 `CHANGELOG\.md` 中找到 v9\.9\.9 的条目/,
  );
  rmSync(root, { recursive: true, force: true });
});

test("run 回退「未发布」时给出 warn", () => {
  const root = scratch("fallback");
  const changelog = join(root, "CHANGELOG.md");
  writeFileSync(changelog, SAMPLE, "utf8");
  const messages = [];
  const result = run(["0.2.0", "--file", changelog], { cwd: root, log: (m) => messages.push(m) });
  assert.equal(result.source, "unreleased");
  assert.ok(messages.some((m) => m.startsWith("[warn]")), messages.join("\n"));
  rmSync(root, { recursive: true, force: true });
});

test("run 对未知参数与缺路径报错，--help 直接返回", () => {
  assert.throws(() => run(["--nope"], { log: () => {} }), /未知参数/);
  assert.throws(() => run(["--out"], { log: () => {} }), /缺少路径/);
  assert.throws(() => run(["1.0.0", "2.0.0"], { log: () => {} }), /多余的参数/);
  assert.equal(run(["--help"], { log: () => {} }).help, true);
});

test("run 不带版本号时读 tauri.conf.json 的版本", () => {
  const root = scratch("autoversion");
  const changelog = join(root, "CHANGELOG.md");
  const confVersion = JSON.parse(
    readFileSync(new URL("../src-tauri/tauri.conf.json", import.meta.url), "utf8"),
  ).version;
  writeFileSync(changelog, `## [${confVersion}] - 2026-01-01\n\n- 自动取到的条目\n`, "utf8");

  const messages = [];
  const result = run(["--file", changelog, "--out", join(root, "auto.md")], {
    cwd: root,
    log: (m) => messages.push(m),
  });
  assert.equal(result.version, confVersion);
  assert.equal(result.notes, "- 自动取到的条目");
  rmSync(root, { recursive: true, force: true });
});

test("run 真的用得到仓库里的 CHANGELOG（当前 0.1.7 一节可解析）", () => {
  const result = run(["0.1.7", "--out", join(tmpdir(), `dsh-notes-${process.pid}.md`)], {
    log: () => {},
  });
  assert.equal(result.source, "version");
  assert.match(result.notes, /^### 修复/m);
});
