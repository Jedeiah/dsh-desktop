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

/// 折叠态留给「展开把手」的条高（逻辑 px）。原生 child webview 盖在所有壳页
/// 元素之上：折叠时若工作台顶到窗口最顶，展开把手会被盖住、点不到（用户实测
/// 「看不到展开的按钮」）。故折叠态预留这条把手空间，把手常驻于此。
/// 与 ui/theme.css `--dsh-handle-h`（18px）、shell.js 折叠布局保持一致。
pub const HANDLE_H_LOGICAL: f64 = 18.0;

/// macOS 标题栏高（逻辑 pt，带装饰窗口 frame 顶到内容顶的差值）。
///
/// 实测（2026-09-13，壳页 DOM 探针 + 窗口采样）：
/// - 该环境下 `Window::inner_size()` 返回的是**窗口 frame 高度（含标题栏）**，
///   且 outer==inner（inset=0）；
/// - child webview 的 set_bounds 坐标是 **frame 相对**（含标题栏）；
/// - 壳页视口高 789 ↔ frame 高 821 → 标题栏 = 32pt（macOS 26 Tahoe；旧版 28pt）。
///
/// 因此几何必须同时做两件事（缺一个就出问题）：
///   y = 顶栏 + 标题栏（不补偿 → 工作台上移盖住顶栏，「管理行消失」）
///   h = frame 高 − 标题栏 − 顶栏（不减标题栏 → 底部超出窗口被裁，「显示不全」）
#[cfg(target_os = "macos")]
pub const TITLEBAR_H_PT: f64 = 32.0;
#[cfg(not(target_os = "macos"))]
pub const TITLEBAR_H_PT: f64 = 0.0;

