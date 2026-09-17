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
mod workbench;

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

// ---------------------------------------------------------------------------
// macOS 原生窗口隐藏/唤醒（绕开 Tauri 2.11.5 的窗口 API 缺陷）
// ---------------------------------------------------------------------------
/// 实证（2026-09-13，CGWindowList 窗口服务器采样 + 日志）：
/// - `WebviewWindow::hide()` 调用后窗口仍在窗口服务器的 on-screen 列表中（无效）；
/// - `AppHandle::hide()`（NSApp hide）能隐藏窗口，但随后 `webview_windows()` 返回空、
///   `get_webview_window` 返回 None——召回时 reveal 误走「重建壳页」分支，新窗口
///   没有 child webview（用户看到「只有壳页背景、没有工作台」）。
///   因此直接调用 AppKit 的 orderOut: / makeKeyAndOrderFront:：两者实测可靠，
///   且完全不触碰 Tauri 的窗口注册表。
#[cfg(target_os = "macos")]
// objc 的 msg_send! 需要显式标注返回类型，所以本模块内普遍写作 `let _: () = msg_send![…]`，
// 会被 clippy::let_unit_value 判为「let 绑定到 unit 值」。改为去掉标注会让返回值类型
// 无法推断，故这里集中放行该 lint（仅此 FFI 模块）。
#[allow(clippy::let_unit_value)]
mod macwin {
    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct CGPoint {
        pub x: f64,
        pub y: f64,
    }
    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct CGSize {
        pub width: f64,
        pub height: f64,
    }
    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct CGRect {
        pub origin: CGPoint,
        pub size: CGSize,
    }

    use objc::runtime::Object;
    use objc::{msg_send, sel, sel_impl};
    use tauri::WebviewWindow;

    fn ns_ptr(w: &WebviewWindow) -> Option<*mut Object> {
        match w.ns_window() {
            Ok(p) if !p.is_null() => Some(p as *mut Object),
            _ => None,
        }
    }

    /// 隐藏窗口（w.hide() 的原意，但真正生效）。**仅可在主线程调用**
    /// （AppKit 要求；非主线程调用会 SIGTRAP："Must only be used from the main thread"）。
    pub fn order_out(w: &WebviewWindow) {
        if let Some(ns) = ns_ptr(w) {
            unsafe {
                let nil: *mut Object = std::ptr::null_mut();
                let _: () = msg_send![ns, orderOut: nil];
            }
        }
    }

    /// 唤醒窗口并置前（orderOut 过的窗口用 show() 可能不出现）。
    /// 同 order_out：**仅可在主线程调用**；后台线程请用 order_front_async。
    pub fn order_front(w: &WebviewWindow) {
        if let Some(ns) = ns_ptr(w) {
            unsafe {
                let nil: *mut Object = std::ptr::null_mut();
                let _: () = msg_send![ns, makeKeyAndOrderFront: nil];
            }
        }
    }

    /// 线程安全的唤醒：把 AppKit 调用排到主线程执行（reveal_main_window 可能
    /// 被 boot / 启动宽限等后台线程调用）。
    pub fn order_front_async(app: &tauri::AppHandle, w: &WebviewWindow) {
        let w2 = w.clone();
        let _ = app.run_on_main_thread(move || order_front(&w2));
    }

    /// 消除 WKWebView 导航/刷新期间的白底（点品牌刷新工作台闪白）。
    ///
    /// 上一版误用 `underPageBackgroundColor`——它只影响「页面自身无背景色」时的
    /// 底色，而 dsh 页面自带深色背景，所以对刷新白闪无效（用户实测仍闪）。
    /// 真正生效的做法是两层：
    ///   1. 关掉 WKWebView 自身的背景绘制（`drawsBackground=false`，WKWebView 的
    ///      事实标准 key）→ 页面空白时不再画白底；
    ///   2. 窗口背景色（NSWindow.backgroundColor）设为 #151517 → 空白时露出深色。
    ///      **仅主线程调用**；幂等。
    pub fn set_page_bg_dark(w: &WebviewWindow) {
        use objc::runtime::{Object, Sel};
        use objc::{class, msg_send, sel, sel_impl};
        // 同一个 extern 块上写多个 #[link] 只有第一个生效（rustc duplicated_attributes），
        // 所以分块声明：CGColorCreateSRGB 属于 CoreGraphics，objc_msgSend 来自 objc
        // crate 已链接的 libobjc（此处仅声明）。
        #[link(name = "CoreGraphics", kind = "framework")]
        extern "C" {
            fn CGColorCreateSRGB(r: f64, g: f64, b: f64, a: f64) -> *mut std::ffi::c_void;
            fn CGColorRelease(color: *mut std::ffi::c_void);
        }
        extern "C" {
            fn objc_msgSend();
        }
        let Some(ns) = ns_ptr(w) else {
            return;
        };
        unsafe {
            // #151517
            let cg = CGColorCreateSRGB(
                0x15 as f64 / 255.0,
                0x15 as f64 / 255.0,
                0x17 as f64 / 255.0,
                1.0,
            );
            if cg.is_null() {
                return;
            }
            let color: *mut Object = msg_send![class!(NSColor), colorWithCGColor: cg];
            // CGColorCreateSRGB 是 +1 所有权，NSColor 已把颜色值拷走：这里必须 Release，
            // 否则每次 reveal（托盘/Dock 召回都会调进来）泄漏一个颜色对象。
            CGColorRelease(cg);
            if color.is_null() {
                return;
            }
            // 1) 窗口底色
            let _: () = msg_send![ns, setBackgroundColor: color];
            // 2) 所有 WKWebView 关掉自身背景绘制（setValue:forKey: 双参数 → 手写 msgSend）
            let cv: *mut Object = msg_send![ns, contentView];
            if cv.is_null() {
                return;
            }
            let key: *mut Object = msg_send![class!(NSString), alloc];
            let kc = std::ffi::CString::new("drawsBackground").unwrap_or_default();
            let key: *mut Object = msg_send![key, initWithUTF8String: kc.as_ptr()];
            let no: *mut Object = msg_send![class!(NSNumber), numberWithBool: false];
            if key.is_null() || no.is_null() {
                // alloc/init 出来的 key 归我们所有：这两条早退路径同样要 release
                if !key.is_null() {
                    let _: () = msg_send![key, release];
                }
                return;
            }
            let sel_key = Sel::register("setValue:forKey:");
            let f: unsafe extern "C" fn(*mut Object, Sel, *mut Object, *mut Object) =
                std::mem::transmute(objc_msgSend as *const ());
            let n = clear_webview_bg(cv, f, sel_key, no, key);
            // alloc/init 的 NSString 用完释放（no 是 autoreleased 的 NSNumber，无需处理）
            let _: () = msg_send![key, release];
            crate::logln(&format!("[main] 已关闭 {n} 个 WKWebView 的背景绘制（消除刷新白闪）"));
        }
    }

    /// 把「壳页 webview」的高度限制为 logical_h（points；0 = 全窗）。
    ///
    /// 目的：消除「壳页与工作台两个 webview 在整个工作区重叠」造成的光标闪烁
    /// （AppKit 的光标从最上层设置了 cursor 的视图解析；两层交替 → 小手/箭头
    /// 闪动）。工作台可见时让壳页只覆盖顶栏，工作区就只剩工作台一个 webview。
    /// 壳页 webview 按 URL 区分（含 "shell.html"；工作台是 127.0.0.1:port）。
    /// 返回是否设置成功。**仅主线程调用**。
    pub fn set_shell_height(w: &WebviewWindow, logical_h: f64) -> bool {
        use objc::runtime::{Object, Sel};
        use objc::{class, msg_send, sel, sel_impl};
        #[link(name = "WebKit", kind = "framework")]
        extern "C" {
            fn objc_msgSend();
        }
        let Some(ns) = ns_ptr(w) else {
            return false;
        };
        unsafe {
            let cv: *mut Object = msg_send![ns, contentView];
            if cv.is_null() {
                return false;
            }
            // contentView 尺寸（AppKit 坐标：左下原点）
            let get_frame: unsafe extern "C" fn(*mut Object, Sel) -> CGRect =
                std::mem::transmute(objc_msgSend as *const ());
            let cb = get_frame(cv, Sel::register("frame"));
            let h = if logical_h <= 0.0 {
                cb.size.height
            } else {
                logical_h.min(cb.size.height)
            };
            let rect = CGRect {
                origin: CGPoint {
                    x: 0.0,
                    y: cb.size.height - h,
                },
                size: CGSize {
                    width: cb.size.width,
                    height: h,
                },
            };
            let set_frame: unsafe extern "C" fn(*mut Object, Sel, CGRect) =
                std::mem::transmute(objc_msgSend as *const ());
            let sel_frame = Sel::register("setFrame:");
            // 递归找壳页 WKWebView（URL 含 shell.html）
            let wk = class!(WKWebView);
            let mut ok = false;
            find_and_resize(cv, wk, sel_frame, set_frame, rect, &mut ok);
            ok
        }
    }

    /// 递归查找 URL 含 "shell.html" 的 WKWebView 并设置其 frame。
    unsafe fn find_and_resize(
        view: *mut Object,
        wk: &objc::runtime::Class,
        sel_frame: objc::runtime::Sel,
        set_frame: unsafe extern "C" fn(*mut Object, objc::runtime::Sel, CGRect),
        rect: CGRect,
        ok: &mut bool,
    ) {
        use objc::runtime::Object;
        use objc::{msg_send, sel, sel_impl};
        let is_wk: bool = msg_send![view, isKindOfClass: wk];
        if is_wk {
            let url: *mut Object = msg_send![view, URL];
            if !url.is_null() {
                let abs: *mut Object = msg_send![url, absoluteString];
                if !abs.is_null() {
                    let s: *const std::ffi::c_char = msg_send![abs, UTF8String];
                    if !s.is_null() {
                        let s = std::ffi::CStr::from_ptr(s).to_string_lossy().to_string();
                        if s.contains("shell.html") {
                            let _ = set_frame(view, sel_frame, rect);
                            *ok = true;
                        }
                    }
                }
            }
        }
        let subs: *mut Object = msg_send![view, subviews];
        if subs.is_null() {
            return;
        }
        let cnt: usize = msg_send![subs, count];
        for i in 0..cnt {
            let sub: *mut Object = msg_send![subs, objectAtIndex: i];
            if !sub.is_null() {
                find_and_resize(sub, wk, sel_frame, set_frame, rect, ok);
            }
        }
    }
    unsafe fn clear_webview_bg(
        view: *mut Object,
        f: unsafe extern "C" fn(*mut Object, objc::runtime::Sel, *mut Object, *mut Object),
        sel: objc::runtime::Sel,
        no: *mut Object,
        key: *mut Object,
    ) -> usize {
        use objc::runtime::Object;
        use objc::{class, msg_send, sel, sel_impl};
        let mut n = 0;
        let wk = class!(WKWebView);
        let is_wk: bool = msg_send![view, isKindOfClass: wk];
        if is_wk {
            let _ = f(view, sel, no, key);
            n += 1;
        }
        let subs: *mut Object = msg_send![view, subviews];
        if subs.is_null() {
            return n;
        }
        let cnt: usize = msg_send![subs, count];
        for i in 0..cnt {
            let sub: *mut Object = msg_send![subs, objectAtIndex: i];
            if !sub.is_null() {
                n += clear_webview_bg(sub, f, sel, no, key);
            }
        }
        n
    }
}

/// The running dsh child, kept so it is reaped and so we can kill it on exit.
static CHILD: Mutex<Option<Child>> = Mutex::new(None);
/// The parsed base URL of the running dsh web server.
static DSH_URL: Mutex<Option<String>> = Mutex::new(None);
/// True when we intentionally stopped the child (quit/restart), so EOF is not
/// treated as a crash.
static INTENTIONAL_STOP: AtomicBool = AtomicBool::new(false);
/// Consecutive crash count, used for restart backoff.
static CRASHES: AtomicU32 = AtomicU32::new(0);
/// 用户主动关闭主窗口（红点/⌘W）→「关到后台」：程序与 dsh 继续运行、托盘常驻；
/// 置位期间崩溃自愈 / 后台重启不得重新弹窗（托盘「显示主窗口」与 Dock 图标召回
/// 时清除）。这与「托盘退出=真正退出」的分层语义一致（文件头 Behaviour 注释）。
static USER_HIDDEN: AtomicBool = AtomicBool::new(false);

/// 主窗是否已首次显示。启动流程改为「等 dsh 首帧渲染完成后再显示窗口」（无缝
/// 衔接：用户看到窗口的第一眼就是 dsh 页面，而不是壳页占位/背景）；此标志用于
/// 20s 超时兜底（dsh 加载失败或极慢时仍要显示窗口，壳页有占位与引导兜底）。
static REVEALED: AtomicBool = AtomicBool::new(false);

/// 主窗 handle（创建成功后保存）。关键路径不按 label 查找主窗：这台
/// macOS 26 + Tauri 2.11.5 上 `AppHandle::get_webview_window(WINDOW_LABEL)` /
/// `webview_windows()` 返回空（窗口实际存在且在屏幕上、可交互——CGWindowList
/// 采样与窗口日志可证），导致 reveal 误判「主窗不存在」→ 走重建分支并 return，
/// 跳过显示窗口/恢复工作台（用户症状：托盘召回只见背景、无工作台）。
/// `get_window()`（普通窗口查找）在该环境正常，故 workbench 几何路径不受影响。
///
/// 用 `Mutex<Option<_>>` 而非 `OnceLock`：卸载流程会 `destroy()` 主窗，之后
/// （teardown 失败或重建壳页时）必须能把缓存换成新窗口——`OnceLock` 既清不掉也
/// 覆写不了，会让 `main_window()` 永远返回死句柄。
static MAIN_WIN: Mutex<Option<tauri::WebviewWindow>> = Mutex::new(None);

/// 取主窗 handle：优先用保存的 handle，回退按 label 查找（其他平台/版本）。
pub(crate) fn main_window(app: &AppHandle) -> Option<tauri::WebviewWindow> {
    if let Some(w) = mlock(&MAIN_WIN).as_ref() {
        return Some(w.clone());
    }
    app.get_webview_window(WINDOW_LABEL)
}

