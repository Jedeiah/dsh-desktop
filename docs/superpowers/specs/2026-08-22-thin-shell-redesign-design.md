# DSh Desktop 瘦壳重设计（v0.3 方向）

日期：2026-08-22
状态：已与产品负责人（用户）逐节确认并批准

## 1. 背景与问题

**现状**（v0.2.1）：DSh Desktop 是 Tauri 2 壳应用，内置 node v24 + npm + pnpm + 完整 dsh 闭包（约 340MB），安装包约 400MB。壳页含 6 个 Tab：工作台 / 常规 / 网络（局域网扫码远程连接）/ 插件 / 更新 / 卸载。

**上游事实**（2026-08-22 核实）：

- dsh（`@deepseek-ai/dsh`）处于 developer preview，迭代极快：2026-08-13 至 08-21 共发布 10 个版本（0.0.1-rc.1 → 0.1.1-rc.2）。
- npm `latest` dist-tag 当前为 `0.1.1-rc.2`；预览期 tag 管理不可靠，存在「latest 不是最新」的风险，因此必须支持**指定版本安装**。
- 官方 CLI 无任何更新机制（deepseek-ai/deepseek-harness Discussion #2535 承认，并点名第三方 launcher——含 dsh-desktop——填补此缺口，证明需求真实）。

**问题**：

1. 产品定位偏离：加入了局域网远程连接等与「壳」定位无关的功能。
2. 内置 340MB 闭包导致：安装包巨大；内置版本必然快速过时（当前内置 0.1.0-rc.7，上游已 0.1.1-rc.2），用户为获得新 dsh 被迫更新整个 App。
3. dsh 更新只能跟随 `latest`，无法指定版本安装（预览期必要能力缺失）。

## 2. 产品定位与设计原则

**DSh Desktop = dsh 的桌面壳（thin shell）**。dsh 的哲学是「万物皆插件」，壳不掺和 dsh 内部。壳只做四件事：

1. **免终端启动** dsh web（首次运行自动把 dsh 装好）
2. **dsh 生命周期管理**：安装、更新、指定版本安装、切换、回滚
3. **App 自身更新**：检查 → 一键下载安装
4. **插件管理**：展示已装插件、安装/卸载（壳对「万物皆插件」唯一的服务面）

**设计原则**：

- **瘦壳**：App 不内置 dsh 闭包，只内置 node + npm + pnpm（约 50MB 安装包）。dsh 按需从 npm registry 安装到 App 数据目录。
- **轻量**：安装包小；运行时自包含（不依赖主机 node/npm/bun/python 等任何环境）；壳页零构建链、零前端依赖。
- **不越界**：工作目录、登录自启、日志查看、远程连接等一律砍掉；dsh 内部行为（profile、agent、插件哲学）全部留给 dsh 自己。
- **数据共用**：`~/.dsh`（会话/凭据/配置）与终端 dsh 完全共用，随时可回到终端使用同一份数据；dsh 运行时闭包则归 App 管理（与终端全局安装互不干扰）。
- **失败安全**：任何安装/更新失败都不影响当前可用版本；`~/.dsh` 永不自动删除。
- **预览期稳妥**：dsh 更新默认「提示确认，手动更新」，不自动安装。

## 3. 功能清单

### 3.1 保留（复用/改造）

| 能力 | 处置 |
|---|---|
| 启动 dsh web 进程（spawn_dsh、端口探测、窗口显示） | 保留 |
| 崩溃自愈（1s→15s 退避，5 次后停止并提示日志路径） | 保留（日志 UI 砍掉，仅崩溃提示给路径） |
| 托盘（显示主窗口 / 管理台 / 退出） | 保留 |
| 单实例 | 保留 |
| 卸载（保留 `~/.dsh` / 全删 两档，干净卸载） | 保留 |
| dsh 闭包安装与更新机制（内置 npm 安装 → 双重自检：`--version` + `--profile web --dump-default-config` → 原子切换 `current` 标记 → 保留上一版本回滚 → GC 旧版本） | 复用并改造为「通用版本管理」 |
| 插件安装/卸载（内置 pnpm、allowBuilds/minimumReleaseAge 门禁处理、实时输出、残留清扫） | 复用，UI 重做 |
| registry 可配置（默认 npmjs，支持 npmmirror） | 保留 |

### 3.2 新增

