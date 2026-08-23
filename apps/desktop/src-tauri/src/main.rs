// DSh Desktop (DeepSeek Harness Desktop) — M5 launcher (macOS / Windows)
//
// Spawns the bundled node runtime and manages the dsh closure (install/update),
// parses the readiness URL from dsh's stdout (`dsh web: http://127.0.0.1:<port>`),
// and opens an embedded WebView.
//
// Windows specifics (vs macOS):
//   - node binary is resources/node/node.exe, home env is USERPROFILE
//   - open URL/folder via `cmd start` / `explorer`
//   - trash via the `trash` crate (uninstall recycle bin)
//   - notifications via PowerShell NotifyIcon
//   - `current` version marker is a plain text file (no symlink privilege)
//
// Behaviour:
//   - closing the window hides it and keeps the app in the system tray
//   - quitting (Cmd+Q / tray Quit) kills the dsh child and exits
//   - unexpected dsh crashes auto-restart with exponential backoff
//   - updates: bundled resources are read-only; new closures install into the
//     app data dir via the bundled npm (M3), with registry config + auto-check
//   - logs: launcher + dsh output go to <app-data>/logs/ (dev keeps terminal)
//
// CLI test hooks (no GUI):
//   --self-update-check                 print update status and exit
//   --self-apply-update <version>       install+verify+switch, print, exit

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod appupdate;
mod dsh;
mod plugin;
mod registry;

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tauri::menu::{Menu, MenuItem};
#[cfg(target_os = "macos")]
use tauri::menu::{AboutMetadata, PredefinedMenuItem, SubmenuBuilder};
use tauri::tray::{MouseButton, TrayIconBuilder, TrayIconEvent};
use tauri::{
    AppHandle, Manager, RunEvent, WebviewUrl, WebviewWindowBuilder, WindowEvent,
};
use tauri::Emitter;

/// The running dsh child, kept so it is reaped and so we can kill it on exit.
static CHILD: Mutex<Option<Child>> = Mutex::new(None);
/// The parsed base URL of the running dsh web server.
static DSH_URL: Mutex<Option<String>> = Mutex::new(None);
/// True when we intentionally stopped the child (quit/restart), so EOF is not
/// treated as a crash.
static INTENTIONAL_STOP: AtomicBool = AtomicBool::new(false);
/// Consecutive crash count, used for restart backoff.
static CRASHES: AtomicU32 = AtomicU32::new(0);
/// Launcher log file (packaged mode). Empty in dev (stderr goes to terminal).
static LOG_FILE: Mutex<Option<std::fs::File>> = Mutex::new(None);
/// 自绘弹窗（modal.html）：当前待显示的弹窗内容。替代 rfd 系统对话框，
/// 统一玻璃卡片风格、可居中、可选按钮。
static MODAL_SPEC: Mutex<Option<ModalSpec>> = Mutex::new(None);
/// 自绘弹窗：等待用户按钮结果的通道发送端（show_modal 阻塞等待）。
static MODAL_RESULT: Mutex<Option<mpsc::Sender<bool>>> = Mutex::new(None);
/// 自绘弹窗互斥锁：串行化"设置内容 → 开窗 → 等待结果"，防止并发 show_modal
/// （托盘更新检查 vs boot 崩溃线程）导致内容与结果错配。
static MODAL_LOCK: Mutex<()> = Mutex::new(());

/// App 标识（与 tauri.conf.json identifier 一致；决定 app 数据目录名）。
pub(crate) const APP_ID: &str = "com.dsh-desktop.app";
const RESTART_BASE_MS: u64 = 1000;
const RESTART_MAX_MS: u64 = 15000;
pub(crate) const WINDOW_LABEL: &str = "main";
const MODAL_LABEL: &str = "modal";

/// 用户主目录：优先平台主目录环境变量（unix: HOME，Windows: USERPROFILE），
/// 回退到系统用户目录（跨平台，不写死用户名）。
pub(crate) fn home_dir() -> PathBuf {
    let env = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(env)
        .map(PathBuf::from)
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Poison-safe mutex lock.
pub(crate) fn mlock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Single-instance guard via an exclusive lock file in the app data dir.
/// Returns true when THIS process acquired the lock; false when another
/// instance is running (caller should activate it and exit). The locked file
/// is deliberately leaked so the fd (and thus the lock) lives until exit.
fn acquire_single_instance() -> bool {
    let dir = crate::dsh::app_data_from_home();
    let _ = std::fs::create_dir_all(&dir);
    match std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(dir.join("instance.lock"))
    {
        Ok(f) => {
            if f.try_lock().is_ok() {
                std::mem::forget(f); // keep the fd open so the lock persists
                true
            } else {
                false
            }
        }
        Err(_) => true, // cannot create the lock; do not block startup
    }
}

/// Open the launcher log (creates `<app-data>/logs/launcher.log`).
fn init_log(p: &Paths) {
    let dir = p.app_data.join("logs");
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("launcher.log"));
    if let Ok(f) = f {
        *mlock(&LOG_FILE) = Some(f);
    }
}

/// UTC HH:MM:SS from epoch seconds (tiny, avoids a chrono dependency).
fn hms(secs: u64) -> String {
    let days = secs / 86400;
    let secs_of_day = secs % 86400;
    // civil-from-days (Howard Hinnant) for the date part
    let z = days as i64 + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era as u64 * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    let hh = secs_of_day / 3600;
    let mm = (secs_of_day % 3600) / 60;
    let ss = secs_of_day % 60;
    format!("{y:04}-{m:02}-{d:02} {hh:02}:{mm:02}:{ss:02}")
}

/// Write a launcher log line (file in packaged mode; stderr in dev).
pub(crate) fn logln(msg: &str) {
    let stamp = hms(SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs());
    eprintln!("{msg}");
    if let Some(f) = mlock(&LOG_FILE).as_mut() {
        let _ = writeln!(f, "[{stamp}] {msg}");
    }
}

macro_rules! logln {
    ($($arg:tt)*) => { crate::logln(&format!($($arg)*)) };
}

/// Post a desktop notification (best-effort, per-platform mechanism).
/// 迁自 update.rs（Task 2）：通知与闭包管理解耦，属壳层通用能力。
pub(crate) fn notify(title: &str, body: &str) {
    #[cfg(target_os = "macos")]
    {
        let script = format!(
            "display notification \"{}\" with title \"{}\"",
            body.replace('"', "'"),
            title.replace('"', "'")
        );
        let _ = Command::new("osascript").args(["-e", &script]).spawn();
    }
    #[cfg(target_os = "windows")]
    {
        // PowerShell 无窗口气球提示（NotifyIcon），尽力而为：整体包在
        // try/catch 里，任一环节失败（如 System.Drawing 未加载）都静默。
        let ps = format!(
            "try {{ \
             Add-Type -AssemblyName System.Windows.Forms; Add-Type -AssemblyName System.Drawing; \
             $n = New-Object System.Windows.Forms.NotifyIcon; \
             $n.Icon = [System.Drawing.SystemIcons]::Information; \
             $n.Visible = $true; \
             $n.ShowBalloonTip(5000, '{title}', '{body}', [System.Windows.Forms.ToolTipIcon]::Info); \
             Start-Sleep -Milliseconds 6000; \
             $n.Dispose() \
             }} catch {{}}",
            title = title.replace('\'', "''"),
            body = body.replace('\'', "''")
        );
        let _ = no_console(Command::new("powershell.exe"))
            .arg("-NoProfile")
            .arg("-NonInteractive")
            .arg("-WindowStyle")
            .arg("Hidden")
            .arg("-Command")
            .arg(&ps)
            .spawn();
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let _ = (title, body);
}

/// True when running from a packaged bundle (vs `cargo run` / raw binary).
/// macOS: inside `App.app/Contents/`; Windows: not under a `target\` build dir.
fn is_bundled() -> bool {
    let exe = std::env::current_exe().unwrap_or_default();
    let s = exe.to_string_lossy();
    #[cfg(target_os = "macos")]
    {
        s.contains(".app/Contents/")
    }
    #[cfg(not(target_os = "macos"))]
    {
        !(s.contains("\\target\\") || s.contains("/target/"))
    }
}

/// Resolved paths the launcher needs (shared by the GUI and the CLI hooks).
#[derive(Clone)]
pub struct Paths {
    pub resources: PathBuf,
    pub app_data: PathBuf,
}

// ---------------------------------------------------------------------------
// Paths & settings
// ---------------------------------------------------------------------------

/// Resources root contains the bundled node runtime (node/bin/node or
/// node/node.exe). No dsh closure is bundled anymore (thin shell).
fn has_runtime(dir: &Path) -> bool {
    dir.join("node/bin/node").is_file() || dir.join("node/node.exe").is_file()
}

/// Resources root for the GUI: dev cwd, packaged `Contents/Resources/resources`,
/// or prod `Contents/Resources`.
fn resource_dir(app: &AppHandle) -> PathBuf {
    let res = app.path().resource_dir().unwrap_or_default();
    for cand in [res.join("resources"), res] {
        if has_runtime(&cand) {
            return strip_verbatim(cand);
        }
    }
    strip_verbatim(
        std::env::current_dir()
            .unwrap_or_default()
            .join("resources"),
    )
}

/// Windows verbatim 前缀剥离核心逻辑（纯函数，可跨平台单测）：
/// `\\?\C:\...` → `C:\...`；`\\?\UNC\server\share` → `\\server\share`；无前缀 → None。
/// macOS/Linux 主构建中仅单测使用，故放行 dead_code 提示。
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn strip_verbatim_prefix(s: &str) -> Option<String> {
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        return Some(format!(r"\\{rest}"));
    }
    s.strip_prefix(r"\\?\").map(|rest| rest.to_string())
}

/// Windows：剥离 `\\?\` verbatim 前缀（见 strip_verbatim_prefix）。`std::env::
/// current_exe()` 在 Windows 返回 verbatim 路径，泄漏进子进程参数后 node 的
/// 模块解析会崩溃（`EISDIR: lstat 'C:'`），导致 dsh 无法启动。
/// 非 Windows / 无前缀时原样返回。
fn strip_verbatim(p: PathBuf) -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        if let Some(c) = strip_verbatim_prefix(&p.to_string_lossy()) {
            return PathBuf::from(c);
        }
    }
    p
}

