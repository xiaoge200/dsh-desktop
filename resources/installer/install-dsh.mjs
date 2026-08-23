#!/usr/bin/env node
/**
 * install-dsh.mjs — DSH 运行时准备器
 *
 * 职责（对应 dsh-desktop-plan.md §3.1 / §3.2）：
 *   1. 基线优先：安装包内置 dsh 完整依赖树（resources/dsh-baseline），
 *      无有效安装时直接复制基线 → 首启秒级就绪（0 门槛，不依赖网络）。
 *   2. 更新（后台，非阻塞启动）：npm view 检查最新版 → 需要时 npm install
 *      到 <target>（官方源失败自动切 npmmirror）。
 *   3. 写 .installed.json（精确版本 + 完整性），供壳（Rust）读取。
 *   4. 幂等、可重入、崩溃安全（临时目录 + 原子改名）。
 *
 * 调用方式（由 Tauri 壳 spawn）：
 *   准备（同步，启动路径）：
 *     node install-dsh.mjs prepare --target <dir> --baseline <dir>
 *   更新（异步，后台路径）：
 *     node install-dsh.mjs update --target <dir> [--registry <url>] [--mirror <url>] [--force]
 *   检查（异步，仅查询）：
 *     node install-dsh.mjs check --target <dir> [--registry <url>] [--mirror <url>]
 *
 * 输出契约（stdout 末行 = JSON）：
 *   {"ok":true,"action":"prepared-from-baseline"|"baseline-ok"|"updated"|"up-to-date"|"offline-reuse"|"new-version-available","version":"0.1.1-rc.2","dir":"...","source":"baseline"|"npmjs"|"npmmirror"|"cache"}
 *   {"ok":false,"error":{"kind":"network"|"install"|"integrity"|"unknown","message":"白话摘要","detail":"技术细节"}}
 */

import { spawnSync } from "node:child_process";
import {
  copyFileSync, existsSync, mkdirSync, readFileSync, readdirSync, readlinkSync,
  rmSync, symlinkSync, writeFileSync,
} from "node:fs";
import { createHash } from "node:crypto";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const PKG = "@deepseek-ai/dsh";
const DEFAULT_REGISTRY = "https://registry.npmjs.org";
const DEFAULT_MIRROR = "https://registry.npmmirror.com";
const INSTALLED_FILE = ".installed.json";

// ---- arg parsing ----
function parseArgs(argv) {
  const opts = { mode: null, target: null, baseline: null, registry: null, mirror: null, force: false };
  let rest = [...argv];
  opts.mode = rest.shift();
  while (rest.length) {
    const a = rest.shift();
    switch (a) {
      case "--target": opts.target = rest.shift(); break;
      case "--baseline": opts.baseline = rest.shift(); break;
      case "--registry": opts.registry = rest.shift(); break;
      case "--mirror": opts.mirror = rest.shift(); break;
      case "--force": opts.force = true; break;
      default: break;
    }
  }
  if (!opts.target) throw new Error("--target <dir> is required");
  return opts;
}

// ---- npm 定位：优先用内置 Node 自带的 npm-cli.js（不依赖系统 PATH）----
function resolveNpmCli() {
  const nodeDir = dirname(process.execPath);
  const candidates = [
    // Windows 官方发行版：<nodeDir>/node_modules/npm/bin/npm-cli.js
    join(nodeDir, "node_modules", "npm", "bin", "npm-cli.js"),
    // mac/Linux 官方发行版（bin 在 nodeDir）：<nodeDir>/../lib/node_modules/npm
    join(nodeDir, "..", "lib", "node_modules", "npm", "bin", "npm-cli.js"),
    // 本项目资源布局：node 二进制与 lib 平级（CI 复制 bin/node + lib 到同一目录）
    join(nodeDir, "lib", "node_modules", "npm", "bin", "npm-cli.js"),
  ];
  for (const p of candidates) {
    if (existsSync(p)) return p;
  }
  return null;
}

