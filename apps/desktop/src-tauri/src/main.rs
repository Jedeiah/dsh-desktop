// DSh Desktop (DeepSeek Harness Desktop) — M5 launcher
//
// Spawns the bundled node + dsh closure, parses the readiness URL from dsh's
// stdout (`dsh web: http://127.0.0.1:<port>`), and opens an embedded WebView.
//
// Behaviour:
//   - closing the window hides it and keeps the app in the menu-bar tray
//   - quitting (Cmd+Q / app menu / dock Quit) kills the dsh child and exits
//   - clicking the dock icon re-opens a hidden window (Reopen)
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
/// The running LAN proxy child (bundled node + lan-proxy.js), if enabled.
static LAN_CHILD: Mutex<Option<Child>> = Mutex::new(None);
/// Serializes LAN proxy start/kill so the menu handler and the dsh-readiness
/// thread cannot race (two concurrent starts would orphan the first proxy).
static LAN_MUTEX: Mutex<()> = Mutex::new(());
/// Mirrors whether LAN access is currently on.
static LAN_ON: AtomicBool = AtomicBool::new(false);
/// The tray's "局域网访问" check item.
static LAN_ITEM: Mutex<Option<CheckMenuItem<tauri::Wry>>> = Mutex::new(None);
/// Launcher log file (packaged mode). Empty in dev (stderr goes to terminal).
static LOG_FILE: Mutex<Option<std::fs::File>> = Mutex::new(None);

/// App 标识（与 tauri.conf.json identifier 一致；决定 app 数据目录名）。
pub const APP_ID: &str = "com.dsh-desktop.app";
const RESTART_BASE_MS: u64 = 1000;
const RESTART_MAX_MS: u64 = 15000;
const WINDOW_LABEL: &str = "main";

/// 用户主目录：优先 HOME 环境变量，回退到系统 passwd（跨平台，不写死用户名）。
pub fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
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
    let dir = home_dir().join("Library/Application Support").join(APP_ID);
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

