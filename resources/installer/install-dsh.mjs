#!/usr/bin/env node


import { spawnSync } from "node:child_process";
import {
  copyFileSync, existsSync, mkdirSync, readFileSync, readdirSync, readlinkSync,
  renameSync, rmSync, statSync, symlinkSync, writeFileSync,
} from "node:fs";
import { createHash } from "node:crypto";
import { join, dirname, basename } from "node:path";
import { fileURLToPath } from "node:url";

const PKG = "@deepseek-ai/dsh";
const DEFAULT_REGISTRY = "https://registry.npmjs.org";
const DEFAULT_MIRROR = "https://registry.npmmirror.com";
const INSTALLED_FILE = ".installed.json";
// 宿主给一次 install 的总超时是 30 分钟（node.rs INSTALLER_UPDATE_TIMEOUT），降级重装只在预算内才尝试
const RETRY_BUDGET_MS = 15 * 60 * 1000;


function parseArgs(argv) {
  const opts = { mode: null, target: null, baseline: null, staging: null, registry: null, mirror: null, force: false, pre: false };
  let rest = [...argv];
  opts.mode = rest.shift();
  while (rest.length) {
    const a = rest.shift();
    switch (a) {
      case "--target": opts.target = rest.shift(); break;
      case "--baseline": opts.baseline = rest.shift(); break;
      case "--staging": opts.staging = rest.shift(); break;
      case "--registry": opts.registry = rest.shift(); break;
      case "--mirror": opts.mirror = rest.shift(); break;
      case "--force": opts.force = true; break;
      case "--pre": opts.pre = true; break;
      default: break;
    }
  }
  if (!opts.target) throw new Error("--target <dir> is required");
  return opts;
}


function resolveNpmCli() {
  const nodeDir = dirname(process.execPath);
  const candidates = [
    
    join(nodeDir, "node_modules", "npm", "bin", "npm-cli.js"),
    
    join(nodeDir, "..", "lib", "node_modules", "npm", "bin", "npm-cli.js"),
    
    join(nodeDir, "lib", "node_modules", "npm", "bin", "npm-cli.js"),
  ];
  for (const p of candidates) {
    if (existsSync(p)) return p;
  }
  return null;
}


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



function registryReachable(registry, timeoutMs = 3000) {
  return new Promise((resolve) => {
    const ac = new AbortController();
    const timer = setTimeout(() => ac.abort(), timeoutMs);
    fetch(registry, { method: "HEAD", signal: ac.signal, redirect: "follow" })
      .then(() => { clearTimeout(timer); resolve(true); })
      .catch(() => { clearTimeout(timer); resolve(false); });
  });
}


function compareVersions(a, b) {
  const parse = (v) => {
    const [core, pre] = String(v).split("-");
    return { nums: core.split(".").map(Number), pre: pre ?? null };
  };
  const pa = parse(a), pb = parse(b);
  for (let i = 0; i < 3; i++) {
    const x = pa.nums[i] ?? 0, y = pb.nums[i] ?? 0;
    if (x !== y) return x - y;
  }
  if (pa.pre === null && pb.pre !== null) return 1;
  if (pa.pre !== null && pb.pre === null) return -1;
  if (pa.pre !== null && pb.pre !== null && pa.pre !== pb.pre) {
    return pa.pre < pb.pre ? -1 : 1;
  }
  return 0;
}


function splitVersions(versions) {
  let stable = null, prerelease = null;
  for (const v of versions) {
    if (typeof v !== "string" || !v.trim()) continue;
    if (v.includes("-")) {
      if (!prerelease || compareVersions(v, prerelease) > 0) prerelease = v;
    } else {
      if (!stable || compareVersions(v, stable) > 0) stable = v;
    }
  }
  return { stable, prerelease };
}

function splitTags(tags) {
  const values = Object.values(tags).filter((v) => typeof v === "string" && v.trim());
  if (!values.length) return null;
  const { stable, prerelease } = splitVersions(values);
  const latest = typeof tags.latest === "string" && tags.latest.trim() ? tags.latest : null;
  return { channel: latest || stable || prerelease, stable, prerelease };
}

// 默认通道 = dist-tag latest（与 npm i pkg / 基线 -DshVer latest 一致）；--pre 时才跟随更高预发布
function pickTarget(info, pre) {
  const usePre = !!(
    pre && info.prerelease && info.prerelease !== info.channel
    && compareVersions(info.prerelease, info.channel) > 0
  );
  return { target: usePre ? info.prerelease : info.channel, isPre: usePre };
}


