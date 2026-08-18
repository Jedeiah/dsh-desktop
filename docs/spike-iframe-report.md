# Spike 报告：dsh Web Workbench 能否嵌入 Tauri iframe

> 目标：判断由捆绑 `dsh` CLI 通过 localhost HTTP 提供的 AI 开发者工作台 UI，能否被嵌入到 Tauri `tauri://localhost` 壳页的 `<iframe>` 中。
> 环境：macOS（Tauri shell 使用系统 WebView，即 WebKit/WKWebView）。
> 日期：2026-08-18 · 范围：半天研究 spike · 约束：未安装任何依赖、未修改任何源码、未启动 Tauri App、未打开 GUI。

---

## 摘要（TL;DR）

**有条件可行（路径 A：壳页 + iframe）。** 决定性证据（响应头与 HTML）显示 dsh 工作台 **完全没有设置** `X-Frame-Options`、`Content-Security-Policy`（含 `frame-ancestors`）、`Content-Security-Policy-Report-Only`，HTML 中也没有 CSP `<meta>` 或 frame-breaking JS —— 因此现代 Chromium/WebKit（WebView2/WKWebView）会**允许**该页面被 iframe 嵌入。WebSocket 的 host 是从 iframe **自身 document 的 origin**（`http://127.0.0.1:<port>`）派生的，且 dsh 不设 CSP，故父页面的 CSP **不是** WS 连接的拦截点。剩余故障点集中在 iframe 语义本身：键盘焦点、剪贴板权限、`target="_blank"` 新开上下文、嵌套弹窗与 Tauri 拖拽区——这些都需要真实桌面运行来最终确认（见 Section C）。

---

## A. 证据表

### A.0 运行环境与所用closure

| 项 | 值 |
|---|---|
| Node 二进制 | `/Users/chj/agentProjects/dsh-desktop/apps/desktop/src-tauri/resources/node/bin/node`（v24.14.0） |
| dsh closure（本次 boot） | `/Users/chj/agentProjects/dsh-desktop/apps/desktop/src-tauri/resources/dsh/current/node_modules/@deepseek-ai/dsh/lib/bin.js` |
| `current` symlink → | `0.1.0-rc.6`（`VERSION` 文件 = `0.1.0-rc.6`） |
| 已安装 App 内 closure（旁证，未 boot） | `/Applications/DeepSeek Harness.app/Contents/Resources/resources/dsh/0.1.0-rc.7/...`（`VERSION` = `0.1.0-rc.7`；其 `node` md5 与仓库 node 不同，为独立构建） |
| Spike HOME | `/tmp/dsh-spike-home`（隔离，未触碰真实 `~/.dsh`） |
| 启动命令 | `HOME=/tmp/dsh-spike-home NO_COLOR=1 <node> <bin.js> --profile web --port 0 > /tmp/dsh-spike.log 2>&1 &` |
| 启动耗时 | ~4 秒即在日志出现 `dsh web: http://127.0.0.1:<port>` |
| 分配的端口 | **64259**（`--port 0` 随机分配） |

### A.1 HTTP 响应头（`GET /`）—— verbatim

```
HTTP/1.1 200 OK
content-type: text/html; charset=utf-8
Date: Tue, 18 Aug 2026 15:07:19 GMT
Connection: keep-alive
Keep-Alive: timeout=5
Transfer-Encoding: chunked
```

**关键结论：头中没有 `Content-Security-Policy`、没有 `Content-Security-Policy-Report-Only`、没有 `X-Frame-Options`、没有 `frame-ancestors`。** `/assets/*.js` 资源响应头同样无任何 CSP 相关字段（`content-type: text/javascript; charset=utf-8` + 通用字段）。

### A.2 HTML 级证据

- **CSP meta 标签**：无。全量搜索 `http-equiv|content-security|frame-ancestors|x-frame-options`（大小写不敏感）**零命中**。
- **Frame-breaking JS**：无。`<head>` 内联脚本仅含 `window.__DSH_BOOT__` 插件清单与一个暗色主题脚本；全 HTML 检索 `top.location|window.top|parent.location|self !== window.top|frame-ancestors|X-Frame-Options|http-equiv=Content-Security` **零命中**。
- 主入口：`<script type="module" crossorigin src="/assets/index-Dqw48FrP.js">`，另有 `vendor-Cjbwl5VI.js`、`index-CSGf6Qzd.css`、`vendor-CjyC-hUb.css`、`/manifest.webmanifest`、`/favicon.svg`。

### A.3 JS bundle 检索结果（telltale strings）

下载并检索主入口（`index-Dqw48FrP.js`，442,711 B）与 vendor（`vendor-Cjbwl5VI.js`，744,872 B）：

| 字符串 | index.js | vendor.js | 判定 |
|---|---|---|---|
| `frame-ancestors` / `X-Frame-Options` | 0 | 0 | 无 |
| `top.location` / `window.top` / `parent.location` / `self !== (window.)top` | 0 | 0 | 无 frame-break |
| `navigator.clipboard` | 3 | n/a | 使用异步剪贴板 API（见 C） |
| `document.execCommand` | 2 | n/a | 传统剪贴板回退 |
| `window.open(` / `print()` / `requestFullscreen` | 0 | 0 | 未用（见 C 备注） |
| `target:"_blank"` + `rel:"noopener noreferrer"` | 2 | n/a | 仅对外部 `http(s)` 链接（见 C） |

