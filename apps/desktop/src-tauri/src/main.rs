// DSh Desktop (DeepSeek Harness Desktop) — M5 launcher (macOS / Windows)
//
// Spawns the bundled node + dsh closure, parses the readiness URL from dsh's
// stdout (`dsh web: http://127.0.0.1:<port>`), and opens an embedded WebView.
//
// Windows specifics (vs macOS):
//   - node binary is resources/node/node.exe, home env is USERPROFILE
//   - open URL/folder via `cmd start` / `explorer`
//   - LAN IP parsed from `ipconfig`; LAN dialog + clipboard via rfd + arboard
//   - login autostart = HKCU\...\Run registry key; trash via the `trash` crate
//   - notifications via PowerShell NotifyIcon; kill LAN proxy via taskkill
//   - `current` version marker is a plain text file (no symlink privilege)
//
// Behaviour:
//   - closing the window hides it and keeps the app in the system tray
//   - quitting (Cmd+Q / tray Quit) kills the dsh child and exits
//   - workspace (dsh cwd) is user-selectable via a native picker and persisted
//   - unexpected dsh crashes auto-restart with exponential backoff
//   - updates: bundled resources are read-only; new closures install into the
//     app data dir via the bundled npm (M3), with registry config + auto-check
//   - logs: launcher + dsh output go to <app-data>/logs/ (dev keeps terminal)
//
// CLI test hooks (no GUI):
//   --self-update-check                 print update status and exit
//   --self-apply-update <version>       install+verify+switch, print, exit

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod update;

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tauri::menu::{CheckMenuItem, Menu, MenuItem};
use tauri::tray::{MouseButton, TrayIconBuilder, TrayIconEvent};
use tauri::{
    AppHandle, Manager, RunEvent, WebviewUrl, WebviewWindowBuilder, WindowEvent,
};
use tauri::Emitter;

/// The running dsh child, kept so it is reaped and so we can kill it on exit.
static CHILD: Mutex<Option<Child>> = Mutex::new(None);
/// The parsed base URL of the running dsh web server.
static DSH_URL: Mutex<Option<String>> = Mutex::new(None);
/// 插件操作串行锁：pnpm-workspace.yaml 的读-改-写与 dsh 的 reconcile 均非原子，
/// 并发触发（多窗口/远程）会撕裂文件；插件操作低频，全局串行化最简单可靠。
static PLUGIN_LOCK: Mutex<()> = Mutex::new(());
/// True when we intentionally stopped the child (quit/restart), so EOF is not
/// treated as a crash.
static INTENTIONAL_STOP: AtomicBool = AtomicBool::new(false);
/// Consecutive crash count, used for restart backoff.
static CRASHES: AtomicU32 = AtomicU32::new(0);
/// Mirrors whether the "Launch at Login" launch-agent is enabled.
static LOGIN_ON: AtomicBool = AtomicBool::new(false);
/// The tray's "Launch at Login" check item (kept to flip its state).
static LOGIN_ITEM: Mutex<Option<CheckMenuItem<tauri::Wry>>> = Mutex::new(None);
/// The running LAN proxy PID (bundled node + lan-proxy.js), if enabled.
/// The Child itself is owned by the watcher thread (reaps on exit); kills are
/// PID-based so a superseded watcher can never orphan a live proxy.
static LAN_CHILD: Mutex<Option<u32>> = Mutex::new(None);
/// Serializes LAN proxy start/kill so the menu handler and the dsh-readiness
/// thread cannot race (two concurrent starts would orphan the first proxy).
static LAN_MUTEX: Mutex<()> = Mutex::new(());
/// Mirrors whether LAN access is currently on.
static LAN_ON: AtomicBool = AtomicBool::new(false);
/// Generation counter for LAN proxy spawns: bumped on every kill/start so a
/// watcher of a superseded child can tell it no longer owns the seat and must
/// not restart (prevents double proxies during kill→start transitions).
static LAN_GEN: AtomicU32 = AtomicU32::new(0);
/// The tray's "局域网访问" check item.
static LAN_ITEM: Mutex<Option<CheckMenuItem<tauri::Wry>>> = Mutex::new(None);
/// 壳层增强①：caffeinate 子进程（macOS 阻止休眠；Child 本体在此，kill+wait 回收）。
/// Windows 侧无子进程（SetThreadExecutionState 直接调 API，随进程自动释放）。
#[cfg(target_os = "macos")]
static SLEEP_GUARD: Mutex<Option<Child>> = Mutex::new(None);
/// 壳层增强②：mDNS 通告子进程 PID（macOS: dns-sd；Windows: node mdns-advertise.js）。
/// Child 本体由看护线程持有并负责 reap（wait），与 LAN 代理同模式。
static MDNS_CHILD: Mutex<Option<u32>> = Mutex::new(None);
/// 壳层增强②：mDNS 通告代际计数器（bump 即令看护线程放弃重启，防 kill/start 竞态）。
static MDNS_GEN: AtomicU32 = AtomicU32::new(0);
/// 壳层增强②：mDNS 通告是否已成功启用（决定弹窗是否显示 .local 地址；子进程
/// 异常退出时看护线程会复位）。
static MDNS_ON: AtomicBool = AtomicBool::new(false);
/// 壳层增强③：上次记录的局域网 IP（轮询比较 / 日志用）。
static LAN_IP_LAST: Mutex<Option<String>> = Mutex::new(None);
/// 壳层增强③：IP 轮询线程代际计数器。bump 即令旧线程在下一轮醒来时自行退出；
/// 不用 JoinHandle+join（join 会被最长 30s 的 sleep 阻塞，违反"不阻塞 LAN 主流程"）。
static LAN_IP_WATCH_GEN: AtomicU32 = AtomicU32::new(0);
/// 壳层增强①（Windows）：电源守卫线程代际。SetThreadExecutionState 是**线程
/// 作用域**状态（设置线程退出即清除；别的线程也清不掉），故用专用常驻线程
/// 设置/清除；bump 即令守卫线程在 500ms 内于自身线程内清除并退出。
#[cfg(target_os = "windows")]
static POWER_GUARD_GEN: AtomicU32 = AtomicU32::new(0);
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
pub const APP_ID: &str = "com.dsh-desktop.app";
const RESTART_BASE_MS: u64 = 1000;
const RESTART_MAX_MS: u64 = 15000;
const WINDOW_LABEL: &str = "main";
const PLUGIN_LABEL: &str = "plugins";
const LAN_LABEL: &str = "lan";
const UNINSTALL_LABEL: &str = "uninstall";
const MODAL_LABEL: &str = "modal";

