# prepare-resources.ps1 — 准备随包资源（Node 运行时 + dsh 基线）
#
# 用法：
#   pwsh scripts/prepare-resources.ps1          # 下载 Node 24.9.0 并按当前平台解包
#   pwsh scripts/prepare-resources.ps1 -NodeVer v24.9.0 -SkipBaseline
#   pwsh scripts/prepare-resources.ps1 -DshVer 0.1.1-rc.2   # 固定 dsh 基线版本（默认 latest）
#
# 产物：
#   resources/node/<platform>/   内置 Node 发行版（含 npm）
#   resources/dsh-baseline/      dsh 完整依赖树（511 包，首启秒级就绪）
#
# 这些目录已被 .gitignore 忽略，CI 与本地构建各自生成。

param(
  [string]$NodeVer = "v24.9.0",
  [switch]$SkipBaseline,
  [string]$Registry = "https://registry.npmmirror.com",
  [string]$DshVer = "latest"
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot

function Get-NodePlatformDir {
  if ($IsWindows) { return "win-x64" }
  # macOS/Linux 下 PROCESSOR_ARCHITECTURE 是 Windows 专属变量（恒为空），须用 uname 判断真实架构
  $arch = (& uname -m) 2>$null
  if (-not $arch) { $arch = $env:PROCESSOR_ARCHITECTURE }
  $isArm = $arch -match "arm|aarch64"
  if ($IsMacOS) { return if ($isArm) { "mac-arm64" } else { "mac-x64" } }
  return if ($isArm) { "linux-arm64" } else { "linux-x64" }
}

$plat = Get-NodePlatformDir
$nodeDir = Join-Path $root "resources\node\$plat"

Write-Host "==> Node: $NodeVer / $plat" -ForegroundColor Cyan

if (-not (Test-Path "$nodeDir\node$(if ($IsWindows) {'.exe'})")) {
  Write-Host "==> Downloading Node $NodeVer..." -ForegroundColor Cyan
  $base = "$Registry/-/binary/node/$NodeVer"
  if ($IsWindows) {
    $zip = "$env:TEMP\node-$NodeVer-win-x64.zip"
    Invoke-WebRequest -Uri "$base/node-$NodeVer-win-x64.zip" -OutFile $zip -UseBasicParsing
    $extract = "$env:TEMP\node-$NodeVer-extract"
    if (Test-Path $extract) { Remove-Item $extract -Recurse -Force }
    Expand-Archive -Path $zip -DestinationPath $extract -Force
    New-Item -ItemType Directory -Force -Path $nodeDir | Out-Null
    $src = "$extract\node-$NodeVer-win-x64"
    Copy-Item "$src\node.exe" "$nodeDir\node.exe" -Force
    Copy-Item "$src\node_modules" "$nodeDir\node_modules" -Recurse -Force
    Copy-Item "$src\npm*" "$nodeDir\" -Recurse -Force
    Copy-Item "$src\npx*" "$nodeDir\" -Recurse -Force
  } else {
    $tar = "$env:TEMP\node-$NodeVer-$plat.tar.gz"
    $distro = if ($IsMacOS) { "darwin" } else { "linux" }
    Invoke-WebRequest -Uri "$base/node-$NodeVer-$distro-$plat.tar.gz" -OutFile $tar -UseBasicParsing
    $extract = "$env:TEMP\node-$NodeVer-$plat-extract"
    if (Test-Path $extract) { Remove-Item $extract -Recurse -Force }
    New-Item -ItemType Directory -Force -Path $extract | Out-Null
    tar -xzf $tar -C $extract
    New-Item -ItemType Directory -Force -Path $nodeDir | Out-Null
    $src = "$extract\node-$NodeVer-$distro-$plat"
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
    $work = "$env:TEMP\dsh-baseline-build"
    if (Test-Path $work) { Remove-Item $work -Recurse -Force }
    New-Item -ItemType Directory -Force -Path $work | Out-Null
    Push-Location $work
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
