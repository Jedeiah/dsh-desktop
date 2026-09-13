//! 工作台 child webview（first-party 顶层文档）管理与几何计算。
//!
//! dsh 就绪 URL 直接作为 child webview 的顶层导航地址：token→303→Set-Cookie
//! 全部由 WebView 原生处理，壳不解析/不注入任何认证细节（spec 2026-09-13）。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use tauri::{
    AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, Position, Rect, Size,
    WebviewBuilder, WebviewUrl,
};

/// 顶栏逻辑高度；必须与 ui/theme.css `--dsh-h-chrome`（36px）一致。
pub const TOPBAR_H_LOGICAL: f64 = 36.0;

/// macOS 标准标题栏高（带装饰窗口 frame 顶到内容顶的差值，逻辑点）。
/// 实测（2026-09-13 联调）：child webview 的 set_bounds 坐标按窗口 frame
/// 原点（含标题栏）计算，而壳页内容从标题栏下方开始——窗口化时若不复位
/// 会让 child 上移盖住壳层顶栏（症状：管理按钮只在全屏可见）。
/// 全屏（无标题栏）时差值为 0；macOS 10.15+ 标准带标题窗口统一为 28pt。
#[cfg(target_os = "macos")]
pub const TITLEBAR_H_PT: f64 = 28.0;
#[cfg(not(target_os = "macos"))]
pub const TITLEBAR_H_PT: f64 = 0.0;

