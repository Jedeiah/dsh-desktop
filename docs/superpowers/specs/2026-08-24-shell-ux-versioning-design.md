# DSh Desktop 壳页 UX 与版本管理增强（v0.3.1+）

日期：2026-08-24
状态：草案，待用户审阅批准

## 1. 背景与问题

在 v0.3.1 发版准备期间，用户提出一批壳页体验问题与版本管理改进需求。均为对现有代码的定向增强（bounded 组合），无新子系统。

### 版本管理（原待办）
- **V1**：版本列表当前全量拉取到前端内存（`list_versions` 返回 registry 全量版本），版本非常多时网络/内存成本高；搜索框依赖全量缓存，引导页下拉同样全量。
  - 方案已确认：版本列表只显示最近 10 个；去掉搜索框；新增「输入版本号安装」，下载前校验版本是否存在。

### 新问题（用户 2026-08-24 反馈）
- **V2**：dsh web（iframe）内 AI 回答文本块右上角复制按钮无反应。
  - 根因（探索确认）：`<iframe id="workbenchFrame">`（shell.html:308）**没有 `allow="clipboard-write"`**，Permissions Policy 默认 allowlist 为 `'self'`，跨源 iframe（http://127.0.0.1:PORT）里 `navigator.clipboard.writeText()` 被拒（NotAllowedError）。
- **V3**：插件 tab 已安装插件列表每行没有卸载按钮；卸载需在「安装插件」输入框输包名，交互别扭。
- **V4**：App 内选中文字键盘快捷键复制不可用（壳页无 Cmd/Ctrl+C 处理）。
- **V5**：顶部伸缩 tab 行折叠时「内容隐藏但没完全缩上去」——内容只 `translateY(-10px)`，未随顶栏高度（46→28px）完整收起，观感不一致。
- **V6**：tab 行左侧品牌（图标+App名）无点击事件，期望点击刷新工作台（等同右键 reload）。
- **V7**：期望双击「工作台」tab 用系统浏览器打开当前工作台。
- **V8**：Windows 上引导页 dsh 版本下拉框保留原生 select 外观（无 `appearance:none`），样式不统一、难看。
- **V9**：插件安装只接受 npm 包名（`@scope/pkg`），`github:owner/repo`、`git+https://…`、`https://…tgz` 等 pnpm 支持的包规格无法输入（`valid_pkg_name` 白名单过窄）。

## 2. 设计

### V1 版本管理（后端 + 双 tab 前端）
- **后端**：
  - `registry.rs` 新增 `version_exists(registry, ver) -> Result<bool, String>`：`GET {registry}/{pkg}/{ver}`，200→true、404→false、其它→Err；200 但响应体含 `error` 字段按不存在处理（部分 registry 镜像行为）。
  - `main.rs` 新增 `version_exists_cmd(app, window, ver) -> Result<bool, String>`：`ensure_shell_window` + `valid_version` 白名单；registry 用 settings 默认值；网络查询走 `spawn_blocking`。
  - `list_dsh_versions_cmd` 与 `get_dsh_state` 的版本列表都收敛：只返回最近 10 个（前者供引导页下拉，后者供 dsh tab 列表——审核确认 dsh tab 数据源是 `get_dsh_state`，两处都收敛才根治全量拉取）。
- **前端 dsh tab**：移除搜索框（HTML/CSS/JS 全删）；`renderVersions` 只渲染最近 10 个；新增「输入版本号 + 安装」输入框与按钮——点击 → trim → 后端 `version_exists_cmd` 校验 → 存在则 `updateDsh(ver)`，不存在提示「版本 X 不存在」；已安装版本仍显示「切换」。
- **前端引导页**：`setupVer` 下拉靠 `list_dsh_versions_cmd` 收敛自动生效（最近 10 个）；同样加输入版本安装入口。
- 移除 `dshRender` 全量缓存（不再需要）。

### V2 iframe 剪贴板权限
- `shell.html` 的 `<iframe id="workbenchFrame">` 加 `allow="clipboard-write"`（一行）。iframe 内 `navigator.clipboard.writeText()` 即可用。

### V3 插件 tab 每行卸载按钮
- `shell.js` 已装插件行（`list_installed_plugins` 渲染处）每行加「卸载」按钮 → 点击后按钮变为「确认卸载」→ 再点才调现有 `plugin_op`（op=remove）执行（**二次确认防误触，已确认**）。

### V4 壳页快捷键复制
- `shell.js` 全局 keydown 增加 `Cmd/Ctrl+C`：当焦点在壳页且存在选中文字时，用 `document.getSelection()` 复制（`navigator.clipboard.writeText`）；不拦截 iframe 内的复制（V2 已覆盖）。

### V5 折叠动画
- **需求澄清（用户 v0.3.2 实测确认）**：折叠后顶栏应**完全收起（行全没）**，而非收成 28px 窄条（此前 translateY(-10px→-18px) 只影响动画位移，窄条观感不变——方向修正）。
- `shell.js` `applyTabsCollapsed`：折叠时 `--dsh-topbar-h` 设为 `0px`（展开 46px）；`shell.html` `.titlebar.collapsed { height: 0; border-bottom: none; }`；brand/tabs 淡出（opacity 0 + visibility hidden）不变；把手 `.tb-toggle` 随 `top: var(--dsh-topbar-h)` 浮到窗口顶部，点击展开。工作区 `top: var(--dsh-topbar-h)` 联动全屏。

### V6 brand 点击刷新
- `shell.js` 给 `.brand` 加 click → 重载工作台 iframe（`loadWorkbench(currentUrl, true)` 或 `wb.src = wb.src`），不动壳页。