// ---- helpers ----
function runNpm(args, opts = {}) {
  const npmCli = resolveNpmCli();
  if (!npmCli) {
    return { status: 1, stdout: "", stderr: "npm-cli.js not found next to node" };
  }
  const res = spawnSync(process.execPath, [npmCli, ...args], {
    encoding: "utf8",
    timeout: opts.timeout ?? 600_000,
    env: { ...process.env, ...(opts.env ?? {}) },
    windowsHide: true,
    maxBuffer: 64 * 1024 * 1024,
  });
  return res;
}

function shasum256(file) {
  const h = createHash("sha256");
  h.update(readFileSync(file));
  return h.digest("hex");
}

function readInstalled(dir) {
  const file = join(dir, INSTALLED_FILE);
  if (!existsSync(file)) return null;
  try {
    return JSON.parse(readFileSync(file, "utf8"));
  } catch {
    return null;
  }
}

function writeInstalled(dir, info) {
  writeFileSync(join(dir, INSTALLED_FILE), JSON.stringify(info, null, 2), "utf8");
}

/** 查询某个 registry 上 dsh 的最新版本；失败返回 null */
/** 快速探测 registry 是否可达（≤3s），不可达直接返回 false，避免 npm 内部长重试 */
function registryReachable(registry, timeoutMs = 3000) {
  return new Promise((resolve) => {
    const ac = new AbortController();
    const timer = setTimeout(() => ac.abort(), timeoutMs);
    fetch(registry, { method: "HEAD", signal: ac.signal, redirect: "follow" })
      .then(() => { clearTimeout(timer); resolve(true); })
      .catch(() => { clearTimeout(timer); resolve(false); });
  });
}

async function queryLatest(registry) {
  // 快速连通性探测：离线时立即返回，不等 npm 长重试（NFR-06 离线降级）
  const reachable = await registryReachable(registry);
  if (!reachable) return null;
  const res = runNpm(["view", PKG, "version", "--registry", registry, "--no-audit", "--no-fund", "--fetch-retries=0", "--fetch-timeout=8000"], { timeout: 60_000 });
  if (res.status !== 0) return null;
  const v = (res.stdout ?? "").trim().split(/\r?\n/).filter(Boolean).pop();
  return v || null;
}

/** npm install 到 target；成功返回版本字符串 */
function installTo(target, registry) {
  rmSync(target, { recursive: true, force: true });
  mkdirSync(target, { recursive: true });
  const res = runNpm(
    ["install", PKG, "--prefix", target, "--registry", registry, "--no-audit", "--no-fund", "--no-update-notifier", "--loglevel=error"],
    { timeout: 1_800_000 });
  if (res.status !== 0) {
    const detail = (res.stderr || res.stdout || "").trim();
    throw Object.assign(new Error(detail), { kind: "install" });
  }
  const manifest = join(target, "node_modules", "@deepseek-ai", "dsh", "package.json");
  if (!existsSync(manifest)) {
    throw Object.assign(new Error(`manifest missing after install: ${manifest}`), { kind: "install" });
  }
  return JSON.parse(readFileSync(manifest, "utf8")).version;
}

/**
 * 递归复制目录。注意：Node 的 fs.cpSync 在 Windows 含非 ASCII 路径（如中文
 * 安装目录/用户名）时会崩溃（0xC0000409），因此改用系统原生复制工具：
 *  - Windows: robocopy（原生、Unicode 安全、快）
 *  - macOS/Linux: cp -R
 *  - 兜底：node 逐文件复制（cpSync 崩溃的保险）
 */
