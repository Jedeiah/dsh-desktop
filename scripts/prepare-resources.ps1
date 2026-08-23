# Prepare bundled resources for DSh Desktop (Windows build).
# Mirrors scripts/prepare-resources.sh for PowerShell:
#   resources/node/node.exe      — Node x64 runtime (node.exe)
#   resources/npm, pnpm-bin      — bundled npm/pnpm for in-app dsh install / plugin mgmt
#
# Build-time script (runs on Windows CI); the bundled result is what matters at
# runtime (no system node/npm needed there).
#
# Usage:
#   .\scripts\prepare-resources.ps1 -NodeSrc (Get-Command node).Source
param(
    [string]$NodeSrc = ""
)
$ErrorActionPreference = "Stop"

# 大闭包用 npm 安装（CI 上 npm/arborist 易触发 V8 默认堆上限 OOM）→ 抬高堆上限。
# 本脚本内的 npm install 都走这个 NODE_OPTIONS。
$env:NODE_OPTIONS = "$env:NODE_OPTIONS --max-old-space-size=6144"

$ROOT = Split-Path -Parent $PSScriptRoot
$SRC_TAURI = Join-Path $ROOT "apps\desktop\src-tauri"
$RES = Join-Path $SRC_TAURI "resources"

Write-Host "==> Preparing bundled resources (node + npm + pnpm)"

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

# --- 1b. npm (in-app updates: dsh.rs installs new dsh closure via bundled npm)
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

# --- 1c. pnpm (plugin management; JS distribution + shim, zero runtime net) ---
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
# --- 2. icons ----------------------------------------------------------------
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
