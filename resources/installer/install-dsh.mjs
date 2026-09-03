#!/usr/bin/env node


import { spawnSync } from "node:child_process";
import {
  copyFileSync, existsSync, mkdirSync, readFileSync, readdirSync, readlinkSync,
  renameSync, rmSync, symlinkSync, writeFileSync,
} from "node:fs";
import { createHash } from "node:crypto";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const PKG = "@deepseek-ai/dsh";
const DEFAULT_REGISTRY = "https://registry.npmjs.org";
const DEFAULT_MIRROR = "https://registry.npmmirror.com";
const INSTALLED_FILE = ".installed.json";


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
  const versions = Object.values(tags).filter((v) => typeof v === "string");
  if (versions.length === 0) return null;
  return splitVersions(versions);
}


function fetchTo(target, registry, versionSpec) {
  const parent = dirname(target);
  const stage = join(parent, ".dsh-runtime-staging-" + process.pid);
  rmSync(stage, { recursive: true, force: true });
  mkdirSync(stage, { recursive: true });
  const spec = versionSpec ? `${PKG}@${versionSpec}` : PKG;
  const res = runNpm(
    ["install", spec, "--prefix", stage, "--registry", registry, "--no-audit", "--no-fund", "--no-update-notifier", "--loglevel=error"],
    { timeout: 1_800_000 });
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
  return { version, stage };
}


function swapStaging(target, stage) {
  try {
    rmSync(target, { recursive: true, force: true });
    renameSync(stage, target);
  } catch (e) {
    
    log("swap rename failed, copying from staging: " + (e.message ?? e));
    try {
      rmSync(target, { recursive: true, force: true });
      mkdirSync(target, { recursive: true });
      copyTree(stage, target);
      rmSync(stage, { recursive: true, force: true });
    } catch (e2) {
      throw Object.assign(new Error(`swap failed: ${e2.message ?? e2}`), { kind: "install" });
    }
  }
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
  renameSync(stage, target);
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
      
      
      
      
      const registries = [
        { label: opts.registry ? "custom" : "npmjs", url: opts.registry || DEFAULT_REGISTRY },
        { label: opts.mirror ? "custom" : "npmmirror", url: opts.mirror || DEFAULT_MIRROR },
      ];
      let info = null, latestSource = null;
      for (const r of registries) {
        const d = await queryDistTags(r.url);
        if (d && (d.stable || d.prerelease)) { info = d; latestSource = r.label; break; }
      }
      if (!info) {
        out({ ok: false, error: { kind: "network", message: "暂时无法检查更新", detail: "both registries unreachable" } });
        process.exit(1);
      }
      const installed = readInstalled(target);
      const current = installed && installed.version ? installed.version : readVersion(target);
      const stable = info.stable;
      const prerelease = info.prerelease;
      const cur = current ?? "0.0.0";
      
      let follow = null;
      if (opts.pre && prerelease && stable && compareVersions(prerelease, stable) > 0) {
        follow = prerelease; 
      } else if (stable) {
        follow = stable;
      } else {
        follow = prerelease; 
      }
      const target_available = !!follow && follow !== cur && compareVersions(follow, cur) > 0;
      const pre_available = !!prerelease && prerelease !== cur && compareVersions(prerelease, cur) > 0;
      
      const is_pre_target = follow !== null && prerelease !== null && follow === prerelease && follow !== stable;
      const action = !target_available
        ? "up-to-date"
        : (is_pre_target ? "prerelease-available" : "new-version-available");
      out({
        ok: true, action, version: stable, prerelease, pre_available, current,
        dir: target, source: latestSource,
      });
      return;
    }

    case "update": {
      
      
      const registries = [
        { label: opts.registry ? "custom" : "npmjs", url: opts.registry || DEFAULT_REGISTRY },
        { label: opts.mirror ? "custom" : "npmmirror", url: opts.mirror || DEFAULT_MIRROR },
      ];
      let info = null, latestSource = null;
      for (const r of registries) {
        const d = await queryDistTags(r.url);
        if (d && (d.stable || d.prerelease)) { info = d; latestSource = r.label; break; }
      }
      if (!info) {
        out({ ok: false, error: { kind: "network", message: "当前没有网络，更新等联网后自动进行。", detail: "both registries unreachable" } });
        process.exit(1);
      }
      const installed = readInstalled(target);
      const current = installed && installed.version ? installed.version : readVersion(target);
      const stable = info.stable;
      const prerelease = info.prerelease;
      const cur = current ?? "0.0.0";
      const pre_available = !!prerelease && prerelease !== cur && compareVersions(prerelease, cur) > 0;
      
      let targetVersion = null;
      if (opts.pre && prerelease && stable && compareVersions(prerelease, stable) > 0) {
        targetVersion = prerelease;
      } else if (stable) {
        targetVersion = stable;
      } else {
        targetVersion = prerelease;
      }
      if (!opts.force && current === targetVersion) {
        out({ ok: true, action: "up-to-date", version: stable, prerelease, pre_available, current, dir: target, source: latestSource });
        return;
      }

      
      let fetched = null, usedSource = null;
      const order = latestSource === "npmmirror" ? [registries[1], registries[0]] : [registries[0], registries[1]];
      for (const r of order) {
        try {
          fetched = fetchTo(target, r.url, targetVersion);
          usedSource = r.label;
          break;
        } catch {  }
      }
      if (!fetched) {
        out({ ok: false, error: { kind: "install", message: "新版本下载失败，已保留当前版本。", detail: "download failed on both registries" } });
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
        staging: fetched.stage, prerelease, pre_available, current,
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
        swapStaging(opts.target, opts.staging);
      } catch (e) {
        out({ ok: false, error: { kind: "install", message: "新版本没有装好，已保留旧版本。", detail: String(e.message ?? e) } });
        process.exit(1);
      }
      
      let smoke;
      try {
        smoke = smokeTest(opts.target);
      } catch (e) {
        out({ ok: false, error: { kind: "integrity", message: "替换后文件不完整，请重新安装。", detail: String(e.message ?? e) } });
        process.exit(1);
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


export { restoreBackup, copyTree, copyTreeFallback, smokeTest, readInstalled, writeInstalled, compareVersions, splitVersions };