| 能力 | 说明 |
|---|---|
| 首次引导安装 | 无闭包时启动引导页：下载→安装→双重自检→进工作台；默认 latest，可展开选择版本；可取消、断网可重试 |
| dsh 版本管理页 | 当前版本 + registry 版本列表（latest + 全部版本）+ 一键更新到最新 + 指定版本安装 + 回滚（选上一版本） |
| 插件列表展示 | 读取 `~/.dsh/profiles/web` 已装插件（名称/版本/状态），安装/卸载后自动重启工作台生效 |
| App 内更新 | 检查 GitHub Releases → 新版提示 → 下载（校验）→ 自动安装 → 重启新版本 |
| 启动静默检查 dsh 新版 | 有新版在 dsh 页提示，不自动安装 |

### 3.3 砍掉

- 局域网远程连接（lan-proxy.js、mdns-advertise.js、qrcode.js、相关 Rust 逻辑与打包）
- 常规页：工作目录选择、登录自启、手动重启按钮、日志查看 UI
- 内置 dsh 闭包（resources/dsh/），安装包从约 400MB 降到约 50MB

## 4. 架构

### 4.1 技术选型（已确认）

- **Tauri 2 + Rust** 后端：跨平台（macOS/Windows）、体积小、现有构建链成熟。
- **原生 HTML/CSS/JS** 壳页：`frontendDist: ../ui` 静态文件直接嵌入，零构建链、零 npm 依赖；壳页是管理 UI，业务逻辑全部在 Rust command 层。不引入 TypeScript（避免构建链；现有代码已是原生 JS，重写成本低）。
- 运行时自包含：内置 node 二进制 + npm（更新/安装 dsh 用）+ pnpm（插件管理用）。**不依赖主机任何环境**。

### 4.2 模块结构

```
apps/desktop/src-tauri/src/
  main.rs        # 启动、进程管理（spawn/自愈/退出）、托盘、单实例、路径、设置
  dsh.rs         # 闭包管理：首次安装/版本安装/切换/回滚/GC（从 update.rs 演化）
  registry.rs    # npm registry 查询：latest、版本列表（可配源）
  plugin.rs      # 插件管理：列表读取 + 安装/卸载（复用 pnpm 逻辑）
  appupdate.rs   # App 更新：GitHub Releases 检查 + 下载 + 安装
  update.rs      # 删除（逻辑并入 dsh.rs）
  # main.rs 中 LAN 相关逻辑一并删除：LAN_CHILD/LAN_ON/LAN_GEN/LAN_ITEM、
  # mdns 子进程、lan_* settings 字段及对应 command（无独立 lan 文件）
ui/
  shell.html/js  # 主壳页：Tab = 工作台 / dsh / 插件 / 关于
  setup.html/js  # 首次引导页：下载安装进度、高级版本选择
  theme.css      # 共享设计系统（视觉稿由 open-design 生成后落地）
  modal.html/js  # 自绘弹窗（复用）
scripts/
  prepare-resources.sh / .ps1   # 瘦身后：只打包 node + npm + pnpm，删闭包/LAN 资源
```

### 4.3 数据布局

| 数据 | 位置 | 说明 |
|---|---|---|
| dsh 闭包 | `<app-data>/dsh/v<ver>/` + `current` 标记文件 | App 私有管理；保留上一版本回滚；GC 更旧版本 |
| dsh 会话/凭据/配置 | `~/.dsh`（macOS）/ `%USERPROFILE%\.dsh`（Windows） | 与终端共用，永不自动删除 |
| 插件 | `~/.dsh/profiles/web` | 与终端共用 |
| npm 缓存 | `<app-data>/dsh/npm-cache` | 不泄漏到 `~/.npm` |
| App 更新临时文件 | 系统临时目录 | 安装后清理 |

## 5. 核心流程

### 5.1 首次启动（无闭包）

1. 启动 → 检测 `<app-data>/dsh/current` 无有效闭包 → 打开**引导页**（不启动 dsh 进程）。
2. 引导页默认「安装最新版（latest）」；可展开「高级」选择 registry 源与具体版本（版本列表来自 registry）。
3. 执行：内置 node + npm 安装到 `v<ver>-tmp` → 双重自检（`--version` + web profile 组合）→ 写 VERSION 标记 → 发布为 `v<ver>` → 原子切换 `current` → 清理 tmp。
4. 完成后自动启动 dsh web 并进入工作台。
5. 失败：引导页显示错误与重试按钮，当前无可用版本时不影响任何数据；断网时提示检查网络。

### 5.2 dsh 更新 / 版本管理（dsh 页）