pub(crate) fn paths_from_app(app: &AppHandle) -> Paths {
    let app_data = strip_verbatim(app.path().app_data_dir().unwrap_or_default());
    Paths {
        resources: resource_dir(app),
        app_data,
    }
}

/// Resources root for the CLI hooks (no Tauri handle): bundled resources
/// relative to the executable (macOS `../Resources/resources`, Windows
/// `resources` beside the exe), or dev cwd.
pub(crate) fn paths_from_cli() -> Paths {
    let exe = std::env::current_exe().unwrap_or_default();
    let exe_dir = strip_verbatim(exe.parent().unwrap_or(Path::new(".")).to_path_buf());
    let cwd = strip_verbatim(std::env::current_dir().unwrap_or_default());
    let candidates = [
        exe_dir.join("../Resources/resources"), // macOS .app bundle layout
        exe_dir.join("resources"),              // Windows / generic: beside the exe
        exe_dir,                                // Windows: resources may be the exe dir itself
        cwd.join("resources"),                  // dev: repo cwd
    ];
    let resources = strip_verbatim(
        candidates
            .into_iter()
            .find(|p| has_runtime(p))
            .unwrap_or_else(|| cwd.join("resources")),
    );
    Paths {
        resources,
        app_data: crate::dsh::app_data_from_home(),
    }
}

/// 安全守卫：所有会改变系统状态/访问敏感的 mutating 命令都只接受来自壳页
/// 主窗口（label == WINDOW_LABEL）的调用。虽然 shells-frame 里的远程 dsh 工作
/// 台在 iframe 内通常拿不到 window.__TAURI__（Tauri 仅往主 frame 注入），但
/// tauri.conf.json 的 withGlobalTauri 为 true、且该前提随平台/版本可能变化——
/// 统一在此校验调用窗口，防止任何来源诱导安装/更新/卸载/改写设置等破坏性
/// 操作（纵深防御，不单靠 iframe 隔离）。
pub(crate) fn ensure_shell_window(window: &tauri::WebviewWindow) -> Result<(), String> {
    if window.label() != WINDOW_LABEL {
        return Err("该操作仅限壳页窗口使用".to_string());
    }
    Ok(())
}

/// The bundled node executable (name differs per platform: `node` vs `node.exe`).
pub(crate) fn node_bin(resources: &Path) -> PathBuf {
    if cfg!(windows) {
        resources.join("node/node.exe")
    } else {
        resources.join("node/bin/node")
    }
}

#[cfg(target_os = "macos")]
/// 用户 shell 环境缓存（macOS）。GUI 启动的 App 继承 launchd 最小环境（PATH
/// 只有系统目录，不读 .zshrc/.zprofile）；dsh 的执行器 spawn 的是非交互 shell
/// （不读 rc 文件、PATH 全靠继承）→ 终端里可用的 fnm/node/Homebrew/bun 在
/// app 里不可见。解法：spawn dsh 前取一次"登录交互 shell"的环境（`<shell>
/// -l -i -c env`，与 VS Code 同法），合并进 dsh 子进程，使 app 内执行器与
/// 终端 dsh web 行为一致。只捕获一次并缓存；任何失败静默降级（继承原环境）。
static USER_ENV: Mutex<Option<Vec<(String, String)>>> = Mutex::new(None);

/// 取用户默认 shell 路径（GUI 环境通常无 SHELL 变量：先看 $SHELL，再查
/// passwd（dscl UserShell），最后回退 /bin/zsh）。
#[cfg(target_os = "macos")]
fn user_shell_path() -> String {
    if let Some(sh) = std::env::var_os("SHELL") {
        let sh = sh.to_string_lossy().to_string();
        if !sh.is_empty() && Path::new(&sh).exists() {
            return sh;
        }
    }
    if let Some(user) = std::env::var_os("USER") {
        let user = user.to_string_lossy().to_string();
        if let Ok(out) = Command::new("dscl")
            .arg(".")
            .arg("-read")
            .arg(format!("/Users/{user}"))
            .arg("UserShell")
            .output()
        {
            let s = String::from_utf8_lossy(&out.stdout);
            if let Some(shell) = s
                .lines()
                .find_map(|l| l.trim().strip_prefix("UserShell:").map(|s| s.trim().to_string()))
            {
                if !shell.is_empty() && Path::new(&shell).exists() {
                    return shell;
                }
            }
        }
    }
    "/bin/zsh".into()
}

