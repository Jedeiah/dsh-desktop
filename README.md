# DeepSeek Harness 桌面端（DSh Desktop）

一个 **macOS 桌面 App**：双击即用，把官方 DeepSeek Harness（`dsh web`）装进一个 Mac App 里，带托盘、自动更新、干净卸载，还能让同一局域网里的手机/平板通过浏览器连进来用。

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
| macOS 原生 App | 菜单栏托盘、标准退出、Dock 常驻，符合 Mac 使用习惯 |
| 双击即用 | 内置 Node v24 + dsh 完整闭包（含平台原生预编译），**不依赖系统 bun / npm / node** |
| 局域网可连 | 可选开启令牌鉴权转发器，同一 WiFi 下手机/平板浏览器输令牌即可操作工作台 |
| 自动更新 | 跟随上游 `@deepseek-ai/dsh`，原子切换 + 失败安全 |
| 干净卸载 | 连带内置 dsh、应用数据、缓存、自启项、Dock 残留一并清理 |

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

1. 双击 **DeepSeek Harness_<版本>_aarch64.dmg**，把 **DeepSeek Harness.app** 拖进 **应用程序**。
2. 首次打开：**右键 → 打开**（未签名，需确认一次），之后正常双击即可。

### 3.2 首次使用

启动后自动拉起内置 dsh web → 窗口显示工作台（即 dsh web UI）。会话/凭据与终端 dsh 共用同一份 `~/.dsh`——你在终端建过的会话，这里直接能看到。

### 3.3 日常操作

| 操作 | 方式 |
|---|---|
| 显示/隐藏主窗口 | 左键点托盘图标，或点 Dock 图标，或托盘 *显示主窗口* |
| 关闭窗口 | 隐藏到托盘（App 与 dsh 继续后台运行，不退出） |
| 用系统浏览器打开 | 托盘 *在浏览器中打开* |
| 退出 | `Cmd+Q` 或托盘 *退出*（连带结束 dsh，无残留） |
| 崩溃自愈 | dsh 意外退出自动重启（1s→2s→…→15s 退避）；连续 5 次后停止并提示查看日志 |

### 3.4 局域网访问（手机/平板）

1. Mac 连上家里 WiFi，托盘勾选 **局域网访问**（默认关闭）。
2. 弹窗显示 **地址**（`http://<Mac局域网IP>:3190`）与 **访问令牌**（可一键复制）。
3. 手机/平板连同一 WiFi，浏览器打开地址，输入令牌，即可进入工作台（30 天免登录）。
4. 随时用托盘 *显示局域网访问信息* 查看地址与令牌；取消勾选即关闭。

> ⚠️ 令牌是唯一门禁（128-bit 随机）。知道令牌 ≈ 能操作你 Mac 上的 dsh。**只在可信网络开启，令牌不要外传。**

### 3.5 更新

- 托盘 *检查更新…* 手动检查；App 每 24h 自动静默检查。
- 有新版 → 确认 → 自动安装并重启 dsh，窗口自动刷新为新版工作台。

### 3.6 登录自启

托盘 *登录时启动*（勾选，默认关闭）→ 下次登录时自动启动 App。只影响本 App，不碰其他应用的启动设置。

### 3.7 卸载

托盘 *卸载 DeepSeek Harness…* → 三键弹窗：

| 按钮 | 效果 |
|---|---|
| 取消 | 什么都不做 |
| 卸载（保留 ~/.dsh） | 卸载，保留会话/凭据（推荐） |
| 卸载并删除 ~/.dsh | 卸载，连会话/凭据一起删（不可恢复） |

卸载会：结束 dsh 与转发器 → 删除登录自启项、应用数据、WebView 缓存 → 把 App 移入废纸篓（可恢复）→ 清理 Dock 最近使用 → 退出。

### 3.8 日志与故障排查

- 托盘 *打开日志* 直接打开日志目录：`launcher.log`（启动器）+ `dsh.log`（dsh 运行输出）。
- dsh 启动失败 / 连续崩溃时，App 会弹窗给出日志位置。
- 常见问题：
  - **首次打不开** → 右键 → 打开（未签名）。
  - **局域网连不上** → 确认同一 WiFi、Mac 未开代理、令牌输入正确。

---

## 4. 技术细节（构建 / 架构）

> 这一节给想自己折腾的人看。日常使用不需要。

### 4.1 构建与运行

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

> 首次编译较慢（Tauri 依赖树）。打包态日志落 `<app-data>/logs/`，dev 模式直接输出到终端。

**资源来源**：Node 从 fnm 安装（v24.14.0 arm64）拷贝；dsh 闭包为 `@deepseek-ai/dsh` 的完整 `node_modules`（含平台原生预编译，运行期零编译）；图标为 `icons/icon.png`（RGBA 1024）+ `icon.icns`。

### 4.2 目录结构

