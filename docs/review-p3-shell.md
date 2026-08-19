# P3 管理台整合（壳页 + iframe）代码评审

- 评审对象：`git diff 21bed03 7734c5b`（P3 Shell 页），侧重 `main.rs`、`tauri.conf.json`、`shell.html`、`shell.js`、`README.md`、`docs/plan-*.md`。
- 范围：不重评 21bed03（P1/P2/P4），但核验了 P3 是否回归了共用文件里的既有行为。
- 方法：读取源码 + 对照 tauri-2.11.5 / tauri-runtime-wry-2.11.4 / wry-0.55.1 源码核实线程/IPC 语义；`cargo build` / `cargo test` / `cargo clippy`，并与 21bed03 基准（临时 worktree）对比告警。
- 结论：**Ship-with-minor-fixes（需 4 项必改）**。

---

## [必须修复]

### 1. `check_update_cmd` 会把主线程卡死 → 「更新」流程彻底失效
- `apps/desktop/src-tauri/src/main.rs:1523-1547`（`check_update_cmd`，同步 `#[tauri::command]`）→ 内部 `main.rs:1529` 调 `show_modal(...)`。
- 依据（已核实）：
  - 同步 command 在**主线程**执行：wry wkwebview 的 IPC 委托标记 `#[thread_kind = MainThreadOnly]`（wry-0.55.1 `src/wkwebview/class/wry_web_view_delegate.rs:26`），并在 `didReceiveScriptMessage` 里直接调用 `ipc_handler(r)`（同文件 :61），也就是 `handle_ipc_message` → 同步命令本体。
  - `show_modal` 自身文档 `main.rs:1377-1378` 明确要求「**在后台线程调用**（boot 线程 / 更新检查线程）」，其内部 `main.rs:1395-1397` 用 `run_on_main_thread(open_modal_window)` 打开弹窗并 `rx.recv_timeout(24h)` 阻塞。
  - 当 `check_update_cmd` 在主线程里阻塞等待用户点击时，主事件循环被占住，队列里的 `run_on_main_thread(open_modal_window)` 永远排不上；`show_modal` 的 `opened` 循环（`main.rs:1399-1407`，约 10s）超时后返回 `false`。
- 后果：**确认弹窗永远不会弹出、GUI 冻结约 10 秒、命令返回 `"cancelled"`、更新实际无法安装**。原 `check_for_updates` 用 `std::thread::spawn` 包一层正是为绕开此问题；`plugin_op` 也用 `async fn` + `spawn_blocking`（`main.rs:1210,1228`）规避主线程占用——P3 把这条路径改成了同步主线程调用，属回归。
- 修复：把 `check_update_cmd` 改为 `async fn`，将「检查更新 + `show_modal`」整体放进 `tauri::async_runtime::spawn_blocking(...)`（与已删除的 `check_for_updates` 等价），`apply_update` 再放到 `std::thread::spawn` 里异步执行。

### 2. 插件「实时滚动」进度流是死的：`dsh:plugin-output` 仍只发给已不存在的 `plugins` 窗口
- `apps/desktop/src-tauri/src/main.rs:1155` 与 `:1170`：`run_dsh_plugin` 逐行 `emit("dsh:plugin-output", &l)` 时目标仍是 `PLUGIN_LABEL == "plugins"`。
- P3 后不再创建 PLUGIN_LABEL 窗口（`grep` 仅 `WINDOW_LABEL` 被 `builder` 创建），`get_webview_window("plugins")` **恒为 `None`**，emit 到不了任何地方；壳页插件 Tab 的监听（`shell.js:210-213`）永远收不到实时行。命令返回后 `shell.js:226` 才用一次性 `text` 覆盖输出。
- 后果：pnpm 安装/卸载期间只有「正在安装…」静态文案，**「输出区实时滚动」宣告失效**（README「实时滚动」、`shell.html` 的期望均不符）。功能未完全丢失（兜底有最终全文），但实时性是带货特性，属 P3 回归。
- 修复：`main.rs:1155、1170` 改为向 `WINDOW_LABEL`（壳页）emit（可保留对 `PLUGIN_LABEL` 的兼容 emit，但主目标必须是 `WINDOW_LABEL`）。

