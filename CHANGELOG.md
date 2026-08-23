# Changelog

## [0.1.0] - 2026-08-23

### 新增
- Tauri v2 桌面壳：WebView 纯内嵌 DSH Web 界面（无系统浏览器依赖）
- 内置 Node 24 LTS（含 npm），随安装包分发，不依赖系统环境
- 内置 dsh 基线版本（511 个依赖包），首次启动秒级就绪，不依赖网络
- 后台自动更新 DSH 包（官方源失败自动切换国内镜像，失败不打扰用户）
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