/// 用户主目录：优先平台主目录环境变量（unix: HOME，Windows: USERPROFILE），
/// 回退到系统用户目录（跨平台，不写死用户名）。
pub fn home_dir() -> PathBuf {
    let env = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(env)
        .map(PathBuf::from)
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Poison-safe mutex lock.
fn mlock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Single-instance guard via an exclusive lock file in the app data dir.
/// Returns true when THIS process acquired the lock; false when another
/// instance is running (caller should activate it and exit). The locked file
/// is deliberately leaked so the fd (and thus the lock) lives until exit.
fn acquire_single_instance() -> bool {
    let dir = update::app_data_from_home();
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
    let y = yoe as u64 + era as u64 * 400;
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
fn logln(msg: &str) {
    let stamp = hms(SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs());
    eprintln!("{msg}");
    if let Some(f) = mlock(&LOG_FILE).as_mut() {
        let _ = writeln!(f, "[{stamp}] {msg}");
    }
}

macro_rules! logln {
    ($($arg:tt)*) => { crate::logln(&format!($($arg)*)) };
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

/// Resources root for the GUI: dev cwd, packaged `Contents/Resources/resources`,
/// or prod `Contents/Resources`.
fn resource_dir(app: &AppHandle) -> PathBuf {
    let res = app.path().resource_dir().unwrap_or_default();
    for cand in [res.join("resources"), res] {
        if cand.join("dsh").is_dir() {
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
        Some(format!(r"\\{rest}"))
    } else if let Some(rest) = s.strip_prefix(r"\\?\") {
        Some(rest.to_string())
    } else {
        None
    }
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

fn paths_from_app(app: &AppHandle) -> Paths {
    let app_data = strip_verbatim(app.path().app_data_dir().unwrap_or_default());
    Paths {
        resources: resource_dir(app),
        app_data,
    }
}

/// Resources root for the CLI hooks (no Tauri handle): bundled resources
/// relative to the executable (macOS `../Resources/resources`, Windows
/// `resources` beside the exe), or dev cwd.
fn paths_from_cli() -> Paths {
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
            .find(|p| p.join("dsh").is_dir())
            .unwrap_or_else(|| cwd.join("resources")),
    );
    Paths {
        resources,
        app_data: update::app_data_from_home(),
    }
}

/// The bundled node executable (name differs per platform: `node` vs `node.exe`).
pub fn node_bin(resources: &Path) -> PathBuf {
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
fn apply_user_env(cmd: &mut Command) {
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
    /// dsh 进程默认工作目录（兜底 cwd）。旧键名 `workspace` 兼容读取。
    #[serde(alias = "workspace")]
    default_cwd: Option<PathBuf>,
    /// npm registry base URL override (e.g. https://registry.npmmirror.com)
    registry: Option<String>,
    /// 局域网访问（转发器）开关，默认关
    #[serde(default)]
    lan_enabled: bool,
    /// 局域网转发器监听端口（默认 3190）
    #[serde(default)]
    lan_port: Option<u16>,
    /// 局域网访问令牌（首次开启时生成）
    #[serde(default)]
    lan_token: Option<String>,
    /// 当前局域网配对码（每次开启重新生成；App 端二维码用它构造扫码链接）
    #[serde(default)]
    lan_pair: Option<String>,
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

fn save_settings_at(app_data: &Path, s: &Settings) {
    let p = settings_path_from_data(app_data);
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(raw) = serde_json::to_string_pretty(s) {
        // atomic write: temp file + rename, so a crash never leaves a corrupt
        // settings.json (a corrupt file silently resets all preferences).
        let tmp = p.with_extension("json.tmp");
        if std::fs::write(&tmp, raw).is_ok() {
            let _ = std::fs::rename(&tmp, &p);
        }
    }
}

fn load_settings(app: &AppHandle) -> Settings {
    load_settings_at(&paths_from_app(app).app_data)
}

fn save_settings(app: &AppHandle, s: &Settings) {
    save_settings_at(&paths_from_app(app).app_data, s)
}

fn workspace_dir(app: &AppHandle) -> PathBuf {
    strip_verbatim(
        load_settings(app)
            .default_cwd
            .filter(|p| p.is_dir())
            .unwrap_or_else(home_dir),
    )
}

// ---------------------------------------------------------------------------
// Closure resolution
// ---------------------------------------------------------------------------

fn closure_marker(dir: &Path) -> bool {
    dir.join("node_modules/@deepseek-ai/dsh/package.json").is_file()
}

/// Resolve the active closure under a `dsh/` dir using the `current` marker.
/// The marker is a plain text file holding the version directory name; older
/// installs that used a `current` symlink to a version dir are also accepted.
fn resolve_current(dsh_dir: &Path) -> Option<PathBuf> {
    let marker = dsh_dir.join("current");
    if let Ok(ver) = std::fs::read_to_string(&marker) {
        let ver = ver.trim();
        if !ver.is_empty() {
            let dir = dsh_dir.join(ver);
            if closure_marker(&dir) {
                return Some(dir);
            }
        }
    }
    // legacy: `current` was a symlink pointing at a version dir
    if closure_marker(&marker) {
        return Some(marker);
    }
    None
}

/// Resolve the active dsh closure: prefer the app-data `current` (managed by
/// updates), otherwise the bundled one.
pub fn active_closure(p: &Paths) -> Option<PathBuf> {
    resolve_current(&p.app_data.join("dsh")).or_else(|| resolve_closure(&p.resources))
}

/// Scan the (read-only) bundled `dsh/` for the active version dir.
fn resolve_closure(res: &Path) -> Option<PathBuf> {
    let dsh = res.join("dsh");
    if let Some(c) = resolve_current(&dsh) {
        return Some(c);
    }
    if let Ok(entries) = std::fs::read_dir(&dsh) {
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() && closure_marker(&p) {
                return Some(p);
            }
        }
    }
    None
}

fn spawn_dsh(app: &AppHandle) -> std::io::Result<Child> {
    let p = paths_from_app(app);
    let node = node_bin(&p.resources);
    let closure = active_closure(&p).ok_or_else(|| {
        std::io::Error::other(format!("dsh closure not found under {}", p.resources.display()))
    })?;
    let bin = closure.join("node_modules/@deepseek-ai/dsh/lib/bin.js");
    let cwd = workspace_dir(app);
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
        // native OS dialogs open on the host machine and cannot serve phone/
        // LAN clients, whereas the web picker works everywhere.
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

fn boot(app: AppHandle) {
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
                "DeepSeek Harness 启动失败",
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
                        // keep the LAN proxy pointed at the current dsh port
                        if let Some(port) = url.rsplit(':').next().map(|s| s.to_string()) {
                            ensure_lan_on_ready(&app, &port);
                        }
                        let app2 = app.clone();
                        let u = url.clone();
                        let _ = app2.clone().run_on_main_thread(move || {
                            show_window(&app2, &u);
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
                "DeepSeek Harness 运行异常",
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

fn show_window(app: &AppHandle, url: &str) {
    let parsed = match url.parse::<tauri::Url>() {
        Ok(u) => u,
        Err(_) => return,
    };
    if let Some(w) = app.get_webview_window(WINDOW_LABEL) {
        // only navigate when the URL actually changed (启动页 → dsh 工作台，
        // 或 dsh 重启换端口)；同 URL 点击只显示/聚焦，避免整页重载闪烁。
        let need_navigate = match w.url() {
            Ok(cur) => cur.as_str() != parsed.as_str(),
            Err(_) => true,
        };
        if need_navigate {
            let _ = w.navigate(parsed);
        }
        let _ = w.center(); // 每次打开/显示都居中于当前屏幕
        let _ = w.show();
        let _ = w.set_focus();
        return;
    }
    // 兜底：窗口不存在（极罕见）时重建（探针在 setup 的窗口 builder 里已挂）
    let _ = WebviewWindowBuilder::new(app, WINDOW_LABEL, WebviewUrl::External(parsed))
        .title("DeepSeek Harness")
        .inner_size(1280.0, 820.0)
        .min_inner_size(800.0, 560.0)
        .center()
        .theme(Some(tauri::Theme::Dark))
        .on_navigation(webview_navigation_policy)
        .on_new_window(webview_new_window_policy)
        .build();
}

/// Inject a one-shot error trap + probe into the webview and log the result.
fn probe_webview(w: &tauri::WebviewWindow, label: String, tag: String) {
    // trap JS errors into document.title so a later probe can read them
    let _ = w.eval(
        r#"window.addEventListener('error', e => { try { document.title = 'JSERR:' + String(e.message||e.error||'').slice(0,200); } catch(_){} });"#,
    );
    let label2 = label.clone();
    let _ = w.eval_with_callback(
        r#"JSON.stringify({ready:document.readyState,title:document.title,bodyLen:document.body?document.body.innerText.length:-1,bodyHead:document.body?document.body.innerText.slice(0,160):''})"#,
        move |res| logln!("[webview:{label2}] probe({tag}): {res}"),
    );
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

/// Open a folder in the platform file manager.
fn open_dir(dir: &Path) {
    #[cfg(target_os = "windows")]
    let _ = no_console(Command::new("explorer")).arg(dir).spawn();
    #[cfg(not(target_os = "windows"))]
    let _ = Command::new("open").arg(dir).spawn();
}

/// 是否为允许在 WebView 内导航的地址：App 内置页（tauri://localhost /
/// http(s)://tauri.localhost）与 **dsh 工作台源端口**（http://127.0.0.1:<dsh
/// 实际端口>）。只放行"当前 dsh 端口"，避免 LAN 代理端口（3190 等）在 App 内
/// 打开（应走系统浏览器）。未就绪（DSH_URL 空）或 host 不符时一律放行到外部。
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
/// 导航触发，iframe 不受影响。data:/blob:/about:/javascript: 一律放行以免
/// 破坏内嵌内容（如 srcdoc 预览）。
fn webview_navigation_policy(url: &tauri::Url) -> bool {
    if is_internal_webview_url(url) {
        return true;
    }
    let scheme = url.scheme();
    if matches!(scheme, "data" | "blob" | "about" | "javascript") {
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

fn open_in_browser(_app: &AppHandle) {
    let url = mlock(&DSH_URL)
        .clone()
        .unwrap_or_else(|| "http://127.0.0.1:3080".into());
    open_url(&url);
}

fn kill_dsh() {
    // mark as intentional so the boot thread never treats the EOF as a crash
    // and never spawns a restart after the app is quitting (avoids orphans).
    INTENTIONAL_STOP.store(true, Ordering::SeqCst);
    kill_lan();
    if let Some(mut c) = mlock(&CHILD).take() {
        let _ = c.kill();
        let _ = c.wait();
    }
}

fn restart_dsh(app: &AppHandle) {
    INTENTIONAL_STOP.store(true, Ordering::SeqCst);
    kill_dsh();
    if let Some(w) = app.get_webview_window(WINDOW_LABEL) {
        let _ = w.close();
    }
    let handle = app.clone();
    std::thread::spawn(move || boot(handle));
}

fn choose_workspace(app: &AppHandle) {
    let current = workspace_dir(app);
    if let Some(dir) = rfd::FileDialog::new()
        .set_title("选择 DSh 工作文件夹")
        .set_directory(&current)
        .pick_folder()
    {
        let mut s = load_settings(app);
        s.default_cwd = Some(dir.clone());
        save_settings(app, &s);
        logln!("workspace set to {}", dir.display());
        restart_dsh(app);
    }
}

fn open_workspace_in_finder(app: &AppHandle) {
    let dir = workspace_dir(app);
    open_dir(&dir);
}

// ---------------------------------------------------------------------------
// Plugin management（托盘「插件管理…」窗口的后端）
//
// `dsh plugin --profile web <op> <pkg>` = 在 ~/.dsh/profiles/web 里转发给
// pnpm（dsh 闭包写死 spawnSync("pnpm") 从 PATH 找，见 plugin-9h8shc4d.js）。
// 本实现：
//   - 用内置 node 调内置 dsh bin.js，PATH 前置 resources/pnpm-bin（打包的
//     pnpm JS 发行版 + shim，见 prepare-resources.sh/ps1），用户无需装 pnpm；
//   - 安装前确保 profile 的 pnpm-workspace.yaml 含 allowBuilds（pnpm 11 构建
//     脚本门禁）与 minimumReleaseAge: 0（新包发布年龄门禁），并在输出出现
//     ERR_PNPM_IGNORED_BUILDS 时自动补写缺失包名重试；
//   - 尊重 settings.registry（npm registry 覆盖）。
// ---------------------------------------------------------------------------

/// npm 包名宽松校验：仅字母数字与 @ . _ - / ~，不以 . / _ / - 开头（npm 规则 +
/// 防 pnpm 参数混淆：`-g`、`--dir=` 等以 - 开头的 token 会被 pnpm 当选项），
/// 防参数注入/路径穿越。
fn valid_pkg_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 214
        && !s.starts_with('.')
        && !s.starts_with('_')
        && !s.starts_with('-')
        && s.bytes().all(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b'@' | b'.' | b'_' | b'-' | b'/' | b'~')
        })
}

/// 内置 pnpm 可执行文件名（prepare-resources 打包：macOS/Linux 产出 `pnpm`
/// shim，Windows 产出 `pnpm.cmd`——与 Rust 侧存在性检查必须保持一致）。
fn bundled_pnpm_file_name() -> &'static str {
    if cfg!(windows) {
        "pnpm.cmd"
    } else {
        "pnpm"
    }
}

/// 在命令 PATH 最前面插入 dir（读取 cmd 已设置的环境，兼容 apply_user_env 的覆盖）。
fn prepend_path(cmd: &mut Command, dir: &std::path::Path) {
    let mut cur: Option<std::ffi::OsString> = None;
    for (k, v) in cmd.get_envs() {
        if k == "PATH" {
            cur = v.map(|x| x.to_os_string());
        }
    }
    let cur = cur.unwrap_or_else(|| std::env::var_os("PATH").unwrap_or_default());
    let sep = if cfg!(windows) { ";" } else { ":" };
    let mut joined = dir.as_os_str().to_os_string();
    joined.push(sep);
    joined.push(&cur);
    cmd.env("PATH", joined);
}

/// 确保 profile 的 pnpm-workspace.yaml 包含 pnpm 11 门禁配置：
/// allowBuilds（默认已知构建脚本包 + extra_builds）与 minimumReleaseAge: 0。
/// 文件不存在时按 dsh initProfile 模板创建（dsh 检测到缺 package.json 仍会
/// initProfile，且"已有文件不覆盖"，故预写内容会被保留）。
fn ensure_pnpm_workspace(profile: &std::path::Path, extra_builds: &[String]) -> std::io::Result<()> {
    std::fs::create_dir_all(profile)?;
    let yaml_path = profile.join("pnpm-workspace.yaml");
    let mut lines: Vec<String> = if yaml_path.exists() {
        std::fs::read_to_string(&yaml_path)?
            .lines()
            .map(String::from)
            .collect()
    } else {
        vec![
            "packages:".into(),
            "  - .".into(),
            "nodeLinker: hoisted".into(),
            "autoInstallPeers: false".into(),
        ]
    };
    let mut changed = false;

    let mut builds: Vec<String> = vec![
        "cloudflared".into(),
        "ssh2".into(),
        "cpu-features".into(),
    ];
    for b in extra_builds {
        if !builds.contains(b) {
            builds.push(b.clone());
        }
    }
    let allow_idx = lines
        .iter()
        .position(|l| l.trim_end() == "allowBuilds:" || l.starts_with("allowBuilds:"));
    match allow_idx {
        Some(idx) => {
            let mut end = idx + 1;
            while end < lines.len() && lines[end].starts_with("  ") {
                end += 1;
            }
            let existing: Vec<String> = lines[idx + 1..end]
                .iter()
                .filter_map(|l| l.trim_start().split(':').next().map(|s| s.trim().to_string()))
                .collect();
            for b in &builds {
                if !existing.contains(b) {
                    lines.insert(end, format!("  {b}: true"));
                    end += 1;
                    changed = true;
                }
            }
        }
        None => {
            lines.push("allowBuilds:".into());
            for b in &builds {
                lines.push(format!("  {b}: true"));
            }
            changed = true;
        }
    }
    if !lines.iter().any(|l| l.starts_with("minimumReleaseAge:")) {
        lines.push("minimumReleaseAge: 0".into());
        changed = true;
    }
    if changed {
        std::fs::write(&yaml_path, lines.join("\n") + "\n")?;
    }
    Ok(())
}

/// 从 pnpm 输出提取 "Ignored build scripts: a, b" 中的包名（自动补 allowBuilds 用）。
/// 取 "build scripts:" 后到第一个英文句号之间的逗号分隔片段，避免把错误消息
/// 里的普通单词（Run/pnpm/approve-builds…）误当包名。
fn extract_pkg_names(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in text.lines() {
        if let Some(idx) = line.find("build scripts:") {
            let rest = &line[idx + "build scripts:".len()..];
            let end = rest.find('.').unwrap_or(rest.len());
            for tok in rest[..end].split(',') {
                let t = tok.trim().trim_matches('"').trim();
                if valid_pkg_name(t) && !out.iter().any(|x| x == t) {
                    out.push(t.to_string());
                }
            }
        }
    }
    out
}

/// 保留输出尾部 max 字符（输出可能很大，尾部最有价值；按 char 安全截断）。
fn tail_text(s: &str, max: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max {
        s.to_string()
    } else {
        let kept: String = chars[chars.len() - max..].iter().collect();
        format!("…（输出过长，截断前 {} 字符）\n{kept}", chars.len())
    }
}

/// 递归删除空目录（自叶子向上；非空目录 remove_dir 失败被忽略）。
/// pnpm remove 会留下 0B 的空目录骨架（如 @scope 残留），卸载后清扫。
fn remove_empty_tree(dir: &std::path::Path) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                remove_empty_tree(&e.path());
            }
        }
    }
    let _ = std::fs::remove_dir(dir); // 非空（或占用）时静默忽略
}

/// 清扫 node_modules 下的空目录（不动 node_modules 本身）。
fn clean_empty_dirs_under(root: &std::path::Path) {
    if let Ok(entries) = std::fs::read_dir(root) {
        for e in entries.flatten() {
            let p = e.path();
            if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                remove_empty_tree(&p);
            }
        }
    }
}

/// 执行一次 `dsh plugin --profile web <op> <pkg>`，返回（合并输出, 是否成功）。
fn run_dsh_plugin(
    app: &AppHandle,
    op: &str,
    pkg: &str,
    extra_builds: &[String],
) -> Result<(String, bool), String> {
    let p = paths_from_app(app);
    let node = node_bin(&p.resources);
    let closure = active_closure(&p)
        .ok_or_else(|| format!("dsh 闭包未找到：{}", p.resources.display()))?;
    let bin = closure.join("node_modules/@deepseek-ai/dsh/lib/bin.js");
    let home = home_dir();
    let profile = home.join(".dsh/profiles/web");
    // 内置 pnpm（JS 发行版 + shim，由 prepare-resources 打包：macOS 产出
    // pnpm shim，Windows 产出 pnpm.cmd）；缺失时给明确错误
    let pnpm_bin = p.resources.join("pnpm-bin").join(bundled_pnpm_file_name());
    if !pnpm_bin.exists() {
        return Err(format!(
            "内置 pnpm 缺失：{}\n请重新安装 App（或自行安装 pnpm 后重试）",
            pnpm_bin.display()
        ));
    }
    // 插件操作串行化（ensure 的读-改-写与 dsh reconcile 非原子）
    let _guard = mlock(&PLUGIN_LOCK);
    ensure_pnpm_workspace(&profile, extra_builds)
        .map_err(|e| format!("写入 pnpm-workspace.yaml 失败：{e}"))?;

    let mut cmd = Command::new(&node);
    #[cfg(target_os = "windows")]
    {
        cmd = no_console(cmd); // node.exe 是控制台程序，避免弹出控制台窗口
    }
    // 用户环境合并（macOS）：PATH 里可能有用户自己的 pnpm；随后 prepend 内置 pnpm-bin
    #[cfg(target_os = "macos")]
    apply_user_env(&mut cmd);
    // 内置 pnpm 优先于系统 pnpm，行为可预期
    prepend_path(&mut cmd, &p.resources.join("pnpm-bin"));
    // 尊重用户设置的 npm registry 覆盖
    if let Some(reg) = load_settings(app).registry {
        cmd.env("npm_config_registry", reg);
    }
    cmd.arg(&bin)
        .arg("plugin")
        .arg("--profile")
        .arg("web")
        .arg(op)
        .arg(pkg)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if cfg!(windows) {
        cmd.env("USERPROFILE", &home).env("HOME", &home);
    } else {
        cmd.env("HOME", &home);
    }
    // 逐行读取 stdout/stderr：实时 emit 到前端（plugin-output 事件）并收集全文。
    // pnpm 可能运行数分钟，一次性 output() 会让前端长时间只有静态"正在…"。
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("执行 dsh plugin 失败：{e}"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "无法读取 dsh plugin stdout".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "无法读取 dsh plugin stderr".to_string())?;
    let emit_app = app.clone();
    let out_thread = std::thread::spawn(move || {
        let reader = BufReader::new(stdout);
        let mut buf = String::new();
        for line in reader.lines() {
            let Ok(l) = line else { break };
            let l = l.trim_end_matches('\r').to_string();
            buf.push_str(&l);
            buf.push('\n');
            // 收敛到插件管理窗口（避免广播到全部窗口；窗口已销毁则仅收集）
            if let Some(w) = emit_app.get_webview_window(PLUGIN_LABEL) {
                let _ = w.emit("dsh:plugin-output", &l);
            }
        }
        buf
    });
    let emit_app2 = app.clone();
    let err_thread = std::thread::spawn(move || {
        let reader = BufReader::new(stderr);
        let mut buf = String::new();
        for line in reader.lines() {
            let Ok(l) = line else { break };
            let l = l.trim_end_matches('\r').to_string();
            buf.push_str(&l);
            buf.push('\n');
            if let Some(w) = emit_app2.get_webview_window(PLUGIN_LABEL) {
                let _ = w.emit("dsh:plugin-output", &l);
            }
        }
        buf
    });
    let status = match child.wait() {
        Ok(st) => st,
        Err(e) => {
            // wait 失败（罕见）：先回收读线程再返回，避免瞬时线程泄漏
            let _ = out_thread.join();
            let _ = err_thread.join();
            return Err(format!("等待 dsh plugin 退出失败：{e}"));
        }
    };
    let success = status.success();
    let mut text = out_thread.join().unwrap_or_default();
    text.push_str(&err_thread.join().unwrap_or_default());
    // pnpm remove 会留下 0B 空目录骨架：卸载（无论成败）后清扫一次
    if op == "remove" {
        clean_empty_dirs_under(&profile.join("node_modules"));
    }
    logln!(
        "[plugin] dsh plugin {op} {pkg} -> exit {}",
        status
    );
    Ok((format!("退出码 {}\n\n{}", status, tail_text(&text, 60000)), success))
}

/// 前端插件管理窗口的 command：安装(add)/卸载(remove) 插件。
/// pnpm 11 构建脚本门禁失败时自动补写 allowBuilds 并重试（最多 2 轮）。
/// 最终仍失败时返回 Err（输出为错误信息），前端按失败态展示。
///
/// 安全：仅接受来自「插件管理」窗口（label == PLUGIN_LABEL）的调用。远程工作台
/// 页面（http://127.0.0.1，含第三方插件 bundle）也能拿到 window.__TAURI__
/// （withGlobalTauri），本校验把 plugin_op 的授权面收回到专用窗口，防止远程
/// 内容诱导安装任意 npm 包并执行其构建脚本。
#[tauri::command]
async fn plugin_op(
    app: AppHandle,
    window: tauri::WebviewWindow,
    op: String,
    pkg: String,
) -> Result<String, String> {
    if window.label() != PLUGIN_LABEL {
        return Err("该操作仅限插件管理窗口使用".to_string());
    }
    if op != "add" && op != "remove" {
        return Err(format!("不支持的插件操作：{op}（仅支持 add / remove）"));
    }
    if !valid_pkg_name(&pkg) {
        return Err("包名不合法（仅允许字母、数字与 @ . _ - / ~，且不能以 - 开头）".to_string());
    }
    // pnpm 可能运行数分钟：移到阻塞线程池执行，避免占用 Tauri 主线程
    // （否则安装期间 App UI / 托盘冻结）。PLUGIN_LOCK 在阻塞线程内获取释放。
    tauri::async_runtime::spawn_blocking(move || {
        let (mut output, mut success) = run_dsh_plugin(&app, &op, &pkg, &[])?;
        for _ in 0..2 {
            if success {
                break;
            }
            let hits_ignored = output.contains("ERR_PNPM_IGNORED_BUILDS")
                || output.to_lowercase().contains("ignored build scripts");
            if !hits_ignored {
                break;
            }
            let extra = extract_pkg_names(&output);
            if extra.is_empty() {
                break;
            }
            logln!("[plugin] auto-approving build scripts: {extra:?}");
            (output, success) = run_dsh_plugin(&app, &op, &pkg, &extra)?;
        }
        if success {
            Ok(output)
        } else {
            Err(output)
        }
    })
    .await
    .map_err(|e| format!("插件操作线程异常：{e}"))?
}

/// 打开（或聚焦）插件管理窗口。
fn open_plugin_manager(app: &AppHandle) {
    if let Some(w) = app.get_webview_window(PLUGIN_LABEL) {
        let _ = w.show();
        let _ = w.set_focus();
        return;
    }
    match WebviewWindowBuilder::new(app, PLUGIN_LABEL, WebviewUrl::App("plugins.html".into()))
        .decorations(false) // 无系统标题栏/放大缩小关闭按钮
        .resizable(false)
        .visible(false) // 定位后再显示，避免原始位置闪烁
        .center() // 兜底：主窗口中心定位前先居中
        .inner_size(560.0, 480.0)
        .on_navigation(webview_navigation_policy)
        .on_new_window(webview_new_window_policy)
        .build()
    {
        Ok(w) => {
            center_child_on_main(app, &w, 560.0, 480.0);
            let _ = w.show();
            let _ = w.set_focus();
            logln!("[plugins] window opened");
        }
        Err(e) => logln!("[plugins] failed to open window: {e}"),
    }
}

/// 打开（或聚焦）「扫码远程连接」窗口。
fn open_lan_window(app: &AppHandle) {
    if let Some(w) = app.get_webview_window(LAN_LABEL) {
        let _ = w.show();
        let _ = w.set_focus();
        return;
    }
    match WebviewWindowBuilder::new(app, LAN_LABEL, WebviewUrl::App("lan.html".into()))
        .decorations(false) // 无系统标题栏/放大缩小关闭按钮
        .resizable(false)
        .visible(false) // 定位后再显示，避免原始位置闪烁
        .center() // 兜底
        .inner_size(480.0, 560.0)
        .on_navigation(webview_navigation_policy)
        .on_new_window(webview_new_window_policy)
        .build()
    {
        Ok(w) => {
            center_child_on_main(app, &w, 480.0, 560.0);
            let _ = w.show();
            let _ = w.set_focus();
            logln!("[lan] window opened");
        }
        Err(e) => logln!("[lan] failed to open window: {e}"),
    }
}

/// 打开（或聚焦）卸载确认窗口。
fn open_uninstall_window(app: &AppHandle) {
    if let Some(w) = app.get_webview_window(UNINSTALL_LABEL) {
        let _ = w.show();
        let _ = w.set_focus();
        return;
    }
    match WebviewWindowBuilder::new(app, UNINSTALL_LABEL, WebviewUrl::App("uninstall.html".into()))
        .decorations(false) // 无系统标题栏/放大缩小关闭按钮
        .resizable(false)
        .visible(false) // 定位后再显示，避免原始位置闪烁
        .center() // 兜底
        .inner_size(460.0, 320.0)
        .on_navigation(webview_navigation_policy)
        .on_new_window(webview_new_window_policy)
        .build()
    {
        Ok(w) => {
            center_child_on_main(app, &w, 460.0, 320.0);
            let _ = w.show();
            let _ = w.set_focus();
            logln!("[uninstall] window opened");
        }
        Err(e) => logln!("[uninstall] failed to open window: {e}"),
    }
}

// ---------------------------------------------------------------------------
// 自绘弹窗（modal.html）：替代 rfd 系统对话框，统一玻璃卡片风格、可居中。
// ---------------------------------------------------------------------------

/// 自绘弹窗内容。kind: "ok"（单按钮确定）| "yesno"（稍后/确定）。
#[derive(Clone, Serialize)]
struct ModalSpec {
    title: String,
    message: String,
    kind: String,
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

/// 卸载确认窗口返回的状态。
#[derive(Serialize)]
struct LanState {
    enabled: bool,
    ip: String,
    port: u16,
    token: String,
    pair: Option<String>,
    qr_url: Option<String>,
}

/// 「扫码远程连接」窗口：读取当前局域网状态（开关/地址/令牌/二维码链接）。
#[tauri::command]
fn lan_state(app: AppHandle, window: tauri::WebviewWindow) -> Result<LanState, String> {
    if window.label() != LAN_LABEL {
        return Err("该操作仅限扫码远程连接窗口使用".to_string());
    }
    let s = load_settings(&app);
    let token = s.lan_token.unwrap_or_default();
    let pair = s.lan_pair.clone();
    let port = s.lan_port.unwrap_or(LAN_DEFAULT_PORT);
    let ip = lan_ip().unwrap_or_else(|| "127.0.0.1".into());
    let enabled = LAN_ON.load(Ordering::SeqCst);
    let qr_url = if enabled {
        pair.as_ref().map(|p| format!("http://{ip}:{port}/?pair={p}"))
    } else {
        None
    };
    Ok(LanState {
        enabled,
        ip,
        port,
        token,
        pair,
        qr_url,
    })
}

/// 「扫码远程连接」窗口：切换局域网访问开关（复用 set_lan_access 逻辑）。
#[tauri::command]
fn lan_toggle(app: AppHandle, window: tauri::WebviewWindow, enable: bool) -> Result<(), String> {
    if window.label() != LAN_LABEL {
        return Err("该操作仅限扫码远程连接窗口使用".to_string());
    }
    set_lan_access(&app, enable);
    Ok(())
}

/// 卸载确认窗口：执行卸载（wipe=true 连 ~/.dsh 一起删）。
/// 完整流程：销毁 WebView（释放 WebView2 数据占用）→ teardown → 移入回收站 → 退出。
#[tauri::command]
async fn uninstall_run(app: AppHandle, wipe: bool) -> Result<(), String> {
    // 先销毁全部 WebView 窗口：释放 WebView2 用户数据目录（app_data 内）占用，
    // Windows 共享锁下不销毁则删除必然 oserror 32。
    for label in [WINDOW_LABEL, PLUGIN_LABEL, LAN_LABEL, UNINSTALL_LABEL] {
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
        // 卸载确认窗口已销毁，JS 无法回显：用系统通知兜底
        update::notify("卸载未完成", &e);
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
            update::notify(
                "请通过系统卸载",
                "应用数据已清理。请到“设置 → 应用 → 已安装的应用”中卸载 DeepSeek Harness。",
            );
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        if !trash_self() {
            update::notify(
                "请通过系统卸载",
                "应用数据已清理。请通过系统卸载 DeepSeek Harness。",
            );
        }
    }
    app.exit(0);
    Ok(())
}

// ---------------------------------------------------------------------------
// Update flows (GUI)
// ---------------------------------------------------------------------------

/// Manual check: on finding an update, ask, then apply in the background.
fn check_for_updates(app: &AppHandle) {
    let app2 = app.clone();
    std::thread::spawn(move || {
        let p = paths_from_app(&app2);
        let settings = load_settings(&app2);
        match update::check_update(&p, settings.registry.as_deref()) {
            Ok(Some((cur, latest))) => {
                let msg = format!("当前 v{cur}，发现新版本 v{latest}。是否现在更新？");
                let yes = show_modal(&app2, "DeepSeek Harness 更新", &msg, "yesno");
                if yes {
                    let app3 = app2.clone();
                    let p2 = p.clone();
                    let reg = settings.registry.clone();
                    std::thread::spawn(move || {
                        let reg_url = update::registry_url(reg.as_deref());
                        match update::apply_update(&p2, &latest, &reg_url) {
                            Ok(()) => {
                                update::notify("更新完成", &format!("已升级到 v{latest}，正在重启 dsh"));
                                restart_dsh(&app3);
                            }
                            Err(e) => update::notify("更新失败", &e),
                        }
                    });
                }
            }
            Ok(None) => update::notify("检查更新", "已是最新版本"),
            Err(e) => update::notify("检查更新失败", &e),
        }
    });
}

/// Silent periodic check: notify only (no dialog); the user updates via the menu.
fn periodic_check(app: AppHandle) {
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(6 * 3600));
        loop {
            let p = paths_from_app(&app);
            let settings = load_settings(&app);
            match update::check_update(&p, settings.registry.as_deref()) {
                Ok(Some((_, latest))) => update::notify(
                    "有可用更新",
                    &format!("DeepSeek Harness v{latest} 已发布，从托盘菜单更新"),
                ),
                _ => {}
            }
            std::thread::sleep(Duration::from_secs(24 * 3600));
        }
    });
}

// ---------------------------------------------------------------------------
// LAN access (auth proxy) — M6
// ---------------------------------------------------------------------------

/// The LAN proxy's fixed default listen port (avoids clashing with 3080).
const LAN_DEFAULT_PORT: u16 = 3190;

/// Get (or lazily generate) the LAN access token; persists via the caller's save.
fn lan_token(s: &mut Settings) -> String {    if let Some(t) = &s.lan_token {
        if !t.is_empty() {
            return t.clone();
        }
    }
    // 128-bit CSPRNG token（跨平台；getrandom 是本依赖树成员，不再依赖 /dev/urandom）。
    let mut buf = [0u8; 16];
    if getrandom::getrandom(&mut buf).is_err() {
        // 极端兜底：系统熵源不可用时退化为主机时间戳（远比全 0 安全）。
        let t = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u128;
        buf = t.to_le_bytes();
    }
    let t: String = buf.iter().map(|b| format!("{b:02x}")).collect();
    s.lan_token = Some(t.clone());
    t
}

/// 生成局域网配对码（64 位 hex，供 lan-proxy 免输登录与 App 端二维码）。
/// lan-proxy 校验 64-hex 格式：熵源不可用时用两次时间采样填满 32 字节兜底。
fn new_pair_code() -> String {
    let mut buf = [0u8; 32];
    if getrandom::getrandom(&mut buf).is_err() {
        let now = || {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos() as u128
        };
        let (a, b) = (now(), now());
        buf[0..16].copy_from_slice(&a.to_le_bytes());
        buf[16..32].copy_from_slice(&b.to_le_bytes());
    }
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

/// Detect the primary LAN IPv4 (best effort; platform-specific mechanism).
fn lan_ip() -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        for iface in ["en0", "en1"] {
            if let Ok(out) = Command::new("ipconfig").args(["getifaddr", iface]).output() {
                let ip = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if !ip.is_empty() && !ip.starts_with("127.") {
                    return Some(ip);
                }
            }
        }
        None
    }
    #[cfg(target_os = "windows")]
    {
        // 解析 `ipconfig` 输出（中/英文系统都能用），取第一个私网 IPv4。
        let out = no_console(Command::new("ipconfig")).output().ok()?;
        let text = String::from_utf8_lossy(&out.stdout);
        for line in text.lines() {
            if let Some(ip) = first_ipv4(line) {
                let parts: Vec<u8> = ip.split('.').filter_map(|s| s.parse().ok()).collect();
                if let [a, b, ..] = parts[..] {
                    if a == 10 || (a == 172 && (16..=31).contains(&b)) || (a == 192 && b == 168) {
                        return Some(ip);
                    }
                }
            }
        }
        None
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        None
    }
}

