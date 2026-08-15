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