/// child webview 在主窗客户区内的几何（物理像素）。
pub struct Geom {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

/// 由窗口物理尺寸与顶栏折叠态计算工作台几何。
/// `scale` = 缩放比（物理/逻辑），取自窗口 monitor 的 scale_factor。
pub fn geom(win_w: u32, win_h: u32, scale: f64, collapsed: bool) -> Geom {
    let topbar = if collapsed { 0.0 } else { TOPBAR_H_LOGICAL } * scale;
    let y = topbar.round() as i32;
    Geom { x: 0, y, w: win_w, h: win_h.saturating_sub(y as u32) }
}

/// dsh 就绪 URL 是否相对当前已加载 URL 发生变化（端口/token 任一变化即真）。
pub fn url_changed(current: Option<&str>, next: &str) -> bool {
    match current {
        None => true,
        Some(cur) => cur != next,
    }
}

/// child webview 标签（同时作为降级窗口的 label）。
pub const LABEL: &str = "workbench";
/// 降级窗口标题。
const FALLBACK_TITLE: &str = "DeepSeek Harness 工作台";

/// 顶栏折叠态（shell.js 经 workbench_set_collapsed_cmd 同步）。
static COLLAPSED: AtomicBool = AtomicBool::new(false);
/// 业务侧显式隐藏标记：抽屉 / 命令面板打开、主窗关到后台时置 true。
/// child webview「页面加载完成自动显示」必须绕过它——否则 dsh 崩溃自愈
/// 换端口重导航完成后会把工作台从抽屉/面板下面顶出来。
static SUPPRESSED: AtomicBool = AtomicBool::new(false);
/// 工作台是否已完成首次加载（workbench:ready 已发生；供壳页轮询兜底）。
static READY: AtomicBool = AtomicBool::new(false);
/// 降级模式（路径 C：独立窗口）。add_child 成功 = false。
static FALLBACK: AtomicBool = AtomicBool::new(false);
/// 主窗 Resized 监听只挂一次。
static RESIZE_HOOKED: AtomicBool = AtomicBool::new(false);
/// 当前已加载的工作台 URL（换端口/token 重导航判定）。
static CUR_URL: Mutex<Option<String>> = Mutex::new(None);
/// AppHandle（on_page_load 闭包内 emit 用）。
static APP: OnceLock<AppHandle> = OnceLock::new();

/// dsh 就绪入口（boot stdout 解析到 URL 后调用；可重入）：
/// 未创建则创建 child webview 并导航；已创建且 URL 变化则重导航。
/// 创建失败自动降级为独立窗口（路径 C），仅降级一次。
pub fn ensure_ready(app: &AppHandle, url: &str) {
    let _ = APP.set(app.clone());
    READY.store(false, Ordering::SeqCst); // 新导航开始：占位层需重新盖住
    let created = {
        let app2 = app.clone();
        let u = url.to_string();
        let (tx, rx) = std::sync::mpsc::channel::<bool>();
        let _ = app2.clone().run_on_main_thread(move || {
            let _ = tx.send(ensure_ready_on_main(&app2, &u));
        });
        rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap_or(false)
    };
    if !created {
        crate::logln("[workbench] child webview 创建失败，降级为独立窗口（路径 C）");
        FALLBACK.store(true, Ordering::SeqCst);
        let app2 = app.clone();
        let u = url.to_string();
        let _ = app2.clone().run_on_main_thread(move || open_fallback_window(&app2, &u));
    }
    sync_bounds(app);
}

/// 主线程内执行：true = 工作台 webview 就绪（创建或复用成功）。
fn ensure_ready_on_main(app: &AppHandle, url: &str) -> bool {
    let Ok(parsed) = url.parse::<tauri::Url>() else {
        crate::logln("[workbench] 无法解析 dsh URL，跳过");
        return false;
    };
    if FALLBACK.load(Ordering::SeqCst) {
        if let Some(w) = app.get_webview_window(LABEL) {
            let _ = w.navigate(parsed);
            let _ = w.show();
        }
        return true; // 降级态：窗口路径自洽，不再尝试 child
    }
    let Some(window) = app.get_window(crate::WINDOW_LABEL) else {
        return false;
    };
    // 已创建：仅 URL 变化时重导航（dsh 崩溃自愈换端口/token）
    if let Some(wv) = window.get_webview(LABEL) {
        if url_changed(CUR_URL.lock().unwrap().as_deref(), url) {
            let _ = wv.navigate(parsed);
            *CUR_URL.lock().unwrap() = Some(url.to_string());
        }
        return true;
    }
    let builder = WebviewBuilder::new(LABEL, WebviewUrl::External(parsed))
        .on_navigation(crate::webview_navigation_policy)
        .on_new_window(crate::webview_new_window_policy)
        .on_page_load(|wv, payload| {
            if payload.event() == tauri::webview::PageLoadEvent::Finished {
                crate::logln(&format!("[workbench] page loaded: {}", payload.url()));
                // 诊断探针：记录 contentType/title——区分「工作台 HTML」与
                // 「认证失败纯文本页」（历史教训：工作台空白时日志无据可查）。
                // 只取元信息（不读正文），避免把工作台内容写进日志。
                let _ = wv.eval_with_callback(
                    "JSON.stringify({ct:document.contentType,t:document.title})",
                    |r| crate::logln(&format!("[workbench] probe: {r}")),
                );
                READY.store(true, Ordering::SeqCst);
                // 首帧已绘制 → 显示（创建时已隐藏，见 ensure_ready_on_main）；
                // 业务侧隐藏期间（抽屉/命令面板/关到后台）不得顶出。
                if !SUPPRESSED.load(Ordering::SeqCst) {
                    let _ = wv.show();
                }
                if let Some(app) = APP.get() {
                    let _ = app.emit("workbench:ready", ());
                }
            }
        });
    // 初始 (0,0,1,1) 后立即 sync_bounds 校正，避免首帧错误尺寸闪烁
    match window.add_child(builder, PhysicalPosition::new(0, 0), PhysicalSize::new(1u32, 1u32)) {
        Ok(_) => {
            *CUR_URL.lock().unwrap() = Some(url.to_string());
            // 崩溃自愈会「关闭主窗 → 重建」（boot 既有行为）：新窗口需要重新挂
            // Resized 监听。此处是唯一创建点，webview 不存在 ⇒ 窗口必为新建/首次。
            RESIZE_HOOKED.store(false, Ordering::SeqCst);
            attach_resize_hook(app);
            // 首帧渲染完成前隐藏 child：阶段①壳页「正在启动 dsh 工作台…」占位
            // 可见（child 是独立 NSWindow 盖在其上）；阶段② page-load Finished
            // 后（dsh 首帧已绘制）再显示，消除「空隙的灰底/半成品」。
            if let Some(wv) = window.get_webview(LABEL) {
                let _ = wv.hide();
            }
            crate::logln("[workbench] child webview 已创建（首帧前隐藏）");
            true
        }
        Err(e) => {
            crate::logln(&format!("[workbench] add_child 失败: {e}"));
            false
        }
    }
}

/// 主窗 Resized → 同步 child webview 几何（只挂一次）。
fn attach_resize_hook(app: &AppHandle) {
    if RESIZE_HOOKED.swap(true, Ordering::SeqCst) {
        return;
    }
    let Some(window) = app.get_window(crate::WINDOW_LABEL) else {
        return;
    };
    let app2 = app.clone();
    window.on_window_event(move |ev| {
        if let tauri::WindowEvent::Resized(_) = ev {
            sync_bounds(&app2);
        }
    });
}

/// 依窗口当前尺寸与折叠态同步 child webview 几何（Resized/折叠切换共用）。
pub fn sync_bounds(app: &AppHandle) {
    let collapsed = COLLAPSED.load(Ordering::SeqCst);
    let app2 = app.clone();
    let _ = app2.clone().run_on_main_thread(move || {
        if FALLBACK.load(Ordering::SeqCst) {
            return; // 路径 C：独立窗口即工作台，不做内嵌几何
        }
        let Some(window) = app2.get_window(crate::WINDOW_LABEL) else {
            return;
        };
        let scale = window.scale_factor().unwrap_or(1.0);
        let size = window
            .inner_size()
            .unwrap_or(PhysicalSize::new(1280u32, 820u32));
        // macOS 窗口化时原生标题栏算在窗口 frame 里、不算在 content（inner）里：
        // child webview 的坐标若按 frame 定位，会把标题栏高当成内容起始点，导致
        // webview 上移盖住壳层顶栏（症状：管理按钮只在全屏可见）。这里取
        // outer-inner 差值（标题栏高，物理像素）补偿，全屏时差值为 0。
        let outer_h = window.outer_size().unwrap_or(size).height as i64;
        let inset = (outer_h - size.height as i64).max(0) as u32;
        // 标题栏在屏幕坐标系里的真实占位（外/内原点差；outer-inner 尺寸差在
        // macOS 上可能为 0，原点差才是全屏/窗口化差异的证据）
        let op = window.outer_position().unwrap_or(PhysicalPosition::new(0, 0));
        let ip = window.inner_position().unwrap_or(PhysicalPosition::new(0, 0));
        // 窗口化时 child webview 按 frame 原点定位：补上标题栏高，才与壳页
        // 内容区对齐（全屏时标题栏为 0）。见 TITLEBAR_H_PT 注释。
        let is_fs = window.is_fullscreen().unwrap_or(false);
        let tb_pt = if is_fs { 0.0 } else { TITLEBAR_H_PT };
        let g = geom(size.width, size.height, scale, collapsed);
        let y = g.y as u32 + inset + (tb_pt * scale).round() as u32;
        crate::logln(&format!(
            "[workbench] bounds: y={} h={} scale={} collapsed={} fullscreen={} win={}x{} inset={} titlebar_pt={} outer_pos=({},{}) inner_pos=({},{})",
            y, g.h, scale, collapsed, is_fs, size.width, size.height, inset, tb_pt, op.x, op.y, ip.x, ip.y
        ));
        if let Some(wv) = window.get_webview(LABEL) {
            let _ = wv.set_bounds(Rect {
                position: Position::Physical(PhysicalPosition::new(g.x, y as i32)),
                size: Size::Physical(PhysicalSize::new(g.w, g.h)),
            });
        }
    });
}

/// 路径 C：独立工作台窗口（stable API；add_child 不可用时的兜底）。
fn open_fallback_window(app: &AppHandle, url: &str) {
    use tauri::WebviewWindowBuilder;
    let Ok(parsed) = url.parse::<tauri::Url>() else {
        return;
    };
    if let Some(w) = app.get_webview_window(LABEL) {
        let _ = w.navigate(parsed);
        let _ = w.show();
        let _ = w.set_focus();
        return;
    }
    let built = WebviewWindowBuilder::new(app, LABEL, WebviewUrl::External(parsed))
        .title(FALLBACK_TITLE)
        .inner_size(1280.0, 820.0)
        .min_inner_size(800.0, 560.0)
        .theme(Some(tauri::Theme::Dark))
        .on_navigation(crate::webview_navigation_policy)
        .on_new_window(crate::webview_new_window_policy)
        .build();
    match built {
        Ok(w) => {
            READY.store(true, Ordering::SeqCst);
            let _ = w.set_focus();
            if let Some(a) = APP.get() {
                let _ = a.emit("workbench:ready", ());
            }
        }
        Err(e) => crate::logln(&format!("[workbench] 降级窗口创建失败: {e}")),
    }
}

/// 显示工作台（切到工作台 tab / 主窗从后台恢复）。降级：窗口 show+focus。
#[tauri::command]
pub fn show_workbench_cmd(app: AppHandle) {
    show_child(&app);
}

/// 隐藏工作台（切到管理页 / 主窗关到后台）。降级：窗口 hide。
#[tauri::command]
pub fn hide_workbench_cmd(app: AppHandle) {
    hide_child(&app);
}

/// 隐藏工作台 child webview（或降级窗口）。macOS 上 child webview 是独立
/// NSWindow，主窗口 hide 不会带它一起隐藏——主窗关到后台必须显式调这里，
/// 否则工作台残留在屏幕上（用户第 1 版「关闭没反应」的根因之一）。
pub fn hide_child(app: &AppHandle) {
    SUPPRESSED.store(true, Ordering::SeqCst);
    let app2 = app.clone();
    let _ = app2.clone().run_on_main_thread(move || {
        if FALLBACK.load(Ordering::SeqCst) {
            if let Some(w) = app2.get_webview_window(LABEL) {
                let _ = w.hide();
            }
        } else if let Some(window) = app2.get_window(crate::WINDOW_LABEL) {
            if let Some(wv) = window.get_webview(LABEL) {
                let _ = wv.hide();
            }
        }
    });
}

/// 显示工作台 child webview（或降级窗口）；主窗恢复显示时调用。
pub fn show_child(app: &AppHandle) {
    SUPPRESSED.store(false, Ordering::SeqCst);
    let app2 = app.clone();
    let _ = app2.clone().run_on_main_thread(move || {
        if FALLBACK.load(Ordering::SeqCst) {
            if let Some(w) = app2.get_webview_window(LABEL) {
                let _ = w.show();
                let _ = w.set_focus();
            }
        } else if let Some(window) = app2.get_window(crate::WINDOW_LABEL) {
            if let Some(wv) = window.get_webview(LABEL) {
                let _ = wv.show();
            }
        }
    });
}

/// 刷新工作台（brand 点击 / 重试）。
#[tauri::command]
pub fn workbench_reload_cmd(app: AppHandle) {
    let app2 = app.clone();
    let _ = app2.clone().run_on_main_thread(move || {
        if FALLBACK.load(Ordering::SeqCst) {
            if let Some(w) = app2.get_webview_window(LABEL) {
                let _ = w.eval("location.reload()");
            }
        } else if let Some(window) = app2.get_window(crate::WINDOW_LABEL) {
            if let Some(wv) = window.get_webview(LABEL) {
                let _ = wv.eval("location.reload()");
            }
        }
    });
}

/// 壳页启动/事件丢失兜底：工作台是否已完成首次加载。
#[tauri::command]
pub fn workbench_ready_cmd() -> bool {
    READY.load(Ordering::SeqCst)
}

/// 顶栏折叠态同步（shell.js applyTabsCollapsed 调用）。
#[tauri::command]
pub fn workbench_set_collapsed_cmd(app: AppHandle, collapsed: bool) {
    crate::logln(&format!("[workbench] collapsed 同步: {collapsed}"));
    COLLAPSED.store(collapsed, Ordering::SeqCst);
    sync_bounds(&app);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn geom_normal_topbar() {
        // 1280x820 逻辑 @2x：y = 36*2 = 72，h = 820*2 - 72 = 1568
        let g = geom(2560, 1640, 2.0, false);
        assert_eq!((g.x, g.y, g.w, g.h), (0, 72, 2560, 1568));
    }

    #[test]
    fn geom_collapsed_topbar_full_height() {
        let g = geom(2560, 1640, 2.0, true);
        assert_eq!((g.x, g.y, g.w, g.h), (0, 0, 2560, 1640));
    }

    #[test]
    fn geom_scale_one() {
        let g = geom(1280, 820, 1.0, false);
        assert_eq!((g.x, g.y, g.w, g.h), (0, 36, 1280, 784));
    }

    #[test]
    fn url_changed_detects_port_and_token() {
        // dsh 重启后端口/token 都可能变化：任意差异都要重导航
        assert!(url_changed(None, "http://127.0.0.1:1/?token=a"));
        assert!(url_changed(
            Some("http://127.0.0.1:1/?token=a"),
            "http://127.0.0.1:2/?token=a"
        ));
        assert!(url_changed(
            Some("http://127.0.0.1:1/?token=a"),
            "http://127.0.0.1:1/?token=b"
        ));
        assert!(!url_changed(
            Some("http://127.0.0.1:1/?token=a"),
            "http://127.0.0.1:1/?token=a"
        ));
    }
}