/// 捕获用户登录交互 shell 的环境（整体 5s 超时：rc 文件可能卡住，超时即降级）。
/// 读取采用**非阻塞 + 边轮询边排空**：
/// - 等待期间持续排空 stdout，避免 rc 打印超过 64KB 管道缓冲时子 shell 写满
///   卡死（否则会静默超时降级，修复对 verbose-rc 用户失效）；
/// - 不依赖"子进程退出后 read 必 EOF"：rc 里 `&` 后台进程若继承 stdout 写端，
///   退出后 read 也会无限阻塞（会拖垮所有 spawn_dsh）——非阻塞读 + WouldBlock
///   轮询 + 同一 deadline 兜底，任何情况下 5s 内必返回。
///
/// 返回过滤后的 KEY=VALUE 列表；失败返回 None（调用方静默继承原环境）。
#[cfg(target_os = "macos")]
fn capture_user_env() -> Option<Vec<(String, String)>> {
    use std::io::Read;
    use std::os::unix::io::AsRawFd;
    let shell = user_shell_path();
    logln!("capturing user shell env via {shell} (-l -i -c env)");
    let mut child = Command::new(&shell)
        .args(["-l", "-i", "-c", "env"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    // 管道置非阻塞：靠 WouldBlock 判断"暂无数据"，read 永不阻塞。
    unsafe {
        libc::fcntl(stdout.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK);
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        match stdout.read(&mut chunk) {
            Ok(0) => break, // EOF：写端全部关闭，数据完整
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                // 连续写入（后台进程持续向继承的 stdout 写）也要受 deadline 约束，
                // 否则永远到不了 WouldBlock 分支，变成无界排空。
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    logln!("user shell env capture timed out; degraded to inherited env");
                    return None;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    logln!("user shell env capture timed out; degraded to inherited env");
                    return None;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(_) => return None,
        }
    }
    // reap：若子 shell 已退出直接收；若尚未退出（提前关 stdout 的怪 rc），杀掉。
    match child.try_wait() {
        Ok(Some(_)) => {}
        _ => {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
    let out = String::from_utf8_lossy(&buf);
    // 解析 KEY=VALUE；过滤 exec 会重设的变量，以及 DYLD_*/LD_PRELOAD 这类
    // 注入面（保护内置 node 进程；其余变量全部保留，保证与终端环境一致）。
    let denylist = [
        "PWD", "OLDPWD", "SHLVL", "_", "SSH_CONNECTION", "TERM_SESSION_ID",
        "__CFBundleIdentifier", "DYLD_LIBRARY_PATH", "DYLD_INSERT_LIBRARIES",
        "DYLD_FRAMEWORK_PATH", "LD_PRELOAD",
    ];
    let mut envs = Vec::new();
    for line in out.lines() {
        if let Some((k, v)) = line.split_once('=') {
            if !k.is_empty()
                && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                && !denylist.contains(&k)
            {
                envs.push((k.to_string(), v.to_string()));
            }
        }
    }
    if envs.is_empty() {
        logln!("user shell env capture produced nothing; degraded to inherited env");
        return None;
    }
    logln!("user shell env captured: {} vars", envs.len());
    Some(envs)
}

/// 把捕获的用户环境应用到子进程（惰性：只捕获一次并缓存；之后每次 spawn
/// 直接复用）。调用方后续显式设置的变量（HOME/SSH_CONNECTION 等）会覆盖。
#[cfg(target_os = "macos")]
pub(crate) fn apply_user_env(cmd: &mut Command) {
    let mut cached = mlock(&USER_ENV);
    if cached.is_none() {
        *cached = capture_user_env();
    }
    if let Some(envs) = cached.as_ref() {
        for (k, v) in envs {
            cmd.env(k, v);
        }
    }
}

#[derive(Serialize, Deserialize, Default, Clone)]
struct Settings {
    /// npm registry base URL override (e.g. https://registry.npmmirror.com)
    registry: Option<String>,
}

fn settings_path_from_data(app_data: &Path) -> PathBuf {
    app_data.join("settings.json")
}

fn load_settings_at(app_data: &Path) -> Settings {
    match std::fs::read_to_string(settings_path_from_data(app_data)) {
        Ok(raw) => serde_json::from_str(&raw).unwrap_or_default(),
        Err(_) => Settings::default(),
    }
}

pub(crate) fn load_settings(app: &AppHandle) -> Settings {
    load_settings_at(&paths_from_app(app).app_data)
}

fn save_settings_at(app_data: &Path, s: &Settings) -> Result<(), String> {
    let raw = serde_json::to_string_pretty(s).map_err(|e| format!("序列化设置失败：{e}"))?;
    std::fs::write(settings_path_from_data(app_data), raw).map_err(|e| format!("写入设置失败：{e}"))
}

pub(crate) fn save_settings(app: &AppHandle, s: &Settings) -> Result<(), String> {
    save_settings_at(&paths_from_app(app).app_data, s)
}

pub(crate) fn spawn_dsh(app: &AppHandle) -> std::io::Result<Child> {
    let p = paths_from_app(app);
    let node = node_bin(&p.resources);
    let closure = crate::dsh::current_closure(&p).ok_or_else(|| {
        std::io::Error::other(format!("dsh closure not found under {}", p.app_data.display()))
    })?;
    let bin = closure.join("node_modules/@deepseek-ai/dsh/lib/bin.js");
    let cwd = home_dir();
    let home = home_dir();
    logln!("spawn node={} bin={} cwd={}", node.display(), bin.display(), cwd.display());
    // packaged: route dsh stderr to the launcher log file; dev: terminal
    let stderr = if is_bundled() {
        let logf = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(p.app_data.join("logs/dsh.log"));
        match logf {
            Ok(f) => Stdio::from(f),
            Err(_) => Stdio::inherit(),
        }
    } else {
        Stdio::inherit()
    };
    let mut cmd = Command::new(&node);
    #[cfg(target_os = "windows")]
    {
        cmd = no_console(cmd); // node.exe 是控制台程序，避免启动 dsh 时闪控制台窗
    }
    // 用户环境合并（macOS）：让 dsh 执行器与终端 dsh web 环境一致
    // （PATH 里的 fnm/Homebrew/bun 等；下方显式 env 会覆盖同名项）。
    #[cfg(target_os = "macos")]
    apply_user_env(&mut cmd);
    cmd.arg(&bin)
        .arg("--profile")
        .arg("web")
        .arg("--port")
        .arg("0")
        .current_dir(&cwd)
        // Force dsh's directory-picker into web "browse" mode (the only
        // reader of SSH_CONNECTION in the closure is the picker resolver):
        // native OS dialogs open on the host machine and cannot serve
        // remote clients, whereas the web picker works everywhere.
        .env("SSH_CONNECTION", "1")
        .stdout(Stdio::piped())
        .stderr(stderr);
    // 让 dsh 认到用户主目录（unix 读 HOME，Windows 读 USERPROFILE；双设更稳）
    if cfg!(windows) {
        cmd.env("USERPROFILE", &home).env("HOME", &home);
    } else {
        cmd.env("HOME", &home);
    }
    cmd.spawn()
}

// ---------------------------------------------------------------------------
// Boot / lifecycle
// ---------------------------------------------------------------------------

/// 首次引导安装状态（供壳页查询）。
#[derive(Serialize)]
struct SetupState {
    installing: bool,
    current: Option<String>,
    /// 最近一次安装进度文本（轮询兜底用：dsh:setup-progress 事件
    /// 在部分环境不可靠——T.event.listen 曾实测挂起，见工作台 URL 的教训）。
    progress: Option<String>,
    /// 当前工作台 URL（dsh 就绪即非 None）：前端轮询据此确定性进入工作台，
    /// 不再依赖 dsh:url 事件（该事件在本环境曾实测丢失——"安装完成但等待
    /// 工作台启动/点击重试立刻进入"的根因）。
    dsh_url: Option<String>,
}

/// 防止并发触发安装（防重复安装；取消/失败/成功都会复位）。
static SETUP_BUSY: AtomicBool = AtomicBool::new(false);

/// 卸载链进行中标志：ExitRequested 时 prevent_exit，防 teardown 被打断。
static UNINSTALLING: AtomicBool = AtomicBool::new(false);

/// 最近一次安装进度文本（setup_state_cmd 轮询读取；事件仅作增量）。
static SETUP_PROGRESS: Mutex<Option<String>> = Mutex::new(None);

/// 安装指定版本 dsh；进度经 `dsh:setup-progress` 推送（并写入
/// SETUP_PROGRESS 供轮询兜底）；成功后 `boot(app)` 启动工作台。
#[tauri::command]
async fn setup_dsh_cmd(
    app: AppHandle,
    window: tauri::WebviewWindow,
    ver: String,
    registry: String,
) -> Result<(), String> {
    crate::ensure_shell_window(&window)?;
    // M2：入口白名单校验，防止任意字符串（npm 参数注入）进入安装流程
    if !crate::registry::valid_version(&ver) {
        return Err("版本号不合法".to_string());
    }
    if SETUP_BUSY.swap(true, Ordering::SeqCst) {
        return Err("已有一个安装正在进行中".to_string());
    }
    *mlock(&SETUP_PROGRESS) = Some("准备安装…".to_string());
    let app_after = app.clone(); // 供安装成功后切回主线程 boot（app 将被 move 进阻塞线程）
    let result = tauri::async_runtime::spawn_blocking(move || {
        let p = paths_from_app(&app);
        let reg = crate::registry::registry_url(Some(&registry));
        crate::dsh::install_version(&p, &ver, &reg, &|msg| {
            *mlock(&SETUP_PROGRESS) = Some(msg.to_string());
            let _ = app.emit("dsh:setup-progress", msg);
        })
    })
    .await;
    // join 失败（线程 panic）也必须在返回前释放锁，否则永久锁死（审查项）。
    // 进度一并复位（与锁并列：成功/失败/取消所有返回路径都覆盖，防残留旧文本）。
    SETUP_BUSY.store(false, Ordering::SeqCst);
    *mlock(&SETUP_PROGRESS) = None;
    result.map_err(|e| format!("安装线程异常：{e}"))??;
    // 安装成功 → 启动工作台。必须放后台线程：boot() 内部阻塞读 dsh 的 stdout
    // 直到其退出（0.3.0 首次安装完成后"程序无响应"根因——run_on_main_thread
    // 会把 boot 放到主线程占死 UI；正常启动/崩溃自愈/restart 均为 thread::spawn）。
    std::thread::spawn(move || boot(app_after));
    Ok(())
}

/// 终止进行中的 npm 安装（规格 5.1「可取消」）；取消后 install 返回 Err →
/// tmp 清理 → 引导页可重试。
#[tauri::command]
fn setup_cancel_cmd(window: tauri::WebviewWindow) {
    if crate::ensure_shell_window(&window).is_err() {
        return;
    }
    crate::dsh::cancel_install();
}

#[tauri::command]
fn setup_state_cmd(app: AppHandle) -> SetupState {
    let p = paths_from_app(&app);
    SetupState {
        installing: SETUP_BUSY.load(Ordering::SeqCst),
        current: crate::dsh::current_closure(&p).and_then(|d| crate::dsh::closure_version(&d)),
        progress: mlock(&SETUP_PROGRESS).clone(),
        dsh_url: mlock(&DSH_URL).clone(),
    }
}

/// 查询 registry 版本列表（registry=None 时用设置值；壳页「刷新版本」传 UI 输入）。
#[tauri::command]
async fn list_dsh_versions_cmd(app: AppHandle, registry: Option<String>) -> Vec<String> {
    // 网络查询必须离开主线程（同步 command 在 WKWebView 主线程回调执行，
    // 阻塞网络会冻结整个 UI——0.3.0 卡死根因）。
    tauri::async_runtime::spawn_blocking(move || {
        let p = paths_from_app(&app);
        let reg = crate::registry::registry_url(
            registry
                .as_deref()
                .or(load_settings_at(&p.app_data).registry.as_deref()),
        );
        crate::registry::list_versions(&reg).unwrap_or_default()
    })
    .await
    .unwrap_or_default()
}

/// dsh 版本管理状态（壳页「更新」Tab 查询）。
#[derive(Serialize)]
struct DshState {
    current: String,
    latest: Option<String>,
    versions: Vec<String>,
    installing: bool,
}

/// 启动时异步查询的 npm latest 缓存（离线/断网失败静默，latest 保持 None）。
static LATEST_DSH: Mutex<Option<String>> = Mutex::new(None);

#[tauri::command]
async fn get_dsh_state(app: AppHandle) -> DshState {
    let p = paths_from_app(&app);
    let reg = crate::registry::registry_url(load_settings_at(&p.app_data).registry.as_deref());
    let current = crate::dsh::current_closure(&p)
        .and_then(|d| crate::dsh::closure_version(&d))
        .unwrap_or_else(|| "未安装".into());
    let latest = mlock(&LATEST_DSH).clone();
    let installing = SETUP_BUSY.load(Ordering::SeqCst);
    // 网络查询（list_versions）离开主线程：同步 command 在主线程执行，
    // 阻塞网络会冻结 UI（0.3.0 卡死根因，见 list_dsh_versions_cmd）。
    let versions = tauri::async_runtime::spawn_blocking(move || {
        crate::registry::list_versions(&reg).unwrap_or_default()
    })
    .await
    .unwrap_or_default();
    DshState {
        latest,
        current,
        versions,
        installing,
    }
}

/// 安装指定版本 dsh 并自动重启工作台（新版本生效）。
#[tauri::command]
async fn update_dsh_cmd(app: AppHandle, window: tauri::WebviewWindow, ver: String) -> Result<(), String> {
    crate::ensure_shell_window(&window)?;
    // M2：入口白名单校验，防止任意字符串（npm 参数注入）进入安装流程
    if !crate::registry::valid_version(&ver) {
        return Err("版本号不合法".to_string());
    }
    if SETUP_BUSY.swap(true, Ordering::SeqCst) {
        return Err("已有一个安装正在进行中".to_string());
    }
    let app_after = app.clone(); // 供安装成功后切回主线程 restart_dsh（app 将被 move 进阻塞线程）
    *mlock(&SETUP_PROGRESS) = Some("准备安装…".to_string());
    let result = tauri::async_runtime::spawn_blocking(move || {
        let p = paths_from_app(&app);
        let reg = crate::registry::registry_url(load_settings_at(&p.app_data).registry.as_deref());
        crate::dsh::install_version(&p, &ver, &reg, &|msg| {
            *mlock(&SETUP_PROGRESS) = Some(msg.to_string());
            let _ = app.emit("dsh:setup-progress", msg);
        })
    })
    .await;
    // join 失败（线程 panic）也必须在返回前释放锁，否则永久锁死（审查项）。
    // 进度一并复位（与锁并列：成功/失败/取消所有返回路径都覆盖，防残留旧文本）。
    SETUP_BUSY.store(false, Ordering::SeqCst);
    *mlock(&SETUP_PROGRESS) = None;
    result.map_err(|e| format!("安装线程异常：{e}"))??;
    // 安装成功 → 自动重启工作台（新版本生效）
    let _ = app_after
        .clone()
        .run_on_main_thread(move || restart_dsh(&app_after));
    Ok(())
}

pub(crate) fn boot(app: AppHandle) {
    // thin shell: no bundled closure — first run must install dsh first
    let p = paths_from_app(&app);
    if crate::dsh::current_closure(&p).is_none() {
        logln!("no dsh closure installed; entering setup mode");
        let _ = app.emit("dsh:need-setup", ());
        reveal_main_window(&app, None);
        return;
    }
    let mut child = match spawn_dsh(&app) {
        Ok(c) => c,
        Err(e) => {
            logln!("failed to spawn dsh: {e}");
            let logs = paths_from_app(&app).app_data.join("logs");
            let msg = format!(
                "无法启动内置 dsh：\n{e}\n\n日志位置：\n{}\n（托盘菜单 Open Logs 可直接打开）",
                logs.display()
            );
            show_modal(
                &app,
                "DeepSeek Harness Desktop 启动失败",
                &msg,
                "ok",
            );
            return;
        }
    };
    let stdout = match child.stdout.take() {
        Some(s) => s,
        None => {
            logln!("no stdout on child");
            return;
        }
    };
    *mlock(&CHILD) = Some(child);

    let reader = BufReader::new(stdout);
    for line in reader.lines() {
        match line {
            Ok(l) => {
                logln!("[dsh] {l}");
                if let Some(idx) = l.find("http://127.0.0.1:") {
                    let url = l[idx..].split_whitespace().next().unwrap_or("").to_string();
                    if !url.is_empty() {
                        *mlock(&DSH_URL) = Some(url.clone());
                        CRASHES.store(0, Ordering::SeqCst); // healthy
                        let app2 = app.clone();
                        let u = url.clone();
                        let _ = app2.clone().run_on_main_thread(move || {
                            reveal_main_window(&app2, Some(&u));
                        });
                    }
                }
            }
            Err(_) => break,
        }
    }
    logln!("dsh process exited (stdout closed)");

    if !INTENTIONAL_STOP.swap(false, Ordering::SeqCst) {
        let n = CRASHES.fetch_add(1, Ordering::SeqCst) + 1;
        if n >= 5 {
            // give up: surface the logs instead of restarting forever
            logln!("dsh crashed {n} times in a row; giving up");
            let logs = paths_from_app(&app).app_data.join("logs");
            let msg = format!(
                "dsh 连续崩溃 {n} 次，已停止自动重启。\n\n日志位置：\n{}\n\n可点托盘菜单 Open Logs 查看 dsh.log 的异常信息。",
                logs.display()
            );
            show_modal(
                &app,
                "DeepSeek Harness Desktop 运行异常",
                &msg,
                "ok",
            );
            return;
        }
        let delay_ms = (RESTART_BASE_MS * 2_u64.pow(n.min(6))).min(RESTART_MAX_MS);
        logln!("dsh crashed (count={n}); restarting in {delay_ms}ms");
        let app2 = app.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(delay_ms));
            let receiver = app2.clone();
            let closer = app2.clone();
            let _ = receiver.run_on_main_thread(move || {
                if let Some(w) = closer.get_webview_window(WINDOW_LABEL) {
                    let _ = w.close();
                }
            });
            boot(app2);
        });
    }
}

// P3：主窗口是壳页，dsh 工作台在壳页的 <iframe> 里。这里把当前 dsh 地址
// 通过 dsh:url 事件推给壳页，让 shell.js 更新 iframe src；主窗自身始终停在
// shell.html（不整窗导航，避免壳页/顶层丢失）。
//
// 显示主窗口的**唯一通道**（启动 / dsh 就绪 / 托盘 / Dock / 重启后共用）：
//  - dsh:url 事件任何情况下都发（壳页 iframe 换端口 / 工作台重启才需要更新）；
//  - 只在窗口「当前不可见」时才 center + show —— 首次出现即居中，已可见时
//    不再重定位。这是启动"往上闪一下"的根因修复：此前窗口创建即 show、
//    dsh 就绪又 center+show 一次，macOS 会对已显示窗口再次定位，产生跳动。
pub(crate) fn reveal_main_window(app: &AppHandle, url: Option<&str>) {
    let Some(w) = app.get_webview_window(WINDOW_LABEL) else {
        // 兜底：主窗不存在（极罕见）时重建壳页。shell.js 自带 get_dsh_url
        // 轮询兜底，重建后无需再补发地址事件。
        if let Ok(w) = WebviewWindowBuilder::new(app, WINDOW_LABEL, WebviewUrl::App("shell.html".into()))
            .title("DeepSeek Harness Desktop")
            .inner_size(1280.0, 820.0)
            .min_inner_size(800.0, 560.0)
            .visible(false)
            .center()
            .theme(Some(tauri::Theme::Dark))
            .on_navigation(webview_navigation_policy)
            .on_new_window(webview_new_window_policy)
            .build()
        {
            let _ = w.center();
            let _ = w.show();
            let _ = w.set_focus();
        }
        return;
    };
    if let Some(u) = url {
        let _ = w.emit("dsh:url", u);
    }
    let visible = w.is_visible().unwrap_or(false);
    if !visible {
        let _ = w.center();
        let _ = w.show();
    }
    let _ = w.set_focus();
}

/// Open a URL in the system default browser.
/// Windows 用 rundll32 url.dll,FileProtocolHandler：不经 cmd.exe 解析，
/// URL 里的 `&`/`%` 等字符不会被当命令分隔符/变量展开截断（cmd start 会）。
fn open_url(url: &str) {
    #[cfg(target_os = "windows")]
    let _ = no_console(Command::new("rundll32"))
        .args(["url.dll,FileProtocolHandler", url])
        .spawn();
    #[cfg(not(target_os = "windows"))]
    let _ = Command::new("open").arg(url).spawn();
}

/// Windows：给控制台子进程加 CREATE_NO_WINDOW，避免从 GUI 进程
/// (windows_subsystem) spawn 控制台程序时弹出/闪烁控制台窗口。
#[cfg(target_os = "windows")]
pub(crate) fn no_console(mut cmd: Command) -> Command {
    use std::os::windows::process::CommandExt;
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    cmd
}

/// 是否为允许在 WebView 内导航的地址：App 内置页（tauri://localhost /
/// http(s)://tauri.localhost）与 **dsh 工作台源端口**（http://127.0.0.1:<dsh
/// 实际端口>）。只放行"当前 dsh 端口"，其它一律放行到外部。未就绪
/// （DSH_URL 空）或 host 不符时一律放行到外部。
fn is_internal_webview_url(url: &tauri::Url) -> bool {
    if url.scheme() == "tauri" {
        return true; // 内置页（tauri://localhost/...）
    }
    if url.scheme() == "http" || url.scheme() == "https" {
        match url.host_str() {
            Some("tauri.localhost") => return true,
            Some("127.0.0.1") | Some("localhost") => {
                // 仅放行与 dsh 工作台一致的目标端口
                let wanted = mlock(&DSH_URL)
                    .as_ref()
                    .and_then(|u| u.rsplit(':').next().and_then(|p| p.parse::<u16>().ok()));
                return Some(url.port().unwrap_or(0)) == wanted;
            }
            _ => return false,
        }
    }
    false
}

/// WebView 导航策略（on_navigation）：内部地址放行；外部 http(s) 及其它
/// 协议（mailto:/tel:/ftp:/file: 等）交给系统浏览器并拦截。修复：AI 回答里的
/// 外链（https://…）此前点击无反应——Tauri 对 target=_blank 新窗口请求默认
/// 一律 Deny。
/// 已知限制（平台不对称）：wry 在 **macOS** 的导航回调（decidePolicyForNavigationAction）
/// 覆盖所有帧——外部 http(s) 的 iframe/表单提交会被拦截并转交浏览器（有意的
/// 安全边界：外部内容不进工作台）；**Windows** 的 NavigationStarting 仅顶层
/// 导航触发，iframe 不受影响。data:/blob:/about: 放行以免破坏内嵌内容
///（如 srcdoc 预览）；javascript: 已拦截（安全加固）。
fn webview_navigation_policy(url: &tauri::Url) -> bool {
    if is_internal_webview_url(url) {
        return true;
    }
    let scheme = url.scheme();
    // 只放行 data/blob/about（内嵌 srcdoc 等需要）；拦截 javascript: ——
    // 配合注入面可成执行面，且正常导航无需 javascript:（安全加固）。
    // 平台不对称同下方"已知限制"说明：macOS 覆盖所有帧、Windows 仅顶层。
    if matches!(scheme, "data" | "blob" | "about") {
        return true;
    }
    let s = url.as_str();
    logln!("[webview] external navigation -> browser: {s}");
    open_url(s);
    false
}

/// 新窗口请求（target=_blank / window.open）：
/// - 内部地址（dsh 工作台/内置页）：Allow——Tauri 开新 webview 窗口，与主窗口
///   共享同一 session/cookie，可正常鉴权（若 dsh 用新窗口开内部页面）。
/// - 外部地址：交给系统浏览器打开并 Deny（不开新窗口）。
fn webview_new_window_policy(
    url: tauri::Url,
    _features: tauri::webview::NewWindowFeatures,
) -> tauri::webview::NewWindowResponse<tauri::Wry> {
    if is_internal_webview_url(&url) {
        return tauri::webview::NewWindowResponse::Allow;
    }
    let s = url.as_str();
    logln!("[webview] new-window request -> browser: {s}");
    open_url(s);
    tauri::webview::NewWindowResponse::Deny
}

pub(crate) fn kill_dsh() {
    // mark as intentional so the boot thread never treats the EOF as a crash
    // and never spawns a restart after the app is quitting (avoids orphans).
    INTENTIONAL_STOP.store(true, Ordering::SeqCst);
    if let Some(mut c) = mlock(&CHILD).take() {
        let _ = c.kill();
        let _ = c.wait();
    }
}

pub(crate) fn restart_dsh(app: &AppHandle) {
    INTENTIONAL_STOP.store(true, Ordering::SeqCst);
    kill_dsh();
    if let Some(w) = app.get_webview_window(WINDOW_LABEL) {
        let _ = w.close();
    }
    let handle = app.clone();
    std::thread::spawn(move || boot(handle));
}

// ---------------------------------------------------------------------------
// P3：插件管理 / 卸载 已并入壳页（shell.html）Tab，不再有独立窗口；主窗为
// 壳页（shell.html），工作台经 iframe 内嵌。所有窗口（主窗 + 弹窗）统一由
// uninstall_run 的 destroy 列表销毁。
// ---------------------------------------------------------------------------
// 自绘弹窗（modal.html）：替代系统对话框，统一玻璃卡片风格、可居中。
// ---------------------------------------------------------------------------

/// 自绘弹窗内容。kind: "ok"（单按钮确定）| "yesno"（稍后/确定）。
/// ok_label/no_label：自定义按钮文案（None 时前端回退「确定」/「稍后」）。
#[derive(Clone, Serialize)]
struct ModalSpec {
    title: String,
    message: String,
    kind: String,
    ok_label: Option<String>,
    no_label: Option<String>,
}

/// 把弹窗窗口定位到主窗口中心（用逻辑尺寸 × 主窗口缩放因子换算物理坐标，
/// 避免刚 build 完 outer_size 尚为 0 时无法计算）。若主窗口不存在/不可见则回退
/// 屏幕中心（builder 的 .center() 兜底）。clamp 到主窗口所在显示器的工作区，
/// 避免副屏边缘或贴边时弹窗跑出可视区域。
fn center_child_on_main(app: &AppHandle, w: &tauri::WebviewWindow, log_w: f64, log_h: f64) {
    let Some(main) = app.get_webview_window(WINDOW_LABEL) else {
        let _ = w.center();
        return;
    };
    let (Ok(mpos), Ok(msize)) = (main.outer_position(), main.outer_size()) else {
        let _ = w.center();
        return;
    };
    let sf = main.scale_factor().unwrap_or(1.0);
    let dw = (log_w * sf) as i32;
    let dh = (log_h * sf) as i32;
    let tx = mpos.x + (msize.width as i32) / 2 - dw / 2;
    let ty = mpos.y + (msize.height as i32) / 2 - dh / 2;
    let mon = main.current_monitor().ok().flatten();
    let (x, y) = if let Some(m) = mon {
        let wa = *m.work_area();
        let wx = wa.position.x;
        let wy = wa.position.y;
        let ww = wa.size.width as i32;
        let wh = wa.size.height as i32;
        // 饱和 clamp：弹窗大于工作区时（极小分辨率/高分屏）min>max 会 panic，
        // 这里先取 max 门槛，保证 max>=min，落在合理位置即可。
        let x = tx.clamp(wx, (wx + ww - dw).max(wx));
        let y = ty.clamp(wy, (wy + wh - dh).max(wy));
        (x, y)
    } else {
        (tx, ty)
    };
    let _ = w.set_position(tauri::PhysicalPosition::new(x, y));
}

/// 打开（或聚焦）自绘弹窗窗口（无系统标题栏、固定尺寸、相对主窗口中心）。
/// 返回 false 表示无法打开（调用方应尽快结束等待）。
fn open_modal_window(app: &AppHandle) -> bool {
    if let Some(w) = app.get_webview_window(MODAL_LABEL) {
        // 复用分支：强制重新加载以获得最新 spec（理论上 MODAL_LOCK 已串行化，
        // 一般不会走到这；防御性处理，避免残留旧内容）。
        let _ = w.eval("location.reload()");
        let _ = w.show();
        let _ = w.set_focus();
        return true;
    }
    match WebviewWindowBuilder::new(app, MODAL_LABEL, WebviewUrl::App("modal.html".into()))
        .decorations(false)
        .resizable(false)
        .visible(false) // 定位后再显示，避免闪烁
        // 透明：modal.html body 已 transparent，不设则窗口默认底色在圆角卡片
        // 外露出方框（用户反馈的"圆角框外面还有个方框"）。
        .transparent(true)
        // 置顶：确认类弹窗（卸载等）必须浮在所有窗口之上，不可被遮挡。
        .always_on_top(true)
        // 关窗口阴影：macOS 透明窗口的阴影跟随窗口矩形（460×300），会在圆角
        // 卡片外形成方形"背景框"（用户反馈）；卡片自身有柔和圆角阴影，无需窗口阴影。
        .shadow(false)
        .inner_size(460.0, 300.0)
        .center() // 兜底
        .on_navigation(webview_navigation_policy)
        .on_new_window(webview_new_window_policy)
        .build()
    {
        Ok(w) => {
            // Alt+F4 / 系统关窗：在用户没点按钮就关闭弹窗时，解除 show_modal 的
            // 24h 阻塞等待（等价 rfd 对话框被关闭即返回）。发送端已被 take 时
            // 说明是 modal_respond 正常关闭，此处为 no-op。
            w.on_window_event(move |ev| {
                if let tauri::WindowEvent::CloseRequested { .. } = ev {
                    if let Some(tx) = mlock(&MODAL_RESULT).take() {
                        let _ = tx.send(false);
                    }
                }
            });
            center_child_on_main(app, &w, 460.0, 300.0);
            let _ = w.show();
            let _ = w.set_focus();
            true
        }
        Err(e) => {
            logln!("[modal] failed to open window: {e}");
            false
        }
    }
}

/// 自绘弹窗：读取待显示内容（modal.html 加载时调用）。
#[tauri::command]
fn modal_spec(window: tauri::WebviewWindow) -> Result<ModalSpec, String> {
    if window.label() != MODAL_LABEL {
        return Err("该操作仅限弹窗窗口使用".to_string());
    }
    mlock(&MODAL_SPEC)
        .clone()
        .ok_or_else(|| "无待显示内容".to_string())
}

/// 自绘弹窗：用户点击按钮后回传结果并关闭窗口（accept = 用户选择确定）。
#[tauri::command]
fn modal_respond(window: tauri::WebviewWindow, accept: bool) -> Result<(), String> {
    if window.label() != MODAL_LABEL {
        return Err("该操作仅限弹窗窗口使用".to_string());
    }
    if let Some(tx) = mlock(&MODAL_RESULT).take() {
        let _ = tx.send(accept);
    }
    mlock(&MODAL_SPEC).take();
    let _ = window.close();
    Ok(())
}

/// 显示自绘弹窗并阻塞等待用户按钮结果（替代 rfd 同步对话框）。
/// 在后台线程调用（boot 线程 / 更新检查线程）：窗口在主线程打开，本线程阻塞等结果。
/// kind="ok" 忽略按钮值；kind="yesno" 返回用户是否选择"确定"。
fn show_modal(app: &AppHandle, title: &str, message: &str, kind: &str) -> bool {
    show_modal_with_labels(app, title, message, kind, None, None)
}

/// show_modal 的按钮文案自定义版（卸载确认等场景）。
fn show_modal_with_labels(
    app: &AppHandle,
    title: &str,
    message: &str,
    kind: &str,
    ok_label: Option<&str>,
    no_label: Option<&str>,
) -> bool {
    // 串行化弹窗生命周期：并发 show_modal（如托盘更新检查 vs boot 崩溃线程）会
    // 造成「窗口显示旧内容、但点击结果发给新线程」的错配。锁覆盖 设置→开窗→等待，
    // 保证同时只有一个弹窗在途；第二个调用排队至前一个结束。
    let _lock = mlock(&MODAL_LOCK);
    let (tx, rx) = mpsc::channel();
    *mlock(&MODAL_RESULT) = Some(tx);
    *mlock(&MODAL_SPEC) = Some(ModalSpec {
        title: title.to_string(),
        message: message.to_string(),
        kind: kind.to_string(),
        ok_label: ok_label.map(|s| s.to_string()),
        no_label: no_label.map(|s| s.to_string()),
    });
    let app2 = app.clone();
    let opened = Arc::new(AtomicBool::new(false));
    let opened2 = opened.clone();
    let app3 = app2.clone();
    let _ = app2
        .clone()
        .run_on_main_thread(move || opened2.store(open_modal_window(&app3), Ordering::SeqCst));
    // 等待主线程完成窗口创建（主线程忙碌时放宽到 10s；创建失败立即返回避免 24h 假阻塞）
    for _ in 0..500 {
        if opened.load(Ordering::SeqCst) {
            break;
        }
        if app2.get_webview_window(MODAL_LABEL).is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    if !opened.load(Ordering::SeqCst) && app2.get_webview_window(MODAL_LABEL).is_none() {
        // 无法打开弹窗：解除发送端与内容，避免残留
        mlock(&MODAL_RESULT).take();
        mlock(&MODAL_SPEC).take();
        return false;
    }
    // 阻塞等待用户点击（24h 超时防死锁，兜底返回 false）
    rx.recv_timeout(Duration::from_secs(24 * 3600)).unwrap_or(false)
}

// ---------------------------------------------------------------------------
// 壳页命令（P3：管理功能从托盘移入主窗壳页 Tabs，经这些 command 调用）
// ---------------------------------------------------------------------------

/// 当前 dsh 工作台 URL（壳页 iframe 用它设定 src；None = dsh 未就绪）。
#[tauri::command]
fn get_dsh_url() -> Option<String> {
    mlock(&DSH_URL).clone()
}

/// 壳页「更新」需要的初始状态。
#[derive(Serialize)]
struct ShellState {
    app_version: String,
    dsh_version: String,
    registry: String,
}

#[tauri::command]
fn get_shell_state(app: AppHandle) -> ShellState {
    let p = paths_from_app(&app);
    let settings = load_settings_at(&p.app_data);
    ShellState {
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        dsh_version: crate::dsh::current_closure(&p)
            .and_then(|dir| crate::dsh::closure_version(&dir))
            .unwrap_or_else(|| "未知".into()),
        registry: crate::registry::registry_url(settings.registry.as_deref()),
    }
}

/// 持久化 npm registry 源（安装/更新 dsh 的下载源）。校验非空且以 http(s)://
/// 开头，规范化后写入 settings.json；后续 list_dsh_versions_cmd / get_dsh_state
/// 都从 settings 读 registry，保存后自动生效。
#[tauri::command]
fn save_registry_cmd(app: AppHandle, window: tauri::WebviewWindow, registry: String) -> Result<(), String> {
    crate::ensure_shell_window(&window)?;
    let trimmed = registry.trim();
    if trimmed.is_empty() {
        return Err("Registry 源不能为空".into());
    }
    if !(trimmed.starts_with("http://") || trimmed.starts_with("https://")) {
        return Err("Registry 源必须以 http:// 或 https:// 开头".into());
    }
    let canonical = crate::registry::registry_url(Some(trimmed));
    let mut settings = load_settings(&app);
    settings.registry = Some(canonical);
    save_settings(&app, &settings)?;
    Ok(())
}

/// 在系统浏览器打开 App 的 GitHub releases 下载页（手动下载兜底入口，规格 5.3）。
/// 原「打开工作台」语义随常规 Tab 删除；工作台地址经 iframe 内嵌即可触达。
#[tauri::command]
fn open_browser_cmd(_app: AppHandle) -> Result<(), String> {
    open_url("https://github.com/Jedeiah/dsh-desktop/releases/latest");
    Ok(())
}

/// 在系统浏览器打开仓库主页（关于页「项目主页」链接）。
#[tauri::command]
fn open_repo_cmd(_app: AppHandle) -> Result<(), String> {
    open_url("https://github.com/Jedeiah/dsh-desktop");
    Ok(())
}

/// 卸载确认窗口：执行卸载（wipe=true 连 ~/.dsh 一起删）。
/// 完整流程：销毁 WebView（释放 WebView2 数据占用）→ teardown → 移入回收站 → 退出。
/// 卸载入口（关于页按钮）：先弹自绘确认窗（yesno，自定义按钮文案），
/// 用户确认后才执行 uninstall_run；取消则无操作。
#[tauri::command]
async fn confirm_uninstall_cmd(app: AppHandle, window: tauri::WebviewWindow, wipe: bool) -> Result<(), String> {
    crate::ensure_shell_window(&window)?;
    let app2 = app.clone();
    let confirmed = tauri::async_runtime::spawn_blocking(move || {
        let (title, msg) = if wipe {
            (
                "完全卸载",
                "将删除 ~/.dsh 全部数据（会话与凭据），此操作不可撤销。",
            )
        } else {
            (
                "确认卸载",
                "将卸载应用，保留 ~/.dsh 配置与数据（可随时重新安装）。",
            )
        };
        show_modal_with_labels(&app2, title, msg, "yesno", Some("确认卸载"), Some("取消"))
    })
    .await
    .map_err(|e| format!("弹窗线程异常：{e}"))?;
    if confirmed {
        uninstall_run(app, window, wipe).await
    } else {
        Ok(())
    }
}

#[tauri::command]
async fn uninstall_run(app: AppHandle, window: tauri::WebviewWindow, wipe: bool) -> Result<(), String> {
    crate::ensure_shell_window(&window)?;
    // 卸载链进行中：ExitRequested 必须 prevent_exit（见 run 循环），
    // 否则 destroy 全部窗口会触发默认退出，teardown 永远来不及执行
    //（0.3.0 卸载"程序退出但没卸载"根因）。
    UNINSTALLING.store(true, Ordering::SeqCst);
    // 先销毁全部 WebView 窗口：释放 WebView2 用户数据目录（app_data 内）占用，
    // Windows 共享锁下不销毁则删除必然 oserror 32。
    for label in [WINDOW_LABEL, MODAL_LABEL] {
        if let Some(w) = app.get_webview_window(label) {
            let _ = w.destroy();
        }
    }
    std::thread::sleep(Duration::from_millis(300)); // 等 WebView2 进程释放数据目录
    // teardown（可能耗时：杀进程 + 删除重试）移到阻塞线程
    let app2 = app.clone();
    let teardown = tauri::async_runtime::spawn_blocking(move || {
        let p = paths_from_app(&app2);
        uninstall_teardown(&p, wipe)
    })
    .await
    .map_err(|e| format!("卸载线程异常：{e}"))?;
    if let Err(e) = teardown {
        // 卸载确认窗口已销毁，JS 无法回显：用系统通知兜底。
        // 复位标志：否则后续 ExitRequested 一直被 prevent_exit 拦截，用户无法退出。
        UNINSTALLING.store(false, Ordering::SeqCst);
        notify("卸载未完成", &e);
        // 窗口已全毁：重建主窗口让用户有恢复入口（否则无窗口可操作，只能强杀）
        let app2 = app.clone();
        let _ = app2.clone().run_on_main_thread(move || reveal_main_window(&app2, None));
        return Err(e);
    }
    // teardown 成功：移入回收站 / 引导系统卸载，然后退出
    #[cfg(target_os = "macos")]
    {
        trash_self();
        std::thread::sleep(Duration::from_millis(900));
        clear_dock_recents();
    }
    #[cfg(target_os = "windows")]
    {
        // 唯一卸载链：调用系统卸载器（uninstall.exe）完成程序文件删除（含 NSIS
        // PREUNINSTALL 钩子 → --self-uninstall-full 兜底清理数据）。数据侧已在
        // teardown 完成；这里退出自身并唤起卸载器，交给 NSIS 删 $INSTDIR。
        // 不再出现"请到设置里卸载"的割裂兜底。
        let exe = std::env::current_exe().unwrap_or_default();
        let uninstaller = exe.parent().unwrap_or(Path::new(".")).join("uninstall.exe");
        let mut spawned = false;
        if uninstaller.is_file() {
            let ok = no_console(Command::new(&uninstaller)).spawn().is_ok();
            if ok {
                spawned = true;
                logln!("[uninstall] spawned system uninstaller: {}", uninstaller.display());
            }
        }
        if !spawned {
            notify(
                "请通过系统卸载",
                "应用数据已清理。请到“设置 → 应用 → 已安装的应用”中卸载 DeepSeek Harness Desktop。",
            );
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        if !trash_self() {
            notify(
                "请通过系统卸载",
                "应用数据已清理。请通过系统卸载 DeepSeek Harness Desktop。",
            );
        }
    }
    // 卸载链完成：复位标志后退出（否则 app.exit 触发的 ExitRequested 会被
    // 自身的 prevent_exit 拦截——从"退出但没卸载"变成"卸载了但不退出"）。
    UNINSTALLING.store(false, Ordering::SeqCst);
    app.exit(0);
    Ok(())
}

// ---------------------------------------------------------------------------
// Uninstaller (M4)
// ---------------------------------------------------------------------------

fn remove_dir_all_retry(dir: &std::path::Path) -> std::io::Result<()> {
    const ATTEMPTS: u32 = 5;
    const DELAY: Duration = Duration::from_millis(400);
    for i in 0..ATTEMPTS {
        match std::fs::remove_dir_all(dir) {
            Ok(()) => return Ok(()),
            Err(e) => {
                if i + 1 == ATTEMPTS {
                    return Err(e);
                }
                std::thread::sleep(DELAY);
            }
        }
    }
    unreachable!()
}

fn uninstall_teardown(p: &Paths, wipe_dsh: bool) -> Result<(), String> {
    kill_dsh();
    let home = home_dir();
    // 关闭日志句柄：当前进程持有 logs/launcher.log，Windows 共享锁下不关则
    // 删除 app_data 必然 oserror 32。后续 logln 不再写文件（卸载流程无需日志）。
    *mlock(&LOG_FILE) = None;
    // app 数据 + WebView 缓存/状态（卸载器必须连缓存一起清干净）
    let mut dirs = vec![p.app_data.clone()];
    #[cfg(target_os = "macos")]
    {
        dirs.push(home.join("Library/Caches").join(APP_ID));
        dirs.push(home.join("Library/WebKit").join(APP_ID));
    }
    #[cfg(target_os = "windows")]
    {
        // WebView2 的用户数据/缓存落在 %LOCALAPPDATA%\<id>
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            dirs.push(strip_verbatim(PathBuf::from(local)).join(APP_ID));
        }
    }
    // 目录级删除：失败不中断整体卸载。Caches/WebKit 常被 WebView 进程延迟占用
    // （destroy 后 300ms 不够释放），失败项再等 1s 补删一轮（实测可清）。
    let mut leftovers: Vec<String> = Vec::new();
    for dir in &dirs {
        if dir.exists() {
            if let Err(e) = remove_dir_all_retry(dir) {
                leftovers.push(format!("{}（{e}）", dir.display()));
            }
        }
    }
    if !leftovers.is_empty() {
        std::thread::sleep(Duration::from_millis(1000));
        let mut still: Vec<String> = Vec::new();
        for dir in &dirs {
            if dir.exists() {
                if let Err(e) = remove_dir_all_retry(dir) {
                    still.push(format!("{}（{e}）", dir.display()));
                }
            }
        }
        leftovers = still;
    }
    if !leftovers.is_empty() {
        let msg = format!("以下目录被占用未能删除，重启电脑后即可手动清理：\n{}", leftovers.join("\n"));
        logln!("[uninstall] 残留目录: {msg}");
        notify("部分数据将延迟清理", &msg);
    }
    if wipe_dsh {
        let dsh_home = home.join(".dsh");
        if dsh_home.exists() {
            // 用户明确要求删除 ~/.dsh：失败必须如实报告
            remove_dir_all_retry(&dsh_home).map_err(|e| format!("删除 ~/.dsh 失败: {e}"))?;
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
/// Remove this app from the Dock's "recent applications" list and refresh the
/// Dock. Uses defaults export/import (through cfprefsd) + python3 plistlib so
/// ONLY this app's entry is dropped; other recents and settings are untouched.
fn clear_dock_recents() {
    let tmp = std::env::temp_dir().join("dsh-dock.plist");
    let t = tmp.to_string_lossy().to_string();
    let py = r#"
import plistlib, sys
p = sys.argv[1]
try:
    with open(p,'rb') as f: d = plistlib.load(f)
except Exception:
    sys.exit(0)
rec = d.get('recent-apps', [])
def bad(r):
    td = r.get('tile-data', {})
    label = td.get('file-label') or td.get('file-data', {}).get('file-label')
    return label == 'DeepSeek Harness Desktop' or b'DeepSeek' in td.get('book', b'')
d['recent-apps'] = [r for r in rec if not bad(r)]
with open(p,'wb') as f: plistlib.dump(d, f, fmt=plistlib.FMT_BINARY)
"#;
    let _ = Command::new("defaults").args(["export", "com.apple.dock", &t]).status();
    let _ = Command::new("python3").args(["-c", py, &t]).status();
    let _ = Command::new("defaults").args(["import", "com.apple.dock", &t]).status();
    let _ = Command::new("killall").arg("Dock").status();
    let _ = std::fs::remove_file(&tmp);
}

#[cfg(not(target_os = "macos"))]
fn clear_dock_recents() {}

/// Ask the user how to uninstall. Returns:
/// Move the running app to the trash / recycle bin. Returns true on success.
#[cfg(target_os = "macos")]
fn trash_self() -> bool {
    let exe = std::env::current_exe().unwrap_or_default();
    // exe = <App>.app/Contents/MacOS/<bin>  ->  app root = <App>.app
    let app_root = exe
        .parent()
        .and_then(|p| p.parent())
        .and_then(|p| p.parent())
        .unwrap_or(Path::new("/"))
        .to_path_buf();
    if !app_root.join("Contents/Info.plist").is_file() {
        return false; // not running from a bundle (e.g. dev)
    }
    let path = app_root.to_string_lossy().replace('"', "\\\"");
    let script = format!("tell application \"Finder\" to delete POSIX file \"{path}\"");
    Command::new("osascript").args(["-e", &script]).spawn().is_ok()
}

/// Move the running app's install directory to the Recycle Bin (Windows).
/// The running exe itself is locked by the OS, so this may fail — callers
/// should fall back to the system uninstaller.
#[cfg(target_os = "windows")]
fn trash_self() -> bool {
    let exe = std::env::current_exe().unwrap_or_default();
    let install_dir = exe.parent().unwrap_or(Path::new(".")).to_path_buf();
    trash::delete(&install_dir).is_ok()
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn trash_self() -> bool {
    false
}

// ---------------------------------------------------------------------------
// CLI test hooks
// ---------------------------------------------------------------------------

#[cfg(target_os = "windows")]
/// 结束其它运行中的本应用实例及其子进程树（`--self-uninstall-full` 卸载 sidecar 用）。
/// 目的：释放 `$INSTDIR` 程序文件 / app 数据 / WebView2 缓存的文件锁 —— 否则
/// NSIS 删文件时因占用而失败（"右键→卸载 无反应"根因之一）。全程无 GUI/无窗口。
/// 只按「本应用进程名 / 本项目 node 脚本命令行特征」匹配，避免误杀用户其它 node。
fn kill_other_app_instances() {
    let self_pid = std::process::id();
    let script = format!(
        r#"$self={self_pid};
Get-CimInstance Win32_Process -ErrorAction SilentlyContinue | Where-Object {{
  ($_.ProcessId -ne $self) -and (
    $_.Name -eq 'dsh-desktop.exe' -or
    ($_.Name -eq 'node.exe' -and $_.CommandLine -match 'bin\.js.*--profile web')
  )
}} | ForEach-Object {{
  taskkill /PID $_.ProcessId /T /F 2>$null | Out-Null
}}"#,
        self_pid = self_pid
    );
    let status = no_console(Command::new("powershell"))
        .arg("-NoProfile")
        .arg("-NonInteractive")
        .arg("-Command")
        .arg(&script)
        .status();
    match status {
        Ok(s) if s.success() => logln!("[uninstall-full] killed other instances ok"),
        Ok(s) => logln!("[uninstall-full] kill other instances exit={:?}", s.code()),
        Err(e) => logln!("[uninstall-full] kill other instances failed to run: {e}"),
    }
    // 给文件句柄释放留一点时间
    std::thread::sleep(Duration::from_millis(600));
}

fn run_cli_hooks(args: &[String]) -> bool {
    if args.iter().any(|a| a == "--self-update-check") {
        let p = paths_from_cli();
        let settings = load_settings_at(&p.app_data);
        match crate::dsh::check_update(&p, settings.registry.as_deref()) {
            Ok(Some((cur, latest))) => println!("UPDATE_AVAILABLE current={cur} latest={latest}"),
            Ok(None) => println!("UP_TO_DATE"),
            Err(e) => {
                eprintln!("CHECK_ERROR {e}");
                std::process::exit(1);
            }
        }
        std::process::exit(0);
    }
    if let Some(idx) = args.iter().position(|a| a == "--self-apply-update") {
        let ver = args.get(idx + 1).cloned().unwrap_or_default();
        let p = paths_from_cli();
        let settings = load_settings_at(&p.app_data);
        let reg_url = crate::registry::registry_url(settings.registry.as_deref());
        match crate::dsh::install_version(&p, &ver, &reg_url, &|_msg| {}) {
            Ok(()) => {
                println!("APPLIED {ver}");
                std::process::exit(0);
            }
            Err(e) => {
                eprintln!("APPLY_ERROR {e}");
                std::process::exit(1);
            }
        }
    }
    if args.iter().any(|a| a == "--self-uninstall-test") {
        let p = paths_from_cli();
        match uninstall_teardown(&p, false) {
            Ok(()) => {
                println!("UNINSTALL_DONE");
                std::process::exit(0);
            }
            Err(e) => {
                eprintln!("UNINSTALL_ERROR {e}");
                std::process::exit(1);
            }
        }
    }
    if args.iter().any(|a| a == "--self-uninstall-full") {
        // 唯一卸载链（Windows NSIS PREUNINSTALL 调用，无 GUI/无窗口）：
        //   1) 结束可能仍在运行的其它 App 实例（含其 node/dsh 子进程，树杀），
        //      释放程序目录 / 日志 / WebView2 数据文件锁 → 否则 NSIS 删文件必然失败
        //      （这正是"右键→卸载 无反应"的根因之一）。
        //   2) 复用 uninstall_teardown 做数据清理（app 数据、WebView2 缓存、可选 ~/.dsh）。
        //   3) **不删程序文件**（$INSTDIR 由 NSIS 负责删除），失败也不阻断 NSIS。
        let wipe = args.iter().any(|a| a == "--wipe");
        #[cfg(target_os = "windows")]
        kill_other_app_instances();
        let p = paths_from_cli();
        match uninstall_teardown(&p, wipe) {
            Ok(()) => {
                println!("UNINSTALL_DONE");
                std::process::exit(0);
            }
            Err(e) => {
                eprintln!("UNINSTALL_ERROR {e}");
                std::process::exit(1);
            }
        }
    }
    if args.iter().any(|a| a == "--self-trash-test") {
        println!("TRASHED {}", trash_self());
        std::process::exit(0);
    }
    false
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if run_cli_hooks(&args) {
        return;
    }
    if !acquire_single_instance() {
        // 已有实例在运行：把窗口带出来即可，自己不启动（防双托盘/双 dsh）
        #[cfg(target_os = "macos")]
        let _ = Command::new("osascript")
            .args(["-e", r#"tell application "DeepSeek Harness Desktop" to activate"#])
            .spawn();
        #[cfg(not(target_os = "macos"))]
        logln!("another instance is running; exiting");
        return;
    }

    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            plugin::plugin_op,
            plugin::plugin_list_cmd,
            appupdate::check_app_update_cmd,
            appupdate::app_update_cmd,
            uninstall_run,
            confirm_uninstall_cmd,
            modal_spec,
            modal_respond,
            get_dsh_url,
            get_shell_state,
            save_registry_cmd,
            open_browser_cmd,
            open_repo_cmd,
            get_dsh_state,
            update_dsh_cmd,
            setup_dsh_cmd,
            setup_state_cmd,
            setup_cancel_cmd,
            list_dsh_versions_cmd,
        ])
        .setup(|app| {
            // 托盘最小集：显示主窗口 / 退出（左键点击即显示主窗口；
            // 「管理台」与「显示主窗口」功能重复，已移除）。
            let show = MenuItem::with_id(app, "show", "显示主窗口", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show, &quit])?;
            let _tray = TrayIconBuilder::with_id("tray")
                .icon(app.default_window_icon().unwrap().clone())
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "quit" => {
                        kill_dsh();
                        app.exit(0);
                    }
                    // 显示主窗口（左键点击托盘图标同样走此路径；管理台已并入）
                    "show" => {
                        reveal_main_window(app, mlock(&DSH_URL).as_deref());
                    }
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        ..
                    } = event
                    {
                        let app = tray.app_handle();
                        reveal_main_window(app, mlock(&DSH_URL).as_deref());
                    }
                })
                .build(app)?;

            let handle = app.handle().clone();
            init_log(&paths_from_app(app.handle()));

            // 主菜单（macOS 菜单栏）：「关于」→ 唤起主窗口并切到关于页（信息与
            // App 内关于页一致的内容走 metadata（macOS 系统关于面板支持
            // name/version/copyright/credits，如图标与版权行 + 作者/仓库文本）。
            // 「退出」→ 与托盘一致（先停 dsh）；Windows/Linux 不设主菜单。
            #[cfg(target_os = "macos")]
            {
                // 原生 About：PredefinedMenuItem::about 点击直接弹系统关于面板
                // （不依赖菜单事件链——此前自定义项「点击没反应」的回归根因）。
                let about_item = PredefinedMenuItem::about(
                    app,
                    Some("关于 DeepSeek Harness Desktop"),
                    Some(AboutMetadata {
                        name: Some("DeepSeek Harness Desktop".into()),
                        version: Some(app.package_info().version.to_string()),
                        copyright: Some("© 2026 Jedeiah · MIT License".into()),
                        credits: Some(
                            "作者 Jedeiah\n项目主页 github.com/Jedeiah/dsh-desktop".into(),
                        ),
                        ..Default::default()
                    }),
                )?;
                let quit_item =
                    MenuItem::with_id(app, "menu-quit", "退出", true, Some("CmdOrCtrl+Q"))?;
                let app_menu = SubmenuBuilder::new(app, "DeepSeek Harness Desktop")
                    .item(&about_item)
                    .separator()
                    .item(&quit_item)
                    .build()?;
                let main_menu = Menu::with_items(app, &[&app_menu])?;
                app.set_menu(main_menu)?;
                app.on_menu_event(move |app, event| {
                    if event.id.as_ref() == "menu-quit" {
                        kill_dsh();
                        app.exit(0);
                    }
                });
            }
            // P3：主窗口 = 壳页（ui/shell.html）。壳页顶部是 Tab 栏（工作台/常规/
            // 网络/插件/更新/卸载），工作台 Tab 内用 <iframe> 内嵌 dsh 工作台；
            // 其余管理能力内嵌为面板（window.__TAURI__ 只注入主 frame → 远程 iframe
            // 拿不到 IPC，安全面收窄）。dsh 就绪后 reveal_main_window 用 dsh:url
            // 事件告诉壳页把 iframe src 指向工作台地址，而不是整窗导航。
            //
            // 启动顺序（消除"首帧默认位置跳变 / 再次居中上跳"）：
            //   visible(false)+center 隐藏创建 → dsh 就绪或开机 1.2s 宽限后，
            //   统一走 reveal_main_window 显示（其内部仅在不可见时 center+show）。
            let _ = WebviewWindowBuilder::new(
                app,
                WINDOW_LABEL,
                WebviewUrl::App("shell.html".into()),
            )
            .title("DeepSeek Harness Desktop")
            .inner_size(1280.0, 820.0)
            .min_inner_size(800.0, 560.0)
            .visible(false) // 隐藏创建 → 定位后由 reveal 显示，避免首帧位置跳变
            .center() // 主窗启动即居中于当前屏幕
            .theme(Some(tauri::Theme::Dark)) // B1：暗色原生标题栏一致化
            .on_page_load(|_webview, payload| {
                // 顶部帧页面加载记一行日志（壳页自身；dsh 在 iframe 内不在此触发）
                let url = payload.url().to_string();
                logln!("[webview] page loaded: {url}");
            })
            .on_navigation(webview_navigation_policy)
            .on_new_window(webview_new_window_policy)
            .build();
            // 开机宽限：dsh 就绪前先把带占位提示的窗口显示出来（约 1.2s），
            // dsh 就绪后再由 boot 用同一通道更新 iframe —— 两次只显示一次。
            let reveal_app = app.handle().clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(1200));
                reveal_main_window(&reveal_app, mlock(&DSH_URL).as_deref());
            });
            std::thread::spawn(move || boot(handle));
            // 启动时异步查一次 latest（替代旧 24h 定时检查）；离线/断网失败静默不打扰
            {
                let app = app.handle().clone();
                std::thread::spawn(move || {
                    let p = paths_from_app(&app);
                    let reg = crate::registry::registry_url(
                        load_settings_at(&p.app_data).registry.as_deref(),
                    );
                    if let Ok(v) = crate::registry::latest_version(&reg) {
                        *mlock(&LATEST_DSH) = Some(v);
                    }
                });
            }
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|_app_handle, event| match event {
            RunEvent::WindowEvent {
                label,
                event: WindowEvent::CloseRequested { api, .. },
                ..
            } if label == WINDOW_LABEL => {
                api.prevent_close();
                if let Some(w) = _app_handle.get_webview_window(WINDOW_LABEL) {
                    let _ = w.hide();
                }
            }
            // macOS: clicking the dock icon re-opens a hidden window.
            #[cfg(target_os = "macos")]
            RunEvent::Reopen { .. } => {
                reveal_main_window(_app_handle, mlock(&DSH_URL).as_deref());
            }
            RunEvent::ExitRequested { api, .. } => {
                // 卸载链进行中：阻止默认退出，等 teardown/回收站完成后再手动退出
                if UNINSTALLING.load(Ordering::SeqCst) {
                    api.prevent_exit();
                }
                kill_dsh();
            }
            RunEvent::Exit => kill_dsh(),
            _ => {}
        });
}

// ---------------------------------------------------------------------------
// 单元测试：插件管理纯函数
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_verbatim_prefix_logic() {
        assert_eq!(strip_verbatim_prefix(r"\\?\C:\foo\bar"), Some("C:\\foo\\bar".into()));
        assert_eq!(strip_verbatim_prefix(r"\\?\UNC\host\share\x"), Some(r"\\host\share\x".into()));
        assert_eq!(strip_verbatim_prefix(r"C:\foo"), None);
        assert_eq!(strip_verbatim_prefix(r"\\host\share\x"), None);
        assert_eq!(strip_verbatim_prefix(""), None);
    }
}
