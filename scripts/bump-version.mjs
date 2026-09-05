#!/usr/bin/env node
// 版本唯一来源：src-tauri/Cargo.toml（tauri.conf.json 不写 version，回退 CARGO_PKG_VERSION）。
// 本脚本把新版本同步到 package.json / package-lock.json（npm 侧无真实消费，仅保持一致），
// 并重新生成 Cargo.lock。用法：node scripts/bump-version.mjs 0.1.6
import { readFileSync, writeFileSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const next = process.argv[2];
if (!next || !/^\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?$/.test(next)) {
  console.error("用法: node scripts/bump-version.mjs <semver>");
  process.exit(1);
}
const semverOk = (s) => /^\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?$/.test(s);

const cargoPath = join(root, "src-tauri", "Cargo.toml");
let cargo = readFileSync(cargoPath, "utf8");
const cur = cargo.match(/^version = "([^"]+)"/m)?.[1];
if (!cur) throw new Error("Cargo.toml 缺少 version");
cargo = cargo.replace(/^version = "([^"]+)"/m, `version = "${next}"`);
writeFileSync(cargoPath, cargo);

for (const rel of ["package.json", "package-lock.json"]) {
  const p = join(root, rel);
  const json = JSON.parse(readFileSync(p, "utf8"));
  if (!json.version || !semverOk(String(json.version))) {
    console.warn(`跳过 ${rel}（无版本字段）`);
    continue;
  }
  json.version = next;
  writeFileSync(p, JSON.stringify(json, null, 2) + "\n");
}

const runCargo = (args) =>
  spawnSync("cargo", args, { cwd: join(root, "src-tauri"), encoding: "utf8", stdio: "inherit" });
let r = runCargo(["generate-lockfile", "--offline"]);
if (r.status !== 0) r = runCargo(["generate-lockfile"]);
if (r.status !== 0) process.exit(r.status ?? 1);

console.log(`版本 ${cur} -> ${next}：Cargo.toml / package.json / package-lock.json / Cargo.lock 已同步。`);
console.log("记得在 CHANGELOG.md 顶部补 [next] 条目，然后 commit + tag + push。");
