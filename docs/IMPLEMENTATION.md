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
| 自动化测试 | 🟡 | Rust 单测 82 个（`cargo test --lib`：store 钉定 8 + 原生转发器 2 + shim 选择 2）+ `install-dsh.mjs` 8 个 + `scripts/` 发布工具 11 个（`node --test scripts/`）；CI 目前只跑 `scripts/` 那组与编译检查，另两组待接入（本机 1 个既有失败：`café` 路径的 shim 用例，见 §13/§14） |
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
   - **市场（dshmarket）要 pnpm，壳自带一个**：市场安装插件走 pnpm，自己探测
     `pnpm --version`，失败时用 corepack / `npm i -g pnpm` 现场装——macOS 从 Finder
     启动没有终端 PATH，corepack shim 又会让全局安装以 EEXIST 失败（用户只看到
     「pnpm 的可执行文件已存在，npm 拒绝覆盖」）。所以构建期把 pnpm 打进
     `resources/pnpm`（`prepare-resources.ps1`），启动时 `pnpm_env.rs` 在
     `<appData>/pnpm-home` 生成 `pnpm` / `pnpm.cmd` shim 并**把它插到 `PATH` 最前**：
     市场（`spawnEnv()` 从继承的 PATH 出发，`toolSearchDirs` 只往后追加候选目录）与
     `dsh plugin`（内部 `spawnSync("pnpm")`）都用内置版本，与系统工具链无关。
     不设 `PNPM_HOME`——store 目录一律由 `pin_store()` 钉死，理由见 §13。
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

### 10. macOS 插件市场卡在「pnpm 的可执行文件已存在」
- 现象：mac 版网页插件市场锁在「安装插件前需要先配置 pnpm 环境」。同一个根因在不同机器
  上给出不同提示，取决于现场探测/安装失败成什么样（dshmarket `provisionHint` 五选一）：
  - `pnpm --version` 失败 + 全局目录已有同名 pnpm → EEXIST「可执行文件已存在…npm 拒绝覆盖」
  - 同上但 npm 无写权限（Node 装在 `C:\Program Files\nodejs`、macOS 官方 pkg 装到
    `/usr/local`）→ EPERM「没有权限写入 Node 的安装目录…用管理员/sudo」
  - 图形/桌面启动没有 npm/corepack → 「找不到 npm/corepack（…不继承终端 PATH）」
  - 网络受限导致 corepack shim 下不到 pnpm 本体 → 「装 pnpm 时网络失败」
  Windows 同版本「正常」只是那台机器 PATH 上恰有可用的 pnpm。
- 原因：市场（dshmarket 插件）装插件走 pnpm，壳体并不提供 pnpm，于是市场自己探测并
  现场安装。macOS 从 Finder/Dock 启动的应用只有 `/usr/bin:/bin:/usr/sbin:/sbin`
  的 PATH（不留继承终端 profile），corepack 又已在 npm 全局目录留下同名 shim——
  `corepack enable pnpm` 装不出能跑的 pnpm，`npm i -g pnpm` 则直接 EEXIST/EPERM，市场
  于是死循环。
- 解决：构建期内置 pnpm 到 `resources/pnpm`；启动（`boot()`）时生成
  `<appData>/pnpm-home/pnpm[.cmd]`，shim 用内置 Node 跑内置 pnpm（Unix 版 chmod 755），
  并把该目录插到 `PATH` 最前。环境变量是进程级的，服务及其 pnpm/dsh 子进程全部继承，
  重启用新起的服务也一样。内置 pnpm 可用时 `probePnpm()` 直接成功，市场**根本不会走
  现场安装**，上面五条提示一条都不会出现。
- 覆盖边界：注入只作用于壳自己起的服务进程。watchdog 发现端口上有外部服务而自己的子进程
  已退出时，会重新收编并重启（`service.rs` 的 Takeover 分支），所以市场最终总是跑在带
  shim PATH 的进程里；启动时若首选端口被外部 `dsh web` 占用则改选空闲端口，也不会复用
  别人的服务。
