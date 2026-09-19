#!/usr/bin/env node
// 把内置 pnpm 的原生转发器编出来，放到 resources/pnpm/ 随包分发。
//
// 为什么需要它：Windows 上 `pnpm.cmd` 受控制台代码页限制（cmd.exe 解码批处理用的是
// 控制台代码页，写文件用的却是 OEM 代码页），安装路径含该页表示不了的字符时会写坏路径，
// 报「系统找不到指定的路径」；批处理还有 PATHEXT/引号等一堆边角。原生转发器把 argv 与
// stdio 原样转给包内 `node` + `pnpm.mjs`，不经过批处理解析。见 IMPLEMENTATION §14。
//
// 两条设计约束（都是踩过的坑）：
//  1. 产物落在 resources/binaries/ 里。tauri-build 在**编译期**会走一遍 bundle.resources 并
//     要求每个条目已存在，而 resources/ 下的目录由 prepare-resources.ps1 在打包前生成，
//     是天然"早就存在"的地方；放到资源根目录会撞上"前端构建产物 vs cargo 编译期"的顺序
//     问题（CI 上实测编译失败：resource path ... doesn't exist）。单独的 binaries/ 子目录
//     还能让转发器按"自己所在目录的上一级 = 资源根"稳定找到 node/ 与 pnpm/。
//  2. 用 rustc 直接编，不用 `cargo build --bin`。本脚本由前端构建
//     （tauri.conf.json 的 beforeBuildCommand）调用，此时再进 cargo 既可能撞构建锁，
//     又把顺序问题变成循环依赖；转发器不依赖任何 crate，单文件 rustc 足够。
//
// 用法：node scripts/build-forwarder.mjs [--release] [--if-missing]
//   --release    优化编译（打包用；默认不优化，够本地编译/测试用）
//   --if-missing 产物存在且不比源码旧就直接返回（npm test 复用）
import { spawnSync } from "node:child_process";
import { copyFileSync, existsSync, mkdirSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const source = join(root, "src-tauri", "src", "bin", "dsh-pnpm-forwarder.rs");
const args = process.argv.slice(2);
const optimize = args.includes("--release");
const ifMissing = args.includes("--if-missing");

const isWindowsHost = process.platform === "win32";
const exeName = isWindowsHost ? "dsh-pnpm-forwarder.exe" : "dsh-pnpm-forwarder";
// 与 tauri.conf.json 的 bundle.resources 映射一致：
//   "../resources/binaries": "binaries/"  →  <resource_dir>/binaries/dsh-pnpm-forwarder[.exe]
const resource = join(root, "resources", "binaries", exeName);

const mtime = (path) => (existsSync(path) ? statSync(path).mtimeMs : -1);
// "--if-missing" 只在产物确实现成且模式一致时复用：模式（是否 -O）变了必须重编，
// 否则开发态编出来的无优化产物会被打进包里。
const stampPath = join(dirname(resource), ".dsh-forwarder-stamp");
const stamp = existsSync(stampPath) ? readFileSync(stampPath, "utf8").trim() : "";
if (
  ifMissing &&
  existsSync(resource) &&
  mtime(resource) >= mtime(source) &&
  stamp === (optimize ? "release" : "debug")
) {
  console.log(`==> pnpm forwarder reused: ${resource} (${stamp})`);
  process.exit(0);
}

mkdirSync(dirname(resource), { recursive: true });

const rustcArgs = ["--edition", "2021", "-C", "debuginfo=0", "-o", resource];
if (optimize) rustcArgs.push("-O");
rustcArgs.push(source);
const built = spawnSync("rustc", rustcArgs, { stdio: "inherit" });
if (built.error) {
  console.error(`rustc 启动失败: ${built.error.message}`);
  process.exit(1);
}
if (built.status !== 0 || !existsSync(resource)) {
  console.error(`转发器编译失败（rustc exit=${built.status}）`);
  process.exit(built.status ?? 1);
}
// rustc 在 Windows 上可能仍留下 .pdb；它只会白占打包体积
for (const stray of [`${resource}.pdb`, resource.replace(/\.exe$/, ".pdb")]) {
  rmSync(stray, { force: true });
}
writeFileSync(stampPath, `${optimize ? "release" : "debug"}\n`);

// Windows 上顺手把同一份放到 target/<profile>/binaries/：`tauri dev` 的 resource_dir 就是
// target/<profile>，转发器放在那里才能按"自己所在目录的上一级 = 资源根"找到 node/pnpm；
// 开发态因此也走原生转发器，而不是退回批处理 shim。
if (isWindowsHost) {
  for (const profile of ["debug", "release"]) {
    const dir = join(root, "src-tauri", "target", profile, "binaries");
    if (!existsSync(dirname(dir))) continue;
    mkdirSync(dir, { recursive: true });
    copyFileSync(resource, join(dir, exeName));
  }
}

console.log(`==> pnpm forwarder ready: ${resource}`);