/// True when running from a packaged .app bundle (vs `cargo run` / raw binary).
fn is_bundled() -> bool {
    std::env::current_exe()
        .map(|p| p.to_string_lossy().contains(".app/Contents/"))
        .unwrap_or(false)
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

/// Resources root for the CLI hooks (no Tauri handle): bundled
/// `Contents/Resources/resources` relative to the executable, or dev cwd.
fn paths_from_cli() -> Paths {
    let exe = std::env::current_exe().unwrap_or_default();
    let candidates = [
        exe.parent()
            .unwrap_or(Path::new("."))
            .join("../Resources/resources"),
        std::env::current_dir().unwrap_or_default().join("resources"),
    ];
    let resources = candidates
        .into_iter()
        .find(|p| p.join("dsh").is_dir())
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default().join("resources"));
    Paths {
        resources,
        app_data: update::app_data_from_home(),
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

/// Resolve the active dsh closure: prefer the app-data `current` (managed by
/// updates), otherwise the bundled one.
pub fn active_closure(p: &Paths) -> Option<PathBuf> {
    let cur = p.app_data.join("dsh/current");
    if closure_marker(&cur) {
        return Some(cur);
    }
    resolve_closure(&p.resources)
}

/// Scan the (read-only) bundled `dsh/` for a version dir carrying the marker.
fn resolve_closure(res: &Path) -> Option<PathBuf> {
    let dsh = res.join("dsh");
    let cur = dsh.join("current");
    if closure_marker(&cur) {
        return Some(cur);
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
    let node = p.resources.join("node/bin/node");
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
    Command::new(&node)
        .arg(&bin)
        .arg("--profile")
        .arg("web")
        .arg("--port")
        .arg("0")
        .current_dir(&cwd)
        .env("HOME", &home)
        // Force dsh's directory-picker into web "browse" mode (the only
        // reader of SSH_CONNECTION in the closure is the picker resolver):
        // native OS dialogs open on the host Mac and cannot serve phone/
        // LAN clients, whereas the web picker works everywhere.
        .env("SSH_CONNECTION", "1")
        .stdout(Stdio::piped())
        .stderr(stderr)
        .spawn()
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

fn open_in_browser(_app: &AppHandle) {
    let url = mlock(&DSH_URL)
        .clone()
        .unwrap_or_else(|| "http://127.0.0.1:3080".into());
    let _ = Command::new("open").arg(&url).spawn();
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
    let _ = Command::new("open").arg(&dir).spawn();
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
    let mut buf = [0u8; 16];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        use std::io::Read;
        let _ = f.read_exact(&mut buf);
    }
    let t: String = buf.iter().map(|b| format!("{b:02x}")).collect();
    s.lan_token = Some(t.clone());
    t
}

/// Detect the Mac's primary LAN IPv4 (best effort).
fn lan_ip() -> Option<String> {
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

/// Stop the LAN proxy if running.
fn kill_lan() {
    let _g = mlock(&LAN_MUTEX);
    kill_lan_unlocked();
}

fn kill_lan_unlocked() {
    if let Some(mut c) = mlock(&LAN_CHILD).take() {
        let _ = c.kill();
        let _ = c.wait();
    }
}

/// (Re)start the LAN proxy against the given dsh port.
/// `notify` controls whether an enable notification is posted (only on the
/// user-facing enable action, not on automatic re-points after dsh restarts).
fn start_lan(app: &AppHandle, dsh_port: &str, notify: bool) -> Result<(), String> {
    let _g = mlock(&LAN_MUTEX); // serialize with kill_lan / other starts
    let p = paths_from_app(app);
    let node = p.resources.join("node/bin/node");
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
    let child = Command::new(&node)
        .arg(&proxy)
        .arg(dsh_port)
        .arg(&token)
        .arg(&port)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("启动局域网转发器失败: {e}"))?;
    *mlock(&LAN_CHILD) = Some(child);
    LAN_ON.store(true, Ordering::SeqCst);
    sync_lan_check();
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

/// Modal dialog showing the access address + token; the token sits in an
/// editable field (selectable/copyable) with a one-click "复制令牌" action.
fn lan_info_dialog(token: &str, port: u16) {
    let ip = lan_ip().unwrap_or_else(|| "127.0.0.1".into());
    let script = format!(
        r#"display dialog "手机浏览器打开：" & return & "http://{ip}:{port}" & return & return & "访问令牌（可选中复制，或点复制按钮）：" default answer "{token}" buttons {{"关闭", "复制令牌"}} default button "复制令牌" with title "局域网访问" with icon note"#
    );
    if let Ok(out) = Command::new("osascript").args(["-e", &script]).output() {
        let s = String::from_utf8_lossy(&out.stdout);
        if s.contains("复制令牌") {
            copy_to_clipboard(token);
        }
    }
}

/// Copy text to the system clipboard via pbcopy.
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
// Login autostart (opt-in) & uninstaller (M4)
// ---------------------------------------------------------------------------

/// LaunchAgent plist path for login autostart.
fn login_plist() -> PathBuf {
    home_dir()
        .join("Library/LaunchAgents")
        .join(format!("{APP_ID}.plist"))
}

fn login_item_enabled() -> bool {
    login_plist().is_file()
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Write or remove the LaunchAgent plist (the actual mechanism); no GUI deps.
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
    // app 数据 + WebView 缓存/WebKit 状态（卸载器必须连缓存一起清干净）
    for dir in [
        p.app_data.clone(),
        home.join("Library/Caches").join(APP_ID),
        home.join("Library/WebKit").join(APP_ID),
    ] {
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

/// Ask the user how to uninstall (native macOS dialog with Chinese buttons).
/// Returns: Some(true) = uninstall + wipe ~/.dsh; Some(false) = uninstall, keep
/// ~/.dsh; None = cancelled.
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

/// Uninstall: teardown, trash the .app, then exit.
fn uninstall(app: &AppHandle) {
    let Some(wipe) = ask_uninstall() else {
        return; // cancelled
    };
    let p = paths_from_app(app);
    if let Err(e) = uninstall_teardown(&p, wipe) {
        update::notify("卸载未完成", &e);
        return;
    }

    trash_self();
    std::thread::sleep(Duration::from_millis(900)); // let the Finder script start
    // Remove the trashed app from the Dock's "recent applications" so the
    // dead icon does not linger (and refresh the Dock).
    clear_dock_recents();
    app.exit(0);
}

/// Move the running .app bundle to the Trash (via Finder), if we are bundled.
/// Returns true when the bundle was found and trashed.
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
        let _ = Command::new("osascript")
            .args(["-e", r#"tell application "DeepSeek Harness" to activate"#])
            .spawn();
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
            // 市场惯例分组：窗口/视图 → 功能 → 自启/局域网 → 卸载/退出
            let sep1 = tauri::menu::PredefinedMenuItem::separator(app)?;
            let sep2 = tauri::menu::PredefinedMenuItem::separator(app)?;
            let sep3 = tauri::menu::PredefinedMenuItem::separator(app)?;
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
                        let _ = Command::new("open").arg(p.app_data.join("logs")).spawn();
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
