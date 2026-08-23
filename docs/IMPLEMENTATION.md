# 实现记录（dsh-desktop）

> 记录开发过程中遇到的关键问题与解决方案，供团队维护参考。
> 关联需求文档：`../dsh-desktop-plan.md`（v0.3）

## 里程碑进度

| 阶段 | 状态 | 说明 |
|---|---|---|
| M0 可行性验证 | ✅ | 内置 Node 24 + dsh 基线可启动 `dsh web`，HTTP 200 |
| M1 Windows MVP | ✅ | Tauri v2 壳、基线准备、服务托管、WebView 内嵌、托盘、单实例、日志、白话错误 UI |
| M2 更新与分发 | 🚧 | DSH 自动更新 ✅；Tauri updater 代码+密钥+签名链路 ✅；更新服务器端点待配置（发布时） |
| M3 跨平台 | 🚧 | CI 三平台矩阵已配置（.github/workflows/build-release.yml）；macOS/Linux 真机验证待 CI 首跑 |
| M4 打磨发布 | ⏳ | 待内测 |

## 关键决策（实现期新增）

1. **内置基线 dsh 版本**（O5 从开放问题转为已定）
   - 实测：`npm install @deepseek-ai/dsh` 需 511 包 / 约 30 分钟（npmmirror）——0 门槛下不可接受。
   - 方案：安装包内置完整 dsh 依赖树（`resources/dsh-baseline`，解压 192MB / 压缩后约 25MB），
     首启 `install-dsh.mjs prepare` 秒级复制；更新走后台 `check`/`update`。
2. **Node 版本：24 LTS**（O3 已定）——实测 dsh 正常，内置 npm 11.6.0。

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

## 运行环境事实

- 内置 Node：24.9.0（含 npm 11.6.0），resources/node/win-x64，约 98MB。
- 基线 dsh：0.1.1-rc.2（511 包，解压 192MB），resources/dsh-baseline。
- 安装包（NSIS per-user）：约 47MB，安装后 297MB。
- 服务默认 127.0.0.1:3080，被占用自动换空闲端口（实测 49460/64609）。
