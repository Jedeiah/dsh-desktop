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
# 关掉进度条渲染：Windows PowerShell 5.1 下 Invoke-WebRequest 的进度 UI 会显著拖慢
# 大文件下载（31MB 的 setup.exe 容易撞上硬超时）。
$ProgressPreference = 'SilentlyContinue'

$Repo = "Jedeiah/dsh-desktop"
$ExeName = "dsh-desktop.exe"
# App 自己 dsh 子进程的特征：闭包路径必须落在本 App 的 app-data 下。
# Windows 的 app-data 是 **%APPDATA%（Roaming）**\<id>（等于 Rust 侧
# tauri app_data_dir()/dirs::data_dir() 的语义），闭包在 <app-data>\dsh；
# 不是 %LOCALAPPDATA%（那里放的是 WebView2 数据）——旧写法用 LOCALAPPDATA 会让
# 这段过滤恒不命中、升级前的子进程清理静默失效。
# 另外只匹配 bin.js --profile web 会误杀用户终端里手动跑的 dsh。
$DshClosureDirs = @(
    (Join-Path $env:APPDATA "com.dsh-desktop.app\dsh"),
    (Join-Path $env:LOCALAPPDATA "com.dsh-desktop.app\dsh")
) | Where-Object { $_ }

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
        $Tag = [System.Uri]::UnescapeDataString(($Resp.BaseResponse.ResponseUri.AbsolutePath -split "/" | Select-Object -Last 1))
    } elseif ($Resp.BaseResponse.RequestMessage.RequestUri) {
        $Tag = [System.Uri]::UnescapeDataString(($Resp.BaseResponse.RequestMessage.RequestUri.AbsolutePath -split "/" | Select-Object -Last 1))
    }
    if (-not $Tag -or -not $Tag.StartsWith("v")) {
        throw "未找到最新版本（可能尚无发布）"
    }
    $Version = $Tag.Substring(1)
    # release 工作流上传时把空格替换为点号 → 固定命名拼安装器地址（绕开 API）
    $SetupUrl = "https://github.com/$Repo/releases/download/$Tag/DeepSeek.Harness.Desktop_${Version}_x64-setup.exe"
    Write-Host "==> 最新版本: $Tag"

    # --- 退出已运行的实例 ------------------------------------------------
    $procs = Get-Process -Name "dsh-desktop" -ErrorAction SilentlyContinue
    if ($procs) {
        Write-Host "==> 退出正在运行的 App..."
        Stop-Process -Name "dsh-desktop" -Force -ErrorAction SilentlyContinue
        Start-Sleep -Seconds 2
    }
    # 精确清掉 App 自己的 dsh 子进程（不匹配用户手动跑的 dsh）
    Get-CimInstance Win32_Process -Filter "Name = 'node.exe'" -ErrorAction SilentlyContinue |
        Where-Object {
            $cl = $_.CommandLine
            if (-not $cl -or $cl -notmatch 'bin\.js' -or $cl -notmatch '--profile') { return $false }
            # 用 Contains 而不是 -like：路径里的 [ ] 会被 -like 当字符类，导致恒不匹配
            foreach ($d in $DshClosureDirs) {
                if ($d -and $cl.ToLower().Contains($d.ToLower())) { return $true }
            }
            return $false
        } |
        ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }

    # --- 下载安装器 ------------------------------------------------------
    $SetupPath = Join-Path $Tmp "setup.exe"
    Write-Host "==> 下载安装器..."
    Invoke-WebRequest -Uri $SetupUrl -OutFile $SetupPath -UseBasicParsing -TimeoutSec 600
    if (-not (Test-Path $SetupPath) -or (Get-Item $SetupPath).Length -eq 0) {
        throw "下载安装器失败"
    }

    # 校验和：release 为每个产物同时发布 <asset>.sha256（App 内自动更新也校验它）。
    # 只取哈希、不比文件名（文件里记的是 CI 侧路径）。
    Write-Host "==> 校验下载完整性..."
    $Expect = $null
    $SumFile = Join-Path $Tmp "setup.exe.sha256"
    try {
        # 落盘再读：PowerShell 7 下 Invoke-WebRequest 的 .Content 对非文本 MIME
        # （.sha256 是 application/octet-stream）是空串，直接解析会得到空哈希 → 校验必失败。
        Invoke-WebRequest -Uri "$SetupUrl.sha256" -OutFile $SumFile -UseBasicParsing -TimeoutSec 30
        $Expect = ((Get-Content -LiteralPath $SumFile -Raw) -split "\s+")[0].Trim().ToLower()
    } catch {
        throw "无法获取校验和（$SetupUrl.sha256）：$_"
    }
    if ($Expect -notmatch '^[0-9a-f]{64}$') {
        throw "校验和格式异常（$SumFile）：$Expect"
    }
    $Actual = (Get-FileHash -LiteralPath $SetupPath -Algorithm SHA256).Hash.ToLower()
    if ($Expect -ne $Actual) {
        throw "校验失败：下载内容与发布校验和不一致（期望 $Expect，实际 $Actual）"
    }
    Write-Host "    OK sha256 一致"

    # --- 静默安装（NSIS /S）并等待完成 -----------------------------------
    Write-Host "==> 安装中..."
    $p = Start-Process -FilePath $SetupPath -ArgumentList "/S" -PassThru -Wait

    # --- 启动 --------------------------------------------------------------
    # NSIS installMode=currentUser 的安装目录是 $LOCALAPPDATA\<productName>
    # （tauri-bundler 模板 installer.nsi:514 `StrCpy $INSTDIR "$LOCALAPPDATA\${PRODUCTNAME}"`），
    # **不是** ...\Programs\...：旧探测的两个路径都猜错，装完永远走不进"自动启动"分支。
    $Installed = $null
    foreach ($c in @(
        (Join-Path $env:LOCALAPPDATA "DeepSeek Harness Desktop\$ExeName"),
        (Join-Path $env:LOCALAPPDATA "Programs\DeepSeek Harness Desktop\$ExeName"),
        (Join-Path $env:ProgramFiles "DeepSeek Harness Desktop\$ExeName")
    )) {
        if (Test-Path $c) { $Installed = Get-Item -LiteralPath $c; break }
    }
    if ($Installed) {
        Start-Process $Installed.FullName
        Write-Host "==> ✅ 安装完成（$Tag）"
    } else {
        Write-Host "==> 安装完成（$Tag），请从桌面/开始菜单启动"
    }
} finally {
    Remove-Item -Recurse -Force $Tmp -ErrorAction SilentlyContinue
}
