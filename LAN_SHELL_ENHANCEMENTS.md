# 局域网壳层三增强 — 任务书

> 本文件是给开发会话的**自包含实施说明**（无本仓库历史上下文也可直接干活）。
> 主题：为 DeepSeek Harness 桌面壳（Tauri v2 + Rust，macOS + Windows）的"局域网访问"功能增加三个壳层增强：
> **① 局域网期间阻止系统休眠　② mDNS 稳定域名通告　③ 局域网 IP 变化检测与通知**
>
> 背景动机：Mac 休眠 → 手机必断（机器睡了谁都服务不了）；Mac 唤醒后若 IP 变了 → 手机拿着旧 IP 永远连不上，且当前产品没有任何提示。三个增强分别缓解这三件事。

---

## 1. 项目定位与硬约束（先读，再动代码）

- **纯壳定位**：本应用是 dsh 的壳——不二开 dsh、不加插件、不解析 dsh 的任何内部结构（API、协议、配置、UI 都是黑盒）。
- **局域网现状**：dsh 只绑 `127.0.0.1`；壳内置 `lan-proxy.js`（node 进程，绑 `0.0.0.0:<lan_port>`，默认 3190）做字节级 HTTP/WS 中继 + 令牌登录（cookie 30 天）。**本任务只动壳（Rust 侧），不修改 `lan-proxy.js`，不碰任何 dsh 文件。**
- **技术手段限制**：只允许通用系统 API / 系统自带二进制。项目已有先例：`osascript`（弹窗）、`ipconfig`（取 IP）、`libc::kill`（unix）/ `taskkill`（windows）。**优先调系统二进制，禁止为增强引入需要新下载的重型 crate**（若确需新依赖，必须在代码注释和本文件的"变更说明"里写明理由与体积）。
- **失败静默降级**：任何增强失败 → 用 `logln!` 记日志，局域网主功能必须不受影响、不 panic、不阻塞。
- **默认关闭原则**：改变功耗/网络行为的能力必须可开关且默认关（本任务中：仅"阻止休眠"需要开关；mDNS 通告与 IP 通知无害，随 LAN 自动开启，不加 UI 开关）。
- **跨平台**：当前分支为 `windows-support`，macOS + Windows 都必须能编译。跨平台差异用 `#[cfg(target_os = ...)]` 隔离。Windows 侧本期只实现增强③（IP 轮询无平台差异），① ② 标注清晰 TODO 不实现。
- **日志**：统一走 `main.rs` 的 `logln!` 宏（写入 launcher 日志）。

---

## 2. 现状代码地图（直接可查，行号以当前分支为准，偏差按函数名搜索）

主文件：`apps/desktop/src-tauri/src/main.rs`（下称 main.rs）；通知在 `src/update.rs`。

| 符号 | 位置 | 说明 |
|---|---|---|
| `struct Settings` | main.rs ~252 | 字段：`default_cwd`、`registry`、`lan_enabled`、`lan_port`、`lan_token`，全部 `#[serde(default)]`。文件：macOS `~/Library/Application Support/com.dsh-desktop.app/settings.json`。读写：`load_settings(app)` / `save_settings(app, &s)` |
| `fn start_lan(app, dsh_port, notify)` | ~798 | 持 `LAN_MUTEX` → 调 `start_lan_unlocked` |
| `fn start_lan_unlocked(...)` | ~807 | **LAN 一切子进程的出生点**：杀旧代理 → spawn `lan-proxy.js`（`0.0.0.0:<port>`）→ stdout/stderr 管道进日志 → 看护线程（代理意外退出自动重启，重启也走本函数）→ 存 PID 到 `LAN_CHILD` |
| `fn kill_lan_unlocked()` | ~760 | **LAN 一切子进程的埋葬点**：`LAN_GEN` 代际 +1、`LAN_ON=false`、按 PID `libc::kill(SIGTERM)`（unix）/ `taskkill`（windows） |
| `fn kill_lan()` | ~755 | 持锁调 `kill_lan_unlocked` |
| `fn ensure_lan_on_ready(app, port)` | ~1010 | dsh 就绪/崩溃重启后按 `settings.lan_enabled` 拉起 LAN |
| `fn set_lan_access(app, enable)` | ~893 | 托盘"局域网访问"开关入口 |
| `fn lan_ip()` | ~693（macOS）/ ~706（windows） | 取本机局域网 IPv4（macOS：`ipconfig getifaddr en0/en1`） |
| `fn lan_info_dialog(token, port)` | ~947 | osascript 弹窗显示 `http://{ip}:{port}` + 令牌（每次打开实时取 IP） |
| `fn show_lan_info(app)` | ~933 | 托盘"显示局域网访问信息" |
| 托盘菜单 | setup 内 ~1419 起 | `CheckMenuItem::with_id(app, "lan_access", "局域网访问", ...)` ~1437；`MenuItem::with_id(app, "lan_info", "显示局域网访问信息", ...)` ~1447；**事件分发在 `on_menu_event` ~1478**（lan_access 分支 ~1497、lan_info 分支 ~1501）。勾选态同步有现成模式：`LAN_ITEM` 静态变量 + `sync_lan_check()` |
| `update::notify(title: &str, body: &str)` | src/update.rs ~256 | 系统通知（托盘气泡） |
| 退出清理 | `RunEvent::ExitRequested \| Exit => kill_dsh()`（~1289） | kill_dsh → kill_lan。**挂在 kill_lan 体系里的子进程自动获得退出清理** |
| 看护重启 | start_lan_unlocked 内 | 代理退出 → 重新调 `start_lan_unlocked`。**因此：增强子进程的启停写进 start_lan_unlocked / kill_lan_unlocked，就自动获得"崩溃重启跟随 + 退出清理 + 与开关同步"** |

