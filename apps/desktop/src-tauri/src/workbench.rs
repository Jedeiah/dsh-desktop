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
/// 与 ui/theme.css `--dsh-h-handle`（18px）、shell.js 折叠布局保持一致。
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

/// 注入 dsh 页面 document-start 的深色底：消除「刷新/导航时新页面在 CSS 生效前
/// 自行绘制浏览器默认白底」造成的闪白（这是页面画的白，设 webview 背景无效）。
const DARK_BG_JS: &str = r#"(function(){try{
var e=document.documentElement;if(!e)return;
var s=document.createElement('style');
s.textContent='html,body{background:#151517 !important}';
(e.head||e).appendChild(s);
}catch(_){}})();"#;

/// 注入 dsh 页面 document-start 的壳层快捷键转发：工作台是独立原生 webview，
/// 壳页的 window keydown 收不到这里的按键（同 shell.js ⌘C 注释的限制）；Windows
/// 又没有系统菜单加速键可代劳，导致焦点在工作台时快捷键全部失效（用户实测）。
/// 把 App 级组合键（MOD+K / MOD+1–4）以 `shell:shortcut` 事件发回壳页统一处理。
/// 不转发 Esc 等单键——dsh 页面自身可能有用，不能干扰。macOS 上 ⌘K 会被系统
/// 菜单加速键优先接管，本脚本收不到也无需（菜单已转发）；⌘1–4 无菜单加速键，
/// 恰好由本脚本补齐。
const SHORTCUT_FORWARD_JS: &str = r#"(function(){try{
var mac=/Mac/i.test(navigator.platform||navigator.userAgent||'');
window.addEventListener('keydown',function(e){
  var mod=mac?e.metaKey:e.ctrlKey;
  if(!mod||e.altKey||e.shiftKey)return;
  var k=e.key,cmd=null;
  if(k==='k'||k==='K')cmd='k';
  else if(k==='1'||k==='2'||k==='3'||k==='4')cmd=k;
  if(!cmd)return;
  e.preventDefault();e.stopPropagation();
  try{window.__TAURI__&&window.__TAURI__.event&&window.__TAURI__.event.emit('shell:shortcut',cmd);}catch(_){}
});
}catch(_){}})();"#;

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
    // 只有「真的会导航」（首次创建 / dsh 换端口换 token）才复位就绪态并清 cookie：
    // dsh 偶尔会把同一个就绪 URL 输出两次，第二次若也清 cookie，会把当前活跃会话
    // 那张删掉、而页面并没有重载 → SPA 后续请求变成未认证（且 ready 不再补发）。
    let navigating = crate::mlock(&CUR_URL).as_deref() != Some(url);
    if navigating {
        READY.store(false, Ordering::SeqCst); // 新导航开始：占位层需重新盖住
        // 根因修复：导航前清历史 dsh 会话 cookie（防 Cookie 头累计超 dsh 16KB 上限 →
        // 431 → 空白工作台）。**必须在主线程之外调用**，见 purge_stale_auth_cookies。
        let _ = purge_stale_auth_cookies(app);
    }
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

/// 清理历史遗留的 dsh 会话 cookie（工作台空白根因修复）。
///
/// dsh 每次启动用**随机端口**，会话 cookie 名为 `dsh-auth-<sha256(host:port)>`，
/// 且 `Max-Age=2592000`(30 天) / `Path=/` / host 固定 `127.0.0.1`（ip 不是 cookie
/// Domain，按 host-only 存）。于是**每跑一次 app 就永久多一条**：端口不同 → 名字
/// 不同 → 都匹配 `127.0.0.1` → WKWebView 会把它们**全部**塞进 Cookie 头。
///
/// 累计约 65 条 / ≥16KB 时，dsh 的 Node http 服务（默认 `maxHeaderSize` 16KB）
/// 直接回 `431 Request Header Fields Too Large`：**空 body、无 content-type**。
/// WebKit 于是渲染出一个空 `text/plain` 文档、URL 停在 token URL、303 从不发生
/// → 工作台空白（实测阈值：Cookie 头 15,870B → 303 正常；16,366B → 431）。
/// 症状与「最近哪次提交改坏了」无关，是**随反复调试启动逐步累积**的：这也解释了
/// 为什么早先能用、后来突然全白。
///
/// **必须在本函数调用方（非主线程）执行**：`Webview` 的 cookie 接口会把消息派发到
/// 主线程事件循环，每次都泵 run loop（单次上限 1s）；若在主线程内联执行，几十次删除
/// 会把主干占满（实测 >5s），child webview 创建被判超时 → 误降级为独立窗口。
///
/// 清理后 Cookie 头稳定在 1 条（本次导航前该端口必然还没有 cookie，dsh 的 token→303
/// 交换会重新下发一条）。
fn purge_stale_auth_cookies(app: &AppHandle) -> usize {
    let started = std::time::Instant::now();
    let Some(window) = app.get_window(crate::WINDOW_LABEL) else {
        return 0;
    };
    // 所有 webview 共用 defaultDataStore（app 未设 data_store_identifier），
    // 故用主窗口 webview 清理即对 child webview 生效。
    let Some(wv) = window.get_webview(crate::WINDOW_LABEL) else {
        return 0;
    };
    let Ok(cookies) = wv.cookies() else {
        crate::logln("[workbench] 枚举 cookie 失败，跳过历史 dsh 会话 cookie 清理");
        return 0;
    };
    let total = cookies.len();
    let stale: Vec<_> = cookies
        .into_iter()
        .filter(|c| c.name().starts_with("dsh-auth-"))
        .collect();
    let count = stale.len();
    for cookie in stale {
        let _ = wv.delete_cookie(cookie);
    }
    if count > 0 {
        crate::logln(&format!(
            "[workbench] 已清理历史 dsh 会话 cookie {count}/{total} 条（防止 Cookie 头超 16KB 触发 431），耗时 {}ms",
            started.elapsed().as_millis()
        ));
    }
    count
}