#[cfg(target_os = "windows")]
/// Return the first dotted-quad IPv4 found in a line, if any.
fn first_ipv4(line: &str) -> Option<String> {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.') {
                i += 1;
            }
            let tok = &line[start..i];
            if tok.matches('.').count() == 3 {
                let parts: Vec<u8> = tok.split('.').filter_map(|s| s.parse().ok()).collect();
                if parts.len() == 4 {
                    return Some(tok.to_string());
                }
            }
        } else {
            i += 1;
        }
    }
    None
}

/// Stop the LAN proxy if running.
fn kill_lan() {
    let _g = mlock(&LAN_MUTEX);
    kill_lan_unlocked();
}

fn kill_lan_unlocked() {
    LAN_GEN.fetch_add(1, Ordering::SeqCst); // invalidate any running watcher
    LAN_ON.store(false, Ordering::SeqCst);
    // 按 PID 结束代理：Child 本体由看护线程持有并负责 reap（wait）。
    if let Some(pid) = mlock(&LAN_CHILD).take() {
        #[cfg(unix)]
        unsafe {
            libc::kill(pid as libc::pid_t, libc::SIGTERM);
        }
        #[cfg(windows)]
        {
            // Windows 无 SIGTERM 概念：taskkill 强制结束（含其子进程树）。
            let _ = no_console(Command::new("taskkill"))
                .arg("/PID")
                .arg(pid.to_string())
                .arg("/T")
                .arg("/F")
                .status();
        }
    }
    // ---- LAN 壳层增强：随 LAN 停止（全部幂等）----
    stop_sleep_guard(); // ① 释放阻止休眠（进程级 API / caffeinate 退出即释放断言）
    kill_mdns();        // ② 停止 mDNS 通告（进程退出即撤销注册）
    LAN_IP_WATCH_GEN.fetch_add(1, Ordering::SeqCst); // ③ 停止 IP 轮询（旧线程下一轮退出）
    *mlock(&LAN_IP_LAST) = None; // ③ 清上次记录，下次开启重新基线
}