**关键设计结论**：三个增强的子进程启停全部挂在 `start_lan_unlocked` / `kill_lan_unlocked` 生命周期内，不要另起一套生命周期管理。

---

## 3. 三个增强的完整规格

### 增强 ① 局域网期间阻止系统休眠（macOS 实现；Windows 本期不做）

**目标**：局域网访问开启期间（用户可选）阻止 macOS 自动休眠，保证手机随时可连。默认关闭，用户可随时切换。

**实现（macOS）**：
1. `Settings` 新增字段：`lan_prevent_sleep: bool`（`#[serde(default)]`，默认 false）。
2. 托盘菜单新增 `CheckMenuItem`：id `"lan_prevent_sleep"`，文案 `"局域网期间阻止休眠"`，注册在 `lan_info` 菜单项附近；初值从 `load_settings` 读取（参照 `lan_access` 的做法）。
3. `on_menu_event` 加分支（参照 lan_access 分支）：切换 `settings.lan_prevent_sleep` 并 `save_settings`；若 LAN 当前已开启（`LAN_ON`），立即启/停对应的子进程。
4. 在 `start_lan_unlocked`（成功 spawn 代理之后）与 `kill_lan_unlocked` 中，按 `settings.lan_prevent_sleep` 启停子进程：
   - 启动：`Command::new("/usr/bin/caffeinate").args(["-dimsu"]).spawn()`
     - `-i` 阻止 idle 系统休眠（核心）；`-d` 显示器、`-m` 磁盘、`-s` 系统、`-u` 用户活跃，全上最稳。
   - 停止：按现有 PID 模式幂等 kill（unix 用 `libc::kill(SIGTERM)`）。
   - 子进程句柄：新增静态 `Mutex<Option<Child>>`（或 PID），命名如 `SLEEP_GUARD`；kill 前先判断存在，幂等。
   - spawn 失败：`logln!` 警告并继续（降级，LAN 不受影响）。

**Windows**：本期不实现（无 caffeinate；`SetThreadExecutionState` 需 windows crate，标注 `// TODO(windows): SetThreadExecutionState(ES_CONTINUOUS|ES_SYSTEM_REQUIRED) 需引入 windows crate 后实现`）。用 `#[cfg(target_os = "macos")]` 隔离，保证 Windows 构建通过。

**验证**：开关开 + LAN 开 → `pmset -g assertions` 可见 caffeinate 的电源断言；关闭 LAN 或关开关 → 断言消失、`ps` 无 caffeinate 残留。

### 增强 ② mDNS 稳定域名通告（macOS 实现；Windows 本期不做）

**目标**：手机/平板可用稳定名字 `http://DeepSeek-Harness.local:<port>/` 访问，Mac 的局域网 IP 变了**不用改地址**（解决"IP 变了手机连不上"的根治方案之一）。

