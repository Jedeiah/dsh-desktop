//! App 自身更新：GitHub Releases 检查 + 下载安装（macOS DMG / Windows NSIS）。
//! 失败安全：下载/安装任何一步失败都不影响当前运行版本。

use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tauri::Emitter; // Tauri 2：emit 定义在 Emitter trait 上

/// App 更新任务是否在进行中（下载/校验/安装）。
/// UI 侧会禁用相关按钮，但正确性靠这里：并发调用会下载两份安装包、拉起两次安装器。
static UPDATING: AtomicBool = AtomicBool::new(false);

const REPO: &str = "Jedeiah/dsh-desktop";

pub fn parse_tag_from_effective_url(final_url: &str) -> Option<String> {
    let idx = final_url.find("/releases/tag/")?;
    let tag = &final_url[idx + "/releases/tag/".len()..];
    let tag = tag.split(['?', '#']).next().unwrap_or(tag);
    let ver = tag.strip_prefix('v')?;
    // 版本号来自远端（GitHub tag 名），会拼进下载 URL 与本地临时文件名：
    // 只接受 `[0-9A-Za-z.+-]`，其余一律判为无效。否则 `1.0/../../x` 这类 tag 能让
    // `temp_dir().join("dsh-desktop-update-{ver}.dmg")` 写到临时目录之外。
    if ver.is_empty()
        || ver.len() > 64
        || !ver
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+'))
    {
        return None;
    }
    Some(ver.to_string())
}

/// Download URL for the current platform's installer (naming mirrors
/// release.yml + scripts/install.sh: GitHub replaces spaces with dots).
/// macOS CI 仅构建 arm64（macos-14 runner）——x86_64 无对应产物，返回 None，
/// 用户走「关于页手动下载」兜底（规格 5.3）。
pub fn asset_url(ver: &str) -> Option<String> {
    let base = format!("https://github.com/{REPO}/releases/download/v{ver}");
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    let name = format!("DeepSeek.Harness.Desktop_{ver}_aarch64.dmg");
    #[cfg(target_os = "windows")]
    let name = format!("DeepSeek.Harness.Desktop_{ver}_x64-setup.exe");
    #[cfg(not(any(
        all(target_os = "macos", target_arch = "aarch64"),
        target_os = "windows"
    )))]
    let name = return None;
    Some(format!("{base}/{name}"))
}

/// Resolve the newest release tag via the `/releases/latest` redirect.
pub fn latest_app_version() -> Result<String, String> {
    let url = format!("https://github.com/{REPO}/releases/latest");
    let resp = ureq::get(&url)
        .timeout(Duration::from_secs(20))
        .call()
        .map_err(|e| format!("查询 {url} 失败: {e}"))?;
    let final_url = resp.get_url().to_string();
    parse_tag_from_effective_url(&final_url)
        .ok_or_else(|| format!("无法从响应 URL 解析版本：{final_url}"))
}

/// SHA-256 校验和（十六进制小写）——发布流程在 release.yml 为每个安装包生成
/// `<asset>.sha256`（内容为该文件的 SHA-256）。这里下载资产后比对，防止 release
/// 资产被替换导致执行任意代码（安全审查 should-fix）。
fn sha256_of_file(path: &Path) -> std::io::Result<String> {
    use sha2::{Digest, Sha256};
    let mut f = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut f, &mut hasher)?;
    let out = hasher.finalize();
    Ok(out.iter().map(|b| format!("{b:02x}")).collect())
}

/// Download `<url>.sha256`（发布流程生成）；解析首个 64 位十六进制为预期 SHA。
/// 匹配 release.yml 生成的 `shasum -a 256` 输出格式（"<hex>  <file>"）。
fn expected_sha256(url: &str) -> Result<String, String> {
    let sha_url = format!("{url}.sha256");
    let body = ureq::get(&sha_url)
        .timeout(Duration::from_secs(30))
        .call()
        .map_err(|e| format!("下载校验和 {sha_url} 失败: {e}"))?
        .into_string()
        .map_err(|e| format!("读取校验和失败: {e}"))?;
    body.split_whitespace()
        .next()
        .filter(|h| h.len() == 64 && h.chars().all(|c| c.is_ascii_hexdigit()))
        .map(|h| h.to_ascii_lowercase())
        .ok_or_else(|| format!("校验和文件格式异常: {body:?}"))
}

