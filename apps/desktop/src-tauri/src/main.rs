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
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tauri::menu::{CheckMenuItem, Menu, MenuItem};
use tauri::tray::{MouseButton, TrayIconBuilder, TrayIconEvent};
use tauri::{
    AppHandle, Manager, RunEvent, WebviewUrl, WebviewWindowBuilder, WindowEvent,
};

/// The running dsh child, kept so it is reaped and so we can kill it on exit.
static CHILD: Mutex<Option<Child>> = Mutex::new(None);
/// The parsed base URL of the running dsh web server.
static DSH_URL: Mutex<Option<String>> = Mutex::new(None);
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

/// App 标识（与 tauri.conf.json identifier 一致；决定 app 数据目录名）。
pub const APP_ID: &str = "com.dsh-desktop.app";
const RESTART_BASE_MS: u64 = 1000;
const RESTART_MAX_MS: u64 = 15000;
const WINDOW_LABEL: &str = "main";

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
            return cand;
        }
    }
    std::env::current_dir()
        .unwrap_or_default()
        .join("resources")
}

fn paths_from_app(app: &AppHandle) -> Paths {
    let app_data = app.path().app_data_dir().unwrap_or_default();
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
    let exe_dir = exe.parent().unwrap_or(Path::new(".")).to_path_buf();
    let cwd = std::env::current_dir().unwrap_or_default();
    let candidates = [
        exe_dir.join("../Resources/resources"), // macOS .app bundle layout
        exe_dir.join("resources"),              // Windows / generic: beside the exe
        exe_dir,                                // Windows: resources may be the exe dir itself
        cwd.join("resources"),                  // dev: repo cwd
    ];
    let resources = candidates
        .into_iter()
        .find(|p| p.join("dsh").is_dir())
        .unwrap_or_else(|| cwd.join("resources"));
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
    load_settings(app)
        .default_cwd
        .filter(|p| p.is_dir())
        .unwrap_or_else(home_dir)
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
            let _ = rfd::MessageDialog::new()
                .set_title("DeepSeek Harness 启动失败")
                .set_description(&msg)
                .set_buttons(rfd::MessageButtons::Ok)
                .show();
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
            let _ = rfd::MessageDialog::new()
                .set_title("DeepSeek Harness 运行异常")
                .set_description(&msg)
                .set_buttons(rfd::MessageButtons::Ok)
                .show();
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
        let _ = w.show();
        let _ = w.set_focus();
        return;
    }
    // 兜底：窗口不存在（极罕见）时重建（探针在 setup 的窗口 builder 里已挂）
    let _ = WebviewWindowBuilder::new(app, WINDOW_LABEL, WebviewUrl::External(parsed))
        .title("DeepSeek Harness")
        .inner_size(1280.0, 820.0)
        .min_inner_size(800.0, 560.0)
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
/// http(s)://tauri.localhost）与 dsh 工作台源（http://127.0.0.1:*）。
/// 精确比对 host（不用 starts_with，避免 `http://127.0.0.1evil.com` 这类
/// 恶意主机名被误放行）。
fn is_internal_webview_url(url: &tauri::Url) -> bool {
    if url.scheme() == "tauri" {
        return true; // 内置页（tauri://localhost/...）
    }
    if url.scheme() == "http" || url.scheme() == "https" {
        return matches!(url.host_str(), Some("tauri.localhost") | Some("127.0.0.1"));
    }
    false
}

