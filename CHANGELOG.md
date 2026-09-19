# Changelog

## [0.1.9] - 2026-09-19

### 修复
- 插件市场装插件/更新报 `ERR_PNPM_UNEXPECTED_STORE`（干净安装的机器上首次更新就可能撞到）。
  根因不止一个：内置 pnpm **11 不设配置时 store 默认是"项目本地"的
  `<profile>\.pnpm-store`**，而 profile 的 `node_modules/.modules.yaml` 记着安装时的 store
  ——换一版内置 pnpm、换一种注入方式、换一台机器，两边就会对不上，pnpm 直接拒绝运行
  （无 TTY 时还会以 `ERR_PNPM_ABORTED_REMOVE_MODULES_DIR_NO_TTY` 出现，表现为"更新失败
  且回滚无法验证"）。现在启动时把 store **钉死**：优先沿用 profile 里已记录的 `storeDir`
  （不搬家、不重装），全新 profile 用稳定的绝对路径（Windows `%LOCALAPPDATA%\pnpm\store`，
  macOS/Linux `~/.local/share/pnpm/store`），并写进 profile 的 `pnpm-workspace.yaml`，
  市场、`dsh plugin`、用户手动敲的 pnpm 读到的都是同一个值
- Windows 上把**内置 Node 目录也插到 `PATH` 最前**（此前只有 pnpm shim）：无论用户机器上
  有没有 node、有没有 pnpm，市场探测、`dsh plugin` 与 pnpm 自己 spawn 的 `node`/`npm`
  都用包内版本，也不再往用户 Node 安装目录写文件（此前会报
  `corepack enable: EPERM ... \nodejs\pnpm`）
- Windows 内置 pnpm 改用**原生转发器**（`pnpm.exe`，由 `scripts/build-forwarder.mjs` 构建、
  作为普通资源随包分发）替代生成的 `pnpm.cmd`：批处理由 cmd.exe 按控制台代码页解码，
  安装路径含该页表示不了的字符（如 `café`）时会被写坏并报「系统找不到指定的路径」；
  原生转发器把 argv/stdio 原样转给包内 `node` + `pnpm.mjs`，不再经过批处理解析。
  旧版遗留的 `pnpm.cmd` 在启动时删除

### 内部
- `pnpm_env.rs`：新增 `pin_store()` / `recorded_store_dir()` 与 12 个单测（store 钉定 8 个、
  原生转发器 2 个、shim 选择 2 个）；`plugins::profile_dir` 提为 crate 内可见
- 转发器由 `scripts/build-forwarder.mjs` 用 **rustc 单文件编译**（不经 cargo，避开构建锁与
  顺序依赖）产出 `resources/binaries/dsh-pnpm-forwarder[.exe]`，随 `bundle.resources` 进包；
  `npm run build` / `npm test` / `tauri build` 都会先跑它（`--if-missing` 供测试复用），
  开发态另拷一份到 `target/<profile>/binaries/`
- 两次打包失败换来的两条硬约束（详见 IMPLEMENTATION §14）：**不要用 `bundle.externalBin`**
  （Windows MSI 会因 WiX 标识符里的 `-` 让 `light.exe` 失败，上游 tauri#14681 未修完；
  另加 Cargo 多 bin 后 tauri CLI 找不到主程序）；**资源产物不能放资源根目录**（tauri-build
  在编译期就会校验条目存在，release 时前端构建产物还没就位就报 `resource path ... doesn't
  exist`），放 `prepare-resources.ps1` 提前生成的目录下才稳

## [0.1.8] - 2026-09-16