/// Drain a child pipe into the launcher log (so proxy stdout/stderr is visible).
fn spawn_pipe_logger<R: std::io::Read + Send + 'static>(reader: R, tag: &'static str) {
    std::thread::spawn(move || {
        let reader = BufReader::new(reader);
        for line in reader.lines() {
            if let Ok(l) = line {
                logln!("{tag} {l}");
            }
        }
    });
}

/// (Re)start the LAN proxy against the given dsh port.
/// `notify` controls whether an enable notification is posted (only on the
/// user-facing enable action, not on automatic re-points after dsh restarts).
/// Locking wrapper: takes `LAN_MUTEX` and delegates to the unlocked core.
fn start_lan(app: &AppHandle, dsh_port: &str, notify: bool) -> Result<(), String> {
    let _g = mlock(&LAN_MUTEX); // serialize with kill_lan / other starts
    start_lan_unlocked(app, dsh_port, notify)
}

/// Core of `start_lan`; requires `LAN_MUTEX` to be held by the caller.
/// Split so the crash-watcher can hold the mutex across check+restart without
/// re-locking (std Mutex is not reentrant — re-locking there used to deadlock
/// exactly on the crash-restart path it was meant to fix).
fn start_lan_unlocked(app: &AppHandle, dsh_port: &str, notify: bool) -> Result<(), String> {
    let p = paths_from_app(app);
    let node = node_bin(&p.resources);
    let proxy = p.resources.join("lan-proxy.js");
    if !node.is_file() || !proxy.is_file() {
        return Err(format!(
            "内置转发器缺失 (node={}, proxy={})",
            node.display(),
            proxy.display()
        ));
    }
    let mut s = load_settings(app);
    let token = lan_token(&mut s);
    let port = s.lan_port.unwrap_or(LAN_DEFAULT_PORT).to_string();
    // 每次开启生成新配对码（App 端二维码用它构造扫码链接；lan-proxy 用它做
    // 免输登录）。代理重启=新码=旧二维码作废，语义与"重启撤销全部设备"一致。
    let pair = new_pair_code();
    s.lan_pair = Some(pair.clone());
    save_settings(app, &s); // persist the generated token/port/pair

    kill_lan_unlocked(); // mutex already held
    let gen = LAN_GEN.fetch_add(1, Ordering::SeqCst) + 1;
    let mut cmd = Command::new(&node);
    #[cfg(target_os = "windows")]
    {
        cmd = no_console(cmd); // node.exe 是控制台程序，避免闪控制台窗
    }
    let mut child = cmd
        .arg(&proxy)
        .arg(dsh_port)
        .arg(&token)
        .arg(&port)
        .arg(&pair)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("启动局域网转发器失败: {e}"))?;
    // 代理自身的输出进 launcher 日志：崩溃、报错都能看到，不再凭空消失。
    if let Some(h) = child.stdout.take() {
        spawn_pipe_logger(h, "[lan-proxy]");
    }
    if let Some(h) = child.stderr.take() {
        spawn_pipe_logger(h, "[lan-proxy:err]");
    }
    // 看护线程：代理意外退出且局域网仍应开启时自动重启（之前代理一崩，
    // 手机端就全线 load failed，且无人重启，只能等 App 重启）。
    let pid = child.id();
    let app2 = app.clone();
    let port_now = mlock(&DSH_URL)
        .as_ref()
        .and_then(|u| u.rsplit(':').next().map(|s| s.to_string()))
        .unwrap_or_else(|| dsh_port.to_string());
    std::thread::spawn(move || {
        let status = child.wait(); // 持有 Child：负责 reap，且保证进程退出后才会走到重启判断
        match status {
            Ok(st) => logln!("lan proxy exited (code {:?}); watching for restart", st.code()),
            Err(e) => logln!("lan proxy wait failed: {e}"),
        }
        // 全程持锁做"校验 + 重启"，与托盘开关/其他 start/kill 严格串行；
        // 重启走 start_lan_unlocked（不再二次上锁，避免自死锁）。
        let _g = mlock(&LAN_MUTEX);
        if LAN_ON.load(Ordering::SeqCst) && LAN_GEN.load(Ordering::SeqCst) == gen && load_settings(&app2).lan_enabled {
            let port = mlock(&DSH_URL)
                .as_ref()
                .and_then(|u| u.rsplit(':').next().map(|s| s.to_string()))
                .unwrap_or(port_now);
            logln!("lan proxy died; restarting against dsh port {port}");
            if let Err(e) = start_lan_unlocked(&app2, &port, false) {
                logln!("lan proxy restart failed: {e}");
            }
        } else {
            // 不再重启：仅当 LAN_CHILD 仍指向我们看护的这个 pid 才清理，
            // 防止误清（比如 dsh 重启后新代理已入位，gen 已变）。
            let mut slot = mlock(&LAN_CHILD);
            if *slot == Some(pid) {
                *slot = None;
            }
        }
    });
    *mlock(&LAN_CHILD) = Some(pid);
    LAN_ON.store(true, Ordering::SeqCst);
    sync_lan_check();
    // ---- LAN 壳层增强：随 LAN 启动（失败仅日志，不影响 LAN 主流程）----
    start_sleep_guard(); // ① 阻止休眠（与"局域网访问"开关联动，双平台）
    start_mdns(app, s.lan_port.unwrap_or(LAN_DEFAULT_PORT)); // ② mDNS 通告（双平台）
    start_ip_watch(s.lan_port.unwrap_or(LAN_DEFAULT_PORT)); // ③ IP 变化轮询（双平台）
    // 开启后的地址/二维码统一在「扫码远程连接」窗口查看（托盘菜单）。
    let _ = notify;
    Ok(())
}

