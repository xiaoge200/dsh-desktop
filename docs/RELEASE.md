# 发布手册（Release Guide）

> 从 v0.1.0 起，发布流程由 GitHub Actions 全自动完成（`.github/workflows/release.yml`）。
> 你只需要：更新版本号 → 打 tag → 在 Releases 页面点一下「发布」。

## 1. 一键发布流程

1. **更新版本号**（三处保持一致）：
   - `src-tauri/tauri.conf.json` → `version`
   - `src-tauri/Cargo.toml` → `version`
   - `package.json` → `version`
2. **更新** `CHANGELOG.md`
3. **提交并打 tag 推送**：

   ```bash
   git tag v0.2.0
   git push origin v0.2.0
   ```

4. GitHub Actions 自动构建并发布为 **Draft Release（草稿）**：
   - Windows：NSIS per-user 安装包（`*-setup.exe`）
   - macOS：Intel `*-x64.dmg`、Apple Silicon `*-aarch64.dmg`（含 updater 用的 `.app.tar.gz`）
   - 更新清单 `latest.json`（自动合并各平台签名）
5. 到仓库 **Releases** 页面检查产物，确认无误后点「发布」公开（公开后应用内自动更新才会生效）。

> 也可以不推 tag，直接在 Actions 页面手动运行 `Release` workflow（会按当前版本创建对应 tag）。

## 2. CI 做了什么

`.github/workflows/release.yml`（tag `v*` 触发）：

| 任务 | 说明 |
|---|---|
| build × 3 | `windows-latest`（NSIS）、`macos-13`（Intel x64）、`macos-14`（Apple Silicon），各自下载内置 Node + 构建 dsh 基线 → `tauri build` → tauri-action 上传产物到同一个 Release |
| merge-updater-json | 仅在配置了签名密钥时运行；下载全部产物，由 `scripts/merge-updater-json.mjs` 合并各平台 `.sig` 生成完整 `latest.json` 并上传 |

> 为什么单独合并：tauri-action 在多平台矩阵下各自生成的 `latest.json` 会互相覆盖
> （[tauri-apps/tauri-action#409](https://github.com/tauri-apps/tauri-action/issues/409)、[#927](https://github.com/tauri-apps/tauri-action/issues/927)），
> 所以统一由合并任务从 `.sig` 文件重建完整清单，保证三个平台都在。

另有 `.github/workflows/ci.yml`：PR / main 分支的快速编译检查（不打包），防止坏代码合入。

## 3. 前置：密钥与 GitHub Secrets

### updater 签名（应用内自动更新必需）

Tauri updater 使用 Ed25519 签名验证更新包，密钥已在开发期生成：

- 私钥：`src-tauri/tauri.updater.key`（**已 gitignore，绝不提交**）
- 公钥：`src-tauri/tauri.updater.key.pub`（已写入 `tauri.conf.json → plugins.updater.pubkey`）

> ⚠️ 私钥 + 密码丢失将导致无法再发布更新。请妥善备份（密码管理器 / 加密存储）。

到仓库 **Settings → Secrets and variables → Actions** 添加：

| Secret | 值 | 是否必填 |
|---|---|---|
| `TAURI_SIGNING_PRIVATE_KEY` | 私钥内容（`Get-Content src-tauri/tauri.updater.key -Raw`） | 必填（否则无更新清单，自动更新不可用） |
| `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | 私钥密码 | 必填（同密钥） |
| `APPLE_CERTIFICATE` | macOS Developer ID 证书 `.p12` 的 **Base64**（`certutil -encode` 或 `base64 -i` 生成） | 选填（macOS 签名） |
| `APPLE_CERTIFICATE_PASSWORD` | `.p12` 密码 | 选填 |
| `APPLE_SIGNING_IDENTITY` | 签名身份，如 `Developer ID Application: xxx (TEAMID)` | 选填 |
| `APPLE_ID` / `APPLE_PASSWORD` | Apple 账号与 App 专用密码（公证用） | 选填 |
| `APPLE_TEAM_ID` | Apple Team ID | 选填 |

未配置选填 Secrets 时构建照常成功，只是产物未签名/未公证（macOS 上 Gatekeeper 会拦截，需右键打开）。

## 4. 更新端点

应用内自动更新从 `tauri.conf.json → plugins.updater.endpoints` 拉取版本清单，
当前已配置为本仓库的 Releases：

```json
"endpoints": [
  "https://github.com/xiaoge200/dsh-desktop/releases/latest/download/latest.json"
]
```

`latest.json` 由 CI 的合并任务自动生成并上传到 Release，无需手工维护。

> 注意：`/releases/latest/` 只指向**已公开**的最新 Release。草稿期间的更新检查会失败（应用内静默跳过，不影响使用），公开后即生效。

## 5. 本地构建

```bash
npm install
pwsh scripts/prepare-resources.ps1     # 下载内置 Node + 构建 dsh 基线（resources/ 不入库）
npm run tauri build                    # Windows: 产出 NSIS 安装包
npm run tauri build -- --bundles app,dmg   # macOS 本地打包用
```

Windows 本地构建产物示例：
`src-tauri/target/release/bundle/nsis/DSH 工作台_0.1.0_x64-setup.exe`（约 48MB）

## 6. 常见问题

**Q：Release 里没有 latest.json？**
说明 `TAURI_SIGNING_PRIVATE_KEY` 未配置，合并任务被跳过。配置后重新运行 workflow 即可。

**Q：latest.json 只有部分平台？**
检查合并任务的日志：某个构建任务没产出 `.sig`（签名失败）或产物命名不符（macOS 的 `.app.tar.gz` 需包含 `x64` / `aarch64`）。

**Q：macOS 产物打不开？**
未签名/未公证版本需右键 →「打开」绕过 Gatekeeper；正式分发请配置 `APPLE_*` Secrets 后重新发布。

**Q：dsh 基线版本想固定？**
`pwsh scripts/prepare-resources.ps1 -DshVer <版本>`（默认 `latest`）。固定后记得把版本号写进 CHANGELOG。
