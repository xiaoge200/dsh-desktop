#!/usr/bin/env node
// 把内置 pnpm 的原生转发器编出来，并放到 tauri externalBin 找得到的位置。
//
// 为什么需要它：Windows 上 `pnpm.cmd` 受控制台代码页限制（cmd.exe 解码批处理用的是
// 控制台代码页，写文件用的却是 OEM 代码页），安装路径含该页表示不了的字符时会写坏路径，
// 报「系统找不到指定的路径」。见 docs/IMPLEMENTATION.md §14。
//
// externalBin 的源路径由 tauri-build 在编译期按**相对模式**查找（实测它要的是 crate 根
// 与 target/<profile>/ 下的 <bin>-<triple>[.exe]），所以本脚本把同一份产物落三处：
//   1. target/<profile>/<bin>-<triple>[.exe]  —— tauri-build 校验并拷成 <bin>[.exe]
//   2. target/<profile>/<bin>[.exe]           —— 兼容不带 triple 的查找
//   3. src-tauri/<bin>-<triple>[.exe]         —— crate 根兜底（另一条被查的路径）
// src-tauri/binaries/ 也留一份（tauri CLI 打包时的约定位置）。
// 因此本步必须在 `cargo build/test` 之前跑；npm run build / test / tauri 都已接上。
//
// 用法：node scripts/build-forwarder.mjs [--release] [--if-missing]
//   --release    编 release（打包用；默认 debug，够本地编译/测试用）
//   --if-missing 目标已存在就直接返回（避免每次 npm test 都重新 cargo build）
import { spawnSync } from "node:child_process";
import { copyFileSync, existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const manifest = join(root, "src-tauri", "Cargo.toml");
const confPath = join(root, "src-tauri", "tauri.conf.json");
const binName = "dsh-pnpm-forwarder";
const args = process.argv.slice(2);
const profileDir = args.includes("--release") ? "release" : "debug";
const ifMissing = args.includes("--if-missing");
const target = process.env.DSH_TARGET || process.env.CARGO_BUILD_TARGET || undefined;

const rustc = spawnSync("rustc", ["-vV"], { encoding: "utf8" });
const reportedTriple =
  rustc.status === 0 ? /host: (\S+)/.exec(rustc.stdout ?? "")?.[1] : undefined;
// 不依赖 rustc 可用/可捕获输出：按宿主平台推导（DSH_TARGET 可覆盖）
const derivedTriple = {
  "win32-x64": "x86_64-pc-windows-msvc",
  "win32-arm64": "aarch64-pc-windows-msvc",
  "darwin-x64": "x86_64-apple-darwin",
  "darwin-arm64": "aarch64-apple-darwin",
  "linux-x64": "x86_64-unknown-linux-gnu",
  "linux-arm64": "aarch64-unknown-linux-gnu",
}[`${process.platform}-${process.arch}`];
const hostTriple = reportedTriple ?? derivedTriple;
const triple = target ?? hostTriple;
if (!triple) {
  console.error(`无法确定 target triple（${process.platform}-${process.arch}），请设置 DSH_TARGET`);
  process.exit(1);
}
const isWindows = triple.includes("windows");
const exeName = isWindows ? `${binName}.exe` : binName;

const outDir = join(root, "src-tauri", "target", ...(target ? [target] : []), profileDir);
const built = join(outDir, exeName);
const inTarget = [
  join(outDir, `${binName}-${triple}${isWindows ? ".exe" : ""}`),
  join(outDir, exeName),
];
const inBinaries = join(
  root,
  "src-tauri",
  "binaries",
  `${binName}-${triple}${isWindows ? ".exe" : ""}`,
);
// crate 根也放一份：tauri-build 的 externalBin 查找是相对模式，crate 根与
// target/<profile>/ 都在它的候选里，两处都备上就不必赌是哪一处。
const inCrateRoot = join(root, "src-tauri", `${binName}-${triple}${isWindows ? ".exe" : ""}`);
const outputs = [...inTarget, inBinaries, inCrateRoot];

if (!ifMissing || !outputs.every((p) => existsSync(p))) {
  // tauri-build 在编译期要求 externalBin 已存在，而这步正是为了产出它——先把
  // externalBin 从配置里摘掉再编自己，编完原样写回（字节级还原，含行尾）。
  // 构建失败时故意不还原：tauri.conf.json 会被脚本重新生成，避免留下"配置有、文件没"的
  // 假象；重新跑 `npm run build-forwarder` 即可。
  const original = readFileSync(confPath, "utf8");
  let conf;
  try {
    conf = JSON.parse(original);
  } catch (e) {
    console.error(`tauri.conf.json 解析失败: ${e.message}`);
    process.exit(1);
  }
  writeFileSync(confPath, `${JSON.stringify({ ...conf, bundle: { ...conf.bundle, externalBin: undefined } }, null, 2)}\n`);

  const cargoArgs = ["build", "--bin", binName, "--manifest-path", manifest];
  if (profileDir === "release") cargoArgs.push("--release");
  if (target) cargoArgs.push("--target", target);
  const outcome = spawnSync("cargo", cargoArgs, { stdio: "inherit" });
  writeFileSync(confPath, original);
  if (outcome.error) throw outcome.error;
  if (outcome.status !== 0) process.exit(outcome.status ?? 1);
}

if (!existsSync(built)) {
  console.error(`编译产物不存在: ${built}`);
  process.exit(1);
}
for (const out of outputs) {
  mkdirSync(dirname(out), { recursive: true });
  copyFileSync(built, out);
  console.log(`==> pnpm forwarder ready: ${out} (${triple}, ${profileDir})`);
}
