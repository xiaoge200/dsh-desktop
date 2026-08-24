# prepare-resources.ps1 — 准备随包资源（Node 运行时 + dsh 基线）
#
# 用法：
#   pwsh scripts/prepare-resources.ps1          # 下载 Node 24.9.0 并按当前平台解包
#   pwsh scripts/prepare-resources.ps1 -NodeVer v24.9.0 -SkipBaseline
#   pwsh scripts/prepare-resources.ps1 -DshVer 0.1.1-rc.2   # 固定 dsh 基线版本（默认 latest）
#   pwsh scripts/prepare-resources.ps1 -NodeBase https://nodejs.org/dist   # Node 官方源（CI 用）
#
# 产物：
#   resources/node/<platform>/   内置 Node 发行版（含 npm）
#   resources/dsh-baseline/      dsh 完整依赖树（511 包，首启秒级就绪）
#
# 这些目录已被 .gitignore 忽略，CI 与本地构建各自生成。
#
# 说明：Node 二进制与 dsh 基线走不同源——
#   - Node：NodeBase（默认 npmmirror 的 /-/binary/node，国内快；CI 传 nodejs.org/dist）
#   - dsh 基线：Registry（默认 npmmirror，CI 传 registry.npmjs.org）

param(
  [string]$NodeVer = "v24.9.0",
  [switch]$SkipBaseline,
  [string]$Registry = "https://registry.npmmirror.com",
  [string]$DshVer = "latest",
  [string]$NodeBase = "https://registry.npmmirror.com/-/binary/node"
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
# $env:TEMP 是 Windows 专属变量，macOS/Linux 上为空；统一用系统临时目录
$tmpRoot = [System.IO.Path]::GetTempPath()

function Get-NodePlatformDir {
  if ($IsWindows) { return "win-x64" }
  # macOS/Linux 下 PROCESSOR_ARCHITECTURE 是 Windows 专属变量（恒为空），须用 uname 判断真实架构
  $arch = (& uname -m) 2>$null
  if (-not $arch) { $arch = $env:PROCESSOR_ARCHITECTURE }
  $isArm = $arch -match "arm|aarch64"
  if ($IsMacOS) {
    if ($isArm) { return "mac-arm64" }
    return "mac-x64"
  }
  if ($isArm) { return "linux-arm64" }
  return "linux-x64"
}

$plat = Get-NodePlatformDir
$nodeDir = Join-Path $root "resources\node\$plat"

Write-Host "==> Node: $NodeVer / $plat" -ForegroundColor Cyan

if (-not (Test-Path "$nodeDir\node$(if ($IsWindows) {'.exe'})")) {
  Write-Host "==> Downloading Node $NodeVer..." -ForegroundColor Cyan
  $base = "$NodeBase/$NodeVer"
  if ($IsWindows) {
    $zip = Join-Path $tmpRoot "node-$NodeVer-win-x64.zip"
    Invoke-WebRequest -Uri "$base/node-$NodeVer-win-x64.zip" -OutFile $zip -UseBasicParsing
    $extract = Join-Path $tmpRoot "node-$NodeVer-extract"
    if (Test-Path $extract) { Remove-Item $extract -Recurse -Force }
    Expand-Archive -Path $zip -DestinationPath $extract -Force
    New-Item -ItemType Directory -Force -Path $nodeDir | Out-Null
    $src = "$extract\node-$NodeVer-win-x64"
    Copy-Item "$src\node.exe" "$nodeDir\node.exe" -Force
    Copy-Item "$src\node_modules" "$nodeDir\node_modules" -Recurse -Force
    Copy-Item "$src\npm*" "$nodeDir\" -Recurse -Force
    Copy-Item "$src\npx*" "$nodeDir\" -Recurse -Force
  } else {
    # 发行包命名是 <distro>-<arch>（如 darwin-arm64 / linux-x64），
    # 与本地资源目录名（mac-arm64 / linux-x64）不同，须单独推导，不能直接复用 $plat
    $distro = if ($IsMacOS) { "darwin" } else { "linux" }
    $nodeArch = if ($plat -match "arm64") { "arm64" } else { "x64" }
    $tar = Join-Path $tmpRoot "node-$NodeVer-$distro-$nodeArch.tar.gz"
    Invoke-WebRequest -Uri "$base/node-$NodeVer-$distro-$nodeArch.tar.gz" -OutFile $tar -UseBasicParsing
    $extract = Join-Path $tmpRoot "node-$NodeVer-$distro-$nodeArch-extract"
    if (Test-Path $extract) { Remove-Item $extract -Recurse -Force }
    New-Item -ItemType Directory -Force -Path $extract | Out-Null
    tar -xzf $tar -C $extract
    New-Item -ItemType Directory -Force -Path $nodeDir | Out-Null
    $src = "$extract\node-$NodeVer-$distro-$nodeArch"
    Copy-Item "$src\bin\node" "$nodeDir\node" -Force
    Copy-Item "$src\lib" "$nodeDir\lib" -Recurse -Force
  }
  Write-Host "==> Node ready: $nodeDir" -ForegroundColor Green
} else {
  Write-Host "==> Node already present, skip" -ForegroundColor DarkGray
}

if (-not $SkipBaseline) {
  $baseline = Join-Path $root "resources\dsh-baseline"
  if (-not (Test-Path "$baseline\node_modules\@deepseek-ai\dsh\lib\bin.js")) {
    Write-Host "==> Building dsh baseline (npm install, ~几分钟)..." -ForegroundColor Cyan
    $work = Join-Path $tmpRoot "dsh-baseline-build"
    if (Test-Path $work) { Remove-Item $work -Recurse -Force }
    New-Item -ItemType Directory -Force -Path $work | Out-Null
    Push-Location $work
    # 大依赖树（511 包）在 CI 上会触发 npm 的 JavaScript 堆 OOM（默认约 2GB），放宽上限
    $env:NODE_OPTIONS = "--max-old-space-size=4096"
    npm install "@deepseek-ai/dsh@$DshVer" --no-audit --no-fund --registry $Registry --loglevel=error
    Pop-Location
    New-Item -ItemType Directory -Force -Path $baseline | Out-Null
    Move-Item "$work\node_modules" "$baseline\node_modules" -Force
    Remove-Item $work -Recurse -Force
    Write-Host "==> Baseline ready: $baseline" -ForegroundColor Green
  } else {
    Write-Host "==> Baseline already present, skip" -ForegroundColor DarkGray
  }
}

Write-Host "==> Done." -ForegroundColor Green
