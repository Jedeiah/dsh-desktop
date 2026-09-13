# DSh Desktop：工作台原生 WebView 设计（child webview 替换 iframe 认证桥）

日期：2026-09-13 · 状态：已与产品负责人（用户）确认架构方向 · 分支：`fix/workbench-webview-not-loading`

## 1. 背景与根因

**现象**：启动 DeepSeek Harness Desktop 后，工作台 tab 无法显示 dsh web 页面（历史表现：401「dsh web authentication required」；更早版本曾卡「正在启动」、刷新误跳系统浏览器）。

**根因**（2026-09-13 依据 deepseek-harness master 源码核实）：

1. dsh web 自 0.1.2-alpha.5 起强制浏览器会话认证（`packages/client/connection/src/browser-auth.ts`）：
   - `GET /?token=…` → 303 → `Set-Cookie: dsh-auth-<sha256(host:port)>; HttpOnly; SameSite=Strict; Max-Age=30天`（HMAC 签名、绑定 authority=host:port，换端口即失效）；
   - 无任何配置可关闭认证（`cookieMaxAgeDays` 最小 1；`--trusted-host` 只放行 Host fence 403 层，cookie 401 层仍必须）。
2. 现有壳页在 `tauri://localhost`，工作台用 `<iframe src="http://127.0.0.1:<port>?token=…">` 内嵌——**跨站嵌套上下文**：
   - `SameSite=Strict` cookie 不随跨站 iframe 请求发送；
   - WKWebView/WebView2 丢弃跨站响应中的 `Set-Cookie`；
   - → iframe 永远 401。
3. 现有认证桥（`webview_auth.rs`：预认证 + 改写 SameSite=None+Secure 注入 cookie store）依赖 dsh 认证协议每个细节（token 格式、303 行为、Set-Cookie 属性）与 WebKit/Chromium 对 loopback 注入的特殊豁免——dsh 每升一版认证细节变化即失效（已多轮修复仍未根治），平台差异大（macOS/Windows 两套注入实现）。
4. 官方桌面方案（Electron + `dsh-app://` 协议 + IPC）**完全不开 HTTP 端口**，印证「HTTP + 认证 + iframe」组合不是可维护的嵌入通道。

**结论**：iframe + 认证桥是错误架构通道；修复方向是**换通道**——让 dsh 页面成为 WebView 的**顶层文档（first-party）**，认证全部由 WKWebView/WebView2 原生网络栈处理。

## 2. 目标与非目标

### 目标
- 启动后工作台**必然**显示 dsh web（macOS + Windows 一致）。
- **零依赖 dsh 行为细节**：dsh 升级（换 token 格式、cookie 属性、认证机制）均自动跟随，与普通浏览器打开一致。
- 保持「单窗口 + 顶栏 tab + 内嵌工作台」的产品体验（thin shell 定位不变）。
- 删除认证桥及其依赖，代码净减。

### 非目标
- 不做 dsh 本体功能（profile/agent/模型配置）——那是 dsh 自己的事。
- 不引入前端框架/构建链（壳页保持原生 HTML/CSS/JS）。
- 不改 dsh CLI 参数与部署方式（仍 `--profile web --port 0`）。
- 不实现「免认证模式」（dsh 无此功能，实测代码上不可能）。
- 不跟随官方 Electron 架构（与 thin-shell 定位冲突，投入过大）。

## 3. 架构

### 3.1 核心机制

- 主窗（shell.html：顶栏 + 4 tab + 管理面板）保持不变，仍是 `tauri://localhost`。
- 工作台区域由**同窗口内的 child webview**（label = `workbench`）承载，通过 Tauri `window.add_child(WebviewBuilder::new("workbench", WebviewUrl::External(url)), pos, size)` 创建（tauri `unstable` feature；底层 wry `build_as_child`：macOS = NSView 子视图，Windows = WebView2 子窗口）。
- child webview 的**顶层文档即 `http://127.0.0.1:<port>/?token=…`** → first-party：token→303→Set-Cookie→后续页面/API/WS 请求全部携带 cookie，由 WebView 原生完成。壳不解析 token、不注入 cookie、不干预任何认证细节。
- child webview **常驻**：切走时 `hide()`、切回时 `show()`，保留工作台会话状态（与现状 iframe 常驻行为一致）。
- bounds = 工作区区域（顶栏下方，`topbar-h` 联动：46px 常态 / 0px 折叠态）；窗口 `Resized` 时同步 `set_bounds`。

### 3.2 生命周期与数据流

```
启动 → 主窗 shell.html（占位层可见）
  → spawn_dsh（参数不变）→ stdout 解析 `http://127.0.0.1:<port>/?token=…`
  → 主线程：create child webview（navigate token URL）
  → WebView 原生认证 → index 渲染
  → on_page_load → emit("workbench:ready") → 壳页撤占位
  → dsh 崩溃 → boot 循环（新端口）→ child webview navigate 新 URL
  → tab 切换：show_workbench / hide_workbench command
  → brand 点击刷新：workbench_webview.reload()