/// Reflect the LAN state into the tray check item.
fn sync_lan_check() {
    if let Some(item) = mlock(&LAN_ITEM).as_ref() {
        let _ = item.set_checked(LAN_ON.load(Ordering::SeqCst));
    }
}

/// Toggle LAN access from the tray.
fn set_lan_access(app: &AppHandle, enable: bool) {
    let result = if enable {
        let port = mlock(&DSH_URL)
            .clone()
            .and_then(|u| u.rsplit(':').next().map(|s| s.to_string()))
            .ok_or_else(|| "dsh 尚未就绪，请稍后再试".to_string());
        match port {
            Ok(p) => {
                let mut s = load_settings(app);
                s.lan_enabled = true;
                save_settings(app, &s);
                match start_lan(app, &p, true) {
                    Ok(()) => Ok(()),
                    Err(e) => {
                        // revert the persisted flag so we don't retry-loop
                        let mut s = load_settings(app);
                        s.lan_enabled = false;
                        save_settings(app, &s);
                        Err(e)
                    }
                }
            }
            Err(e) => Err(e),
        }
    } else {
        kill_lan();
        LAN_ON.store(false, Ordering::SeqCst);
        let mut s = load_settings(app);
        s.lan_enabled = false;
        save_settings(app, &s);
        sync_lan_check();
        update::notify("局域网访问已关闭", "仅本机可访问");
        Ok(())
    };
    if let Err(e) = result {
        sync_lan_check(); // Tauri 点击已自动翻转勾选：失败时恢复菜单与 LAN_ON 一致
        update::notify("局域网访问设置失败", &e);
    }
}

/// On dsh readiness (and when LAN is enabled), (re)point the proxy at the
/// current dsh port — dsh may have restarted on a new random port.
fn ensure_lan_on_ready(app: &AppHandle, dsh_port: &str) {
    if load_settings(app).lan_enabled {
        if let Err(e) = start_lan(app, dsh_port, false) {
            logln!("lan proxy restart failed: {e}");
        }
    }
}