function registryCandidates(opts) {
  const primary = opts.registry || opts.mirror || DEFAULT_REGISTRY;
  const others = [];
  for (const url of [DEFAULT_REGISTRY, DEFAULT_MIRROR]) {
    if (url !== primary && !others.includes(url)) others.push(url);
  }
  for (const url of [opts.registry, opts.mirror]) {
    if (url && url !== primary && !others.includes(url)) others.push(url);
  }
  return [primary, ...others].map((url) => ({
    label: url === DEFAULT_REGISTRY ? "npmjs" : url === DEFAULT_MIRROR ? "npmmirror" : "custom",
    url,
  }));
}

async function queryDistTags(registry) {
  
  const reachable = await registryReachable(registry);
  if (!reachable) return null;
  const res = runNpm(["view", PKG, "dist-tags", "--json", "--registry", registry, "--no-audit", "--no-fund", "--fetch-retries=0", "--fetch-timeout=8000"], { timeout: 60_000 });
  if (res.status !== 0) return null;
  let tags;
  try {
    tags = JSON.parse((res.stdout ?? "").trim());
  } catch {
    return null;
  }
  if (!tags || typeof tags !== "object") return null;
  return splitTags(tags);
}


function sweepStaleStaging(parent, keepPid) {
  const cutoff = Date.now() - 6 * 3600 * 1000;
  let names;
  try {
    names = readdirSync(parent);
  } catch {
    return;
  }
  for (const n of names) {
    if (!n.startsWith(".dsh-runtime-staging-") || n.endsWith("-" + keepPid)) continue;
    const p = join(parent, n);
    try {
      if (statSync(p).mtimeMs < cutoff) rmSync(p, { recursive: true, force: true });
    } catch {}
  }
}

const NATIVE_SCRIPT_HINT = /Failed to load prebuilt binary|rebuilding from source|cnoke/i;
const TOOLCHAIN_HINT = /node-gyp|gyp ERR|Python|Visual Studio|MSBUILD|CMake/i;
const NETWORK_HINT = /ETIMEDOUT|ECONNRESET|ENOTFOUND|EAI_AGAIN|ECONNREFUSED|ENETUNREACH|ERR_SOCKET|socket hang up|notarget|fetch failed/i;
const LOCK_HINT = /EPERM|EBUSY|EACCES|ENOTEMPTY/;

function classifyInstallFailure(output) {
  const t = String(output || "");
  if (NATIVE_SCRIPT_HINT.test(t)) return "native-prebuilt";
  if (TOOLCHAIN_HINT.test(t)) return "build-toolchain";
  if (NETWORK_HINT.test(t)) return "network";
  if (LOCK_HINT.test(t)) return "lock";
  return "unknown";
}

// 只有原生脚本编译失败才值得再无障碍（--ignore-scripts）装一次；网络/占用类重装也没用
function retryWithoutScripts(kind) {
  return kind === "native-prebuilt" || kind === "build-toolchain";
}

// 多源报错时原生类失败更可操作（能靠降级重装绕开），优先保留它，别被网络错误盖掉
function pickInstallFailure(prevKind, raw) {
  const kind = classifyInstallFailure(raw);
  if (raw && !retryWithoutScripts(prevKind)) return { kind, detail: raw };
  return { kind: prevKind, detail: null };
}

function installFailureMessage(kind) {
  switch (kind) {
    case "native-prebuilt":
      return "新版本的原生组件未就绪（本机加载失败），已保留当前版本，请稍后重试。";
    case "build-toolchain":
      return "新版本包含需要编译的原生依赖，本机缺少 Python/VS 构建工具链，无法安装该版本。";
    case "network":
      return "新版本下载失败（网络或更新源不可用），已保留当前版本，请稍后重试。";
    case "lock":
      return `新版本没有装好，不影响现在使用。${lockedHint()}`;
    default:
      return "新版本下载失败，已保留当前版本。";
  }
}

function logTail(text, n) {
  return String(text || "").trim().split(/\r?\n/).slice(-n).join("\n");
}

// 失败细节落盘到 <runtime 同级>/logs/dsh-update-install.log（设置页「打开日志目录」可见），并回显到 stderr 供宿主日志收集
function recordInstallFailure(target, output, summary) {
  const text = String(output || "").trim();
  const tail = text ? logTail(text, 60).slice(0, 4000) : "";
  try {
    const dir = join(dirname(target), "logs");
    mkdirSync(dir, { recursive: true });
    writeFileSync(join(dir, "dsh-update-install.log"), `[${new Date().toISOString()}] ${summary}\n${text}\n`, "utf8");
  } catch {  }
  if (tail) log("npm output:\n" + tail);
  return tail;
}