- 启动静默检查 registry `latest`；有新版时 dsh 页显示提示（不自动安装）。
- **更新到最新**：与首次安装同一路径（tmp → 自检 → 原子切换 → 自动重启工作台 → GC）。
- **指定版本安装**：版本列表（含全部版本，按 semver 倒序）点选 → 确认 → 同上路径。
- **回滚**：版本列表选择上一版本安装（同一路径，天然支持）。
- 当前版本与「上一版本」始终各保留一份（约 340MB × 2）；GC 仅清理更旧版本。

### 5.3 App 更新（关于页）

1. 检查 `github.com/Jedeiah/dsh-desktop/releases/latest`（跳转解析 tag，与现状 install.sh 同法，失败静默）。
2. 有新版 → 关于页提示 → 点击「下载并安装」。
3. 下载安装包（DMG / EXE）到临时目录，校验大小（与 Release asset 一致）；失败清理，不动当前版本。
4. macOS：挂载 DMG → 复制 `.app` 到 `/Applications`（弹系统授权）→ 退出当前实例 → 打开新版本。
5. Windows：运行 setup.exe 静默安装（NSIS `/S`，currentUser 模式）→ 退出 → 新版本由安装器启动。
6. 平台差异细节在实现计划中定稿；手动方式（打开下载页自己装）作为兜底入口保留。

### 5.4 插件管理（插件页）

1. 进入插件页 → 读取 `~/.dsh/profiles/web` 已装插件列表（名称/版本/状态）。
2. 安装：输入包名（如 `@linxin666/dsh-web-ui-all`）→ 内置 pnpm 安装（复用现状门禁处理与实时输出）→ 完成后**自动重启工作台**生效（替代现状「手动点重启」）。
3. 卸载：列表中选择 → 卸载 → 自动重启工作台。
4. 插件仅作用于 web profile（与终端 dsh 共用 `~/.dsh/profiles/web`）。

## 6. 错误处理与安全

| 场景 | 行为 |
|---|---|
| dsh 安装/更新失败 | tmp 清理；当前版本不受影响；UI 显示错误与重试 |
| 下载中断 | 可重试；npm 侧 integrity 校验 |
| dsh 进程崩溃 | 自动重启（退避 1s→15s）；连续 5 次停止并弹窗提示日志路径 |
| 网络不可用 | 所有检查静默失败；引导页/更新页显示「无法连接」可重试 |
| App 更新安装失败 | 当前版本不受影响；提示手动下载 |
| 卸载 | 两档：保留 `~/.dsh`（推荐）/ 全删；Windows 唤起系统卸载器 |
| 并发 | 单实例；安装/更新期间锁定，避免重复操作 |

## 7. UI 结构（主壳页）

- **Tab 布局**：工作台 / dsh / 插件 / 关于（共 4 个，替换现状 6 个）。
  - 工作台：dsh web iframe + 启动占位层。
  - dsh：当前版本、检查更新、版本列表（安装/切换/回滚）、registry 源设置。
  - 插件：已装插件列表、安装输入、实时输出。
  - 关于：App 版本、检查 App 更新、一键安装、卸载入口。
- **引导页**（主窗首屏，替代工作台占位层；不新增窗口）：首次安装进度、版本高级选项、错误重试。复用现有「启动占位层」的位置与单窗口结构。
- 视觉稿用 open-design 生成（本机 MCP 已配置，需先启用），落地为 `theme.css`，沿用现状「紫粉蓝光斑玻璃拟态」设计语言基调。

## 8. 测试策略

- **Rust 单元测试**：registry 解析（latest/版本列表）、版本比较（semver 含 rc 后缀）、闭包安装/切换/GC 状态机、插件列表解析、App 更新 tag 解析。
- **手动回归清单**（每次发版前）：首次引导安装、更新到 latest、指定版本安装、回滚、插件装/卸 + 自动重启、App 更新流程、崩溃自愈、卸载（两档）、托盘/单实例。
- **CI**：现状 GitHub Actions 构建链保留；prepare-resources 瘦身后产物自检（node/npm/pnpm 校验，不再有闭包自检）。

## 9. 非目标（Non-goals）

- 不做 dsh 本体功能（profile、agent、模型配置等）——那是 dsh 自己的事。
- 不做局域网/远程访问。
- 不支持 Linux（维持 macOS/Windows）。
- 不引入前端框架/构建链。
- 不做多 profile 插件管理（仅 web profile）。
- 不做 dsh 自动更新（保持手动确认）。