**实现（macOS）**：
1. `start_lan_unlocked` 成功 spawn 代理后，spawn：
   ```
   /usr/bin/dns-sd -R "DeepSeek Harness" _http._tcp local <lan_port>
   ```
   `<lan_port>` = `settings.lan_port.unwrap_or(3190)`。
   - `-R` 注册服务；该进程常驻（持续通告），kill 即撤销。注册名派生 `.local` 域名：`DeepSeek-Harness.local`（空格→连字符）。
2. `kill_lan_unlocked` 里幂等终止（同增强 ① 的子进程管理模式，新增静态如 `MDNS_CHILD`）。
3. 文案：`lan_info_dialog` / `show_lan_info` 的弹窗同时显示两条地址：
   - `http://{ip}:{port}`（现有）
   - `http://DeepSeek-Harness.local:{port}`（新增，注明"IP 变了也不用改"）
4. 无开关（无害、随 LAN 自动开启）。spawn 失败 → `logln!` 降级。

**已知限制（写进弹窗副文案，不是 bug）**：
- `.local` 解析仅 iOS/macOS 客户端；**Android 浏览器不解析 .local**，Android 用户仍用 IP 访问。
- 仅同子网有效；路由器开启"AP/客户端隔离"或禁用 mDNS 时无效（自动降级为 IP 访问）。

**验证**：Mac 上 `dns-sd -B _http._tcp` 能看到 `DeepSeek Harness`；iPhone Safari 直接打开 `http://DeepSeek-Harness.local:<port>/` 能进登录页。

### 增强 ③ 局域网 IP 变化检测 + 通知（macOS + Windows 都实现）

**目标**：Mac 的局域网 IP 变化（休眠重连 / 换 Wi-Fi / DHCP 重分配）时，用户立刻收到新地址通知；否则手机拿着旧 IP 连不上且无从查起。

**实现**：
1. LAN 开启期间起一个后台轮询线程：每 30 秒调一次 `lan_ip()`，与上次记录比较。
2. 变化时：
   - `logln!("lan ip changed: {old} -> {new}")`
   - `update::notify("局域网地址已变化", format!("新地址 http://{new}:{port}"))`（port = `settings.lan_port.unwrap_or(3190)`）
   - 更新内部记录（静态 `Mutex<Option<String>>`，如 `LAN_IP_LAST`）。
3. 线程启停挂在 `start_lan_unlocked` / `kill_lan_unlocked`（用 `Arc<AtomicBool>` 停止标记 + `JoinHandle`；注意看护重启路径会重新走 start_lan_unlocked → 线程自然重建）。
4. 双平台：`lan_ip()` 已有 windows 分支，轮询逻辑无平台差异，不需要 cfg。
5. 无开关（无害）。30s 一次 `ipconfig` 子进程，CPU 可忽略。
6. 可选优化（本期不做，写 TODO 注释即可）：macOS 改用 `SCDynamicStore` / `NWPathMonitor` 事件驱动，零轮询。

**验证**：开启 LAN → 改 Mac 的局域网 IP（系统设置改静态 IP，或断开重连 Wi-Fi）→ ~30 秒内收到系统通知，内容含新地址。

---

## 4. 通用实现要求（每条都要做到）

1. 增强子进程全部挂在 `start_lan_unlocked` / `kill_lan_unlocked` 生命周期内（自动获得：看护重启跟随、App 退出清理、与托盘开关同步）。
2. 所有 kill 幂等（子进程可能已死 / 从未启动 / 被看护重启替换）；用现有静态变量模式（`Mutex<Option<Child>>` 或 PID），不引入新锁序，避免死锁（LAN_MUTEX 只在 start_lan_unlocked / kill_lan_unlocked 调用方持有时操作）。
3. spawn 失败一律 `logln!` 降级，绝不 panic、绝不阻塞 LAN 主流程、绝不导致 start_lan 返回 Err。
4. 新增 `Settings` 字段必须 `#[serde(default)]`（老 settings.json 无此键，缺键要能反序列化）。
5. 托盘新增菜单项按现有模式：`CheckMenuItem::with_id` + `on_menu_event` 分支；初值从 settings 读取（参照 `lan_access` / `LAN_ITEM` / `sync_lan_check` 的完整模式）。
6. `cargo check` 通过（macOS 本机至少）；改动用 `#[cfg(target_os = ...)]` 隔离，不破坏 Windows 构建。
7. 不修改 `lan-proxy.js`、不修改任何 dsh 相关文件、不动 `tauri.conf.json`（除非确有必要并说明）。
8. 每个增强在代码里写清注释（中文），说明"为什么存在"和"失败时的降级行为"。