function copyTree(src, dst) {
  const platform = process.platform;
  if (platform === "win32") {
    // robocopy 返回码 0-7 均表示成功（1=有文件复制，0=无变化）；>=8 为失败
    const r = spawnSync("robocopy", [src, dst, "/E", "/NFL", "/NDL", "/NJH", "/NJS", "/NP", "/R:1", "/W:1"], {
      encoding: "utf8", timeout: 1_800_000, windowsHide: true, maxBuffer: 64 * 1024 * 1024,
    });
    if (r.status !== null && r.status < 8) return;
    throw Object.assign(new Error(`robocopy failed (code ${r.status}): ${r.stderr || r.stdout || ""}`), { kind: "copy" });
  }
  if (platform === "darwin" || platform === "linux") {
    const r = spawnSync("cp", ["-R", src + "/.", dst + "/"], {
      encoding: "utf8", timeout: 1_800_000, windowsHide: true, maxBuffer: 64 * 1024 * 1024,
    });
    if (r.status === 0) return;
    throw Object.assign(new Error(`cp failed: ${r.stderr || r.stdout || ""}`), { kind: "copy" });
  }
  // 兜底：逐文件复制（不用 cpSync，避免崩溃）
  copyTreeFallback(src, dst);
}

function copyTreeFallback(src, dst) {
  const walk = (from, to) => {
    mkdirSync(to, { recursive: true });
    for (const entry of readdirSync(from, { withFileTypes: true })) {
      const s = join(from, entry.name);
      const d = join(to, entry.name);
      if (entry.isDirectory()) walk(s, d);
      else if (entry.isFile()) copyFileSync(s, d);
      else if (entry.isSymbolicLink()) symlinkSync(readlinkSync(s), d);
    }
  };
  walk(src, dst);
}

/** 清理 target 同级的旧 staging 残留（崩溃安全：崩溃后不累积垃圾） */
function cleanStaleStaging(target) {
  const parent = dirname(target);
  let entries;
  try {
    entries = readdirSync(parent, { withFileTypes: true });
  } catch {
    return;
  }
  for (const entry of entries) {
    if (entry.isDirectory() && entry.name.startsWith(".dsh-runtime-staging-")) {
      try {
        rmSync(join(parent, entry.name), { recursive: true, force: true });
      } catch { /* best effort */ }
    }
  }
}

/** 从基线复制完整依赖树到 target（快，秒级）。baseline 接受两种形态：
 *  1) 直接是 node_modules 目录（含 @deepseek-ai/...）
 *  2) 是 dsh-baseline 根目录（含 node_modules/ 子目录）
 */
function copyBaseline(target, baselineRoot) {
  let src = baselineRoot;
  if (existsSync(join(src, "node_modules")) && existsSync(join(src, "node_modules", "@deepseek-ai", "dsh", "lib", "bin.js"))) {
    src = join(src, "node_modules");
  }
  if (!existsSync(src) || !existsSync(join(src, "@deepseek-ai", "dsh", "lib", "bin.js"))) {
    throw Object.assign(new Error(`baseline missing: ${baselineRoot}`), { kind: "unknown" });
  }
  cleanStaleStaging(target);
  const stage = join(dirname(target), ".dsh-runtime-staging-" + process.pid);
  rmSync(stage, { recursive: true, force: true });
  mkdirSync(stage, { recursive: true });
  copyTree(src, join(stage, "node_modules"));
  rmSync(target, { recursive: true, force: true });
  mkdirSync(target, { recursive: true });
  copyTree(stage, target);
  rmSync(stage, { recursive: true, force: true });
}

/** 调试日志（写 stderr，不进 stdout JSON 契约） */
function log(msg) {
  try {
    process.stderr.write(`[install-dsh] ${msg}\n`);
  } catch { /* ignore */ }
}

/** 从备份恢复旧版本（更新失败回滚，R7/R10） */
function restoreBackup(target, bakDir, hasBackup) {
  if (!hasBackup || !existsSync(bakDir)) {
    log("no backup to restore");
    return;
  }
  try {
    rmSync(target, { recursive: true, force: true });
    mkdirSync(target, { recursive: true });
    copyTree(bakDir, target);
    rmSync(bakDir, { recursive: true, force: true });
    log("restored old runtime from backup");
  } catch (e) {
    log("restore failed: " + (e.message ?? e));
  }
}