/// 记录主窗 handle（创建/重建成功后调用）。
pub(crate) fn set_main_window(w: &tauri::WebviewWindow) {
    *mlock(&MAIN_WIN) = Some(w.clone());
}

/// 主窗被销毁时清掉缓存句柄（卸载流程 `destroy()` 后调用），使后续
/// `main_window()` 返回 None → reveal 走重建分支，用户重新拿到窗口。
fn clear_main_window() {
    *mlock(&MAIN_WIN) = None;
}
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

/// 日志/诊断用：抹掉 URL 里的 `token=` 会话凭据。
///
/// dsh 的就绪 URL 形如 `http://127.0.0.1:<port>/?token=<secret>`，而日志会长期
/// 留存在 `<app-data>/logs/` 且常被一起贴出来排障——会话 token 不该落盘。
/// 只影响展示，不影响任何请求（真实 URL 仍原样交给 WebView）。
pub(crate) fn redact_token(url: &str) -> String {
    match url.find("token=") {
        None => url.to_string(),
        Some(i) => {
            let end = url[i..].find('&').map(|k| i + k).unwrap_or(url.len());
            format!("{}token=***{}", &url[..i], &url[end..])
        }
    }
}

/// 日志/诊断用：抹掉 URL 里可能携带的凭据。覆盖两类：
/// - `scheme://user:pass@host` 的 userinfo（registry 源可以这么写、插件 spec 可以是
///   带 token 的 git URL）；
/// - query 里敏感键的值（`token` / `key` / `access_token` / `auth` / `password`）。
///
/// 只影响写日志的文本，不改任何实际请求。
pub(crate) fn redact_url(url: &str) -> String {
    // 先拆 fragment，避免它被当成 query 最后一对 key=value 的一部分
    let (main, frag) = match url.split_once('#') {
        Some((m, f)) => (m, Some(f)),
        None => (url, None),
    };
    let (head, query) = match main.split_once('?') {
        Some((h, q)) => (h, Some(q)),
        None => (main, None),
    };
    let mut s = head.to_string();
    // 1) userinfo：只在 authority 段（`://` 之后、第一个 '/' 之前）里找 '@'，
    //    避免把路径里的 '@' 当凭据
    if let Some(i) = s.find("://") {
        let start = i + 3;
        let rest = &s[start..];
        let slash = rest.find('/').unwrap_or(rest.len());
        if let Some(at) = rest[..slash].find('@') {
            if rest[..at].contains(':') {
                s = format!("{}***@{}", &s[..start], &s[start + at + 1..]);
            }
        }
    }
    // 2) query：逐对 key=value 处理，敏感键的值一律替换为 ***
    if let Some(q) = query {
        let masked = q
            .split('&')
            .map(|kv| match kv.split_once('=') {
                Some((k, _)) if is_secret_key(k) => format!("{k}=***"),
                _ => kv.to_string(),
            })
            .collect::<Vec<_>>()
            .join("&");
        s = format!("{s}?{masked}");
    }
    if let Some(f) = frag {
        s = format!("{s}#{f}");
    }
    s
}