插件客户端（动态加载，`/plugins/@deepseek-ai/<name>/client.js`）同样无 frame-break 字符串。连接客户端（`dsh-client-connection/client.js`，350,205 B）用 `new URL(path, this.resolveBase())` 派生 WS 地址，`resolveBase()` 读取 `globalThis.location.origin`。

### A.4 WebSocket 实测

- WS 路径常量：`MUX_EVENTS_PATH = "${API_PATH}/events.mux"`、`HOST_EVENTS_PATH = "${API_PATH}/events.host"`，其中 `API_PATH = "/api"`。
- curl 携带 `Upgrade: websocket / Sec-WebSocket-Version: 13 / Sec-WebSocket-Key` 发起握手，**`/api/events.mux` 返回 `HTTP/1.1 101 Switching Protocols`（成功升级）**。
- **地址派生**：`new URL(path, location.origin)` 且 `url.protocol = http: ? wss: : ws:` → iframe document origin 为 `http://127.0.0.1:64259`，故 WS 为 **`ws://127.0.0.1:64259/api/events.mux`，与 dsh 服务同源**。

> 治理结论：WS 连接由 **iframe 自身的 document** 发起，受 **该 document 的 CSP** 约束。dsh 不设任何 CSP（无 `connect-src`），因此父 Tauri 壳页的 CSP 不是 WS 的拦截点。若将来 dsh 加上带 `connect-src` 的 CSP，则必须允许 `ws://127.0.0.1:*`；当前无此约束。

---

## B. 判定：有条件可行（路径 A）

决定性测试（无 XFO/CSP/`frame-ancestors` + 无 meta CSP + 无 frame-break JS）成立 → **明确支持 iframe**。以下按交互类别列出条件与依据。

| 维度 | 结论 | 证据 / 条件 |
|---|---|---|
| **能否加载/渲染** | ✅ 可行 | 无 `X-Frame-Options`、无 `frame-ancestors`（响应头 + meta 均无）。WKWebView/Chromium 仅在显式禁止时才拦 iframe；此处未禁止。 |
| **WebSocket 流式输出** | ✅ 可行 | `ws://127.0.0.1:<port>/api/events.mux` 实测 101 升级成功；host 由 iframe document 自身 origin 派生，父页 CSP 不拦截。 |
| **键盘/快捷键** | ⚠️ 条件 | 工作台依赖键盘快捷键。iframe 仅在自身获得键盘焦点后才收到按键；用户需先点击 iframe 内部。壳页若监听全局快捷键，需在 iframe 有焦点时把事件转交给 iframe document，或用 `postMessage` 转发。 |
| **剪贴板（copy/paste）** | ⚠️ 条件 | 使用 `navigator.clipboard`（+ `document.execCommand` 回退）。异步剪贴板在 iframe 内要求：iframe 文档**获得焦点** + 父/子均允许 `clipboard-write`（无沙箱属性 + 无限制性 Permissions Policy 时可放行）。剪贴板读取更严，需用户手势与焦点。若壳页 WebView 未授权剪贴板，应优先走 Tauri 原生剪贴板桥接。 |
| **嵌套弹窗/Modal** | ✅ 大概率可行 | 工作台 Modal 为普通 DOM 覆盖层，在 iframe 内自洽渲染；不依赖新窗口。需在真实运行中确认（无 JS 证据显示其使用 `window.open`）。 |
| **新窗口 / `target=_blank`** | ⚠️ 条件 | bundle 中 `target:"_blank"` 仅用于外部 `http(s)` 链接（`rel="noopener noreferrer"`）。iframe 内 `_blank` 会在**新标签/新窗口**打开，脱离 Tauri 单窗口框架；壳页需拦截 `window.open`/`<a target=_blank>` 导航并转为 Tauri 原生方式（如系统浏览器或内部路由）。 |
| **拖拽区（Tauri `data-tauri-drag-region`）** | ⚠️ 条件 | 若工作台顶部标题栏用 Tauri 拖拽区，iframe 内该属性**不生效**（拖拽区由壳页窗口控制，iframe 内容不继承）。壳页需在 iframe 外用自带覆盖条提供拖拽，或去掉工作台自身拖拽。 |
| **打印 / 全屏** | ✅ 无使用 | bundle 检索 `print()`/`requestFullscreen` **零命中**；工作台未用。 |
| **文件/目录选择** | ⚠️ 条件 | 有 `dsh-client-ui-directory-picker-browse`（目录选择）等 UI 插件。iframe 内 `<input type=file>` / 目录选择器在本机 WKWebView 一般可用（需用户手势；无沙箱属性时不被 `allow` 限制）。目录选择在 macOS WKWebView iframe 内的表现需真实验证（见 C）。 |
| **父页面 CSP 是否阻 WS** | ✅ 不阻 | WS 由 iframe 自身文档发起、受自身 CSP 约束；dsh 无 CSP。 |