// ---------------------------------------------------------------------------
// LAN 壳层增强（任务书 LAN_SHELL_ENHANCEMENTS.md，按用户决定调整）
//  ① 局域网期间阻止休眠（与"局域网访问"开关联动：LAN 开 = 阻止休眠。
//     macOS: /usr/bin/caffeinate；Windows: SetThreadExecutionState）
//  ② mDNS 稳定域名通告（macOS: /usr/bin/dns-sd；Windows: 内置 node 跑
//     mdns-advertise.js —— Windows 无系统自带 mDNS 注册命令行工具）
//  ③ 局域网 IP 变化检测与通知（双平台轮询）
//
// 设计要点：
//  - 全部挂在 start_lan_unlocked / kill_lan_unlocked 生命周期内，自动获得
//    "代理崩溃看护重启时跟随、App 退出清理、与托盘开关同步"。
//  - 所有 spawn/调用失败一律 logln! 降级，绝不 panic、绝不阻塞/失败 LAN 主流程。
//  - macOS 只调系统二进制；Windows 复用内置 node（零新下载）。
// ---------------------------------------------------------------------------

/// 增强①：LAN 开启 → 阻止系统休眠（与"局域网访问"开关联动，无独立开关）。
/// macOS: caffeinate 子进程；Windows: SetThreadExecutionState（进程级，退出自动释放）。
fn start_sleep_guard() {
    #[cfg(target_os = "macos")]
    {
        if let Err(e) = start_caffeinate() {
            logln!("caffeinate start failed (degraded): {e}");
        }
    }
    #[cfg(target_os = "windows")]
    start_sleep_guard_win();
}

/// 增强①：LAN 关闭 → 释放阻止休眠。
fn stop_sleep_guard() {
    #[cfg(target_os = "macos")]
    kill_caffeinate();
    #[cfg(target_os = "windows")]
    stop_sleep_guard_win();
}

/// 增强①（macOS）：spawn `/usr/bin/caffeinate -dimsu`（-i 阻止 idle 系统休眠为
/// 核心，-d 显示器、-m 磁盘、-s 系统、-u 用户活跃，全上最稳），保证手机随时可连。
/// 加 `-w <自身PID>`：App 被强杀（kill -9/崩溃）时 caffeinate 随之退出，
/// 避免孤儿进程永久持有电源断言导致 Mac 无法休眠。
/// 幂等：先清旧实例。失败返回 io::Error，调用方 logln 降级。
#[cfg(target_os = "macos")]
fn start_caffeinate() -> std::io::Result<()> {
    kill_caffeinate();
    let pid = std::process::id();
    let child = Command::new("/usr/bin/caffeinate")
        .args(["-dimsu", "-w", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    *mlock(&SLEEP_GUARD) = Some(child);
    logln!("caffeinate started (prevent sleep while LAN on, -w {pid})");
    Ok(())
}

/// 增强①（macOS）：幂等停止 caffeinate（SIGTERM 后 wait 回收，无残留/无僵尸）。
#[cfg(target_os = "macos")]
fn kill_caffeinate() {
    if let Some(mut c) = mlock(&SLEEP_GUARD).take() {
        unsafe {
            libc::kill(c.id() as libc::pid_t, libc::SIGTERM);
        }
        let _ = c.wait();
        logln!("caffeinate stopped");
    }
}

/// 增强①（Windows）：SetThreadExecutionState 是**线程作用域**的状态——设置线程
/// 退出即清除，其它线程也无法清除本线程的标记。而 start_lan_unlocked 会被主线程
/// （托盘）、boot 线程（dsh 就绪/崩溃重启）、看护线程（代理崩溃重启，调用完即
/// 退出）三条线程调用：直接在此设置会导致"看护重启后失效、关闭时放不掉"。
/// 因此用专用常驻守卫线程（同 wakepy 方案）：设置与清除永远发生在同一线程，
/// 代际 bump 让守卫线程在 500ms 内自行清除后退出（不 join，不阻塞 LAN 主流程）。
#[cfg(target_os = "windows")]
fn start_sleep_guard_win() {
    use windows::Win32::System::Power::{
        SetThreadExecutionState, ES_CONTINUOUS, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED,
        EXECUTION_STATE,
    };
    let gen = POWER_GUARD_GEN.fetch_add(1, Ordering::SeqCst) + 1;
    std::thread::spawn(move || {
        unsafe {
            SetThreadExecutionState(EXECUTION_STATE(
                ES_CONTINUOUS.0 | ES_SYSTEM_REQUIRED.0 | ES_DISPLAY_REQUIRED.0,
            ));
        }
        logln!("SetThreadExecutionState: prevent sleep while LAN on (guard thread)");
        while POWER_GUARD_GEN.load(Ordering::SeqCst) == gen {
            std::thread::sleep(Duration::from_millis(500));
        }
        // 同一线程内清除后退出（线程退出即释放，双保险）
        unsafe {
            SetThreadExecutionState(ES_CONTINUOUS);
        }
        logln!("SetThreadExecutionState: sleep prevention released (guard thread)");
    });
}

/// 增强①（Windows）：请求释放阻止休眠（守卫线程 500ms 内自行清除并退出）。
#[cfg(target_os = "windows")]
fn stop_sleep_guard_win() {
    POWER_GUARD_GEN.fetch_add(1, Ordering::SeqCst);
}

/// 增强②：启动 mDNS 通告（macOS: dns-sd；Windows: node mdns-advertise.js）。
/// 统一存储 PID + 起看护线程（子进程异常退出时自动重启/复位 MDNS_ON）。
/// 失败仅日志降级（.local 不可用时用户仍可用 IP 访问）。
fn start_mdns(app: &AppHandle, port: u16) {
    #[cfg(target_os = "macos")]
    let child = start_mdns_macos(&port.to_string());
    #[cfg(target_os = "windows")]
    let child = start_mdns_windows(app, &port.to_string());
    let mut child = match child {
        Ok(c) => c,
        Err(e) => {
            MDNS_ON.store(false, Ordering::SeqCst);
            logln!("mdns start failed (degraded): {e}");
            return;
        }
    };
    let pid = child.id();
    let gen = MDNS_GEN.fetch_add(1, Ordering::SeqCst) + 1;
    // 先落槽再起看护：若子进程瞬间退出（5353 被占等），看护线程不会先于
    // MDNS_CHILD/MDNS_ON 赋值执行 else 清理，避免"陈旧 true + 死 PID"竞态。
    *mlock(&MDNS_CHILD) = Some(pid);
    MDNS_ON.store(true, Ordering::SeqCst);
    let app2 = app.clone();
    std::thread::spawn(move || {
        let status = child.wait(); // 持有 Child：负责 reap，退出后才会走到重启判断
        match status {
            Ok(st) => logln!("mdns advertisement exited (code {:?})", st.code()),
            Err(e) => logln!("mdns advertisement wait failed: {e}"),
        }
        // 重启退避（锁外 sleep，不阻塞 LAN 操作）：脚本持续崩溃/5353 被占时
        // 避免 spawn→wait→spawn 紧循环。
        std::thread::sleep(Duration::from_millis(1000));
        // 与 LAN 代理看护同模式：全程持 LAN_MUTEX 做"校验 + 重启"，与托盘开关/
        // kill_lan/start_lan 严格串行，杜绝"检查通过后、重启前 LAN 被关"导致
        // 重启出游离 mdns 进程的竞态（代际 + LAN_ON + MDNS_ON 三重校验兜底）。
        let _g = mlock(&LAN_MUTEX);
        if LAN_ON.load(Ordering::SeqCst) && MDNS_ON.load(Ordering::SeqCst) && MDNS_GEN.load(Ordering::SeqCst) == gen {
            logln!("mdns advertisement died; restarting");
            start_mdns(&app2, port); // 新代际，看护随之重建
        } else {
            MDNS_ON.store(false, Ordering::SeqCst);
            let mut slot = mlock(&MDNS_CHILD);
            if *slot == Some(pid) {
                *slot = None;
            }
        }
    });
}

/// 增强②（macOS）：spawn `/usr/bin/dns-sd -R "DeepSeek Harness" _http._tcp
/// local <port>`，常驻通告稳定域名 `http://DeepSeek-Harness.local:<port>/`
/// （IP 变了也不用改地址）。已知限制（写进弹窗副文案，非 bug）：.local 仅
/// iOS/macOS/同子网可解析，Android 浏览器不支持 .local；路由器开 AP 隔离或
/// 禁 mDNS 时自动降级为 IP 访问。
#[cfg(target_os = "macos")]
fn start_mdns_macos(port: &str) -> std::io::Result<Child> {
    Command::new("/usr/bin/dns-sd")
        .args(["-R", "DeepSeek Harness", "_http._tcp", "local", port])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
}

/// 增强②（Windows）：spawn 内置 node 运行 `mdns-advertise.js <port>`（纯 JS
/// mDNS 通告器，见 mdns-advertise.js 注释）。node.exe 是控制台程序，加
/// CREATE_NO_WINDOW 避免闪控制台窗。
#[cfg(target_os = "windows")]
fn start_mdns_windows(app: &AppHandle, port: &str) -> std::io::Result<Child> {
    let p = paths_from_app(app);
    let node = node_bin(&p.resources);
    let script = p.resources.join("mdns-advertise.js");
    if !node.is_file() || !script.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("mdns assets missing (node={}, script={})", node.display(), script.display()),
        ));
    }
    no_console(Command::new(&node))
        .arg(&script)
        .arg(port)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
}

/// 增强②：幂等停止 mDNS 通告（bump 代际使看护线程放弃重启；按 PID 结束，
/// Child 由看护线程负责 reap）。macOS SIGTERM 时脚本发 goodbye；Windows
/// taskkill /F 强杀无 goodbye，靠 TTL 到期清理。
fn kill_mdns() {
    MDNS_GEN.fetch_add(1, Ordering::SeqCst);
    MDNS_ON.store(false, Ordering::SeqCst);
    if let Some(pid) = mlock(&MDNS_CHILD).take() {
        #[cfg(unix)]
        unsafe {
            libc::kill(pid as libc::pid_t, libc::SIGTERM);
        }
        #[cfg(windows)]
        {
            let _ = no_console(Command::new("taskkill"))
                .arg("/PID")
                .arg(pid.to_string())
                .arg("/T")
                .arg("/F")
                .status();
        }
    }
}

