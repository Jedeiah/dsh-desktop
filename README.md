# DeepSeek Harness Desktop 桌面端（DSh Desktop）

<p align="center">
  <img src="apps/desktop/src-tauri/icons/icon-rounded-256.png" alt="DeepSeek Harness 图标" width="128">
</p>

一个 **macOS / Windows 桌面 App**：双击即用，把官方 DeepSeek Harness（`dsh web`）装进一个桌面 App 里。App 是**瘦壳**——不内置 dsh，首次运行自动安装；带 dsh 版本管理、App 内更新、插件管理、托盘与干净卸载。

---

## 1. 项目介绍

**它是什么**：一个**壳**（thin shell）——只内置 Node 运行时 + npm + pnpm（安装包约 50MB），**不内置 dsh 闭包**。首次运行自动把官方 `@deepseek-ai/dsh` 从 npm registry 安装到 App 数据目录，之后在窗口里运行的就是**官方 dsh 工作台**，和你在终端跑 `dsh web` 完全一样。壳只做四件事：

1. **免终端启动** dsh web（首次运行自动安装）
2. **dsh 生命周期管理**：安装、更新、指定版本安装、回滚
3. **App 自身更新**：检查 → 一键下载安装
4. **插件管理**：展示已装插件、安装/卸载（壳对 dsh「万物皆插件」哲学唯一的服务面）

**它不是**：不是二次开发、不加任何插件、不改官方行为。保持"壳"的定位，是为了**最大自由度**：
- 官方怎么配你就怎么配（`~/.dsh` 配置/会话/凭据与终端 dsh **完全共用**）
- dsh 内部行为（profile、agent、插件生态）全部留给 dsh 自己，壳不掺和
- 想装插件，直接走官方 dsh 的机制即可（壳页提供列表与装/卸入口）

**关键特性**：
| 特性 | 说明 |
|---|---|
| 跨平台原生 App | macOS：菜单栏托盘 / Dock；Windows：系统托盘 / 任务栏，符合各平台使用习惯 |
| 瘦壳、首次自动装 dsh | 不内置 dsh 闭包（安装包约 50MB）；首次运行引导安装（默认 latest，可指定版本），**不依赖系统 bun / npm / node** |
| dsh 版本管理 | dsh 页查看当前版本与全部版本：一键更新到最新、指定版本安装、回滚到上一版本；registry 源可配 |
| App 内更新 | 关于页检查 GitHub Releases 新版 → 下载（校验）→ 自动安装 → 重启新版本 |
| 插件管理 | 插件页列出已装插件（`~/.dsh/profiles/web`），安装/卸载后自动重启工作台生效 |
| 干净卸载 | 两档：保留 `~/.dsh` / 连会话凭据一起删；Windows 走"唯一卸载链"彻底清理 |

> 平台差异：`~/.dsh`（配置/会话/凭据）在 macOS 是 `~/.dsh`，在 Windows 是 `%USERPROFILE%\.dsh`，与终端 dsh 完全共用。

---

## 2. 亮点

- **电脑上无需安装任何环境**：不需要装 Node、npm、bun、Python、Rust 或任何运行时——Node/npm/pnpm 都内置在 App 里，dsh 由内置 npm 自动安装，删掉系统里的开发环境也不影响它运行。
- **零偏差**：壳不注入任何东西，跑的就是官方 dsh，`~/.dsh` 无缝复用，随时可回到终端使用同一份数据。
- **dsh 版本随心换**：预览期上游迭代极快（`latest` 标签管理不可靠）——支持指定版本安装与回滚；当前版本与上一版本各保留一份，失败不影响当前可用版本。
- **标准 Mac 体验**：关窗口隐藏进托盘、Cmd+Q 连带结束 dsh 无孤儿、崩溃自动重启（退避 5 次后提示日志路径）。
- **干净利落**：单实例（不会开双托盘）、卸载一步到位（含 WebView 缓存与 Dock 最近使用；Windows 自系统「设置/右键」触发同样彻底卸载）。

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
1. 双击 **DeepSeek Harness Desktop_<版本>_aarch64.dmg**，把 **DeepSeek Harness.app** 拖进 **应用程序**。
2. 首次打开：**右键 → 打开**（未签名，需确认一次），之后正常双击即可。
3. 若提示"已损坏，无法打开"（Chrome 下载的未签名 App 常见）：`xattr -dr com.apple.quarantine "/Applications/DeepSeek Harness Desktop.app"`