### 3. `choose_workspace_cmd` 不再重启 dsh，与界面文案自相矛盾
- `apps/desktop/src-tauri/src/main.rs:1460-1475`：新 command 只 `save_settings_at`，**没有**重启 dsh；旧托盘 `choose_workspace` 是「保存 + `restart_dsh(app)`」。
- `apps/desktop/ui/shell.html:191` 明确写着「dsh 进程的默认工作目录；**改后自动重启工作台**」——现在不重启，新工作目录要等手动点「重启工作台」才生效，文案与行为不符。
- 这是行为变更（与旧托盘项语义不一致），请二选一：让 `choose_workspace_cmd` 返回后由 shell.js 调 `restart_dsh_cmd`（或 command 内重启），或修正 shell.html 文案为「改后需手动重启生效」。建议前者以保持 README/文案承诺。

### 4. `show_window` 的重建兜底用了 shell.js 不监听的裸 `CustomEvent`
- `apps/desktop/src-tauri/src/main.rs:757-758`：新建壳页窗口后注入 `window.dispatchEvent(new CustomEvent('dsh:url', {detail:'{url}'}))`。
- 壳页接收 URL 用的是 `T.event.listen('dsh:url', cb)`（`shell.js:77`）——那是 **Tauri 事件总线**（由 `w.emit` 注入的 JS 触发），不是 `window` 上的原生 `CustomEvent`；裸 `dispatchEvent(CustomEvent)` 永远不会触发 `listen()` 的回调。因此这段兜底**实际上无效**。
- 影响的路径仅在「主窗不存在时重建」——极其罕见，且 shell.js 初始化时自带 `get_dsh_url()` 兜底（`shell.js:69`）一般已能拉起 iframe，所以不被日常触达，属潜在缺陷而非崩溃。另：`{url}` 未做 JS 字符串转义，若 URL 含 `'` 会注入/破坏（当前 dsh URL 不含，故低危）。
- 修复：改为主窗就绪后再次 `w.emit("dsh:url", url)`（或延后到 on_page_load 触发），不要手搓 CustomEvent 字符串。

---

## [建议改进]

- **rfd 目录选择器在主线程阻塞**（`choose_workspace_cmd`，`main.rs:1466`）：同步命令在主线程跑原生 `pick_folder()`，弹窗期间 Tauri 主循环被占、`run_on_main_thread` 排队任务停摆（可暂时冻结 UI / 延迟 dsh:url）。与旧托盘行为一致（也曾在主线程调 rfd），不算回归；建议与 `plugin_op` 保持一致用 `async fn` + `spawn_blocking`。
- **`get_dsh_url` 初始化兜底的极小竞态**：`shell.js:69` 先 `get_dsh_url()`、随后 `shell.js:77` 才 `listen('dsh:url')`；若 dsh 恰在这两步之间就绪并向壳页 emit，监听可能错过。正常首启 dsh 需数秒、shell.js 毫秒级注册完，实际不可达；可在监听注册后再补一次 `get_dsh_url()`，属保险性加固。
- **iframe 内快捷键不可达**：`⌘K`/`Esc` 监听在壳页主窗口 `window`（`shell.js:35`），用户焦点落在 dsh iframe 时按键进 iframe 文档、到不了壳页。`⌘K` 只在管理 Tab 聚焦时可用（工作台 iframe 内不响应）——与 plan 的 Spike §C「iframe 键盘焦点」残留风险一致，建议真机确认或加提示。
- **`window.__openShellTab = selectTab`（`shell.js:297`）是死代码**：main.rs/托盘已改用 `shell:tab` 事件（`main.rs:2702`），无任何消费者；无害，可保留或删除。
- README 的「⌘K 在工作台与管理 Tab 间循环切换」仅在管理 Tab 可用（见上），文案可补充 iframe 限制说明。

---

## [已确认OK]

