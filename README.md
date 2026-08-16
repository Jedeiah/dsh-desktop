# DeepSeek Harness 桌面端（DSh Desktop）

<p align="center">
  <img src="apps/desktop/src-tauri/icons/icon-rounded-256.png" alt="DeepSeek Harness 图标" width="128">
</p>

一个 **macOS / Windows 桌面 App**：双击即用，把官方 DeepSeek Harness（`dsh web`）装进一个桌面 App 里，带托盘、自动更新、干净卸载，还能让同一局域网里的手机/平板通过浏览器连进来用。

---

## 1. 项目介绍

**它是什么**：一个**壳**——内置了官方 dsh 的完整运行环境（Node 运行时 + 全部依赖闭包），启动后在窗口里运行的就是**官方 dsh 工作台**，和你在终端跑 `dsh web` 完全一样。

**它不是**：不是二次开发、不加任何插件、不改官方行为。保持"壳"的定位，是为了**最大自由度**：
- 官方怎么配你就怎么配（`~/.dsh` 配置/会话/凭据与终端 dsh **完全共用**）
- 官方更新即跟随（内置自动更新到上游 `@deepseek-ai/dsh` 新版）
- 想加插件、改行为，直接走官方 dsh 自己的机制即可，壳不掺和

**关键特性**：
| 特性 | 说明 |
|---|---|
| 跨平台原生 App | macOS：菜单栏托盘 / Dock；Windows：系统托盘 / 任务栏，符合各平台使用习惯 |
| 双击即用 | 内置 Node v24 + dsh 完整闭包（含该平台原生预编译），**不依赖系统 bun / npm / node** |
| 局域网可连 | 可选开启令牌鉴权转发器，同一 WiFi 下手机/平板浏览器输令牌即可操作工作台 |
| 自动更新 | 跟随上游 `@deepseek-ai/dsh`，原子切换 + 失败安全 |
| 干净卸载 | 连带内置 dsh、应用数据、缓存、自启项、残留图标一并清理 |

> 平台差异：`~/.dsh`（配置/会话/凭据）在 macOS 是 `~/.dsh`，在 Windows 是 `%USERPROFILE%\.dsh`，与终端 dsh 完全共用。

---

## 2. 亮点

- **电脑上无需安装任何环境**：不需要装 Node、npm、bun、Python、Rust 或任何运行时——Node 与 dsh 全部依赖都内置在 App 里，双击即用，删掉系统里的开发环境也不影响它运行。
- **零偏差**：壳不注入任何东西，跑的就是官方 dsh，`~/.dsh` 无缝复用，随时可回到终端使用同一份数据。
- **跟随上游**：dsh 发新版，App 里一键更新，内置 npm 装新闭包 → 自检 → 原子切换 → 自动重启，失败不动当前版本。
- **标准 Mac 体验**：关窗口隐藏进托盘、Cmd+Q 连带结束 dsh 无孤儿、崩溃自动重启（退避 5 次后给日志）。
- **手机也能用**：家里 WiFi 下，手机浏览器输个令牌就能操作同一个工作台（对话/会话/文件，执行仍在 Mac 上）。
- **干净利落**：单实例（不会开双托盘）、卸载一步到位（含 WebView 缓存与 Dock 最近使用）。

---

## 3. 使用手册

### 3.1 安装

**macOS 一键安装 / 升级（推荐，自动追最新版）：**
```bash
curl -sSL https://raw.githubusercontent.com/Jedeiah/dsh-desktop/main/scripts/install.sh | bash
```
> 通过 GitHub 跳转自动解析最新正式版；curl 下载不带隔离标记，装完直接可用、无"损坏"提示；已运行会自动退出并覆盖安装。

**Windows 一键安装 / 升级（推荐，PowerShell）：**
```powershell
powershell -ExecutionPolicy Bypass -Command "irm https://raw.githubusercontent.com/Jedeiah/dsh-desktop/main/scripts/install.ps1 | iex"
```
> 自动解析最新正式版，退出已运行实例后下载安装器静默安装并启动。要求 Windows 10/11（自带 WebView2 运行时）。

**macOS 手动安装：**
1. 双击 **DeepSeek Harness_<版本>_aarch64.dmg**，把 **DeepSeek Harness.app** 拖进 **应用程序**。
2. 首次打开：**右键 → 打开**（未签名，需确认一次），之后正常双击即可。
3. 若提示"已损坏，无法打开"（Chrome 下载的未签名 App 常见）：`xattr -dr com.apple.quarantine "/Applications/DeepSeek Harness.app"`