/// Stream-download `url` to `dest`; verifies size against Content-Length when
/// the server provides it (规格 5.3：校验大小与 asset 一致；缺失/不符即失败清理，
/// 杜绝静默截断)。大文件下载用 30 分钟总超时——ureq 的 timeout 覆盖整个请求，
/// 数十秒的默认值必然中断数百 MB 的安装包下载。
/// 下载完成后比对 release 提供的 SHA-256（安全加固），不符则删除并失败。
pub fn download_installer(url: &str, dest: &Path, app: &tauri::AppHandle) -> Result<u64, String> {
    let resp = ureq::get(url)
        .timeout(Duration::from_secs(1800))
        .call()
        .map_err(|e| format!("下载失败: {e}"))?;
    let expected = resp
        .header("Content-Length")
        .and_then(|s| s.parse::<u64>().ok());
    let mut reader = resp.into_reader().take(2 << 30);
    let mut f = std::fs::File::create(dest).map_err(|e| format!("创建临时文件失败: {e}"))?;
    // 手动分块拷贝（原 std::io::copy 无法统计进度）：统计字节数 → 只在**整数百分比
    // 变化**时向壳页发一次进度事件（每 64KB 发一次会把 IPC 打满）。
    let mut buf = vec![0u8; 64 * 1024];
    let mut n: u64 = 0;
    // 初值取 i64::MIN：否则"无 Content-Length（pct 恒为 -1）"时首条事件就不发，
    // 壳页会一直停在「准备下载…」。
    let mut last_pct: i64 = i64::MIN;
    let mut last_emit: u64 = 0;
    loop {
        let read = reader.read(&mut buf).map_err(|e| format!("读取失败: {e}"))?;
        if read == 0 {
            break;
        }
        f.write_all(&buf[..read]).map_err(|e| format!("写入失败: {e}"))?;
        n += read as u64;
        let pct = match expected {
            Some(t) if t > 0 => (n.saturating_mul(100) / t) as i64,
            _ => -1, // 服务端没给长度：百分比未知，壳页按"不确定"展示
        };
        // 有总长：按整数百分比变化发；没有总长：每 4MB 发一次（否则整段下载只有一条事件）
        if pct != last_pct || n - last_emit >= 4 * 1024 * 1024 {
            last_pct = pct;
            last_emit = n;
            let _ = app.emit(
                "app:update-progress",
                serde_json::json!({ "phase": "download", "downloaded": n, "total": expected }),
            );
        }
    }
    drop(f);
    if let Some(exp) = expected {
        if n != exp {
            let _ = std::fs::remove_file(dest);
            return Err(format!("下载不完整: 期望 {exp} 字节, 实际 {n}"));
        }
    }
    // 进入校验/安装阶段：没有可用的百分比，告知壳页切成"不确定"文案
    let _ = app.emit(
        "app:update-progress",
        serde_json::json!({ "phase": "install", "downloaded": n, "total": expected }),
    );
    // SHA-256 校验（发布流程生成 `<asset>.sha256`）
    let got = sha256_of_file(dest).map_err(|e| format!("计算安装包 SHA-256 失败: {e}"))?;
    let want = expected_sha256(url)?;
    if got != want {
        let _ = std::fs::remove_file(dest);
        return Err(format!("安装包校验失败: SHA-256 不符(期望 {want}, 实际 {got})"));
    }
    Ok(n)
}