### V7 双击工作台 tab 浏览器打开
- `shell.js` 「工作台」tab 加 `dblclick` → `invoke('open_workbench_url_cmd')` → 系统浏览器打开。新增该命令：取当前工作台 URL（复用 `get_dsh_url` 逻辑）→ 复用私有 `open_url`（main.rs:891）。不复用现有 `open_browser_cmd`（main.rs:1242，硬编码 releases 页）。**工作台 URL 为空（dsh web 未就绪/启动中）时双击无效、不提示（已确认）**。

### V8 下拉框样式
- **需求澄清（用户 v0.3.4 实测）**：`appearance:none` 只改 select 框外观；Windows WebView2 的 option **弹出列表是系统原生渲染**（白底），CSS 无法定制，mac/win 无法统一。
- **方案（已实施）**：引导页版本下拉**自绘组件**（`.cselect`：button 触发器 + 暗色菜单面板 + 点击/失焦/Esc/Enter 交互），替代原生 select——mac/win 视觉完全一致；样式见 `theme.css` `.cselect-*`。全项目仅 `setupVer` 一处 select。

### V9 插件安装支持非 npm 包规格（git 类 / tarball 等）
- **问题**：当前 `valid_pkg_name`（plugin.rs:61）白名单仅允许 `@ . _ - / ~` + 字母数字，且不能以 `.`/`_`/`-` 开头。因此 `github:user/repo`、`git+https://…`、`https://…tgz`、`user/repo` 等 pnpm 支持的包规格会被拒。
- **调研结论（pnpm 官方文档 package-sources，2026-08 核实）**：
  - **不存在 `git:xxx/xxx` 这种裸 `git:` 前缀写法**（用户举例的格式在 pnpm/npm 中非法）；`git` 仅以 `git+ssh://`/`git+https://` 组合 scheme 出现，明文 `git://` 协议不开放。
  - pnpm 实际支持的包规格（与本产品相关的）：
    - npm 包名：`pkg`、`@scope/pkg`、`pkg@version`、`pkg@tag`（含空格的版本范围 `pkg@">=0.1.0 <0.2.0"` 官方支持，但 UI 输入不开放）
    - Git 简写（默认 GitHub）：`owner/repo`（如 `kevva/is-positive`）
    - 托管商简写：`github:owner/repo`、`gitlab:owner/repo`、`bitbucket:owner/repo`
    - 完整 Git URL：`git+ssh://…`、`git+https://…`、`https://…git`（官方示例为 git+ssh 与 https；git+https 为 npm 常见变体；可带 `#ref`/`#semver:`/`#path:`，多参数用 `&` 组合，如 `#beta&path:/packages/app`）
    - 远程 tarball：`https://…tgz`（http/https 开头）
    - 本地路径/tarball：`./dir`、`./pkg.tgz`（本产品场景不适用，不开放）
    - JSR / named registry / workspace：本产品场景不适用，不开放
- **设计**：扩展校验以允许上述「适用于本产品」的规格，同时**保持防注入**。定稿为两级校验（`valid_pkg_name` 或新增独立函数，实施时按此规则）：
  - **第一级（前缀白名单）**：仅允许以下开头——`@`（scoped npm 包）、字母数字（npm 包名 / `owner/repo` Git 简写）、`github:`、`gitlab:`、`bitbucket:`、`git+ssh://`、`git+https://`、`https://`、`http://`。其余（`git://`、`git+http://`、`file:`、`jsr:`、`workspace:`、`./`、`../`、`-` 开头等）一律拒绝。
  - **第二级（字符白名单）**：仅允许字母数字与 `@ . _ - / ~ : + # &`，以及出现在 `#semver:` 参数段内的 `< > = ^`。空白、控制字符、管道符/分号/美元符/反引号/单双引号/括号/星号/感叹号/问号等一律拒绝。
  - **`#` 参数段规则**：`#` 之后为参数段，支持 `#<ref>`（commit hash / 分支 / tag，含 `v` 前缀）、`#semver:<range>`（含 `^`、`~`、`<`、`<=`、`>`、`>=`、`v` 前缀）、`#path:<dir>`（monorepo 子目录）；多个参数用 `&` 分隔（如 `#beta&path:/packages/app`）。`&` 仅允许出现在参数段内。
  - **不开放**：`?` query 参数（tarball 带 token 场景暂无真实需求）、含空格的版本范围、本地路径 / `file:`、明文协议 `git://`。
  - 长度上限保留；不以 `.`/`_` 开头（排除本地路径形态）。
- **交互**：插件输入框提示更新为「支持 npm 包名（@scope/pkg）与 Git/tarball 源（owner/repo、github:owner/repo、git+ssh://…、git+https://…、https://…tgz），可用 #ref、#semver:、#path: 指定版本」。

## 3. 实施批次与验证

- 批次 1：V1（版本管理，核心）
- 批次 2：V2、V3、V4（修复类）
- 批次 3：V5、V6、V7、V8（体验增强）
- 批次 4：V9（插件安装包规格扩展）
- 每批：`cargo test`（22 用例）/ `cargo clippy -D warnings` / `node --check` + review 子代理复审。
- 全部完成后本地构建 v0.3.1 dmg 供实测。

## 4. 已确认 / 待确认

已确认（用户已表态 / 调研定稿）：
- 版本列表最近 10 个；去搜索；输入版本安装 + 下载前校验；双 tab 一致。
- V3 每行卸载按钮 + **二次确认**（点击后变「确认卸载」再执行）。
- V6 刷新工作台；V7 仅「工作台」tab，**URL 为空（未就绪/启动中）时双击无效、不提示**。
- V9 插件安装需支持非 npm 包规格（git 类 / tarball）；白名单已按 pnpm 官方文档定稿（见 V9 两级校验）。

待确认：无（所有决策点已定稿）。