### 修复
- macOS 插件市场无法安装插件（报「pnpm 的可执行文件已存在…npm 拒绝覆盖」）：
  桌面端自带 pnpm（构建期生成 `resources/pnpm`，可在 `prepare-resources.ps1`
  用 `-PnpmVer` 换版本、`-SkipPnpm` 跳过），启动时把生成的 shim 目录插到 `PATH` 最前，
  网页插件市场与 `dsh plugin` 直接用内置 pnpm，不再依赖系统 pnpm/corepack
  （macOS 图形启动没有终端 PATH，corepack shim 还会让 `npm i -g pnpm` 以 EEXIST 失败）。
  Windows 同样生效，行为不再取决于用户机器上是否恰好装了可用的 pnpm。两处不能想当然：
  生成的 shim 按 cmd.exe 的规矩写（去掉 exe 路径带的 `\\?\` 前缀、用系统代码页而不是
  UTF-8 编码），安装目录或用户名含中文（`C:\software\DSH 工作台\`）也能正常调用——否则
  市场报「系统找不到指定的路径」；**不设 `PNPM_HOME`**——pnpm 的 store 目录默认跟着它走，
  一设就会把用户已装好的 profile 判成 store 不符，装插件/更新报
  `ERR_PNPM_UNEXPECTED_STORE`（PATH 已足够让市场与 `dsh plugin` 找到内置 pnpm）
- macOS Intel（x86_64）包此前实际不可用：resources 按 **runner** 架构（`uname -m`）准备，
  而程序按编译目标架构找 `node/mac-x64`，在 arm64 runner 上交叉编出的 Intel 包里放的是
  arm64 内置 Node（dsh 基线里的 koffi / node-pty 也按宿主架构编译，交叉构建补不回来）。
  现已**停止发布 x86_64 产物，只发 Apple Silicon**；`prepare-resources.ps1` 新增
  `-NodePlat`（目标平台，非法值报错）并把资源和 `--target` 显式对齐，基线安装补
  `--os/--cpu`，交叉构建时 pnpm 冒烟在宿主跑不动目标 Node 时只警告不失败（要恢复 Intel
  包须换回 Intel runner，步骤见 docs/RELEASE.md）

### 移除
- 网页里自定义的右键菜单（设置/重启服务/退出）：它在捕获阶段 `preventDefault()`
  掉整个页面的右键，与网页自带的右键菜单冲突。这两个入口在托盘右键与设置窗口里
  都还在，功能不丢；Rust 命令与权限仍保留，只是不再注入脚本

### 内部
- 发布正文自动化：Release 的「更新内容」自动取自 `CHANGELOG.md` 对应版本一节
  （`scripts/extract-changelog.mjs`，附 11 个单测），与 `## 安装包` 清单拼成完整正文；
  没有该版本条目时回退顶部的「未发布」一节，两者都没有则正文写明「未找到条目」
  提醒发布者（CI 日志同时告警），不再依赖手写 Release 说明

## [0.1.7] - 2026-09-14

### 修复
- dsh 安装失败可自愈：对失败原因分类（预编译包缺失 / 构建工具链 / 网络 /
  锁冲突），仅在确因原生模块编译导致时才降级重试 `--ignore-scripts`，重试后
  重建必要包并在子进程探测 koffi，探测通过才切换新运行时，避免装坏当前可用
  的安装；失败详情落盘 logs/dsh-update-install.log，用户提示更明确
- macOS CI 编译失败：installer 输出读取去掉多余的 `&mut &` 包装并声明 mut

## [0.1.6] - 2026-09-08

### 新增
- dsh/app 更新链路整合：手动与自动更新并入同一状态机，设置页更新按钮统一为
  单按钮形态（检查更新 → 立即更新 → 下载中可「停止」），阶段与进度实时显
  示；更新完成/失败/发现新版本弹系统通知；更新互斥防并发触发

### 修复
- dsh 更新默认跟随 npm dist-tag `latest`：此前对 dist-tags 全量按 semver 取
  「最高正式版」，无正式版时退化为最高预发布——会把 alpha 通道版本
  （0.1.3-alpha.2）排在 `latest` 指向的 0.1.2-rc.1 之前。现在与
  `npm i @deepseek-ai/dsh`、基线 `-DshVer latest` 一致，缺失 `latest` tag 才
  回退原逻辑；update 早退守卫改为 target ≤ current，已装更高版本不会被降回
  通道

## [0.1.5] - 2026-09-06

### 修复
- dshmarket「立即重启」由壳接管：页内注入 restart-shim 拦截 restart 路由转
  受管重启；watchdog 兜底接管探测（外部自杀式重启自动重新收编），任意插件
  更新后重启不再卡「启动失败」
- 设置页运行状态实时化（轻量状态轮询 + 重启中/恢复过渡态文案）
- 插件增删/dsh 更新被运行进程占用文件失败：统一互斥（OPS_LOCK）、停服原子
  操作自动重启、杀残留扩面等待（ensure_stopped）、installer 安全 swap
  （旧版保留→回滚），全部排他操作收敛单一出口并自动同步托盘/按钮状态
- 重启期间整窗卡死：TCP 探测去锁（probe_port）、重命令 async 化、node 版本
  缓存；用户重启免 3 次/300s 配额（仅 watchdog 自动接管保留）