**Windows 手动安装：**
- 下载 Release 里的 `DeepSeek.Harness.Desktop_<版本>_x64-setup.exe` 双击安装（无需管理员权限，装到当前用户）。
- 或下载 `DeepSeek-Harness-Desktop-Windows-x64.zip` 解压后双击 `dsh-desktop.exe` 直接运行（便携版，同样内置 node + npm + pnpm）。

### 3.2 首次使用

首次启动（检测到尚未安装 dsh）会打开**引导页**（不启动 dsh 进程）：

1. 默认**安装最新版（latest）**，点「安装」开始：内置 npm 下载安装 → 双重自检 → 原子切换 → 自动进入工作台。
2. 可展开**高级选项**选择 Registry 源与**指定版本**（版本列表来自 registry，按 semver 倒序）。
3. 安装中可随时**取消**（不影响任何数据）；失败/断网时显示错误与**重试**按钮，提示检查网络。
4. 安装完成后自动启动 dsh web → 窗口显示工作台（即 dsh web UI）。

会话/凭据与终端 dsh 共用同一份数据目录——macOS 为 `~/.dsh`，Windows 为 `%USERPROFILE%\.dsh`——你在终端建过的会话，这里直接能看到。

### 3.3 日常操作

壳页共 **4 个 Tab**：**工作台 / dsh / 插件 / 关于**；`⌘K`（Windows `Ctrl+K`）循环切换 Tab，`Esc` 返回工作台（焦点位于工作台 iframe 内时快捷键由 dsh 工作台自身响应，属预期）。

| 操作 | macOS | Windows |
|---|---|---|
| 显示/隐藏主窗口 | 左键点托盘图标，或点 Dock 图标 | 双击/单击托盘图标，或点任务栏图标 |
| 关闭窗口 | 隐藏到托盘 | 隐藏到托盘（App 与 dsh 继续后台运行） |
| 窗口位置 | 主窗隐藏创建、首次显示即居中（已可见时不再重定位，避免启动"闪一下"）；自绘弹窗相对主窗中心显示（无系统标题栏按钮，✕/Esc 关闭） | 同左（弹窗在多显示器下自动避让到工作区） |
| 打开外链 | 工作台内点击外链（https 等）自动在系统浏览器打开 | 同左 |
| 退出 | `Cmd+Q` 或托盘 *退出* | 托盘 *退出*（连带结束 dsh，无残留） |
| 崩溃自愈 | dsh 意外退出自动重启（1s→2s→…→15s 退避）；连续 5 次后停止并弹窗提示日志路径 | 同左 |
| dsh 版本管理 | 壳页 *dsh* Tab（详见 3.4） | 同左 |
| 插件管理 | 壳页 *插件* Tab（详见 3.5） | 同左 |
| App 更新 | 壳页 *关于* Tab（详见 3.6） | 同左 |
| 卸载 | 壳页 *关于* Tab（详见 3.7） | 同左 |

> 管理功能全部集中在主窗壳页 Tab，托盘仅保留 **显示主窗口 / 管理台 / 退出** 三项（管理台 = 切到 *dsh* Tab）。

### 3.4 dsh 版本管理（dsh 页）

- **当前版本**：显示正在运行的 dsh 版本；启动时静默检查 registry `latest`，有新版时在 dsh 页提示（**不自动安装**）。
- **更新到最新**：一键安装 `latest` → 自检 → 原子切换 → 工作台自动重启为新版。
- **指定版本安装**：版本列表（含全部版本，按 semver 倒序）点选 → 确认 → 安装（同一路径）。
- **回滚**：在版本列表选择上一版本安装即可（当前/上一版本始终各保留一份）。
- **Registry 源**：可配置（默认官方 npmjs；国内可换 `https://registry.npmmirror.com`），写入 `settings.json`。
- 任何安装/更新失败都不影响当前可用版本（tmp 清理、版本标记不动）。

### 3.5 插件管理（插件页）

- **插件列表**：进入插件页自动读取 `~/.dsh/profiles/web` 已装插件（名称/版本/状态）。
- **安装**：输入包名（如 `@linxin666/dsh-web-ui-all`）→ 输出区**实时滚动**显示 pnpm 进度（下载、依赖解析、构建脚本等）→ 完成后退出码与结果。
- **卸载**：列表中选择 → 卸载。
- 安装/卸载写入 `~/.dsh/profiles/web`（与终端 dsh 完全共用）；**完成后自动重启工作台生效**（无需手动操作）。

