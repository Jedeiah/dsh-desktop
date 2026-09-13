# 工作台原生 WebView（child webview 替换 iframe 认证桥）实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 用同窗口 child webview（顶层文档即 dsh 工作台 URL，first-party）替换「跨站 iframe + 认证桥 cookie 注入」，使工作台在 macOS/Windows 必然显示且不依赖 dsh 认证协议细节。

**Architecture:** 主窗保留 shell.html（顶栏 + 管理面板）；dsh 就绪后 Rust 在主线程 `window.add_child(WebviewBuilder::new("workbench", WebviewUrl::External(url)))` 创建 child webview，认证由 WKWebView/WebView2 原生完成；tab 切换 show/hide，窗口 resize/顶栏折叠联动 `set_bounds`；`add_child` 失败降级为独立 `WebviewWindow`（路径 C）。删除 `webview_auth.rs` 及 objc2/webview2-com 依赖。

**Tech Stack:** Tauri 2.11.5（`unstable` feature）+ wry 0.55.1 `build_as_child`；壳页原生 HTML/CSS/JS（零构建链）。

**Spec:** `docs/superpowers/specs/2026-09-13-workbench-native-webview-design.md`（实现依据；执行前先读 spec）

## Global Constraints

- tauri 依赖版本锁定 `2.11.5`（Cargo.lock 现值），新增 feature：`unstable`；**不得**升级 tauri/wry 版本。
- 删除且仅删除认证桥依赖：`objc2 = "=0.6.4"`、`objc2-foundation = "=0.3.2"`、`webview2-com = "=0.38.2"`、`block2`（若存在且仅被 webview_auth.rs 引用，执行前用 grep 验证）；**不动** windows-core / trash 等其它锁版本。
- dsh 启动参数不变：`node bin.js --profile web --port 0`（spawn_dsh 不改）。
- 壳页零构建链零 npm 依赖：shell.js 必须是经典脚本（无顶层 await），可用 `node -e "new (require('vm').Script)(fs.readFileSync(...))"` 校验语法（项目既有惯例）。
- child webview 不注入 `__TAURI__`（WebviewBuilder 默认即不注入，无需动作，但不得显式开启）。
- 外部链接策略不变：`webview_navigation_policy` / `webview_new_window_policy` 复用（外部 http(s) 转系统浏览器）。
- 顶栏高度字面量：常态 `46px`、折叠 `0px`（与 shell.css `--dsh-topbar-h` 及 shell.js `applyTabsCollapsed` 一致）。
- 提交信息用中文 conventional commits（fix(webview)/feat(webview)/chore 等，仓库惯例）。

---

### Task 1: 移除认证桥（依赖 + 模块 + boot 引用）

**Files:**
- Delete: `apps/desktop/src-tauri/src/webview_auth.rs`
- Modify: `apps/desktop/src-tauri/src/main.rs`（`mod webview_auth;` 声明 + boot 内调用处，约 802 行）
- Modify: `apps/desktop/src-tauri/Cargo.toml`（依赖摘除）

**Interfaces:**
- Consumes: 无（纯移除）。
- Produces: `main.rs` 的 `boot()` 中 stdout 解析到 URL 后**不再调用**任何认证逻辑（Task 3 会在此处插入 `workbench::ensure_ready`）；`DSH_URL` 静态量与 `reveal_main_window` 保持不变。

- [ ] **Step 1: 确认认证桥依赖引用面**

```bash
cd apps/desktop/src-tauri
grep -rn "webview_auth\|objc2\|webview2_com\|webview2-com\|block2" src/ Cargo.toml
```

预期：`webview_auth` 仅在 main.rs 有 `mod webview_auth;` 与 `webview_auth::preauth_and_inject` 两处；objc2/block2 仅 webview_auth.rs 使用。若发现其它引用，停下报告，不得扩大删除面。

- [ ] **Step 2: 删除模块与引用**

```bash
git rm apps/desktop/src-tauri/src/webview_auth.rs
```

