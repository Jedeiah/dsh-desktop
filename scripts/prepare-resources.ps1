# Prepare bundled resources for DSh Desktop (Windows build).
# Mirrors scripts/prepare-resources.sh for PowerShell:
#   resources/node/node.exe      — Node x64 runtime (node.exe)
#   resources/dsh/<ver>/         — full dsh closure (node_modules incl. @deepseek-ai/dsh)
#   resources/dsh/current        — version-marker file (content = version name)
#   resources/lan-proxy.js       — LAN forwarder
#
# Build-time script (runs on Windows CI); the bundled result is what matters at
# runtime (no system node/npm needed there).
#
# Usage:
#   .\scripts\prepare-resources.ps1 -DshVersion 0.1.0-rc.6 `
#       -NodeSrc (Get-Command node).Source `
#       -ClosureSrc "$env:RUNNER_TEMP\closure\node_modules"
param(
    [string]$DshVersion = "0.1.0-rc.6",
    [string]$NodeSrc = "",
    [string]$ClosureSrc = ""
)
$ErrorActionPreference = "Stop"

# 大闭包用 npm 安装（CI 上 npm/arborist 易触发 V8 默认堆上限 OOM）→ 抬高堆上限。
# 本脚本内的 npm install 都走这个 NODE_OPTIONS。
$env:NODE_OPTIONS = "$env:NODE_OPTIONS --max-old-space-size=6144"

$ROOT = Split-Path -Parent $PSScriptRoot
$SRC_TAURI = Join-Path $ROOT "apps\desktop\src-tauri"
$RES = Join-Path $SRC_TAURI "resources"

Write-Host "==> Preparing resources for dsh closure $DshVersion"

# --- 1. Node runtime ---------------------------------------------------------
if (-not $NodeSrc) { $NodeSrc = (Get-Command node -ErrorAction SilentlyContinue).Source }
if (-not $NodeSrc -or -not (Test-Path $NodeSrc)) {
    throw "node source not found; set -NodeSrc to a node.exe path"
}
$NodeDir = Join-Path $RES "node"
New-Item -ItemType Directory -Force -Path $NodeDir | Out-Null
Copy-Item -Force $NodeSrc (Join-Path $NodeDir "node.exe")
$nodeVer = & (Join-Path $NodeDir "node.exe") --version
Write-Host "    node: $nodeVer"

# --- 1b. npm (in-app updates: update.rs installs new dsh closure via bundled npm)
# Windows node ships npm at <node.exe dir>\node_modules\npm (no lib/);
# source is the *origin* node dir, not the staged $RES\node.
$NpmSrc = Join-Path (Split-Path -Parent $NodeSrc) "node_modules\npm"
if (-not (Test-Path (Join-Path $NpmSrc "bin\npm-cli.js"))) {
    throw "built-in npm not found at $NpmSrc; set NpmSrc to the npm directory"
}
$NpmDir = Join-Path $RES "npm"
if (Test-Path $NpmDir) { Remove-Item -Recurse -Force $NpmDir }
Copy-Item -Recurse -Force $NpmSrc $NpmDir
$npmCli = Join-Path $NpmDir "bin\npm-cli.js"
Write-Host "    npm: $((Get-Item $npmCli).Length) B npm-cli.js"

# --- 2. dsh closure ---------------------------------------------------------
if (-not $ClosureSrc) {
    $candidate = Join-Path $RES "dsh\current\node_modules"
    if (Test-Path (Join-Path $candidate "@deepseek-ai\dsh")) { $ClosureSrc = $candidate }
}
if (-not (Test-Path (Join-Path $ClosureSrc "@deepseek-ai\dsh"))) {
    throw "closure source missing @deepseek-ai/dsh at $ClosureSrc; set -ClosureSrc"
}

$DST = Join-Path $RES "dsh\$DshVersion"
if (Test-Path $DST) { Remove-Item -Recurse -Force $DST }
New-Item -ItemType Directory -Force -Path $DST | Out-Null
Copy-Item -Recurse -Force $ClosureSrc (Join-Path $DST "node_modules")
$pkg = @{ name = "dsh-closure"; version = $DshVersion } | ConvertTo-Json -Compress
Set-Content -Path (Join-Path $DST "package.json") -Value $pkg -Encoding ascii
Set-Content -Path (Join-Path $DST "VERSION") -Value $DshVersion -Encoding ascii
# `current` 是版本标记文件（内容=版本号），跨平台（无需软链权限）。
Set-Content -Path (Join-Path $RES "dsh\current") -Value $DshVersion -Encoding ascii
Remove-Item -ErrorAction SilentlyContinue (Join-Path $RES "dsh\current.tmp")

