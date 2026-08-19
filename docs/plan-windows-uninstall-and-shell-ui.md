# DSh Desktop：Windows 卸载 / App 壳与弹框 / 居中 / 功能上移 —— 审查与设计方案

> 状态：**实施中（P1-P4 均已编码，P1/P2/P4 已评审通过，P3 壳页待真机回归）**
> 版本基准：`main` @ `34cc8f1`（v0.1.8 之后），dsh `0.1.0-rc.7`
> 日期：2026-08-19

---

## 0.5 实施进展与关键调整（2026-08-19 追加）

- **P1（居中与弹窗形态）+ P2（设计统一 + 标题栏）已编码完成**：全窗口 `.center()`；主窗每次显示居中；lan/plugins/uninstall/自绘弹窗全部 `decorations(false)+resizable(false)+固定尺寸`，并新增 `center_child_on_main`（相对主窗口中心物理定位 + clamp 到工作区，多显示器正确）；弹窗 ✕ 关闭 + Esc 关闭（uninstall 属危险操作，仅 ✕=取消、禁 Esc）；rfd 三个系统对话框（启动失败/连续崩溃/更新确认）已替换为自绘 `modal.html`/`modal.js`（玻璃卡片、居中、可按需 ok/yesno）；新建 `theme.css` 共享设计 token，六个页面统一过一遍（Segoe UI 字体栈、光斑收敛、prefers-reduced-motion 保留）。
- **B1 标题栏落地方式调整**：原方案「macOS overlay + Windows 深色原生」。落地时发现主窗口大段时间承载**外部 dsh 工作台页面**（非本项目 HTML）——overlay 会把交通灯压在 dsh 自己的顶部工具栏上、且无 `data-tauri-drag-region` 可注入外部页面，拖拽与可读性会受损。故 B1 两平台统一用**原生暗色标题栏**（主窗 builder `.theme(Dark)`），同样消除「亮标题栏 vs 暗内容」违和、不损失任何系统窗口能力，但零回归风险；macOS overlay 的完整自绘（B2）仍留作二期对比项。
- **P3 架构前提已完成（2026-08-18 半天 spike）**：`docs/spike-iframe-report.md` 结论 **路径 A（壳页 + iframe）有条件可行**——dsh 无 `X-Frame-Options`/CSP `frame-ancestors`，无 frame-breaking JS，WebSocket 流式同源可用；剩余为 iframe 语义级验证（键盘焦点/剪贴板/嵌套弹层/新开窗口），P3 时按报告 §C 做真实桌面回归。
- **P4（NSIS 完整卸载）已编码**：新增 `--self-uninstall-full [--wipe]` CLI sidecar（Windows：先按进程名/命令行特征树杀其它运行实例与 node/dsh/lan/mdns 子进程释放文件锁 → 复用 `uninstall_teardown` 清用户数据/WebView2/登录自启/可选 ~/.dsh，全程无窗口，失败不阻断 NSIS）；新增 `installer-hooks.nsh`（`NSIS_HOOK_PREUNINSTALL` 调 sidecar、`POSTUNINSTALL` 删自启注册表项）；`tauri.conf.json` 接入 `bundle.windows.nsis.installerHooks`；App 内卸载 Windows 分支改为「teardown 数据 → 唤起 `$INSTDIR\uninstall.exe` → 退出」，删除割裂的"请到设置里卸载"兜底。macOS 分支保持 trash_self。此部分逻辑在 macOS 本地只能编译期验证，完整验收需 Windows 实机（见 §6）。**P4 已随 P1/P2 一并评审（docs/review-p1p2p4.md，Ship-with-minor-fixes，3 项必改已修）。**
- **P3（管理台整合）已编码**：
  - **主窗 = 壳页 `shell.html`**：顶部 Tab 栏（工作台 / 常规 / 网络 / 插件 / 更新 / 卸载）；**工作台 Tab 用 `<iframe>` 内嵌 dsh**（`show_window` 改为向壳页 `emit dsh:url`，由 shell.js 设置 iframe src，整窗不再导航外部页面）。
  - **安全面收窄（红利）**：Tauri 只往**主 frame** 注入 `window.__TAURI__`（核实 tauri-runtime-wry `for_main_frame_only: true`）→ dsh 在 iframe 内**拿不到 IPC**，比之前的 label 白名单更安全；管理面板因此必须内嵌壳页主 frame（不走 iframe）。
  - **托盘收敛为「显示主窗口 / 管理台 / 退出」**，其余 9 项移入壳页管理 Tab。
  - 新增壳页 command：`get_dsh_url`、`get_shell_state`、`choose_workspace_cmd`、`open_workspace_cmd`、`open_logs_cmd`、`restart_dsh_cmd`、`open_browser_cmd`、`set_login_cmd`、`check_update_cmd`；`plugin_op`/`lan_state`/`lan_toggle` 白名单扩到 `WINDOW_LABEL | PLUGIN_LABEL | LAN_LABEL`。
  - CSP 增 `frame-src http://127.0.0.1:* http://localhost:*`（Spike §D）。
  - **P3 残留风险（Spike §C，需 macOS/Windows 真机回归）**：iframe 内键盘焦点/快捷键直达、剪贴板、`target=_blank`/新窗口承接、目录选择器、嵌套弹层布局等——见 §6 P3 验收矩阵；本环境无法跑 GUI，仅能编译期+DOM 层验证。
  - **P3 已评审（docs/review-p3-shell.md，Ship-with-minor-fixes，4 项必改已修）**：① `check_update_cmd` 改 async+spawn_blocking（原同步主线程会卡死弹窗/更新）；② 插件实时输出 `dsh:plugin-output` 改向 `WINDOW_LABEL`（壳页）+PLUGIN_LABEL 双 emit；③ `choose_workspace_cmd` 保存后自动重启工作台（与文案一致）+ async+rfd 入 spawn_blocking；④ `show_window` 重建兜底改用 Tauri 事件总线 `w.emit`（去掉无效裸 CustomEvent），另 shell.js 先 listen 再轮询 get_dsh_url 消除竞态。
  - **⚠ v0.2.0 真机回归发现 + 修复（2026-08-19）**：壳页 `shell.js` 的 `loadWorkbench` 初始化误写成**顶层 await**（普通 `<script>` 非 module，顶层 await 非法）→ WKWebView 解析报 `SyntaxError: Unexpected identifier 'T'`，整个 JS 不执行 → `get_dsh_url`/`listen('dsh:url')` 未运行 → iframe src 永不设置，界面卡在「正在启动 dsh 工作台…」（dsh 本身正常，见 launcher.log）。修复：把事件监听 + URL 轮询包进 async 自执行函数（与 lan/plugins 同模式）。**教训**：ego-browser/静态检查只验证 DOM 结构，不触发经典脚本顶层 await 的语法错误——后续改动任一 ui/*.js 需用 `vm.Script`（经典 script 语义）编译验证 + 真机回归。

---

## 0. 结论先行（TL;DR）

| # | 用户反馈 | 根因（已核实） | 一句话方案 |
|---|---|---|---|
| 1 | Windows 右键→删除 无效、无反应 | App 常驻托盘+隐藏窗口，uninstall.exe 删程序目录时进程/文件被锁；且 NSIS 卸载器不清用户数据 | 统一卸载链：NSIS `installer_hooks` 注入完整卸载（杀进程→删数据→删文件），App 内卸载=退出自身+唤起 uninstall.exe |
| 2 | uninstall.exe 功能不齐全 | 默认 NSIS 卸载器只删 `$INSTDIR`+快捷方式+注册表，不删 `%LOCALAPPDATA%/<id>`、`~/.dsh`、登录自启 | 同上（完全卸载），并把 ~/.dsh 做成卸载页可选的 checkbox |
| 3 | App 壳 / 弹框 UI 太丑 | 自绘暗色内容 vs 系统亮色标题栏违和；rfd 原生系统对话框与整体风格冲突 | 主窗/弹窗统一设计系统：自绘或深色标题栏、rfd 全部替换为自绘弹层 |
| 4 | 弹框未居中、主窗未居中 | 所有 `WebviewWindowBuilder` 未调用 `.center()`；弹窗无相对父窗口定位 | 弹窗相对主窗中心定位（`decorations(false)` 无系统按钮）；主窗每次显示 `.center()` |
| 5 | 右键功能移到 App 壳 | 12 项功能堆在托盘右键 | 收敛托盘为「显示/管理台/退出」，功能并入主框架「管理台」（Tab 或 ⌘K 命令面板） |

---

## 1. 现状核查（代码证据）

### 1.1 Windows 卸载链路

**注册表出口（正常存在）**：Tauri NSIS 模板（本项目 bundler 2.9.4）确认写入
`WriteRegStr SHCTX ... UninstallString -> "$INSTDIR\uninstall.exe"`（`installer.nsi:706`，`WriteUninstaller` 679 行）。
即 Windows「设置→应用」与开始菜单右键→卸载 的入口本身是注册过的。

**但默认 uninstall.exe 只做**：
- 删 `$INSTDIR` 程序文件 + 开始菜单快捷方式 + 本应用 Uninstall 注册表键。
- **不删** `%LOCALAPPDATA%\com.dsh-desktop.app`（WebView2 数据/缓存）、`~/.dsh`、登录自启项。

**App 内卸载（`uninstall_run` / `uninstall_teardown`）**：
- 销毁所有 WebView → 杀 dsh 子进程 → 关登录自启 → 删 app_data / WebView2 缓存 / 可选 `~/.dsh`。
- 随后 **Windows 侧 `trash_self()` 基本必然失败**（要删除的正是正在运行的 exe 与 DLL，OS 锁定）→ 回退弹系统通知「请到设置→应用→已安装的应用中卸载」，把删程序文件甩给 NSIS。

→ **割裂**：数据清理和程序文件删除是两条互不感知的路径，且 Windows 下 App 内卸载永远无法自己删干净。

### 1.2 “右键→删除 无反应”最可能根因（待实测确认）

1. **App 常驻托盘、主窗口隐藏** —— 用户以为 App “关了”，实际 `dsh-desktop.exe` + node 仍在跑。此时从系统触发 uninstall.exe，它会尝试删除运行中 exe/DLL 失败，既不弹错误也无进度 → 表现为“点了没反应 / 卡死”。
2. 若旧版安装残留/手动删过 `uninstall.exe`，UninstallString 指向失效文件 → 完全无反应。
3. 部分场景 Win11 从开始菜单右键卸载走 `Virtualization-aware` 路径，运行中应用会被系统静默拦下。

### 1.3 窗口与居中（全部核实）

- 主窗口（`main.rs:2511`）、lan（1292）、plugins（1269）、uninstall（1315）四处 `WebviewWindowBuilder` **均无 `.center()`、无 `decorations=false`、无相对父窗口定位**。
- Tauri 2.11.5 已核实支持：builder `.center()` / `window.center()`（`webview_window.rs:456/1915`），`decorations(bool)`，`title_bar_style()`（macOS overlay；`mod.rs:768`）。
- 页面内 CSS 实测（ego-browser DOM 度量）：卡片在页面内是**居中**的（`cx=vw/2, cy=vh/2` 全部命中）→ “未居中”问题 100% 在**窗口级**，不涉及前端 CSS。

### 1.4 UI 违和点归纳

- 自绘内容 = 暗色玻璃（#05060a 底、紫/粉/蓝光、玻璃卡片），但每个窗口都顶着**原生标题栏**（macOS 深灰 / Windows 白底 + ↑□✕），色温、圆角、材质完全冲突 → “壳子丑”。
- 现有弹窗带系统标题栏 + Windows 缩/放/关按钮，与“一个操作卡片”的形态冲突（用户点名：弹框不需要放大缩小关闭）。
- **rfd 系统对话框**（启动失败、连续崩溃、更新确认 YesNo）是 Win32/AppKit 原生白底对话框 → “App 的弹框丑”最突出来源。
- OS 通知（`update::notify`）本身合理，保留。

### 1.5 功能分布现状（托盘 12 项）

显示主窗口 / 浏览器打开 / 设工作目录 / 打开工作目录 / 检查更新 / 打开日志 / 重启工作台 / 插件管理 / 扫码远程 / 登录自启(勾选) / 卸载 / 退出。全部驻留托盘右键，主窗口壳层没有入口。

---

## 2. 产品调研

### 2.1 dsh 本体 = 开发者工作台，壳应“安静承接”

- 官方 `dsh --profile web` 是类 Claude Code / IDE 风格的工作台（会话 / 工作区 / 工具链多区），视觉偏开发者中性（社区出现了多款主题美化插件：`dsh-neu-theme`、`DSH-Transparent-UI-Plugin`、液态玻璃丝绸暗色主题等，反证官方界面较朴素）。
- **结论**：App 壳的作用是“框住并服务于 dsh 内容”，不是第二个主题秀场；壳与弹层应低调、跟随系统深/浅、以功能清晰为准，避免粉紫色光斑喧宾夺主。

### 2.2 社区同类壳（竞品参照）

- [dsh-desktop-hub](https://github.com/FlashingChen/dsh-desktop-hub)（Electron）：**左侧四 Tab（Harness 官方 web UI 内嵌 + 插件/MCP/Skills 管理台）**，管理能力全部进主框架，而非托盘。
- [dsh-launcher](https://github.com/dzbxdgn/dsh-launcher)：日志、健康监控、停止/重启、deep dark UI、双实例保护 —— 对应本项目托盘里的“日志/重启/状态”。
- 桌面 AI 工作台惯例（ChatGPT / Claude / 各类 IDE）：设置与管理都进 **App 内**（侧栏 / 模态弹层 / ⌘K 命令面板），**托盘只留显示与退出**。

### 2.3 Tauri NSIS “完全卸载”官方能力（已核实本机 bundler 2.9.4）

- `nsis.installer_hooks`（`.nsh` 文件）提供 `NSIS_HOOK_PREUNINSTALL` / `NSIS_HOOK_POSTUNINSTALL` 等宏，可注入自定义卸载逻辑；另有 `template` 全量自定义、`uninstaller_icon` 等。
- 官方近期已推进“卸载清理用户数据与自启项”（tauri commit [b367786](https://github.com/tauri-apps/tauri/commit/b3677864e93218b7f4ae8a71a8ee31071a2bbed4)、[2b960df](https://github.com/tauri-apps/tauri/commit/2b960dfd9fdc995bd6474958c05783ff53b64b7e)）→ 完全卸载方向与官方一致。
- 结论：**不需要引入第三方卸载器**，用官方 NSIS hooks + 现有 Rust teardown 即可做到“uninstall.exe 一键完全卸载”。

---

## 3. 目标设计

### 3.1 窗口体系与居中（含用户最新确认）

**主窗口（工作台）**
- 每次打开/显示（首次启动、托盘「显示」、Dock 激活）都 **`.center()` 到电脑屏幕中心**（用户已确认）。
- 标题栏（**已决策：方案 B 全自绘玻璃标题栏**，分两步落地，降低回归）：
  - **B1（随 P2 上）**：macOS `titleBarStyle: Overlay`（保留原生红绿灯、内容上顶，零自绘成本）+ Windows **先上深色原生标题栏**（`tauri.conf.json` 主题可控时跟随系统）。这一步已消除“亮标题栏 vs 暗内容”的违和，且不损失任何系统窗口能力。
  - **B2（二期，对比后决定）**：Windows 全自绘（`decorations(false)` + `data-tauri-drag-region` 拖拽区 + 自绘 min/max/close）。需重造：Aero 贴边/双击最大化、快照布局（Snap Layouts）、圆角、Resize 边框、标题栏右键系统菜单、多显示器 Snap、辅助功能焦点。
  - **决策安全阀**：B2 若体验/回归成本不及预期，保留 B1 即为最终态（退路清晰）；B1 已是统一暗色壳。
  - 视觉与弹窗同一玻璃设计语言（主题 token），避免“亮标题栏 vs 暗内容”违和。
  - 注意事项：自绘标题栏要覆盖拖拽、双击最大化/还原、右键系统菜单、全屏/沉浸、辅助功能焦点，归属 P2/B2，需 macOS + Windows 双侧回归。

**操作弹窗（lan / plugins / uninstall / 新增 settings）**
- `decorations(false)` + `resizable(false)`：**无系统标题栏、无放大/缩小/关闭按钮**（用户已确认只留操作按钮）。
- 定位：**相对主窗口中心**（取主窗 `outer_position + outer_size/2`，减去弹窗半尺寸 → `set_position`），多显示器也正确；主窗不存在/隐藏时回退屏幕中心。
- 全部加 `.center()` 兜底；重复打开时聚焦不重建（现状已具备）。

**系统对话框替换**
- 替换 rfd（启动失败 / 连续崩溃 / 更新确认）为**自绘模态弹层**（复用现有玻璃卡片设计），统一风格、可居中、可选操作按钮（更新/重试/打开日志等）。
- 保留 `update::notify` OS 通知（那是合理的系统级通知，不算“弹框”）。

**统一设计 token（新 `shared.css` / `theme.css`）**
- 颜色 / 圆角 / 字体 / 阴影 / 按钮 / 输入框 / 弹层 / 状态色 抽成公共样式，四个页面共用（消除现有 4 份重复 CSS），同时收敛光斑强度（弹层弱化、主壳中性化、跟随系统深浅）。

### 3.2 App 壳信息架构：把功能从托盘移进 App

目标（用户已确认方向：右键功能整体上移到 App，托盘只留最小集）：

**⚠ 架构前提（P3 前必须先行验证）**：主窗口当前是「单 webview 直接导航 dsh URL」，要在主框架承载壳层 UI（左栏 / Tab）只有两条路：

- **路 A：壳页 + `<iframe>` 内嵌 dsh 工作台**（dsh-desktop-hub 同款思路；架构级改动）。需先验证：
  - dsh web 是否允许被 iframe（CSP `frame-ancestors` / `X-Frame-Options`，且是跨源 `127.0.0.1:<dsh端口>` → tauri localhost）；
  - iframe 内交互完整性：键盘事件、焦点、**WebSocket（dsh 流式输出）**、剪贴板、弹层定位、全屏/打印/新窗口打开策略（须兼容现有 `on_new_window` 策略）。
- **路 B：独立管理窗 + ⌘K 命令面板**（托盘三项不变、管理功能仍全进 App，形态为弹窗而非 Tab）。
- **Spike（P3 动工前，半天）**：做「壳页 iframe 内嵌 dsh」最小原型，跑完上面的交互自测。
  - iframe 可行 → 走路 A（Tab 并入主框架，落实已确认决策 4）；
  - iframe 不可行 → 切路 B（决策 4 降级为「独立管理窗 + 命令面板」，其余决策不受影响）。

**主框架新增“管理台”（实现路径由 Spike 结论决定：并入主框架 / 独立管理窗）**
- 入口：主窗口左上角轻量齿轮/⌘K（Ctrl+K）命令面板，列出全部管理动作并支持键盘直达。
- 内容以**单窗口内 Tab** 组织（对齐 dsh-desktop-hub 心智）：
  - `常规`：工作目录（选/打开）、登录自启、重启工作台、打开日志、（加入）状态/健康。
  - `网络`：扫码远程连接（现有 lan 面板迁入，QR/地址/口令/开关）。
  - `插件`：现有插件管理面板迁入。
  - `更新`：检查更新/更新内容（替换 rfd 确认）。
  - `关于/卸载`：版本、卸载入口。
- 现有 lan / plugins / uninstall 三个独立弹窗 → **并入 Tab**（窗口数从 4 → 1，天然“居中于 App”，解决弹框杂乱；uninstall 保留为危险操作的二次模态确认）。

**托盘收敛（保留最小集）**
- `显示主窗口` / `管理台` / `退出`（+ 可选：开机自启勾选保留在托盘还是仅管理台——倾向仅管理台）。
- 其余 9 项全部进 App 壳；Dock/任务栏也能完成同样操作。

### 3.3 Windows 卸载统一（目标态）

**唯一卸载链：uninstall.exe（NSIS hooks + Rust teardown 复用）**
1. `NSIS_HOOK_PREUNINSTALL`（.nsh）运行本项目的卸载 sidecar（见下方机制）→ 结束 `dsh-desktop.exe` + 本项目 node/dsh/lan-proxy 子进程 → 等句柄释放 → 执行 teardown（删 `%LOCALAPPDATA%/<id>`、WebView2 数据、可选 `~/.dsh`、登录自启注册表项）→ 再交给 NSIS 删程序文件（`NSIS_HOOK_POSTUNINSTALL` 兜底清扫）。
2. **~/.dsh 由用户在卸载页勾选**（默认保留；自定义卸载页 checkbox，走 installer_hooks/template）。
3. 卸载完成页显示结果；失败清单（被占用文件）以可读文本给出。

**关键机制（覆盖“从系统右键/设置触发”的场景）**
- 从系统入口触发时，**没有任何人先跑过我们的 teardown**，因此唯一兜底是：**PREUNINSTALL 执行 `dsh-desktop.exe --self-uninstall-full [--wipe]`**（复用现有 CLI hook 风格；`CREATE_NO_WINDOW` 隐藏控制台/窗口）。
- 卸载 sidecar 失败**不阻断** NSIS 继续删文件（容忍清理），失败清单归入卸载结果页。
- 该入口改为**无 GUI、无控制台**的专用卸载模式（区别于现在的 `--self-uninstall-test`，后者仅测试不删程序文件）。

**App 内「卸载」入口的职责变化**
- Windows：App 内卸载 = 二次确认（含 ~/.dsh 选项，沿用现有玻璃卡片）→ 退出自身 → 唤起 `uninstall.exe`/系统卸载器完成唯一链。不再出现“请到设置里卸载”的割裂兜底。
- macOS：保留现有 `trash_self()`（有效，可直接删 .app）+ teardown；与 Windows 语义对齐（同样可选 ~/.dsh）。

**对应修复“右键无反应”**
- uninstall.exe 启动即结束运行实例（不再因锁卡死/无反馈）；若 UI 卸载中遇到占用，明确提示。
- 顺带验证 UninstallString/DisplayName/DisplayIcon 完整（当前已由模板写入）。

### 3.4 弹窗交互细节（无系统装饰后的补齐项）

自绘/无装饰后，原“系统 ✕”消失，补齐以下行为：

- **关闭方式**：`Esc` 关闭普通弹窗（lan/plugins；uninstall 属危险操作**禁用 Esc 关闭**）+ 卡片内置 ✕ 按钮；不采用“点遮罩关闭”（易误触）。
- **固定尺寸**：所有弹窗统一 `resizable(false)`（现在 lan/plugins 可缩放，无系统按钮后缩放意义不大），观感更整洁。
- **居中 clamp**：相对主窗中心定位后再 clamp 到当前工作区（主窗在副屏 / 最大化时避免弹窗跑出屏幕边缘）。
- **启动页（index.html）收敛**：现有紫粉光斑过“炫”，与“安静承接 dsh”的目标矛盾 —— 统一弱化光斑、跟随系统深浅色，减少动画（保留 `prefers-reduced-motion` 分支）。
- **字体栈**：Windows 增加 `Segoe UI` 置于首位（当前 `Microsoft YaHei` 观感一般），中文兜底保留。
- **rfd 选目录对话框**（工作目录 FileDialog）同为系统原生，未点名但属同类违和 —— 列入二期自绘候选，不阻断。

---

## 4. 分阶段实施计划（每阶段可独立发版）

| 阶段 | 内容 | 风险 | 触发方式 |
|---|---|---|---|
| **P1 居中与弹窗形态** | 全窗口 `.center()`；主窗每次显示居中；弹窗 `decorations(false)` + 相对主窗中心定位 + 无系统按钮；弹窗 Esc/✕ 关闭、固定尺寸、居中 clamp | 低 | 下一版 v0.1.9 |
| **P2 设计统一 + 标题栏 B1** | 抽 `theme.css` 设计 token；弹窗样式收敛；替换 rfd → 自绘弹层；启动页收敛、字体栈；**标题栏 B1**（macOS overlay + Windows 深色原生） | 低-中（不损失系统窗口能力） | v0.1.9 |
| **P3 管理台整合** | **先跑 iframe Spike（半天）定路径 A/B** → 管理台（常规/网络/插件/更新/卸载：Tab 或 独立管理窗+⌘K）；托盘收敛为 显示/管理台/退出 | 中 | v0.2.0 |
| **P4 卸载统一** | NSIS hooks 注入完整卸载（PREUNINSTALL 跑 `--self-uninstall-full [--wipe]`，失败不阻断）；App 内卸载改为唤起唯一链；~/.dsh checkbox；Windows 实测右键→卸载 | 中-高（需 Windows 实机验收） | v0.2.0 或 单独 v0.1.x |
| **B2（二期可选）** | Windows 全自绘玻璃标题栏（decorations=false + 自绘 min/max/close + Snap/贴边/圆角），对比 B1 后决定是否保留 | 高（需 mac/win 双侧回归） | 二期 |

> 建议：P1+P2（含标题栏 B1）作为 **v0.1.9** 先发（低风险立刻改善体验）；P3+P4 作为 **v0.2.0**（结构性改动，Windows 需实机回归）；B2 视 B1 验收结果与资源再排。

---

## 5. 已确认决策（2026-08-19 用户拍板）

1. **弹窗居中**：弹窗居 App 主窗口中心；主窗启动即居电脑屏幕中心。（已确认）
2. **主窗标题栏**：**B 全自绘玻璃标题栏**，落地为两步：**B1**（P2：macOS overlay + Windows 深色原生）→ **B2**（二期：Windows 全自绘 `decorations=false`，对比后决定留否）。
3. **~/.dsh 卸载策略**：**默认保留 + 卸载页可勾选删除**（复选框，默认不选）。
4. **管理台形态**：**并入主框架内 Tab**（常规 / 网络 / 插件 / 更新 / 卸载），窗口 4→1。
5. **托盘最终形态**：**显示主窗口 / 管理台 / 退出** 三项；其余（含登录自启开关）全部移入管理台。

> 若后续想调整（如把自绘标题栏降级为原生深色、或管理台改独立小窗），改动点集中在 §3.1 / §3.2，其余不受影响。

---

## 6. 分阶段验收矩阵（mac / win 双侧）

| 阶段 | macOS 验收 | Windows 验收 |
|---|---|---|
| **P1** | 主窗/各弹窗打开即居中；弹窗相对主窗中心、无系统按钮；Esc 关闭 lan/plugins、uninstall 禁止 Esc；拖动/多屏不越界 | 同左；弹窗在深/浅主题下均可见；Win11 分屏时 clamp 生效 |
| **P2** | 标题栏 B1（overlay 红绿灯可用，内容上顶不遮挡）；浅色/深色下弹层对比度达标 | 深色原生标题栏无白边；rfd 已无残留（弹层自绘）；字体栈 Segoe UI 生效 |
| **P3** | iframe 壳内 dsh：键盘/焦点/WebSocket 流式/剪贴板/弹层定位正常；⌘K 面板可用；托盘=显示/管理台/退出 | 同左；多显示器下管理台位置正确 |
| **P4** | 唯一卸载链（trash_self + teardown）手动全流程：保留/删除 ~/.dsh 两态 | 系统「设置→应用→右键→卸载」与开始菜单右键→卸载均能完成 完整卸载（含数据、登录项）；卸载时已运行实例自动结束；~/.dsh checkbox 生效 |

---

## 附：涉及文件（供计划参考）

- `apps/desktop/src-tauri/src/main.rs`（窗口 builder、tray menu、uninstall_run/teardown、rfd、`--self-uninstall-full` CLI 卸载入口）
- `apps/desktop/src-tauri/tauri.conf.json`（nsis 配置含 installer_hooks、窗口默认）
- `apps/desktop/src-tauri/installer-hooks.nsh`（新增：PREUNINSTALL 完整卸载逻辑）
- `apps/desktop/ui/`（index/lan/plugins/uninstall + 新增 theme.css、shell 壳页（spike 原型 A）、command palette、settings）
- `.github/workflows/release.yml`（不变，P1-P4 均随版本发布即可）