```
dsh-desktop/
├── README.md                    # 本文档
├── scripts/
│   └── prepare-resources.sh     # 打包 node + dsh 闭包 + 图标
└── apps/desktop/
    ├── ui/index.html            # 前端占位（实际窗口指向内置 dsh localhost）
    └── src-tauri/
        ├── Cargo.toml           # tauri2(tray-icon,image-png) + serde + rfd + ureq
        ├── tauri.conf.json      # identifier / bundle.resources / CSP / dmg
        ├── icons/               # icon.png(RGBA 1024) + icon.icns
        ├── resources/           # 内置 node + npm + lan-proxy + dsh 闭包（只读基线，gitignore）
        └── src/
            ├── main.rs          # 启动器：路径/闭包解析、spawn/boot、托盘、更新、卸载、日志、LAN 控制
            ├── update.rs        # 更新子系统：registry、semver、内置 npm 安装、原子切换、版本清理
            └── lan-proxy.js     # 局域网转发器（令牌鉴权 + HTTP/WebSocket 透传）
```

### 4.3 架构

**进程模型**：App 单个进程（Rust/Tauri），作为壳拉起一个 dsh web 子进程（内置 node 运行 `dsh --profile web --port 0`），由 Rust 托管（stdout 解析就绪、退出回收、崩溃重启、退出时连带结束）。局域网模式另起一个转发器子进程（内置 node 运行 `lan-proxy.js`）。

**内置资源（只读基线）**：

```
DSh.app/Contents/Resources/resources/
├── node/bin/node       # 内置 Node arm64
├── npm/                # 内置 npm（更新用）
├── lan-proxy.js        # 局域网转发器
└── dsh/
    ├── current         # 软链 → 当前激活版本
    └── v<版本>/        # 该版本完整闭包（node_modules 全量 + VERSION 标记）
```

**更新机制**：内置 `resources/` 只读作基线；每次更新把新闭包经内置 npm 装入 app 数据目录 `dsh/v<新>-tmp` → 自检（版本号 + web profile 组合双重校验）→ 发布为 `v<新>` → 原子切换 `current` 软链 → 重启 dsh。保留上一版本用于回滚，更旧版本自动清理。失败安全：切换前任何失败都不动当前版本。

**局域网访问**：dsh 出于安全只绑 `127.0.0.1`。转发器监听局域网，带令牌登录页（cookie 30 天），把 HTTP/WebSocket 转发给本机 dsh，从 dsh 视角一切连接均来自本机（无需 `--trusted-host`，安全模型不变）。转发时剥离 `origin`/`sec-fetch-site`/`referer` 以通过 dsh 的 /api 信任篱笆，并向主文档注入 `crypto.randomUUID` polyfill（明文 HTTP 下该安全上下文 API 不可用）；启动 dsh 时设 `SSH_CONNECTION=1` 启用网页版目录浏览器。

**app 数据目录**（卸载时整个删除）：

```
~/Library/Application Support/com.dsh-desktop.app/
├── settings.json       # 偏好（见下）
├── logs/               # launcher.log + dsh.log（打包态）
└── dsh/                # 更新闭包（current + 当前/上一版本 + npm-cache）
```

**settings.json 字段**：

| 字段 | 类型 | 说明 |
|---|---|---|
| `default_cwd` | string(路径) | dsh 进程默认工作目录（兜底 cwd）；设置默认工作目录… 写入；无效时回退 `$HOME` |
| `registry` | string(URL) | npm registry 换源，如 `https://registry.npmmirror.com`；缺省官方 npmjs |
| `lan_enabled` | bool | 局域网访问开关（默认 false） |
| `lan_port` | number | 局域网转发器端口（默认 3190） |
| `lan_token` | string | 局域网访问令牌（首次开启生成，128-bit 随机） |

**CLI 测试钩子**（无 GUI，用于自检/自动化）：

```
dsh-desktop --self-update-check            # UP_TO_DATE / UPDATE_AVAILABLE
dsh-desktop --self-apply-update <ver>      # 安装+自检+切换 → APPLIED / APPLY_ERROR
dsh-desktop --self-login-item on|off       # 登录自启开关（plist 层）
dsh-desktop --self-uninstall-test          # 卸载 teardown（删数据/登录项，不删自身）
dsh-desktop --self-trash-test              # 把当前 .app 移入废纸篓（需从包内运行）
```

### 4.4 安全与边界

- **只绑 loopback**：dsh 强制 `127.0.0.1`（`--host 0.0.0.0` 被 dsh 自身拒绝）。
- **端口无冲突**：`--port 0` 随机分配，由 stdout 回传。
- **数据本地**：凭据/会话在 `~/.dsh`，App 不额外落盘敏感数据；日志本地，不上传。
- **更新可信**：npm 自动校验包 `dist.integrity`（sha512）；切换前自检，失败不动当前版本。
- **WebView CSP**：最小策略（`default-src 'self'` + 允许连 127.0.0.1）；dsh 页面为外部 localhost，CSP 只约束内置资产页。
- **局域网门禁**：令牌是唯一门禁；dsh 仍只绑本机；只在可信网络开启。
- **已知限制**：App 未签名（个人使用），首次打开需右键→打开；如需分发可后续补签名/公证。