/// 主线程内执行：true = 工作台 webview 就绪（创建或复用成功）。
fn ensure_ready_on_main(app: &AppHandle, url: &str) -> bool {
    let Ok(parsed) = url.parse::<tauri::Url>() else {
        crate::logln("[workbench] 无法解析 dsh URL，跳过");
        return false;
    };
    let Some(window) = app.get_window(crate::WINDOW_LABEL) else {
        return false;
    };
    // 已创建：仅 URL 变化时重导航（dsh 崩溃自愈换端口/token）
    if let Some(wv) = window.get_webview(LABEL) {
        // 自愈：child 确实存在 ⇒ 撤销降级标记。FALLBACK 会被「主线程拥塞超 5s 的
        // recv_timeout」等瞬时原因误置，而旧实现一旦置位就再不复位，工作台会永久停在
        // 屏幕外（后续 apply_bounds / hide 全部走 fallback 分支）。
        FALLBACK.store(false, Ordering::SeqCst);
        if url_changed(crate::mlock(&CUR_URL).as_deref(), url) {
            let _ = wv.navigate(parsed);
            *crate::mlock(&CUR_URL) = Some(url.to_string());
        }
        return true;
    }
    if FALLBACK.load(Ordering::SeqCst) {
        // 降级态（路径 C）：独立窗口承载工作台。换 URL 重导航后必须重新宣告就绪，
        // 否则壳页 workbench_ready_cmd 恒为 false、占位层永不撤（旧实现此处只
        // navigate 不置 READY）。
        // 按 windows 表取窗口、再取窗口里的 webview：该环境
        // `get_webview_window()`/`webview_windows()` 恒为空（见 main.rs MAIN_WIN 注释），
        // 只有 `get_window()` 可靠，用错会静默跳过（既不导航也不宣告就绪）。
        if let Some(w) = app.get_window(LABEL) {
            if let Some(wv) = w.get_webview(LABEL) {
                let _ = wv.navigate(parsed);
                let _ = wv.emit("workbench:ready", ());
            }
            let _ = w.show();
            READY.store(true, Ordering::SeqCst);
        }
        return true; // 降级态：窗口路径自洽，不再尝试 child
    }
    let builder = WebviewBuilder::new(LABEL, WebviewUrl::External(parsed))
        .initialization_script(DARK_BG_JS)
        .initialization_script(SHORTCUT_FORWARD_JS)
        .on_navigation(crate::webview_navigation_policy)
        .on_new_window(crate::webview_new_window_policy)
        .on_page_load(|wv, payload| {
            if payload.event() == tauri::webview::PageLoadEvent::Finished {
                crate::logln(&format!(
                    "[workbench] page loaded: {}",
                    crate::redact_token(payload.url().as_ref())
                ));
                // 诊断探针：记录 content-type/title/href——区分「工作台 HTML」与
                // 「认证失败纯文本页」（历史教训：工作台空白时日志无据可查）。
                // href 还能判断 token→303 是否真的发生（停在带 token 的原 URL ⇒
                // 请求未通过认证链）。
                // 只取元信息（不读正文），避免把工作台内容写进日志。
                let _ = wv.eval_with_callback(
                    "JSON.stringify({href:location.origin+location.pathname,ct:document.contentType,t:document.title,vw:innerWidth,vh:innerHeight,sw:document.documentElement.scrollWidth,sh:document.documentElement.scrollHeight,dpr:devicePixelRatio})",
                    |r| crate::logln(&format!("[workbench] probe: {r}")),
                );
                // 借鉴 main 的启动衔接：dsh 是 SPA，page-load Finished 早于首帧渲染
                // 完成，立即移入会露出空白/半成品。延迟 900ms 让首帧渲染完成，再移入
                // 工作台（原生视图盖住壳页的加载页），**移入之后才宣告就绪**：
                // 壳页收到 workbench:ready 后再延迟撤掉加载页——这样「撤加载页」永远
                // 发生在工作台已经盖住它之后，用户看不到「加载页已撤、工作台没到」
                // 的空档（此前 ready 在 Finished 时立即发，撤页比移入早 900ms，中间
                // 露出背景——即用户反馈的「启动时看到背景」）。
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
                            READY.store(true, Ordering::SeqCst);
                            let _ = a2.emit("workbench:ready", ());
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
                // 注意：这里**不能**立即移入工作台、也**不能**立即发 workbench:ready。
                //
                // page-load Finished 只代表文档加载完，此时 dsh 这个 SPA 尚未绘制首帧，
                // 原生 child webview 一旦移入就会盖住壳页的加载页，用户看到的先是
                // 注入的深色底（就是反馈里的「加载页消失后先出现背景，之后才出现 dsh
                // 页面」）；而立即发 ready 又会让壳页 1.2s 后撤掉加载页，进一步把空档
                // 暴露出来。移入与 ready 都交给下面 900ms 后的延迟分支（先让 SPA 在
                // 屏幕外完成布局与首帧，再移入；移入之后才宣告就绪）。
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
    // set_bounds 的坐标基准是「窗口客户区」，所以**不能**再叠加 outer−inner 的
    // 标题栏/边框差值：
    //   - macOS：本环境实测 outer==inner（inset=0），子视图坐标即内容视图坐标；
    //   - Windows：wry 把 WebView2 装进 WS_CHILD 容器窗口（wry webview2/mod.rs 的
    //     create_container_hwnd 用 WS_CHILD | WS_CLIPCHILDREN），SetWindowPos 的坐标
    //     相对父窗口客户区，而 Tauri 的 inner_size() 在 Windows 上就是客户区尺寸；
    //   - Linux(webkit2gtk)：同为窗口内子控件。
    // 叠加差值会让工作台整体下移一个标题栏高度、底边被窗口裁掉（Windows 100% DPI
    // 约 39px），顶栏下方还会多出一条壳页底色。
    let outer_h = window.outer_size().unwrap_or(size).height as i64;
    let inset = (outer_h - size.height as i64).max(0) as u32; // 仅用于日志观测
    let g = geom(size.width, size.height, scale, collapsed);
    // 顶栏 + 标题栏补偿（见 TITLEBAR_H_PT 注释）；高度再扣掉标题栏
    let tb_px = (TITLEBAR_H_PT * scale).round() as u32;
    let y = g.y as u32 + tb_px;
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
fn apply_shell_clip_on_main(_app: &AppHandle) {
    // 非 macOS 平台本函数为空实现；参数统一写成 `_app`，否则 Windows 上会触发
    // unused_variables —— CI 的 Windows job 用 `clippy --all-targets -- -D warnings`
    // 把关，警告即失败。
    #[cfg(target_os = "macos")]
    {
        let Some(w) = crate::main_window(_app) else {
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
        if let Some(w) = app.get_window(LABEL) {
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
        // 首次就绪前也不动：此时工作台仍在屏幕外预渲染（尺寸已按 bounds_on_main
        // 建好，只差位置）。若在这里顺手移入，就会抢在 on_page_load 里 900ms 的
        // 延迟分支之前把还没绘制首帧的 SPA 盖到加载页上——用户看到的就是
        // 「加载页消失 → 一片背景 → 才出现 dsh 页面」。移入只由延迟分支与
        // show_child（抽屉/面板关闭后恢复）触发。
        if !WORKBENCH_VISIBLE.load(Ordering::SeqCst) {
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
    // 存在性判定必须查 windows 表：`get_webview_window` 查的是 webviews 表，在该环境
    // 恒为空 —— 漏判就会再次 build 同 label 窗口，撞 "already exists" 后降级窗口彻底建不出来。
    if let Some(w) = app.get_window(LABEL) {
        if let Some(wv) = w.get_webview(LABEL) {
            let _ = wv.navigate(parsed);
        }
        let _ = w.show();
        let _ = w.set_focus();
        return;
    }
    let built = WebviewWindowBuilder::new(app, LABEL, WebviewUrl::External(parsed))
        .title(FALLBACK_TITLE)
        .inner_size(1280.0, 820.0)
        .min_inner_size(800.0, 560.0)
        .theme(Some(tauri::Theme::Dark))
        .initialization_script(SHORTCUT_FORWARD_JS) // 降级窗口同样是独立 webview，快捷键同样需要转发
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
            if let Some(w) = app2.get_window(LABEL) {
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
            if let Some(w) = app2.get_window(LABEL) {
                if let Some(wv) = w.get_webview(LABEL) {
                    let _ = wv.eval("location.reload()");
                }
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
