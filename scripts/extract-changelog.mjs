#!/usr/bin/env node
/**
 * extract-changelog.mjs — 把 CHANGELOG.md 里某个版本的更新内容抽出来，供 Release 正文使用。
 *
 * 用法：
 *   node scripts/extract-changelog.mjs [version] [--file CHANGELOG.md] [--out release-notes.md]
 *
 * 不传 version 时取 src-tauri/tauri.conf.json 的 version（与 tag、updater 清单同源）。
 * 找不到该版本条目时回退「未发布」/「Unreleased」一节；两者都没有则输出一句
 * 「未找到 vX.Y.Z 条目」的提醒并正常退出——Release 只是草稿，缺条目不该让流水线失败。
 * 只有 CHANGELOG.md 本身读不到才以 1 退出。
 *
 * 输出：默认把这段内容写到 stdout；给了 --out 就写文件。所有状态信息走 stderr，
 * stdout 可安全重定向。
 *
 * 测试：node --test scripts/
 */
import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const scriptDir = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(scriptDir, "..");

const USAGE =
  "用法: node scripts/extract-changelog.mjs [version] [--file CHANGELOG.md] [--out release-notes.md]";

// `## [0.1.8] - 2026-09-20` / `## 0.1.8` / `## [v0.1.8]` / `## [0.1.8-alpha.1]`
const VERSION_HEADING = /^##\s+\[?v?(\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?)\]?(?:\s|$)/;
// `## 未发布` / `## [Unreleased]`
const UNRELEASED_HEADING = /^##\s+\[?(未发布|unreleased)\]?\s*$/i;

/** 没有对应条目时正文里放这句，提醒发布者去补 CHANGELOG（而不是发一份空白说明）。 */
function missingNote(version) {
  return `> 本次发布未在 \`CHANGELOG.md\` 中找到 v${version} 的条目。`;
}

/**
 * 抽出 `version` 一节的正文（不含标题行）。
 *
 * 标题匹配不到该版本时回退「未发布」；都没有则 `notes` 为空、`source` 为 null。
 * @param markdown - CHANGELOG.md 全文。
 * @param version - 目标版本号（不含前导 v）。
 * @returns `{ notes, source }`，`source` 为 `version` / `unreleased` / null。
 */
export function extractSection(markdown, version) {
  const lines = markdown.split(/\r?\n/);
  const headings = [];
  for (let i = 0; i < lines.length; i += 1) {
    const matched = VERSION_HEADING.exec(lines[i]);
    if (matched) {
      headings.push({ index: i, version: matched[1], unreleased: false });
      continue;
    }
    if (UNRELEASED_HEADING.test(lines[i])) {
      headings.push({ index: i, version: null, unreleased: true });
    }
  }

  const picked =
    headings.find((h) => h.version === version) ?? headings.find((h) => h.unreleased);
  if (!picked) return { notes: "", source: null };

  // 一节到下一个 `## ` 标题为止（`### 修复` 这类子标题不算边界）。
  let end = lines.length;
  for (let i = picked.index + 1; i < lines.length; i += 1) {
    if (/^##\s/.test(lines[i])) {
      end = i;
      break;
    }
  }
  const notes = lines
    .slice(picked.index + 1, end)
    .join("\n")
    .replace(/^\s*\n+/, "")
    .replace(/\s+$/, "");
  return { notes, source: picked.unreleased ? "unreleased" : "version" };
}

/** 发布版本号的唯一来源：tauri.conf.json（bump-version.mjs 会把它同步到各处）。 */
function readReleaseVersion() {
  const conf = JSON.parse(readFileSync(join(repoRoot, "src-tauri", "tauri.conf.json"), "utf8"));
  if (typeof conf.version !== "string" || conf.version === "") {
    throw new Error("src-tauri/tauri.conf.json 缺少 version");
  }
  return conf.version;
}

/**
 * CLI 主体（可被测试直接调用，不必起子进程）。
 * @param argv - 不含 `node script` 的参数表。
 * @param options - `cwd`（相对路径基准）与 `log`（状态输出，默认 stderr）。
 * @returns `{ notes, version, source }`。
 */
export function run(argv, { cwd = process.cwd(), log = console.error } = {}) {
  let version = null;
  let file = join(repoRoot, "CHANGELOG.md");
  let out = null;

  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    if (arg === "--file" || arg === "--out") {
      const value = argv[i + 1];
      if (value === undefined || value.startsWith("--")) throw new Error(`${arg} 缺少路径`);
      i += 1;
      if (arg === "--file") file = resolve(cwd, value);
      else out = resolve(cwd, value);
      continue;
    }
    if (arg === "--help" || arg === "-h") {
      log(USAGE);
      return { notes: "", version: null, source: null, help: true };
    }
    if (arg.startsWith("-")) throw new Error(`未知参数：${arg}`);
    if (version !== null) throw new Error(`多余的参数：${arg}`);
    version = arg.replace(/^v/, "");
  }

  if (version === null) version = readReleaseVersion();

  const markdown = readFileSync(file, "utf8");
  const { notes, source } = extractSection(markdown, version);
  if (source === "version") {
    log(`[ok] 取到 v${version} 的更新内容（${notes === "" ? 0 : notes.split("\n").length} 行）`);
  } else if (source === "unreleased") {
    log(`[warn] CHANGELOG 中没有 v${version} 条目，回退使用「未发布」一节`);
  } else {
    log(`[warn] CHANGELOG 中既没有 v${version} 条目也没有「未发布」一节，正文将写明未找到条目`);
  }

  // 输出的是可以直接放进 Release 正文的内容：拿不到条目时给一句提醒，而不是留空。
  const published = notes === "" ? missingNote(version) : notes;
  const text = `${published}\n`;
  if (out !== null) writeFileSync(out, text, "utf8");
  else process.stdout.write(text);

  return { notes, version, source };
}

const invokedByTest = process.argv[1]?.endsWith?.(".test.mjs") ?? false;
if (!invokedByTest) {
  try {
    run(process.argv.slice(2));
  } catch (error) {
    console.error(`[error] ${error?.message ?? error}`);
    process.exit(1);
  }
}