# --- 2b. LAN proxy ----------------------------------------------------------
Copy-Item -Force (Join-Path $SRC_TAURI "lan-proxy.js") (Join-Path $RES "lan-proxy.js")
# --- 2b2. mDNS 通告器（壳层增强②，Windows 用）-----------------------------
Copy-Item -Force (Join-Path $SRC_TAURI "mdns-advertise.js") (Join-Path $RES "mdns-advertise.js")
# --- 2b2b. QR code generator lib (login page scan-to-connect, MIT single file) --
Copy-Item -Force (Join-Path $SRC_TAURI "qrcode.js") (Join-Path $RES "qrcode.js")
# 无网络自测：验证 mDNS 报文逻辑（打包前兜底，失败即中止）
$mdnsCheck = & (Join-Path $NodeDir "node.exe") (Join-Path $RES "mdns-advertise.js") --self-test 2>&1
if ($LASTEXITCODE -ne 0) {
    throw "mdns-advertise self-test failed: $mdnsCheck"
}
Write-Host "    mdns-advertise.js self-test OK"

# --- 2b3. pnpm (plugin management; JS distribution + shim, zero runtime net) ---
# dsh plugin spawns "pnpm" from PATH; bundle pnpm so users never install it.
# Strategy: install pnpm's JS distribution (pnpm@^11, self-contained dist
# bundle), copy bin/pnpm.mjs + dist/ + package.json into resources/pnpm-bin,
# and generate a `pnpm.cmd` shim that runs the bundled node by relative path
# (path-stable after packaging). Pin ^11 (pnpm 11 gate semantics: allowBuilds /
# minimumReleaseAge; pnpm 12 is a Rust rewrite).
$PnpmStage = Join-Path $env:TEMP ("pnpm-stage-" + [guid]::NewGuid().ToString("N"))
try {
    npm install --prefix $PnpmStage "pnpm@^11" --ignore-scripts --no-audit --no-fund
    if ($LASTEXITCODE -ne 0) { throw "pnpm install failed (see npm output above)" }
    $Pkg = Join-Path $PnpmStage "node_modules\pnpm"
    if (-not (Test-Path (Join-Path $Pkg "bin\pnpm.mjs")) -or -not (Test-Path (Join-Path $Pkg "dist"))) {
        throw "pnpm package structure unexpected ($Pkg)"
    }
    $PnpmBin = Join-Path $RES "pnpm-bin"
    New-Item -ItemType Directory -Force -Path $PnpmBin | Out-Null
    Copy-Item -Recurse -Force (Join-Path $Pkg "bin") (Join-Path $PnpmBin "bin")
    Copy-Item -Recurse -Force (Join-Path $Pkg "dist") (Join-Path $PnpmBin "dist")
    Copy-Item -Force (Join-Path $Pkg "package.json") (Join-Path $PnpmBin "package.json")
    # pnpm shim: bundled node by relative path (resources/pnpm-bin -> resources/node)
    $shim = "@echo off`r`n`"%~dp0..\node\node.exe`" `"%~dp0bin\pnpm.mjs`" %*`r`n"
    Set-Content -Path (Join-Path $PnpmBin "pnpm.cmd") -Value $shim -Encoding ascii
    $pnpmVer = ((& (Join-Path $PnpmBin "pnpm.cmd") --version 2>$null) | Out-String).Trim()
    if (-not $pnpmVer) { throw "bundled pnpm failed verification (pnpm.cmd --version)" }
    Write-Host "    pnpm: $pnpmVer"
} finally {
    if (Test-Path $PnpmStage) { Remove-Item -Recurse -Force $PnpmStage }
}
# --- 2c. closure self-check (must boot under the bundled node) --------------
$node = Join-Path $NodeDir "node.exe"
$bin = Join-Path $DST "node_modules\@deepseek-ai\dsh\lib\bin.js"
$detected = ((& $node $bin --version 2>$null) | Out-String).Trim()
if ($detected -ne $DshVersion) {
    throw "ERROR: bundled closure does not boot (expected $DshVersion, got '$detected')"
}
Write-Host "    closure self-check OK: dsh $detected"

# --- 3. icons ----------------------------------------------------------------
$Icons = Join-Path $SRC_TAURI "icons"
if (-not (Test-Path (Join-Path $Icons "icon.ico"))) {
    throw "missing icons/icon.ico (commit it; Windows builds require an .ico)"
}
if (-not (Test-Path (Join-Path $Icons "icon.png"))) {
    throw "missing icons/icon.png (needed by the splash page)"
}
Write-Host "    icons: icon.png + icon.ico present"

Write-Host "==> Done. resources:"
$size = (Get-ChildItem -Recurse -File $RES | Measure-Object Length -Sum).Sum
Write-Host ("    {0:N1} MB" -f ($size / 1MB))