**Windows 手动安装：**
- 下载 Release 里的 `DeepSeek.Harness_<版本>_x64-setup.exe` 双击安装（无需管理员权限，装到当前用户）。
- 或下载 `DeepSeek-Harness-Windows-x64.zip` 解压后双击 `dsh-desktop.exe` 直接运行（便携版，同样内置 node + dsh）。

### 3.2 首次使用

启动后自动拉起内置 dsh web → 窗口显示工作台（即 dsh web UI）。会话/凭据与终端 dsh 共用同一份数据目录——macOS 为 `~/.dsh`，Windows 为 `%USERPROFILE%\.dsh`——你在终端建过的会话，这里直接能看到。

### 3.3 日常操作

| 操作 | macOS | Windows |
|---|---|---|
| 显示/隐藏主窗口 | 左键点托盘图标，或点 Dock 图标 | 双击/单击托盘图标，或点任务栏图标 |
| 关闭窗口 | 隐藏到托盘 | 隐藏到托盘（App 与 dsh 继续后台运行） |
| 用系统浏览器打开 | 托盘 *在浏览器中打开* | 同左 |
| 退出 | `Cmd+Q` 或托盘 *退出* | 托盘 *退出*（连带结束 dsh，无残留） |
| 崩溃自愈 | dsh 意外退出自动重启（1s→2s→…→15s 退避）；连续 5 次后停止并提示查看日志 | 同左 |

### 3.4 局域网访问（手机/平板）

1. 电脑连上家里 WiFi，托盘勾选 **局域网访问**（默认关闭）。
2. 弹窗显示 **地址**（`http://<电脑局域网IP>:3190`）与 **访问令牌**（可一键复制）。
3. 手机/平板连同一 WiFi，浏览器打开地址，输入令牌，即可进入工作台（30 天免登录）。
4. 随时用托盘 *显示局域网访问信息* 查看地址与令牌；取消勾选即关闭。

**壳层增强**（随局域网开启自动生效，失败自动降级、不影响主功能）：
- **mDNS 稳定域名**（macOS）：可用 `http://DeepSeek-Harness.local:<端口>/` 访问，IP 变了也不用改地址。注意 `.local` 仅 iOS/macOS/同子网可解析，Android 浏览器请继续用 IP 地址。
- **IP 变化通知**（macOS / Windows）：局域网开启期间每 30 秒检测一次本机 IP，变化（休眠重连 / 换 Wi-Fi / DHCP 重分配）时弹系统通知并给出新地址。
- **阻止休眠**（macOS，可选）：托盘勾选 *局域网期间阻止休眠*（默认关）后，局域网开启期间阻止系统自动休眠，保证手机随时可连。

> ⚠️ 令牌是唯一门禁（128-bit 随机）。知道令牌 ≈ 能操作你电脑上的 dsh。**只在可信网络开启，令牌不要外传。**

### 3.5 更新

- 托盘 *检查更新…* 手动检查；App 每 24h 自动静默检查。
- 有新版 → 确认 → 自动安装并重启 dsh，窗口自动刷新为新版工作台。
- Windows 上更新同样通过内置 npm 安装新闭包，失败不动当前版本。

### 3.6 登录自启

托盘 *登录时启动*（勾选，默认关闭）→ 下次登录时自动启动 App。macOS 写 LaunchAgent，Windows 写注册表 `HKCU\...\Run` 键；只影响本 App，不碰其他应用的启动设置。

### 3.7 卸载

托盘 *卸载 DeepSeek Harness…* → 三键弹窗：

| 按钮 | 效果 |
|---|---|
| 取消 | 什么都不做 |
| 卸载（保留 ~/.dsh） | 卸载，保留会话/凭据（推荐） |
| 卸载并删除 ~/.dsh | 卸载，连会话/凭据一起删（不可恢复） |

卸载会：结束 dsh 与转发器 → 删除登录自启项、应用数据、WebView 缓存 → 把 App 移入废纸篓 / 回收站（可恢复）→ 退出。Windows 上若运行中的程序被占用无法移入回收站，会引导你到「设置 → 应用」里卸载。

### 3.8 日志与故障排查