- 验证：`resources/node/win-x64/node.exe resources/pnpm/bin/pnpm.mjs --version` →
  `11.7.0`；生成的 `pnpm.cmd` 经 `cmd /d /s /c pnpm --version`、`spawnSync("pnpm",
  {shell:true})`（`dsh plugin` 的调用形态）均为 0；POSIX shim 在 Git Bash 下
  `pnpm-real --version` 亦返回 11.7.0。中文路径见 §12——当时是手写 shim 验的，
  漏掉了 `write_shim()` 自己的写法。
  资源打包：`tauri build`（含 `tauri dev`）会把 `bundle.resources` 复制到 resource_dir，
  实测 `src-tauri/target/debug/{pnpm,node,dsh-baseline}` 齐全，`bundled_entry()` 在开发态
  同样能找到 `pnpm/bin/pnpm.mjs`。
- 已发布版本的处理：0.1.7 及更早没有内置 pnpm，只能在终端按提示装一个
  （`npm i -g pnpm --force` / `brew install pnpm`）后重启应用；README 的 FAQ 已写明。

### 11. macOS x86_64 包不可用：资源按 runner 架构而不是 target 架构准备
- 现象：Intel 版 `.app`/`.dmg` 在 Intel Mac 上直接「程序文件不完整，请重新安装」（或即使
  起来也缺原生模块）。
- 原因：`prepare-resources.ps1` 原先按 **runner** 架构（`uname -m`）取内置 Node，而
  `src/node.rs` 按**编译目标**架构找 `node/<mac-arm64|mac-x64>/node`。CI 在 arm64 的
  `macos-latest` 上交叉编 `--target x86_64-apple-darwin`，于是包里是 `node/mac-arm64`、
  程序找 `node/mac-x64`。同类错配还在 `resources/dsh-baseline`：`koffi`（install 钩子
  `cnoke --prebuild`）和 `node-pty`（`scripts/prebuild.js || node-gyp rebuild`）按**宿主**
  架构编译/拷贝，npm 的 `--os/--cpu` 也覆盖不到它们。
- 解决：脚本新增 `-NodePlat`（目标平台，取值 win-x64 / mac-arm64 / mac-x64 / linux-arm64 /
  linux-x64，非法值直接报错），据此决定下载哪个 Node 包与解包方式；宿主侦测改为
  Windows 走 `$env:OS`、mac/linux 走 `uname`（顺带兼容 PowerShell 5.1，不再依赖
  `$IsWindows/$IsMacOS`；Windows 上 PATH 里的 Git `uname` 在受限环境会硬失败，不能先调）。
  发布矩阵把 `-NodePlat` 与 `--target` 绑定；基线安装补 `--os/--cpu`（原生构建无副作用，
  交叉时至少平台门控的 optionalDependencies 不会装错）。
- **决定：x86_64 产物下掉，macOS 只发 Apple Silicon。** 交叉构建修不好（见上），而为一个
  已经发不出来的包维持 Intel runner 不值得——0.1.7 之前的 Intel DMG 本来也起不来，没有
  实际用户损失。要恢复：矩阵加回一条 `macos-15-intel`（`--target x86_64-apple-darwin` +
  `nodePlat: mac-x64` + `rustTargets: x86_64-apple-darwin`），release job 的 `prefix_dir`
  与 Release 正文同步加回；`-NodePlat mac-x64` 的下发分支已就绪。
- 交叉构建时 pnpm 冒烟会先试 `node --version`：宿主跑不动就只警告跳过（不是 pnpm 的问题），
  能跑才要求 `pnpm --version` 成功。
- 验证：默认路径在 Windows 上 `==> pnpm smoke ok: pnpm 11.7.0 on node v24.9.0`（exit 0）；
  `-NodePlat mac-x64` 会走 darwin-x64 下载分支、`-SkipPnpm` 可跳过、`-NodePlat mac-mips`
  被校验拒绝（exit 1）；宿主跑不动目标运行时（伪造 `resources/node/mac-x64/node`）时
  只打印 `pnpm smoke skipped` 并以 0 退出。

