param(
  [string]$NodeVer = "v24.9.0",
  [switch]$SkipBaseline,
  [switch]$SkipPnpm,
  [string]$Registry = "https://registry.npmmirror.com",
  [string]$DshVer = "latest",
  [string]$PnpmVer = "11.7.0",
  [string]$NodeBase = "https://registry.npmmirror.com/-/binary/node",
  [string]$NodePlat = ""
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot

$tmpRoot = [System.IO.Path]::GetTempPath()

$platInfo = @{
  "win-x64"     = @{ distro = "win";    npmOs = "win32";  arch = "x64" }
  "mac-arm64"   = @{ distro = "darwin"; npmOs = "darwin"; arch = "arm64" }
  "mac-x64"     = @{ distro = "darwin"; npmOs = "darwin"; arch = "x64" }
  "linux-arm64" = @{ distro = "linux";  npmOs = "linux";  arch = "arm64" }
  "linux-x64"   = @{ distro = "linux";  npmOs = "linux";  arch = "x64" }
}

function Get-NodePlatformDir {
  if ($env:OS -eq "Windows_NT") { return "win-x64" }
  $sys = ""
  try { $sys = "$(& uname -s 2>$null)".Trim() } catch { $sys = "" }
  if (-not $sys) {
    if ($IsMacOS) { $sys = "Darwin" } elseif ($IsLinux) { $sys = "Linux" }
  }
  $machine = ""
  try { $machine = "$(& uname -m 2>$null)".Trim() } catch { $machine = "" }
  if (-not $machine) { $machine = $env:PROCESSOR_ARCHITECTURE }
  $isArm = $machine -match "arm|aarch64"
  if ($sys -match "Darwin") { if ($isArm) { return "mac-arm64" } return "mac-x64" }
  if ($isArm) { return "linux-arm64" }
  return "linux-x64"
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

$plat = if ("$NodePlat".Trim() -ne "") { "$NodePlat".Trim() } else { Get-NodePlatformDir }
if (-not $platInfo.ContainsKey($plat)) {
  throw "Unsupported -NodePlat '$plat' (expected one of: $($platInfo.Keys -join ', '))"
}
$distro = $platInfo[$plat].distro
$npmOs = $platInfo[$plat].npmOs
$arch = $platInfo[$plat].arch
$nodeIsWindows = $plat -eq "win-x64"
$nodeExeName = if ($nodeIsWindows) { "node.exe" } else { "node" }

$nodeDir = Join-Path $root "resources\node\$plat"

Write-Host "==> Node: $NodeVer / $plat" -ForegroundColor Cyan

if (-not (Test-Path (Join-Path $nodeDir $nodeExeName))) {
  Write-Host "==> Downloading Node $NodeVer ($distro-$arch)..." -ForegroundColor Cyan
  $base = "$NodeBase/$NodeVer"

  if ($nodeIsWindows) {
    $zip = Join-Path $tmpRoot "node-$NodeVer-$distro-$arch.zip"
    Invoke-WebRequest -Uri "$base/node-$NodeVer-$distro-$arch.zip" -OutFile $zip -UseBasicParsing

    $extract = Join-Path $tmpRoot "node-$NodeVer-$distro-$arch-extract"
    if (Test-Path $extract) { Remove-Item $extract -Recurse -Force }
    Expand-Archive -Path $zip -DestinationPath $extract -Force

    New-Item -ItemType Directory -Force -Path $nodeDir | Out-Null
    $src = "$extract\node-$NodeVer-$distro-$arch"
    Copy-Item "$src\node.exe" "$nodeDir\node.exe" -Force
    Copy-Item "$src\node_modules" "$nodeDir\node_modules" -Recurse -Force
    Copy-Item "$src\npm*" "$nodeDir\" -Recurse -Force
    Copy-Item "$src\npx*" "$nodeDir\" -Recurse -Force
  } else {
    # Node 官方发行包：darwin-arm64 / darwin-x64 / linux-arm64 / linux-x64
    $tar = Join-Path $tmpRoot "node-$NodeVer-$distro-$arch.tar.gz"
    Invoke-WebRequest -Uri "$base/node-$NodeVer-$distro-$arch.tar.gz" -OutFile $tar -UseBasicParsing

    $extract = Join-Path $tmpRoot "node-$NodeVer-$distro-$arch-extract"
    if (Test-Path $extract) { Remove-Item $extract -Recurse -Force }
    New-Item -ItemType Directory -Force -Path $extract | Out-Null
    tar -xzf $tar -C $extract

    New-Item -ItemType Directory -Force -Path $nodeDir | Out-Null
    $src = "$extract\node-$NodeVer-$distro-$arch"
    Copy-Item "$src\bin\node" "$nodeDir\node" -Force
    Copy-Item "$src\lib" "$nodeDir\lib" -Recurse -Force
  }

  Write-Host "==> Node ready: $nodeDir" -ForegroundColor Green
} else {
  Write-Host "==> Node already present, skip" -ForegroundColor DarkGray
}

if ($SkipPnpm) {
  Write-Host "==> pnpm bundling skipped" -ForegroundColor DarkGray
} else {
  $pnpmDir = Join-Path $root "resources\pnpm"
  $pnpmEntry = Join-Path $pnpmDir "bin\pnpm.mjs"

  if (-not (Test-Path $pnpmEntry)) {
    Write-Host "==> Bundling pnpm $PnpmVer..." -ForegroundColor Cyan

    $work = Join-Path $tmpRoot "pnpm-bundle-build"
    if (Test-Path $work) { Remove-Item $work -Recurse -Force }
    New-Item -ItemType Directory -Force -Path $work | Out-Null

    npm install "pnpm@$PnpmVer" --prefix $work --no-audit --no-fund --no-save --registry $Registry --loglevel=error

    $src = Join-Path $work "node_modules\pnpm"
    if (-not (Test-Path (Join-Path $src "bin\pnpm.mjs"))) {
      throw "pnpm package layout unexpected (bin/pnpm.mjs missing): $src"
    }

    if (Test-Path $pnpmDir) { Remove-Item $pnpmDir -Recurse -Force }
    Move-Item $src $pnpmDir -Force
    Remove-Item $work -Recurse -Force

    Write-Host "==> pnpm ready: $pnpmDir" -ForegroundColor Green
  } else {
    Write-Host "==> pnpm already present, skip" -ForegroundColor DarkGray
  }

  $nodeBin = Join-Path $nodeDir $nodeExeName
  if (Test-Path $nodeBin) {
    $nodeVer = ""
    $nodeRuns = $false
    try {
      $nodeVer = "$(& $nodeBin --version 2>&1 | Select-Object -First 1)".Trim()
      $nodeRuns = $LASTEXITCODE -eq 0
    } catch {
      $nodeVer = $_.Exception.Message
    }
    if (-not $nodeRuns) {
      Write-Host "==> pnpm smoke skipped: bundled node ($plat) does not run on this host: $nodeVer" -ForegroundColor Yellow
    } else {
      $pnpmVer = & $nodeBin $pnpmEntry --version 2>&1
      if ($LASTEXITCODE -ne 0) {
        throw "bundled pnpm failed to run: $pnpmVer"
      }
      Write-Host "==> pnpm smoke ok: pnpm $pnpmVer on node $nodeVer" -ForegroundColor Green
    }
  }
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
      npm install "@deepseek-ai/dsh@$DshVer" --os $npmOs --cpu $arch --no-audit --no-fund --registry $Registry --loglevel=error
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

  if ($plat -like "linux-*") {
    $linuxArch = $arch
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
