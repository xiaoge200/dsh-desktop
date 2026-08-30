# Changelog

## [0.1.3] - 2026-08-31

### 修复
- 服务进程挂载到 Windows Job Object（`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`）：主进程无论以何种方式
  退出（含强杀/崩溃/安装器结束进程），node 服务进程树都随之终止，彻底杜绝孤儿进程
- NSIS 安装/卸载钩子：覆盖/升级安装前整棵树结束正在运行的应用（含 node 服务进程），
  解决升级时「node 被占用」导致文件覆盖失败；并兜底清理安装目录下的残留 node 进程

### 新增
- Windows 产物新增 MSI 安装包（`*-x64.msi`，WiX 构建），与 NSIS 一起发布

### CI
- macOS DMG 打包改为单独步骤（tauri 的 create-dmg 在无头 CI 上偶发失败）：
  显式 `CI=true` 跳过 AppleScript 图标排版（-10826），失败自动重试 3 次并打印日志尾部

## [0.1.2] - 2026-08-30

### 修复
- 设置页版本/环境/工作区/服务状态显示为空：设置窗口在应用启动时就已创建（hidden），页面加载早于
  boot 完成，首次拉取到的是空状态——现在每次打开窗口（`settings://refresh`）及 boot 事件都会重新拉取，
  无需再点「重启服务」才会出现
- 设置页服务状态不准确：改为对监听端口做实时健康检查，并按启动阶段显示
  （运行中 / 正在启动 / 启动失败 / 已停止 / 未启动），不再仅凭记录的端口判断
- 重启服务后主页面不刷新：主窗口已加载 DSH 页面时直接导航到新地址（端口变化也正确）；
  托盘/右键菜单重启入口同样等待就绪并刷新主窗口
- 重启服务不杀旧进程导致启动失败：`stop()` 现在按命令行标记（dsh 入口脚本）清理残留/孤儿进程
  （新增 sysinfo 进程枚举），上一会话异常退出遗留的进程不再占着端口和运行时
- 退出/重启/清理进程时闪黑窗口：taskkill 一律加 `CREATE_NO_WINDOW`
- 插件列表不刷新：窗口每次打开时重新拉取已安装插件列表
- Vite dev 崩溃（EBUSY）：watch 忽略原子写入临时目录（`*.tmpdir`）

### 新增
- DSH 更新状态展示：设置页「关于」区显示最近一次更新结果（含后台自动更新，
  失败原因可见）；支持手动「检查更新」/「立即更新」，更新后提示重启服务生效
- 已安装插件列表与市场一致支持滚动，安装输入行移到列表上方

### 技术
- 后端：`get_dsh_update_status` / `check_dsh_update` / `update_dsh` 三个命令；
  `node.rs` 新增安装器输出统一解析器（含单元测试）；`AppState` 新增更新状态存储
- 前端：`settings.ts` 更新状态渲染与按钮逻辑，i18n 中英双语新增 ~10 键

## [0.1.1] - 2026-08-25

### 新增
- 插件管理器（FR-18）：设置窗口新增「插件」卡片
  - 已安装列表：名称/版本/来源（内置/npm/Git/本地）、启用状态、移除按钮
  - 安装：支持 npm 包名、`github:owner/repo`、本地绝对路径；第三方来源（Git/本地）安装前安全确认
  - 启用/禁用：写入 profile 的 `cordis.patch.yml`，dsh 热重载立即生效（无需重启）
  - 新增/移除 bundle 后提示「重启服务后生效」（重启快照对比机制）
- 应用市场：浏览 awesome-dsh-plugin 社区目录 + GitHub `dsh-plugin` 主题仓库，搜索并一键安装
- 默认自动安装 dsh-market 市场插件（npm 包 dshmarket）：启动后台静默安装进 web profile，
  安装后插件页提示重启生效；成功写标记，用户手动移除后不再装回，失败下次启动重试
- 完全复用 dsh 官方 Cordis 插件体系（bundle + 补丁层），零新增运行时依赖（内置 npm 安装）

