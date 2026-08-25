# 实现记录（dsh-desktop）

> 记录开发过程中遇到的关键问题与解决方案，供团队维护参考。
> 关联需求文档：`../dsh-desktop-plan.md`（v0.3）

## 里程碑进度

| 阶段 | 状态 | 说明 |
|---|---|---|
| M0 可行性验证 | ✅ | 内置 Node 24 + dsh 基线可启动 `dsh web`，HTTP 200 |
| M1 Windows MVP | ✅ | Tauri v2 壳、基线准备、服务托管、WebView 内嵌、托盘、单实例、日志、白话错误 UI |
| M1+ 打磨 | ✅ | 设置窗口（开机自启/服务状态/版本信息/目录入口）、托盘「重启服务」、CHANGELOG |
| FR-15 高级入口 | ✅ | `--dsh-args` 透传 dsh CLI 参数（实测 --trusted-host 完整到达） |
| FR-13 卸载数据 | ✅ 核心已满足 | 实测卸载后 %APPDATA% 用户数据完整保留（NSIS per-user 默认行为）；卸载前询问对话框为 P2 增强，需自定义 NSIS 模板（有破坏安装器风险，暂缓） |
| NFR-10 本地化 | ✅ | i18n 中英双语：boot 页/设置页按 navigator.language，托盘按系统区域设置（Windows GetUserDefaultUILanguage / LANG） |
| 自动化测试 | ✅ | Rust 单元测试 19 个 + install-dsh.mjs 测试 8 个（含回滚机制测试），CI 已接入 |
| 更新回滚（R7/R10） | ✅ | update 前备份 `.dsh-runtime-bak`，安装/冒烟失败自动恢复旧版本，成功删备份 |
| NFR-06 离线降级 | ✅ 优化 | registry 不可达时 HEAD 探测 3s 快速失败（原 npm 长重试 240s+ → 实测 0.1s）；在线路径 3.4s 正常 |
| NFR-02 冷启动 | ✅ 达标 | 实测二次启动：服务就绪 ~4.5s（含 dsh 服务自身初始化 ~3s），WebView 跳转后用户可交互；首启含基线复制约 60-90s（文档已注明安装场景除外） |
| M2 更新与分发 | 🚧 | DSH 自动更新 ✅；Tauri updater 代码+密钥+签名链路 ✅；更新服务器端点待配置（发布时） |
| M3 跨平台 | 🚧 | CI 三平台矩阵已配置（.github/workflows/build-release.yml）；macOS/Linux 真机验证待 CI 首跑 |
| FR-18 插件机制 | 🟡 | 后端 `plugins.rs`（28 单测）+ 设置页「插件」卡片（管理器+市场）+ 重启快照；端到端手动验证待跑 |
| M4 打磨发布 | ⏳ | 待内测 |

## 关键决策（实现期新增）

1. **内置基线 dsh 版本**（O5 从开放问题转为已定）
   - 实测：`npm install @deepseek-ai/dsh` 需 511 包 / 约 30 分钟（npmmirror）——0 门槛下不可接受。
   - 方案：安装包内置完整 dsh 依赖树（`resources/dsh-baseline`，解压 192MB / 压缩后约 25MB），
     首启 `install-dsh.mjs prepare` 秒级复制；更新走后台 `check`/`update`。
2. **Node 版本：24 LTS**（O3 已定）——实测 dsh 正常，内置 npm 11.6.0。
3. **插件机制（FR-18）：完全复用 dsh 官方 Cordis 插件体系，不另起炉灶**
   - 插件实体 = 声明 `"dsh": {"bundle": {"patch": ...}}` 的 npm 包，安装在
     `$DSH_HOME/profiles/web`（桌面端运行的 web profile；dsh 运行时更新 rm -rf 重建，
     但 profile 归 dsh 管，天然持久）。
   - 安装后端用**内置 npm**（`npm-cli.js`，不依赖系统 pnpm），Rust 侧复刻上游
     `dsh plugin` 的 `reconcilePlugins`（扫描依赖 manifest 的 `dsh.bundle.patch` →
     写入 `dsh.profile.bundles`）与 `initProfile`（模板逐字复制自
     `@deepseek-ai/dsh-app-boot`）。**注意**：上游改动 reconcile/模板语义时需跟进
     `src-tauri/src/plugins.rs`。
   - `--legacy-peer-deps` 对齐上游 pnpm `autoInstallPeers: false`：没有它 npm 会把
     `@deepseek-ai/*` peer 复制进 profile/node_modules，遮蔽安装树 fallback 符号链接。
   - 启用/禁用 = 向 profile 的 `cordis.patch.yml` **追加** `- id: <id> disabled: <bool>`
     行（后写覆盖先写，与 applyEntryPatches 语义一致）。dsh `watchUserPatches` 热重载，
     无需重启。YAML 块序列**没有收尾符**——模板的空流式 `[]` 首写时展开为块式，
     之后只追加；从不整文件重序列化用户补丁（保留注释与 `!!js` 表达式）。
   - 新增/移除 bundle 行只在启动时读取 → 「重启快照」：boot/restart/repair 服务就绪后
     把当前 bundle+版本写入 `appData/plugins-state.json`，插件页据此对比提示「重启后生效」。
   - home 层（`$DSH_HOME/cordis.patch.yml`）优先级高于 profile 层：被 home 层禁用/启用的
     行在设置页显示为只读（无法从 profile 层切换）。
   - 默认插件：启动后台自动安装 `dshmarket`（dsh-market 市场插件，装进 dsh 网页设置）。
     成功后写 `appData/default-plugin.json` 标记——用户手动移除后不会被自动装回；
     失败静默（下次启动重试），安装后由重启快照提示「重启后生效」。
   - 托盘语言：`is_zh_locale()` 在 Windows 上**注册表系统 UI 语言优先**、环境变量回退
     （修复从 Git Bash 启动时 `LANG=en_US` 导致托盘英文、与网页端语言不一致）。