**内置 pnpm，零环境依赖**：App 打包 pnpm 运行库 + 启动器，安装/卸载不需要你装 Node/pnpm。自动处理 pnpm 11 门禁（构建脚本授权 `allowBuilds`、新包发布年龄 `minimumReleaseAge: 0`），遇到被忽略的构建脚本会解析包名自动补授权重试。卸载后自动清扫残留空目录。仅限壳页调用，工作台页面无法触发。

### 3.6 App 更新（关于页）

- 壳页 **关于** Tab 点 **检查更新**（检查 `github.com/Jedeiah/dsh-desktop/releases/latest`，失败静默提示）。
- 有新版 → 点 **下载并安装**：下载安装包到临时目录（校验大小）→ macOS 挂载 DMG 复制到 `/Applications`（弹系统授权）→ 自动重启新版本；Windows 静默运行 NSIS 安装器（`/S`）→ 退出 → 由安装器启动新版。
- **macOS x86_64 兜底**：CI 仅构建 arm64 产物，x86_64 无安装包 → 点 **在浏览器打开下载页** 手动下载安装。
- 下载/安装失败不影响当前版本，可重试或走手动下载。

### 3.7 卸载

壳页 **关于** Tab 的 *卸载* 区（macOS/Windows 一致，中文两选项）：

| 按钮 | 效果 |
|---|---|
| 仅卸载应用 | 卸载，保留 `~/.dsh` 配置与数据（推荐，便于将来重装） |
| 卸载并删除 ~/.dsh | 卸载，连会话/凭据一起删（不可恢复） |

卸载会：结束 dsh 子进程 → 删除应用数据、WebView 缓存 → 把 App 移入废纸篓 / 回收站（可恢复）→ 退出。Windows 上走「**唯一卸载链**」：数据侧在本 App 内完成清理后，自动唤起系统卸载器（`uninstall.exe`）删除程序文件——`uninstall.exe` 内置了同款完整清理（先结束运行中的 dsh 进程树释放文件锁，再清数据），因此**从「设置 → 应用」或开始菜单右键触发的卸载一样是彻底卸载**。个别被占用的数据目录会自动重试，仍失败会提示重启电脑后清理，卸载本身不被阻塞。

### 3.8 故障排查