#[tauri::command]
pub async fn check_app_update_cmd() -> Option<String> {
    // 网络检查离开主线程（同步 command 在主线程执行会冻结 UI——0.3.0 卡死根因）。
    // 返回**真正可更新**的版本（latest > 当前 App 版本才 Some），避免已是最新版
    // 仍误报「发现新版本」——此前仅返回 latest，前端 `if (v)` 拿到版本号即误判。
    tauri::async_runtime::spawn_blocking(|| {
        let latest = latest_app_version().ok()?;
        let cur = env!("CARGO_PKG_VERSION");
        if crate::registry::cmp_versions(&latest, cur) == std::cmp::Ordering::Greater {
            Some(latest)
        } else {
            None
        }
    })
    .await
    .unwrap_or(None)
}

#[tauri::command]
pub async fn app_update_cmd(
    app: tauri::AppHandle,
    webview: tauri::Webview,
) -> Result<(), String> {
    crate::ensure_shell_webview(&webview)?;
    // 先做所有可能失败的校验（版本查询 / 平台产物 / 文件名拼装）：这些失败不必占用并发门，
    // 门一旦置位就只允许从下面那个唯一出口复位，避免"卡在更新中"的永久锁定。
    let ver = latest_app_version()?;
    let url = asset_url(&ver).ok_or_else(|| "当前平台暂不支持自动安装".to_string())?;
    // 文件名显式拼装，**不要**用 Path::with_extension：它会把 "0.4.2" 的 ".2" 当扩展名
    // 替换掉，生成 dsh-desktop-update-0.4.dmg（版本号被截断，且 0.4.2 / 0.4.3 会撞名）。
    let installer = if url.ends_with(".dmg") {
        std::env::temp_dir().join(format!("dsh-desktop-update-{ver}.dmg"))
    } else if url.ends_with(".exe") {
        std::env::temp_dir().join(format!("dsh-desktop-update-{ver}.exe"))
    } else {
        return Err("未知安装包类型".to_string());
    };
    let _ = std::fs::remove_file(&installer); // 清掉同版本的历史残留

    // 并发门：第二次调用直接拒绝（否则会下载两份安装包、拉起两次安装器）
    if UPDATING.swap(true, Ordering::SeqCst) {
        return Err("已有更新任务在进行中，请稍候".to_string());
    }
    let cleanup = installer.clone();
    let app2 = app.clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        let dest = installer.clone();
        download_installer(&url, &dest, &app2)?;
        // 安装器接手前落标记：Windows 上 /S 安装器会杀掉本进程，能不能装成只能靠
        // **下次启动**核对（见 note_boot_after_update_attempt）
        mark_update_attempt(&app2, &ver);
        #[cfg(target_os = "macos")]
        install_macos(&installer)?;
        #[cfg(target_os = "windows")]
        install_windows(&installer, &app2)?;
        Ok::<(), String>(())
    })
    .await
    // 线程 panic（JoinError）与安装/下载失败并入同一条出口——**不能用 `?` 提前返回**，
    // 否则并发门不复位，UI 会永久停在"更新中"（点不了检查更新，只能重启 App）。
    .map_err(|e| format!("更新线程异常：{e}"))
    .and_then(|r| r);

    // 安装包用完即删（成功失败都删）：否则每次更新都把整个包留在临时区
    // （macOS DMG ≈54MB、Windows setup.exe ≈30MB；用户实测残留 dsh-desktop-update-0.4.dmg）。
    // macOS 此刻已 hdiutil detach、重启交给独立延迟进程，删除安全。
    // Windows 走不到这里（/S 安装器在安装开始就杀掉本进程，见 install_windows 注释），
    // 那种情况由下次启动的 sweep_stale_installers 兜底。
    let _ = std::fs::remove_file(&cleanup);
    // 唯一出口：无论成败都放开并发门（成功路径紧随其后 app.exit(0) 退出本进程；
    // Windows 上安装器会杀掉本进程，同样无需复位）
    UPDATING.store(false, Ordering::SeqCst);
    if let Err(e) = result {
        // 走到这里说明本进程还活着 = 安装器没接手（macOS hdiutil/ditto 失败、Windows
        // 安装器提前退出或被拦下）：结果已知且 UI 已弹出失败原因，撤掉启动标记，
        // 免得下次启动再报一次「上次更新未生效」。
        let _ = std::fs::remove_file(update_attempt_path(&app));
        return Err(e);
    }

    // 安装成功 → 退出当前实例（安装器/新版会负责启动）
    app.exit(0);
    Ok(())
}