- **CSP（`tauri.conf.json`）**：新增 `frame-src 'self' http://127.0.0.1:* http://localhost:*` 正确放开壳页 iframe 的 `http://127.0.0.1:<port>` 加载；iframe 自身文档与其 `ws://127.0.0.1` 受 dsh 自带（spike 报告：无）响应头约束，**不受父 CSP 的 connect-src 管**，故父 CSP 只 gate frame-src 是正确语义；壳页自己的 `invoke`（IPC 自绘协议，非 connect-src 管辖）与样式/图片（`style-src 'self' 'unsafe-inline'`、`img-src 'self' data:`，含 QR `data:` 图）均无缺口。壳页 `connect-src` 里新增的 `ws://localhost:* http://localhost:*` 对壳页自身无实际消费者，属无害冗余。
- **安全隔离（iframe 拿不到 IPC）**：核实 tauri-2.11.5 `manager/webview.rs` 中 `__TAURI_INTERNALS__`/`__TAURI__`（withGlobalTauri）全部经 `main_frame_script(..., for_main_frame_only: true)` 注入（:159-219）；dsh iframe 无 `__TAURI__`/`__TAURI_INTERNALS__`。且 iframe 源为 `http://127.0.0.1:<port>`、壳页为 `tauri://localhost`，**跨源**，iframe 无法经 `window.parent.__TAURI__` 或 `window.__openShellTab` 触达父全局（同源策略报 SecurityError）。shell.js 无 postMessage 桥。`__openShellTab` 即便被触达也仅切 CSS 显示，非 IPC 面。
- **show_window / dsh:url 时序**：dsh 就绪→`boot` 在主线程 `show_window`→`w.emit("dsh:url")`（`main.rs:739`，顺序在 `show()`/`set_focus()` 之前，隐藏时监听仍在）；`center()`-on-every-show 保留（`main.rs:740`）。shell.js 初始化 `get_dsh_url()` 兜底 + `listen('dsh:url')` 覆盖大部分漏发场景。托盘左键显示主窗、`shell:tab` 切「常规」均正常。
- **`get_shell_state` / `active_closure`**：`active_closure` 返回 `Option`（`main.rs:565-567`），`get_shell_state` 内 `.and_then(|dir| closure_version(&dir)).unwrap_or("未知")`（`main.rs:1451`）——dev/CLI/无闭包上下文不 panic，返回「未知」。
- **`set_login_cmd` / LOGIN_ON**：托盘勾选项已删，`LOGIN_ITEM` 恒为默认 None，`item.set_checked`（`main.rs:1514`）是无害空操作；`LOGIN_ON` 现已是只写状态（无 reader），`get_shell_state` 直接读磁盘态 `login_item_enabled()`（`main.rs:1449`），壳页初始开关正确。P3 移除 setup 里 `LOGIN_ON.store(login_item_enabled())` 无影响。
- **回归（P1/P2/P4 共存文件）**：`uninstall_run` 销毁循环（`main.rs:1610-1614`）遍历 PLUGIN/LAN/UNINSTALL 标签，这些窗口已不再创建 → 各自 `None` 空操作，**无害**；销毁含壳页主窗 `WINDOW_LABEL` + `app.exit(0)` 是预期（async command，销毁在运行时线程，失败路径用 `update::notify` 兜底，与既有设计一致）。自绘 modal 在 boot/崩溃后台线程调用 `show_modal`（`main.rs:654,709`）线程语义正确，未受影响；`periodic_check` 仍在后台线程。托盘已收敛三项且均走 `show_window`。
- **构建/告警**：`cargo build` ✓、`cargo test` ✓（9 passed / 0 failed）、`cargo clippy` ✓。**P3 新增告警 0 条**：当前 8 处 clippy 告警 = main.rs:185/259/371/1685/1714/1731/1842/1843 + update.rs:76；与 21bed03 基准（main.rs:185/259/371/1676/1705/1722/1833/1834 + update.rs:76，共 9 条）逐条按 +9 行号偏移一一对应，无任何新告警。

---

### 说明
除新增本报告 `docs/review-p3-shell.md` 外，**未修改任何源码/配置文件**；仅临时创建并已删除 git worktree（/tmp/p3_base，供 21bed03 clippy 对比），未做任何提交。
