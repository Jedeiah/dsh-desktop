# Code Review — P1 居中/无边框弹窗、P2 共享主题+自绘弹窗、P4 NSIS 完整卸载

- 审查对象：`apps/desktop` 的 main.rs / tauri.conf.json / installer-hooks.nsh / ui/*（theme.css、modal.html/js，及四个页面）
- 基线：git HEAD `34cc8f1`；未修改任何源文件（本报告仅交付产物）
- 验证方式：`cargo check` / `cargo clippy`（均在本机 macOS 通过）；tauri-bundler 2.9.4 的 `installer.nsi` 源码逐行核对 hook 作用域与执行顺序
- 结论见文首，随后为 [必须修复]、[建议改进]、[已确认OK]

---

## 结论：Ship-with-minor-fixes

整体设计清晰、自洽：弹窗替代 rfd 语义等价、卸载链路（App 内 → $INSTDIR\\uninstall.exe → NSIS PREUNINSTALL → sidecar）闭环且顺序正确，`$INSTDIR` 程序文件不被 sidecar 删除，NSIS hook 在删文件前执行且失败不阻断——这些关键点均已核实无误。未引入任何新的 clippy warning。

但存在 **1 个并发正确性缺陷（内容/结果错配）**、**1 个 Alt+F4/系统关窗导致 24h 阻塞的漏网**、**1 个极低概率的 `clamp` panic 边界**，建议修复后再作为正式版发布；其中并发问题若可接受（罕见路径）也可降级为建议并记录。

---

## 必须修复

### 1. show_modal 并发：内容与结果错配 + 旧发送端被静默丢弃
`apps/desktop/src-tauri/src/main.rs:1460-1473`（重点 1461、1404-1407）

```rust
fn show_modal(app: &AppHandle, ...) -> bool {
    let (tx, rx) = mpsc::channel();
    *mlock(&MODAL_RESULT) = Some(tx);        // ← 覆盖上一个未完成的 tx（旧发送端被 Drop）
    *mlock(&MODAL_SPEC) = Some(ModalSpec { ... });   // ← 覆盖内容
    ...
}
```

`open_modal_window()` 的复用分支（main.rs:1404-1407）在弹窗**已存在**时只 `show()`+`set_focus()`，**不会重新加载页面、更不会让 JS 再次调 `modal_spec`**。因此若两次 `show_modal` 并发（例如托盘「检查更新」线程 vs `boot` 崩溃线程同时触发）：
1. 窗口仍显示**上一次** spec 的标题/文案；
2. `MODAL_RESULT` 已换成**新** tx；
3. 用户点击「确定」，`modal_respond`（main.rs:1449）把结果发给**新**线程，而**旧**线程的 `rx` 永远收不到，阻塞到 24h 超时（main.rs:1490）返回 false；
4. **用户看到的是旧对话框内容，结果却被新线程消费**——可能把「确认启动失败/崩溃」误判成「现在更新」，或反之。

修复建议（任选其一，推荐前者）：
- 给 `show_modal` 加一个全局 `static MODAL_LOCK: Mutex<()>`，进入 `show_modal` 时上锁，使两次调用串行化；锁在 `rx.recv_timeout` 期间持有，天然消除错配。
- 或在 `open_modal_window` 复用分支里强制 `w.navigate(App(modal.html))` 重新渲染（并让 JS 每次加载重取 `modal_spec`），同时在写 `MODAL_RESULT` 前把旧发送端安全收尾。

### 2. 弹窗被 Alt+F4 / 系统关窗关闭时，show_modal 会阻塞到 24h 超时
`apps/desktop/ui/modal.js`（无关闭请求处理）与 `apps/desktop/src-tauri/src/main.rs:1489-1490`

无系统标题栏后，Windows 上仍可通过 **Alt+F4** 触发 WM_CLOSE 关掉弹窗；或主窗口被 rm（进程外）销毁。此时 `modal.js` 的 `responded`/`respond()` 从不触发，`modal_respond` 不被调用，`MODAL_RESULT` 的 tx 永远空置，`show_modal` 阻塞 24h（`recv_timeout` 兜底）才返回 false。相比旧 rfd 对话框（Esc/关闭立即返回），这是一个真实的行为回退：启动失败/连续崩溃场景下 `boot` 线程会被挂起近一天，给用户的观感是「程序卡死」。

修复建议：在 `modal.js` 顶部监听窗口关闭请求并回传 false：
```js
window.__TAURI__.window.getCurrentWindow()
  .onCloseRequested(() => respond(spec && spec.kind === 'yesno' ? false : false));
```
（对 ok 型回传值无所谓；对 yesno 型回传 false=取消）。Rust 侧同样可在 `modal_respond` 之外，用 `on_window_event(CloseRequested)` 把尚未 take 的 sender `send(false)`。

### 3. `center_child_on_main`：对话框大于工作区时 `clamp` 会 panic
`apps/desktop/src-tauri/src/main.rs:1392-1393`

```rust
let x = tx.clamp(wx, wx + ww - dw);   // 当 dw > ww 时 max < min → i32::clamp 会 panic
```
`i32::clamp` 在 `min > max` 时 **panic**（不是饱和），而 `center_child_on_main` 在主线程执行（`open_modal_window` 内）。当前对话框最大 560×480（插件管理）×1.0 缩放、工作区普遍 > 该宽度，命中概率极低；但在极小虚拟分辨率/高分屏组合（例如 500px 宽 work-area）下会直接 panic 崩主线程。建议改为饱和 clamp：
```rust
let max_x = (wx + ww - dw).max(wx);   // 防 max < min
let x = tx.clamp(wx, max_x);
```

---

## 建议改进（非阻塞）

- **S1. NSIS 硬编码二进制名**（`installer-hooks.nsh:25`）：用 `"$INSTDIR\dsh-desktop.exe"`。已核实 tauri-bundler 的 `installer.nsi:782,786` 在同一个 `Section Uninstall` 里使用 `${MAINBINARYNAME}`，且 hook 在 `!include`（`installer.nsi:34-35`）之后、`Section Uninstall` 展开时才 `!insertmacro`，**`${MAINBINARYNAME}` 在 hook 作用域内可用**。建议改为 `'$INSTDIR\${MAINBINARYNAME}.exe'`，避免未来改名（`mainBinaryName`/Cargo bin 名）后静默失效。
- **S2. `dsH_SKIP_SELF_UNINSTALL` 守卫是死分支**（`installer-hooks.nsh:21`）：该符号从未在构建中定义，`!ifndef` 恒真。作为逃生舱保留没问题，但建议在注释里说明如何注入（或去掉）。
- **S3. show_modal 的 2s 轮询窗口**（main.rs:1475-1488）：若主线程忙导致窗口创建 >2s，`opened` 仍 false 会走清理分支返回 false，随后主线程又建出悬浮弹窗（无发送端）。建议把轮询上限提高到 5-10s，或改为在主线程闭包内用回调/事件确认创建完成，而非轮询。
- **S4. MODAL_SPEC / MODAL_RESULT 残留**：`modal_respond` 只 `take()` 了 `MODAL_RESULT`，未清理 `MODAL_SPEC`（main.rs:1449-1451）。单个弹窗流程无影响（下次被覆盖），但配合 S3/问题2 的异常关闭会残留旧 spec。可在 respond 里一并 `MODAL_SPEC = None`。
- **S5. `kill_other_app_instances` 静默吞错**（main.rs:2519-2527）：`let _ = ...status()` 丢弃 PowerShell 退出码；若 CIM/任务失败（WMI 不可用等），文件锁未释放、NSIS 删文件失败，却没有日志线索。建议至少 `logln!` 退出状态（sidecar 无 GUI）。
- **S6. 固定无边框窗口不可拖动**（plugins/lan/uninstall 均 `decorations(false)` 且未加 `data-tauri-drag-region`）：与计划 B2（拖拽区）一致属已知取舍，但建议至少在标题区加 `data-tauri-drag-region` 让用户能移动，否则窗口被主窗口遮挡区域性操作不便。
- **S7. `show_window` 每次显示 `.center()`**（main.rs:746 / 756）：主窗不再记忆用户上次位置，每次启动/显示都回中。属于计划的既定行为，但建议确认产品意图（若用户拖到副屏，主窗重开会跳回主屏中心）。
- **S8. clippy 既有告警未清理**：8 条 warning 全部位于**未被本次 diff 触及**的既有代码（main.rs:182、256、368、1652、1681、1698、1809，update.rs:76），本次改动 **0 新增**。可选清理 `as u128`/`as u64`、`manual_flatten`、`manual_map` 等，与本次无关。

---

## 已确认OK

1. **rfd → 自绘弹窗语义等价**：启动失败（main.rs:651）、连续崩溃（main.rs:706）、更新确认（main.rs:1622）三处全部改走 `show_modal`；`"ok"` 忽略返回值、`"yesno"` 返回用户是否确定；✕/Esc 在 yesno 回传 false、ok 回传 true（无副作用）。未丢失任何"更新确认阻塞"语义。
2. **CSP 合规**（tauri.conf.json `script-src 'self'; style-src 'self' 'unsafe-inline'`）：modal.html 外部引 theme.css/`<script src="modal.js">`，无内联事件处理器（全部 addEventListener），与 lan/plugins/uninstall 既有 `window.__TAURI__.core.invoke` 用法一致；`withGlobalTauri:true` 注入不违反 `script-src 'self'`。✅
3. **`center_child_on_main` 数学**（main.rs:1371-1399）：logical×scale_factor 换算物理尺寸/坐标正确；`current_monitor()` 取主窗所在显示器工作区进行 clamp（多显示器方向正确）；复用分支不重复居中、`visible(false)`+定位后 `show()` 避免闪烁。仅 #3（clamp panic）为边界问题。
4. **自绘弹窗响应**（modal.js）：`responded` 二重守卫防双击双响应；`modal_respond` 先 send 再 `window.close()`，JS 端 120ms `window.close()` 仅为兜底；`getCurrentWindow` 语义下 Rust 侧关闭可靠（弹窗不依赖裸 `window.close()`，故不受"脚本未打开窗口无法 close"限制）。
5. **destroy WebView 再删数据**（uninstall_run main.rs:1547-1552）：先销毁全部 WebView 释放 WebView2 数据占用，再 teardown，顺序正确。
6. **uninstall_teardown 不删程序文件**（main.rs:2382-2425）：`dirs` 仅含 `p.app_data` + macOS/Windows 缓存与 WebView2 数据，**不含 $INSTDIR/资源目录**；`--wipe` 才删 `~/.dsh`。符合「NSIS 负责删 $INSTDIR」的设计。
7. **`kill_other_app_instances` 正确性**（main.rs:2503-2527）：
   - 自进程排除：`$self` 插值成 `std::process::id()`，`$_.ProcessId -ne $self` 排除自身（sidecar 不自杀）。验证 `CmdLine` 正则与实际 spawn 一致：dsh node 为 `node <...>bin.js --profile web --port 0`（main.rs:615-619）→ `bin\.js.*--profile web` ✅；lan-proxy.js / mdns-advertise.js 直接出现在命令行（main.rs:1833,2153）✅。
   - 误杀评估：仅匹配 `dsh-desktop.exe` 及三个项目专属 node 脚本特征，用户无关 node（如含 `bin.js` 但无 `--profile web`）不会被命中，风险可控。
   - `2>$null` 在 PowerShell `-Command` 内为合法重定向；`no_console`（CREATE_NO_WINDOW，main.rs:791-795）已应用于 powershell 与 uninstall.exe spawn，全程无控制台闪现。
8. **`--self-uninstall-full` 干净退出**（main.rs:2582-2610）：成功 `println!("UNINSTALL_DONE"); exit(0)`，失败 `exit(1)`；sidecar 返回后 NSIS 继续删文件不阻断。`paths_from_cli()`（main.rs:288-308）在 NSIS 上下文中 `current_exe`= `$INSTDIR\dsh-desktop.exe`，`resources` 落到 `$INSTDIR\resources`，且本流程不删除资源目录，正确。
9. **installer-hooks.nsh 作用域/顺序**（对照 tauri-bundler 2.9.4 `installer.nsi`）：
   - `NSIS_HOOK_PREUNINSTALL` 在 `Section Uninstall` 顶部（installer.nsi:778-780），**先于** `CheckIfAppIsRunning`(:782) 与 `Delete "$INSTDIR\${MAINBINARYNAME}.exe"`(:786)——sidecar 此时仍可执行，删除前生效 ✅。
   - `ExecWait` 同步等待 sidecar 退出（`uninstaller.is_file()` 分支在 main.rs:1582-1588），退出后才删除主程序文件，自身句柄已释放 ✅。
   - `POSTUNINSTALL` 幂等删自启注册表项（installer-hooks.nsh:33），且 hook 名与 bundler `installer.nsi:778,886` 匹配 ✅。
10. **App 内卸载链**（uninstall_run Windows 分支 main.rs:1573-1595）：teardown 数据（擦除 wipe 在 App 侧已执行）→ `no_console` 拉起 `$INSTDIR\uninstall.exe` → `app.exit(0)`（不等待子进程，`app.exit` 不杀已 spawn 的独立进程）→ 未 spawn 时的系统卸载通知兜底。闭环无割裂。
11. **theme.css 跨 WebView**：`mask-composite` 同时给 `-webkit-mask-composite`（WKWebView 用）与 `mask-composite`（WebView2 用）双声明；`backdrop-filter` 及 `-webkit-backdrop-filter` 双声明；`prefers-reduced-motion` 保留。对比度：`--text-faint`(#6a) 仅用于 close-x/弱化文字，主文本/按钮全白，充分。
12. **Esc/✕ 与危险页面**：uninstall.html 仅 ✕=取消、**未加 Esc**（符合计划「危险操作禁 Esc」）；lan/plugins/uninstall 的 ✕ 与 Esc 均走既有 `window.close()`，与既有 `cancel` 按钮同机制，行为一致。
13. **回归面**：tray menu 未触及（diff 无 tray 相关 hunk）；`rfd` 仍被 `choose_workspace` 的 FileDialog 使用（main.rs:899），依赖未死；`docs/` 未在 tauri.conf.json/build.rs 被引用，非构建必需（仅文档/本报告）。
14. **编译与静态检查**：`cargo check --offline` 通过；`cargo clippy` 8 条 warning 全为既有代码，**本次改动 0 新增**。