// 只用 npm 11/12 都认的开关；脚本门槛靠 --ignore-scripts，不依赖 npm 12 才有的 allow-scripts 配置
function installArgs(stage, registry, spec, ignoreScripts) {
  const args = ["install", spec, "--prefix", stage, "--registry", registry, "--no-audit", "--no-fund", "--no-update-notifier", "--loglevel=warn"];
  if (ignoreScripts) args.push("--ignore-scripts", "--prefer-online");
  return args;
}

function fetchTo(target, registry, versionSpec, opts = {}) {
  const parent = dirname(target);
  const stage = join(parent, ".dsh-runtime-staging-" + process.pid);
  rmSync(stage, { recursive: true, force: true });
  sweepStaleStaging(parent, process.pid);
  mkdirSync(stage, { recursive: true });
  const spec = versionSpec ? `${PKG}@${versionSpec}` : PKG;
  const res = runNpm(installArgs(stage, registry, spec, !!opts.ignoreScripts), { timeout: 1_800_000 });
  if (res.status !== 0) {
    rmSync(stage, { recursive: true, force: true });
    const detail = (res.stderr || res.stdout || "").trim();
    throw Object.assign(new Error(detail), { kind: "install" });
  }
  const manifest = join(stage, "node_modules", "@deepseek-ai", "dsh", "package.json");
  if (!existsSync(manifest)) {
    rmSync(stage, { recursive: true, force: true });
    throw Object.assign(new Error(`manifest missing after install: ${manifest}`), { kind: "install" });
  }
  const version = JSON.parse(readFileSync(manifest, "utf8")).version;
  return { version, stage, ignoreScripts: !!opts.ignoreScripts };
}

// koffi 的 install 脚本（cnoke --prebuild）会在预编译包误判时转源码编译，是下载失败的主要来源；
// 跳过全部脚本后，只补跑确实需要且安全的两个包，避免 Windows 缺 conpty.dll、非 Windows 缺可执行位
const NEEDED_SCRIPT_PACKAGES = ["node-pty", "@deepseek-ai/dsh-subprocess-local"];

function runNeededScripts(stage) {
  const nm = join(stage, "node_modules");
  const present = NEEDED_SCRIPT_PACKAGES.filter((n) => existsSync(join(nm, ...n.split("/"))));
  if (!present.length) return;
  const res = runNpm(
    ["rebuild", ...present, "--prefix", stage, "--no-audit", "--no-fund", "--no-update-notifier", "--loglevel=warn"],
    { timeout: 600_000 });
  if (res.status !== 0) {
    log("rebuild of needed packages failed (continuing): " + logTail((res.stderr || res.stdout || "").trim(), 20));
  }
}

const NATIVE_PROBE = `
const path = require("node:path");
const dir = process.argv[1];
const out = (o) => console.log(JSON.stringify(o));
try {
  const koffi = require(dir);
  const want = require(path.join(dir, "package.json")).version;
  if (koffi.version !== want) {
    out({ ok: false, why: "版本不匹配：" + koffi.version + " != " + want });
    process.exit(0);
  }
  const lib = process.platform === "win32" ? "kernel32.dll" : process.platform === "darwin" ? "libSystem.B.dylib" : "libc.so.6";
  const fn = process.platform === "win32"
    ? koffi.load(lib).func("GetTickCount", "int", [])
    : koffi.load(lib).func("getpid", "int", []);
  fn();
  out({ ok: true, version: want });
} catch (e) {
  out({ ok: false, why: String((e && e.message) || e) });
}
`;

function probeKoffi(stage) {
  const dir = join(stage, "node_modules", "koffi");
  if (!existsSync(dir)) return { ok: true, skipped: true };
  const res = spawnSync(process.execPath, ["-e", NATIVE_PROBE, dir], { encoding: "utf8", timeout: 60_000, windowsHide: true });
  const line = (res.stdout || "").trim().split(/\r?\n/).filter(Boolean).pop();
  if (!line) return { ok: false, why: `原生探测进程异常退出（status ${res.status}）` };
  try {
    return JSON.parse(line);
  } catch {
    return { ok: false, why: "原生探测输出无法解析：" + line.slice(0, 200) };
  }
}