```

## 4. 组件与接口变更

| 文件 | 变更 |
|---|---|
| `apps/desktop/src-tauri/Cargo.toml` | tauri 增加 `unstable` feature；**删除** objc2、objc2-foundation、webview2-com（认证桥依赖）；如需同步调整 windows-core/trash 等锁版本 |
| `apps/desktop/src-tauri/src/webview_auth.rs` | **整文件删除** |
| `apps/desktop/src-tauri/src/main.rs` | 删除 `webview_auth` 模块引用与工作区未提交调试改动；新增 `workbench` webview 管理（create/show/hide/reload/navigate/set_bounds、窗口 Resized 联动）；boot 就绪分支改为创建/导航 child webview；崩溃自愈衔接 navigate；新增 command：`show_workbench` / `hide_workbench` / `workbench_reload`；`reveal_main_window` 不再 emit `dsh:url`（或保留兼容但壳页不再依赖） |
| `apps/desktop/ui/shell.html` | **删除** `<iframe id="workbenchFrame">` 与占位层（占位改为主窗 shell 自身元素，由 `workbench:ready` 事件驱动显隐）；工作台区域留空容器（透明，child webview 叠于其上） |
| `apps/desktop/ui/shell.js` | 删除 `loadWorkbench`/轮询 `get_dsh_url`/iframe load 监听；tab 切换工作台 → `invoke('show_workbench'|'hide_workbench')`；监听 `workbench:ready` 撤占位；brand 点击 → `invoke('workbench_reload')`；`open_workbench_url_cmd` 语义保留（双击 tab 浏览器打开） |
| `apps/desktop/src-tauri/tauri.conf.json` | CSP 收紧：删除 `frame-src`（不再有 iframe）；`connect-src` 稳妥保留；评估 `macOSPrivateApi` 是否仍需（认证桥删除后不再用 `with_webview`） |

**接口契约（Rust ↔ shell.js）**：
- `show_workbench()` / `hide_workbench()` — 无参，幂等。
- `workbench_reload()` — 无参；child webview 不存在时无害忽略。
- 事件 `workbench:ready` — child webview 首次 `on_page_load` 后 emit，壳页撤占位。
- 事件 `dsh:url` — 改为内部流转（不再依赖壳页监听；shell.js 已实测该事件在本环境丢失）。

**导航策略**：child webview 复用 `webview_navigation_policy`（127.0.0.1 当前 dsh 端口放行；外部 http(s) 转系统浏览器；data:/blob:/about: 放行）。child webview **不注入 `__TAURI__`**（安全面不扩大）。

## 5. 错误处理与兜底

| 场景 | 行为 |
|---|---|
| dsh 崩溃/重启 | boot 循环衔接：child webview `navigate(新 URL)`，无需重建；5 次连崩停止并弹日志提示（现状逻辑保留） |
| `add_child` 失败 / 平台异常 | **兜底路径 C**：改独立 `WebviewWindow`（`WebviewUrl::External`），同一组 command 接口，仅创建处不同；判定后写死降级开关，避免每启都试 B |
| 导航到外部链接 | 拦截转系统浏览器（策略不变） |
| child webview 未就绪时用户切工作台 | 显示占位层（「正在启动 dsh 工作台…」）；`workbench:ready` 后撤除 |
| 首次引导安装 | 安装流程不变（setup 视图）；成功后 boot → 创建 child webview → 自动进入 |

## 6. 平台注意

- **macOS**：wry `build_as_child` = NSView 子视图。resize 联动以 Rust 侧 `on_window_event(Resized)` + `set_bounds` 为准（不依赖平台自动布局）。
- **Windows**：WebView2 子窗口；`no_console` 等现有进程处理不变。
- tauri `unstable` feature 为实验性 API：锁定当前 tauri 2.11.5；升级 tauri 时需回归验证 `add_child`。
- 若 `unstable` API 在任一平台不可用/不稳定：整体切路径 C（纯 stable API）。

## 7. 验证

- **Rust 单测**：URL/端口判断（`dsh_url_port` 回归）、bounds 计算（常态/折叠顶栏）、状态机（未就绪→就绪→崩溃→新 URL）。
- **手动回归矩阵**（macOS + Windows 各一轮）：
  1. 启动即显示工作台（无 401、无白屏）。
  2. tab 来回切换：工作台状态保留；管理面板正常。
  3. brand 点击刷新工作台。
  4. 崩溃自愈：kill dsh 进程 → 自动重启 → 工作台换端口恢复。
  5. 外部链接点击 → 系统浏览器打开，工作台不受影响。
  6. 窗口缩放/折叠顶栏 → child webview bounds 跟随。
  7. 首次引导：安装 dsh → 自动进入工作台。
  8. 托盘/单实例行为不回归。
- **CI**：编译 + clippy 双平台通过。

## 8. 删除清单

- `webview_auth.rs` 全文件。
- Cargo.toml 依赖：`objc2 = "=0.6.4"`、`objc2-foundation = "=0.3.2"`、`webview2-com = "=0.38.2"`（及仅被认证桥引用的其它依赖，如块相关 crate）。
- 工作区未提交的认证桥调试改动（ureq 303 分支、NSNumber Secure、同步 completion、shell.js payload 兼容）——随本方案作废。
- shell.html 的 `<iframe>` 与占位层 DOM；shell.js 的 iframe 相关函数。

## 9. 附件：UI 调整（另行 od 设计，用户确认后并入实现）

- 工作台区域视觉：移除 iframe 后空白容器与 child webview 无缝衔接；占位层/启动态视觉与「紫粉蓝光斑玻璃拟态」基调一致。
- 壳页整体设计哲学审视（顶栏/tab/管理面板与品牌一致性）——由 OpenDesign 出视觉稿，落地为 `theme.css`，用户确认后随本方案一起实现或独立提交。