#!/usr/bin/env node
/**
 * install-dsh.test.mjs — install-dsh.mjs 的单元测试（Node 内置 test runner）
 *
 * 运行：node --test resources/installer/
 * 覆盖：copyTree 跨平台分支、copyBaseline 两种形态、.installed.json 读写、
 *       smokeTest 冒烟、输出契约（ok/action 字段）。
 */
import { test } from "node:test";
import assert from "node:assert/strict";
import { existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";

// 通过子进程跑 install-dsh.mjs 的 main（不易直接 import，因其顶层会执行 main）
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { dirname } from "node:path";

const INSTALLER = fileURLToPath(new URL("./install-dsh.mjs", import.meta.url));

// 顶层 import：install-dsh.mjs 检测到测试进程时不执行 main，仅导出内部函数
const { restoreBackup, copyTreeFallback, compareVersions, splitVersions } = await import("./install-dsh.mjs");

function runInstaller(args, timeout = 300_000) {
  return spawnSync(process.execPath, [INSTALLER, ...args], {
    encoding: "utf8",
    timeout,
    windowsHide: true,
  });
}

test("copyTreeFallback 逐文件复制包含子目录与内容", () => {
  const src = join(tmpdir(), `dsh-test-cp-src-${process.pid}`);
  const dst = join(tmpdir(), `dsh-test-cp-dst-${process.pid}`);
  rmSync(src, { recursive: true, force: true });
  rmSync(dst, { recursive: true, force: true });
  mkdirSync(join(src, "sub"), { recursive: true });
  writeFileSync(join(src, "a.txt"), "hello");
  writeFileSync(join(src, "sub", "b.txt"), "world");

  copyTreeFallback(src, dst);
  assert.equal(readFileSync(join(dst, "a.txt"), "utf8"), "hello");
  assert.equal(readFileSync(join(dst, "sub", "b.txt"), "utf8"), "world");

  rmSync(src, { recursive: true, force: true });
  rmSync(dst, { recursive: true, force: true });
});

test("prepare 从基线复制（端到端，验证 copyTree 实际可用）", { timeout: 300_000 }, () => {
  // 构造一个最小基线：node_modules/@deepseek-ai/dsh/lib/bin.js + package.json
  const baseline = join(tmpdir(), `dsh-test-base-${process.pid}`);
  const target = join(tmpdir(), `dsh-test-target-${process.pid}`);
  rmSync(baseline, { recursive: true, force: true });
  rmSync(target, { recursive: true, force: true });
  const binDir = join(baseline, "node_modules", "@deepseek-ai", "dsh", "lib");
  mkdirSync(binDir, { recursive: true });
  writeFileSync(join(binDir, "bin.js"), "#!/usr/bin/env node\nconsole.log('0.0.0-test');\n");
  writeFileSync(
    join(baseline, "node_modules", "@deepseek-ai", "dsh", "package.json"),
    JSON.stringify({ name: "@deepseek-ai/dsh", version: "0.0.0-test" }),
  );

  const r = runInstaller(["prepare", "--target", target, "--baseline", baseline]);
  assert.equal(r.status, 0, `prepare failed: ${r.stderr}`);
  const line = r.stdout.trim().split("\n").pop();
  const json = JSON.parse(line);
  assert.equal(json.ok, true);
  assert.equal(json.action, "prepared-from-baseline");
  assert.equal(json.version, "0.0.0-test");
  // 目标已含 dsh bin 与 .installed.json
  assert.ok(existsSync(join(target, "node_modules", "@deepseek-ai", "dsh", "lib", "bin.js")));
  assert.ok(existsSync(join(target, ".installed.json")));
  const installed = JSON.parse(readFileSync(join(target, ".installed.json"), "utf8"));
  assert.equal(installed.version, "0.0.0-test");

  rmSync(baseline, { recursive: true, force: true });
  rmSync(target, { recursive: true, force: true });
});

test("prepare 复用已有有效安装（baseline-ok）", { timeout: 60_000 }, () => {
  // 先准备一次，再跑第二次应直接复用
  const baseline = join(tmpdir(), `dsh-test-base2-${process.pid}`);
  const target = join(tmpdir(), `dsh-test-target2-${process.pid}`);
  rmSync(baseline, { recursive: true, force: true });
  rmSync(target, { recursive: true, force: true });
  const binDir = join(baseline, "node_modules", "@deepseek-ai", "dsh", "lib");
  mkdirSync(binDir, { recursive: true });
  writeFileSync(join(binDir, "bin.js"), "#!/usr/bin/env node\nconsole.log('0.1.0');\n");
  writeFileSync(
    join(baseline, "node_modules", "@deepseek-ai", "dsh", "package.json"),
    JSON.stringify({ name: "@deepseek-ai/dsh", version: "0.1.0" }),
  );

  const r1 = runInstaller(["prepare", "--target", target, "--baseline", baseline]);
  assert.equal(r1.status, 0);
  const r2 = runInstaller(["prepare", "--target", target, "--baseline", baseline]);
  assert.equal(r2.status, 0);
  const json2 = JSON.parse(r2.stdout.trim().split("\n").pop());
  assert.equal(json2.action, "baseline-ok");
  assert.equal(json2.version, "0.1.0");

  rmSync(baseline, { recursive: true, force: true });
  rmSync(target, { recursive: true, force: true });
});

test("prepare 基线缺失时返回白话错误 JSON", () => {
  const target = join(tmpdir(), `dsh-test-target3-${process.pid}`);
  rmSync(target, { recursive: true, force: true });
  const missing = join(tmpdir(), `dsh-test-missing-base-${process.pid}`);
  rmSync(missing, { recursive: true, force: true });
  const r = runInstaller(["prepare", "--target", target, "--baseline", missing], 30_000);
  // 失败时 status 非 0，但 stdout 末行仍是 JSON
  assert.notEqual(r.status, 0);
  const line = r.stdout.trim().split("\n").pop();
  const json = JSON.parse(line);
  assert.equal(json.ok, false);
  assert.ok(json.error.message, "应有白话错误 message");
  rmSync(target, { recursive: true, force: true });
});

test("参数缺失时拒绝", () => {
  const r = runInstaller(["prepare"], 30_000);
  assert.notEqual(r.status, 0);
});

test("未知模式拒绝", () => {
  const target = join(tmpdir(), `dsh-test-unknown-${process.pid}`);
  const r = runInstaller(["bogus", "--target", target], 30_000);
  assert.notEqual(r.status, 0);
});

// ---- 回滚机制（R7/R10）：直接单元测试 restoreBackup ----
test("restoreBackup 恢复旧版本并清理备份", () => {
  const target = join(tmpdir(), `dsh-test-rb-${process.pid}`);
  const bak = join(tmpdir(), ".dsh-runtime-bak");
  rmSync(target, { recursive: true, force: true });
  rmSync(bak, { recursive: true, force: true });

  // 备份目录 = 旧版本（含 bin.js 与 .installed.json）
  mkdirSync(join(bak, "node_modules", "@deepseek-ai", "dsh", "lib"), { recursive: true });
  writeFileSync(join(bak, "node_modules", "@deepseek-ai", "dsh", "lib", "bin.js"), "#!/usr/bin/env node\nconsole.log('0.9.0');\n");
  writeFileSync(join(bak, "node_modules", "@deepseek-ai", "dsh", "package.json"), JSON.stringify({ name: "@deepseek-ai/dsh", version: "0.9.0" }));
  writeFileSync(join(bak, ".installed.json"), JSON.stringify({ version: "0.9.0" }));

  // 目标目录 = 被破坏的新版本（半成品）
  mkdirSync(join(target, "node_modules"), { recursive: true });
  writeFileSync(join(target, "broken.txt"), "partial install");

  restoreBackup(target, bak, true);

  // 恢复后：目标含旧版 bin 与 .installed.json，半成品消失，备份已清理
  assert.ok(existsSync(join(target, "node_modules", "@deepseek-ai", "dsh", "lib", "bin.js")), "旧版 bin 恢复");
  assert.equal(JSON.parse(readFileSync(join(target, ".installed.json"), "utf8")).version, "0.9.0");
  assert.ok(!existsSync(join(target, "broken.txt")), "半成品清除");
  assert.ok(!existsSync(bak), "备份清理");

  rmSync(target, { recursive: true, force: true });
});

test("restoreBackup 无备份时安全跳过", () => {
  const target = join(tmpdir(), `dsh-test-rb-none-${process.pid}`);
  const bak = join(tmpdir(), `.dsh-runtime-bak-none-${process.pid}`);
  rmSync(target, { recursive: true, force: true });
  rmSync(bak, { recursive: true, force: true });
  mkdirSync(target, { recursive: true });
  writeFileSync(join(target, "x.txt"), "1");

  // hasBackup=false：不应动 target
  restoreBackup(target, bak, false);
  assert.ok(existsSync(join(target, "x.txt")));

  // hasBackup=true 但备份目录不存在：也不应破坏 target
  restoreBackup(target, bak, true);
  assert.ok(existsSync(join(target, "x.txt")));

  rmSync(target, { recursive: true, force: true });
});

// ---- 版本比较（预发布支持）----
test("compareVersions 比较正式版", () => {
  assert.ok(compareVersions("1.5.0", "1.4.0") > 0);
  assert.ok(compareVersions("1.4.0", "1.5.0") < 0);
  assert.equal(compareVersions("1.4.0", "1.4.0"), 0);
  assert.ok(compareVersions("1.10.0", "1.9.0") > 0, "多位数版本按数字比较");
});

test("compareVersions 预发布低于同 core 正式版", () => {
  assert.ok(compareVersions("1.5.0-rc.1", "1.5.0") < 0);
  assert.ok(compareVersions("1.5.0", "1.5.0-rc.1") > 0);
  assert.ok(compareVersions("1.5.0-rc.2", "1.5.0-rc.1") > 0);
  assert.ok(compareVersions("1.5.0-beta.1", "1.5.0-rc.1") < 0, "beta < rc（字符串序）");
});

// ---- 版本列表区分正式版/预发布（按 semver 标记，不依赖 tag 名）----
test("splitVersions 按 semver 标记区分正式版与预发布", () => {
  const versions = [
    "0.1.0", "0.1.1", "0.1.2-alpha.1", "0.1.2-rc.1", "0.2.0", "0.2.1-beta.2",
  ];
  const { stable, prerelease } = splitVersions(versions);
  assert.equal(stable, "0.2.0", "正式版取不含 - 的最高版本");
  assert.equal(prerelease, "0.2.1-beta.2", "预发布取含 - 的最高版本");
});

test("splitVersions latest tag 指向预发布时仍正确区分", () => {
  // 模拟 latest tag 指向预发布（如 0.1.1-rc.2），正式版是更早的 0.1.0
  const versions = ["0.1.0", "0.1.1-rc.1", "0.1.1-rc.2"];
  const { stable, prerelease } = splitVersions(versions);
  assert.equal(stable, "0.1.0");
  assert.equal(prerelease, "0.1.1-rc.2");
});

test("splitVersions 只有预发布时 stable 为 null", () => {
  const { stable, prerelease } = splitVersions(["1.0.0-rc.1", "1.0.0-rc.2"]);
  assert.equal(stable, null);
  assert.equal(prerelease, "1.0.0-rc.2");
});