### 技术
- 后端：新增 `src-tauri/src/plugins.rs`（~1500 行，含 28 个单元测试）
  - 复刻上游 `dsh plugin` 的 `reconcilePlugins`（bundle 注册进 `dsh.profile.bundles`）与 `initProfile`（profile 模板）
  - 内置 npm（`npm-cli.js`）安装，`--legacy-peer-deps` 对齐上游 pnpm 的 `autoInstallPeers: false`
  - 补丁文件用 `serde_yaml::Value` 解析（容忍 `!!js` 标签），追加式写入、原子替换，从不重写用户补丁
  - 插件操作串行锁 + 异步命令（安装不卡界面）；registry 镜像配置复用（auto 失败自动切 npmmirror）
- 前端：`src/plugins.ts`（独立模块），i18n 中英双语新增 ~25 键
- 插件实体安装在 `$DSH_HOME/profiles/web`——dsh 运行时更新（rm -rf 重建）不影响用户插件

### 修复
- 托盘菜单/tooltip 变英文：Windows 下系统 UI 语言（注册表）优先于环境变量——
  从 Git Bash 等环境启动时 `LANG=en_US` 不再干扰托盘文案（网页端与托盘语言一致）

## [0.1.0] - 2026-08-23

### 新增
- Tauri v2 桌面壳：WebView 纯内嵌 DSH Web 界面（无系统浏览器依赖）
- 内置 Node 24 LTS（含 npm），随安装包分发，不依赖系统环境
- 内置 dsh 基线版本（511 个依赖包），首次启动秒级就绪，不依赖网络
- 后台自动更新 DSH 包（官方源失败自动切换国内镜像，失败不打扰用户）
- DSH 更新回滚：更新失败自动恢复旧版本（备份 .dsh-runtime-bak）
- 离线快速降级：无网络时更新检查 0.1s 内静默跳过（不再长时间等待）
- 应用壳自动更新（Tauri updater，签名校验；发布端点待配置）
- 托盘常驻：关闭窗口最小化到托盘；托盘菜单（打开界面 / 设置 / 重启服务 / 退出）
- 设置窗口：开机自启开关、自动更新开关、服务状态、版本信息、工作区/日志目录入口
- 首次使用引导：首启后展示白话引导屏，点「开始使用」进入，仅显示一次
- 中英双语（i18n）：boot 页、设置页、托盘菜单按系统语言自动切换
- 配置持久化：用户设置存于 appData/config.json（默认即最优，0 门槛）
- 单实例：重复启动聚焦已有窗口
- 服务托管：健康检查、限次自动重启、端口冲突自动更换、退出进程树清理
- 白话错误体系：所有用户可见错误为白话 + 一键操作，技术详情折叠
- 鲸鱼品牌图标（全套 ICO/ICNS/PNG + boot 页 logo）

### 技术
- Rust：tauri v2 + single-instance + updater + autostart 插件
- 前端：Vite 多页面（boot 页 + 设置页），TypeScript
- 安装包：NSIS per-user（免管理员），Windows 优先

### 修复
- Node `fs.cpSync` 在中文路径崩溃 → 改用系统原生复制（robocopy/cp）
- Windows `\\?\` verbatim 路径导致 Node 崩溃 → 参数规范化
- Unix 进程组清理阻塞风险 → SIGTERM 5s 超时 + SIGKILL 兜底
- updater 插件配置非法字段 → 移除，per-user 由 bundle 配置决定
- dsh 服务 stdout/stderr 落盘（修复 pipe 未读导致的服务卡死，落实 FR-09）
- 托盘退出前先停服务，避免残留 dsh 进程
- install-dsh.mjs npm 解析支持 mac/Linux 资源布局（lib/node_modules/npm），保证 CI 三平台可用

### 待办（发布前）
- [ ] 配置更新服务器端点（docs/RELEASE.md §2）
- [ ] Windows Authenticode 代码签名
- [ ] macOS 公证、Linux 真机验证（CI 首跑）
