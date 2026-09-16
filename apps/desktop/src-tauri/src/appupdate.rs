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

/// 是否有 App 更新任务在进行中——供其它会拉起 node/dsh 的命令做互斥。
/// 必要性（Windows）：更新收尾阶段安装器正在覆盖 `$INSTDIR` 里的文件，此时任何一次
/// dsh 重启 / dsh 安装 / 插件操作都会重新拉起 `resources\node\node.exe`，把刚释放的
/// 映像锁又占回去 → 安装器静默跳过该文件（NSIS 默认 AllowSkipFiles=on）。前端已按钮级
/// 禁用，这里是后端兜底。
pub fn update_in_progress() -> bool {
    UPDATING.load(Ordering::SeqCst)
}

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
    // 安装包路径的副本：**只有 macOS 分支会用它**（装完即删）；Windows 分支不删（要留给
    // 更新助手在退出后使用），所以这个绑定也必须跟着 cfg——否则 Windows 上 clippy
    // `-D warnings` 会报 unused variable（本机 macOS 编译看不到，CI 的 windows job 才会炸）。
    #[cfg(not(target_os = "windows"))]
    let cleanup = installer.clone();
    let app2 = app.clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        let dest = installer.clone();
        download_installer(&url, &dest, &app2)?;
        // 交给安装器/助手之前落标记：本进程随后就退出了（Windows 是等助手握手后退出、
        // macOS 是安装完延迟重启），结果只能靠**下次启动**核对（见
        // note_boot_after_update_attempt）
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

    // 安装包用完即删（macOS）：此前每次更新都把整个包留在临时区（DMG ≈54MB；用户实测
    // 残留 dsh-desktop-update-0.4.dmg）。macOS 此刻已 hdiutil detach、重启交给独立延迟
    // 进程，删除安全。
    // **Windows 不在这里删**：安装包要留给更新助手在**本进程退出之后**使用（NSIS 是按需从
    // 自身文件读压缩数据的，提前删掉有可能让安装中途读不到数据），失败时也要留着重试；
    // 留给下次启动的 sweep_stale_installers（>1h 才清）兜底。
    #[cfg(not(target_os = "windows"))]
    let _ = std::fs::remove_file(&cleanup);
    // 唯一出口：无论成败都放开并发门（成功路径紧随其后 app.exit(0) 退出本进程）
    UPDATING.store(false, Ordering::SeqCst);
    if let Err(e) = result {
        // 走到这里说明本进程还活着 = 助手/安装器没能接手（macOS hdiutil/ditto 失败、
        // Windows 更新助手未就绪）：结果已知且 UI 已弹出失败原因，撤掉启动标记，
        // 免得下次启动再报一次「上次更新未生效」。
        clear_update_attempt(&app);
        return Err(e);
    }

    // 安装成功 → 退出当前实例（安装器/新版会负责启动）
    app.exit(0);
    Ok(())
}

/// 启动时清扫更新器遗留的安装包。存在的必要性（Windows）：安装包由更新助手在**本进程退出后**
/// 使用，退出前不能删（安装器是按需从自身文件读压缩数据的，失败时也要留着重试），
/// 所以在下次启动（那时安装器早已退出）清掉最安全。
pub fn sweep_stale_installers() {
    sweep_update_packages(false);
}

/// 清理临时区里本 App 的更新包。
/// `force = false`：启动时的常规清扫——只清超过 TTL 的，绝不碰正在下载的那个；
/// `force = true`：卸载时用——此刻不存在进行中的下载（卸载按钮在更新期间被禁用），
/// 用户已经明确要卸载，留着几十 MB 安装包就是残留。
pub fn sweep_update_packages(force: bool) {
    let Ok(rd) = std::fs::read_dir(std::env::temp_dir()) else {
        return;
    };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if !is_update_package(&name) {
            continue;
        }
        let age = e
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.elapsed().ok());
        if force || is_stale_installer(&name, age) {
            let path = e.path();
            match std::fs::remove_file(&path) {
                // 成功才记：此前先记日志再删，删失败也会留下"已清理"的误导性日志
                Ok(()) => crate::logln(&format!("[update] 清理遗留安装包: {}", path.display())),
                // NotFound=期间被别的路径删了，不算问题；其它（占用等）如实记一条
                Err(err) if err.kind() != std::io::ErrorKind::NotFound => crate::logln(&format!(
                    "[update] 清理安装包失败（留着稍后再试）: {}（{err}）",
                    path.display()
                )),
                Err(_) => {}
            }
        }
    }
}