### 12. Windows：市场装插件/更新报「系统找不到指定的路径」
- 现象：Windows 上 `dshmarket` 更新、市场装插件失败，错误里带 `系统找不到指定的路径。`
  （壳里显示成 GBK 乱码 `ϵͳ�Ҳ���ָ����·����`）与
  `dsh: pnpm failed in profile directory <profile>`。macOS 同版本正常；Windows 以前
  「正常」是因为那时没有内置 pnpm，市场走的是用户自己装的系统 pnpm——0.1.8 注入内置
  pnpm（§10）后才暴露。
- 原因：`pnpm-home/pnpm.cmd` 是给 **cmd.exe** 读的批处理文件，而驱动它的 Node/pnpm
  路径由 `write_shim()` 直接拼进文件，两处都不合 cmd.exe 的规矩：
  1. 路径来自 `resource_dir`（由 exe 路径推导，开发态实测带 `\\?\` 前缀）。
     cmd.exe 把 `\\?\C:\...` 当 UNC 路径，报 `ERROR_PATH_NOT_FOUND`。
  2. `std::fs::write` 写的是 UTF-8，cmd.exe 却按**控制台代码页（OEM）**解码批处理：
     安装目录含中文（默认装在 `C:\software\DSH 工作台\`，见日志）时路径整段变乱码，
     同样报「系统找不到指定的路径」。内置 Node/pnpm 都在那个目录里，所以不是文件缺失，
     是 shim 解析不出来。
- 解决：`write_shim()` 落盘前用 `node::normalize_for_node()` 去掉 `\\?\`（该函数本就是
  为 Node 子进程写的，这里同样适用）；Windows 落盘改走 `write_script()`，用
  `GetOEMCP()` + `WideCharToMultiByte(WC_NO_BEST_FIT_CHARS)` 把整份批处理编成控制台
  代码页再写。代码页表示不了的字符（路径里带 emoji 之类）记一条 warn 后照样写：那种
  路径本来就没法从批处理里启动，报错比无声失败好。Unix 侧仍是纯文本写入 + chmod 755。
- 验证：修复前 `cmd /c <appData>/pnpm-home/pnpm.cmd --version` 复现「系统找不到指定的
  路径。」，修复后同一命令返回 `11.7.0`；单测 `write_shim_strips_verbatim_prefix`、
  `shim_runs_when_the_install_dir_is_not_ascii`（临时目录名 `café-<pid>`、入口故意传
  `\\?\` 形式，经 `cmd /c` 真跑内置 Node）各覆盖一条规则，回归时会直接红；另用 GBK 手写
  shim 指向 `C:\software\DSH 工作台\node\win-x64\node.exe` + 内置 `pnpm.mjs`，实测
  `11.7.0`（即发行版装中文目录的场景）。

### 13. 更新/装插件报 ERR_PNPM_UNEXPECTED_STORE：store 目录必须由壳钉死
- 现象：Windows 上市场装插件/更新 `dshmarket` 报
  `ERR_PNPM_UNEXPECTED_STORE: Unexpected store location`（无 TTY 时也可能表现为
  `ERR_PNPM_ABORTED_REMOVE_MODULES_DIR_NO_TTY`——pnpm 想删掉 node_modules 重装但没人
  能确认）。市场日志里典型是成对的 `update-rollback … restoration of the previous build
  could not be verified` + `update … exit=1`：失败与"回滚验证也失败"是同一个根因（回滚
  同样拿 pnpm 重装）。**另一台干净安装 0.1.8 的 Windows 机器上首次就撞到它。**
- 原因（两个，先后叠加）：
  1. `activate()` 曾设 `PNPM_HOME=<appData>/pnpm-home`，而 **pnpm 的 store 默认跟着
     `PNPM_HOME` 走**（设了变 `<PNPM_HOME>\store\v11`）。已有 profile 的
     `node_modules/.modules.yaml` 记着安装时的 store，`pnpm install` 时
     `checkCompatibility()` 一比不一致就抛错（pnpm 的
     `path.relative(modules.storeDir, storeDir) !== ""`）：壳一注入 `PNPM_HOME`，用户
     所有装过的 profile 都被判成"store 不对"。
  2. **0.1.8 把 PNPM_HOME 去掉只解决了这一半。** 内置 pnpm 11.7.0 **不设任何配置时
     store 是"项目本地"的 `<profile>/.pnpm-store/v11`**（实测 `pnpm config get
     store-dir` → `undefined`，`pnpm store path` → `<cwd>\.pnpm-store\v11`），而用户
     此前用系统 pnpm 装好的 profile 记的是**全局** store
     `%LOCALAPPDATA%\pnpm\store\v11`。于是"壳改一次注入方式 / 换一版内置 pnpm / 换一台
     机器"都会让两边对不上——这正是干净机器首次更新就报错的原因：`activate()` 每次启动
     都生成 shim，但 store 解析取决于当时是谁、在哪、以什么参数跑的 pnpm。
- 解决：`pnpm_env.rs` 的 `pin_store()` 在每次启动时把 store **钉死**，并写进 profile：
  1. 先读 profile 的 `node_modules/.modules.yaml` 里的 `storeDir`（绝对路径），**有就
     原样沿用**——换 store 会让 pnpm 拒绝运行直到整棵 node_modules 重装（GB 级重下），
     所以连续性优先；
  2. 没有（全新 profile）才用稳定默认值：Windows `%LOCALAPPDATA%\pnpm\store`、macOS/Linux
     `$XDG_DATA_HOME/pnpm/store` 或 `~/.local/share/pnpm/store`（绝不用相对路径）；
  3. 把这个绝对路径写进 profile 的 `pnpm-workspace.yaml` 的 `storeDir:`（逐行 upsert，
     `allowBuilds` 等其他行原样保留）——这个文件在 profile 目录里，**市场、`dsh plugin`、
     用户手动敲的 pnpm 都读得到**，与 cwd、PATH、谁启动无关。旧版 pnpm 不认识这个键，
     但那时它的默认值本来就是全局 store，与 1) 读到的值一致。
  可用 `DSH_PNPM_STORE_DIR=<绝对路径>` 覆盖（测试与逃生口）。
- 另：`activate()` 现在同时把**内置 Node 目录**插到 `PATH` 最前（shim 目录之前），于是
  "没装 node"、"有 node 没 pnpm"、"有 node 也有 pnpm" 三种机器都统一用包内的
  `node.exe`/`npm.cmd`：市场的 corepack/`npm i -g` 兜底路径不再写用户的 Node 安装目录
  （`C:\software\nodejs\pnpm` 那种 `EPERM` 就是它），pnpm 自己 spawn 的 `node` 也是包内
  版本。
- 验证（内置 pnpm 11.7.0 实测）：
  - 复现：同一工程 storeA 装包 → 再用 `--config.store-dir=storeB` 跑同一条命令 → exit 1、
    `ERR_PNPM_UNEXPECTED_STORE`（错误信息里明确写"currently linked from … now wants to
    use …"）；把 store 指回 storeA → exit 0。
  - 修复：`pnpm-workspace.yaml` 写 `storeDir` 后，`pnpm store path`（cwd=profile）与
    `pnpm store path --dir <profile>`（cwd 在别处）都返回该路径；装一个包（`.modules.yaml`
    记下同一路径），**再跑一次同命令 exit 0 不再报错**。
  - 单测：`pnpm_env::store_tests` 8 个（沿用旧 store、全新 profile 用绝对默认值、替换陈旧
    路径且保留 `allowBuilds`、幂等、路径带 `'` 的转义、不可达 profile 报错、默认值在无
    `LOCALAPPDATA` 时仍为绝对路径、PATH 顺序内置 node 在 shim 之前）。
