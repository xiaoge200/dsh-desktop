# DSH 工作台（dsh-desktop）

将 DSH（DeepSeek Harness）Web 封装为跨平台桌面应用：**内置 Node 运行时 + 安装后自动获取最新 DSH 包 + 本地运行 + WebView 纯内嵌**。

需求与设计文档见 [`dsh-desktop-plan.md`](../dsh-desktop-plan.md)。

## 特性

- 🖥️ Tauri v2 壳（Windows / macOS / Linux）
- 📦 内置 Node 24 LTS（随安装包分发，含 npm，不依赖系统环境）
- 🔄 启动时自动安装/更新最新 `@deepseek-ai/dsh`（npm registry，官方源失败自动切国内镜像）
- 🪟 WebView 纯内嵌 DSH Web 界面（无系统浏览器依赖）
- 🧩 托盘常驻、单实例、服务健康检查与限次自动重启
- 🛡️ 服务仅绑定 `127.0.0.1`（沿用 dsh 安全默认，`0.0.0.0` 被上游禁用）
- 💬 全白话错误提示（技术术语只进日志）

## 目录结构

```
dsh-desktop/
├─ src/                     # 前端（boot 进度页）
├─ src-tauri/               # Rust 壳
│  ├─ src/
│  │  ├─ main.rs            # 入口
│  │  ├─ lib.rs             # Tauri builder / boot 编排 / 托盘 / 单实例
│  │  ├─ state.rs           # 全局状态与启动阶段机
│  │  ├─ node.rs            # 内置 Node 解析与冒烟、安装器调用
│  │  ├─ supervisor.rs      # dsh 服务进程托管（spawn/健康检查/重启/清理）
│  │  └─ logging.rs         # 滚动日志
│  ├─ capabilities/         # 权限声明
│  └─ tauri.conf.json
├─ resources/
│  ├─ node/                 # 内置 Node 发行版（构建期注入）
│  └─ installer/
│     └─ install-dsh.mjs    # DSH 运行时安装器（npm view/install + 镜像 + 完整性）
└─ package.json
```

## 开发

前置：Rust 1.77+、Node 18+、平台系统依赖（见 [Tauri 文档](https://tauri.app/start/prerequisites/)）。

```bash
npm install
npm run tauri dev      # 开发模式
npm run tauri build    # 构建安装包
```

> 注意：`npm install` 依赖 npm 的脚本执行（`@tauri-apps/cli`、`esbuild` 需要 postinstall）。
> 若环境启用了 npm `allow-scripts` 策略，需在项目 `.npmrc` 的 `allow-scripts` 中放行它们。

## 运行机制

1. 启动 → 校验内置 Node（`node --version` 冒烟）
2. 运行 `install-dsh.mjs prepare`：无有效安装时从**内置基线**（`resources/dsh-baseline`，随安装包分发）快速复制到 `appData/dsh-runtime`（秒级，不依赖网络）
3. 后台 `install-dsh.mjs check/update`：`npm view @deepseek-ai/dsh version` 对比 → 有新版本自动安装（官方源失败自动切 npmmirror），失败不打扰用户，下次启动重试
4. spawn `node <dsh>/lib/bin.js web --no-open --port <port>`（默认 3080，被占用自动换空闲端口，用户无感）
5. 健康检查（HTTP GET /）通过后，WebView 装载 `http://127.0.0.1:<port>`
6. 关闭窗口 → 最小化到托盘；托盘「退出」→ 清理 dsh 进程树后退出

## 平台说明

| 平台 | 说明 |
|---|---|
| Windows | NSIS per-user 安装（免管理员）；WebView2（Win10/11 自带或引导安装） |
| macOS | 需 Developer ID 签名 + 公证（内置 Node 二进制有 quarantine 限制）；WKWebView |
| Linux | deb/rpm/AppImage；依赖 webkit2gtk-4.1 |
