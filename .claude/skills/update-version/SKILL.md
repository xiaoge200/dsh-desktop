---
name: update-version
description: dsh-desktop 发版流程（版本升级 → 提交 → tag → 推送 → 触发 CI）。触发词：更新版本、升级版本、发版、release、bump version、打 tag 推送。
---

# dsh-desktop 发版流程

把版本从当前（如 0.1.5）升到新版本（如 0.1.6）并发布。所有命令在仓库根目录执行，除非注明。

## 版本号规则

- 权威/同步入口：**`node scripts/bump-version.mjs <新版本>`**，它会一次写入：
  - `src-tauri/Cargo.toml`（运行时 pkg.version 与打包版本来源）
  - `src-tauri/tauri.conf.json` 的 `version`（**不可删除**：tauri CLI/bundler/updater 产物与 release 元数据从它读取；缺省会令 release 版本号为空）
  - `package.json` / `package-lock.json`（npm 侧无真实消费，仅保持一致）
  - 重新生成 `Cargo.lock`（脚本离线优先，避免锁文件漂移）
- 版本必须 semver，且与最终 tag 完全一致。

## 步骤

1. **确认前提**
   - `git status` 干净（有未提交改动先问用户是否并入本次发布）。
   - 工作树与远端同步：`git fetch && git status`（有落后先 pull/合并，发布从最新 main 出）。

2. **升版本**
   ```
   node scripts/bump-version.mjs 0.1.6
   ```
   确认四个文件版本一致：`grep -n '"version"' package.json src-tauri/tauri.conf.json` 与 `grep '^version' src-tauri/Cargo.toml`、`src-tauri/Cargo.lock` 中 `name = "dsh-desktop"` 后的 version。

3. **更新 CHANGELOG.md**：顶部插入 `## [x.y.z] - YYYY-MM-DD` 条目，按既有风格用「### 修复 / ### 新增」概括本次变更（参考上个版本条目的颗粒度）。

4. **验证**
   ```
   cd src-tauri && cargo test
   node --test resources/installer/install-dsh.test.mjs
   cd .. && npx tsc --noEmit && npm run build
   ```
   全绿才继续。

5. **提交**
   ```
   git add package.json package-lock.json src-tauri/Cargo.toml src-tauri/tauri.conf.json src-tauri/Cargo.lock CHANGELOG.md
   git commit -m "chore: 版本升至 x.y.z"
   ```
   ⚠️ **Cargo.lock 极易漏**（版本号变更会改它），提交前用 `git status` 核对它已在内。提交信息以仓库现有 commit 风格为准。

6. **打 tag 并推送**
   ```
   git tag -a vx.y.z -m "vx.y.z"
   git push origin main
   git push origin vx.y.z
   ```
   若远端已有同名 tag（如发布中途补提交）：先 `git push origin --delete refs/tags/vx.y.z`、`git tag -d vx.y.z`，重打后推送，确保 tag 指向含全部修复的 HEAD。

7. **收尾确认**
   - `git status` 干净；`git ls-remote --tags origin vx.y.z` 指向最新 HEAD。
   - 提示用户 CI 已触发（GitHub Actions 在 tag 上跑 release），留意 CI 是否绿。

## 已知坑（务必检查）

- **发布中途的修复必须进 tag**：CI release 从 tag 构建；补提交后要移 tag 再推，否则构建的是旧代码。
- **tauri.conf.json 的 version 别删**：删掉后 CLI/bundler 侧没有可靠回退 → release/updater 版本号为空、产物异常（运行时代码回退 Cargo 版本救不了 CLI 侧）。
- **Cargo.lock 漏提交** / **离线 generate-lockfile 造成依赖降级漂移**：脚本已离线优先，若仍失败需联网重试并 diff 检查 lock 未大范围变动。
- **bump 脚本前先确认无其它未提交改动**，避免把无关变更卷进版本 commit（必要时拆分提交）。
- dev 端口 1420 残留 vite 会挡 `npm run tauri dev`：按需清理旧进程再起。
- 用户全局规则：不自动 commit/push——本 skill 的提交/tag/推送步骤须在用户明确要求发版（如「更新版本到 x.y.z 并发布」）时执行。