/// 增强③：启动局域网 IP 轮询线程（双平台；30s 一次 `lan_ip()`，变化 →
/// 日志 + 系统通知）。线程无需 AppHandle（notify 是自由函数）。
/// 停止机制：代际计数器。kill 侧只 bump `LAN_IP_WATCH_GEN`，旧线程下一轮
/// 醒来校验代际不匹配即退出——不 join（避免 kill_lan 被最长 30s 的 sleep
/// 阻塞），也不会与重启后的新线程双跑。
/// TODO(macos 优化): 改用 SCDynamicStore / NWPathMonitor 事件驱动，零轮询。
fn start_ip_watch(port: u16) {
    let gen = LAN_IP_WATCH_GEN.fetch_add(1, Ordering::SeqCst) + 1;
    std::thread::spawn(move || {
        // 初始记录（不通知；上次 LAN 周期若遗留旧值，先覆盖为当前值）
        let mut last = lan_ip();
        *mlock(&LAN_IP_LAST) = last.clone();
        loop {
            std::thread::sleep(Duration::from_secs(30));
            if LAN_IP_WATCH_GEN.load(Ordering::SeqCst) != gen {
                break; // 被 stop / 被新周期取代
            }
            if let Some(now) = lan_ip() {
                if last.as_ref() != Some(&now) {
                    logln!("lan ip changed: {:?} -> {}", last, now);
                    // None→Some（网络刚就绪）只记日志不弹通知，避免误报
                    // "地址变化"；只有两个真实地址之间的变化才通知用户。
                    if last.is_some() {
                        update::notify("局域网地址已变化", &format!("新地址 http://{now}:{port}"));
                    }
                    last = Some(now);
                    *mlock(&LAN_IP_LAST) = last.clone();
                }
            }
        }
    });
}

// ---------------------------------------------------------------------------
// Login autostart (opt-in) & uninstaller (M4)
// ---------------------------------------------------------------------------

#[cfg(target_os = "macos")]
/// LaunchAgent plist path for login autostart.
fn login_plist() -> PathBuf {
    home_dir()
        .join("Library/LaunchAgents")
        .join(format!("{APP_ID}.plist"))
}

#[cfg(target_os = "macos")]
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(target_os = "macos")]
fn login_item_enabled() -> bool {
    login_plist().is_file()
}