### 一句话判定
工作台**可以被 iframe 嵌入**（无任何 frame-block），WS 流式输出在 iframe 内**同源可用**；需要额外工程处理的是**键盘焦点移交、剪贴板授权、`_blank` 新窗拦截、拖拽区由壳页承担**这四类 iframe 语义差异。

---

## C. 无法无头验证的残余风险（需真实桌面运行确认）

以下无法仅凭 curl/grep 判定，需在真实 macOS WebView（WKWebView）桌面运行中核对：

1. **键盘焦点与全局快捷键转发**：验证点击 iframe 内部后快捷键是否直达工作台；确认壳页与 iframe 之间的焦点切换体验，以及是否需要 `postMessage`/Shell 桥接转发。
2. **剪贴板读写权限**：确认 WKWebView 内 iframe 的 `navigator.clipboard.writeText/readText` 是否可用、是否需要先有用户手势/焦点，是否需要走 Tauri 原生剪贴板命令。
3. **`target="_blank"` 新窗行为**：确认外部链接在 iframe 内点击时是开系统浏览器还是被 WKWebView 拦截；决定壳页是否要接管导航。
4. **目录/文件选择器**（`dsh-client-ui-directory-picker-browse`）：macOS WKWebView 在 iframe 内触发目录选择弹窗的可行性。
5. **Tauri 拖拽区**：工作台顶部若使用 `data-tauri-drag-region`，确认真实窗口内不会导致无法拖动（预期需壳页覆盖条替代）。
6. **嵌套 Modal 与小工具在 iframe 视口裁剪下的布局**：工作台全屏布局在 iframe 尺寸内的滚动/定位表现（预期 OK，但需目视）。
7. **`dsh.internal` 回退**：`resolveBase()` 在 `location.origin === "null"`（如 `srcdoc`/极端 case）时回退到 `http://dsh.internal`；正常 http iframe 不会触发，但真实运行可顺带确认。

---

## D. 5 行架构建议

1. **采用路径 A（壳页 + iframe）**：无 XFO、无 CSP frame-ancestors、无 frame-break JS 已证明可直接嵌入；WS 在 iframe 内与工作台同源可用，架构改动最小。
2. 壳页 iframe 不用 `sandbox` 属性（保持剪贴板/文件/picker 父权限），并给自己的 Tauri 壳页 CSP 放行 `frame-src http://127.0.0.1:*`、`connect-src ws://127.0.0.1:*`（为了稳健，即便当前 iframe 自身文档才管 WS）。
3. 壳页负责**键盘焦点移交**与**顶部拖拽条**（iframe 内 `data-tauri-drag-region` 不生效）；全局快捷键在 iframe 有焦点时把事件导向 iframe document。
4. 剪贴板走 Tauri 原生桥接（`writeText`/`readText`）作为首选，避免 iframe 焦点与权限的不确定性；用 `onClick`/焦点触发规避手势限制。
5. 拦截 `target="_blank"` 与 `window.open`，外部 http(s) 链接转系统浏览器或 Tauri 内部路由，保持单窗口体验。

> 执行验证清单：在真实桌面运行中核对 Section C 的 7 项，落地路径 A；若其中 ≥2 项（尤其键盘/剪贴板/拖拽）无法通过壳页桥接解决，再回退路径 B（独立管理窗 + 命令面板）。

---

## 附录：实际执行过的命令摘要

```bash
# 定位与验证
ls -la apps/desktop/src-tauri/resources/dsh/current          # -> 0.1.0-rc.6
<repo>/resources/node/bin/node --version                     # v24.14.0

# 启动（后台、隔离 HOME）
mkdir -p /tmp/dsh-spike-home
HOME=/tmp/dsh-spike-home NO_COLOR=1 <node> <bin.js> --profile web --port 0 > /tmp/dsh-spike.log 2>&1 &
# 日志 ~4s 后出现: dsh web: http://127.0.0.1:64259

# 响应头
curl -sS -i http://127.0.0.1:64259/                          # 无 CSP/XFO/CSPRO

# HTML 与 bundle 检索
curl -sS http://127.0.0.1:64259/ -o /tmp/dsh-spike-index.html
grep -iE 'http-equiv|content-security|frame-ancestors|x-frame-options' index.html  # 无命中
grep -oE 'top\.location|window\.top|parent\.location|self !== window\.top' index.js vendor.js  # 无命中
curl -sS .../assets/index-Dqw48FrP.js -o index.js            # 442,711 B

# WebSocket 握手（101 Switching Protocols）
curl -sS -i -H "Connection: Upgrade" -H "Upgrade: websocket" \
     -H "Sec-WebSocket-Version: 13" -H "Sec-WebSocket-Key: <b64>" \
     http://127.0.0.1:64259/api/events.mux

# 清理
kill <PID> ; pkill -f dsh-spike-home ; rm -rf /tmp/dsh-spike*
```

*未修改任何仓库源码；仅产出本报告并清理临时文件。*