/** 冒烟：node <dsh>/lib/bin.js --version */
function smokeTest(target) {
  const bin = join(target, "node_modules", "@deepseek-ai", "dsh", "lib", "bin.js");
  if (!existsSync(bin)) {
    throw Object.assign(new Error(`dsh bin missing: ${bin}`), { kind: "integrity" });
  }
  const res = spawnSync(process.execPath, [bin, "--version"], { encoding: "utf8", timeout: 60_000, windowsHide: true });
  if (res.status !== 0) {
    throw Object.assign(new Error(`smoke failed: ${res.stderr || res.stdout || "exit " + res.status}`), { kind: "integrity" });
  }
  return (res.stdout ?? "").trim();
}

function smokeOk(target) {
  try { smokeTest(target); return true; } catch { return false; }
}

function readVersion(target) {
  const manifest = join(target, "node_modules", "@deepseek-ai", "dsh", "package.json");
  if (!existsSync(manifest)) return null;
  try { return JSON.parse(readFileSync(manifest, "utf8")).version; } catch { return null; }
}

// ---- 输出 ----
function out(obj) {
  console.log(JSON.stringify(obj));
}

// ============ 主流程 ============
async function main() {
  const opts = parseArgs(process.argv.slice(2));
  const target = opts.target;
  mkdirSync(target, { recursive: true });

  switch (opts.mode) {
    case "prepare": {
      // 启动路径：若有有效安装则复用；否则从基线复制（秒级）
      const installed = readInstalled(target);
      if (installed && installed.version && smokeOk(target)) {
        out({ ok: true, action: "baseline-ok", version: installed.version, dir: target, source: "cache" });
        return;
      }
      if (opts.baseline) {
        try {
          copyBaseline(target, opts.baseline);
          const v = readVersion(target);
          if (!v || !smokeOk(target)) {
            out({ ok: false, error: { kind: "integrity", message: "程序文件不完整，请重新安装。", detail: "baseline copy smoke failed" } });
            process.exit(1);
          }
          writeInstalled(target, {
            package: PKG, version: v, installedAt: new Date().toISOString(),
            smoke: smokeTest(target), source: "baseline", node: process.version,
            integrity: shasum256(join(target, "node_modules", "@deepseek-ai", "dsh", "lib", "bin.js")),
          });
          out({ ok: true, action: "prepared-from-baseline", version: v, dir: target, source: "baseline" });
          return;
        } catch (e) {
          out({ ok: false, error: { kind: "unknown", message: "程序文件不完整，请重新安装。", detail: String(e.message ?? e) } });
          process.exit(1);
        }
      }
      out({ ok: false, error: { kind: "unknown", message: "缺少内置组件，请重新安装。", detail: "no baseline provided" } });
      process.exit(1);
    }

    case "check": {
      // 后台：仅查询最新版本
      const registries = [
        { label: opts.registry ? "custom" : "npmjs", url: opts.registry || DEFAULT_REGISTRY },
        { label: opts.mirror ? "custom" : "npmmirror", url: opts.mirror || DEFAULT_MIRROR },
      ];
      let latest = null, latestSource = null;
      for (const r of registries) {
        const v = await queryLatest(r.url);
        if (v) { latest = v; latestSource = r.label; break; }
      }
      if (!latest) {
        out({ ok: false, error: { kind: "network", message: "暂时无法检查更新", detail: "both registries unreachable" } });
        process.exit(1);
      }
      const installed = readInstalled(target);
      const current = installed && installed.version ? installed.version : readVersion(target);
      if (current === latest) {
        out({ ok: true, action: "up-to-date", version: latest, dir: target, source: latestSource });
      } else {
        out({ ok: true, action: "new-version-available", version: latest, current, dir: target, source: latestSource });
      }
      return;
    }

    case "update": {
      // 后台：安装/更新到最新版；带回滚（R7/R10：失败保留旧版本可用）
      const registries = [
        { label: opts.registry ? "custom" : "npmjs", url: opts.registry || DEFAULT_REGISTRY },
        { label: opts.mirror ? "custom" : "npmmirror", url: opts.mirror || DEFAULT_MIRROR },
      ];
      let latest = null, latestSource = null;
      for (const r of registries) {
        const v = await queryLatest(r.url);
        if (v) { latest = v; latestSource = r.label; break; }
      }
      if (!latest) {
        out({ ok: false, error: { kind: "network", message: "当前没有网络，更新等联网后自动进行。", detail: "both registries unreachable" } });
        process.exit(1);
      }
      const installed = readInstalled(target);
      const current = installed && installed.version ? installed.version : readVersion(target);
      if (!opts.force && current === latest) {
        out({ ok: true, action: "up-to-date", version: latest, dir: target, source: latestSource });
        return;
      }

      // 1) 备份当前版本（若存在且有效），供回滚
      const bakDir = join(dirname(target), ".dsh-runtime-bak");
      rmSync(bakDir, { recursive: true, force: true });
      const hasOld = smokeOk(target);
      if (hasOld) {
        try {
          mkdirSync(bakDir, { recursive: true });
          copyTree(target, bakDir);
          log("backed up old runtime to " + bakDir);
        } catch (e) {
          log("backup failed (continue without rollback): " + (e.message ?? e));
          rmSync(bakDir, { recursive: true, force: true });
        }
      }

      // 2) 安装新版本（换源重试）
      let version = null, usedSource = null;
      const order = latestSource === "npmmirror" ? [registries[1], registries[0]] : [registries[0], registries[1]];
      for (const r of order) {
        try {
          version = installTo(target, r.url);
          usedSource = r.label;
          break;
        } catch { /* 换源重试 */ }
      }
      if (!version) {
        // 安装失败：恢复旧版本
        restoreBackup(target, bakDir, hasOld);
        out({ ok: false, error: { kind: "install", message: "新版本没有装好，已保留旧版本。", detail: "install failed on both registries" } });
        process.exit(1);
      }

      // 3) 冒烟：失败则回滚
      let smoke;
      try {
        smoke = smokeTest(target);
      } catch (e) {
        restoreBackup(target, bakDir, hasOld);
        out({ ok: false, error: { kind: "integrity", message: "新版本文件不完整，已恢复旧版本。", detail: String(e.message ?? e) } });
        process.exit(1);
      }

      // 4) 成功：写状态，删备份
      writeInstalled(target, {
        package: PKG, version, installedAt: new Date().toISOString(),
        smoke, source: usedSource, node: process.version,
        integrity: shasum256(join(target, "node_modules", "@deepseek-ai", "dsh", "lib", "bin.js")),
      });
      rmSync(bakDir, { recursive: true, force: true });
      out({ ok: true, action: "updated", version, dir: target, source: usedSource });
      return;
    }

    default:
      out({ ok: false, error: { kind: "unknown", message: "参数错误", detail: `unknown mode: ${opts.mode}` } });
      process.exit(1);
  }
}

// 测试钩子：被 install-dsh.test.mjs import 时不执行 main（argv[1] 是测试文件）；
// 直接运行时（argv[1] 是自身）执行 main。
const invokedByTest = process.argv[1]?.endsWith?.("install-dsh.test.mjs");
if (!invokedByTest) {
  main().catch((e) => {
    console.log(JSON.stringify({ ok: false, error: { kind: "unknown", message: "程序内部错误", detail: String(e?.message ?? e) } }));
    process.exit(1);
  });
}

// 导出供测试（顶层 export 是 ESM 静态约束）
export { restoreBackup, copyTree, copyTreeFallback, smokeTest, readInstalled, writeInstalled };
