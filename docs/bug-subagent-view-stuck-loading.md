# Bug：子代理会话视图卡在「载入历史…」（待排查）

> 状态：**未定位**。记录于 2026-09-20，等有空再查。
> 本文件只记录已确认的事实、已排除的嫌疑和待验证的假设，**不含结论**。

## 现象

- 当 dsh 有**子代理正在运行**时，在 App 内点击该子代理：视图一直显示「载入历史…」+「深度求索中...」，既不出现内容，也不报错、不超时。
- **已完成的**子代理，点击可正常打开并看到内容。
- 同一个子代理，**在系统浏览器里打开工作台**（顶栏双击「工作台」或管理面板 →「在浏览器打开工作台」）**显示正常**。

## 环境（复现时记录）

| 项 | 值 |
|---|---|
| App 版本 | v0.5.8（Tauri 2 · macOS · WKWebView） |
| dsh 版本 | 0.1.6-alpha.2 |
| 工作台地址 | `http://127.0.0.1:56257/` |
| 窗口 | 3024×1898 @scale=2（`launcher.log` 记录：`bounds: y=136 h=1762 scale=2`） |

## 已确认的事实（附证据）

1. **卡住的文案来自 dsh 客户端插件，不是壳页的文案**
   `chat.loadingHistory` = 「载入历史…」、`chat.deepDiving` = 「深度求索中...」
   词条位于 `~/Library/Application Support/com.dsh-desktop.app/dsh/v0.1.6-alpha.2/node_modules/@deepseek-ai/dsh-client-ui-chat/lib/client.js`
   → 说明**聊天视图自身没有离开「加载中」状态**，不是壳层渲染/遮罩问题。

2. **不是「新窗口请求被拒并被丢给系统浏览器」造成的**（这条嫌疑已排除）
   `main.rs` 的 `webview_new_window_policy()` 对非内部地址会打印 `[webview] new-window request -> browser: …` 然后 Deny；
   实际 `launcher.log` 中该记录 **0 条**。

3. **壳没有改 dsh 的 JS**
   壳只往工作台注入 ⌘K / ⌘1–4 这 5 组按键的转发；dsh 客户端代码未被修改、未被 patch。
   → 历史加载逻辑完全在上游客户端。

4. **App 与浏览器的差异面**（引擎差异之外，壳还做了三件浏览器侧没有的事）
   - 启动时清理历史 dsh 会话 cookie（防 Cookie 头 >16KB 触发 431）
     日志：`[workbench] 已清理历史 dsh 会话 cookie 1/1 条（防止 Cookie 头超 16KB 触发 431），耗时 2ms`
   - 关闭所有 WKWebView 的背景绘制（`drawsBackground=false`，消除刷新白闪）
     日志：`[main] 已关闭 2 个 WKWebView 的背景绘制（消除刷新白闪）`
   - 打开抽屉 / 命令面板时把工作台 child webview **移出窗口再移回**（原生视图永远画在 HTML 之上，只能靠搬移让位）
     日志：`[workbench] bounds: …` / `尺寸稳定后补算几何（全屏/最大化切换）`

## 候选假设与各自的验证方法（都未验证）

### H1 · WebKit 差异：实时通道在 WebKit 下建不起来，但静态历史能拉
- **验证**：在 App 里点开正在跑的子代理后**不要动任何东西**，等子代理自己跑完。
  - 若内容在「跑完那一刻」自动出现 → H1 成立（只有实时通道坏，静态 refetch 正常）。
  - 若跑完后仍然空白 → 连静态历史也没拿到，H1 不成立。

### H2 · 鉴权 / 连接：运行中的 cookie 失效或被顶掉 → 新请求 401
- 机理：主会话的 WebSocket 早已建立，所以主会话照常工作；而**新打开**的子会话视图要发新请求，若鉴权已失效就会 401，UI 停在 loading。
- **验证**：卡住的时间点前后查
  - `launcher.log` 是否出现 cookie 清理记录
  - `dsh.log` 是否有 401 / 431 相关输出
  - 浏览器侧（正常）对照同一请求的返回码

### H3 · 壳的几何搬移干扰实时订阅
- 机理：打开子会话视图时若触发了壳的「浮层让位」逻辑，工作台被搬出/搬回，实时订阅可能被打断。
- **验证**：复现时观察 `launcher.log` 是否在那一刻出现 `[workbench] bounds:` 搬移记录。

## 定位所需的工具（关键）

release 构建**没有 Web Inspector**，看不到工作台 webview 的控制台。用 debug 构建复现：

```sh
cd apps/desktop/src-tauri && cargo tauri dev
```

debug 构建自带 Web Inspector，且 `is_internal_webview_url()` 在 debug 下会放宽 loopback 端口匹配，省去端口对齐的麻烦。复现后在 Console / Network 里看一次即可定性：

| 观察 | 指向 |
|---|---|
| 请求**根本没发出去**，或 Console 有异常 | 客户端在 WebKit 下的 JS 差异（H1） |
| 请求发出、返回 **401** | 鉴权 / cookie（H2） |
| 请求发出、**一直 pending** | 连接数 / 流被阻塞（H3 或连接上限） |

## 临时绕行（现在就能用）

- 需要看**正在跑**的子代理 → 用「在浏览器打开工作台」在系统浏览器里看。
- 已完成的子代理在 App 内点开正常。

## 待办

- [ ] 用 `cargo tauri dev` 复现，抓 Console / Network，定性到 H1 / H2 / H3
- [ ] 按定性结果修：若是壳侧则改壳；若判定为上游问题则整理可提交的 bug 报告
- [ ] 修好后往 [`docs/regression-checklist.md`](regression-checklist.md) 增加一条回归项：
      「点开**正在运行中**的子代理，应正常显示内容（不卡在「载入历史…」）」
- [ ] 若判定为上游问题：按 dsh Discussions 的格式整理（插件名 `dsh-client-ui-chat` + 词条 key + 复现步骤 + 「Chromium 正常 / WKWebView 卡住」的环境差异 + 已排除项）

## 相关位置速查

| 位置 | 说明 |
|---|---|
| `apps/desktop/src-tauri/src/main.rs` → `webview_new_window_policy()` | 新窗口策略（内部放行 / 外部丢浏览器） |
| 同上 → `is_internal_webview_url()` | 内部地址判定（只放行当前 dsh 端口；debug 下放宽 loopback） |
| 同上 → `ensure_shell_webview()` | 管理命令只接受壳页调用 |
| `main.rs` 中「背景绘制」与「清理历史 dsh 会话 cookie」附近 | 壳对 WebView / cookie 的两处额外处理 |
| `~/Library/Application Support/com.dsh-desktop.app/logs/launcher.log` | 壳与工作台几何、cookie 清理、新窗口请求 |
| `~/Library/Application Support/com.dsh-desktop.app/logs/dsh.log` | dsh 子进程输出 |
| `.../dsh/v0.1.6-alpha.2/node_modules/@deepseek-ai/dsh-client-ui-chat/lib/client.js` | 卡住文案的来源插件 |

> 注：上表中的行号未记录，因为本文件写入时 `main.rs` 正在被多语言改造改动；按**符号名**检索更可靠。