#[cfg(target_os = "windows")]
fn login_item_enabled() -> bool {
    // HKCU Run 键存在即视为已开启登录自启。
    no_console(Command::new("reg"))
        .args([
            "query",
            "HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run",
            "/v",
            "DeepSeek Harness",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn login_item_enabled() -> bool {
    false
}

/// Write or remove the login-autostart entry (the actual mechanism); no GUI deps.
#[cfg(target_os = "macos")]
fn set_login_item_core(enable: bool) -> Result<(), String> {
    let plist = login_plist();
    let uid = std::env::var("UID").unwrap_or_else(|_| "501".into());
    if enable {
        let exe = std::env::current_exe().map_err(|e| format!("定位程序失败: {e}"))?;
        let home = home_dir();
        let xml = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>{app_id}</string>
  <key>ProgramArguments</key><array><string>{exe}</string></array>
  <key>RunAtLoad</key><true/>
  <key>WorkingDirectory</key><string>{home}</string>
</dict></plist>"#,
            app_id = xml_escape(APP_ID),
            exe = xml_escape(&exe.to_string_lossy()),
            home = xml_escape(&home.to_string_lossy())
        );
        if let Some(dir) = plist.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("创建 LaunchAgents 失败: {e}"))?;
        }
        std::fs::write(&plist, xml).map_err(|e| format!("写登录自启配置 failed: {e}"))?;
        // 只写 plist（下次登录生效），不要 launchctl bootstrap：
        // bootstrap 会立刻触发 RunAtLoad，导致"开登录自启"时马上再起一个实例。
        let _ = Command::new("launchctl")
            .args(["bootout", &format!("gui/{uid}"), plist.to_str().unwrap_or("")])
            .status(); // 清掉旧的一次性加载（若无则报错，忽略）
    } else {
        let _ = Command::new("launchctl")
            .args(["bootout", &format!("gui/{uid}"), plist.to_str().unwrap_or("")])
            .status();
        if plist.exists() {
            std::fs::remove_file(&plist).map_err(|e| format!("删除登录自启配置失败: {e}"))?;
        }
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn set_login_item_core(enable: bool) -> Result<(), String> {
    const RUN_KEY: &str = "HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run";
    const RUN_NAME: &str = "DeepSeek Harness";
    if enable {
        let exe = std::env::current_exe().map_err(|e| format!("定位程序失败: {e}"))?;
        let cmdline = format!("\"{}\"", exe.display());
        let st = no_console(Command::new("reg"))
            .arg("add")
            .arg(RUN_KEY)
            .arg("/v")
            .arg(RUN_NAME)
            .arg("/t")
            .arg("REG_SZ")
            .arg("/d")
            .arg(&cmdline)
            .arg("/f")
            .status()
            .map_err(|e| format!("写入登录自启注册表失败: {e}"))?;
        if !st.success() {
            return Err("写入登录自启注册表失败 (reg add)".into());
        }
    } else {
        // 删除 Run 键（键不存在时 reg delete 报错，忽略即可）
        let _ = no_console(Command::new("reg"))
            .arg("delete")
            .arg(RUN_KEY)
            .arg("/v")
            .arg(RUN_NAME)
            .arg("/f")
            .status();
    }
    Ok(())
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn set_login_item_core(_enable: bool) -> Result<(), String> {
    Ok(())
}

fn set_login_item(enable: bool) -> Result<(), String> {
    set_login_item_core(enable)?;
    LOGIN_ON.store(enable, Ordering::SeqCst);
    // sync the tray check state
    if let Some(item) = mlock(&LOGIN_ITEM).as_ref() {
        let _ = item.set_checked(enable);
    }
    Ok(())
}

/// Shared teardown for uninstall (also used by the CLI test hook).
/// 删除目录并自动重试（Windows 共享锁：WebView2/日志句柄释放有延迟，
/// macOS 的 unlink 无共享锁语义不受影响）。仍失败则返回最后一次错误。
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
    let _ = set_login_item_core(false);
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
    // 目录级删除：失败不中断整体卸载（残留会在重启后清理），逐个重试。
    let mut leftovers: Vec<String> = Vec::new();
    for dir in dirs {
        if dir.exists() {
            if let Err(e) = remove_dir_all_retry(&dir) {
                leftovers.push(format!("{}（{e}）", dir.display()));
            }
        }
    }
    if !leftovers.is_empty() {
        let msg = format!("以下目录被占用未能删除，重启电脑后即可手动清理：\n{}", leftovers.join("\n"));
        logln!("[uninstall] 残留目录: {msg}");
        update::notify("部分数据将延迟清理", &msg);
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
    return label == 'DeepSeek Harness' or b'DeepSeek' in td.get('book', b'')
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
    ($_.Name -eq 'node.exe' -and $_.CommandLine -match 'bin\.js.*--profile web') -or
    ($_.Name -eq 'node.exe' -and $_.CommandLine -match 'lan-proxy\.js') -or
    ($_.Name -eq 'node.exe' -and $_.CommandLine -match 'mdns-advertise\.js')
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
        match update::check_update(&p, settings.registry.as_deref()) {
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
        let reg_url = update::registry_url(settings.registry.as_deref());
        match update::apply_update(&p, &ver, &reg_url) {
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
    if let Some(idx) = args.iter().position(|a| a == "--self-login-item") {
        let on = args.get(idx + 1).map(|s| s == "on").unwrap_or(false);
        match set_login_item_core(on) {
            Ok(()) => {
                println!("LOGIN_ITEM {on}");
                std::process::exit(0);
            }
            Err(e) => {
                eprintln!("LOGIN_ITEM_ERROR {e}");
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
        //   1) 结束可能仍在运行的其它 App 实例（含其 node/dsh/lan 子进程，树杀），
        //      释放程序目录 / 日志 / WebView2 数据文件锁 → 否则 NSIS 删文件必然失败
        //      （这正是"右键→卸载 无反应"的根因之一）。
        //   2) 复用 uninstall_teardown 做数据清理（登录自启、app 数据、WebView2 缓存、
        //      可选 ~/.dsh）。
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
            .args(["-e", r#"tell application "DeepSeek Harness" to activate"#])
            .spawn();
        #[cfg(not(target_os = "macos"))]
        logln!("another instance is running; exiting");
        return;
    }

    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            plugin_op,
            lan_state,
            lan_toggle,
            uninstall_run,
            modal_spec,
            modal_respond,
        ])
        .setup(|app| {
            let show = MenuItem::with_id(app, "show", "显示主窗口", true, None::<&str>)?;
            let open = MenuItem::with_id(app, "open_browser", "在浏览器中打开", true, None::<&str>)?;
            let ws = MenuItem::with_id(app, "choose_ws", "设置默认工作目录…", true, None::<&str>)?;
            let open_ws =
                MenuItem::with_id(app, "open_ws", "打开默认工作目录", true, None::<&str>)?;
            let check = MenuItem::with_id(app, "check_update", "检查更新…", true, None::<&str>)?;
            let logs = MenuItem::with_id(app, "open_logs", "打开日志", true, None::<&str>)?;
            let restart = MenuItem::with_id(app, "restart", "重启工作台", true, None::<&str>)?;
            let plugins = MenuItem::with_id(app, "plugins", "插件管理…", true, None::<&str>)?;
            let login = CheckMenuItem::with_id(
                app,
                "launch_login",
                "登录时启动",
                true,
                login_item_enabled(),
                None::<&str>,
            )?;
            LOGIN_ON.store(login_item_enabled(), Ordering::SeqCst);
            *mlock(&LOGIN_ITEM) = Some(login.clone());
            let lan_panel = MenuItem::with_id(app, "lan_panel", "扫码远程连接…", true, None::<&str>)?;
            let uninstall_item = MenuItem::with_id(app, "uninstall", "卸载 DeepSeek Harness…", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
            let sep1 = tauri::menu::PredefinedMenuItem::separator(app)?;
            let sep2 = tauri::menu::PredefinedMenuItem::separator(app)?;
            let sep3 = tauri::menu::PredefinedMenuItem::separator(app)?;
            // 市场惯例分组：窗口/视图 → 功能 → 自启/局域网 → 卸载/退出
            let menu = Menu::with_items(
                app,
                &[
                    &show,
                    &open,
                    &sep1,
                    &ws,
                    &open_ws,
                    &check,
                    &logs,
                    &restart,
                    &plugins,
                    &lan_panel,
                    &sep2,
                    &login,
                    &sep3,
                    &uninstall_item,
                    &quit,
                ],
            )?;
            let _tray = TrayIconBuilder::with_id("tray")
                .icon(app.default_window_icon().unwrap().clone())
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "quit" => {
                        kill_dsh();
                        app.exit(0);
                    }
                    "show" => {
                        let url = mlock(&DSH_URL)
                            .clone()
                            .unwrap_or_else(|| "http://127.0.0.1:3080".into());
                        show_window(app, &url);
                    }
                    "open_browser" => open_in_browser(app),
                    "choose_ws" => choose_workspace(app),
                    "open_ws" => open_workspace_in_finder(app),
                    "check_update" => check_for_updates(app),
                    "open_logs" => {
                        let p = paths_from_app(app);
                        open_dir(&p.app_data.join("logs"));
                    }
                    "lan_panel" => open_lan_window(app),
                    "restart" => restart_dsh(app),
                    "plugins" => open_plugin_manager(app),
                    "launch_login" => {
                        let next = !LOGIN_ON.load(Ordering::SeqCst);
                        match set_login_item(next) {
                            Ok(()) => {}
                            Err(e) => {
                                // Tauri 点击已自动翻转勾选：失败时恢复菜单与 LOGIN_ON 一致
                                if let Some(item) = mlock(&LOGIN_ITEM).as_ref() {
                                    let _ = item.set_checked(LOGIN_ON.load(Ordering::SeqCst));
                                }
                                update::notify("登录自启设置失败", &e);
                            }
                        }
                    }
                    "uninstall" => open_uninstall_window(app),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        ..
                    } = event
                    {
                        let app = tray.app_handle();
                        let url = mlock(&DSH_URL)
                            .clone()
                            .unwrap_or_else(|| "http://127.0.0.1:3080".into());
                        show_window(app, &url);
                    }
                })
                .build(app)?;

            let handle = app.handle().clone();
            init_log(&paths_from_app(app.handle()));
            // 启动先显示占位启动页（ui/index.html，"正在启动…"），
            // dsh 就绪后 show_window 会导航到工作台地址，避免白屏。
            let _ = WebviewWindowBuilder::new(
                app,
                WINDOW_LABEL,
                WebviewUrl::App("index.html".into()),
            )
            .title("DeepSeek Harness")
            .inner_size(1280.0, 820.0)
            .min_inner_size(800.0, 560.0)
            .center() // 主窗启动即居中于当前屏幕
            .theme(Some(tauri::Theme::Dark)) // B1：暗色原生标题栏一致化
            .on_page_load(|webview, payload| {
                // 所有页面加载记一行日志；DOM 探针只针对 dsh 工作台页面
                let url = payload.url().to_string();
                logln!("[webview] page loaded: {url}");
                if url.starts_with("http://127.0.0.1") {
                    let label = webview.label().to_string();
                    probe_webview(&webview, label.clone(), "load".to_string());
                    let wv = webview.clone();
                    std::thread::spawn(move || {
                        std::thread::sleep(Duration::from_millis(4000));
                        let _ = wv.eval_with_callback(
                            r#"JSON.stringify({ready:document.readyState,title:document.title,bodyLen:document.body?document.body.innerText.length:-1,bodyHead:document.body?document.body.innerText.slice(0,160):''})"#,
                            move |res| logln!("[webview:{label}] probe(t+4s): {res}"),
                        );
                    });
                }
            })
            .on_navigation(webview_navigation_policy)
            .on_new_window(webview_new_window_policy)
            .build();
            std::thread::spawn(move || boot(handle));
            periodic_check(app.handle().clone());
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
                let url = mlock(&DSH_URL)
                    .clone()
                    .unwrap_or_else(|| "http://127.0.0.1:3080".into());
                show_window(_app_handle, &url);
            }
            RunEvent::ExitRequested { .. } | RunEvent::Exit => kill_dsh(),
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
    fn pkg_name_validation() {
        // 合法
        for ok in ["@linxin666/dsh-web-ui-all", "lodash", "@scope/pkg-name", "a.b-c_d", "x~y"] {
            assert!(valid_pkg_name(ok), "{ok} 应合法");
        }
        // 非法
        for bad in [
            "",
            "a b",
            "a$b",
            "a;rm",
            "a\"b",
            "a`b",
            "a\\b",
            "a|b",
            "a>b",
            ".x",
            "-g",
            "--dir=/tmp",
            "--prefix=x",
            "..",
            &"x".repeat(215),
        ] {
            assert!(!valid_pkg_name(bad), "{bad:?} 应非法");
        }
    }

    #[test]
    fn extract_ignored_builds() {
        let out = "ERR_PNPM_IGNORED_BUILDS Ignored build scripts: cloudflared, ssh2. Run \"pnpm approve-builds\" to pick which dependencies should be allowed to run scripts.";
        let names = extract_pkg_names(out);
        assert!(names.contains(&"cloudflared".to_string()));
        assert!(names.contains(&"ssh2".to_string()));
        assert_eq!(names.len(), 2);
        // scope 包名 + 无句点结尾的 pnpm 11 真实格式（[ERR_...] 前缀、行尾无句点）
        let real = "[ERR_PNPM_IGNORED_BUILDS] Ignored build scripts: @scope/pkg, cpu-features, ssh2";
        let names2 = extract_pkg_names(real);
        assert!(names2.contains(&"@scope/pkg".to_string()));
        assert!(names2.contains(&"cpu-features".to_string()));
        assert_eq!(names2.len(), 3);
        assert!(extract_pkg_names("no matches here").is_empty());
    }

    #[test]
    fn workspace_gate_config() {
        let dir = std::env::temp_dir().join(format!("dsh-ws-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // 首次：文件不存在 → 生成模板 + 门禁配置
        ensure_pnpm_workspace(&dir, &["cloudflared".into()]).unwrap();
        let yaml = std::fs::read_to_string(dir.join("pnpm-workspace.yaml")).unwrap();
        assert!(yaml.contains("nodeLinker: hoisted"));
        assert!(yaml.contains("allowBuilds:"));
        assert!(yaml.contains("  cloudflared: true"));
        assert!(yaml.contains("minimumReleaseAge: 0"));
        // 幂等：再次调用不重复
        ensure_pnpm_workspace(&dir, &["cloudflared".into()]).unwrap();
        let again = std::fs::read_to_string(dir.join("pnpm-workspace.yaml")).unwrap();
        assert_eq!(yaml, again);
        // 补充新包名（allowBuilds 已存在时插入块尾）
        ensure_pnpm_workspace(&dir, &["ssh2".into(), "some-other".into()]).unwrap();
        let third = std::fs::read_to_string(dir.join("pnpm-workspace.yaml")).unwrap();
        assert!(third.contains("  ssh2: true"));
        assert!(third.contains("  some-other: true"));
        assert!(!third.contains("  ssh2: true\n  ssh2: true"));
        // minimumReleaseAge 已存在时保持幂等、不重复
        assert_eq!(third.matches("minimumReleaseAge:").count(), 1);
        ensure_pnpm_workspace(&dir, &[]).unwrap();
        let fourth = std::fs::read_to_string(dir.join("pnpm-workspace.yaml")).unwrap();
        assert_eq!(fourth.matches("minimumReleaseAge:").count(), 1);
        assert_eq!(fourth, third); // 无新增时内容不变
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tail_limits_output() {
        assert_eq!(tail_text("short", 100), "short");
        let long = "x".repeat(1000);
        let t = tail_text(&long, 100);
        assert!(t.contains("截断"));
        assert!(t.ends_with("x".repeat(100).as_str()));
        // char 边界安全（中文）
        let cn = "中".repeat(500);
        let t2 = tail_text(&cn, 100);
        assert!(t2.ends_with("中".repeat(100).as_str()));
    }

    #[test]
    fn strip_verbatim_prefix_logic() {
        assert_eq!(strip_verbatim_prefix(r"\\?\C:\foo\bar"), Some("C:\\foo\\bar".into()));
        assert_eq!(strip_verbatim_prefix(r"\\?\UNC\host\share\x"), Some(r"\\host\share\x".into()));
        assert_eq!(strip_verbatim_prefix(r"C:\foo"), None);
        assert_eq!(strip_verbatim_prefix(r"\\host\share\x"), None);
        assert_eq!(strip_verbatim_prefix(""), None);
    }

    #[test]
    fn clean_empty_dirs_removes_only_empty() {
        let root = std::env::temp_dir().join(format!("dsh-clean-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let scoped = root.join("node_modules/@linxin666/dsh-web-ui-all");
        std::fs::create_dir_all(&scoped).unwrap();
        let keep = root.join("node_modules/dsh-base");
        std::fs::create_dir_all(&keep).unwrap();
        std::fs::write(keep.join("index.js"), "x").unwrap();
        clean_empty_dirs_under(&root.join("node_modules"));
        assert!(!scoped.exists(), "空目录应被删除");
        assert!(keep.exists(), "非空目录应保留");
        assert!(root.join("node_modules").exists(), "node_modules 本身不应被删");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn pkg_name_length_boundary() {
        // 214 是上限（npm 包名最大长度），215 必须拒绝（已在非法列表）
        let max_ok = "x".repeat(214);
        assert!(valid_pkg_name(&max_ok));
    }

    #[test]
    fn bundled_pnpm_name_matches_platform() {
        // 与 prepare-resources.sh/ps1 的产物名保持一致：
        // macOS/Linux 生成 `pnpm` shim，Windows 生成 `pnpm.cmd`。
        let name = bundled_pnpm_file_name();
        #[cfg(windows)]
        assert_eq!(name, "pnpm.cmd");
        #[cfg(not(windows))]
        assert_eq!(name, "pnpm");
    }

    #[test]
    fn prepend_path_prefixes_and_keeps_existing() {
        use std::process::Command;
        let sep = if cfg!(windows) { ";" } else { ":" };
        // 已有 PATH 时：前置 + 保留原值
        let mut cmd = Command::new("true");
        cmd.env("PATH", "/usr/bin:/bin");
        prepend_path(&mut cmd, std::path::Path::new("/opt/pnpm-bin"));
        let mut paths: Vec<String> = cmd
            .get_envs()
            .filter(|(k, _)| k.to_str() == Some("PATH"))
            .map(|(_, v)| v.map(|x| x.to_string_lossy().into_owned()).unwrap_or_default())
            .collect();
        assert_eq!(paths.len(), 1);
        let p = paths.pop().unwrap();
        assert!(p.starts_with(&format!("/opt/pnpm-bin{sep}")), "前置失败: {p}");
        assert!(p.contains("/usr/bin"), "原 PATH 丢失: {p}");
        // 空 PATH 时：前置 + 兜底分隔符（不 panic）
        let mut cmd2 = Command::new("true");
        cmd2.env("PATH", "");
        prepend_path(&mut cmd2, std::path::Path::new("/opt/pnpm-bin"));
        let p2: String = cmd2
            .get_envs()
            .filter(|(k, _)| k.to_str() == Some("PATH"))
            .map(|(_, v)| v.map(|x| x.to_string_lossy().into_owned()).unwrap_or_default())
            .next()
            .unwrap();
        assert!(p2.starts_with(&format!("/opt/pnpm-bin{sep}")), "空 PATH 前置失败: {p2}");
    }
}