- 托盘/设置/右键重启入口状态机一致（启动中禁灰、错误态可点）
- Tauri 2.11 remote ACL：内部页面调用 restart_service/open_context_menu 显式
  授权（permissions/app-commands.toml）
- 启动失败诊断与 60s 就绪宽限；安装器 45min 超时；更新互斥与状态如实反映
  重启结果

## [0.1.4] - 2026-09-03

### 修复
- dsh 0.1.2-alpha.5 起启用 URL token 鉴权：壳此前导航到裸 `http://127.0.0.1:<port>`，
  页面报 "authentication required"。现在服务输出改为匿名管道 + 双读线程捕获，提取
  dsh 打印的 `http://127.0.0.1:<port>/?token=…` 完整 URL 并以其导航（含 boot 快照
  兜底、托盘/右键/设置重启路径）；无捕获时回退裸地址，兼容旧版 dsh
  - 兑取会话 Cookie 采用「两跳」：从启动页跨站直接带 token 导航时，WebView2 会把
    Set-Cookie（SameSite=Strict）当第三方 Cookie 拦截导致换不到会话（页面仍显示
    authentication required）；改为先让主窗口落在站点裸地址，再由壳在站内发起带
    token 的导航——同站请求，Cookie 正常落盘（实测站内 fetch('/') 返回 200）
- dsh 启动即退（如插件树加载失败）时原会盲等 30s×2 + 3 次重启后进降级态：现在等待
  循环检测进程退出提前返回，并按输出分类（插件树失败 / 残留锁文件 / 未知）
- 新版 dsh 插件可能在监听后才崩溃（页面短暂可用后服务退出）——此前无人察觉：
  新增就绪后监视，进程意外退出时分类并回到启动页给出恢复入口
- 强杀残留锁（task-board `ledger-v2.lock` 等）导致下次启动失败：分类到残留锁时，
  确认路径在 ~/.dsh 内且无其他宿主进程后自动删除并重试一次
- 服务日志双句柄截断写入会互相覆盖文件头：改为单句柄 append + `try_clone` 共享游标
- 设置页「已是最新版本（）」空括号：版本号不存在时不再附加括号

### 新增
- 启动失败恢复 UI：插件树加载失败时错误页列出不兼容插件并给出「移除不兼容插件」
  按钮（离线可用，复用插件管理链路，移除后自动重启）；错误事件带快照持久副本，
  启动页晚于事件加载也不丢恢复入口
- 浏览器通知支持：DSH 网页（如 dsh-notification 插件）走标准 Notification API 的
  完成/出错等通知，在桌面窗口内同样弹出系统通知——主窗口注入 Notification 兼容
  垫片（permission 恒 granted），经事件通道转发，由 tauri-plugin-notification 弹
  真实 toast。页面权限授予：capabilities remote 显式放开 core:default（此前远端页
  无 IPC，自定义命令在远端不可调，故用事件通道）。Windows 开发态以 PowerShell
  身份显示、安装版为应用身份（官方限制）；点击行为依赖系统激活 + 单实例聚焦

### 内部
- 新模块 `serviceout.rs`：输出捕获（有界滚动缓冲）与失败分类纯逻辑，含单测

## [0.1.3] - 2026-08-31

### 修复
- 服务进程挂载到 Windows Job Object（`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`）：主进程无论以何种方式
  退出（含强杀/崩溃/安装器结束进程），node 服务进程树都随之终止，彻底杜绝孤儿进程
- NSIS 安装/卸载钩子：覆盖/升级安装前整棵树结束正在运行的应用（含 node 服务进程），
  解决升级时「node 被占用」导致文件覆盖失败；并兜底清理安装目录下的残留 node 进程

### 新增
- Windows 产物新增 MSI 安装包（`*-x64.msi`，WiX 构建），与 NSIS 一起发布
  - MSI 使用 `wix.language: "zh-CN"`（代码页 936），规避 WiX 默认 en-US 代码页 1252
    不支持中文 productName 导致 `light.exe` 失败的问题（tauri#8363）

### CI
- macOS DMG 打包改为单独步骤（tauri 的 create-dmg 在无头 CI 上偶发失败）：
  显式 `CI=true` 跳过 AppleScript 图标排版（-10826），失败自动重试 3 次并打印日志尾部
- 每次构建前删除该 tag 的旧 Draft Release：解决重跑/移动 tag 后上传同名资产报
  `already_exists`（GitHub 不允许重复资产名），使 workflow 可重复运行

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