// 换入前验证原生组件真能加载并调用：上游 koffi 预编译包在部分 Windows 机器上调用即崩，
// 探测放在子进程里，崩溃只让本次更新放弃，不会污染当前运行时
function verifyNative(stage) {
  const problems = [];
  const k = probeKoffi(stage);
  if (!k.ok) problems.push(`koffi ${k.why}`);
  const pty = join(stage, "node_modules", "node-pty");
  if (existsSync(pty) && !existsSync(join(pty, "prebuilds", `${process.platform}-${process.arch}`)) && !existsSync(join(pty, "build", "Release"))) {
    problems.push("node-pty 缺少预编译模块");
  }
  return { ok: problems.length === 0, why: problems.join("；") };
}


function sleep(ms) {
  return new Promise((r) => setTimeout(r, ms));
}

function retriable(e) {
  const code = (e && e.code) || "";
  return /EPERM|EBUSY|ENOTEMPTY|EACCES/.test(code);
}

async function retryRename(from, to, what) {
  let last;
  for (let i = 1; i <= 4; i++) {
    try {
      renameSync(from, to);
      return;
    } catch (e) {
      last = e;
      if (!retriable(e)) throw e;
      log(`${what}: rename retry ${i}/4 (${e.code ?? e})`);
      await sleep(500 * i);
    }
  }
  throw last;
}

function lockedHint() {
  return "文件可能被占用（其他 DSH 窗口、杀毒或残留进程），请稍后重试，持续失败请重启电脑。";
}

// 安全替换：旧版先改名让位（完整保留），新版就位后删除旧版；失败回滚，绝不先删旧版。
// keepOld=true 时新版就位后保留 .old（调用方 smoke 通过后再删或回滚）。
async function replaceDir(target, stage, keepOld = false) {
  const oldDir = target + ".old";
  if (existsSync(oldDir)) {
    if (!existsSync(target)) {
      log("stale .old without target; restoring then replacing");
      try {
        await retryRename(oldDir, target, "restore .old");
      } catch (e) {
        throw Object.assign(
          new Error(`上次替换中断，恢复旧版本失败：${lockedHint()}。旧版本保留在 ${oldDir}。`),
          { kind: "install", userMessage: `上次替换未完成：${lockedHint()}旧版本保留在 ${oldDir}。` }
        );
      }
    } else {
      try {
        rmSync(oldDir, { recursive: true, force: true });
        log("removed stale .old leftover");
      } catch (e) {
        log("stale .old removal failed (ignored): " + (e.message ?? e));
      }
    }
  }
  if (!existsSync(target)) {
    await retryRename(stage, target, "fresh install");
    return;
  }
  try {
    await retryRename(target, oldDir, "move current aside");
  } catch (e) {
    throw Object.assign(
      new Error(`当前版本无法让位：${lockedHint()}`),
      { kind: "install", userMessage: `新版本替换失败，已保留当前版本：${lockedHint()}` }
    );
  }
  try {
    await retryRename(stage, target, "move new into place");
  } catch (e) {
    if (e && e.code === "EXDEV") {
      try {
        mkdirSync(target, { recursive: true });
        copyTree(stage, target);
        rmSync(stage, { recursive: true, force: true });
        rmSync(oldDir, { recursive: true, force: true });
        return;
      } catch (e2) {
        throw Object.assign(
          new Error(`swap copy fallback failed: ${e2.message ?? e2}`),
          { kind: "install" }
        );
      }
    }
    try {
      await retryRename(oldDir, target, "rollback");
    } catch (e2) {
      throw Object.assign(
        new Error(`替换失败且回滚失败：${target} 缺失，旧版本完整保留在 ${oldDir}。请关闭其他 DSH 窗口后重试，或重新安装。`),
        { kind: "install", userMessage: `新版本替换失败：${lockedHint()}旧版本完整保留在 ${oldDir}。` }
      );
    }
    throw Object.assign(
      new Error(`replace failed (${e.code ?? e}); old version restored`),
      { kind: "install", userMessage: `新版本替换失败，已恢复原版本：${lockedHint()}` }
    );
  }
  if (!keepOld) {
    try {
      rmSync(oldDir, { recursive: true, force: true });
    } catch (e) {
      log("old runtime kept at " + oldDir + ": " + (e.message ?? e));
    }
  }
}

function cleanRuntimeLeftovers(target) {
  const parent = dirname(target);
  for (const name of [".dsh-runtime-bak", basename(target) + ".old"]) {
    const p = join(parent, name);
    try {
      if (existsSync(p)) {
        rmSync(p, { recursive: true, force: true });
        log("removed leftover " + name);
      }
    } catch (e) {
      log("leftover removal failed: " + name + " (" + (e.message ?? e) + ")");
    }
  }
}