/// 折叠态工作台顶部偏移：展开 = 顶栏高；折叠 = 把手条高。
fn topbar_offset(collapsed: bool) -> f64 {
    if collapsed {
        HANDLE_H_LOGICAL
    } else {
        TOPBAR_H_LOGICAL
    }
}

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
    let topbar = topbar_offset(collapsed) * scale;
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
                    "JSON.stringify({ct:document.contentType,t:document.title,vw:innerWidth,vh:innerHeight,sw:document.documentElement.scrollWidth,sh:document.documentElement.scrollHeight,dpr:devicePixelRatio})",
                    |r| crate::logln(&format!("[workbench] probe: {r}")),
                );
                READY.store(true, Ordering::SeqCst);
                // 借鉴 main 的启动衔接：dsh 是 SPA，page-load Finished 远早于首帧
                // 渲染完成，立即移入会露出它的空白/半成品（用户反馈「启动后看到
                // 背景而不是无缝进入 dsh」）。延迟 900ms 让首帧渲染完成，再移入
                // 工作台并显示主窗口——窗口出现第一眼即是 dsh 页面。
                if let Some(app) = APP.get() {
                    let a = app.clone();
                    std::thread::spawn(move || {
                        std::thread::sleep(std::time::Duration::from_millis(900));
                        let a2 = a.clone();
                        let _ = a.run_on_main_thread(move || {
                            if !SUPPRESSED.load(Ordering::SeqCst) {
                                apply_bounds_on_main(&a2);
                                crate::reveal_main_window(&a2, None);
                            }
                        });
                    });
                }
                // 视口诊断：dsh 是单页应用，Finished 时布局可能未稳定；3s 后复探，
                // 对比「webview 视口尺寸 vs 页面 scroll 尺寸」定位「内容显示不全」。
                if let Some(app) = APP.get() {
                    let a = app.clone();
                    std::thread::spawn(move || {
                        std::thread::sleep(std::time::Duration::from_millis(3000));
                        let a2 = a.clone();
                        let _ = a.run_on_main_thread(move || {
                            if let Some(w) = a2.get_window(crate::WINDOW_LABEL) {
                                if let Some(wv2) = w.get_webview(LABEL) {
                                    let _ = wv2.eval_with_callback(
                                        "JSON.stringify({vw:innerWidth,vh:innerHeight,sw:document.documentElement.scrollWidth,sh:document.documentElement.scrollHeight,bodyW:document.body.getBoundingClientRect().width})",
                                        |r| crate::logln(&format!("[workbench] probe+3s: {r}")),
                                    );
                                }
                            }
                        });
                    });
                }
                // 首帧已绘制 → 移回窗口内可见位置（创建时在屏幕外预渲染）；
                // 业务侧隐藏期间（抽屉/命令面板/关到后台）不移动。
                if !SUPPRESSED.load(Ordering::SeqCst) {
                    if let Some(app) = APP.get() {
                        apply_bounds_on_main(app);
                    }
                }
                if let Some(app) = APP.get() {
                    let _ = app.emit("workbench:ready", ());
                }
            }
        });
    // 初始即用正确几何、位置放到屏幕外：dsh 从第一个字节起就在正确的视口尺寸下
    // 渲染（此前 1x1 起步 + 后续校正会让单页应用按错误视口布局，表现为「内容
    // 显示不全」）；屏幕外加载同时保证壳页「正在启动 dsh 工作台…」占位不被盖住，
    // 首帧绘制完成后（on_page_load Finished）再移回窗口内。
    let init_size = bounds_on_main(app)
        .map(|r| rect_size(&r))
        .unwrap_or(PhysicalSize::new(1280u32, 820u32));
    match window.add_child(builder, PhysicalPosition::new(OFFSCREEN, OFFSCREEN), init_size) {
        Ok(_) => {
            *CUR_URL.lock().unwrap() = Some(url.to_string());
            // 崩溃自愈会「关闭主窗 → 重建」（boot 既有行为）：新窗口需要重新挂
            // Resized 监听。此处是唯一创建点，webview 不存在 ⇒ 窗口必为新建/首次。
            RESIZE_HOOKED.store(false, Ordering::SeqCst);
            attach_resize_hook(app);
            crate::logln(&format!(
                "[workbench] child webview 已创建（屏幕外 {}x{} 预渲染，首帧后移入）",
                init_size.width, init_size.height
            ));
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

/// child webview 隐藏时的屏幕外坐标。隐藏 = 保持尺寸移出可视区：不触发
/// resize/reflow，dsh 页面布局与状态不受影响。macOS 上 Webview::hide() 与
/// WebviewWindow::hide() 一样不可靠（见 main.rs 关闭路径的实证注释）。
const OFFSCREEN: i32 = -32000;

/// 上次 bounds 日志文本（Resized 风暴下重复行会淹没日志，仅在几何变化时打）。
static LAST_BOUNDS: Mutex<Option<String>> = Mutex::new(None);

/// 主线程内计算 child webview 应处的物理矩形（窗口不存在返回 None）。
fn bounds_on_main(app: &AppHandle) -> Option<Rect> {
    let window = app.get_window(crate::WINDOW_LABEL)?;
    let collapsed = COLLAPSED.load(Ordering::SeqCst);
    let scale = window.scale_factor().unwrap_or(1.0);
    let size = window.inner_size().unwrap_or(PhysicalSize::new(1280u32, 820u32));
    // child webview 的 set_bounds 坐标是**窗口 content 相对**（实测 2026-09-13：
    // 窗口采样显示它不是独立窗口、而是内容视图内子视图；视口探针 vw/vh 与设定
    // 尺寸一致）。**不要加标题栏补偿**——曾误加 28pt（把「顶栏被折叠」误判为
    // 「标题栏遮挡」），导致工作台整体下移 28pt：顶部露出壳页底色一条、底部
    // 超出 content 被窗口裁掉（用户报告的「dsh 内容显示不全」）。
    // inset（outer-inner 尺寸差）在 macOS 上为 0，保留以兼容其他平台。
    let outer_h = window.outer_size().unwrap_or(size).height as i64;
    let inset = (outer_h - size.height as i64).max(0) as u32;
    let g = geom(size.width, size.height, scale, collapsed);
    // frame 相对坐标：顶栏 + 标题栏；高度再扣掉标题栏（见 TITLEBAR_H_PT 注释）
    let tb_px = (TITLEBAR_H_PT * scale).round() as u32;
    let y = g.y as u32 + inset + tb_px;
    let h = g.h.saturating_sub(tb_px);
    let desc = format!(
        "y={} h={} scale={} collapsed={} win={}x{} inset={} titlebar_pt={}",
        y, h, scale, collapsed, size.width, size.height, inset, TITLEBAR_H_PT
    );
    if LAST_BOUNDS.lock().unwrap().as_deref() != Some(desc.as_str()) {
        crate::logln(&format!("[workbench] bounds: {desc}"));
        *LAST_BOUNDS.lock().unwrap() = Some(desc);
    }
    Some(Rect {
        position: Position::Physical(PhysicalPosition::new(g.x, y as i32)),
        size: Size::Physical(PhysicalSize::new(g.w, h)),
    })
}

fn rect_size(r: &Rect) -> PhysicalSize<u32> {
    match r.size {
        Size::Physical(s) => s,
        _ => PhysicalSize::new(1280u32, 820u32),
    }
}

/// child webview 是否处于「移出屏幕」隐藏态（用于移回时打一行日志）。
static CHILD_OFFSCREEN: AtomicBool = AtomicBool::new(false);

/// 工作台是否已移入窗口内（可见）。用于决定壳页 webview 的可视高度：
/// 工作台可见且无浮层时，壳页只覆盖顶栏——消除两个 webview 在工作区重叠导致的
/// 光标「小手⇄箭头」闪烁（AppKit 从最上层视图解析光标，两层交替）。
static WORKBENCH_VISIBLE: AtomicBool = AtomicBool::new(false);

/// 壳页 webview 的可视高度（逻辑 pt；0 = 全窗），并应用到原生视图：
/// - 工作台可见且无浮层 → 顶栏高（折叠态 = 把手条高）
/// - 启动加载页 / 抽屉 / 命令面板 / 关到后台 → 全窗
fn apply_shell_clip_on_main(app: &AppHandle) {
    #[cfg(target_os = "macos")]
    {
        let Some(w) = crate::main_window(app) else {
            return;
        };
        let clipped = WORKBENCH_VISIBLE.load(Ordering::SeqCst)
            && !SUPPRESSED.load(Ordering::SeqCst);
        // 壳页 webview 从窗口 frame 顶开始（含标题栏区），可见内容从标题栏下方起：
        // 只设顶栏高会被标题栏吃掉（实测视口仅剩 4pt），必须加上标题栏高。
        let h = if clipped {
            let base = if COLLAPSED.load(Ordering::SeqCst) {
                HANDLE_H_LOGICAL
            } else {
                TOPBAR_H_LOGICAL
            };
            base + TITLEBAR_H_PT
        } else {
            0.0
        };
        crate::macwin::set_shell_height(&w, h);
    }
}

/// 主线程内把 child webview 移到窗口内正确位置（无 run_on_main_thread 包装，
/// 供已在主线程的调用点用）。true = 已应用（非降级路径）。
fn apply_bounds_on_main(app: &AppHandle) -> bool {
    if FALLBACK.load(Ordering::SeqCst) {
        return false;
    }
    let Some(window) = app.get_window(crate::WINDOW_LABEL) else {
        return false;
    };
    let Some(wv) = window.get_webview(LABEL) else {
        return false;
    };
    let Some(rect) = bounds_on_main(app) else {
        return false;
    };
    let _ = wv.set_bounds(rect);
    if CHILD_OFFSCREEN.swap(false, Ordering::SeqCst) {
        crate::logln("[workbench] child 移回窗口内（可见）");
    }
    WORKBENCH_VISIBLE.store(true, Ordering::SeqCst);
    apply_shell_clip_on_main(app);
    true
}

/// 主线程内把 child webview 保持尺寸移到屏幕外（隐藏用）。
fn hide_child_on_main(app: &AppHandle) -> bool {
    if FALLBACK.load(Ordering::SeqCst) {
        if let Some(w) = app.get_webview_window(LABEL) {
            let _ = w.hide();
        }
        return true;
    }
    let Some(window) = app.get_window(crate::WINDOW_LABEL) else {
        return false;
    };
    let Some(wv) = window.get_webview(LABEL) else {
        return false;
    };
    let Some(rect) = bounds_on_main(app) else {
        return false;
    };
    let size = rect_size(&rect);
    let _ = wv.set_bounds(Rect {
        position: Position::Physical(PhysicalPosition::new(OFFSCREEN, OFFSCREEN)),
        size: Size::Physical(size),
    });
    CHILD_OFFSCREEN.store(true, Ordering::SeqCst);
    WORKBENCH_VISIBLE.store(false, Ordering::SeqCst);
    apply_shell_clip_on_main(app);
    crate::logln(&format!(
        "[workbench] child 移出屏幕（保持 {}x{}，不触发页面重排）",
        size.width, size.height
    ));
    true
}

/// 依窗口当前尺寸与折叠态同步 child webview 几何（Resized/折叠切换共用）。
pub fn sync_bounds(app: &AppHandle) {
    let app2 = app.clone();
    let _ = app2.clone().run_on_main_thread(move || {
        // 业务侧隐藏期间不应用几何：否则 Resized 会把已隐藏的工作台移回屏幕
        if SUPPRESSED.load(Ordering::SeqCst) {
            return;
        }
        apply_bounds_on_main(&app2);
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

/// 隐藏工作台（切到管理页 / 主窗关到后台）。实现为「保持尺寸移出屏幕」：
/// macOS 上 Webview/WebviewWindow 的 hide() 不可靠（实证），而 set_bounds 可靠。
pub fn hide_child(app: &AppHandle) {
    SUPPRESSED.store(true, Ordering::SeqCst);
    let app2 = app.clone();
    let _ = app2.clone().run_on_main_thread(move || {
        hide_child_on_main(&app2);
    });
}

/// 同步版隐藏（调用方必须在主线程）：CloseRequested 事件回调内使用，与
/// app.hide() 同 tick 完成，避免异步排队期间与窗口隐藏竞争。
pub fn hide_child_now(app: &AppHandle) {
    SUPPRESSED.store(true, Ordering::SeqCst);
    hide_child_on_main(app);
}

/// 显示工作台（主窗恢复 / 关闭抽屉与面板）：把 child 移回窗口内正确几何。
pub fn show_child(app: &AppHandle) {
    SUPPRESSED.store(false, Ordering::SeqCst);
    let app2 = app.clone();
    let _ = app2.clone().run_on_main_thread(move || {
        if !apply_bounds_on_main(&app2) && FALLBACK.load(Ordering::SeqCst) {
            // 降级路径（独立窗口）：show 恢复
            if let Some(w) = app2.get_webview_window(LABEL) {
                let _ = w.show();
                let _ = w.set_focus();
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

/// 顶栏折叠态同步（shell.js 折叠按钮 / 展开把手调用）。
///
/// 几何延迟到顶栏 CSS 动画（theme.css --dur，150ms）结束后再应用：工作台是
/// 原生 webview，位置无法参与 CSS 动画，若立即 set_bounds 会出现「工作台已
/// 跳到新位置、顶栏还在滑动」的重叠/露底闪烁（用户反馈「伸缩有问题」）。
#[tauri::command]
pub fn workbench_set_collapsed_cmd(app: AppHandle, collapsed: bool) {
    crate::logln(&format!("[workbench] collapsed 同步: {collapsed}"));
    COLLAPSED.store(collapsed, Ordering::SeqCst);
    let app2 = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(170));
        // sync_bounds 内部检查 SUPPRESSED：抽屉/命令面板打开期间不会被移动
        sync_bounds(&app2);
    });
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
    fn geom_collapsed_keeps_handle_strip() {
        // 折叠：顶栏收起，但保留 18pt 展开把手条（否则原生工作台会盖住把手，
        // 用户点不到展开入口）→ y = 18*2 = 36，h = 1640 - 36 = 1604
        let g = geom(2560, 1640, 2.0, true);
        assert_eq!((g.x, g.y, g.w, g.h), (0, 36, 2560, 1604));
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