/// 启动时清扫更新器遗留的安装包。存在的必要性（Windows）：`/S` 安装器在安装 Section
/// 开头就 KillProcessCurrentUser，所以「装完即删」的代码根本执行不到，残留只能在
/// **下一次启动**（新版起来、安装器早已退出）时清掉。
pub fn sweep_stale_installers() {
    let Ok(rd) = std::fs::read_dir(std::env::temp_dir()) else {
        return;
    };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        let age = e
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.elapsed().ok());
        if is_stale_installer(&name, age) {
            crate::logln(&format!("[update] 清理遗留安装包: {}", e.path().display()));
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// 更新包在临时区的保留时长上限：超过才清（正在进行的更新刚下载完，mtime 很新，
/// 绝不会被误删——这是"下一次启动清扫"能安全落地的关键）。
const STALE_INSTALLER_AGE: Duration = Duration::from_secs(3600);

/// 是否属于「本 App 的、已过期的更新包」——纯函数便于测试。
/// 只认固定前缀 `dsh-desktop-update-`，不做通配匹配，因此不会波及临时区其它文件。
fn is_stale_installer(name: &str, age: Option<Duration>) -> bool {
    name.starts_with("dsh-desktop-update-")
        && age.map(|d| d > STALE_INSTALLER_AGE).unwrap_or(false)
}

/// 从 `hdiutil attach -plist` 的 XML 输出解析挂载点。
/// 卷名可能含空格（产品名 "DeepSeek Harness Desktop"，多次挂载还会带 " 1" 序号），
/// 旧实现按行尾 token 解析会取错（"1"）→ read_dir 报 os error 2（0.3.2 更新失败根因）。
/// 仅 macOS 使用（install_macos）；Windows 编译需排除，否则 clippy -D warnings 报 dead_code。
#[cfg(any(target_os = "macos", test))]
fn parse_mount_point(plist: &str) -> Option<String> {
    let key = "<key>mount-point</key>";
    // 取第一个**非空**值：plist 里可能先出现空 mount-point（占位实体），
    // 只看第一项会直接判定失败（旧实现遇到空值就走 None）
    let mut rest = plist;
    while let Some(idx) = rest.find(key) {
        let after = &rest[idx + key.len()..];
        if let Some(s) = after.find("<string>").map(|i| i + "<string>".len()) {
            if let Some(e) = after[s..].find("</string>") {
                let v = &after[s..s + e];
                if !v.is_empty() {
                    return Some(v.to_string());
                }
            }
        }
        rest = &after[0..]; // 跳过这一个 key 继续找
        if rest.is_empty() {
            break;
        }
    }
    None
}

#[cfg(target_os = "macos")]
fn install_macos(dmg: &Path) -> Result<(), String> {
    use std::process::Command;
    // 1. mount（-plist 输出结构化挂载点——卷名含空格时按行解析会取错，
    //    0.3.2 更新失败根因：取到最后一个 token "1" → "读取 DMG 内容失败"）
    let out = Command::new("hdiutil")
        .args(["attach", "-plist", "-nobrowse", "-readonly"])
        .arg(dmg)
        .output()
        .map_err(|e| format!("挂载 DMG 失败: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "挂载 DMG 失败: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let mount = parse_mount_point(&stdout)
        .ok_or_else(|| format!("无法解析挂载点:\n{stdout}"))?;
    // 2/3. 取 .app 并复制到 /Applications。用闭包包住：**挂载之后的每条失败路径都必须
    // detach**，否则每次失败的更新都在 /Volumes 下留一个挂载点（累积成 "名称 1/2/3"，
    // 卷名冲突还会改变后续挂载点解析）。旧实现在 read_dir / 未找到 .app / 复制失败三处
    // 直接 return，跳过了唯一的 detach。
    let copy_in = || -> Result<std::path::PathBuf, String> {
        // 2. find .app
        let app_name = std::fs::read_dir(&mount)
            .map_err(|e| format!("读取 DMG 内容失败: {e}"))?
            .flatten()
            .find(|e| e.path().extension().map(|x| x == "app").unwrap_or(false))
            .map(|e| e.file_name().to_string_lossy().to_string())
            .ok_or_else(|| "DMG 中未找到 .app".to_string())?;
        let src = std::path::Path::new(&mount).join(&app_name);
        let dst = std::path::Path::new("/Applications").join(&app_name);
        // 3. copy (plain first; escalate via osascript if permission denied)
        let cp = Command::new("ditto").arg(&src).arg(&dst).status();
        if !matches!(cp, Ok(s) if s.success()) {
            // 路径要穿过两层转义：先按 AppleScript 字符串规则（\\ 与 \"），再按 shell
            // 单引号规则（' → '\''）。只转义单引号时，路径里带 `"` 或 `\` 会让脚本解析失败。
            let esc = |p: &Path| {
                p.display()
                    .to_string()
                    .replace('\\', "\\\\")
                    .replace('"', "\\\"")
                    .replace('\'', "'\\''")
            };
            let script = format!(
                "do shell script \"ditto '{}' '{}'\" with administrator privileges",
                esc(&src),
                esc(&dst)
            );
            let ok = Command::new("osascript")
                .args(["-e", &script])
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if !ok {
                return Err("复制到 /Applications 失败（无写入权限且提权被取消）".to_string());
            }
        }
        Ok(dst)
    };
    let copied = copy_in();
    // 4. detach（成功失败都执行）
    let _ = Command::new("hdiutil").args(["detach"]).arg(&mount).output();
    let dst = copied?;
    // 5. 重启新版——不能在这里直接 `open`：当前实例随即 app.exit(0)，
    //    open 请求会被 Launch Services 路由给"仍注册中的旧实例"（实测更新后
    //    App 消失且无新进程）。改用独立延迟进程：sleep 等旧实例退净后
    //    `open -n` 强制启动新实例（-n 防单实例路由残留）。失败仅致不自动
    //    重启（可手动打开），不影响安装结果。
    let app_path = dst.display().to_string();
    let _ = Command::new("/bin/sh")
        .args(["-c", &format!("sleep 2; open -n \"{app_path}\"")])
        .spawn();
    Ok(())
}

/// Windows NSIS 安装器：静默安装 + 装完自己拉起新版。
///
/// **函数体在所有平台都参与编译**（只有调用点是 `cfg(windows)`），这样 Windows 专属的
/// 更新链路在 macOS 上也能被编译器检查到——本机 `cargo check --target
/// x86_64-pc-windows-msvc` 会在 ring 的 C 依赖处失败（缺 MSVC 头文件），无法作为
/// 类型检查手段。平台相关的部分收在函数内的 cfg 块里。
///
/// 必须同时传 `/R`（安装完成后由安装器拉起新版）。
///
/// 只传 `/S` 是不行的：Tauri 的 NSIS 模板在安装 Section 开头就执行
/// `CheckIfAppIsRunning`，而该宏在静默模式下**不询问、直接
/// KillProcessCurrentUser**（tauri-bundler 的 nsis/utils.nsh：`IfSilent kill_…`）。
/// 也就是说 `/S` 一开始就会杀掉「正阻塞在 .status() 上等它结束」的本进程，
/// 于是本函数之后的任何代码（包括原先那段 powershell 延迟启动）都是死代码，
/// 表现为「App 内更新后程序消失、不自动重启」。`/R` 让安装器自己在
/// `.onInstSuccess` 里 RunAsUser 拉起新版（installer.nsi 的 /R 分支）。
///
/// 但 `/S` 只杀**主程序**（KillProcessCurrentUser 按 `${MAINBINARYNAME}.exe` 匹配）：
/// 本 App 拉起的 dsh 子进程（跑的是 `$INSTDIR\resources\node\node.exe`）以及
/// 安装 dsh 用的 pnpm 都不在其中。Windows 上「正在运行的映像文件不可写」，安装器
/// 覆盖 node.exe 时会 ERROR_SHARING_VIOLATION → 安装中断，而此时主程序已被杀，
/// 用户看到的就是「程序退出后没反应、也没更新」（0.5.x 实测反馈）。
/// 卸载路径早先踩过同一个坑并已修（installer-hooks.nsh：dsh 子进程占用 $INSTDIR
/// 程序文件 → NSIS 删文件必然失败且无反馈），这里用同样的解法：**先自己收干净**，
/// 再让安装器接手。
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn install_windows(exe: &Path, app: &tauri::AppHandle) -> Result<(), String> {
    use std::process::Command;
    crate::logln("[update] 结束 dsh/pnpm 子进程后交给安装器（$INSTDIR 文件锁）");
    crate::kill_dsh(); // 本进程的 dsh 树 + 安装中的 pnpm（并标记为主动停止，不再拉起）
    crate::kill_stale_children(app); // 兜底：孤儿 dsh / plugin install 的 node
    // 内核回收进程对象、释放映像锁还有一点延迟：以「能否写打开 node.exe」为准等它落地
    // （只写打开、不 truncate，不改文件内容）。
    let node_exe = crate::node_bin(&crate::paths_from_app(app).resources);
    if !wait_node_exe_writable(&node_exe, Duration::from_secs(5)) {
        // 不阻断：可能本就没有内置 node（或路径判定不同），交给安装器自己报错，
        // 但日志里留一条线索供事后定位。
        crate::logln(&format!(
            "[update] 等待 node.exe 释放写锁超时（继续安装）：{}",
            node_exe.display()
        ));
    }
    let mut cmd = Command::new(exe);
    #[cfg(target_os = "windows")]
    {
        cmd = crate::no_console(cmd); // 避免静默安装期间闪控制台窗口
    }
    let status = cmd.arg("/S").arg("/R").status().map_err(|e| {
        // 本进程还活着说明安装器没接手：把 dsh 拉回来，别把用户丢在连不上工作台的壳里
        crate::restart_dsh(app);
        format!("启动安装器失败: {e}")
    })?;
    if !status.success() {
        crate::restart_dsh(app);
        return Err(format!("安装器退出码异常: {status}"));
    }
    Ok(())
}

/// 等 `<resources>/node/node.exe` 变成「可写打开」——这正是安装器覆盖该文件时要做的事。
/// 用于 install_windows：kill_dsh/kill_stale_children 之后映像锁不是立刻释放的，
/// 抢先让安装器开始复制就会撞上 ERROR_SHARING_VIOLATION。超时返回 false（由调用方决定
/// 是否继续）。
/// 平台无关（纯 std::fs）+ 本机可单测；只有 Windows 的安装路径调用它，
/// 故其它平台放行 dead_code 提示（仓库既有惯例）。
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn wait_node_exe_writable(node_exe: &Path, timeout: Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        // 只写打开、不 truncate、不 create：纯粹探测「现在能不能写」，不改文件内容。
        if std::fs::OpenOptions::new().write(true).open(node_exe).is_ok() {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// 更新尝试的落盘标记：交给安装器之前写，**下次启动**核对。
///
/// 为什么需要：Windows 上 `/S` 安装器会杀掉本进程（`CheckIfAppIsRunning`），安装到底成没成
/// 本进程无从知晓——失败时用户只看到「程序退出后就没反应了」，日志里也没有任何一行
/// （0.5.x 实测反馈）。下次启动读到这个标记，就能明确告诉用户「上次更新到 vX 未生效」
/// 并给出日志/手动安装路径，而不是让人对着一个消失的窗口猜。
fn update_attempt_path(app: &tauri::AppHandle) -> std::path::PathBuf {
    crate::paths_from_app(app).app_data.join("update-attempt.txt")
}

/// 记录「本次启动正在尝试更新到 ver」（安装器接手前调用）。
fn mark_update_attempt(app: &tauri::AppHandle, ver: &str) {
    let p = update_attempt_path(app);
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Err(e) = std::fs::write(&p, format!("{ver}\n")) {
        crate::logln(&format!("[update] 写更新标记失败: {e}"));
    }
}

/// 标记内容 → 目标版本（纯函数便于单测：只认第一行、去空白，空/无内容 → None）。
fn parse_update_attempt(mark: &str) -> Option<String> {
    let v = mark.lines().next().unwrap_or("").trim().to_string();
    if v.is_empty() {
        None
    } else {
        Some(v)
    }
}

/// 启动时核对上次 App 更新是否生效（只报一次，随后清掉标记）。
pub fn note_boot_after_update_attempt(app: &tauri::AppHandle) {
    let p = update_attempt_path(app);
    let Ok(txt) = std::fs::read_to_string(&p) else {
        return; // 没尝试过更新：绝大多数启动走这里
    };
    let _ = std::fs::remove_file(&p);
    let Some(target) = parse_update_attempt(&txt) else {
        return;
    };
    let cur = env!("CARGO_PKG_VERSION");
    if target == cur {
        crate::logln(&format!("[update] 已更新到 v{cur}"));
        return;
    }
    let log = crate::paths_from_app(app).app_data.join("logs/launcher.log");
    let msg = format!(
        "上次更新到 v{target} 未生效（当前仍为 v{cur}）。可能是安装目录文件被占用或安装器被安全软件拦下；可重试，或到 Releases 手动下载安装。\n日志：{}",
        log.display()
    );
    crate::logln(&format!("[update] {msg}"));
    crate::notify("更新未生效", &msg);
}

#[cfg(test)]
mod tests {
    use super::*;
    // brief 测试函数名与实现同名：本地定义会遮蔽 glob 导入的同名实现，
    // 故以显式别名导入解除遮蔽（断言内容与 brief 完全一致）。
    use super::parse_tag_from_effective_url as parse_tag_impl;

    #[test]
    fn parse_tag_from_effective_url() {
        assert_eq!(
            parse_tag_impl("https://github.com/Jedeiah/dsh-desktop/releases/tag/v0.3.1").as_deref(),
            Some("0.3.1")
        );
        assert_eq!(parse_tag_impl("https://github.com/Jedeiah/dsh-desktop/releases/tag/v0.3.1?foo=1").as_deref(), Some("0.3.1"));
        assert_eq!(parse_tag_impl("https://github.com/other/releases/tag/v1.0.0").as_deref(), Some("1.0.0"));
        assert_eq!(parse_tag_impl("https://example.com/404"), None);
        assert_eq!(parse_tag_impl(""), None);
    }

    #[test]
    fn asset_url_built_per_platform() {
        // macOS CI 仅出 arm64 产物；x86_64 mac 无产物 → None（手动兜底）
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        {
            let url = asset_url("0.3.1").unwrap();
            assert!(url.contains("DeepSeek.Harness.Desktop_0.3.1_aarch64.dmg"), "macOS arm64: {url}");
        }
        #[cfg(all(target_os = "macos", not(target_arch = "aarch64")))]
        assert!(asset_url("0.3.1").is_none(), "macOS x86_64 无 CI 产物");
        #[cfg(target_os = "windows")]
        assert!(
            asset_url("0.3.1").unwrap().ends_with("DeepSeek.Harness.Desktop_0.3.1_x64-setup.exe"),
            "Windows: {}",
            asset_url("0.3.1").unwrap()
        );
    }

    #[test]
    fn tag_version_is_sanitized() {
        // 用别名 parse_tag_impl：模块内同名测试函数会遮蔽 glob 导入的实现
        assert_eq!(
            parse_tag_impl("https://github.com/a/b/releases/tag/v0.4.2").as_deref(),
            Some("0.4.2")
        );
        assert_eq!(
            parse_tag_impl("https://github.com/a/b/releases/tag/v0.1.5-rc.2?x=1").as_deref(),
            Some("0.1.5-rc.2")
        );
        // 远端可控：带路径分隔符 / .. / 非 ASCII 的 tag 一律拒绝
        // （否则会拼进下载 URL 与本地临时文件名，写到临时目录之外）
        assert_eq!(
            parse_tag_impl("https://github.com/a/b/releases/tag/v1.0/../../evil"),
            None
        );
        assert_eq!(parse_tag_impl("https://github.com/a/b/releases/tag/v中文"), None);
        assert_eq!(parse_tag_impl("https://github.com/a/b/releases"), None);
    }

    #[test]
    fn parse_mount_point_extracts_plist_value() {
        // 卷名含空格 + 序号（产品名 "DeepSeek Harness Desktop" 挂载后可能带 " 1"）——
        // 0.3.2 更新失败根因：旧行解析取到最后一个 token（"1"），read_dir 报 os error 2
        let plist = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>system-entities</key><array><dict>
<key>mount-point</key><string>/Volumes/DeepSeek Harness Desktop 1</string>
</dict></array></dict></plist>"#;
        assert_eq!(
            parse_mount_point(plist).as_deref(),
            Some("/Volumes/DeepSeek Harness Desktop 1")
        );
        // 无挂载点（attach 失败/无卷）→ None
        assert_eq!(parse_mount_point("<plist><dict></dict></plist>"), None);
        assert_eq!(parse_mount_point(""), None);
    }

    #[test]
    fn update_attempt_mark_parses_first_line() {
        assert_eq!(parse_update_attempt("0.5.3\n").as_deref(), Some("0.5.3"));
        assert_eq!(parse_update_attempt("0.5.3").as_deref(), Some("0.5.3"));
        assert_eq!(parse_update_attempt("  0.5.3-rc.1  \n噪声\n").as_deref(), Some("0.5.3-rc.1"));
        assert_eq!(parse_update_attempt("\n0.5.3\n"), None);
        assert_eq!(parse_update_attempt(""), None);
        assert_eq!(parse_update_attempt("   \n"), None);
    }

    #[test]
    fn node_exe_write_probe_reports_and_times_out() {
        let dir = std::env::temp_dir().join(format!("dsh-update-probe-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("node.exe");
        std::fs::write(&f, b"stub").unwrap();
        // 可写 → 立刻 true，且**不改动内容**（探测只写打开、不 truncate）
        assert!(wait_node_exe_writable(&f, Duration::from_millis(200)));
        assert_eq!(std::fs::read(&f).unwrap(), b"stub");
        // 打不开（不存在的路径）→ 等满超时后 false
        let missing = dir.join("no-such-dir").join("node.exe");
        let t0 = std::time::Instant::now();
        assert!(!wait_node_exe_writable(&missing, Duration::from_millis(250)));
        assert!(t0.elapsed() >= Duration::from_millis(250));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stale_installer_sweep_is_conservative() {
        let old = Some(Duration::from_secs(7200));
        let fresh = Some(Duration::from_secs(60));
        // 只清「本 App 前缀 + 已过期」的文件
        assert!(is_stale_installer("dsh-desktop-update-0.4.2.dmg", old));
        assert!(is_stale_installer("dsh-desktop-update-0.4.2.exe", old));
        assert!(!is_stale_installer("dsh-desktop-update-0.4.2.dmg", fresh)); // 刚下载的安装包不能删
        assert!(!is_stale_installer("dsh-desktop-update-0.4.2.dmg", None)); // 拿不到 mtime → 保守不动
        // 非本 App 文件一律不碰（临时区还有别的程序的文件）
        assert!(!is_stale_installer("other-app.dmg", old));
        assert!(!is_stale_installer("DSH-notes.txt", old));
    }
}
