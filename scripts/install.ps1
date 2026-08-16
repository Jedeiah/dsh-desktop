# DSh Desktop 一键安装 / 升级脚本（Windows / PowerShell）
#
# 用法（PowerShell，自动安装/升级到最新正式版）：
#   powershell -ExecutionPolicy Bypass -Command "irm https://raw.githubusercontent.com/Jedeiah/dsh-desktop/main/scripts/install.ps1 | iex"
#
# 特性：
#   - 通过 GitHub /releases/latest 跳转解析最新 tag，不写死版本号
#   - 退出已运行的实例（只杀本 App 自己的进程，不动终端里手动跑的 dsh）
#   - 下载 NSIS 安装器 → 静默安装 → 自动启动
#   - 任何退出路径都清理临时文件
$ErrorActionPreference = "Stop"

$Repo = "Jedeiah/dsh-desktop"
$ExeName = "dsh-desktop.exe"
$ChildBin = "resources\dsh\current\node_modules\@deepseek-ai\dsh\lib\bin.js"
$ChildProxy = "resources\lan-proxy.js"

# --- 解析最新版本（走 github.com 跳转，绕开 api.github.com） ----------------
Write-Host "==> 查询最新版本（$Repo）..."
$Tmp = New-Item -ItemType Directory -Path (Join-Path $env:TEMP ("dsh-install-" + [guid]::NewGuid().ToString("N"))) -Force

try {
    $Resp = $null
    try {
        $Resp = Invoke-WebRequest -Uri "https://github.com/$Repo/releases/latest" -MaximumRedirection 5 -UseBasicParsing -TimeoutSec 25
    } catch {
        throw "无法访问 GitHub（可能是网络问题）：$_"
    }
    $Tag = $null
    # 兼容 Windows PowerShell 5.1（HttpWebResponse.ResponseUri）与 PowerShell 7
    # （HttpResponseMessage.RequestMessage.RequestUri）。
    if ($Resp.BaseResponse.ResponseUri) {
        $Tag = [System.Uri]::UnescapeDataString($Resp.BaseResponse.ResponseUri.AbsolutePath -split "/" | Select-Object -Last 1)
    } elseif ($Resp.BaseResponse.RequestMessage.RequestUri) {
        $Tag = [System.Uri]::UnescapeDataString($Resp.BaseResponse.RequestMessage.RequestUri.AbsolutePath -split "/" | Select-Object -Last 1)
    }
    if (-not $Tag -or -not $Tag.StartsWith("v")) {
        throw "未找到最新版本（可能尚无发布）"
    }
    $Version = $Tag.Substring(1)
    # release 工作流上传时把空格替换为点号 → 固定命名拼安装器地址（绕开 API）
    $SetupUrl = "https://github.com/$Repo/releases/download/$Tag/DeepSeek.Harness_${Version}_x64-setup.exe"
    Write-Host "==> 最新版本: $Tag"

    # --- 退出已运行的实例 ------------------------------------------------
    $procs = Get-Process -Name "dsh-desktop" -ErrorAction SilentlyContinue
    if ($procs) {
        Write-Host "==> 退出正在运行的 App..."
        Stop-Process -Name "dsh-desktop" -Force -ErrorAction SilentlyContinue
        Start-Sleep -Seconds 2
    }
    # 精确清掉 App 自己的 dsh/lan-proxy 子进程（不匹配用户手动跑的 dsh）
    Get-CimInstance Win32_Process -Filter "Name = 'node.exe'" -ErrorAction SilentlyContinue |
        Where-Object { $_.CommandLine -like "*$ChildBin*" -or $_.CommandLine -like "*$ChildProxy*" } |
        ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }

    # --- 下载安装器 ------------------------------------------------------
    $SetupPath = Join-Path $Tmp "setup.exe"
    Write-Host "==> 下载安装器..."
    Invoke-WebRequest -Uri $SetupUrl -OutFile $SetupPath -UseBasicParsing -TimeoutSec 600
    if (-not (Test-Path $SetupPath) -or (Get-Item $SetupPath).Length -eq 0) {
        throw "下载安装器失败"
    }

    # --- 静默安装（NSIS /S）并等待完成 -----------------------------------
    Write-Host "==> 安装中..."
    $p = Start-Process -FilePath $SetupPath -ArgumentList "/S" -PassThru -Wait

    # --- 启动 --------------------------------------------------------------
    $Installed = Get-ChildItem -Path @(
        (Join-Path $env:LOCALAPPDATA "Programs\DeepSeek Harness"),
        (Join-Path $env:ProgramFiles "DeepSeek Harness")
    ) -Filter $ExeName -Recurse -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($Installed) {
        Start-Process $Installed.FullName
        Write-Host "==> ✅ 安装完成（$Tag）"
    } else {
        Write-Host "==> 安装完成（$Tag），未找到已安装程序，请从开始菜单启动"
    }
} finally {
    Remove-Item -Recurse -Force $Tmp -ErrorAction SilentlyContinue
}
