# 发布手册（Release Guide）

> 对应需求文档 §10 里程碑 M2/M3/M4 的发布部分。

## 1. 前置：签名密钥

Tauri updater 使用 Ed25519 签名验证更新包。密钥已在开发期生成：

- 私钥：`src-tauri/tauri.updater.key`（**已 gitignore，绝不提交**）
- 公钥：`src-tauri/tauri.updater.key.pub`（已写入 `tauri.conf.json → plugins.updater.pubkey`）

> ⚠️ 私钥 + 密码丢失将导致无法发布更新。请将私钥与密码妥善备份（密码管理器 / 加密存储）。

### GitHub Actions Secrets（CI 发布需要）

| Secret | 值 |
|---|---|
| `TAURI_SIGNING_PRIVATE_KEY` | 私钥内容（`Get-Content src-tauri/tauri.updater.key -Raw`） |
| `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | 私钥密码 |
| `GITHUB_TOKEN` | 无需手动配置（Actions 自动提供） |

## 2. 更新服务器

Tauri updater 从 `tauri.conf.json → plugins.updater.endpoints` 拉取版本清单。
当前为占位地址，发布前必须替换为真实端点，例如：

```json
"endpoints": [
  "https://updates.example.com/dsh-desktop/{{target}}/{{arch}}/{{current_version}}"
]
```

### 端点响应格式

端点需返回 `application/json`，格式：

```json
{
  "version": "0.2.0",
  "notes": "更新说明",
  "pub_date": "2026-08-23T00:00:00Z",
  "platforms": {
    "windows-x86_64": {
      "signature": "dW50cnVzdGVk...（tauri signer sign 生成）",
      "url": "https://updates.example.com/dsh-desktop/0.2.0/dsh-desktop_0.2.0_x64-setup.exe"
    },
    "darwin-aarch64": {
      "signature": "dW50cnVzdGVk...",
      "url": "https://updates.example.com/dsh-desktop/0.2.0/dsh-desktop_0.2.0_aarch64.dmg"
    },
    "linux-x86_64": {
      "signature": "dW50cnVzdGVk...",
      "url": "https://updates.example.com/dsh-desktop/0.2.0/dsh-desktop_0.2.0_amd64.deb"
    }
  }
}
```

### 生成签名（发布时）

```bash
# 对每个安装包签名，产出 signature 字符串（-f 为私钥文件路径）
npx tauri signer sign \
  -f src-tauri/tauri.updater.key \
  -p "$TAURI_SIGNING_PRIVATE_KEY_PASSWORD" \
  "安装包路径"
```

> 实测（2026-08）：`npx tauri signer sign -f <key> -p <password> <file>` 可正常产出
> `dW50cnVzdGVk...` 格式签名，写入更新清单的 `signature` 字段即可。

## 3. 发布流程

1. 更新版本号：`src-tauri/tauri.conf.json`（version）+ `package.json`（version）
2. 更新 CHANGELOG
3. 打 tag 推送：

```bash
git tag v0.2.0
git push origin v0.2.0
```

4. GitHub Actions 自动执行三平台构建（workflow: `build-release.yml`）：
   - Windows: NSIS per-user 安装包
   - macOS: dmg
   - Linux: deb + AppImage
   - 产物发布为 Draft Release（需人工确认后公开）
5. 将各平台安装包上传到更新服务器，用 `tauri signer sign` 生成 signature，写入版本清单 JSON。

## 4. 三平台注意事项

| 平台 | 注意事项 |
|---|---|
| Windows | NSIS per-user（免管理员）；Authenticode 代码签名（可选但建议）；WebView2 系统自带 |
| macOS | 需 Developer ID 证书签名 + 公证（notarization），否则内置 Node 二进制被 quarantine 拦截 |
| Linux | 依赖 `libwebkit2gtk-4.1-0`、`libappindicator3-1`；deb 依赖已在 tauri.conf.json 声明 |

## 5. 本地构建

```bash
npm install
npm run tauri build    # 产出 src-tauri/target/release/bundle/<platform>/
```

Windows 本地构建产物示例：
`src-tauri/target/release/bundle/nsis/DSH 工作台_0.1.0_x64-setup.exe`（约 48MB）