- dsh 启动失败 / 连续崩溃（5 次）时，App 会弹出自绘弹窗并给出日志位置：macOS 为 `~/Library/Application Support/com.dsh-desktop.app/logs`，Windows 为 `%APPDATA%\com.dsh-desktop.app\logs`（`launcher.log` + `dsh.log`）。
- 常见问题：
  - **macOS 首次打不开** → 右键 → 打开（未签名）。
  - **Windows 白屏/黑窗** → 确认系统装有 WebView2 运行时（Win10/11 自带；老系统需安装 [WebView2 Runtime](https://developer.microsoft.com/en-us/microsoft-edge/webview2/)）。
  - **首次引导装不上 dsh** → 检查网络/registry 源（可换 npmmirror），点重试；断网时引导页会明确提示。

---

## 4. 技术细节（构建 / 架构）

> 这一节给想自己折腾的人看。日常使用不需要。

### 4.1 构建与运行

**macOS：**
```bash
# 前置：Rust 工具链 + tauri-cli（仅打包需要）
cargo install tauri-cli --version "^2"

# 准备内置资源（node + npm + pnpm；瘦壳不含 dsh 闭包）
scripts/prepare-resources.sh
#   NODE_SRC=<node二进制>  默认取 fnm 安装的 node v24；npm 随 node 目录分发
#   pnpm 由脚本临时安装（pnpm@^11 JS 发行版 + shim）后拷入 resources/pnpm-bin

# 开发运行（终端可见 dsh 日志）
cd apps/desktop/src-tauri && cargo run

# 发布构建（产出 .app + DMG）
cargo tauri build
#   产物：target/release/bundle/macos/DeepSeek Harness.app
#        target/release/bundle/dmg/DeepSeek Harness Desktop_<版本>_aarch64.dmg
```

**Windows**（需在 Windows 机器或 Windows CI 上构建）：
```powershell
# 前置：Rust 工具链（VS Build Tools）+ tauri-cli
cargo install tauri-cli --locked

# 准备内置资源（node.exe + npm + pnpm）
.\scripts\prepare-resources.ps1 -NodeSrc (Get-Command node).Source

# 发布构建（产出 NSIS 安装器；加 --bundles msi 得到 MSI）
cd apps/desktop/src-tauri
cargo tauri build --bundles nsis
#   产物：target/release/bundle/nsis/DeepSeek Harness Desktop_<版本>_x64-setup.exe
#        （+ target/release/dsh-desktop.exe 与 resources/ 即为便携版）
```

> 首次编译较慢（Tauri 依赖树）。打包态日志落 `<app-data>/logs/`，dev 模式直接输出到终端。

**资源来源**：Node 从 fnm 安装（macOS v24.14.0 arm64；Windows 用官方 x64 node.exe）拷贝；npm 随 node 安装目录分发；pnpm 用 `pnpm@^11` 的 JS 发行版（bin + dist + 自建 shim，比 SEA 二进制省约 130MB）；图标为 `icons/icon.png`（RGBA 1024）+ `icon.icns`（macOS）+ `icon.ico`（Windows）。**不再打包 dsh 闭包与 LAN 资源**——dsh 由 App 首次运行/更新时经内置 npm 装入 app 数据目录。

### 4.2 目录结构

```
dsh-desktop/
├── README.md                    # 本文档
├── scripts/
│   ├── prepare-resources.sh     # macOS：打包 node + npm + pnpm（+ 图标）
│   ├── prepare-resources.ps1    # Windows：同名 PowerShell 版本
│   ├── install.sh               # macOS 一键安装
│   └── install.ps1              # Windows 一键安装
├── docs/
│   ├── regression-checklist.md  # 手工回归清单（发版前过一遍）
│   └── superpowers/specs/…      # 设计文档（瘦壳重设计 v0.3）
└── apps/desktop/
    ├── ui/                      # 内置资产页（tauri://localhost，零构建链）
    │   ├── theme.css            # 共享设计系统（颜色/圆角/字体/按钮/弹窗 token）
    │   ├── shell.html/.js       # 壳页（主窗）：工作台/dsh/插件/关于 4 Tab + 首次引导视图
    │   ├── modal.html/.js       # 自绘弹窗（替代 rfd 系统对话框：启动失败/崩溃/更新确认）
    │   └── icon.png             # 壳页图标
    └── src-tauri/
        ├── Cargo.toml           # tauri2(tray-icon,image-png) + serde + serde_json + ureq + dirs（unix: libc；windows: trash）
        ├── tauri.conf.json      # identifier / bundle.resources / CSP / withGlobalTauri / nsis(installerHooks) / dmg
        ├── installer-hooks.nsh  # NSIS 卸载钩子：PREUNINSTALL 调 --self-uninstall-full（完全卸载）
        ├── icons/               # icon.png(RGBA 1024) + icon.icns(macOS) + icon.ico(Windows)
        ├── resources/           # 内置 node + npm + pnpm-bin（只读基线，gitignore）
        └── src/
            ├── main.rs          # 启动器：路径/设置、spawn/boot、崩溃自愈、托盘、单实例、卸载、CLI hooks
            ├── dsh.rs           # 闭包管理：首次安装/版本安装/切换/回滚/GC/取消（从 update.rs 演化）
            ├── registry.rs      # npm registry 查询：latest、版本列表（可配源）、semver 比较
            ├── plugin.rs        # 插件管理：列表读取 + 安装/卸载（内置 pnpm + 门禁处理）
            └── appupdate.rs     # App 更新：GitHub Releases 检查 + 下载 + 安装
```

### 4.3 架构

**进程模型**：App 单个进程（Rust/Tauri），作为壳拉起一个 dsh web 子进程（内置 node 运行 `<app-data>/dsh/v<版本>/node_modules/@deepseek-ai/dsh/lib/bin.js --profile web --port 0`），由 Rust 托管（stdout 解析就绪 URL、退出回收、崩溃重启、退出时连带结束）。dsh 的会话/凭据在 `~/.dsh`，闭包本体归 App 管理——与终端全局安装互不干扰。

**环境一致性（macOS）**：App 由 Finder/launchd 启动，继承的是系统最小环境（PATH 只有系统目录）。启动 dsh 前会自动捕获一次用户登录交互 shell 的环境（PATH/LANG 等，含 fnm / Homebrew / bun 注入的路径），合并进 dsh 子进程——app 工作台里执行命令的环境与电脑终端一致。捕获超时或失败时静默沿用原环境，不影响启动；修改 shell 配置文件后重启 App 生效。

**内置资源（只读基线）**：

macOS（`DSh.app/Contents/Resources/resources/`）、Windows（`<exe 旁>/resources/`）：

```
resources/
├── node/
│   ├── bin/node       # macOS：内置 Node arm64
│   └── node.exe       # Windows：内置 Node x64
├── npm/               # 内置 npm（dsh 安装/更新用）
└── pnpm-bin/          # 内置 pnpm（JS 发行版 + shim，插件管理用）
```

**dsh 闭包管理与更新机制**（`dsh.rs`，通用版本管理）：

- 闭包装在 app 数据目录 `<app-data>/dsh/`：`current`（纯文本版本标记文件，Windows 无软链权限也能工作）+ `v<版本>/`（该版本完整闭包 + VERSION 标记）+ `npm-cache/`（npm 缓存，不泄漏到 `~/.npm`）。
- 每次安装：内置 npm 装入 `v<新版本>-tmp` → **双重自检**（`--version` + `--profile web --dump-default-config` 组合）→ 写 VERSION 标记 → 发布为 `v<新版本>` → **原子切换** `current` 标记（旧目录先移开再 rename，失败恢复）→ 清理 tmp → 重启工作台。
- **GC**：当前版本与上一版本始终各保留一份（约 300MB × 2，用于回滚）；更旧版本自动清理。
- **失败安全**：切换前任何失败都不动当前版本；安装中可取消（SIGTERM / taskkill 子进程）。

**插件管理**（`plugin.rs`）：壳页「插件」Tab（`shell.html`），经 `plugin_op`/`plugin_list_cmd` command 读写 `~/.dsh/profiles/web`（`dsh plugin --profile web add|remove <包名>`）：内置 pnpm（`resources/pnpm-bin`，PATH 前置）运行安装，stdout/stderr 逐行 `emit` 实时回显；安装前自动写入 profile 的 `pnpm-workspace.yaml` 门禁配置（`allowBuilds` + `minimumReleaseAge: 0`），`ERR_PNPM_IGNORED_BUILDS` 时解析包名自动补授权重试；卸载后清扫残留空目录。装/卸完成后自动重启工作台。命令仅接受壳页调用（window label 校验），插件操作全局串行锁保护。**工作台在 iframe 内，拿不到 `window.__TAURI__`，远程内容无法触发插件操作（比 label 白名单更安全）。**

**app 数据目录**（卸载时整个删除；macOS 为 `~/Library/Application Support/…`，Windows 为 `%APPDATA%\…`）：

```
/…/com.dsh-desktop.app/
├── settings.json       # 偏好（见下）
├── logs/               # launcher.log + dsh.log（打包态）
└── dsh/                # 闭包管理（current 标记 + v<版本>/ 闭包 + npm-cache）
```

**settings.json 字段**：

| 字段 | 类型 | 说明 |
|---|---|---|
| `registry` | string(URL) | npm registry 换源，如 `https://registry.npmmirror.com`；缺省官方 npmjs |

**CLI 测试钩子**（无 GUI，用于自检/自动化；macOS 需从 .app 包内运行）：

```
dsh-desktop --self-update-check            # UP_TO_DATE / UPDATE_AVAILABLE
dsh-desktop --self-apply-update <ver>      # 安装+自检+切换 → APPLIED / APPLY_ERROR
dsh-desktop --self-uninstall-test          # 卸载 teardown（删数据，不删自身）
dsh-desktop --self-uninstall-full [--wipe] # 完全卸载 sidecar（结束实例+子进程、清数据；供 NSIS 钩子调用，不删程序文件）
dsh-desktop --self-trash-test              # 把自身移入废纸篓/回收站（打包态运行）
```

### 4.4 安全与边界

- **只绑 loopback**：dsh 强制 `127.0.0.1`（`--host 0.0.0.0` 被 dsh 自身拒绝）。
- **端口无冲突**：`--port 0` 随机分配，由 stdout 回传。
- **数据本地**：凭据/会话在 `~/.dsh`（Windows 为 `%USERPROFILE%\.dsh`），App 不额外落盘敏感数据；日志本地，不上传。
- **安装可信**：dsh 安装走 npm（校验 `dist.integrity` sha512）；切换前双重自检，失败不动当前版本。App 更新校验下载大小与 Release asset 一致，不符即失败清理。
- **WebView CSP**：最小策略（`default-src 'self'` + 允许连 127.0.0.1）；dsh 页面为外部 localhost，CSP 只约束内置资产页。
- **并发**：单实例；安装/更新期间串行锁，避免重复操作。
- **已知限制**：App 未签名（个人使用）——macOS 首次打开需右键→打开；Windows SmartScreen 可能提示"未知发布者"，点"仍要运行"即可。如需分发可后续补签名/公证。

---

## 5. License

[MIT](LICENSE)，与上游 [deepseek-harness](https://github.com/deepseek-ai/deepseek-harness) 一致。