---

## 5. 验收清单

- [ ] **回归**：默认状态下（新字段缺省 / false），三增强全部不激活，开启 LAN 后行为与现状完全一致
- [ ] **①**：开关开 + LAN 开 → `pmset -g assertions` 出现 caffeinate 断言；LAN 关或开关关 → 断言消失、进程退出
- [ ] **②**：`dns-sd -B _http._tcp` 可见 `DeepSeek Harness`；iPhone Safari 可打开 `.local` 地址；Android 走 IP 不受影响；弹窗同时显示 IP 与 .local 两个地址
- [ ] **③**：改 IP 后 ~30 秒内收到通知，内容含新地址；日志有记录
- [ ] **看护跟随**：手动 kill 代理进程 → 代理自动重启，三增强子进程自动跟随（`ps` 全部在，且指向同一个新周期）
- [ ] **退出清理**：退出 App 后 `ps` 无 caffeinate / dns-sd / 轮询线程残留
- [ ] **跨平台**：`cargo check` 通过；Windows 构建路径不受影响（① ② 有清晰 TODO，③ 双平台生效）
- [ ] **无新增重型依赖**（如有例外，变更说明里写明）

---

## 6. 参考锚点（抄作业用）

- 现有 LAN 子进程管理（照此办理增强子进程）：
  - spawn：`start_lan_unlocked` 内 `let pid = child.id(); ... *mlock(&LAN_CHILD) = Some(pid);`
  - kill：`kill_lan_unlocked` 内 `libc::kill(pid as libc::pid_t, libc::SIGTERM)`（unix）/ `taskkill /pid <pid> /f`（windows）
- 托盘菜单：setup 内 `lan_access`（~1437）/ `lan_info`（~1447）注册；`on_menu_event`（~1478）分发。
- 通知：`update::notify(title, body)`（src/update.rs ~256）。
- Settings 读写：`load_settings(app)` / `save_settings(app, &s)`；字段全 `#[serde(default)]`。
- 日志：`logln!`（main.rs 宏）。
- 端口：LAN 端口 = `settings.lan_port.unwrap_or(3190)`（常量 `LAN_DEFAULT_PORT`）。
- 分支说明：本任务建立在 main 分支提交 `4311c41`（LAN 代理防崩溃 + 自动重启）之上，当前 `windows-support` 分支已包含；实现时以**当前检出分支的实际代码**为准，行号有偏差按函数名搜索。

## 7. 变更说明（实现后回填）

| 文件 | 改动摘要 |
|---|---|
| apps/desktop/src-tauri/src/main.rs | ① `Settings` 新增 `lan_prevent_sleep: bool`（`#[serde(default)]`，默认 false）；新增静态 `SLEEP_GUARD_ITEM`/`SLEEP_GUARD`(macOS)/`MDNS_CHILD`(macOS)/`LAN_IP_LAST`/`LAN_IP_WATCH_GEN`；新增函数 `start_caffeinate`/`kill_caffeinate`（macOS，`/usr/bin/caffeinate -dimsu`）、`start_mdns`/`kill_mdns`（macOS，`/usr/bin/dns-sd -R "DeepSeek Harness" _http._tcp local <port>`）、`start_ip_watch`（双平台，30s 轮询 `lan_ip()`，代际计数器停止，变化 → `logln!` + `update::notify`）。三增强全部挂入 `start_lan_unlocked`/`kill_lan_unlocked` 生命周期（随 LAN 启停/看护重启跟随/退出清理）。托盘新增 macOS 专属 `CheckMenuItem("lan_prevent_sleep","局域网期间阻止休眠")`（菜单改用 `Vec<&dyn IsMenuItem>` 组装以 cfg 门控）+ `on_menu_event` 分支；`lan_info_dialog` macOS 弹窗新增 `.local` 地址与限制提示。所有 spawn 失败仅 `logln!` 降级。 |
| README.md | §3.4 局域网访问新增"壳层增强"说明（mDNS 域名 / IP 变化通知 / 阻止休眠开关及平台差异）。 |
| 其他 | 无新增依赖（仅系统自带二进制 caffeinate/dns-sd/ipconfig；getrandom/libc 为此前既有）。Windows 侧：增强③生效；① ② 以 `#[cfg(target_os = "macos")]` 隔离并留有 `TODO(windows)` 注释，未实现。`tauri.conf.json`、`lan-proxy.js`、dsh 文件均未改动。 |