async function swapStaging(target, stage, keepOld = false) {
  await cleanRuntimeLeftovers(target);
  await replaceDir(target, stage, keepOld);
}


function copyTree(src, dst) {
  const platform = process.platform;
  if (platform === "win32") {
    
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
      } catch {  }
    }
  }
}


async function copyBaseline(target, baselineRoot) {
  let src = baselineRoot;
  if (existsSync(join(src, "node_modules")) && existsSync(join(src, "node_modules", "@deepseek-ai", "dsh", "lib", "bin.js"))) {
    src = join(src, "node_modules");
  }
  if (!existsSync(src) || !existsSync(join(src, "@deepseek-ai", "dsh", "lib", "bin.js"))) {
    throw Object.assign(new Error(`baseline missing: ${baselineRoot}`), { kind: "unknown" });
  }
  cleanStaleStaging(target);
  cleanRuntimeLeftovers(target);
  const stage = join(dirname(target), ".dsh-runtime-staging-" + process.pid);
  rmSync(stage, { recursive: true, force: true });
  mkdirSync(stage, { recursive: true });
  copyTree(src, join(stage, "node_modules"));
  await replaceDir(target, stage);
}


function log(msg) {
  try {
    process.stderr.write(`[install-dsh] ${msg}\n`);
  } catch {  }
}


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


function out(obj) {
  console.log(JSON.stringify(obj));
}