- 仍存在的边界（已单测暴露，未修）：`write_script()` 按 `GetOEMCP()` 写 `pnpm.cmd`，但
  cmd.exe 是按**控制台代码页**解批处理的；目标代码页表示不了的字符（本机单测用 `café`
  复现，与本次改动无关，改动前同样红）会写坏路径 → 报"系统找不到指定的路径"。中文安装
  目录（GBK 可表示）不受影响；Windows 已改走原生 `pnpm.exe` 转发器、这条批处理回退路径
  只在没有转发器时才走到（见 §14）。

### 14. 四种用户机器 / 两种系统的现状与边界
- 目标矩阵与今天的答案（"包内优先、系统兜底、store 钉死"）：
  1. **没有 node**：包内 `node.exe` 跑包内 `pnpm.mjs`，PATH 现在同时含内置 Node 目录与
     shim 目录 → 市场探测、`dsh plugin`、pnpm 自己 spawn 的 `node` 全部命中包内版本；
  2. **有 node 没有 pnpm**：同上（PATH 最前是包内目录）；即便内置 pnpm 不可用而回落到
     市场的 corepack/`npm i -g` 兜底，用的也是包内 npm，不会再往
     `C:\Program Files\nodejs` / `C:\software\nodejs` 写（历史 `EPERM` 的来源）；
  3. **有 node 也有 pnpm**：PATH 最前仍是包内版本，用户自己那套只在包内资源缺失时才被
     用到（此时日志有 `pnpm: no bundled pnpm` 警告）；
  4. Windows/macOS 一致：store 路径与 shim 形态按平台取（Windows
     `%LOCALAPPDATA%\pnpm\store` + 原生 `pnpm.exe`；macOS `~/.local/share/pnpm/store` +
     可执行 `pnpm` 脚本，chmod 755）。