main.rs 中删除 `mod webview_auth;`（及其上方关联注释），并删除 boot() 中这一段（约 800-802 行）：

```rust
// 认证桥：先完成会话预认证 + cookie 注入，再让壳页拿到
// URL——保证 iframe 首帧即带 cookie（注入失败容忍跳过）
crate::webview_auth::preauth_and_inject(&app, &url);
```

- [ ] **Step 3: Cargo.toml 摘除依赖**

删除以下行（若名称/行号有差异以 Step 1 的 grep 结果为准）：

```toml
objc2 = "=0.6.4"
objc2-foundation = "=0.3.2"
webview2-com = "=0.38.2"
```

以及 `block2`（若存在）与其关联注释行（"WKHTTPCookieStore 注入（工作台 iframe 认证桥）…"、"CoreWebView2CookieManager 注入…"）。

- [ ] **Step 4: 编译与测试**

```bash
cd apps/desktop/src-tauri && cargo check 2>&1 | tail -5 && cargo test 2>&1 | tail -8
```

Expected: check 零 error（warning 中不得再出现 objc2/webview2 相关）；`cargo test` 全部通过（`dsh_url_port_extracts_port_from_token_url` 等既有测试不受影响）。

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "refactor(webview): 移除 iframe 认证桥（webview_auth.rs + objc2/webview2-com 依赖）——改用原生 WebView first-party 认证（步骤 1/2）"
```

---

### Task 2: workbench.rs 纯逻辑（几何 + URL 判定，TDD）

**Files:**
- Create: `apps/desktop/src-tauri/src/workbench.rs`
- Modify: `apps/desktop/src-tauri/src/main.rs`（加 `mod workbench;`）

**Interfaces:**
- Consumes: 无。
- Produces（Task 3 依赖，签名必须一致）:
  - `pub const TOPBAR_H_LOGICAL: f64` = `46.0`
  - `pub struct Geom { pub x: i32, pub y: i32, pub w: u32, pub h: u32 }`
  - `pub fn geom(win_w: u32, win_h: u32, scale: f64, collapsed: bool) -> Geom`
  - `pub fn url_changed(current: Option<&str>, next: &str) -> bool`

- [ ] **Step 1: 写失败测试（先于实现，放进 workbench.rs 底部 `#[cfg(test)] mod tests`）**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn geom_normal_topbar() {
        // 1280x820 逻辑 @2x：y = 46*2 = 92，h = 820*2 - 92 = 1548
        let g = geom(2560, 1640, 2.0, false);
        assert_eq!((g.x, g.y, g.w, g.h), (0, 92, 2560, 1548));
    }

    #[test]
    fn geom_collapsed_topbar_full_height() {
        let g = geom(2560, 1640, 2.0, true);
        assert_eq!((g.x, g.y, g.w, g.h), (0, 0, 2560, 1640));
    }

    #[test]
    fn geom_scale_one() {
        let g = geom(1280, 820, 1.0, false);
        assert_eq!((g.x, g.y, g.w, g.h), (0, 46, 1280, 774));
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
```

- [ ] **Step 2: 运行确认失败**

```bash
cd apps/desktop/src-tauri && cargo test workbench 2>&1 | tail -5
```

Expected: 编译失败（`workbench` 模块尚无 `geom`/`url_changed`，或文件不存在）。若先建文件则测试 FAIL。

- [ ] **Step 3: 最小实现**

```rust
//! 工作台 child webview（first-party 顶层文档）管理与几何计算。
//!
//! dsh 就绪 URL 直接作为 child webview 的顶层导航地址：token→303→Set-Cookie
//! 全部由 WebView 原生处理，壳不解析/不注入任何认证细节（spec 2026-09-13）。

/// 顶栏逻辑高度；必须与 ui/shell.css `--dsh-topbar-h`（46px）一致。
pub const TOPBAR_H_LOGICAL: f64 = 46.0;

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
```

main.rs 顶部模块声明区（`mod webview_auth;` 原位置附近）加：

```rust
mod workbench;
```

- [ ] **Step 4: 运行确认通过**

```bash
cd apps/desktop/src-tauri && cargo test workbench 2>&1 | tail -5
```

Expected: 4 个测试全部 PASS。

- [ ] **Step 5: Commit**

```bash
git add apps/desktop/src-tauri/src/workbench.rs apps/desktop/src-tauri/src/main.rs && git commit -m "feat(webview): workbench 模块纯逻辑——工作台几何计算与就绪 URL 变化判定"
```

---

### Task 3: workbench webview 生命周期（创建/显隐/联动 + boot 衔接 + 路径 C 降级）

**Files:**
- Modify: `apps/desktop/src-tauri/src/workbench.rs`（追加生命周期实现）
- Modify: `apps/desktop/src-tauri/src/main.rs`（boot 衔接 + command 注册 + 主窗 Resized 联动）

**Interfaces:**
- Consumes: Task 2 的 `geom`/`url_changed`；main.rs 的 `DSH_URL`（`Mutex<Option<String>>`）、`webview_navigation_policy`、`webview_new_window_policy`、`logln!`、`WINDOW_LABEL`。
- Produces（Task 4 依赖）:
  - command：`show_workbench_cmd()`、`hide_workbench_cmd()`、`workbench_reload_cmd()`、`workbench_ready_cmd() -> bool`、`workbench_set_collapsed_cmd(collapsed: bool)`
  - 事件：`workbench:ready`（payload 空，child webview 首次 Finished 时 emit）
  - `pub fn ensure_ready(app: &tauri::AppHandle, url: &str)`（boot 调用入口）

- [ ] **Step 1: workbench.rs 追加实现**

在 Task 2 的纯逻辑之下追加（`use` 区按需补 `tauri::{AppHandle, Manager, WebviewUrl, WebviewBuilder, Emitter, PhysicalPosition, PhysicalSize}`）：

```rust
use std::sync::atomic::{AtomicBool, Ordering};

/// 工作台 webview 标签（同时是 child webview 与降级窗口的 label）。
pub const LABEL: &str = "workbench";
/// 降级窗口标题。
const FALLBACK_TITLE: &str = "DeepSeek Harness 工作台";

static COLLAPSED: AtomicBool = AtomicBool::new(false);
static READY: AtomicBool = AtomicBool::new(false);
/// 降级模式（路径 C：独立窗口）。add_child 成功=false。
static FALLBACK: AtomicBool = AtomicBool::new(false);

/// dsh 就绪入口（boot stdout 解析到 URL 后调用；可重入）：
/// 未创建则创建 child webview 并导航到 url；已创建且 URL 变化则重导航。
/// 创建失败（add_child 异常）自动降级为独立窗口（路径 C），仅降级一次。
pub fn ensure_ready(app: &tauri::AppHandle, url: &str) {
    READY.store(false, Ordering::SeqCst); // 新导航开始，占位层应重新盖住
    let current = {
        let app2 = app.clone();
        let u = url.to_string();
        let (tx, rx) = std::sync::mpsc::channel::<bool>();
        let _ = app2.run_on_main_thread(move || {
            let _ = tx.send(ensure_ready_on_main(&app2, &u));
        });
        rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap_or(false)
    };
    if !current {
        logln!("[workbench] child webview 创建失败，降级为独立窗口（路径 C）");
        FALLBACK.store(true, Ordering::SeqCst);
        let app2 = app.clone();
        let u = url.to_string();
        let _ = app2.run_on_main_thread(move || {
            open_fallback_window(&app2, &u);
        });
    }
    sync_bounds(app);
}

/// 主线程内执行：返回 true=child 创建/复用成功。
fn ensure_ready_on_main(app: &AppHandle, url: &str) -> bool {
    if FALLBACK.load(Ordering::SeqCst) {
        if let Some(w) = app.get_webview_window(LABEL) {
            let _ = w.navigate(url.parse::<tauri::Url>().expect("valid dsh url"));
            let _ = w.show();
        }
        return true; // 降级态：窗口路径自洽，不再尝试 child
    }
    if let Some(window) = app.get_webview_window(crate::WINDOW_LABEL) {
        if let Some(wv) = window.get_webview(LABEL) {
            // 已存在：URL 变化才重导航（dsh 换端口/重启场景）
            let cur = mlock_url();
            if url_changed(cur.as_deref(), url) {
                let _ = wv.navigate(url.parse::<tauri::Url>().expect("valid dsh url"));
                *CUR_URL.lock().unwrap() = Some(url.to_string());
            }
            return true;
        }
        let parsed: tauri::Url = url.parse().expect("valid dsh url");
        let builder = WebviewBuilder::new(LABEL, WebviewUrl::External(parsed))
            .on_navigation(crate::webview_navigation_policy)
            .on_new_window(crate::webview_new_window_policy)
            .on_page_load(|_wv, payload| {
                if payload.event() == tauri::webview::PageLoadEvent::Finished {
                    READY.store(true, Ordering::SeqCst);
                    // emit 由调用方 app handle 完成（见下）——此处无 app 句柄，
                    // 通过静态汇报由 ensure_ready 的调用线程 emit。
                }
            });
        // 主窗 Resized 联动（挂一次；重复挂会堆叠，用标志防重）
        match window.add_child(builder, PhysicalPosition::new(0, 0), PhysicalSize::new(1u32, 1u32)) {
            Ok(_wv) => {
                *CUR_URL.lock().unwrap() = Some(url.to_string());
                attach_resize_hook(app);
                true
            }
            Err(e) => {
                logln!("[workbench] add_child 失败: {e}");
                false
            }
        }
    } else {
        false
    }
}
```

> 实现注意（执行者必读）：
> 1. `on_page_load` 闭包拿不到 `AppHandle`——ready 事件改在 `ensure_ready` 的 `run_on_main_thread` 收尾处 emit 不可行（页面加载是异步的）。正确做法：`on_page_load` 里用 `std::sync::OnceLock<tauri::AppHandle>`（模块级 `APP: OnceLock<AppHandle>`，在 `ensure_ready` 首次调用时 `APP.get_or_init(|| app.clone())` 存入），闭包内 `if let Some(app) = APP.get() { let _ = app.emit("workbench:ready", ()); }`。`READY` 原子量照设，供 `workbench_ready_cmd` 查询兜底（事件丢失防护——本项目 T.event.listen 有丢失史）。
> 2. `CUR_URL` 为模块级 `static CUR_URL: Mutex<Option<String>>`（用 `std::sync::Mutex`，`mlock_url()` 是读锁便捷函数，或直接内联 lock）。
> 3. `add_child` 的初始 size 传 `(1,1)` 后立刻 `sync_bounds` 校正，避免首帧闪烁。
> 4. `attach_resize_hook`：`app.get_webview_window(WINDOW_LABEL)` 上挂 `on_window_event`，`WindowEvent::Resized(_)` → `sync_bounds(app)`。用 `static RESIZE_HOOKED: AtomicBool` 防重复挂载。

```rust
/// 依窗口当前尺寸与折叠态同步 child webview 几何（Resized/折叠切换共用）。
pub fn sync_bounds(app: &tauri::AppHandle) {
    let collapsed = COLLAPSED.load(Ordering::SeqCst);
    let app2 = app.clone();
    let _ = app2.run_on_main_thread(move || {
        let Some(window) = app2.get_webview_window(crate::WINDOW_LABEL) else { return };
        let scale = window.scale_factor().unwrap_or(1.0);
        let size = window.inner_size().unwrap_or_else(|_| tauri::PhysicalSize::new(1280u32, 820u32));
        let g = geom(size.width, size.height, scale, collapsed);
        if FALLBACK.load(Ordering::SeqCst) {
            // 路径 C：独立窗口不做内嵌几何（窗口自身即工作台）
            return;
        }
        if let Some(wv) = window.get_webview(LABEL) {
            let _ = wv.set_bounds(tauri::Rect::new(
                tauri::PhysicalPosition::new(g.x, g.y),
                tauri::PhysicalSize::new(g.w, g.h),
            ));
        }
    });
}

/// 路径 C：独立工作台窗口（stable API；样式贴近主窗）。
fn open_fallback_window(app: &AppHandle, url: &str) {
    use tauri::WebviewWindowBuilder;
    if let Some(w) = app.get_webview_window(LABEL) {
        let _ = w.navigate(url.parse::<tauri::Url>().expect("valid dsh url"));
        let _ = w.show();
        return;
    }
    let _ = WebviewWindowBuilder::new(app, LABEL, WebviewUrl::External(url.parse().expect("valid dsh url")))
        .title(FALLBACK_TITLE)
        .inner_size(1280.0, 820.0)
        .min_inner_size(800.0, 560.0)
        .theme(Some(tauri::Theme::Dark))
        .on_navigation(crate::webview_navigation_policy)
        .on_new_window(crate::webview_new_window_policy)
        .build()
        .map(|w| {
            READY.store(true, Ordering::SeqCst);
            let _ = APP.get().map(|a| a.emit("workbench:ready", ()));
            w
        });
}

/// 显示工作台（切到工作台 tab）。child：show；降级窗口：show+set_focus。
#[tauri::command]
pub fn show_workbench_cmd(app: tauri::AppHandle) {
    let app2 = app.clone();
    let _ = app2.run_on_main_thread(move || {
        if FALLBACK.load(Ordering::SeqCst) {
            if let Some(w) = app2.get_webview_window(LABEL) { let _ = w.show(); let _ = w.set_focus(); }
        } else if let Some(window) = app2.get_webview_window(crate::WINDOW_LABEL) {
            if let Some(wv) = window.get_webview(LABEL) { let _ = wv.show(); }
        }
    });
}

/// 隐藏工作台（切到管理 tab）。
#[tauri::command]
pub fn hide_workbench_cmd(app: tauri::AppHandle) {
    let app2 = app.clone();
    let _ = app2.run_on_main_thread(move || {
        if FALLBACK.load(Ordering::SeqCst) {
            if let Some(w) = app2.get_webview_window(LABEL) { let _ = w.hide(); }
        } else if let Some(window) = app2.get_webview_window(crate::WINDOW_LABEL) {
            if let Some(wv) = window.get_webview(LABEL) { let _ = wv.hide(); }
        }
    });
}

/// 刷新工作台（brand 点击 / 重试）。
#[tauri::command]
pub fn workbench_reload_cmd(app: tauri::AppHandle) {
    let app2 = app.clone();
    let _ = app2.run_on_main_thread(move || {
        if FALLBACK.load(Ordering::SeqCst) {
            if let Some(w) = app2.get_webview_window(LABEL) { let _ = w.eval("location.reload()"); }
        } else if let Some(window) = app2.get_webview_window(crate::WINDOW_LABEL) {
            if let Some(wv) = window.get_webview(LABEL) { let _ = wv.eval("location.reload()"); }
        }
    });
}

/// 壳页启动/事件丢失兜底：工作台是否已就绪（首次 Finished 已发生）。
#[tauri::command]
pub fn workbench_ready_cmd() -> bool {
    READY.load(Ordering::SeqCst)
}

/// 顶栏折叠态同步（shell.js applyTabsCollapsed 调用）。
#[tauri::command]
pub fn workbench_set_collapsed_cmd(app: tauri::AppHandle, collapsed: bool) {
    COLLAPSED.store(collapsed, Ordering::SeqCst);
    sync_bounds(&app);
}
```

- [ ] **Step 2: main.rs 衔接**

a) boot() stdout 解析处（原 `webview_auth::preauth_and_inject` 位置）改为：

```rust
// 工作台通道：child webview 顶层导航到就绪 URL（first-party 认证由
// WebView 原生完成；换端口/重启时 url_changed 判定后重导航）
crate::workbench::ensure_ready(&app, &url);
```

b) `invoke_handler` 列表追加：

```rust
workbench::show_workbench_cmd,
workbench::hide_workbench_cmd,
workbench::workbench_reload_cmd,
workbench::workbench_ready_cmd,
workbench::workbench_set_collapsed_cmd,
```

- [ ] **Step 3: 编译 + 测试**

```bash
cd apps/desktop/src-tauri && cargo check 2>&1 | tail -8 && cargo test 2>&1 | tail -5
```

Expected: 零 error。若 `WebviewBuilder::on_new_window` 在 unstable feature 下不存在（版本差异），移除该行并把 `webview_new_window_policy` 仅保留在降级窗口 builder 上——child webview 的弹窗默认被 wry 拒绝，可接受（工作台外链走导航策略已在 `on_navigation` 拦截）。若 `Webview::navigate` 不存在则改用 `wv.eval("location.replace('<url>')")` 等价实现并注明。

- [ ] **Step 4: 手动烟测（cargo tauri dev，macOS）**

```bash
cd apps/desktop && cargo tauri dev
```

Expected（dsh 已安装，本机 closure v0.1.2-rc.1）：窗口出现 → 占位「正在启动 dsh 工作台…」→ 数秒后 child webview 在顶栏下方出现且**加载出 dsh 工作台**（无 401、无 "authentication required"）；日志出现 `[workbench]` 与 `workbench:ready` 相关行为。此刻 shell.js 尚未改造，占位层不会自动撤——属预期（Task 4 处理）；用 DevTools（右键 Inspect 若启用）或临时日志确认 child webview 内容已渲染即可。

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat(webview): workbench child webview 生命周期——创建/显隐/几何联动/就绪事件 + 独立窗口降级（路径 C）"
```

---

### Task 4: 壳页改造（shell.html / shell.js）

**Files:**
- Modify: `apps/desktop/ui/shell.html`（删除 iframe 元素，占位层保留）
- Modify: `apps/desktop/ui/shell.js`（删除 iframe/轮询逻辑，改 command 驱动）

**Interfaces:**
- Consumes: Task 3 的 5 个 command 与 `workbench:ready` 事件。
- Produces: 壳页行为——tab 切换驱动 child webview 显隐；占位层由 ready 信号撤除。

- [ ] **Step 1: shell.html 删 iframe**

删除（约 308-310 行）：

```html
<iframe id="workbenchFrame" allow="clipboard-write" title="DeepSeek Harness 工作台"></iframe>
```

同一 `#activity-workbench` 容器内保留 `#wbPlaceholder`（占位层）与 `#setupView`。给容器加注释说明「工作台由原生 child webview 承载（Rust workbench 模块管理），此容器仅承载占位与引导视图」。

- [ ] **Step 2: shell.js 删旧逻辑**

删除：
- `const wb = $('workbenchFrame');`（100 行附近）及 `wb.addEventListener('load'|'error')` 两处（125-129 行）
- `loadWorkbench` 函数（131-145 行）与 brand 点击中的 `loadWorkbench(lastUrl, true)`（149-150 行，改为 reload command）
- 155-177 行的轮询 `get_dsh_url` + `T.event.listen('dsh:url')` 整个 IIFE

- [ ] **Step 3: shell.js 新增信号驱动逻辑**

```js
// ---------------- 工作台（child webview 由 Rust workbench 模块管理） ----------------
const wbPlaceholder = $('wbPlaceholder');
const setupView = $('setupView');
let setupActive = false; // 引导视图是否正覆盖工作台
let workbenchReady = false;

function showPlaceholder() {
  clearTimeout(placeholderTimer);
  wbPlaceholder.style.display = '';
  wbPlaceholder.hidden = false;
}
function hidePlaceholder() {
  wbPlaceholder.hidden = true;
  wbPlaceholder.style.display = 'none';
}
let placeholderTimer = null;

// 就绪信号：事件为主（Rust 首次 Finished emit），轮询兜底（本环境 listen 有丢失史）
(async () => {
  try {
    workbenchReady = await invoke('workbench_ready_cmd');
  } catch (e) { /* 能力缺失：仅靠事件 */ }
  if (workbenchReady) { hidePlaceholder(); if (setupActive) hideSetupView(); return; }
  try {
    await Promise.race([
      T.event.listen('workbench:ready', () => markReady()),
      new Promise((_, rej) => setTimeout(() => rej(new Error('listen-timeout')), 3000)),
    ]);
  } catch (e) { /* 超时走轮询兜底 */ }
  for (let i = 0; i < 60 && !workbenchReady; i++) {
    await new Promise((r) => setTimeout(r, 1000));
    try { if (await invoke('workbench_ready_cmd')) markReady(); } catch (e) { /* 忽略 */ }
  }
})();
function markReady() {
  if (workbenchReady) return;
  workbenchReady = true;
  hidePlaceholder(); // 就绪后延迟撤占位，等首帧渲染（child webview load 已含绘制）
  if (setupActive) hideSetupView();
}

// 顶栏折叠联动（applyTabsCollapsed 内追加，c = 折叠态）
//   invoke('workbench_set_collapsed_cmd', { collapsed: c }).catch(() => {});
```

（把 `invoke('workbench_set_collapsed_cmd', ...)` 插入现有 `applyTabsCollapsed(c)` 函数尾部。）

- [ ] **Step 4: tab 切换接入显隐**

`tabOf(name)` 改为：

```js
function tabOf(name) {
  if (name === 'workbench') {
    document.querySelectorAll('.panel').forEach((p) => p.classList.remove('active'));
    $('activity-workbench').style.display = 'block';
    invoke('show_workbench_cmd').catch(() => {});
    if (!workbenchReady) showPlaceholder();
    return;
  }
  $('activity-workbench').style.display = 'none';
  invoke('hide_workbench_cmd').catch(() => {});
  TAB_NAMES.slice(1).forEach((n) => {
    $('panel-' + n).classList.toggle('active', n === name);
  });
  if (name === 'dsh') refreshDsh();
  if (name === 'plugins') refreshPlugins();
}
```

brand 刷新（148-150 行）改为：

```js
brandEl.addEventListener('click', () => { invoke('workbench_reload_cmd').catch(() => {}); });
brandEl.addEventListener('keydown', (e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); invoke('workbench_reload_cmd').catch(() => {}); } });
```

- [ ] **Step 5: setup 流程改信号驱动**

`startSetupProgressPolling` 中 `if (st.dsh_url) { ... loadWorkbench(st.dsh_url, true); ... }` 分支改为：

```js
if (st.dsh_url) {
  waitBusyReset = false;
  clearInterval(setupProgressTimer);
  if (setupActive) hideSetupView();   // 收引导视图
  invoke('show_workbench_cmd').catch(() => {});
  if (!workbenchReady) showPlaceholder(); // 占位保持到 workbench:ready
  return;
}
```

`hideSetupView` 中 `if (!lastUrl) showPlaceholder();` 改为 `if (!workbenchReady) showPlaceholder();`（`lastUrl` 变量一并删除）。

- [ ] **Step 6: 语法与行为校验**

```bash
node -e "const fs=require('fs'),vm=require('vm');new vm.Script(fs.readFileSync('apps/desktop/ui/shell.js','utf8'));console.log('SYNTAX_OK')"
```

Expected: `SYNTAX_OK`（经典脚本无顶层 await）。grep 确认无残留：`grep -n "workbenchFrame\|loadWorkbench\|get_dsh_url\|dsh:url" apps/desktop/ui/shell.js` 应零命中（`get_dsh_state` 保留，勿误删）。

- [ ] **Step 7: 手动验证（cargo tauri dev）**

启动 → 占位显示 → dsh 就绪后 child webview 出现 → `workbench:ready` 撤占位；⌘K 切到 dsh/插件/关于（工作台隐藏、面板正常）→ 切回工作台（状态保留、无重载闪烁）；折叠顶栏 → 工作区与 child webview 同步扩展；brand 点击 → 工作台重载。

- [ ] **Step 8: Commit**

```bash
git add apps/desktop/ui/shell.html apps/desktop/ui/shell.js && git commit -m "feat(shell): 壳页工作台切换 command 化（show/hide/reload/collapsed）+ workbench:ready 信号撤占位——移除 iframe 与 URL 轮询"
```

---

### Task 5: CSP 收紧 + 全量验证

**Files:**
- Modify: `apps/desktop/src-tauri/tauri.conf.json`（CSP）

**Interfaces:**
- Consumes: Task 1-4 全部完成。
- Produces: 收紧后的安全配置 + 回归记录。

- [ ] **Step 1: CSP 收紧**

`security.csp` 由：

```json
"default-src 'self'; frame-src 'self' http://127.0.0.1:* http://localhost:*; connect-src 'self' http://127.0.0.1:* ws://127.0.0.1:* http://localhost:* ws://localhost:*; img-src 'self' data:; style-src 'self' 'unsafe-inline'; script-src 'self'"
```

改为（删 `frame-src`——不再有 iframe；child webview 是独立文档，其资源不受壳页 CSP 约束）：

```json
"default-src 'self'; connect-src 'self'; img-src 'self' data:; style-src 'self' 'unsafe-inline'; script-src 'self'"
```

- [ ] **Step 2: 全量检查**

```bash
cd apps/desktop/src-tauri && cargo check 2>&1 | tail -3 && cargo test 2>&1 | tail -3 && cargo clippy -- -D warnings 2>&1 | tail -5
```

Expected: check/test/clippy 全过（macOS 本地；Windows 由 CI 验证）。

- [ ] **Step 3: spec 手动回归矩阵（macOS）**

逐项执行 spec §7 清单：启动显示工作台（无 401）／tab 切换状态保留／brand 刷新／kill dsh 进程自愈换端口恢复／外部链接转系统浏览器／窗口缩放+折叠联动／首次引导安装后自动进入／托盘与单实例。记录结果于提交信息。

- [ ] **Step 4: Commit**

```bash
git add apps/desktop/src-tauri/tauri.conf.json && git commit -m "chore(webview): 壳页 CSP 收紧（移除 frame-src）——工作台已无 iframe"
```

---

## Self-Review 记录

- Spec 覆盖：§3 架构（Task 3）、§4 接口表（Task 1/3/4/5 逐文件对应）、§5 错误处理（Task 3 降级路径 + boot 自愈衔接；「add_child 失败仅降级一次」由 `FALLBACK` 原子量保证）、§6 平台注意（Global Constraints 锁版本 + Task 3 Step 3 的 API 差异预案）、§7 验证（Task 2 单测 / Task 3-4 手动 / Task 5 全量）、§8 删除清单（Task 1）。§9 UI 调整独立于本计划（od v4 设计流程并行进行，落地时另行提交）。
- 占位扫描：无 TBD/TODO；Task 3 对两个潜在 API 差异（`navigate`/`on_new_window`）给出了等价替代与判定方法，非占位。
- 类型一致性：`geom`/`url_changed`/`Geom` 在 Task 2 定义、Task 3 消费，签名一致；command 名与 Task 4 的 invoke 名逐字一致（`show_workbench_cmd`/`hide_workbench_cmd`/`workbench_reload_cmd`/`workbench_ready_cmd`/`workbench_set_collapsed_cmd`）；事件名 `workbench:ready` 两处一致。