/// WebView 导航策略（on_navigation）：内部地址放行；外部 http(s) 及其它
/// 协议（mailto:/tel:/ftp:/file: 等）交给系统浏览器并拦截。修复：AI 回答里的
/// 外链（https://…）此前点击无反应——Tauri 对 target=_blank 新窗口请求默认
/// 一律 Deny。
/// 注意（已知限制）：wry 的导航回调不区分主框架/子框架，外部 http(s) 的
/// iframe/表单提交也会被拦截并转交浏览器——这是有意的安全边界（外部内容不进
/// 工作台）；data:/blob:/about:/javascript: 放行以免破坏内嵌内容（如 srcdoc
/// 预览）。
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
                let yes = rfd::MessageDialog::new()
                    .set_title("DeepSeek Harness 更新")
                    .set_description(&msg)
                    .set_buttons(rfd::MessageButtons::YesNo)
                    .show();
                if yes == rfd::MessageDialogResult::Yes {
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
fn lan_token(s: &mut Settings) -> String {
    if let Some(t) = &s.lan_token {
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
    save_settings(app, &s); // persist the generated token/port

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
    if notify {
        lan_info_dialog(&token, s.lan_port.unwrap_or(LAN_DEFAULT_PORT));
    }
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
        update::notify("局域网访问设置失败", &e);
    }
}

/// Re-show the access address + token (menu "显示局域网访问信息").
fn show_lan_info(app: &AppHandle) {
    if !LAN_ON.load(Ordering::SeqCst) {
        update::notify("局域网访问未开启", "请先在托盘菜单开启“局域网访问”");
        return;
    }
    let s = load_settings(app);
    let token = s.lan_token.unwrap_or_default();
    let port = s.lan_port.unwrap_or(LAN_DEFAULT_PORT);
    lan_info_dialog(&token, port);
}

/// Modal dialog showing the access address + token.
/// macOS: osascript editable-field dialog with a copy button.
/// Windows: rfd message dialog + clipboard copy (no editable field on Win).
fn lan_info_dialog(token: &str, port: u16) {
    let ip = lan_ip().unwrap_or_else(|| "127.0.0.1".into());
    #[cfg(target_os = "macos")]
    {
        let mut script = format!(r#"display dialog "手机浏览器打开：" & return & "http://{ip}:{port}""#);
        if MDNS_ON.load(Ordering::SeqCst) {
            script.push_str(&format!(
                r#" & return & "http://DeepSeek-Harness.local:{port}（IP 变了也不用改）" & return & "提示：.local 仅 iOS/macOS/同子网可用，Android 请用上方 IP 地址""#
            ));
        }
        script.push_str(&format!(
            r#" & return & return & "访问令牌（可选中复制，或点复制按钮）：" default answer "{token}" buttons {{"关闭", "复制令牌"}} default button "复制令牌" with title "局域网访问" with icon note"#
        ));
        if let Ok(out) = Command::new("osascript").args(["-e", &script]).output() {
            let s = String::from_utf8_lossy(&out.stdout);
            if s.contains("复制令牌") {
                copy_to_clipboard(token);
            }
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let mut msg = format!("手机浏览器打开：\nhttp://{ip}:{port}");
        if MDNS_ON.load(Ordering::SeqCst) {
            msg.push_str(&format!("\nhttp://DeepSeek-Harness.local:{port}（IP 变了也不用改）"));
        }
        msg.push_str(&format!("\n\n访问令牌（点“Yes/是”复制）：\n{token}"));
        let yes = rfd::MessageDialog::new()
            .set_title("局域网访问")
            .set_description(&msg)
            .set_buttons(rfd::MessageButtons::YesNo)
            .show();
        if yes == rfd::MessageDialogResult::Yes {
            copy_to_clipboard(token);
        }
    }
}

/// Copy text to the system clipboard via pbcopy (macOS).
#[cfg(target_os = "macos")]
fn copy_to_clipboard(text: &str) {
    let mut child = match Command::new("pbcopy").stdin(Stdio::piped()).spawn() {
        Ok(c) => c,
        Err(_) => return,
    };
    if let Some(mut si) = child.stdin.take() {
        use std::io::Write;
        let _ = si.write_all(text.as_bytes());
        drop(si);
    }
    let _ = child.wait();
    update::notify("已复制", "访问令牌已复制到剪贴板");
}

/// Copy text to the system clipboard (Windows: arboard)。
#[cfg(target_os = "windows")]
fn copy_to_clipboard(text: &str) {
    if let Ok(mut cb) = arboard::Clipboard::new() {
        if cb.set_text(text.to_string()).is_ok() {
            update::notify("已复制", "访问令牌已复制到剪贴板");
            return;
        }
    }
}

/// Fallback for other platforms (no-op).
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn copy_to_clipboard(_text: &str) {}

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
    let app2 = app.clone();
    std::thread::spawn(move || {
        let status = child.wait(); // 持有 Child：负责 reap，退出后才会走到重启判断
        match status {
            Ok(st) => logln!("mdns advertisement exited (code {:?})", st.code()),
            Err(e) => logln!("mdns advertisement wait failed: {e}"),
        }
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
    *mlock(&MDNS_CHILD) = Some(pid);
    MDNS_ON.store(true, Ordering::SeqCst);
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
                    update::notify("局域网地址已变化", &format!("新地址 http://{now}:{port}"));
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
fn uninstall_teardown(p: &Paths, wipe_dsh: bool) -> Result<(), String> {
    kill_dsh();
    let _ = set_login_item_core(false);
    let home = home_dir();
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
            dirs.push(PathBuf::from(local).join(APP_ID));
        }
    }
    for dir in dirs {
        if dir.exists() {
            std::fs::remove_dir_all(&dir).map_err(|e| format!("删除 {} 失败: {e}", dir.display()))?;
        }
    }
    if wipe_dsh {
        let dsh_home = home.join(".dsh");
        if dsh_home.exists() {
            std::fs::remove_dir_all(&dsh_home).map_err(|e| format!("删除 ~/.dsh 失败: {e}"))?;
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
/// Some(true) = uninstall + wipe ~/.dsh; Some(false) = uninstall, keep ~/.dsh;
/// None = cancelled.
#[cfg(target_os = "macos")]
fn ask_uninstall() -> Option<bool> {
    let script = r#"
try
  set r to display dialog "卸载 DeepSeek Harness 将执行：
• 结束 dsh 子进程
• 删除应用数据与登录自启项
• 把 App 移入废纸篓（可恢复）

~/.dsh（你的会话与凭据）默认保留，也可一并删除，且不可恢复。" with title "卸载 DeepSeek Harness？" buttons {"取消", "卸载（保留 ~/.dsh）", "卸载并删除 ~/.dsh"} default button "卸载（保留 ~/.dsh）" with icon caution
  return button returned of r
on error number -128
  return "取消"
end try
"#;
    let out = Command::new("osascript").args(["-e", script]).output().ok()?;
    let s = String::from_utf8_lossy(&out.stdout);
    if s.contains("卸载并删除") {
        Some(true)
    } else if s.contains("卸载（保留") {
        Some(false)
    } else {
        None
    }
}

/// Ask the user how to uninstall (Windows: rfd 三键弹窗)。
#[cfg(not(target_os = "macos"))]
fn ask_uninstall() -> Option<bool> {
    let r = rfd::MessageDialog::new()
        .set_title("卸载 DeepSeek Harness？")
        .set_description(
            "卸载将：结束 dsh 子进程、删除应用数据与登录自启项、把程序移入回收站（可恢复）。\n\n~/.dsh（你的会话与凭据）默认保留，也可一并删除，且不可恢复。\n\nYes/是 = 卸载并删除 ~/.dsh；No/否 = 卸载，保留 ~/.dsh；Cancel/取消 = 什么都不做",
        )
        .set_buttons(rfd::MessageButtons::YesNoCancel)
        .show();
    match r {
        rfd::MessageDialogResult::Yes => Some(true),
        rfd::MessageDialogResult::No => Some(false),
        _ => None,
    }
}

/// Uninstall: teardown, trash the app, then exit.
fn uninstall(app: &AppHandle) {
    let Some(wipe) = ask_uninstall() else {
        return; // cancelled
    };
    let p = paths_from_app(app);
    if let Err(e) = uninstall_teardown(&p, wipe) {
        update::notify("卸载未完成", &e);
        return;
    }

    #[cfg(target_os = "macos")]
    {
        trash_self();
        std::thread::sleep(Duration::from_millis(900)); // let the Finder script start
        // Remove the trashed app from the Dock's "recent applications" so the
        // dead icon does not linger (and refresh the Dock).
        clear_dock_recents();
    }
    #[cfg(not(target_os = "macos"))]
    {
        // Windows：尝试把安装目录移入回收站。运行中的 exe 可能被占用而失败，
        // 此时引导用户走系统“设置 → 应用”卸载。
        if !trash_self() {
            update::notify(
                "请通过系统卸载",
                "应用数据已清理。请到“设置 → 应用 → 已安装的应用”中卸载 DeepSeek Harness。",
            );
        }
    }
    app.exit(0);
}

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
        .setup(|app| {
            let show = MenuItem::with_id(app, "show", "显示主窗口", true, None::<&str>)?;
            let open = MenuItem::with_id(app, "open_browser", "在浏览器中打开", true, None::<&str>)?;
            let ws = MenuItem::with_id(app, "choose_ws", "设置默认工作目录…", true, None::<&str>)?;
            let open_ws =
                MenuItem::with_id(app, "open_ws", "打开默认工作目录", true, None::<&str>)?;
            let check = MenuItem::with_id(app, "check_update", "检查更新…", true, None::<&str>)?;
            let logs = MenuItem::with_id(app, "open_logs", "打开日志", true, None::<&str>)?;
            let restart = MenuItem::with_id(app, "restart", "重启 Harness", true, None::<&str>)?;
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
            let lan = CheckMenuItem::with_id(
                app,
                "lan_access",
                "局域网访问",
                true,
                load_settings(app.handle()).lan_enabled,
                None::<&str>,
            )?;
            LAN_ON.store(load_settings(app.handle()).lan_enabled, Ordering::SeqCst);
            *mlock(&LAN_ITEM) = Some(lan.clone());
            let lan_info = MenuItem::with_id(app, "lan_info", "显示局域网访问信息", true, None::<&str>)?;
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
                    &sep2,
                    &login,
                    &lan,
                    &lan_info,
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
                    "lan_access" => {
                        let next = !LAN_ON.load(Ordering::SeqCst);
                        set_lan_access(app, next);
                    }
                    "lan_info" => show_lan_info(app),
                    "restart" => restart_dsh(app),
                    "launch_login" => {
                        let next = !LOGIN_ON.load(Ordering::SeqCst);
                        match set_login_item(next) {
                            Ok(()) => {}
                            Err(e) => update::notify("登录自启设置失败", &e),
                        }
                    }
                    "uninstall" => uninstall(app),
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
