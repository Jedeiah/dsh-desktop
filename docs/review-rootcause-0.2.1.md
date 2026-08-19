# v0.2.1 根因修复审查记录

本文件记录 v0.2.1 期间对用户反馈问题的**根因定位 + 修复 + 复审**，供后续会话避免重复踩坑。

## 0. 用户反馈的复现现象

1. 启动后窗口"往上闪一下"（出现后跳动/重新定位）。
2. "loding 没了 → 白屏 → 又出现 loding"的闪烁。
3. 担心发版；明确要求：认真审查项目、修根因、别打补丁、清理会话垃圾、讲清 `resources/dsh/current` 是否软链、注意 mac/windows 双环境。

---

## 1. 问题一：启动"往上闪一下" —— 根因 = 同一窗口被"显示两次 + 二次居中"

### 根因
旧 `show_window` 的启动路径把主窗**显示了两次**：

```
创建：visible(false)+center → build() → 立即 .map(|w| w.show())
dsh 就绪（~1-2s 后）：show_window() → emit url → center() → show() → set_focus()
```

- 第一次 `show()` 时 macOS 用**默认/上次位置**显示；第二次（dsh 就绪）又 `center() + show()` 对**已显示窗口再次定位** → macOS 重新摆放 → 视觉上"往上闪一下/跳一下"。
- 托盘/Dock 复用同一函数，每次打开都强制居中，也会在已可见窗口上造成重复定位。

### 修复（根因级，非补丁）
- 主窗创建后**不再立刻 show**（去掉 builder 的 `.map(|w| w.show())`）。
- 收敛为**唯一显示通道** `reveal_main_window(app, url: Option<&str>)`：
  - 永远先推 `dsh:url` 给壳页（iframe 更新需要）；
  - **仅当窗口当前不可见**时才 `center() + show()`（首次出现即居中）；
  - 已可见时**不再重定位**（消除已显示窗口上的第二次 show/center）。
- 启动宽限：隐藏创建 → 1.2s 后若 dsh 未就绪先显示带占位的窗口（避免"点了没反应"）；dsh 就绪后走同一通道更新 iframe。两次只显示一次。
- 消除了随路硬编码的 `http://127.0.0.1:3080` 默认地址（那是开发机端口残留）。

## 2. 问题二：占位/白屏闪烁 —— 根因 = "src 一赋值就撤占位，iframe 还没画"

### 根因
旧 `loadWorkbench`：拿到 URL 后 `wb.src = url` **立刻**把占位层隐藏。此时 iframe 刚导航、尚未绘制 → 露出**白底**（iframe 页面背景在 CSS 加载前是白色）；等 dsh 内容画出来又是另一帧。于是"loding → 白屏 → dsh 自己的 loading"。

### 修复
- 占位层改为**默认可见**（"正在启动 dsh 工作台…"），且是绝对定位**覆盖在 iframe 之上**。
- iframe `load`（实际绘制完成）事件才撤占位；`error` 事件保留占位。
- 换端口/重启工作台（新 URL）时**先重新盖住**，onload 后撤 → 切换过程全程有 spinner 覆盖，不再露白屏。
- `showPlaceholder` 先清内联 `display` 再取消 `hidden`（避免 `inline display:none` 残留导致永远不可见）。
- shell.html 补 `[hidden]{display:none!important}` 防 `.placeholder{display:flex}` 覆盖隐藏（历史"占位层挡住点击"根因）。

### 验证
DOM 模拟（`vm.Script` 编译 + 假 DOM）全部通过：
`SYNTAX_OK`（经典脚本无顶层 await）→ 启动可见 → src 就绪后占位保持 → iframe load 后隐藏 → 换端口重显 → load 再隐藏。监听器/轮询均注册成功。

## 3. 问题三：`resources/dsh/current` 是软链吗？ —— 答案 + 根因

**答案**：**大改（v0.2.0）之前是软链**（`current -> <版本目录>`，见 prepare-resources.sh 旧实现与脚本头注释）；**v0.2.0 起设计已明确改为"纯文本版本标记文件"**（内容 = 版本号，Windows 无软链权限、跨平台通用）—— update.rs 的 `apply_update` 与 prepare-resources.sh/ps1 都已按纯文本写。

**根因（v0.2.0 期间本地构建偶发失败的真凶）**：
- `resources/dsh/current` 若为软链 → tauri 的 `resources/**/*` glob 会**跟随软链把整个闭包再物化一份**进包 → .app 里出现 `dsh/current/node_modules/...` 副本，**体积翻倍**（实测 .app 1.8GB，闭包 1.7GB）。
- 若为纯文本文件 → 本地 `cargo tauri build` 曾报 `Is a directory (os error 21)`——**不是纯文本形式的问题**，而是 **target/{debug,release}/resources/dsh/current 残留了旧软链构建产生的目录**，`fs::copy(文件 → 已存在目录)` → EISDIR。CI 干净环境一直能过（v0.2.0 线上产物正常）。
- 修复：源 `current` 恢复为**纯文本标记文件**（与脚本/update.rs 一致）；删除 target 内残留的 `current` 目录后重建。

**验证**：
- 本地 debug 全链构建成功；产物内 `current` = ASCII 文本；resources/dsh = 863M（单闭包），.app 1.0GB（对比旧 1.8GB）。
- 无头运行 `dsh-desktop --self-update-check` 输出 `UP_TO_DATE` → 证明 `resolve_current` 用纯文本 `current` 正确解析出 rc.7 闭包并与 npm 上游一致。

## 4. 问题四：Windows 右键-删除无效 / uninstall.exe 不齐全（一次复审确认）

- `installer-hooks.nsh` 的 `NSIS_HOOK_PREUNINSTALL`（先 `--self-uninstall-full` 杀进程树 + 清数据）与 `POSTUNINSTALL`（清 HKCU Run 键）宏名与参数均与 tauri-bundler 2.9.4 模板核对一致（`MAINBINARYNAME` 在模板 :52 定义；四个 HOOK 均存在 :641/:733/:778/:886）。
- Windows 侧 `prepare-resources.ps1` 同样写纯文本 `current`，与 mac 侧 .sh 对齐。
- 卸载唯一链：`uninstall_run`（壳页卸载 Tab）→ teardown → 唤起系统 `uninstall.exe`；NSIS PREUNINSTALL → `--self-uninstall-full`；两者共用 `uninstall_teardown`。右键/设置里卸载与 App 内卸载走同一条数据清理路径。

## 5. 清理

- 删除 P3 前的**死代码**页面：`ui/index.html`、`lan.html/js`、`plugins.html/js`、`uninstall.html/js`（全库无任何引用，主窗只建 shell.html + modal.html）。
- 删除仓库内 4 个 `.DS_Store`；清理会话诊断遗留 `/tmp/dsh-*`（closure 暂存、diag 日志、shell 模拟脚本等）。
- 移除 main.rs 的调试探针 `probe_webview`（含 eval 注入），保留一行顶部帧 page-load 日志。

## 6. 复审结论

- 双环境关键点逐一核对：mac 旧软链与纯文本均可被 `resolve_current` 解析（纯文本为规范）；Windows 无软链依赖；NSIS 卸载链完整；CSP 允许 iframe 加载 `127.0.0.1/*`。
- shell.js 为经典脚本（无顶层 await），语法经 `vm.Script` 校验。
- 发布构建（release）通过后发布 v0.2.1。