/// 更新包在临时区的保留时长上限：超过才清（正在进行的更新刚下载完，mtime 很新，
/// 绝不会被误删——这是"下一次启动清扫"能安全落地的关键）。
const STALE_INSTALLER_AGE: Duration = Duration::from_secs(3600);

/// 更新包文件名前缀（`dsh-desktop-update-<ver>.dmg|.exe`）。
const UPDATE_PACKAGE_PREFIX: &str = "dsh-desktop-update-";

/// 是否属于「本 App 的更新包」——纯函数便于测试与复用（卸载清扫按名字即可）。
/// 只认固定前缀，不做通配匹配，因此不会波及临时区其它文件。
pub fn is_update_package(name: &str) -> bool {
    name.starts_with(UPDATE_PACKAGE_PREFIX)
}

/// 是否属于「本 App 的、已过期的更新包」——纯函数便于测试。
fn is_stale_installer(name: &str, age: Option<Duration>) -> bool {
    name.starts_with(UPDATE_PACKAGE_PREFIX)
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
/// 三段事实（均已核对 tauri-bundler 2.9.4 模板与 NSIS 源码，非推测）：
///
/// 1. `/S` 下安装器只按**主程序名**杀进程（installer.nsi 的 `CheckIfAppIsRunning` →
///    utils.nsh 的 `KillProcessCurrentUser "${MAINBINARYNAME}.exe"`；nsis-tauri-utils
///    只按映像名匹配并跳过自身 pid）。→ 本 App 拉起的 dsh 子进程（跑的是
///    `$INSTDIR\resources\node\node.exe`）与安装用的 pnpm 都不在其中。
/// 2. **被占用的文件不会让安装中断，而是被静默跳过**：NSIS 默认 `AllowSkipFiles=on`
///    （build.cpp），打开失败时 MessageBox 类型带 `IDIGNORE<<21`（script.cpp），
///    静默模式下 `my_MessageBox` 直接返回该默认值（util.c）→ `exec.c` 走
///    「跳过该文件、`exec_error++`、继续」——**退出码仍是 0**、`.onInstSuccess` 照常执行。
///    Windows 上「正在运行的映像文件不可写」，所以复制那一刻只要有我们的进程还在跑
///    （主程序或 dsh 的 node.exe），那个文件就被跳过、旧文件留在原地，而安装器报成功。
/// 3. `.onInstSuccess` 只在带 `/R` 时用 `RunAsUser` 拉起 `$INSTDIR\${MAINBINARYNAME}.exe`，
///    且**不检查返回值**（installer.nsi）：旧实例还在时，新进程会被单实例插件转发给
///    旧实例后自行退出。
///
/// 这三条能解释 0.5.x 实测的「点更新 → 程序退出后没反应、也没更新」：安装器没能接管
/// （杀不动/来不及杀）→ 主程序 exe 被静默跳过 → 安装器退出码 0 → 本进程以为成功、
/// `app.exit(0)` → `/R` 拉起的新进程被单实例插件吞掉 → 桌面上什么都不剩、版本还是旧的。
/// **具体是哪一环没杀掉没有在实机复现**（用户机器不可用），但结论不依赖它：只要「复制
/// 文件那一刻还有本应用的进程在跑」就会走到这个结局（机制 2 是模板与 NSIS 源码里已核实的
/// 确定性行为），所以修复方向是让安装发生在**没有任何本应用进程**的时候。
///
/// 因此这里不做「自己 spawn 安装器再退出」那种赌时序的写法（退出时 WebView2 卸载、
/// 进程回收都要时间，安装器可能正好在那一刻开始复制），而是交给一个**外部助手**：
/// 助手先写握手文件 → 等本进程消失 → 才静默安装 → 安装器退出后若没有任何实例在跑，
/// 补一次启动（`/R` 不检查返回值，失败就什么都不剩）。
/// 本函数只在**等到握手文件之后**才返回，调用方随即 `app.exit(0)`；PowerShell 被策略
/// 禁用等情况握手文件不会出现，这里直接报错、保住 App（而不是退出后才发现没人接手）。
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn install_windows(exe: &Path, app: &tauri::AppHandle) -> Result<(), String> {
    crate::logln("[update] 结束 dsh/pnpm 子进程后交给更新助手（$INSTDIR 文件锁）");
    crate::kill_dsh(); // 本进程的 dsh 树 + 安装中的 pnpm（并标记为主动停止，不再拉起）
    crate::kill_stale_children(app); // 兜底：孤儿 dsh / plugin install 的 node
    // 内核回收进程对象、释放映像锁还有一点延迟：以「能否写打开 node.exe」为准等它落地
    // （只写打开、不 truncate，不改文件内容）。超时也继续——本进程马上要退出，
    // 退出后这个锁同样消失；日志留一条线索供事后定位。
    let node_exe = crate::node_bin(&crate::paths_from_app(app).resources);
    if !wait_node_exe_writable(&node_exe, Duration::from_secs(5)) {
        crate::logln(&format!(
            "[update] 等待 node.exe 释放写锁超时（继续交给助手）：{}",
            node_exe.display()
        ));
    }
    match spawn_update_helper(app, exe) {
        Ok(()) => Ok(()),
        Err(e) => {
            // 助手没起来：本进程还活着，但 dsh 已经被我们收掉了，必须拉回来，
            // 否则用户停在一个连不上工作台的壳里；标记也撤掉（结果已知，UI 会报）
            clear_update_attempt(app);
            crate::respawn_dsh(app);
            Err(e)
        }
    }
}

/// 更新助手握手文件：助手启动后第一件事就是写它（内容不重要，只看存在性）。
fn update_helper_marker(app: &tauri::AppHandle) -> std::path::PathBuf {
    crate::paths_from_app(app).app_data.join("update-helper.txt")
}

/// PowerShell 单引号字符串字面量（路径可能含空格/单引号）。
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn ps_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// 起一个脱离本进程的更新助手（PowerShell），等它握手后返回。
///
/// 助手的顺序很关键：**先握手、再等我们退出、最后才安装**——
/// 安装器复制文件时安装目录里必须没有任何我们的进程（含正在退出的自己），
/// 否则被占用的文件会被静默跳过（见 install_windows 的注释 2）。
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn spawn_update_helper(app: &tauri::AppHandle, installer: &Path) -> Result<(), String> {
    use std::process::Command;
    // 安装包必须先还在：被安全软件删掉/隔离时给出明确错误，而不是让助手白跑一趟
    if !installer.is_file() {
        return Err(format!(
            "安装包不存在（可能被杀毒软件隔离）：{}",
            installer.display()
        ));
    }
    let marker = update_helper_marker(app);
    let _ = std::fs::remove_file(&marker); // 清掉上次残留，否则下面的等待会立刻通过
    // 「本进程是否已退出」用 **pid** 判定（精确，不依赖进程名）；兜底拉起时的「有没有
    // 实例在跑」用 exe 文件名判定（新实例是另一个 pid，且名字与安装后的主程序一致）。
    let self_pid = std::process::id();
    let proc_name = std::env::current_exe()
        .ok()
        .and_then(|p| p.file_stem().map(|s| s.to_string_lossy().to_string()))
        .unwrap_or_else(|| "dsh-desktop".to_string());
    // 安装后的主程序路径：与当前 exe 同路径（安装器就地覆盖它）。
    // **必须 strip_verbatim**：Windows 上 `current_exe()` 返回 `\\?\C:\…`，verbatim 前缀
    // 泄漏进子进程参数是本仓库踩过的坑（见 main.rs strip_verbatim 注释），而
    // PowerShell 的 Start-Process 走 ShellExecute，扩展长度路径不被接受。
    // 便携版（不是安装器装的）这里会回退到便携副本——助手只在「一个实例都没起来」时
    // 才用它兜底，不会覆盖 /R 已经拉起的新版。
    let exe_path = std::env::current_exe().ok().map(crate::strip_verbatim);
    let attempt = update_attempt_path(app);
    let script = update_helper_script(
        &marker,
        &attempt,
        self_pid,
        &proc_name,
        installer,
        exe_path.as_deref(),
    );
    let mut cmd = Command::new("powershell");
    // 不加 `-WindowStyle Hidden`：控制台窗口由 no_console（CREATE_NO_WINDOW）负责，
    // 而该参数在非 Windows 平台/部分 PowerShell 版本上会直接报「未实现」把整次调用
    // 变成空跑（实测）。
    cmd.args(["-NoProfile", "-NonInteractive", "-Command", &script]);
    #[cfg(target_os = "windows")]
    {
        cmd = crate::no_console(cmd); // 不闪控制台窗口
    }
    let mut child = cmd.spawn().map_err(|e| format!("启动更新助手失败: {e}"))?;
    // 等握手（最多 10s）：出现=助手确实在跑，可以放心退出。
    // 超时必须**杀掉助手**再报错：否则它 30s 后照样安装，而我们已经告诉用户「已取消」，
    // 还会在 App 存活的情况下装（脚本内也有一道「本进程没退出就放弃」的保险）。
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        if marker.is_file() {
            crate::logln("[update] 更新助手已就绪（等本进程退出后静默安装）");
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            crate::logln("[update] 更新助手未就绪，已结束助手进程并取消本次更新");
            return Err(
                "更新助手未就绪（PowerShell 可能被安全策略禁用），已取消本次更新；可到 Releases 手动下载安装"
                    .to_string(),
            );
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// 生成更新助手脚本（纯函数：便于单测与人工核对）。
///
/// 顺序：**先握手 → 再等本进程消失 → 再确认它真的没了 → 才安装**，安装完若没有任何实例
/// 在跑则补启动一次（`/R` 不检查返回值，失败就什么都不剩）。
/// 握手放在最前，是为了让 App 确认「助手真的在跑」之后才退出；等待+确认放在中间、安装放在
/// 最后，是为了让安装器复制文件时安装目录里一个我们的进程都没有（否则被占用的文件会被
/// 安装器静默跳过，见 install_windows）。**确认那一步不能省**：等待有 30s 上限，到点若
/// 本进程还活着就 `exit`——宁可不装，也不要在 App 活着时装（那正是要消灭的场景）。
/// 「本进程是否已退出」按 `pid` 判（精确）；「有没有实例在跑」按 exe 文件名判（新实例是
/// 另一个 pid，名字与安装后的主程序一致）。
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn update_helper_script(
    marker: &Path,
    attempt: &Path,
    self_pid: u32,
    proc_name: &str,
    installer: &Path,
    exe: Option<&Path>,
) -> String {
    let relaunch = match exe {
        // 拿不到自身路径时不写兜底拉起：宁可不拉，也不要拉起一个空路径
        Some(p) => format!(
            " if (-not (Get-Process -Name {proc} -ErrorAction SilentlyContinue)) {{ Start-Process -FilePath {exe} }}",
            proc = ps_quote(proc_name),
            exe = ps_quote(&p.display().to_string()),
        ),
        None => String::new(),
    };
    format!(
        "$ErrorActionPreference='SilentlyContinue'; \
         Set-Content -LiteralPath {marker} -Value 'ready' -Encoding ASCII; \
         for ($i=0; $i -lt 300; $i++) {{ if (-not (Get-Process -Id {pid} -ErrorAction SilentlyContinue)) {{ break }}; Start-Sleep -Milliseconds 100 }}; \
         if (Get-Process -Id {pid} -ErrorAction SilentlyContinue) {{ exit }}; \
         Add-Content -LiteralPath {attempt} -Value 'state=launching' -Encoding ASCII; \
         $p = Start-Process -FilePath {installer} -ArgumentList '/S','/R' -PassThru; \
         if ($p) {{ $p.WaitForExit() }}; \
         Start-Sleep -Seconds 2;{relaunch}",
        marker = ps_quote(&marker.display().to_string()),
        attempt = ps_quote(&attempt.display().to_string()),
        pid = self_pid,
        installer = ps_quote(&installer.display().to_string()),
        relaunch = relaunch,
    )
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

/// 更新尝试的落盘标记：交给安装器/助手之前写，**下次启动**核对。
///
/// 为什么需要：发起更新的进程随后就退出了（Windows 等助手握手后退出，macOS 装完延迟重启），
/// 安装到底成没成本进程无从知晓——失败时用户只看到「程序退出后就没反应了」，日志里也没有
/// 任何一行（0.5.x 实测反馈）。下次启动读到这个标记，就能明确告诉用户「上次更新到 vX
/// 未生效」并给出日志/手动安装路径，而不是让人对着一个消失的窗口猜。
fn update_attempt_path(app: &tauri::AppHandle) -> std::path::PathBuf {
    crate::paths_from_app(app).app_data.join("update-attempt.txt")
}

/// 记录「本次启动正在尝试更新到 ver」（安装器接手前调用）。
/// 第一行是目标版本；助手真正开始安装时会追加一行 `state=launching`（见
/// update_helper_script），启动核对据此区分「装到一半」与「压根没开始装」。
fn mark_update_attempt(app: &tauri::AppHandle, ver: &str) {
    let p = update_attempt_path(app);
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Err(e) = std::fs::write(&p, format!("{ver}\n")) {
        crate::logln(&format!("[update] 写更新标记失败: {e}"));
    }
}

/// 清掉更新尝试标记：本进程还活着 = 结果已知（失败），由 UI 当场报错，
/// 不该让下次启动再报一遍。
fn clear_update_attempt(app: &tauri::AppHandle) {
    let _ = std::fs::remove_file(update_attempt_path(app));
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

/// 标记判定结论（见 update_attempt_verdict）。
#[derive(Debug, PartialEq, Eq)]
enum UpdateAttempt {
    /// 已经是目标版本：上次更新生效了
    Applied,
    /// 标记太新：安装可能还在进行（用户关掉后马上又打开的情况），本次不下结论
    Pending,
    /// 标记已过等待期而版本仍未变：上次更新没生效
    Failed,
}

/// 安装开始后，允许「已经装完但版本还没变」的观察窗口。发起更新的进程在安装前就退出了，
/// 用户可能立刻又双击图标把**旧**版本拉起来 —— 此时标记才写了十几秒，安装可能正在进行，
/// 不能报「更新未生效」（假警报）。
const UPDATE_ATTEMPT_SETTLE: Duration = Duration::from_secs(120);
/// 助手**还没开始装**（标记里没有 `state=launching`）时的等待上限：助手等本进程退出最多
/// 30s，加上启动开销，60s 后还没进入安装即视为失败——比全窗口更早给出结论。
const UPDATE_ATTEMPT_SETTLE_PRELAUNCH: Duration = Duration::from_secs(60);

/// 纯函数便于单测：标记内容 + 当前版本 + 标记年龄 → 结论。
/// 标记里带 `state=launching`（助手在真正运行安装器前追加）说明安装已开始；没有则是
/// 「只发起了、还没开始装」——两者用不同等待窗口，既避免假警报，也不让「压根没开始装」
/// 拖到两分钟后才发现。
fn update_attempt_verdict(mark: &str, current: &str, age: Option<Duration>) -> Option<UpdateAttempt> {
    let target = parse_update_attempt(mark)?;
    if target == current {
        return Some(UpdateAttempt::Applied);
    }
    let launching = mark.contains("state=launching");
    let window = if launching {
        UPDATE_ATTEMPT_SETTLE
    } else {
        UPDATE_ATTEMPT_SETTLE_PRELAUNCH
    };
    // 拿不到年龄（mtime 缺失）时按「已过等待期」处理：宁可如实报一次失败，
    // 也不要因为读不到时间而永远沉默。
    if age.map(|d| d < window).unwrap_or(false) {
        return Some(UpdateAttempt::Pending);
    }
    Some(UpdateAttempt::Failed)
}

/// 启动时核对上次 App 更新是否生效（只报一次，随后清掉标记）。
pub fn note_boot_after_update_attempt(app: &tauri::AppHandle) {
    let p = update_attempt_path(app);
    let Ok(txt) = std::fs::read_to_string(&p) else {
        return; // 没尝试过更新：绝大多数启动走这里
    };
    let age = std::fs::metadata(&p)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.elapsed().ok());
    let cur = env!("CARGO_PKG_VERSION");
    match update_attempt_verdict(&txt, cur, age) {
        None => {
            let _ = std::fs::remove_file(&p);
        }
        Some(UpdateAttempt::Applied) => {
            let _ = std::fs::remove_file(&p);
            crate::logln(&format!("[update] 已更新到 v{cur}"));
        }
        Some(UpdateAttempt::Pending) => {
            // **保留**标记：等装完（或彻底失败）后的下一次启动再判定
            crate::logln("[update] 更新标记很新，安装可能仍在进行；本次不判定");
        }
        Some(UpdateAttempt::Failed) => {
            let _ = std::fs::remove_file(&p);
            let log = crate::paths_from_app(app).app_data.join("logs/launcher.log");
            let msg = format!(
                "上次更新到 {} 未生效（当前仍为 v{cur}）。可能是安装被安全软件拦下或安装包失效；可重试，或到 Releases 手动下载安装。\n日志：{}",
                parse_update_attempt(&txt).unwrap_or_default(),
                log.display()
            );
            crate::logln(&format!("[update] {msg}"));
            crate::notify("更新未生效", &msg);
        }
    }
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
    fn update_helper_script_shape_and_quoting() {
        let s = update_helper_script(
            Path::new(r"C:\Users\a b\AppData\Roaming\com.dsh-desktop.app\update-helper.txt"),
            Path::new(r"C:\Users\a b\AppData\Roaming\com.dsh-desktop.app\update-attempt.txt"),
            4242,
            "dsh-desktop",
            Path::new(r"C:\Users\a b\AppData\Local\Temp\dsh-desktop-update-0.5.3.exe"),
            Some(Path::new(
                r"C:\Users\a b\AppData\Local\DeepSeek Harness Desktop\dsh-desktop.exe",
            )),
        );
        // 关键顺序：握手 → 等本进程（pid）退出 → **确认真的退出了** → 标记安装已开始 →
        // 安装 → 装完按需补启动
        let handshake = s.find("Set-Content").expect("握手续写");
        let wait_app = s.find("Get-Process -Id 4242").expect("按 pid 等待本进程退出");
        let confirm = s.rfind("Get-Process -Id 4242").expect("确认本进程已退出");
        let mark_launching = s.find("state=launching").expect("标记安装已开始");
        let install = s.find("Start-Process -FilePath 'C:\\Users\\a b\\AppData\\Local\\Temp")
            .expect("启动安装器");
        let relaunch = s.find("Get-Process -Name 'dsh-desktop'").expect("兜底拉起判据");
        assert!(handshake < wait_app && wait_app < confirm, "等待/确认顺序错：{s}");
        assert!(confirm < mark_launching && mark_launching < install, "安装标记/安装顺序错：{s}");
        assert!(install < relaunch, "兜底拉起的判据必须在安装之后：{s}");
        // 确认那一步必须「仍在运行就退出」——宁可不装，也不要在 App 活着时装
        assert!(s.contains("{ exit }") || s.contains(") { exit }"), "缺少放弃保险：{s}");
        // 静默 + 装完拉起新版
        assert!(s.contains("-ArgumentList '/S','/R'"), "缺少 /S /R：{s}");
        // 含空格路径必须是完整单引号字面量（否则 PowerShell 按空格切分）；且用 -LiteralPath
        // （-Path 会把路径里的 [ ] 当通配符）
        assert!(s.contains("'C:\\Users\\a b\\AppData\\Local\\Temp\\dsh-desktop-update-0.5.3.exe'"));
        assert!(s.contains("Set-Content -LiteralPath"), "标记写入应用 -LiteralPath：{s}");
        assert!(s.contains("Add-Content -LiteralPath"), "状态追加应用 -LiteralPath：{s}");
        // 拿不到自身路径时不写兜底拉起（宁可不拉，也不拉起空路径）：-Name 只出现在
        // 兜底拉起那一句里，用它判断该句是否存在
        let no_exe = update_helper_script(
            Path::new("C:/m.txt"),
            Path::new("C:/a.txt"),
            1,
            "p",
            Path::new("C:/s.exe"),
            None,
        );
        assert!(!no_exe.contains("Get-Process -Name"), "{no_exe}");
        assert!(no_exe.contains("Get-Process -Id 1"), "{no_exe}");
        // 单引号转义：路径里的 ' 必须写成 ''
        let q = update_helper_script(
            Path::new("C:/tmp/it's here/m.txt"),
            Path::new("C:/tmp/it's here/a.txt"),
            7,
            "dsh-desktop",
            Path::new("C:/tmp/it's here/setup.exe"),
            Some(Path::new("C:/tmp/it's here/app.exe")),
        );
        assert!(q.contains("'C:/tmp/it''s here/setup.exe'"), "未转义单引号：{q}");
        assert!(ps_quote("a'b") == "'a''b'");
    }

    /// 把生成的助手脚本写到固定路径，供人工用真实 PowerShell 核对/试跑
    /// （本机/CI 没有 PowerShell，故默认忽略）：
    ///   cd apps/desktop/src-tauri && cargo test -- --ignored --nocapture update_helper_script_dump
    #[test]
    #[ignore]
    fn update_helper_script_dump() {
        let out = std::env::temp_dir().join("dsh-update-helper-script.ps1");
        let script = update_helper_script(
            Path::new("C:\\Temp\\helper-marker.txt"),
            Path::new("C:\\Temp\\update-attempt.txt"),
            std::process::id(),
            "dsh-desktop",
            Path::new("C:\\Temp\\dsh-desktop-update-0.5.3.exe"),
            Some(Path::new("C:\\App\\dsh-desktop.exe")),
        );
        std::fs::write(&out, &script).unwrap();
        println!("脚本已写入 {}", out.display());
        println!("{script}");
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
    fn update_attempt_verdict_handles_three_cases() {
        let launched = "0.5.3\nstate=launching\n";
        // 已到目标版本 → 生效
        assert_eq!(
            update_attempt_verdict(launched, "0.5.3", Some(Duration::from_secs(5))),
            Some(UpdateAttempt::Applied)
        );
        // 版本没变但标记很新（用户装完前又双击了旧版）→ 不下结论、保留标记
        assert_eq!(
            update_attempt_verdict(launched, "0.5.2", Some(Duration::from_secs(30))),
            Some(UpdateAttempt::Pending)
        );
        // 过了等待期仍未变 → 判定失败
        assert_eq!(
            update_attempt_verdict(launched, "0.5.2", Some(Duration::from_secs(600))),
            Some(UpdateAttempt::Failed)
        );
        // 只发起了、助手还没开始装（无 state=launching）→ 等 60s 就够，不必等满 120s
        assert_eq!(
            update_attempt_verdict("0.5.3\n", "0.5.2", Some(Duration::from_secs(90))),
            Some(UpdateAttempt::Failed)
        );
        assert_eq!(
            update_attempt_verdict("0.5.3\n", "0.5.2", Some(Duration::from_secs(10))),
            Some(UpdateAttempt::Pending)
        );
        // 拿不到年龄 → 按失败处理（不因读不到时间而永远沉默）
        assert_eq!(
            update_attempt_verdict(launched, "0.5.2", None),
            Some(UpdateAttempt::Failed)
        );
        // 空/无效标记 → 无结论
        assert_eq!(update_attempt_verdict("\n", "0.5.2", None), None);
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
        // 卸载时的强制清扫按名字判（不看年龄）；判定本身仍只认前缀
        assert!(is_update_package("dsh-desktop-update-0.5.3.exe"));
        assert!(is_update_package("dsh-desktop-update-0.5.3.dmg"));
        assert!(!is_update_package("other-app.dmg"));
        assert!(!is_update_package("dsh-desktop-update")); // 没有 `-` 之后的版本段 → 不认
        assert!(!is_update_package("my-dsh-desktop-update-1.exe")); // 前缀必须从头匹配
    }
}