## 踩坑记录（重要）

### 1. Node fs.cpSync 在中文路径崩溃（0xC0000409）
- 现象：`install-dsh.mjs` 用 `fs.cpSync` 复制基线时，若路径含非 ASCII（如安装目录「DSH 工作台」、
  中文用户名下的 APPDATA），进程崩溃 `STATUS_STACK_BUFFER_OVERRUN`。
- 解决：改用系统原生复制——Windows `robocopy`（返回码 0-7 为成功）、macOS/Linux `cp -R`，
  兜底逐文件复制。见 `install-dsh.mjs#copyTree`。

### 2. Tauri resource_dir 返回 `\\?\` verbatim 路径，Node 崩溃
- 现象：`app.path().resource_dir()` 在 Windows 返回 `\\?\C:\...` 前缀路径；
  将其作为 node 入口脚本参数时，node 报 `EISDIR: lstat 'C:'` 崩溃。
- 解决：`normalize_for_node()` 去除 `\\?\` 前缀（node.rs / supervisor.rs 双份实现，
  保持模块独立）。**注意**：此问题不影响 Rust 侧 fs 操作，仅影响传给 node 的参数。

### 3. npm 12 allow-scripts 策略
- 现象：用户级 `.npmrc`/环境变量 `npm_config_allow_scripts` 导致 npm 12 在项目安装时报
  `EALLOWSCRIPTS`。
- 解决：项目 `.npmrc` 的 `allow-scripts` 放行 `@tauri-apps/cli, esbuild`（构建期）。
- 备注：安装器走内置 npm 11（Node 24 自带），无此问题。

### 4. 单实例插件导致新实例静默退出
- 现象：应用立即退出且无日志——实为残留实例（旧版测试）持有单实例锁。
- 排查方法：`Get-CimInstance Win32_Process | where Name -match dsh`。
- 教训：测试前先清残留进程。

### 5. NSIS 配置字段
- `perMachine` 不是合法字段；per-user 安装用 `"installMode": "currentUser"`。
- `bundle.targets` 用 `["nsis"]`（"all" 会尝试 MSI 并因 WiX light.exe 失败）。

### 6. 日志中文显示乱码（控制台）
- 日志文件为 UTF-8；PowerShell `Get-Content` 默认按 GBK 解码导致显示乱码。
- 不影响功能；排查时用 `Get-Content -Encoding UTF8` 或 VSCode 打开。

### 7. updater 插件配置字段
- 现象：`plugins.updater.windows.installMode: "currentUser"` 导致启动 panic
  （"unknown variant `currentUser`, expected basicUi/quiet/passive"）。
- 原因：NSIS 的 `installMode` 是 `bundle.windows.nsis` 的字段；updater 插件的
  `windows.installMode` 是**安装 UI 模式**，不是 per-user 开关。
- 解决：per-user 安装只配置 `bundle.windows.nsis.installMode: "currentUser"`，
  updater 插件不设 windows 段。

### 8. pipe 未读取导致 dsh 进程卡死（重要）
- 现象：supervisor 用 `Stdio::piped()` 接管 dsh 输出但从不读取；dsh 持续输出
  日志（HMR、工具调用），管道缓冲（~64KB）填满后 dsh 的 write 永久阻塞 → 服务卡死。
- 解决：改为直接重定向到 `appData/logs/service.log`（每次启动截断），
  同时落实 FR-09（服务日志落盘）。实测 service.log 内容正确。
- 教训：spawn 子进程后不读取输出，必须重定向到文件或启动读取线程。

### 9. 进程退出清理
- 托盘「退出」先 `supervisor.stop()`（杀进程树）再 `app.exit(0)`，避免残留。
- 强杀（任务管理器/Stop-Process -Force）无法触发 Rust Drop，残留属操作系统边界；
  下次启动端口自动更换兜底。

## 运行环境事实

- 内置 Node：24.9.0（含 npm 11.6.0），resources/node/win-x64，约 98MB。
- 基线 dsh：0.1.1-rc.2（511 包，解压 192MB），resources/dsh-baseline。
- 安装包（NSIS per-user）：约 47MB，安装后 297MB。
- 服务默认 127.0.0.1:3080，被占用自动换空闲端口（实测 49460/64609）。