async function main() {
  const opts = parseArgs(process.argv.slice(2));
  const target = opts.target;
  mkdirSync(target, { recursive: true });

  switch (opts.mode) {
    case "prepare": {
      
      const installed = readInstalled(target);
      if (installed && installed.version && smokeOk(target)) {
        out({ ok: true, action: "baseline-ok", version: installed.version, dir: target, source: "cache" });
        return;
      }
      if (opts.baseline) {
        try {
          await copyBaseline(target, opts.baseline);
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
      
      
      
      
      const registries = registryCandidates(opts);
      let info = null, latestSource = null;
      for (const r of registries) {
        const d = await queryDistTags(r.url);
        if (d) { info = d; latestSource = r.label; break; }
      }
      if (!info) {
        out({ ok: false, error: { kind: "network", message: "暂时无法检查更新", detail: "both registries unreachable" } });
        process.exit(1);
      }
      const installed = readInstalled(target);
      const current = installed && installed.version ? installed.version : readVersion(target);
      const cur = current ?? "0.0.0";
      const { target: follow, isPre } = pickTarget(info, opts.pre);
      const target_available = !!follow && follow !== cur && compareVersions(follow, cur) > 0;

      const action = !target_available
        ? "up-to-date"
        : (isPre ? "prerelease-available" : "new-version-available");
      out({
        ok: true, action, version: follow,
        prerelease: isPre ? info.prerelease : null, pre_available: isPre,
        current, dir: target, source: latestSource,
      });
      return;
    }

    case "update": {


      const registries = registryCandidates(opts);
      let info = null, latestSource = null;
      for (const r of registries) {
        const d = await queryDistTags(r.url);
        if (d) { info = d; latestSource = r.label; break; }
      }
      if (!info) {
        out({ ok: false, error: { kind: "network", message: "当前没有网络，更新等联网后自动进行。", detail: "both registries unreachable" } });
        process.exit(1);
      }
      const installed = readInstalled(target);
      const current = installed && installed.version ? installed.version : readVersion(target);
      const cur = current ?? "0.0.0";
      const { target: follow, isPre } = pickTarget(info, opts.pre);
      if (!opts.force && follow && compareVersions(follow, cur) <= 0) {
        out({ ok: true, action: "up-to-date", version: follow, prerelease: null, pre_available: false, current, dir: target, source: latestSource });
        return;
      }

      
      let fetched = null, usedSource = null, rawDetail = "", failKind = "unknown";
      const startedAt = Date.now();
      for (const ignoreScripts of [false, true]) {
        for (const r of registries) {
          try {
            fetched = fetchTo(target, r.url, follow, { ignoreScripts });
            usedSource = r.label;
            break;
          } catch (e) {
            const picked = pickInstallFailure(failKind, String((e && e.message) || ""));
            failKind = picked.kind;
            if (picked.detail) rawDetail = picked.detail;
          }
        }
        if (fetched) break;
        if (!retryWithoutScripts(failKind)) break;
        if (Date.now() - startedAt > RETRY_BUDGET_MS) {
          log("install already took too long; keeping this failure instead of retrying");
          break;
        }
        log(`install failed (${failKind}); retrying with --ignore-scripts`);
      }
      if (!fetched) {
        recordInstallFailure(target, rawDetail, `install failed: ${failKind}`);
        out({ ok: false, error: { kind: "install", message: installFailureMessage(failKind), detail: rawDetail.slice(0, 600) } });
        process.exit(1);
      }
      if (fetched.ignoreScripts) runNeededScripts(fetched.stage);

      const native = verifyNative(fetched.stage);
      if (!native.ok) {
        rmSync(fetched.stage, { recursive: true, force: true });
        recordInstallFailure(target, native.why, "native verification failed");
        out({
          ok: false,
          error: {
            kind: "integrity",
            message: `新版本的原生组件未就绪（${native.why}），已保留当前版本，请稍后重试。`,
            detail: String(native.why).slice(0, 600),
          },
        });
        process.exit(1);
      }

      
      try {
        smokeTest(fetched.stage);
      } catch (e) {
        rmSync(fetched.stage, { recursive: true, force: true });
        out({ ok: false, error: { kind: "integrity", message: "新版本文件不完整，已保留当前版本。", detail: String(e.message ?? e) } });
        process.exit(1);
      }

      out({
        ok: true, action: "downloaded", version: fetched.version,
        staging: fetched.stage, prerelease: isPre ? info.prerelease : null,
        pre_available: isPre, current,
        dir: target, source: usedSource,
      });
      return;
    }

    case "swap": {


      if (!opts.staging || !existsSync(opts.staging)) {
        out({ ok: false, error: { kind: "install", message: "缺少已下载的暂存版本。", detail: "staging missing" } });
        process.exit(1);
      }
      try {
        await swapStaging(opts.target, opts.staging, true);
      } catch (e) {
        out({
          ok: false,
          error: {
            kind: "install",
            message: e?.userMessage || "新版本没有装好，已保留旧版本。",
            detail: String(e.message ?? e),
          },
        });
        process.exit(1);
      }

      let smoke;
      try {
        smoke = smokeTest(opts.target);
      } catch (e) {
        const oldDir = opts.target + ".old";
        let restored = false;
        try {
          rmSync(opts.target, { recursive: true, force: true });
          renameSync(oldDir, opts.target);
          restored = true;
        } catch (e2) {
          log("rollback after smoke failure failed: " + (e2.message ?? e2));
        }
        out({
          ok: false,
          error: {
            kind: "integrity",
            message: restored
              ? "新版本文件不完整，已恢复原版本，请稍后重试。"
              : `新版本文件不完整且回滚失败，旧版本保留在 ${oldDir}。`,
            detail: String(e.message ?? e),
          },
        });
        process.exit(1);
      }
      try {
        rmSync(opts.target + ".old", { recursive: true, force: true });
      } catch (e) {
        log("old runtime kept at " + (opts.target + ".old") + ": " + (e.message ?? e));
      }
      const version = readVersion(opts.target);
      writeInstalled(opts.target, {
        package: PKG, version, installedAt: new Date().toISOString(),
        smoke, source: "npm", node: process.version,
        integrity: shasum256(join(opts.target, "node_modules", "@deepseek-ai", "dsh", "lib", "bin.js")),
      });
      out({ ok: true, action: "updated", version, dir: opts.target, source: "npm" });
      return;
    }

    default:
      out({ ok: false, error: { kind: "unknown", message: "参数错误", detail: `unknown mode: ${opts.mode}` } });
      process.exit(1);
  }
}



const invokedByTest = process.argv[1]?.endsWith?.("install-dsh.test.mjs");
if (!invokedByTest) {
  main().catch((e) => {
    console.log(JSON.stringify({ ok: false, error: { kind: "unknown", message: "程序内部错误", detail: String(e?.message ?? e) } }));
    process.exit(1);
  });
}


export {
  restoreBackup, copyTree, copyTreeFallback, smokeTest, readInstalled, writeInstalled,
  compareVersions, splitVersions, splitTags, pickTarget,
  replaceDir, swapStaging, retryRename, retriable, cleanRuntimeLeftovers, copyBaseline,
  classifyInstallFailure, installFailureMessage, retryWithoutScripts, pickInstallFailure,
  logTail, installArgs, recordInstallFailure, runNeededScripts, probeKoffi, verifyNative,
};
