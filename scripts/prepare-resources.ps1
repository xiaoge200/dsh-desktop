











param(
  [string]$NodeVer = "v24.9.0",
  [switch]$SkipBaseline,
  [string]$Registry = "https://registry.npmmirror.com",
  [string]$DshVer = "latest",
  [string]$NodeBase = "https://registry.npmmirror.com/-/binary/node"
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot

$tmpRoot = [System.IO.Path]::GetTempPath()

function Get-NodePlatformDir {
  if ($IsWindows) { return "win-x64" }
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

function Get-LinuxArchitecture {
  $arch = (& uname -m) 2>$null
  if (-not $arch) { throw "Unable to determine Linux architecture." }
  if ($arch -match "aarch64|arm64") { return "arm64" }
  if ($arch -match "x86_64|amd64") { return "x64" }
  throw "Unsupported Linux architecture: $arch"
}

function Remove-IfExists {
  param(
    [Parameter(Mandatory = $true)]
    [string]$Path
  )
  if (Test-Path $Path) {
    Write-Host "Removing incompatible native module: $Path" -ForegroundColor Yellow
    Remove-Item $Path -Recurse -Force
  }
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
    # Node 官方发行包：darwin-arm64 / darwin-x64 / linux-arm64 / linux-x64
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

# ============================================================
# dsh baseline
# ============================================================
if (-not $SkipBaseline) {
  $baseline = Join-Path $root "resources\dsh-baseline"

  if (-not (Test-Path "$baseline\node_modules\@deepseek-ai\dsh\lib\bin.js")) {
    Write-Host "==> Building dsh baseline (npm install, ~几分钟)..." -ForegroundColor Cyan

    $work = Join-Path $tmpRoot "dsh-baseline-build"
    if (Test-Path $work) { Remove-Item $work -Recurse -Force }
    New-Item -ItemType Directory -Force -Path $work | Out-Null

    Push-Location $work
    try {
      # 大依赖树在 CI 上可能触发 npm JavaScript heap OOM
      $env:NODE_OPTIONS = "--max-old-space-size=4096"
      npm install "@deepseek-ai/dsh@$DshVer" --no-audit --no-fund --registry $Registry --loglevel=error
    } finally {
      Pop-Location
    }

    New-Item -ItemType Directory -Force -Path $baseline | Out-Null
    Move-Item "$work\node_modules" "$baseline\node_modules" -Force
    Remove-Item $work -Recurse -Force

    Write-Host "==> Baseline ready: $baseline" -ForegroundColor Green
  } else {
    Write-Host "==> Baseline already present, skip" -ForegroundColor DarkGray
  }

  # ==========================================================
  # Linux native module cleanup
  #
  # npm 会安装 optionalDependencies / prebuilds，可能把 x64、arm64、musl 等
  # 多个平台的 native binary 同时放进 node_modules。linuxdeploy 会扫描 AppDir
  # 中所有 ELF 文件，因此无关架构/ABI 的文件必须在打包前移除。
  # ==========================================================
  if ($IsLinux) {
    $linuxArch = Get-LinuxArchitecture
    Write-Host ""
    Write-Host "==> Cleaning Linux native modules for $linuxArch..." -ForegroundColor Cyan

    $nm = Join-Path $baseline "node_modules"
    if (-not (Test-Path $nm)) { throw "dsh baseline node_modules not found: $nm" }

    if ($linuxArch -eq "x64") {
      Write-Host "==> Target: Linux x86_64 / glibc" -ForegroundColor Cyan

      # node-pty
      Remove-IfExists (Join-Path $nm "node-pty\prebuilds\linux-arm64")

      # sharp
      Remove-IfExists (Join-Path $nm "@img\sharp-linux-arm64")
      Remove-IfExists (Join-Path $nm "@img\sharp-libvips-linux-arm64")

      # koffi：保留 glibc x64，删除 ARM64 和 musl
      Remove-IfExists (Join-Path $nm "@koromix\koffi-linux-arm64")
      Remove-IfExists (Join-Path $nm "@koromix\koffi-linux-x64\musl_x64")

      # ripgrep ARM64
      Remove-IfExists (Join-Path $nm "@vscode\ripgrep-linux-arm64")

      # ARM64 landlock-run
      Remove-IfExists (Join-Path $nm "@deepseek-ai\node-addon-landlock-run-linux-arm64")

      # x64 landlock-run 是静态 ELF，linuxdeploy patchelf 会报
      # 'cannot find section .dynamic'，先移除避免阻塞 AppImage 构建
      Remove-IfExists (Join-Path $nm "@deepseek-ai\node-addon-landlock-run-linux-x64")
    } elseif ($linuxArch -eq "arm64") {
      Write-Host "==> Target: Linux ARM64 / glibc" -ForegroundColor Cyan

      # node-pty
      Remove-IfExists (Join-Path $nm "node-pty\prebuilds\linux-x64")

      # sharp
      Remove-IfExists (Join-Path $nm "@img\sharp-linux-x64")
      Remove-IfExists (Join-Path $nm "@img\sharp-libvips-linux-x64")

      # koffi：保留 glibc ARM64，删除 x64 和 musl
      Remove-IfExists (Join-Path $nm "@koromix\koffi-linux-x64")
      Remove-IfExists (Join-Path $nm "@koromix\koffi-linux-arm64\musl_arm64")

      # ripgrep x64
      Remove-IfExists (Join-Path $nm "@vscode\ripgrep-linux-x64")

      # x64 landlock-run
      Remove-IfExists (Join-Path $nm "@deepseek-ai\node-addon-landlock-run-linux-x64")

      # ARM64 landlock-run 是静态 ELF，linuxdeploy patchelf 会报
      # 'cannot find section .dynamic'
      Remove-IfExists (Join-Path $nm "@deepseek-ai\node-addon-landlock-run-linux-arm64")
    }

    Write-Host "==> Linux native module cleanup complete." -ForegroundColor Green
  }
}

Write-Host "==> Done." -ForegroundColor Green