fn is_secret_key(k: &str) -> bool {
    matches!(
        k.to_ascii_lowercase().as_str(),
        "token" | "key" | "access_token" | "auth" | "password" | "pwd" | "secret"
    )
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
/// App 更新助手也用（传给 PowerShell 的 exe 路径同样不能带 verbatim 前缀）。
pub(crate) fn strip_verbatim(p: PathBuf) -> PathBuf {
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
/// 校验调用方是壳页 webview（拒绝工作台/弹窗等其它 webview 调用管理命令）。
///
/// 用 `tauri::Webview` 而不是 `tauri::WebviewWindow` 作命令参数是**必须的**：
/// `WebviewWindow::from_command` 内部会调 `Window::is_webview_window()`，而该判定
/// 要求「这个窗口下所有 webview 的 label 都等于窗口 label」。工作台一旦经
/// `window.add_child()` 挂到主窗（自身 label=workbench、window_label=main），主窗
/// 就不再满足该条件，于是**所有以 `WebviewWindow` 为参数的命令都会直接报
/// "current webview is not a WebviewWindow"**——插件安装/卸载、dsh 更新/切换、
/// 应用自更新、卸载、保存 Registry、双击品牌开浏览器会全部静默失效（工作台就绪后
/// 必然发生）。`Webview::from_command` 不走该判定，且 `webview.label()` 依然能精确
/// 区分壳页 / 工作台 / 弹窗，安全边界不变。
pub(crate) fn ensure_shell_webview(webview: &tauri::Webview) -> Result<(), String> {
    if webview.label() != WINDOW_LABEL {
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

/// 安装/更新 dsh 是否进行中（供重启命令避让：安装期间重启会撞上 pnpm 与 tmp 目录）。
pub(crate) fn setup_busy() -> bool {
    SETUP_BUSY.load(Ordering::SeqCst)
}

/// 卸载链进行中标志：ExitRequested 时 prevent_exit，防 teardown 被打断。
static UNINSTALLING: AtomicBool = AtomicBool::new(false);

/// 最近一次安装进度文本（setup_state_cmd 轮询读取；事件仅作增量）。
static SETUP_PROGRESS: Mutex<Option<String>> = Mutex::new(None);

/// 安装指定版本 dsh；进度经 `dsh:setup-progress` 推送（并写入
/// SETUP_PROGRESS 供轮询兜底）；成功后 `boot(app)` 启动工作台。
#[tauri::command]
async fn setup_dsh_cmd(
    app: AppHandle,
    webview: tauri::Webview,
    ver: String,
    registry: String,
) -> Result<(), String> {
    crate::ensure_shell_webview(&webview)?;
    // M2：入口白名单校验，防止任意字符串（npm 参数注入）进入安装流程
    if !crate::registry::valid_version(&ver) {
        return Err("版本号不合法".to_string());
    }
    // registry 与 save_registry_cmd 同规则（非空 / http(s) 前缀 / 长度上限），
    // 防任意串进 npm --registry / ureq URL（安全审查 should-fix）。
    let reg = registry.trim();
    valid_registry_url(reg)?;
    // App 更新期间拒绝：收尾阶段安装器会覆盖 $INSTDIR，此刻装 dsh 会把刚释放的
    // node.exe 映像锁占回去（安装器随即静默跳过该文件）；更新完成本进程就退出，操作也会被打断
    //（详见 appupdate::update_in_progress）
    if crate::appupdate::update_in_progress() {
        return Err("应用正在更新，请稍后再试".to_string());
    }
    if SETUP_BUSY.swap(true, Ordering::SeqCst) {
        return Err("已有一个安装正在进行中".to_string());
    }
    *mlock(&SETUP_PROGRESS) = Some("准备安装…".to_string());
    let app_after = app.clone(); // 供安装成功后切回主线程 boot（app 将被 move 进阻塞线程）
    let result = tauri::async_runtime::spawn_blocking(move || {
        let p = paths_from_app(&app);
        let reg = crate::registry::registry_url(Some(registry.trim()));
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
fn setup_cancel_cmd(webview: tauri::Webview) {
    if crate::ensure_shell_webview(&webview).is_err() {
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
        // 收敛：只返回最近 10 个（引导页下拉 + dsh tab 列表共用），避免版本非常多时
        // 全量拉取到内存。输入指定版本走 version_exists_cmd 单独校验。
        crate::registry::list_versions(&reg).unwrap_or_default().into_iter().take(10).collect()
    })
    .await
    .unwrap_or_default()
}

/// 校验指定版本是否存在于 registry（下载前校验,避免输入不存在的版本白白下载）。
#[tauri::command]
async fn version_exists_cmd(
    app: AppHandle,
    webview: tauri::Webview,
    ver: String,
) -> Result<bool, String> {
    crate::ensure_shell_webview(&webview)?;
    if !crate::registry::valid_version(&ver) {
        return Err("版本号不合法（应为 x.y.z 或 x.y.z-pre，如 0.1.1-rc.2）".to_string());
    }
    let p = paths_from_app(&app);
    let reg = crate::registry::registry_url(load_settings_at(&p.app_data).registry.as_deref());
    tauri::async_runtime::spawn_blocking(move || crate::registry::version_exists(&reg, &ver))
        .await
        .map_err(|e| format!("校验线程异常: {e}"))?
}

/// dsh 版本管理状态（壳页「更新」Tab 查询）。
#[derive(Serialize)]
struct DshState {
    current: String,
    latest: Option<String>,
    versions: Vec<String>,
    installing: bool,
    /// 已安装（本地 `dsh/v<ver>` 目录存在）的版本集合——前端据此把已装版本
    /// 的按钮显示为「切换」而非「安装」（配套 install_version 复用已有目录）。
    installed: Vec<String>,
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
    // 已安装版本集合（本地读取,同步；用于前端区分「切换」/「安装」）
    let installed = crate::dsh::installed_versions(&p);
    // 网络查询（list_versions）离开主线程：同步 command 在主线程执行，
    // 阻塞网络会冻结 UI（0.3.0 卡死根因，见 list_dsh_versions_cmd）。
    // 收敛：只返回最近 10 个（与 list_dsh_versions_cmd 一致，dsh tab 数据源在此，
    // 避免版本非常多时全量拉取到内存——spec V1「数据源根治」）。
    let versions = tauri::async_runtime::spawn_blocking(move || {
        crate::registry::list_versions(&reg).unwrap_or_default().into_iter().take(10).collect()
    })
    .await
    .unwrap_or_default();
    DshState {
        latest,
        current,
        versions,
        installing,
        installed,
    }
}

/// 安装指定版本 dsh 并自动重启工作台（新版本生效）。
#[tauri::command]
async fn update_dsh_cmd(app: AppHandle, webview: tauri::Webview, ver: String) -> Result<(), String> {
    crate::ensure_shell_webview(&webview)?;
    // M2：入口白名单校验，防止任意字符串（npm 参数注入）进入安装流程
    if !crate::registry::valid_version(&ver) {
        return Err("版本号不合法".to_string());
    }
    // App 更新期间拒绝：收尾阶段安装器会覆盖 $INSTDIR，此刻装 dsh 会把刚释放的
    // node.exe 映像锁占回去（安装器随即静默跳过该文件）；更新完成本进程就退出，操作也会被打断
    //（详见 appupdate::update_in_progress）
    if crate::appupdate::update_in_progress() {
        return Err("应用正在更新，请稍后再试".to_string());
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
    // App 更新期间不重启：更新收尾阶段安装器要覆盖 $INSTDIR，此刻拉起 dsh 会把 node.exe
    // 的映像锁占回去（本进程马上要退出，重启也没有意义）
    if crate::appupdate::update_in_progress() {
        logln("[setup] 应用更新中，跳过 dsh 安装完成后的自动重启");
        return Ok(());
    }
    // 安装成功 → 自动重启工作台（新版本生效）
    let _ = app_after
        .clone()
        .run_on_main_thread(move || restart_dsh(&app_after));
    Ok(())
}


/// 启动时清理「上一次运行残留的子进程」：dsh 服务进程 与 安装用的 pnpm。
/// 也被 App 自更新使用（Windows 安装器接手前必须先把这些子进程收掉，见
/// appupdate::install_windows）。
///
/// 为什么需要（实测复现）：dsh 是独立进程，App 被强杀（崩溃 / 强退 / dev 工具重启）时
/// 它不会跟着退出。残留进程占住 `~/.dsh/profiles/web` 的 profile 锁，本次启动的 dsh
/// 会秒退——表现为「dsh 连续崩溃 N 次」弹窗，用户以为应用坏了。
/// 单实例插件保证同一时刻只有一个 App，所以此刻任何「命令行命中本 App app-data 路径」
/// 且形如「dsh 服务进程」或「安装 dsh 的 pnpm」的进程必然是残留。只按本 App 的 app-data
/// 路径匹配，**不会误伤**用户终端里自己跑的那份 dsh / pnpm（不在此路径下）。
pub(crate) fn kill_stale_children(app: &AppHandle) {
    let closure_dir = paths_from_app(app).app_data.join("dsh");
    let self_pid = std::process::id();
    #[cfg(unix)]
    {
        let Ok(out) = Command::new("ps").args(["-Ao", "pid=,command="]).output() else {
            return;
        };
        let text = String::from_utf8_lossy(&out.stdout);
        let mut killed: Vec<u32> = Vec::new();
        for line in text.lines() {
            let Some((pid_s, cmd)) = line.trim_start().split_once(' ') else {
                continue;
            };
            let Ok(pid) = pid_s.trim().parse::<u32>() else {
                continue;
            };
            let stale = is_stale_dsh_cmdline(cmd, &closure_dir)
                || is_stale_pnpm_cmdline(cmd, &closure_dir);
            if pid == self_pid || !stale {
                continue;
            }
            let _ = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
            killed.push(pid);
        }
        if killed.is_empty() {
            return;
        }
        logln(&format!(
            "[main] 清理残留子进程 {killed:?}（dsh 占 profile 锁会连崩；pnpm 会撞 store/tmp）"
        ));
        std::thread::sleep(Duration::from_millis(800));
        for pid in killed {
            // 仍存活则升级到 SIGKILL。**动手前重新读一次该 pid 的命令行**：这 800ms
            // 里 pid 可能已被系统回收并分配给别的进程，只凭 `kill(pid,0)` 存活就补刀
            // 会误杀新进程（这些残留进程是上个实例的孤儿，其 pid 不在我们手上，可被复用）。
            let Ok(out) = Command::new("ps")
                .args(["-p", &pid.to_string(), "-o", "command="])
                .output()
            else {
                continue;
            };
            let cmd = String::from_utf8_lossy(&out.stdout).trim().to_string();
            let still_ours = !cmd.is_empty()
                && (is_stale_dsh_cmdline(&cmd, &closure_dir)
                    || is_stale_pnpm_cmdline(&cmd, &closure_dir));
            if still_ours && unsafe { libc::kill(pid as libc::pid_t, 0) } == 0 {
                let _ = unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
            }
        }
    }
    #[cfg(windows)]
    {
        let needle = closure_dir.display().to_string().replace('\'', "''");
        let script = format!(
            // 闭包路径用 [regex]::Escape 精确匹配，不用 -like：路径含 [ ] 时 -like 会把它
            // 当字符类，过滤恒不成立（漏杀）。
            "Get-CimInstance Win32_Process -ErrorAction SilentlyContinue | Where-Object {{ $_.ProcessId -ne {self_pid} -and $_.CommandLine -match [regex]::Escape('{needle}') -and ($_.CommandLine -match 'bin\\.js.*--profile' -or ($_.CommandLine -like '*pnpm*' -and $_.CommandLine -like '*--store-dir*' -and $_.CommandLine -like '*deepseek-ai/dsh*')) }} | ForEach-Object {{ taskkill /PID $_.ProcessId /T /F 2>$null | Out-Null }}"
        );
        let _ = no_console(Command::new("powershell"))
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .status();
    }
}

/// 命令行是否属于「本 App 安装 dsh 时启动的 pnpm」——纯函数便于单测。
/// 命中条件：本 App app-data 下的 dsh 路径（`--store-dir .../dsh/pnpm-store` 必然含它）
/// + pnpm + --store-dir + 安装目标 @deepseek-ai/dsh。四个条件同时在，误判概率极低。
// 调用点只在 unix 分支（清理残留子进程）+ 单测；Windows 的非测试构建里是死代码
#[cfg_attr(not(unix), allow(dead_code))]
fn is_stale_pnpm_cmdline(cmdline: &str, closure_dir: &std::path::Path) -> bool {
    cmdline.contains(&closure_dir.to_string_lossy().to_string())
        && cmdline.contains("pnpm")
        && cmdline.contains("--store-dir")
        && cmdline.contains("deepseek-ai/dsh")
}

/// 命令行是否属于「本 App 闭包启动的 dsh 进程」——纯函数便于单测。
/// 三个条件同时满足才算：本 App app-data 下的 dsh 路径 + bin.js + --profile。
// 同上：unix 分支与单测使用，Windows 非测试构建里是死代码
#[cfg_attr(not(unix), allow(dead_code))]
fn is_stale_dsh_cmdline(cmdline: &str, closure_dir: &std::path::Path) -> bool {
    cmdline.contains(&closure_dir.to_string_lossy().to_string())
        && cmdline.contains("bin.js")
        && cmdline.contains("--profile")
}

pub(crate) fn boot(app: AppHandle) {
    // 先清掉上一次运行残留的 dsh 进程：它们占着 profile 锁，会让本次启动的 dsh 秒退
    kill_stale_children(&app);
    // 上次 App 更新是否生效：Windows 安装器会杀掉本进程，只有这里能给出结论
    crate::appupdate::note_boot_after_update_attempt(&app);
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
                "无法启动内置 dsh：\n{e}\n\n日志位置：\n{}",
                logs.display()
            );
            // 致命错误：必须给出路，不能只弹一个「确定」就把用户丢在加载页
            //（实测反馈：点确定后既没有工作台、也退不出去）。「重试」重新拉起，
            //「退出」关闭应用（✕/Esc 也走重试，避免误退）。
            // 先显示主窗：启动失败时它可能还停在"隐藏创建"状态，否则弹窗会浮在空桌面上。
            reveal_main_window(&app, None);
            if fatal_choice(&app, "DeepSeek Harness Desktop 启动失败", &msg) {
                logln!("[boot] 启动失败 → 用户选择重试");
                CRASHES.store(0, Ordering::SeqCst);
                let a = app.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(500));
                    boot(a);
                });
            } else {
                logln!("[boot] 启动失败 → 用户选择退出");
                kill_dsh();
                app.exit(0);
            }
            return;
        }
    };
    let stdout = match child.stdout.take() {
        Some(s) => s,
        None => {
            // 没有 stdout 管道：这个 child 永远不会进 CHILD（下面那行才存），
            // 若直接 return 它会变成没人管的孤儿——继续占着 profile 锁，下次启动
            // 的 dsh 会秒退（kill_stale_children 也只能事后补救）。就地收干净。
            logln!("no stdout on child");
            let _ = child.kill();
            let _ = child.wait();
            return;
        }
    };
    *mlock(&CHILD) = Some(child);

    let reader = BufReader::new(stdout);
    for line in reader.lines() {
        match line {
            Ok(l) => {
                logln!("[dsh] {}", redact_token(&l));
                if let Some(idx) = l.find("http://127.0.0.1:") {
                    let url = l[idx..].split_whitespace().next().unwrap_or("").to_string();
                    if !url.is_empty() {
                        // 工作台通道：child webview 顶层导航到就绪 URL（first-party
                        // 认证由 WebView 原生完成；换端口/重启时 url_changed 判定后重导航）
                        crate::workbench::ensure_ready(&app, &url);
                        *mlock(&DSH_URL) = Some(url.clone());
                        CRASHES.store(0, Ordering::SeqCst); // healthy
                        // 无缝衔接：**不在这里显示窗口**。工作台在屏幕外预渲染，
                        // 等 dsh 首帧渲染完成（workbench page-load Finished + 延迟）
                        // 后由 workbench 模块统一 reveal——用户看到窗口第一眼即是
                        // dsh 页面，而不是壳页占位/背景。dsh:url 仍推给壳页（换端口/
                        // 重启时壳页需要更新地址）。
                        let app2 = app.clone();
                        let u = url.clone();
                        let _ = app2.clone().run_on_main_thread(move || {
                            if let Some(w) = main_window(&app2) {
                                let _ = w.emit("dsh:url", u);
                            }
                        });
                    }
                }
            }
            Err(_) => break,
        }
    }
    logln!("dsh process exited (stdout closed)");
    // 回收已退出的子进程：`Child` 被覆盖/丢弃不会 wait，自愈重启每崩一次就留一个
    // 僵尸（macOS/Linux 到父进程退出为止；Windows 是句柄泄漏）。CHILD 里此刻装的
    // 就是刚退出的这个（kill_dsh 路径已 take 过则拿到 None，正常）。
    if let Some(mut dead) = mlock(&CHILD).take() {
        let _ = dead.wait();
    }

    if !INTENTIONAL_STOP.swap(false, Ordering::SeqCst) {
        let n = CRASHES.fetch_add(1, Ordering::SeqCst) + 1;
        if n >= 5 {
            // give up: surface the logs instead of restarting forever
            logln!("dsh crashed {n} times in a row; giving up");
            let logs = paths_from_app(&app).app_data.join("logs");
            let msg = format!(
                "dsh 连续崩溃 {n} 次，已停止自动重启。\n\n日志位置：\n{}\n\n其中 dsh.log 记录了 dsh 的异常输出。",
                logs.display()
            );
            if fatal_choice(&app, "DeepSeek Harness Desktop 运行异常", &msg) {
                logln!("[boot] 运行异常 → 用户选择重试（重置崩溃计数）");
                CRASHES.store(0, Ordering::SeqCst);
                let a = app.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(500));
                    boot(a);
                });
            } else {
                logln!("[boot] 运行异常 → 用户选择退出");
                kill_dsh();
                app.exit(0);
            }
            return;
        }
        let delay_ms = (RESTART_BASE_MS * 2_u64.pow(n.min(6))).min(RESTART_MAX_MS);
        logln!("dsh crashed (count={n}); restarting in {delay_ms}ms");
        // 直接重启，不关闭/重建主窗：child webview 由 ensure_ready 按新 URL 重导航，
        // 窗口保持原可见状态（用户已关到后台时不会被弹回——USER_HIDDEN 语义）。
        let app2 = app.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(delay_ms));
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
    // 用户已主动关到后台（红点/⌘W）：仍推送最新 dsh URL（工作台换端口/重启后
    // 壳页兜底需要），但不重新弹出窗口——崩溃自愈、后台重启都不得打扰。
    if USER_HIDDEN.load(Ordering::SeqCst) {
        if let Some(u) = url {
            let _ = main_window(app).map(|w| w.emit("dsh:url", u));
        }
        return;
    }
    let Some(w) = main_window(app) else {
        // 兜底：主窗不存在（极罕见）时重建壳页。shell.js 自带 get_dsh_url
        // 轮询兜底，重建后无需再补发地址事件。
        // 注意：重建出的窗口没有 child webview（工作台需 ensure_ready 重建），
        // 若这里被频繁走到，用户会看到「只有壳页背景、没有工作台」。
        logln("[main] reveal: 主窗不存在 → 重建壳页（工作台需重新创建）");
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
            // 重建后必须写回缓存 handle：否则 main_window() 仍返回旧死句柄，
            // 后续 reveal/几何/托盘操作全部打在已销毁的窗口上。
            set_main_window(&w);
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
    // 「窗口本来是隐藏的」= 真正的召回（托盘/Dock/二次启动/开机首显）；窗口本来就
    // 可见的 reveal（dsh 重启后就绪、崩溃自愈完成）不应该关掉用户正开着的浮层、
    // 也不应该把原生工作台移进来盖住它（用户实测：插件装卸后「隔几秒又跳回工作台」）。
    let was_hidden = !visible;
    if !visible {
        let _ = w.center();
        let _ = w.show();
    }
    // 首次显示记一行：配合 [workbench] page loaded 的时间戳可验证「无缝衔接」
    // （窗口出现在 dsh 首帧渲染之后，约 page-load + 900ms）
    if !REVEALED.load(Ordering::SeqCst) {
        logln("[main] 首次显示主窗口（dsh 首帧已就绪 → 无缝衔接）");
    }
    // 页面底色设深色：消除 WKWebView 在导航/刷新期间露出的默认白底（闪白）
    #[cfg(target_os = "macos")]
    macwin::set_page_bg_dark(&w);
    // 原生唤醒：orderOut 隐藏过的窗口，Tauri 的 show() 不一定让它出现
    #[cfg(target_os = "macos")]
    macwin::order_front_async(app, &w);
    let _ = w.set_focus();
    REVEALED.store(true, Ordering::SeqCst);
    // macOS child webview 是独立 NSWindow，不随主窗恢复——同步恢复，否则工作台
    // 从后台召回后是空白页。
    // 恢复前先让壳页收起浮层（抽屉/命令面板）：show_child 是无条件把原生工作台
    // 移回窗口内，而原生视图盖在所有 HTML 之上——浮层若还开着就被压到下面，
    // 用户看到的是「工作台上残留一个输入框和按钮」（实测反馈）。浮层已由
    // 关到后台时的 __onHideToTray 收过一轮，这里是托盘/Dock 召回等其它入口的兜底。
    // 仅真正的召回才：收浮层 + 把工作台移回窗口。工作台此时该不该显示由
    // apply_bounds_on_main 按 SUPPRESSED 决定——浮层开着（SUPPRESSED=true）时它
    // 保持屏幕外，用户收起浮层后 show_workbench_cmd 再移入。窗口本来可见时这里
    // 什么都不做，避免把用户正开着的抽屉/面板关掉。
    if was_hidden {
        let _ = w.eval("window.__onMainReveal && window.__onMainReveal()");
        crate::workbench::show_child(app);
    }
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

/// 从 dsh 就绪 URL 提取端口（兼容 dsh stdout 截取出的完整 URL 串）。
/// dsh 0.1.2-alpha.5 起工作台地址带 ?token=<token> 本地鉴权参数（如
/// http://127.0.0.1:51940/?token=…），旧实现 rsplit(':') 会把
/// "51940/?token=…" 整段当端口解析而失败（wanted=None），工作台被误判为
/// 外链 → 导航策略拦截 iframe 并转交系统浏览器（卡"正在启动"+刷新跳浏览器）。
/// 改用标准 URL 解析取 port。
fn dsh_url_port(raw: &str) -> Option<u16> {
    tauri::Url::parse(raw).ok()?.port()
}

/// 是否为允许在 WebView 内导航的地址：App 内置页（tauri://localhost /
/// http(s)://tauri.localhost）与 **dsh 工作台源端口**（http://127.0.0.1:<dsh
/// 实际端口>）。只放行"当前 dsh 端口"，其它一律放行到外部。未就绪
/// （DSH_URL 空）或 host 不符时一律放行到外部。
fn is_internal_webview_url(url: &tauri::Url) -> bool {
    if url.scheme() == "tauri" {
        return true; // 内置页（tauri://localhost/...）
    }
    // dev 模式：tauri 内置静态服务在 loopback 随机端口提供 ui/ 目录，壳页 URL 为
    // http://127.0.0.1:<devport>/shell.html——debug 构建额外放行 loopback 任意端口；
    // release 不受影响（下方原逻辑仍严格匹配 dsh 端口，保持安全边界）。
    #[cfg(debug_assertions)]
    {
        if (url.scheme() == "http" || url.scheme() == "https")
            && matches!(url.host_str(), Some("127.0.0.1") | Some("localhost"))
        {
            return true;
        }
    }
    if url.scheme() == "http" || url.scheme() == "https" {
        match url.host_str() {
            Some("tauri.localhost") => return true,
            Some("127.0.0.1") | Some("localhost") => {
                // 仅放行与 dsh 工作台一致的目标端口
                let wanted = mlock(&DSH_URL).as_ref().and_then(|u| dsh_url_port(u));
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
    // 安装中的 pnpm 也是我们的子进程：退出路径必须一起结束，否则它会变成孤儿继续跑
    // （还留着 v<ver>-tmp 目录，下次安装可能撞 tmp/共享 store 锁）。幂等：没在装就是 no-op。
    crate::dsh::kill_setup_child_blocking();
    // 只有**确实有子进程要杀**时才置「主动停止」：该标志是给 boot 的读取循环用的——
    // 让它别把这次 EOF 当崩溃。没有子进程就没有待解释的 EOF，这时置位反而有害：
    // 标志会一直留到下一个 dsh 真的崩溃时被消费掉，那次崩溃就不再自愈、也不计数
    // （App 内更新失败后 respawn_dsh 会二次调用 kill_dsh，正是这种情形）。
    if mlock(&CHILD).is_none() {
        return;
    }
    INTENTIONAL_STOP.store(true, Ordering::SeqCst); // 让 boot 循环别把这次 EOF 当崩溃
    if let Some(mut c) = mlock(&CHILD).take() {
        // Windows：先按进程树整棵结束——node 派生的 dsh 执行器不会随
        // TerminateProcess 一起结束，残留进程会占住 ~/.dsh 的 profile 锁，导致下次
        // 启动「dsh 连续崩溃」。范式与 dsh.rs::cancel_install 一致；随后仍无条件
        // c.kill() 兜底（taskkill 失败时至少保证直接子进程结束）。
        #[cfg(target_os = "windows")]
        {
            let _ = no_console(Command::new("taskkill"))
                .args(["/PID", &c.id().to_string(), "/T", "/F"])
                .spawn();
        }
        let _ = c.kill();
        let _ = c.wait();
    }
}

pub(crate) fn restart_dsh(app: &AppHandle) {
    // 「主动停止」标志由 kill_dsh 在有子进程可杀时置位（见其注释），这里不再重复置位：
    // 无子进程时重复置位会让**下一次**真实崩溃被误判为主动停止（不自愈、不计数）。
    kill_dsh();
    // 关键：重启立刻清 DSH_URL——否则新 dsh 启动前仍持旧 URL,前端(updateDsh 非首装
    // 分支启动的轮询 / 引导等待态轮询)会拿到已死端口的旧 URL 而误重载。dsh:url 事件
    // 实测会丢失,前端主通道是轮询 get_dsh_url；先清空,等新 dsh 写入新 URL 后轮询才拿对。
    *mlock(&DSH_URL) = None;
    // 程序化重启 ≠ 用户关窗：**不能再走 `w.close()`**。close 会触发 CloseRequested，
    // 那里会 prevent_close 并把状态标成「用户关到后台」（USER_HIDDEN）+ 隐藏 child
    // （SUPPRESSED）；紧接着新 dsh 就绪时 reveal_main_window 被 USER_HIDDEN 拦掉、
    // child 也被 SUPPRESSED 挡住 → 窗口永久消失只剩托盘（dsh 更新 / 插件装卸后
    // 「App 不见了」）。
    // 这里改为：复位隐藏态 + 刷新壳页（等价于原先 close 想要的「壳页重置」效果），
    // 窗口留在屏幕上，新工作台就绪后照常移入。
    // 浮层占用工作区时（抽屉/命令面板打开——插件装卸、版本更新都是从这里发起的）
    // **不要**把工作台移回窗口内、也不要整页刷新壳页：
    //   · 工作台是原生视图，移回窗口会立刻盖住抽屉（用户实测「装完抖一下跳回工作台」）；
    //   · 壳页 location.reload() 会把插件输出面板、分段状态、日志全部清掉。
    // 等用户收起浮层时走 closeDrawer → maybeShowWorkbench → show_workbench_cmd →
    // show_child，工作台会带着新代码归位（新 dsh 就绪时已经按新 URL 重导航、并在
    // 屏幕外预渲染好）。窗口从后台被召回（__onMainReveal）仍由 reveal 路径处理。
    if !crate::workbench::is_suppressed() {
        crate::workbench::show_child(app);
        if let Some(w) = main_window(app) {
            let _ = w.eval("location.reload()");
        }
    }
    let handle = app.clone();
    std::thread::spawn(move || boot(handle));
}

/// 重新拉起 dsh 运行时，**不动壳页**：不 reload、不强制把工作台移回窗口内。
///
/// 与 `restart_dsh` 的区别：那条路径面向「用户显式重启」（菜单/命令面板），刷新壳页是
/// 期望行为；而 App 更新失败时走它会把刚推送的「更新失败：…」提示、进度条与锁定态
/// 一起冲掉，用户什么都看不到。工作台摆放仍由 boot → workbench::ensure_ready 负责
/// （SUPPRESSED 时保持屏幕外，不会踢掉抽屉）。
/// 调用点只在 Windows 的更新链路（appupdate::install_windows），其它平台放行 dead_code。
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub(crate) fn respawn_dsh(app: &AppHandle) {
    kill_dsh();
    *mlock(&DSH_URL) = None;
    let handle = app.clone();
    std::thread::spawn(move || boot(handle));
}

// ---------------------------------------------------------------------------
// P3：插件管理 / 卸载 已并入壳页（shell.html）阶段，不再有独立窗口；主窗为
// 壳页（shell.html），工作台是 workbench.rs 创建的原生 child webview（非 iframe）。
// 所有窗口（主窗 + 弹窗）统一由 uninstall_run 的 destroy 列表销毁。
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
    let Some(main) = main_window(app) else {
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
    // 先清残留同名窗口再建。残留窗口是「webview 已被关掉、只剩窗口壳」的形态，只存在于
    // windows 表；`get_webview_window` 查的是 webviews 表，因此查不到它，下面的 build()
    // 就会撞 label 冲突。这里按 windows 表查（该环境 `get_webview_window`/`webview_windows`
    // 恒为空，而 `get_window()` 正常，见 MAIN_WIN 注释），更可靠。
    if let Some(w) = app.get_window(MODAL_LABEL) {
        let _ = w.destroy();
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
fn modal_spec(webview: tauri::Webview) -> Result<ModalSpec, String> {
    if webview.label() != MODAL_LABEL {
        return Err("该操作仅限弹窗窗口使用".to_string());
    }
    mlock(&MODAL_SPEC)
        .clone()
        .ok_or_else(|| "无待显示内容".to_string())
}

/// 自绘弹窗：用户点击按钮后回传结果并关闭窗口（accept = 用户选择确定）。
#[tauri::command]
fn modal_respond(webview: tauri::Webview, accept: bool) -> Result<(), String> {
    if webview.label() != MODAL_LABEL {
        return Err("该操作仅限弹窗窗口使用".to_string());
    }
    if let Some(tx) = mlock(&MODAL_RESULT).take() {
        let _ = tx.send(accept);
    }
    mlock(&MODAL_SPEC).take();
    // 关的是承载弹窗的【窗口】，不能只 `webview.close()`：后者（tauri/src/webview/mod.rs
    // `Webview::close`）只销毁 webview 并从 webviews 表注销，窗口与 label `modal` 仍留在
    // windows 表——于是 `get_webview_window("modal")` 查不到、`WebviewWindowBuilder` 再建
    // 又撞 "a window with label `modal` already exists"，此后**所有**弹窗（卸载/更新/
    // 崩溃提示）全部打不开，直到重启 App。跨平台问题：`on_webview_close` 只动 webviews 表，
    // Windows 同样中招。
    let _ = webview.window().destroy();
    Ok(())
}

/// 致命错误弹窗（dsh 起不来 / 连续崩溃放弃重启）：主按钮「退出」、次按钮「重试」。
/// 返回 true = 用户选「重试」，false = 选「退出」。
///
/// 为什么这么映射：`show_modal_with_labels` 的返回值只代表**主按钮(ok)**是否被点，
/// 而 modal.js 里 ✕ / Esc 对 yesno 一律回 false。把「重试」放在 no 上，就能保证
/// 「顺手把弹窗关掉」不会把应用退掉（误关不该等于退出）；要真正退出必须点主按钮。
/// 用户反馈的原始问题：致命错误只弹一个「确定」，点完什么也没发生，dsh 已死、
/// 应用却还停在加载页 —— 既没有工作台也退不出去。
fn fatal_choice(app: &AppHandle, title: &str, msg: &str) -> bool {
    let body = format!("{msg}\n\n点「退出」关闭应用；点「重试」重新拉起 dsh（✕ 等同于重试）。");
    let quit = show_modal_with_labels(app, title, &body, "yesno", Some("退出"), Some("重试"));
    !quit
}

/// 显示自绘弹窗并阻塞等待用户按钮结果（替代 rfd 同步对话框）。
/// 在后台线程调用（boot 线程 / 更新检查线程）：窗口在主线程打开，本线程阻塞等结果。
/// kind="ok" 忽略按钮值；kind="yesno" 返回用户是否选择"确定"（主按钮）。
/// 按钮文案自定义版（卸载确认 / 致命错误等场景用）。
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
    // 两个独立标志：done = 主线程已执行完开窗；opened = 开窗成功。不能只用一个 bool——
    //「开窗失败」与「主线程还没执行」都是 false，会让下面的等待循环白等满 10s（用户症状：
    // 取消一次后再点卸载，卡约 10 秒才提示「已取消卸载」）。
    let done = Arc::new(AtomicBool::new(false));
    let opened = Arc::new(AtomicBool::new(false));
    let (done2, opened2) = (done.clone(), opened.clone());
    let app3 = app2.clone();
    let _ = app2.clone().run_on_main_thread(move || {
        opened2.store(open_modal_window(&app3), Ordering::SeqCst);
        done2.store(true, Ordering::SeqCst);
    });
    // 等主线程跑完开窗（主线程极忙时最多 10s）；执行完立即返回，开窗失败不空等
    for _ in 0..500 {
        if done.load(Ordering::SeqCst) {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    if !opened.load(Ordering::SeqCst) {
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
    /// 真实窗口是否属于「小窗」（窄 ≤900pt 或矮 ≤620pt）。见 shell.js 的 body.small-window：
    /// 壳页视口在「工作台可见且无浮层」时被裁到只剩顶栏（≈36pt），CSS 的 max-height
    /// 媒体查询会把这种正常状态误判成矮窗（抽屉打开那一帧闪成整幅），故高度条件改由
    /// Rust 按真实窗口尺寸判定后下发。
    window_small: bool,
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
        window_small: window_is_small(&app),
    }
}

/// 真实窗口是否「小窗」（窄 ≤900pt 或矮 ≤620pt）——供壳页切 body.small-window。
/// 为什么不直接用 CSS 媒体查询的 max-height：壳页 webview 在「工作台可见且无浮层」时
/// 被裁到只剩顶栏（视口≈36pt），media query 会把这种正常状态误判成矮窗。
fn window_is_small(app: &AppHandle) -> bool {
    let Some(w) = main_window(app) else {
        return false;
    };
    let Ok(size) = w.inner_size() else {
        return false;
    };
    let scale = w.scale_factor().unwrap_or(1.0);
    let (lw, lh) = (size.width as f64 / scale, size.height as f64 / scale);
    lw <= 900.0 || lh <= 620.0
}

/// 持久化 npm registry 源（安装/更新 dsh 的下载源）。校验非空且以 http(s)://
/// 开头，规范化后写入 settings.json；后续 list_dsh_versions_cmd / get_dsh_state
/// 都从 settings 读 registry，保存后自动生效。
/// Registry 源长度上限（前端输入框同值 maxlength）：registry URL 实际都很短，
/// 超长串只可能是误粘贴——显式报错比"静默截断成另一个 URL"安全得多。
const MAX_REGISTRY_LEN: usize = 400;

/// 校验 registry 源（保存与安装入口共用同一规则）：非空、http(s) 前缀、长度受限。
fn valid_registry_url(trimmed: &str) -> Result<(), String> {
    if trimmed.is_empty() {
        return Err("Registry 源不能为空".into());
    }
    if !(trimmed.starts_with("http://") || trimmed.starts_with("https://")) {
        return Err("Registry 源必须以 http:// 或 https:// 开头".into());
    }
    if trimmed.len() > MAX_REGISTRY_LEN {
        return Err(format!("Registry 源过长（最多 {MAX_REGISTRY_LEN} 字符）"));
    }
    Ok(())
}

/// 保存 registry 源（壳页「Registry 源设置」弹窗）。
/// 都从 settings 读 registry，保存后自动生效。
#[tauri::command]
fn save_registry_cmd(app: AppHandle, webview: tauri::Webview, registry: String) -> Result<(), String> {
    crate::ensure_shell_webview(&webview)?;
    let trimmed = registry.trim();
    valid_registry_url(trimmed)?;
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

/// V7：双击「工作台」tab → 用系统浏览器打开当前工作台 URL。
/// URL 为空（dsh 未就绪/启动中）时静默返回：双击无效、不提示（已确认）。
#[tauri::command]
fn open_workbench_url_cmd(webview: tauri::Webview) -> Result<(), String> {
    crate::ensure_shell_webview(&webview)?;
    if let Some(url) = get_dsh_url() {
        logln(&format!(
            "[main] 双击品牌 → 系统浏览器打开工作台: {}",
            redact_token(&url)
        ));
        open_url(&url);
    } else {
        logln("[main] 双击品牌 → 工作台地址未就绪，忽略");
    }
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
async fn confirm_uninstall_cmd(app: AppHandle, webview: tauri::Webview, wipe: bool) -> Result<(), String> {
    crate::ensure_shell_webview(&webview)?;
    // App 更新期间拒绝卸载：更新收尾阶段会退出本进程、由更新助手（临时区那个安装包）
    // 接管安装，此刻卸载会与它抢同一个目录/同一个安装包（轻则更新白下，重则留下
    // 半装状态）。前端在更新期间也锁了卸载按钮，这里是后端兜底——从系统「设置 → 应用」
    // 触发的卸载绕过 UI，那条链由 NSIS 侧车处理，不受本门约束。
    if crate::appupdate::update_in_progress() {
        return Err("应用正在更新，请稍后再试".to_string());
    }
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
        uninstall_run(app, webview, wipe).await
    } else {
        Ok(())
    }
}

#[tauri::command]
async fn uninstall_run(app: AppHandle, webview: tauri::Webview, wipe: bool) -> Result<(), String> {
    crate::ensure_shell_webview(&webview)?;
    // 卸载链进行中：ExitRequested 必须 prevent_exit（见 run 循环），
    // 否则 destroy 全部窗口会触发默认退出，teardown 永远来不及执行
    //（0.3.0 卸载"程序退出但没卸载"根因）。
    UNINSTALLING.store(true, Ordering::SeqCst);
    // 先销毁全部 WebView 窗口：释放 WebView2 用户数据目录（app_data 内）占用，
    // Windows 共享锁下不销毁则删除必然 oserror 32。
    // 主窗用保存的 handle；弹窗按 windows 表查——该环境 `get_webview_window()` 恒为空
    //（见 MAIN_WIN 注释），且弹窗点过按钮后就是「webview 已关、只剩窗口壳」的形态，
    // 只存在于 windows 表：用错查找就会漏销毁，Windows 上 WebView2 数据目录仍被占用，
    // 随后的 teardown 删 app 数据必然 oserror 32。
    if let Some(w) = main_window(&app) {
        let _ = w.destroy();
        // 主窗已销毁：清掉缓存 handle，否则后续 main_window() 仍返回这个死句柄
        // （reveal 的 is_visible/show 全部 Err 被吞，重建分支因 Some 永不进入），
        // 卸载失败时用户会落进「没有窗口也退不出去」的死角。
        clear_main_window();
    }
    if let Some(w) = app.get_window(MODAL_LABEL) {
        let _ = w.destroy();
    }
    std::thread::sleep(Duration::from_millis(300)); // 等 WebView2 进程释放数据目录
    // teardown（可能耗时：杀进程 + 删除重试）移到阻塞线程
    let app2 = app.clone();
    let teardown = tauri::async_runtime::spawn_blocking(move || -> Result<(), String> {
        // Windows 分支不使用 paths（数据清理移交 NSIS sidecar），前缀下划线避免 unused
        #[cfg(target_os = "windows")]
        let _p = paths_from_app(&app2);
        #[cfg(not(target_os = "windows"))]
        let p = paths_from_app(&app2);
        #[cfg(target_os = "windows")]
        {
            // Windows：数据清理（app_data / WebView2 缓存）**交给 NSIS 卸载器的
            // --self-uninstall-full sidecar**（installer-hooks.nsh PREUNINSTALL）——
            // App 侧不删大目录（dsh 闭包数百 MB），程序文件删除不被拖延
            // （v0.3.4 实测"App 退出后卸载器迟迟不出现"根因）。
            // 这里只删 ~/.dsh（wipe 时）：小目录秒级；sidecar 调用不带 --wipe。
            // 卸载器以 /S 静默运行（App 侧已确认过），不存在"取消"路径，故此处
            // 先删数据不会留下"数据已删、卸载却被取消"的中间态。
            if wipe {
                let dsh_home = crate::home_dir().join(".dsh");
                if dsh_home.exists() {
                    remove_dir_all_retry(&dsh_home)
                        .map_err(|e| format!("删除 ~/.dsh 失败: {e}"))?;
                }
            }
            Ok::<(), String>(())
        }
        #[cfg(not(target_os = "windows"))]
        {
            uninstall_teardown(&p, wipe)
        }
    })
    .await
    // 线程 panic（JoinError）与 teardown 自身失败走同一条错误分支：**不能在这里用 `?`
    // 提前返回**——复位 UNINSTALLING 的代码在下面，跳过去就成了「标志永真 → ExitRequested
    // 一直被 prevent_exit 拦住」，此时窗口已全毁，用户既没窗口也退不出（只能强杀）。
    .map_err(|e| format!("卸载线程异常：{e}"))
    .and_then(|r| r);
    if let Err(e) = teardown {
        // 卸载确认窗口已销毁，JS 无法回显：用系统通知兜底。
        // fail_uninstall 会复位标志（否则 ExitRequested 一直被 prevent_exit 拦截，用户
        // 退不出去）、重建主窗**并把工作台 child 接回来**（否则用户拿到空白工作区）。
        return fail_uninstall(&app, "卸载未完成", &e);
    }
    // teardown 成功：移入回收站 / 引导系统卸载，然后退出
    #[cfg(target_os = "macos")]
    {
        if !trash_self() {
            // Finder / 自动化权限失败时 .app 会留在 /Applications：数据已清但本体没动，
            // 必须给可操作的提示（通用分支一直这么做，macOS 分支此前是静默的）。
            notify(
                "应用未移入废纸篓",
                "应用数据已清理。App 本体未能自动移入废纸篓，请手动把它拖进废纸篓。",
            );
        }
        std::thread::sleep(Duration::from_millis(900));
        // WebKit 的网络进程退出时会把 cookie 落盘——"卸载后重装旧 cookie 还在"是用户实测
        // 过的老问题。等它退干净后**补删一轮文件类目标**（目录类已在 teardown 清过），
        // 这样即便它在 teardown 之后才把 cookie 写回来，也会被这轮清掉。
        let p = paths_from_app(&app);
        let (_, files) = uninstall_targets(&home_dir(), &p.app_data);
        let late = remove_uninstall_targets(&[], &files);
        if !late.is_empty() {
            logln(&format!(
                "[uninstall] 退出前仍有 cookie/偏好残留: {}",
                late.join("; ")
            ));
        }
        // 上面这两轮都是**本进程还在运行**时删的，而 WebKit 恰恰会在进程退出那一刻再落盘
        // 一次（本机实测：卸载后 cookie 文件又出现，mtime 与 App 退出同秒）。所以再挂一个
        // 脱离本进程的 shell，等我们退出后按同一份清单删最后一遍。
        let (dirs, files) = uninstall_targets(&home_dir(), &p.app_data);
        let cmd = post_exit_cleanup_cmd(&dirs, &files, &p.app_data, APP_ID);
        match std::process::Command::new("/bin/sh")
            .args(["-c", &cmd])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(_) => logln!("[uninstall] 已挂退出后清理（等本进程退出再删一遍 cookie/偏好）"),
            Err(e) => logln!("[uninstall] 退出后清理未能挂起: {e}"),
        }
        clear_dock_recents();
    }
    #[cfg(target_os = "windows")]
    {
        // 唯一卸载链：调用系统卸载器（uninstall.exe）完成程序文件删除（其 NSIS
        // PREUNINSTALL 钩子 → --self-uninstall-full sidecar 清理用户数据，见上方
        // spawn_blocking 注释）。这里退出自身并唤起卸载器，交给 NSIS 处理。
        //
        // `/S` 静默：App 侧已经弹过确认窗，卸载器再弹确认页会多出一条"取消"路径——
        // 而 wipe 分支在拉起卸载器之前就已删除 ~/.dsh，用户在确认页取消就会变成
        // "数据没了、程序还在、界面已退出"的中间态。静默化后该路径不存在。
        // 副作用：静默模式跳过确认页，模板的"删除应用数据"勾选框恒为未选中，故
        // app 数据/WebView2 缓存改由 PREUNINSTALL 的 sidecar 无条件清理。
        let exe = std::env::current_exe().unwrap_or_default();
        let uninstaller = exe.parent().unwrap_or(Path::new(".")).join("uninstall.exe");
        if uninstaller.is_file() {
            if let Err(e) = no_console(Command::new(&uninstaller)).arg("/S").spawn() {
                // 安装版但卸载器起不来（安全软件拦下 / 权限 / 文件损坏）：**不能**当成
                // 便携版去删数据——那会变成"数据没了、程序还在、提示还写着便携版"。
                // 程序文件与 app 数据都保持原样（仅「删除 ~/.dsh」档位在拉起卸载器**之前**
                // 就已按用户选择删掉 ~/.dsh，故在提示里如实说明），让用户重试或改走系统
                // 「设置 → 应用」。
                let wiped = if wipe {
                    "\n（已按你的选择删除 ~/.dsh；程序文件与应用数据保持原样）"
                } else {
                    ""
                };
                return fail_uninstall(
                    &app,
                    "卸载未完成",
                    &format!(
                        "无法启动系统卸载器：{e}\n可重试，或从「设置 → 应用」中卸载。{wiped}"
                    ),
                );
            }
            logln!("[uninstall] spawned system uninstaller (/S): {}", uninstaller.display());
        } else {
            // 便携版（解压即用，没有 uninstall.exe）：没有系统卸载器可委托，只能自己
            // 清理用户数据。此前这里只发通知、什么都不删，文案却写着"应用数据已清理"
            // ——既留下数百 MB 的 dsh 闭包与 WebView2 缓存，又误报。
            let p = paths_from_app(&app);
            if let Err(e) = uninstall_teardown(&p, wipe) {
                return fail_uninstall(&app, "卸载未完成", &e);
            }
            notify(
                "便携版：数据已清理",
                "程序文件未自动删除。便携版直接删除所在文件夹即可。",
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

/// 删除单个文件（卸载时的 cookie 存储 / 偏好 plist）：与目录一样做有限重试
/// ——浏览器/WebKit 进程可能短暂占用。失败由调用方汇总为"延迟清理"。
fn remove_file_retry(path: &std::path::Path) -> std::io::Result<()> {
    const ATTEMPTS: u32 = 5;
    const DELAY: Duration = Duration::from_millis(400);
    for i in 0..ATTEMPTS {
        match std::fs::remove_file(path) {
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

/// 卸载时要清理的路径——纯函数，便于审计与单测；调用方只按返回结果删除。
///
/// **安全约束（有单测守护，见 tests::uninstall_targets_only_touch_app_owned_paths）**：
/// 每个返回路径的最后一段都必须包含本 App 的 bundle id（`APP_ID`），或者就是
/// `app_data` 自身。换句话说，只会删「以本 App 命名」的目录/文件，
/// **不会**返回 `~/Library/Preferences`、`~/Library/HTTPStorages`、`~/Library` 这类
/// 共享父目录，也不含任何通配符 —— 因此不可能误删其它 App 或系统数据。
/// 返回 (目录列表, 文件列表)。
// Windows 分支只用 app_data（`home` 仅 macOS 的 Library 路径需要）——CI 在 Windows 上
// 以 `-D warnings` 跑 clippy，未使用参数会直接失败，故精确豁免而不改签名。
#[cfg_attr(target_os = "windows", allow(unused_variables))]
fn uninstall_targets(
    home: &std::path::Path,
    app_data: &std::path::Path,
) -> (Vec<PathBuf>, Vec<PathBuf>) {
    // app 数据目录（macOS: ~/Library/Application Support/<id>，Windows: %APPDATA%\<id>）
    let mut dirs = vec![app_data.to_path_buf()];
    #[cfg(target_os = "macos")]
    {
        let lib = home.join("Library");
        dirs.push(lib.join("Caches").join(APP_ID)); // WebView 资源缓存
        dirs.push(lib.join("WebKit").join(APP_ID)); // WebView 存储（LocalStorage/IndexedDB）
        // HTTPStorages 有**两种形态**，都要清：`<id>.binarycookies`（文件，见下方 files）
        // 与 `<id>/`（目录，内含 httpstorages.sqlite）。本机实测该目录下 91 个目录形态、
        // 12 个 `.binarycookies` 文件形态——只清文件形态会漏（沙箱实测：造出目录形态后
        // 卸载不删，加上本项后被清）。
        dirs.push(lib.join("HTTPStorages").join(APP_ID));
        dirs.push(
            lib.join("Saved Application State")
                .join(format!("{APP_ID}.savedState")),
        );
    }
    #[cfg(target_os = "windows")]
    {
        // WebView2 的用户数据/缓存落在 %LOCALAPPDATA%\<id>
        // 空串要挡掉：`PathBuf::from("").join(APP_ID)` 是**相对路径**，卸载时会对
        // 相对路径做 remove_dir_all（相对当前工作目录）——宁可不删也不能删错东西。
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            let p = PathBuf::from(local);
            if !p.as_os_str().is_empty() && p.is_absolute() {
                dirs.push(strip_verbatim(p).join(APP_ID));
            }
        }
    }
    #[cfg(target_os = "macos")]
    let files = vec![
        // 工作台认证用的 cookie 存储 —— 就是当初导致工作台空白（431）的那个文件。
        // 卸载不删它，重装后旧 cookie 仍在（用户实测卸载残留）。
        home.join("Library/HTTPStorages")
            .join(format!("{APP_ID}.binarycookies")),
        // 偏好（窗口位置等）
        home.join("Library/Preferences")
            .join(format!("{APP_ID}.plist")),
    ];
    #[cfg(not(target_os = "macos"))]
    let files: Vec<PathBuf> = Vec::new();
    (dirs, files)
}

/// 按 uninstall_targets 的结果删除：目录与文件各自有限重试，失败项汇总返回
/// （调用方决定是否补删一轮）。失败不中断整体卸载。
fn remove_uninstall_targets(dirs: &[PathBuf], files: &[PathBuf]) -> Vec<String> {
    let mut leftovers: Vec<String> = Vec::new();
    // 只碰绝对路径：相对路径（例如环境变量为空串拼出来的名字）会被解析到当前工作
    // 目录，等于删用户看不明白的东西。卸载路径上的"宁可不删"优先于"删干净"。
    let safe = |p: &PathBuf| {
        if p.is_absolute() {
            true
        } else {
            logln(&format!("[uninstall] 跳过非绝对路径: {}", p.display()));
            false
        }
    };
    // 按**实际形态**删，而不是按登记表：同一个位置在不同系统/版本上可能是文件也可能是
    // 目录（HTTPStorages 的 `<id>.binarycookies` 与 `<id>/` 就是典型）。按登记表硬删会
    // 报 NotADirectory/IsADirectory —— 变成"明明能删却报残留"，而且真的删不掉。
    let remove = |p: &PathBuf| {
        if p.is_dir() {
            remove_dir_all_retry(p)
        } else {
            remove_file_retry(p)
        }
    };
    for dir in dirs.iter().chain(files.iter()) {
        if safe(dir) && dir.exists() {
            if let Err(e) = remove(dir) {
                leftovers.push(format!("{}（{e}）", dir.display()));
            }
        }
    }
    leftovers
}

/// 卸载失败后的恢复：重建主窗 + 把工作台 child 也接回来。
///
/// 为什么不能只调 `reveal_main_window`：`uninstall_run` 动手前就 `destroy()` 了全部
/// webview（为释放 WebView2 数据目录占用），而重建出的壳页**没有 child webview**
/// （reveal_main_window 自己的注释也这么写）——`ensure_ready` 只在 dsh 输出就绪 URL 时
/// 被调用，而 dsh 此刻还活着、不会再输出 → 用户会拿到一个**工作区空白的窗口**，只能
/// 靠「重启 dsh」自救。这里直接用当前已知的就绪 URL 把 child 重建出来：不重启 dsh，
/// 也就不会打断正在跑的任务。
fn restore_after_failed_uninstall(app: &AppHandle) {
    let url = mlock(&DSH_URL).clone();
    // 两步都要做，但**不能在同一个主线程闭包里做**：`ensure_ready` 内部会清历史 dsh 会话
    // cookie（`purge_stale_auth_cookies` 走 WebView 的 cookie API、可能阻塞），其注释明确
    // 要求"必须在主线程之外调用"。这里先往主线程排队重建主窗，然后在**当前线程**直接调
    // ensure_ready —— 它内部同样往主线程排队建 child，两次投递按 FIFO，所以建 child 时
    // 主窗一定已经存在。
    let app2 = app.clone();
    let url2 = url.clone();
    let _ = app
        .clone()
        .run_on_main_thread(move || reveal_main_window(&app2, url2.as_deref()));
    if let Some(u) = url.as_deref() {
        crate::workbench::ensure_ready(app, u);
    }
}

/// shell 单引号字面量（POSIX）：把 `'` 写成 `'\''`。
/// 只在 macOS 的卸载收尾里用（Windows 不需要：那边的删除由独立 sidecar 在杀进程之后做，
/// 不存在"App 退出时把数据写回来"的时序）。非 macOS 构建放行 dead_code，否则 CI 的
/// windows job（clippy -D warnings）会报「never used」——本机 macOS 编译看不到。
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn sh_quote(p: &std::path::Path) -> String {
    format!("'{}'", p.display().to_string().replace('\'', "'\\''"))
}

/// 生成「App 退出之后」再删一遍的命令（纯函数便于单测）。
///
/// 为什么需要：WebKit/AppKit 会在**本进程退出时**才把 cookie 等数据落盘——实测本机卸载后
/// `~/Library/HTTPStorages/<id>.binarycookies` 又出现，mtime 与 App 退出同一秒（而它已被
/// 前面两轮删除清掉过）。那一刻进程内已经没有"再删一次"的机会了，所以把同一份目标清单
/// 交给一个脱离本进程的短命 shell：睡几秒（等我们彻底退出）后 `rm` 一遍。
///
/// 安全边界与 `uninstall_targets` 一致（绝对路径 + 末段含 bundle id）；额外**排除 app_data**：
/// 万一用户在这几秒里重装并启动，新实例刚写下的 app 数据不能被误删。
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn post_exit_cleanup_cmd(dirs: &[PathBuf], files: &[PathBuf], keep: &std::path::Path, app_id: &str) -> String {
    let mut cmd = String::from("sleep 3;");
    for d in dirs.iter().filter(|d| d.as_path() != keep) {
        cmd.push_str(&format!(" rm -rf {}", sh_quote(d)));
    }
    for f in files {
        cmd.push_str(&format!(" rm -f {}", sh_quote(f)));
    }
    // 偏好域交给 cfprefsd：只删文件的话它可能把内存里的域再写回磁盘
    cmd.push_str(&format!(" defaults delete {app_id} 2>/dev/null;"));
    cmd
}

/// 卸载失败后的统一收尾：复位「卸载中」标志、系统通知、恢复可用界面，并回错。
/// 三条失败路径（数据清理失败 / 系统卸载器起不来 / 便携版清理失败）共用，
/// 避免各写一遍时漏掉某一步。
fn fail_uninstall(app: &AppHandle, title: &str, msg: &str) -> Result<(), String> {
    UNINSTALLING.store(false, Ordering::SeqCst);
    notify(title, msg);
    restore_after_failed_uninstall(app);
    Err(msg.to_string())
}

fn uninstall_teardown(p: &Paths, wipe_dsh: bool) -> Result<(), String> {
    kill_dsh();
    let home = home_dir();
    // 关闭日志句柄：当前进程持有 logs/launcher.log 与 logs/install.log，Windows 共享锁下
    // 不关则删除 app_data 必然 oserror 32。后续 logln 不再写文件（卸载流程无需日志）。
    *mlock(&LOG_FILE) = None;
    crate::dsh::close_install_log();
    #[cfg(target_os = "macos")]
    {
        // 偏好域交给 cfprefsd 删除：只删文件的话它可能把缓存里的域写回磁盘，
        // 于是"卸载后 Preferences 又出现"。域名为本 App 的 bundle id，精确无副作用。
        let _ = std::process::Command::new("defaults")
            .args(["delete", APP_ID])
            .status();
    }
    let (dirs, files) = uninstall_targets(&home, &p.app_data);
    // 目录/文件删除：失败不中断整体卸载。Caches/WebKit 常被 WebView 进程延迟占用
    // （destroy 后 300ms 不够释放），失败项再等 1s 补删一轮（实测可清）。
    let mut leftovers = remove_uninstall_targets(&dirs, &files);
    if !leftovers.is_empty() {
        std::thread::sleep(Duration::from_millis(1000));
        leftovers = remove_uninstall_targets(&dirs, &files);
    }
    // 临时区里的更新包（几十 MB）也一并清掉：留给它们只有"卸载后还在"这一种结局
    // （启动清扫只在 App 还在运行时发生）。App 内卸载在更新期间被前端锁 + 后端门挡住
    // （见 uninstall_run 的 update_in_progress），所以按名字强制清、不等 TTL；
    // 从系统「设置 → 应用」触发的卸载绕过 UI，此时若有更新包正被占用，删除会失败并留日志。
    crate::appupdate::sweep_update_packages(true);
    // Windows：模板只在勾选"删除应用数据"时才清 MANU 注册表键，而 /S 下该勾选框恒为
    // 未选中（确认页不出现）→ App 内卸载必然留下 `HKCU\Software\dsh-desktop\DeepSeek
    // Harness Desktop`（含 InstallLocation / Installer Language）。这里按同一语义补清：
    // 先删产品键，父键**仅在确实为空时**删（对应模板的 DeleteRegKey /ifempty）。
    #[cfg(target_os = "windows")]
    cleanup_registry_keys();
    if !leftovers.is_empty() {
        let msg = format!(
            "以下数据被占用未能删除，重启电脑后即可手动清理：\n{}",
            leftovers.join("\n")
        );
        logln!("[uninstall] 残留: {msg}");
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
    let py = r#"import plistlib, sys
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
/// 清掉安装器留在 HKCU 的 MANU 注册表键（卸载路径专用）。
///
/// 键名来源（可核对，不是猜的）：tauri-bundler 把模板的 `{{manufacturer}}` 填成
/// `publisher().unwrap_or(bundle_id.split('.').nth(1))`（`nsis/mod.rs:269-271`），本项目
/// 未配 publisher、bundle id 为 `com.dsh-desktop.app` → `dsh-desktop`；
/// 模板里 `MANUPRODUCTKEY = Software\${MANUFACTURER}\${PRODUCTNAME}`
/// （`installer.nsi:67-68`，PRODUCTNAME = "DeepSeek Harness Desktop"）：安装时把
/// `$INSTDIR` 写成该键的**默认值**（`installer.nsi:682`），另有 MUI 语言选择读写的
/// `Installer Language`（`:164` 的 `MUI_LANGDLL_REGISTRY_KEY`）。`InstallLocation` 是
/// 另一处——在 `UNINSTKEY`（卸载项）下、由 `:705` 写入，模板删除 ARP 键时一并处理。
///
/// 删除语义对齐模板：产品键直接删；父键**只在没有子键/值时才删**（模板用的是
/// `DeleteRegKey /ifempty`），避免误删同名厂商键下的其它内容。
/// 用 PowerShell（本模块已有依赖）：注册表 cmdlet 在无提权下可写 HKCU。
fn cleanup_registry_keys() {
    let script = "$ErrorActionPreference='SilentlyContinue'; \
         Remove-Item -LiteralPath 'HKCU:\\Software\\dsh-desktop\\DeepSeek Harness Desktop' -Recurse -Force; \
         $k = Get-Item -LiteralPath 'HKCU:\\Software\\dsh-desktop'; \
         if ($k -and $k.SubKeyCount -eq 0 -and $k.ValueCount -eq 0) { Remove-Item -LiteralPath 'HKCU:\\Software\\dsh-desktop' -Force }";
    match no_console(Command::new("powershell"))
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .status()
    {
        Ok(s) if s.success() => logln!("[uninstall] 注册表键已清理"),
        Ok(s) => logln!("[uninstall] 注册表清理退出码={:?}", s.code()),
        Err(e) => logln!("[uninstall] 注册表清理未能运行: {e}"),
    }
}

#[cfg(target_os = "windows")]
/// 结束其它运行中的本应用实例及其子进程树（`--self-uninstall-full` 卸载 sidecar 用）。
/// 目的：释放 `$INSTDIR` 程序文件 / app 数据 / WebView2 缓存的文件锁 —— 否则
/// NSIS 删文件时因占用而失败（"右键→卸载 无反应"根因之一）。全程无 GUI/无窗口。
/// 只按「本应用进程名 / 本项目 node 脚本命令行特征」匹配，避免误杀用户其它 node。
fn kill_other_app_instances() {
    let self_pid = std::process::id();
    // node 的匹配必须带本 App 的闭包目录：只匹配 `bin.js.*--profile web` 会把**用户
    // 自己在终端里跑的 dsh**（同一 profile web、却装在别处）一起杀掉。与
    // `is_stale_dsh_cmdline` 的判定保持一致：命令行里必须出现本 App 的 app-data
    // 闭包路径才算"我们的 dsh"。
    let closure_dir = paths_from_cli().app_data.join("dsh");
    let closure_dir = closure_dir.to_string_lossy().replace('\'', "''"); // PowerShell 单引号转义
    let script = format!(
        r#"$self={self_pid};
Get-CimInstance Win32_Process -ErrorAction SilentlyContinue | Where-Object {{
  ($_.ProcessId -ne $self) -and (
    $_.Name -eq 'dsh-desktop.exe' -or
    ($_.Name -eq 'node.exe' -and $_.CommandLine -match 'bin\.js.*--profile' -and $_.CommandLine -like '*{closure_dir}*')
  )
}} | ForEach-Object {{
  taskkill /PID $_.ProcessId /T /F 2>$null | Out-Null
}}"#,
        self_pid = self_pid,
        closure_dir = closure_dir
    );
    let status = no_console(Command::new("powershell"))
        .arg("-NoProfile")
        .arg("-NonInteractive")
        .arg("-Command")
        .arg(&script)
        .status();
    match status {
        Ok(s) if s.success() => logln!("[uninstall-full] killed other instances ok"),
        Ok(s) => {
            logln!("[uninstall-full] kill other instances exit={:?}", s.code());
            kill_other_app_instances_no_powershell(self_pid);
        }
        Err(e) => {
            logln!("[uninstall-full] kill other instances failed to run: {e}");
            kill_other_app_instances_no_powershell(self_pid);
        }
    }
    // 给文件句柄释放留一点时间
    std::thread::sleep(Duration::from_millis(600));
}

/// `kill_other_app_instances` 的 **PowerShell 不可用**兜底（策略禁用 / WMI 故障）。
///
/// 为什么需要：主路径依赖 PowerShell + `Get-CimInstance`（因为要按命令行区分"我们的 node"
/// 与用户自己的 node）。一旦 PowerShell 起不来，主程序实例就不会被结束 → NSIS 删
/// `$INSTDIR` 时文件被占用 → 静默跳过（NSIS 的 `Delete` 失败不弹框）→ 卸载"成功"但目录还在。
/// 兜底只用 **tasklist + taskkill**（系统自带、无策略面）：按映像名列出 pid，**排除自己**
/// （sidecar 与主程序同名，用 pid 排除），逐个 `/T` 树杀——树杀同时覆盖主程序拉起的
/// node/dsh 子进程。不按 node 名字泛杀（那会误伤用户自己的 node），与主路径同一取舍。
#[cfg(target_os = "windows")]
fn kill_other_app_instances_no_powershell(self_pid: u32) {
    let exe_name = std::env::current_exe()
        .ok()
        .map(strip_verbatim)
        .and_then(|p| p.file_name().map(|s| s.to_string_lossy().to_string()))
        .unwrap_or_else(|| "dsh-desktop.exe".to_string());
    let out = no_console(Command::new("tasklist"))
        .args(["/FI", &format!("IMAGENAME eq {exe_name}"), "/FO", "CSV", "/NH"])
        .output();
    let Ok(out) = out else {
        logln!("[uninstall-full] tasklist 兜底也失败");
        return;
    };
    let pids = tasklist_pids(&String::from_utf8_lossy(&out.stdout), self_pid);
    logln(&format!("[uninstall-full] tasklist 兜底命中 pid={pids:?}"));
    for pid in pids {
        let _ = no_console(Command::new("taskkill"))
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .status();
    }
}

/// 从 `tasklist /FO CSV /NH` 输出里取 pid（排除 exclude）。
/// 纯函数便于单测：每行形如 `"dsh-desktop.exe","1234","Console","1","12,345 K"`；
/// 无匹配时 tasklist 会输出一行 `INFO: No tasks are running...`（非 CSV，直接忽略）。
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn tasklist_pids(csv: &str, exclude: u32) -> Vec<u32> {
    let mut pids = Vec::new();
    for line in csv.lines() {
        let mut fields = line.split("\",\"");
        let (Some(_name), Some(pid)) = (fields.next(), fields.next()) else {
            continue;
        };
        let pid = pid.trim_matches('"').trim();
        if let Ok(pid) = pid.parse::<u32>() {
            if pid != exclude {
                pids.push(pid);
            }
        }
    }
    pids
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
    // 单实例由 tauri-plugin-single-instance 接管（下方 .plugin()）：已有实例
    // 被二次启动时，插件在回调里 reveal 主窗口并聚焦（macOS/Windows 统一，
    // 替换了旧的"手动锁 + macOS osascript activate / Windows 仅退出"方案）。
    // CLI hooks 在插件注册前 return，保持原有绕过逻辑不变。

    tauri::Builder::default()
        // 单实例：已有实例被二次启动时聚焦主窗口（跨平台统一激活）
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            // 二次启动 = 用户明确想看到窗口，先清「关到后台」标记再 reveal。
            // 不清则 reveal 会在 USER_HIDDEN 分支直接早退（只发 dsh:url、不弹窗）——
            // 表现为「关到托盘后再次双击图标毫无反应」（Windows 没有 RunEvent::Reopen
            // 兜底，只能在托盘右键召回；macOS 双击 .app 会走 Reopen 清标记，故只在
            // Windows 上必现）。与托盘「显示主窗口」同一语义。
            USER_HIDDEN.store(false, Ordering::SeqCst);
            crate::reveal_main_window(app, mlock(&DSH_URL).as_deref());
        }))
        .invoke_handler(tauri::generate_handler![
            plugin::plugin_op,
            plugin::plugin_list_cmd,
            plugin::plugin_check_updates_cmd,
            appupdate::check_app_update_cmd,
            appupdate::app_update_probe_cmd,
            appupdate::app_update_cmd,
            uninstall_run,
            confirm_uninstall_cmd,
            modal_spec,
            modal_respond,
            get_dsh_url,
            get_shell_state,
            save_registry_cmd,
            open_browser_cmd,
            open_workbench_url_cmd,
            open_repo_cmd,
            get_dsh_state,
            update_dsh_cmd,
            setup_dsh_cmd,
            setup_state_cmd,
            setup_cancel_cmd,
            list_dsh_versions_cmd,
            version_exists_cmd,
            workbench::show_workbench_cmd,
            workbench::hide_workbench_cmd,
            workbench::workbench_reload_cmd,
            workbench::dsh_restart_cmd,
            workbench::workbench_ready_cmd,
            workbench::workbench_set_collapsed_cmd,
        ])
        .setup(|app| {
            // 托盘最小集：显示主窗口 / 退出（左键点击即显示主窗口；
            // 「管理台」与「显示主窗口」功能重复，已移除）。
            let show = MenuItem::with_id(app, "show", "显示主窗口", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show, &quit])?;
            let _tray = TrayIconBuilder::with_id("tray")
                .icon(app.default_window_icon().unwrap().clone())
                // 悬停提示 app 名称（Windows/Linux 生效；macOS 状态项无 tooltip，忽略）
                .tooltip("DeepSeek Harness Desktop")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "quit" => {
                        kill_dsh();
                        app.exit(0);
                    }
                    // 显示主窗口（左键点击托盘图标同样走此路径；管理台已并入）。
                    // 用户主动召回：清除「关到后台」标记，允许 reveal 弹出。
                    "show" => {
                        USER_HIDDEN.store(false, Ordering::SeqCst);
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
                        USER_HIDDEN.store(false, Ordering::SeqCst);
                        reveal_main_window(app, mlock(&DSH_URL).as_deref());
                    }
                })
                .build(app)?;

            let handle = app.handle().clone();
            init_log(&paths_from_app(app.handle()));
            // 清扫更新器遗留的安装包（Windows 上「装完即删」执行不到，靠这里兜底）
            crate::appupdate::sweep_stale_installers();

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
                // 标准 Edit 菜单（macOS 文本编辑必需）——缺它时 Cmd+C/V/X/A
                // 被菜单系统吞掉，壳页/输入框内快捷键复制粘贴全部失效（实测反馈）。
                let edit_menu = SubmenuBuilder::new(app, "编辑")
                    .undo()
                    .redo()
                    .separator()
                    .cut()
                    .copy()
                    .paste()
                    .select_all()
                    .build()?;
                // 「视图」菜单：⌘K 打开管理面板。壳页的 window keydown 只在壳页有
                // 焦点时收到——焦点在工作台（独立 webview）时按键不会冒泡过来
                // （shell.js 里对 ⌘C 的注释已记录同一限制），所以 ⌘K 必须挂到
                // 系统菜单快捷键（系统级触发，不受 webview 焦点影响），由 Rust 转发
                // 给壳页的全局函数（不新增 IPC 命令/事件）。
                let manage_item = MenuItem::with_id(
                    app,
                    "menu-manage",
                    "管理面板",
                    true,
                    Some("CmdOrCtrl+K"),
                )?;
                let view_menu = SubmenuBuilder::new(app, "视图").item(&manage_item).build()?;
                let main_menu = Menu::with_items(app, &[&app_menu, &edit_menu, &view_menu])?;
                app.set_menu(main_menu)?;
                app.on_menu_event(move |app, event| match event.id.as_ref() {
                    "menu-quit" => {
                        kill_dsh();
                        app.exit(0);
                    }
                    "menu-manage" => {
                        if let Some(w) = main_window(app) {
                            let _ = w.eval(
                                "window.__openManagePalette && window.__openManagePalette()",
                            );
                        }
                    }
                    _ => {}
                });
            }
            // P3：主窗口 = 壳页（ui/shell.html）：36px 顶栏 + 命令面板 + 管理抽屉
            //（dsh/插件/关于）+ 首次引导浮层。工作台是 workbench.rs 用
            // `window.add_child()` 创建的原生 child webview（顶层文档、非 iframe），
            // 覆盖在壳页工作区之上；dsh 就绪后由它自己顶层导航到就绪 URL。
            //
            // 启动顺序（消除"首帧默认位置跳变 / 再次居中上跳"）：
            //   visible(false)+center 隐藏创建 → dsh 就绪或开机 1.2s 宽限后，
            //   统一走 reveal_main_window 显示（其内部仅在不可见时 center+show）。
            match WebviewWindowBuilder::new(
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
                // 顶部帧页面加载记一行日志（壳页自身；dsh 在 child webview 另有回调）
                let url = payload.url().to_string();
                logln!("[webview] page loaded: {url}");
                // 事件驱动显示：壳页一渲染完成就显示窗口（比固定 0.8s 宽限更快且
                // 不会「窗口先于内容」出现空窗）；0.8s 宽限线程保留作兜底。
                if payload.event() == tauri::webview::PageLoadEvent::Finished
                    && !REVEALED.load(Ordering::SeqCst)
                    && !USER_HIDDEN.load(Ordering::SeqCst)
                {
                    if let Some(w) = main_window(_webview.app_handle()) {
                        reveal_main_window(&w.app_handle().clone(), None);
                    }
                }
            })
            .on_navigation(webview_navigation_policy)
            .on_new_window(webview_new_window_policy)
            .build()
            {
                Ok(w) => {
                    // 保存 handle 供后续所有主窗操作使用（该环境按 label 查找不可用，
                    // 见 MAIN_WIN 注释）
                    set_main_window(&w);
                    logln("[main] 主窗已创建并保存 handle");
                }
                Err(e) => logln(&format!("[main] 主窗创建失败: {e}")),
            }
            // 无缝衔接：窗口不在启动宽限内显示，改为等 dsh 首帧渲染完成后再显示
            // （workbench page-load 后触发 reveal）。留 20s 超时兜底（下方），
            // 覆盖 dsh 加载失败/极慢的情况——届时窗口显示，壳页有占位与引导兜底。
            // 启动即显示窗口：加载页（品牌 + 「正在启动 dsh 工作台…」）必须可见
            // ——用户明确要求保留加载页。dsh 首帧就绪后由 workbench 模块移入工作台
            // （page-load 后延迟 900ms，避免露出 SPA 半成品），届时加载页被覆盖。
            let reveal_app2 = app.handle().clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(800));
                reveal_main_window(&reveal_app2, mlock(&DSH_URL).as_deref());
            });
            std::thread::spawn(move || boot(handle));
            // 诊断模式（DSH_SELF_DOM=1）：启动 12s 后把壳页 DOM 状态（顶栏/管理
            // 按钮/把手/背景层/启动占位的位置与可见性）写入日志，并对窗口截图
            // 存 PNG（进程内截自己的窗口，无需屏幕录制权限）。用于无法人工截图时
            // 定位「顶栏不可见」「背景不对」这类纯视觉问题。
            if std::env::var("DSH_SELF_DOM").is_ok() {
                let d = app.handle().clone();
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_secs(12));
                    let d2 = d.clone();
                    let _ = d.run_on_main_thread(move || {
                        let Some(w) = crate::main_window(&d2) else {
                            crate::logln("[diag] 主窗 handle 不可用");
                            return;
                        };
                        let js = r#"JSON.stringify({
                          body: document.body.className,
                          htmlBg: getComputedStyle(document.documentElement).backgroundColor,
                          vw: innerWidth, vh: innerHeight, dpr: devicePixelRatio,
                          chrome: (()=>{const c=document.getElementById('chrome');if(!c)return null;const r=c.getBoundingClientRect();const s=getComputedStyle(c);return{x:r.x,y:r.y,w:r.width,h:r.height,transform:s.transform,opacity:s.opacity,visibility:s.visibility,display:s.display};})(),
                          manage: (()=>{const m=document.getElementById('btnManage');if(!m)return null;const r=m.getBoundingClientRect();return{x:r.x,y:r.y,w:r.width,h:r.height,txt:m.textContent.trim()};})(),
                          restore: (()=>{const b=document.getElementById('chromeRestore');if(!b)return null;const r=b.getBoundingClientRect();return{hidden:b.hidden,x:r.x,y:r.y,w:r.width,h:r.height};})(),
                          stage: (()=>{const s=document.getElementById('stage');if(!s)return null;const cs=getComputedStyle(s);return{top:cs.top,bg:cs.backgroundColor};})(),
                          ambient: (()=>{const a=document.querySelector('.ambient');if(!a)return null;const cs=getComputedStyle(a);const r=a.getBoundingClientRect();return{display:cs.display,opacity:cs.opacity,z:cs.zIndex,w:r.width,h:r.height};})(),
                          startup: (()=>{const s=document.getElementById('startupView');if(!s)return null;return{hidden:s.hidden,display:getComputedStyle(s).display};})()
                        })"#;
                        let _ = w.eval_with_callback(js, |r| crate::logln(&format!("[diag] dom={r}")));
                        // 注：进程内 CGWindowListCreateImage 在无屏幕录制权限时会抛
                        // Objective-C 异常（无法被 Rust 捕获，直接 abort），已弃用。
                    });
                });
            }
            // 自检钩子（DSH_SELF_HIDE_TEST=1）：启动 13s 后复刻 CloseRequested 的
            // 「关到后台」动作（USER_HIDDEN + 隐藏 child + AppHandle::hide），
            // 6s 后列出所有窗口可见性并恢复显示后退出。用于无法点击红点（自动化 /
            // 远程会话）时验证 close-to-tray 链路；不设该环境变量完全不参与运行。
            if std::env::var("DSH_SELF_HIDE_TEST").is_ok() {
                let t = app.handle().clone();
                std::thread::spawn(move || {
                    // 基线：启动完成（窗口已建、dsh 就绪）后、任何隐藏动作之前，
                    // 打印 tauri 窗口注册表——用于区分「查找 API 失效」与「隐藏导致」。
                    std::thread::sleep(Duration::from_secs(8));
                    let t0 = t.clone();
                    let t0b = t0.clone();
                    let _ = t0.run_on_main_thread(move || {
                        let labels: Vec<String> =
                            t0b.webview_windows().keys().cloned().collect();
                        crate::logln(&format!(
                            "[selftest] 启动后（hide 前）注册表: {:?} get_main={}",
                            labels,
                            crate::main_window(&t0b).is_some()
                        ));
                    });
                    std::thread::sleep(Duration::from_secs(5));
                    let t2 = t.clone();
                    let _ = t.run_on_main_thread(move || {
                        crate::logln("[selftest] 复刻关到后台动作");
                        USER_HIDDEN.store(true, Ordering::SeqCst);
                        crate::workbench::hide_child(&t2);
                        #[cfg(target_os = "macos")]
                        let r = {
                            if let Some(w) = crate::main_window(&t2) {
                                macwin::order_out(&w);
                            }
                            Ok::<(), tauri::Error>(())
                        };
                        #[cfg(not(target_os = "macos"))]
                        let r = Ok::<(), tauri::Error>(());
                        crate::logln(&format!("[selftest] hide → {r:?}"));
                    });
                    std::thread::sleep(Duration::from_secs(6));
                    let t3 = t.clone();
                    let t3b = t3.clone();
                    let _ = t3.run_on_main_thread(move || {
                        // 诊断：app.hide 后 tauri 的窗口注册表是否还在（若为空，
                        // 召回时 reveal 会误判「窗口不存在」而重建壳页 → 丢工作台）
                        let labels: Vec<String> =
                            t3b.webview_windows().keys().cloned().collect();
                        crate::logln(&format!(
                            "[selftest] hidden 后注册表: {:?} get_main={}",
                            labels,
                            crate::main_window(&t3b).is_some()
                        ));
                        // 模拟托盘「显示主窗口」/ Dock 召回：清 USER_HIDDEN 后走
                        // 统一 reveal 通道（内部 app.show + 窗口 show + child show）
                        USER_HIDDEN.store(false, Ordering::SeqCst);
                        logln("[selftest] 复刻召回动作（reveal_main_window）");
                        reveal_main_window(&t3b, mlock(&DSH_URL).as_deref());
                    });
                    std::thread::sleep(Duration::from_secs(5));
                    let t4 = t.clone();
                    let t4b = t4.clone();
                    let _ = t4.run_on_main_thread(move || {
                        let labels: Vec<String> =
                            t4b.webview_windows().keys().cloned().collect();
                        crate::logln(&format!("[selftest] 召回后注册表: {:?}", labels));
                        t4b.exit(0);
                    });
                });
            }
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
                // 红点关闭 = 关到后台（close-to-tray）：只隐藏主窗，程序与 dsh
                // 继续运行、托盘常驻；托盘「显示主窗口」/Dock 图标可召回，
                // 托盘「退出」才真正退出。此前曾改为直接退出应用（用户实测反馈
                // 预期与此语义相悖），且崩溃自愈循环会在杀掉 dsh 后反复弹回窗口，
                // 观感为「关闭没反应」——那已由「自愈不 close 主窗 + USER_HIDDEN
                // 不弹回 + child webview 随主窗隐藏」三处修复解决。
                api.prevent_close();
                logln!("[main] close requested → 关到后台（原生 orderOut；程序与 dsh 继续运行）");
                USER_HIDDEN.store(true, Ordering::SeqCst);
                // child webview 是独立窗口，单独移出屏幕（同步 + 排队兜底）
                crate::workbench::hide_child_now(_app_handle);
                crate::workbench::hide_child(_app_handle);
                // 收起壳页浮层（抽屉/命令面板）：浮层状态不能跨「隐藏 → 召回」存活，
                // 否则召回时 reveal 无条件 show_child 会把它压到原生工作台下面
                // （用户实测：召回后看到残留的输入框与按钮）。静默关闭——不触发
                // show_workbench，否则 260ms 后的回调会把工作台移回窗口内，而此刻
                // 窗口不可见，macOS 上会变成一个孤立浮窗。
                if let Some(w) = main_window(_app_handle) {
                    let _ = w.eval("window.__onHideToTray && window.__onHideToTray()");
                }
                // 主窗：原生 orderOut（Tauri 的 hide() 无效 / AppHandle::hide() 会
                // 清空窗口注册表导致召回丢工作台——见 macwin 模块注释）
                if let Some(w) = main_window(_app_handle) {
                    #[cfg(target_os = "macos")]
                    macwin::order_out(&w);
                    #[cfg(not(target_os = "macos"))]
                    let _ = w.hide();
                }
                // 诊断：1.2s 后记录主窗可见性（该环境 get_webview_window/webview_windows
                // 不可用，改用保存的 handle），便于现场核对关到后台是否生效。
                {
                    let c = _app_handle.clone();
                    std::thread::spawn(move || {
                        std::thread::sleep(Duration::from_millis(1200));
                        let c2 = c.clone();
                        let _ = c.run_on_main_thread(move || {
                            let vis = crate::main_window(&c2)
                                .map(|w| w.is_visible().unwrap_or(true));
                            crate::logln(&format!("[main] post-close: main_visible={vis:?}"));
                        });
                    });
                }
            }
            // macOS: clicking the dock icon re-opens a hidden window.
            #[cfg(target_os = "macos")]
            RunEvent::Reopen { .. } => {
                USER_HIDDEN.store(false, Ordering::SeqCst);
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
    fn redact_url_hides_credentials_and_keeps_shape() {
        // userinfo（registry 源可写成 https://user:pass@host）
        assert_eq!(
            redact_url("https://user:s3cr3t@npm.example.com/repo"),
            "https://***@npm.example.com/repo"
        );
        // query 里的敏感键（dsh 就绪 URL 的 token=）
        assert_eq!(
            redact_url("http://127.0.0.1:65333/?token=abc123&port=65333"),
            "http://127.0.0.1:65333/?token=***&port=65333"
        );
        // fragment 不被当成 query 的一部分，且原样保留
        assert_eq!(redact_url("http://h/p?auth=xyz#frag"), "http://h/p?auth=***#frag");
        // 路径里的 '@' 不是凭据：保持不变
        assert_eq!(
            redact_url("https://example.com/@scope/pkg"),
            "https://example.com/@scope/pkg"
        );
        // 无凭据时不动（含中文路径）
        assert_eq!(redact_url("https://example.com/中文/包"), "https://example.com/中文/包");
        // 不带 scheme 的 registry（npm 允许 host:port/path 写法）也不会误改
        assert_eq!(redact_url("registry.npmmirror.com"), "registry.npmmirror.com");
    }

    #[test]
    fn stale_dsh_cmdline_matches_only_our_closure() {
        let dir = std::path::Path::new("/Users/u/Library/Application Support/com.dsh-desktop.app/dsh");
        // 本 App 闭包启动的 dsh（node 跑 app-data 里的 bin.js）→ 命中
        let ours = "/x/resources/node/bin/node /Users/u/Library/Application Support/com.dsh-desktop.app/dsh/v0.1.5-rc.2/node_modules/@deepseek-ai/dsh/lib/bin.js --profile web --port 0";
        assert!(is_stale_dsh_cmdline(ours, dir));
        // 用户终端里自己装的 dsh：没有本 App 的 app-data 路径 → 不碰
        let theirs = "/usr/local/bin/node /usr/local/lib/node_modules/@deepseek-ai/dsh/lib/bin.js --profile web";
        assert!(!is_stale_dsh_cmdline(theirs, dir));
        // 同路径但不是 dsh 服务进程（例如某个一次性的 CLI 调用）→ 不碰
        assert!(!is_stale_dsh_cmdline(
            "/x/node /Users/u/Library/Application Support/com.dsh-desktop.app/dsh/v0.1.5-rc.2/tool.js",
            dir
        ));
        // 安装 dsh 的 pnpm（App 自己起的）→ 命中
        let pnpm = "/x/resources/pnpm-bin/pnpm install @deepseek-ai/dsh@0.1.5-rc.2 --store-dir /Users/u/Library/Application Support/com.dsh-desktop.app/dsh/pnpm-store";
        assert!(is_stale_pnpm_cmdline(pnpm, dir));
        // 用户终端里自己跑的 pnpm（不同 store/cwd）→ 不碰
        assert!(!is_stale_pnpm_cmdline(
            "pnpm install @deepseek-ai/dsh@0.1.5-rc.2 --store-dir /Users/u/.pnpm-store",
            dir
        ));
        // dsh 服务进程不应被 pnpm 判定命中（两个判定互不越界）
        assert!(!is_stale_pnpm_cmdline(ours, dir));
        // 路径命中但不带 --profile（比如我们自己的 sidecar）→ 不碰
        assert!(!is_stale_dsh_cmdline(
            "/x/node /Users/u/Library/Application Support/com.dsh-desktop.app/dsh/v0.1.5-rc.2/node_modules/@deepseek-ai/dsh/lib/bin.js --self-uninstall-full",
            dir
        ));
    }

    #[test]
    fn strip_verbatim_prefix_logic() {
        assert_eq!(strip_verbatim_prefix(r"\\?\C:\foo\bar"), Some("C:\\foo\\bar".into()));
        assert_eq!(strip_verbatim_prefix(r"\\?\UNC\host\share\x"), Some(r"\\host\share\x".into()));
        assert_eq!(strip_verbatim_prefix(r"C:\foo"), None);
        assert_eq!(strip_verbatim_prefix(r"\\host\share\x"), None);
        assert_eq!(strip_verbatim_prefix(""), None);
    }

    /// 卸载误删防线：uninstall_targets 返回的每个路径都必须「以本 App 命名」
    /// （末段含 bundle id）——这条不变量保证卸载不会删到共享父目录
    /// （~/Library/Preferences、~/Library/HTTPStorages、~/Library…）或其它 App 的数据。
    #[test]
    fn uninstall_targets_only_touch_app_owned_paths() {
        // 假 home 必须是**平台合法**的绝对路径：Windows 上 `/Users/...` 没有盘符前缀，
        // `Path::is_absolute()` 为 false（CI 的 Windows job 正是这样挂过一次）。
        let home = if cfg!(windows) {
            PathBuf::from(r"C:\Users\someone")
        } else {
            PathBuf::from("/Users/someone")
        };
        let app_data = home.join("Library/Application Support").join(APP_ID);
        let (dirs, files) = uninstall_targets(&home, &app_data);
        assert!(dirs.contains(&app_data), "app_data 必须被清理");
        let shared = [
            "Library",
            "Preferences",
            "HTTPStorages",
            "Caches",
            "WebKit",
            "Saved Application State",
            "Application Support",
        ];
        for p in dirs.iter().chain(files.iter()) {
            let name = p
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            assert!(
                name.contains(APP_ID),
                "卸载目标 {p:?} 的末段不含 App id，存在误删风险"
            );
            assert!(!shared.contains(&name.as_str()), "卸载目标 {p:?} 是共享父目录");
        }
        // 共享目录的父路径也绝不能被当作目标
        for p in dirs.iter().chain(files.iter()) {
            assert_ne!(p, &home);
            assert_ne!(p, &home.join("Library"));
            // 全部必须是绝对路径：相对路径会被解析到当前工作目录（remove_* 会删到
            // 用户看不出关系的目录），remove_uninstall_targets 会跳过它们
            assert!(p.is_absolute(), "卸载目标 {p:?} 不是绝对路径");
        }
    }

    #[test]
    fn post_exit_cleanup_excludes_app_data_and_quotes_paths() {
        let keep = PathBuf::from("/Users/x/Library/Application Support/com.dsh-desktop.app");
        let dirs = vec![
            keep.clone(),
            PathBuf::from("/Users/x/Library/WebKit/com.dsh-desktop.app"),
            PathBuf::from("/Users/a b/Library/HTTPStorages/com.dsh-desktop.app"),
        ];
        let files = vec![PathBuf::from("/Users/x/Library/HTTPStorages/com.dsh-desktop.app.binarycookies")];
        let cmd = post_exit_cleanup_cmd(&dirs, &files, &keep, "com.dsh-desktop.app");
        // app_data 必须被排除（用户几秒内重装时不能误删新数据）
        assert!(!cmd.contains("Application Support/com.dsh-desktop.app'"), "app_data 未被排除: {cmd}");
        // 含空格的路径必须整体加引号
        assert!(cmd.contains("'/Users/a b/Library/HTTPStorages/com.dsh-desktop.app'"), "{cmd}");
        assert!(cmd.contains("rm -rf '/Users/x/Library/WebKit/com.dsh-desktop.app'"), "{cmd}");
        assert!(cmd.contains("rm -f '/Users/x/Library/HTTPStorages/com.dsh-desktop.app.binarycookies'"), "{cmd}");
        assert!(cmd.contains("defaults delete com.dsh-desktop.app"), "{cmd}");
        assert!(cmd.starts_with("sleep 3;"), "{cmd}");
        // 单引号转义
        assert_eq!(sh_quote(std::path::Path::new("/tmp/it's/a")), "'/tmp/it'\\''s/a'");
    }

    #[test]
    fn remove_uninstall_targets_handles_both_forms() {
        // 回归：按"登记表"删会在形态错位时报 NotADirectory/IsADirectory —— 既误报残留、
        // 又真的删不掉（新增的 HTTPStorages/<id> 正是"形态可能错位"的目标）。
        let root = std::env::temp_dir().join(format!("dsh-uninst-rm-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let dir_ok = root.join("dir_target");
        std::fs::create_dir_all(dir_ok.join("inner")).unwrap();
        let dir_is_file = root.join("dir_target_is_file"); // 登记为目录、实际是文件
        std::fs::write(&dir_is_file, b"x").unwrap();
        let file_ok = root.join("file_target");
        std::fs::write(&file_ok, b"x").unwrap();
        let file_is_dir = root.join("file_target_is_dir"); // 登记为文件、实际是目录
        std::fs::create_dir_all(file_is_dir.join("inner")).unwrap();

        let leftovers = remove_uninstall_targets(
            &[dir_ok.clone(), dir_is_file.clone()],
            &[file_ok.clone(), file_is_dir.clone()],
        );
        assert!(leftovers.is_empty(), "不应报残留：{leftovers:?}");
        for p in [&dir_ok, &dir_is_file, &file_ok, &file_is_dir] {
            assert!(!p.exists(), "未删除：{}", p.display());
        }
        // 相对路径必须被跳过（宁可不删也不删错东西）
        let rel_dir = PathBuf::from("dsh-relative-should-never-be-deleted");
        assert!(remove_uninstall_targets(std::slice::from_ref(&rel_dir), &[]).is_empty());
        assert!(!rel_dir.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn tasklist_pids_parses_csv_and_skips_self() {
        // 真实输出形如："dsh-desktop.exe","1234","Console","1","12,345 K"
        let csv = "\"dsh-desktop.exe\",\"1234\",\"Console\",\"1\",\"12,345 K\"\r\n\
                   \"dsh-desktop.exe\",\"4321\",\"Console\",\"1\",\"9,876 K\"\r\n";
        assert_eq!(tasklist_pids(csv, 4321), vec![1234]); // 自己（4321）必须排除
        assert_eq!(tasklist_pids(csv, 9999), vec![1234, 4321]);
        // 没有匹配时 tasklist 输出的是提示文本，不是 CSV
        assert!(tasklist_pids("INFO: No tasks are running which match the specified criteria.\r\n", 1).is_empty());
        assert!(tasklist_pids("", 1).is_empty());
    }

    #[test]
    fn dsh_url_port_extracts_port_from_token_url() {
        // 回归：dsh 0.1.2-alpha.5 起就绪 URL 带 ?token= 本地鉴权参数，
        // 旧实现 rsplit(':') 会把 "51940/?token=…" 整段当端口解析而失败。
        assert_eq!(
            dsh_url_port("http://127.0.0.1:51940/?token=DUMMY_TOKEN_FOR_TEST_ONLY"),
            Some(51940)
        );
        // 旧格式（无 token）与带尾斜杠仍正常
        assert_eq!(dsh_url_port("http://127.0.0.1:65430"), Some(65430));
        assert_eq!(dsh_url_port("http://127.0.0.1:51940/"), Some(51940));
        assert_eq!(dsh_url_port("not a url"), None);
    }

    #[test]
    fn webview_internal_url_matches_dsh_port_with_token() {
        // 回归：DSH_URL 带 ?token= 时，同端口工作台 URL 必须放行（内嵌 iframe），
        // 异端口/外部地址仍拦截转浏览器。
        *mlock(&DSH_URL) = Some("http://127.0.0.1:51940/?token=abc".into());
        let same = tauri::Url::parse("http://127.0.0.1:51940/?token=abc").unwrap();
        assert!(is_internal_webview_url(&same), "同源端口带 token 应放行");
        let other_port = tauri::Url::parse("http://127.0.0.1:9999/").unwrap();
        // debug 构建额外放行 loopback 任意端口（tauri dev 内置静态服务的壳页
        // URL http://127.0.0.1:<devport>/shell.html）；release 严格要求 dsh 端口。
        #[cfg(not(debug_assertions))]
        assert!(!is_internal_webview_url(&other_port), "异端口应拦截（release）");
        #[cfg(debug_assertions)]
        assert!(is_internal_webview_url(&other_port), "debug 放行 loopback（dev server）");
        let external = tauri::Url::parse("https://example.com/").unwrap();
        assert!(!is_internal_webview_url(&external), "外部地址应拦截");
        let builtin = tauri::Url::parse("tauri://localhost/shell.html").unwrap();
        assert!(is_internal_webview_url(&builtin), "内置页应放行");
        *mlock(&DSH_URL) = None; // 复位，不影响其它测试
    }
}
