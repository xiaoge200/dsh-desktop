---
name: tauri-acl
description: Tauri 2 capabilities/permissions（ACL）问题排查与修复：命令报 not allowed / 设置页整页读取失败 / 新增命令在前端无效 / App ACL 激活后的严格模式。触发词：权限、capability、ACL、not allowed、读取失败、设置全空、命令无效。
---

# Tauri 2 ACL / capabilities 排查与修复

dsh-desktop 的命令权限由 `src-tauri/capabilities/*.json` + `src-tauri/permissions/*.toml` 决定。以下规律基于 tauri 2.11+ 实测（含踩坑）。

## 核心规律

1. **App ACL 一旦存在即全局严格**：只要 `src-tauri/permissions/` 下有任何应用权限文件（app ACL 非空），**所有窗口（本地页 + remote 页）的所有自定义命令都必须显式授权**。没有 permissions 目录时本地窗口宽松放行（remote 自 2.11 起无论有无都强制，PR tauri-apps/tauri#15266）。所以「新增一个权限文件修 remote」会连带让本地窗口（如 settings）全部命令失效——两处 capability 都要补。
2. **被 ACL 拒的命令不会执行**：后端没有任何日志（命令没进 Rust）。现象＝前端报错（如设置页整页「读取失败」、状态轮询无数据），后端日志干净——此时先怀疑 ACL 而非代码。
3. **错误文本含义**：
   - `"{cmd} not allowed. Plugin not found"` → 无任何 capability 授权该命令（App ACL key 都查不到）
   - 构建期 build script 报 `Permission X not found, expected one of <超长列表>` → capability 引用的 identifier 不存在；列表即当前全部可用 identifier，直接对照。

## 文件格式（容易踩错）

`permissions/app-commands.toml`：
```toml
[[permission]]
identifier = "allow-restart-service"
description = "Allows the restart_service command"
commands = { allow = ["restart_service"] }   # 必须是内联表！写成 commands = ["..."] 会解析失败

[[set]]                                        # 字段名是 set！
identifier = "settings-commands"               # 写成 [[permission_set]] 会被 serde 当未知键静默丢弃
description = "Commands invoked by the settings window"
permissions = ["allow-get-settings", "allow-restart-service"]
```
capability 引用：`"permissions": ["core:default", "settings-commands", ...]`（应用集合/权限无前缀；插件/核心带 `core:`、`autostart:` 等前缀）。

注意：capabilities JSON 无尾随逗号、数组保持文件原样式（别用 JSON.stringify 重写造成格式噪音；用行级插入）。

## 排查步骤

1. 复现，确认后端日志是否出现命令执行痕迹：**无** → IPC/ACL 层拒绝。
2. `cargo build` 看 build script 是否报 expected-one-of（identifier 笔误/集合没解析）。
3. 列出该窗口 invoke 的全部命令（含注入脚本 ctx-menu/restart-shim 调用的），对照 capability 归属：
   - main 窗口本地页（boot）：`get_status / restart_service / repair_service / open_context_menu / open_settings / plugins_remove_incompatible`
   - settings 窗口：`get_settings / get_service_state / set_autostart / open_log_dir / open_workspace_dir / get_config / set_config / get_dsh_update_status / check_dsh_update / update_dsh / get_app_update_status / check_app_update / update_app / restart_service / plugins_list / plugins_add / plugins_remove / plugins_set_enabled / plugins_marketplace`
   - main 窗口 remote（127.0.0.1 工作台页）：`restart_service / open_context_menu`（restart-shim / ctx-menu 注入）
4. 缺的补 `[[permission]]` + 挂到对应 capability（直接列 allow-* 或经 `[[set]]` 聚合）。

## 新增命令 checklist

给应用新增 `#[tauri::command]` 时：
- [ ] 若该命令会被前端/注入脚本调用：在 `permissions/app-commands.toml` 加 `allow-<kebab>` 定义
- [ ] 按调用窗口加入 capability（经 set 或直接引用）
- [ ] `cargo build` 通过（identifier 会被校验；commands 名不校验，注意与注册名一致）
- [ ] 实机验证一次（编译期不查 commands 名，错误只在运行时暴露）

## 其它关联教训

- `tauri.conf.json` 的 `version` 字段**不要删**：CLI/bundler/updater 产物版本与 release 元数据从它读，删了会得到「release 版本号空」；运行时 pkg.version 会回退 Cargo 版本，但救不了 CLI 侧。发版用 `scripts/bump-version.mjs`（见 update-version skill）。
- 排查「设置页整页信息/配置全空」：先看状态行是否「读取失败」；是 → 一条命令都别进后端 → ACL 检查（本 skill 步骤 2-4），而不是查前端 DOM/后端逻辑（DOM id 与实现此前已核对无恙）。
