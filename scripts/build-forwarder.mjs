#!/usr/bin/env node
// 把内置 pnpm 的原生转发器编出来，放到 resources/ 供 tauri 当普通资源打包。
//
// 为什么需要它：Windows 上 `pnpm.cmd` 受控制台代码页限制（cmd.exe 解码批处理用的是
// 控制台代码页，写文件用的却是 OEM 代码页），安装路径含该页表示不了的字符时会写坏路径，
// 报「系统找不到指定的路径」；批处理还有 PATHEXT/引号等一堆边角。原生转发器把 argv 与
// stdio 原样转给包内 `node` + `pnpm.mjs`，不经过批处理解析。见 IMPLEMENTATION §14。
//
// 为什么走 resources 而不是 bundle.externalBin：externalBin 会让 Windows MSI 构建失败
// （tauri-bundler 未修 #14681），而启动时本来就要 `install_shim()` 把它拷进
// `<appData>/pnpm-home/pnpm.exe`，作为普通资源反而更直接——顺带免掉"tauri-build 要求
// externalBin 源在编译期已存在、而本脚本正是为了产出它"的自锁。
//
// 用法：node scripts/build-forwarder.mjs [--release] [--if-missing]
//   --release    编 release（打包用；默认 debug，够本地编译/测试用）
//   --if-missing 产物已存在就直接返回（npm test 复用，不重复 cargo build）
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
const triple = target ?? reportedTriple ?? derivedTriple;
if (!triple) {
  console.error(`无法确定 target triple（${process.platform}-${process.arch}），请设置 DSH_TARGET`);
  process.exit(1);
}
const exeName = triple.includes("windows") ? `${binName}.exe` : binName;

const outDir = join(root, "src-tauri", "target", ...(target ? [target] : []), profileDir);
const built = join(outDir, exeName);
// 与 tauri.conf.json 的 bundle.resources 一致：../resources/dsh-pnpm-forwarder.exe
// 启动时 install_shim() 会把它拷成 <appData>/pnpm-home/pnpm.exe
const resource = join(root, "resources", exeName);

if (!ifMissing || !existsSync(resource)) {
  // tauri-build 在编译期会走一遍 bundle.resources，要求每个条目都已存在——而本脚本
  // 正是为了产出这个条目。编自己之前先把转发器那条资源从配置里摘掉，编完按字节还原
  // （try/finally：即使 cargo 失败也不会把配置留在被改过的状态）。
  const original = readFileSync(confPath, "utf8");
  let conf;
  try {
    conf = JSON.parse(original);
  } catch (e) {
    console.error(`tauri.conf.json 解析失败: ${e.message}`);
    process.exit(1);
  }
  const resources = conf.bundle?.resources;
  const staged = { ...conf, bundle: { ...conf.bundle } };
  if (Array.isArray(resources)) {
    staged.bundle.resources = resources.filter((entry) => !String(entry).includes(binName));
  } else if (resources && typeof resources === "object") {
    const kept = Object.fromEntries(
      Object.entries(resources).filter(([key]) => !key.includes(binName)),
    );
    staged.bundle.resources = kept;
  }
  writeFileSync(confPath, `${JSON.stringify(staged, null, 2)}\n`);

  const cargoArgs = ["build", "--bin", binName, "--manifest-path", manifest];
  if (profileDir === "release") cargoArgs.push("--release");
  if (target) cargoArgs.push("--target", target);
  const outcome = spawnSync("cargo", cargoArgs, { stdio: "inherit" });
  writeFileSync(confPath, original);
  if (outcome.error) throw outcome.error;
  if (outcome.status !== 0) process.exit(outcome.status ?? 1);
  if (!existsSync(built)) {
    console.error(`编译产物不存在: ${built}`);
    process.exit(1);
  }
  mkdirSync(dirname(resource), { recursive: true });
  copyFileSync(built, resource);
}

console.log(`==> pnpm forwarder ready: ${resource} (${triple}, ${profileDir})`);