- 托盘 *打开日志* 直接打开日志目录：`launcher.log`（启动器）+ `dsh.log`（dsh 运行输出）。
- 日志目录平台差异：macOS 为 `~/Library/Application Support/com.dsh-desktop.app/logs`，Windows 为 `%APPDATA%\com.dsh-desktop.app\logs`。
- dsh 启动失败 / 连续崩溃时，App 会弹窗给出日志位置。
- 常见问题：
  - **macOS 首次打不开** → 右键 → 打开（未签名）。
  - **Windows 白屏/黑窗** → 确认系统装有 WebView2 运行时（Win10/11 自带；老系统需安装 [WebView2 Runtime](https://developer.microsoft.com/en-us/microsoft-edge/webview2/)）。
  - **局域网连不上** → 确认同一 WiFi、电脑未开代理、令牌输入正确。

---

## 4. 技术细节（构建 / 架构）

> 这一节给想自己折腾的人看。日常使用不需要。

### 4.1 构建与运行

**macOS：**
```bash
# 前置：Rust 工具链 + tauri-cli（仅打包需要）
cargo install tauri-cli --version "^2"

# 准备内置资源（node + dsh 闭包 + 图标）
scripts/prepare-resources.sh
#   DSH_VERSION=<ver>  NODE_SRC=<node二进制>  CLOSURE_SRC=<闭包目录>  ICON_SRC=<png源图>
#   默认闭包源 = 项目内 resources/dsh/current（自给自足）

# 开发运行（终端可见 dsh 日志）
cd apps/desktop/src-tauri && cargo run

# 发布构建（产出 .app + DMG）
cargo tauri build
#   产物：target/release/bundle/macos/DeepSeek Harness.app
#        target/release/bundle/dmg/DeepSeek Harness_<版本>_aarch64.dmg
```

**Windows**（需在 Windows 机器或 Windows CI 上构建——闭包含 win32 原生二进制，必须在 Windows 上安装）：
```powershell
# 前置：Rust 工具链（VS Build Tools）+ tauri-cli
cargo install tauri-cli --locked

# 准备内置资源（node.exe + Windows dsh 闭包）
npm install --prefix "$env:TEMP\closure" "@deepseek-ai/dsh@<版本>" --ignore-scripts --no-audit --no-fund
./scripts/prepare-resources.ps1 -DshVersion <版本> -NodeSrc (Get-Command node).Source -ClosureSrc "$env:TEMP\closure\node_modules"

# 发布构建（产出 NSIS 安装器；加 --bundles msi 得到 MSI）
cd apps/desktop/src-tauri
cargo tauri build --bundles nsis
#   产物：target/release/bundle/nsis/DeepSeek Harness_<版本>_x64-setup.exe
#        （+ target/release/dsh-desktop.exe 与 resources/ 即为便携版）
```

> 首次编译较慢（Tauri 依赖树）。打包态日志落 `<app-data>/logs/`，dev 模式直接输出到终端。

**资源来源**：Node 从 fnm 安装（macOS v24.14.0 arm64；Windows 用官方 x64 node.exe）拷贝；dsh 闭包为 `@deepseek-ai/dsh` 的完整 `node_modules`（含对应平台原生预编译，运行期零编译）；图标为 `icons/icon.png`（RGBA 1024）+ `icon.icns`（macOS）+ `icon.ico`（Windows）。

### 4.2 目录结构

```
dsh-desktop/
├── README.md                    # 本文档
├── scripts/
│   ├── prepare-resources.sh     # macOS：打包 node + dsh 闭包 + 图标
│   ├── prepare-resources.ps1    # Windows：同名 PowerShell 版本
│   ├── install.sh               # macOS 一键安装
│   └── install.ps1              # Windows 一键安装
└── apps/desktop/
    ├── ui/index.html            # 前端占位（实际窗口指向内置 dsh localhost）
    └── src-tauri/
        ├── Cargo.toml           # tauri2(tray-icon,image-png) + serde + rfd + ureq (+ windows: arboard/trash)
        ├── tauri.conf.json      # identifier / bundle.resources / CSP / nsis / dmg
        ├── icons/               # icon.png(RGBA 1024) + icon.icns(macOS) + icon.ico(Windows)
        ├── resources/           # 内置 node + npm + lan-proxy + dsh 闭包（只读基线，gitignore）
        └── src/
            ├── main.rs          # 启动器：路径/闭包解析、spawn/boot、托盘、更新、卸载、日志、LAN 控制
            ├── update.rs        # 更新子系统：registry、semver、内置 npm 安装、原子切换、版本清理
            └── lan-proxy.js     # 局域网转发器（令牌鉴权 + HTTP/WebSocket 透传）
```

### 4.3 架构

**进程模型**：App 单个进程（Rust/Tauri），作为壳拉起一个 dsh web 子进程（内置 node 运行 `dsh --profile web --port 0`），由 Rust 托管（stdout 解析就绪、退出回收、崩溃重启、退出时连带结束）。局域网模式另起一个转发器子进程（内置 node 运行 `lan-proxy.js`）。

**内置资源（只读基线）**：

macOS（`DSh.app/Contents/Resources/resources/`）、Windows（`<exe 旁>/resources/`）：

```
resources/
├── node/
│   ├── bin/node       # macOS：内置 Node arm64
│   └── node.exe       # Windows：内置 Node x64
├── npm/               # 内置 npm（更新用）
├── lan-proxy.js       # 局域网转发器
└── dsh/
    ├── current        # 版本标记文件（内容 = 当前版本目录名）
    └── v<版本>/        # 该版本完整闭包（node_modules 全量 + VERSION 标记）
```

**更新机制**：内置 `resources/` 只读作基线；每次更新把新闭包经内置 npm 装入 app 数据目录 `dsh/v<新>-tmp` → 自检（版本号 + web profile 组合双重校验）→ 发布为 `v<新>` → 原子切换 `current` 版本标记文件 → 重启 dsh。保留上一版本用于回滚，更旧版本自动清理。失败安全：切换前任何失败都不动当前版本。`current` 用普通文本文件而非软链，Windows 无软链权限也能工作。

**局域网访问**：dsh 出于安全只绑 `127.0.0.1`。转发器监听局域网，带令牌登录页（cookie 30 天），把 HTTP/WebSocket 转发给本机 dsh，从 dsh 视角一切连接均来自本机（无需 `--trusted-host`，安全模型不变）。转发时剥离 `origin`/`sec-fetch-site`/`referer` 以通过 dsh 的 /api 信任篱笆，并向主文档注入 `crypto.randomUUID` polyfill（明文 HTTP 下该安全上下文 API 不可用）；启动 dsh 时设 `SSH_CONNECTION=1` 启用网页版目录浏览器。

**app 数据目录**（卸载时整个删除；macOS 为 `~/Library/Application Support/…`，Windows 为 `%APPDATA%\…`）：

```
/…/com.dsh-desktop.app/
├── settings.json       # 偏好（见下）
├── logs/               # launcher.log + dsh.log（打包态）
└── dsh/                # 更新闭包（current 标记 + 当前/上一版本 + npm-cache）
```

**settings.json 字段**：

| 字段 | 类型 | 说明 |
|---|---|---|
| `default_cwd` | string(路径) | dsh 进程默认工作目录（兜底 cwd）；设置默认工作目录… 写入；无效时回退 `$HOME` |
| `registry` | string(URL) | npm registry 换源，如 `https://registry.npmmirror.com`；缺省官方 npmjs |
| `lan_enabled` | bool | 局域网访问开关（默认 false） |
| `lan_port` | number | 局域网转发器端口（默认 3190） |
| `lan_token` | string | 局域网访问令牌（首次开启生成，128-bit 随机） |

**CLI 测试钩子**（无 GUI，用于自检/自动化；macOS 需从 .app 包内运行）：

```
dsh-desktop --self-update-check            # UP_TO_DATE / UPDATE_AVAILABLE
dsh-desktop --self-apply-update <ver>      # 安装+自检+切换 → APPLIED / APPLY_ERROR
dsh-desktop --self-login-item on|off       # 登录自启开关（macOS plist / Windows 注册表）
dsh-desktop --self-uninstall-test          # 卸载 teardown（删数据/登录项，不删自身）
dsh-desktop --self-trash-test              # 把自身移入废纸篓/回收站（打包态运行）
```

### 4.4 安全与边界

- **只绑 loopback**：dsh 强制 `127.0.0.1`（`--host 0.0.0.0` 被 dsh 自身拒绝）。
- **端口无冲突**：`--port 0` 随机分配，由 stdout 回传。
- **数据本地**：凭据/会话在 `~/.dsh`（Windows 为 `%USERPROFILE%\.dsh`），App 不额外落盘敏感数据；日志本地，不上传。
- **更新可信**：npm 自动校验包 `dist.integrity`（sha512）；切换前自检，失败不动当前版本。
- **WebView CSP**：最小策略（`default-src 'self'` + 允许连 127.0.0.1）；dsh 页面为外部 localhost，CSP 只约束内置资产页。
- **局域网门禁**：令牌是唯一门禁（跨平台 CSPRNG 生成）；dsh 仍只绑本机；只在可信网络开启。
- **已知限制**：App 未签名（个人使用）——macOS 首次打开需右键→打开；Windows SmartScreen 可能提示"未知发布者"，点"仍要运行"即可。如需分发可后续补签名/公证。

---

## 5. License

[MIT](LICENSE)，与上游 [deepseek-harness](https://github.com/deepseek-ai/deepseek-harness) 一致。