- 权限边界：所有写入都落在用户自己的目录（`<appData>\pnpm-home`、profile 目录、store），
  不需要管理员；卸载不删用户数据（NSIS per-user）。
- Windows 路径健壮性（已做）：不再用生成的 `pnpm.cmd` 当主路径，改为**原生转发器**
  `dsh-pnpm-forwarder`（`src-tauri/src/bin/dsh-pnpm-forwarder.rs`：定位包内 `node` +
  `pnpm.mjs`，原样转发 argv/stdio，找不到包内资源就报 127 并提示重装）。
  `scripts/build-forwarder.mjs` 在 `npm run build` / `npm test` / `tauri build` 之前编好它，
  并放到 tauri-build 编译期会查的位置（`target/<profile>/<bin>-<triple>[.exe]`、crate 根、
  `src-tauri/binaries/`；脚本编自己时会临时摘掉 `externalBin`，否则"源必须已存在"的
  检查会自锁，编完按字节还原配置），随 `bundle.externalBin` 进包；启动时 `install_shim()`
  把它拷成 `<appData>\pnpm-home\pnpm.exe` 并删除旧版留下的 `pnpm.cmd`（`.EXE` 在 PATHEXT
  里优先，市场与 `dsh plugin` 的裸 `pnpm` 自然命中）。找不到转发器就退回批处理 shim，
  开发态与老包仍能工作；macOS 继续用 POSIX shim（sh 按 UTF-8 读，没有这个问题）。
- 仍存的边界：批处理回退路径受控制台代码页限制（§13 末尾的 `café` 用例），
  只有"没有原生转发器"时才会走到。
- `cargo test --lib` 现状：81 通过，1 失败——即上述 `café` 用例（改动前同样失败，
  是这台机器控制台代码页导致的既有问题，不是回归）。

## 运行环境事实

- 内置 Node：24.9.0（含 npm 11.6.0），`resources/node/<win-x64|mac-arm64|mac-x64|linux-*>`
  ——程序按编译目标架构找对应目录，CI 每个矩阵条目只准备目标那一个（约 98MB）；目标平台由
  `prepare-resources.ps1 -NodePlat` 指定。
- 基线 dsh：0.1.1-rc.2（511 包，解压 192MB），resources/dsh-baseline；含原生模块
  （koffi / node-pty / sharp / ripgrep），因此必须与目标架构一致地安装。
- 内置 pnpm：11.7.0（npm 包 `pnpm`，`resources/pnpm`，449 文件 / 解压 17.8MB），只服务
  网页插件市场与 `dsh plugin`；换版本用 `prepare-resources.ps1 -PnpmVer <ver>`，
  不需要时 `-SkipPnpm`。
- 安装包（NSIS per-user）：约 47MB，安装后 297MB（+内置 pnpm 解压后约 18MB）。
- 服务默认 127.0.0.1:3080，被占用自动换空闲端口（实测 49460/64609）。
