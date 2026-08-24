# DSh Desktop 壳页 UX 与版本管理增强 实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在 v0.3.1 上实现 spec 的 V1–V9 九项壳页增强：版本管理（最近 10 个 + 输入版本安装）、iframe 剪贴板权限、插件行卸载（二次确认）、壳页复制快捷键、折叠动画、brand 点击刷新、双击工作台 tab 浏览器打开、select 样式统一、插件安装支持非 npm 包规格（两级校验白名单）。

**Architecture:** 纯增量修改既有单窗口 Tauri 应用。后端（Rust）仅新增 2 个 Tauri command（`version_exists_cmd` 半成品补注册、`open_workbench_url_cmd` 新建）+ 1 个 registry 辅助函数补全 + 重写 `valid_pkg_name`；前端（原生 JS，无框架）改 shell.html / shell.js / theme.css。每个功能点独立提交，前端无测试框架（验证 = `node --check` + 手动渲染验证），Rust 逻辑用 `cargo test`。

**Tech Stack:** Tauri 2（Rust）、原生 HTML/CSS/JS（无构建步骤）、ureq（registry 查询）、pnpm（插件安装后端进程）。

**Spec:** `docs/superpowers/specs/2026-08-24-shell-ux-versioning-design.md`（本计划的一切论证以 spec 为准；执行者须同时阅读 spec 与本文档）。

## Global Constraints

（从 spec 逐条复制，执行时每条任务都隐含遵守本清单）

1. 版本列表只显示**最近 10 个**；去掉搜索框；新增「输入版本号安装」，下载前用 `version_exists_cmd` 校验；dsh tab 与引导页**双 tab 一致**。
2. V3：已安装插件每行加「卸载」按钮，**二次确认**（点击后按钮变「确认卸载」再点才执行）。
3. V7：仅「工作台」tab 双击有效；**URL 为空（dsh 未就绪/启动中）时双击无效、不提示**。
4. V9 包名两级校验（前缀白名单 + 字符白名单 + `#` 参数段规则，见 Task 12）：禁止以 `-`/`.`/`_` 开头、空白、控制字符、管道符/分号/美元符/反引号/引号/括号/星号/感叹号/问号、`?` query、含空格版本范围、`file:`/`jsr:`/`workspace:`、明文 `git://`、`git+http://`。
5. 网络查询必须走 `spawn_blocking`（0.3.0 UI 卡死根因，勿在主线程发网络请求）。
6. 版本号格式 `valid_version`：`x.y.z` 或 `x.y.z-pre`（如 `0.1.1-rc.2`）。
7. 每个任务提交后跑：`cargo test` / `cargo clippy -D warnings` / `node --check apps/desktop/ui/shell.js`；批次完成后再加 review 子代理复审。
8. 不改动与本计划无关的代码；不提交工作区中与本计划无关的既有未提交改动（若有）。

## 现状说明（执行者必读：避免重复实现/冲突）

工作区存在**未提交的半成品**（上一个会话遗留，与本计划 V1 相关，本计划在它们基础上完成）：

- `apps/desktop/src-tauri/src/registry.rs`：`version_exists`（约 125 行）已实现但**缺 error 字段处理**（Task 1 完善）。
- `apps/desktop/src-tauri/src/main.rs`：`version_exists_cmd`（约 661 行）已实现但**未注册**到 `invoke_handler`（Task 2 补注册）；`list_dsh_versions_cmd` 已做 `.take(10)` 收敛（保持不动）。

开始执行前先 `git status` 确认这两个文件处于 modified 状态（如已被回退/提交，按各任务中的完整代码实现即可，任务代码是自足的）。

## 文件结构

| 文件 | 职责 | 涉及任务 |
|---|---|---|
| `apps/desktop/src-tauri/src/registry.rs` | registry 查询：`version_exists`（补 error 字段）+ 纯函数 `version_exists_response` + 单测 | Task 1 |
| `apps/desktop/src-tauri/src/main.rs` | Tauri 命令注册表；新增 `open_workbench_url_cmd` | Task 2, 10 |
| `apps/desktop/src-tauri/src/plugin.rs` | `valid_pkg_name` 两级校验重写 + 测试扩展 | Task 12 |
| `apps/desktop/ui/shell.html` | 结构：iframe allow、版本区、引导页版本输入、brand id、折叠 CSS、tab | Task 3, 4, 5, 8, 9 |
| `apps/desktop/ui/shell.js` | 壳页交互：版本渲染/安装、插件行卸载、快捷键、brand 刷新、tab 双击 | Task 3, 4, 6, 7, 9, 10 |
| `apps/desktop/ui/theme.css` | select 跨平台样式统一 | Task 11 |

---

# 批次 1：V1 版本管理

### Task 1: registry.rs `version_exists` 补 error 字段处理

**Files:**
- Modify: `apps/desktop/src-tauri/src/registry.rs:125-137`（现有半成品）
- Test: `apps/desktop/src-tauri/src/registry.rs` 的 `mod tests`（约 153 行）

**Interfaces:**
- Consumes: `PKG: &str = "@deepseek-ai/dsh"`、`Duration`、`Read`（文件头已 import）
- Produces: `pub fn version_exists(registry: &str, ver: &str) -> Result<bool, String>`、私有 `fn version_exists_response(body: &str) -> bool`——Task 2 的 `version_exists_cmd` 依赖前者

- [ ] **Step 1: 写失败测试**

在 `registry.rs` 的 `mod tests`（`use super::*` 之后）追加：

```rust
#[test]
fn version_exists_response_detects_error_body() {
    // 正常 200：版本元数据 JSON，无 error 字段 → 存在
    assert!(version_exists_response(r#"{"name":"@deepseek-ai/dsh","version":"0.1.0"}"#));
    // 部分 registry 镜像对不存在的版本返回 200 + {"error": ...} → 不存在
    assert!(!version_exists_response(r#"{"error":"Not found"}"#));
    assert!(!version_exists_response(r#"{"error":"version not found: 9.9.9"}"#));
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cd apps/desktop/src-tauri && cargo test version_exists_response`
Expected: 编译失败，报 `cannot find function version_exists_response`（函数尚未定义）

- [ ] **Step 3: 实现**

把现有 `version_exists`（约 125 行）替换为：

```rust
/// 200 响应体是否表示「版本存在」。部分 registry 镜像对不存在的版本返回
/// 200 + `{"error": ...}`，仅按状态码会误判为存在，需按 body 判断。
fn version_exists_response(body: &str) -> bool {
    !body.contains("\"error\"")
}

pub fn version_exists(registry: &str, ver: &str) -> Result<bool, String> {
    let url = format!("{registry}/{PKG}/{ver}");
    match ureq::get(&url).timeout(Duration::from_secs(20)).call() {
        Ok(resp) => {
            let mut body = String::new();
            resp.into_reader()
                .take(16 << 20)
                .read_to_string(&mut body)
                .map_err(|e| format!("读取 registry 响应失败: {e}"))?;
            Ok(version_exists_response(&body))
        }
        Err(ureq::Error::Status(404, _)) => Ok(false),
        Err(e) => Err(format!("查询版本存在性失败: {e}")),
    }
}
```

（与 `list_versions` 的 body 读取方式保持一致：`into_reader().take(16 << 20)`）

- [ ] **Step 4: 跑测试确认通过**

Run: `cd apps/desktop/src-tauri && cargo test version_exists_response`
Expected: PASS（`version_exists_response` 3 个断言全过）

- [ ] **Step 5: 全量测试 + clippy + 提交**

```bash
cd apps/desktop/src-tauri && cargo test && cargo clippy -D warnings
git add apps/desktop/src-tauri/src/registry.rs
git commit -m "feat(registry): version_exists 补 200+error 字段判断（镜像兼容）"
```

### Task 2: main.rs 注册 `version_exists_cmd`

**Files:**
- Modify: `apps/desktop/src-tauri/src/main.rs:1630-1650`（`invoke_handler` 列表）

**Interfaces:**
- Consumes: `version_exists_cmd`（约 661 行半成品，签名 `(app: AppHandle, window: WebviewWindow, ver: String) -> Result<bool, String>`，内部已含 `ensure_shell_window` + `valid_version` + `spawn_blocking`——**保持不动**）
- Produces: 前端可 `invoke('version_exists_cmd', { ver })`——Task 3/4 依赖

- [ ] **Step 1: 注册命令**

在 `main.rs` 的 `invoke_handler(tauri::generate_handler![...])` 列表中、`list_dsh_versions_cmd,` 之后追加一行：

```rust
            version_exists_cmd,
```

- [ ] **Step 2: 编译验证**

Run: `cd apps/desktop/src-tauri && cargo build`
Expected: 编译通过（如半成品已被回退，先按 spec V1 补实现再注册：`ensure_shell_window` → `valid_version` 校验 → `spawn_blocking` 调 `crate::registry::version_exists`，参见 spec 第 28-29 行）

- [ ] **Step 3: 跑全量测试 + 提交**

```bash
cd apps/desktop/src-tauri && cargo test && cargo clippy -D warnings
git add apps/desktop/src-tauri/src/main.rs
git commit -m "feat(main): 注册 version_exists_cmd 命令（下载前版本校验）"
```

### Task 3: 前端 dsh tab——删搜索框、最近 10 个、输入版本安装

**Files:**
- Modify: `apps/desktop/ui/shell.html:426-429`（`.version-search` 块）
- Modify: `apps/desktop/ui/shell.js:483-562`（`dshRender` / `renderVersions` / 搜索框监听）

**Interfaces:**
- Consumes: `invoke('version_exists_cmd', { ver }) -> bool`（Task 2）、`updateDsh(ver)`（shell.js:563 已有，不改）、`setDshStatus(text, kind)`（shell.js:438 已有）、`dshVersionsEl`（`$('dshVersions')`）
- Produces: `btnInstallVersion` 点击流程；`renderVersions` 新签名不变（`(versions, current, latest, installing, installed)`）但语义改为「最近 10 个、无本地过滤」

- [ ] **Step 1: HTML——搜索框换为输入版本安装行**

`shell.html` 中删除：

```html
            <div class="version-search">
              <input id="dshVersionSearch" type="text" placeholder="搜索全部已发布版本…" autocomplete="off" spellcheck="false" />
              <span class="version-search-hint">搜索的是全部已发布版本（含未显示的），输入版本号片段如 “0.1.1”</span>
            </div>
```

替换为：

```html
            <div class="version-install">
              <input id="dshVersionInput" type="text" placeholder="输入版本号安装，如 0.1.1-rc.2" autocomplete="off" spellcheck="false" />
              <button id="btnInstallVersion">安装</button>
            </div>
```

- [ ] **Step 2: JS——清理搜索框相关代码**

`shell.js` 中：
1. 删除 `const dshRender = {...}` 缓存对象与渲染函数内的 `dshRender.xxx = xxx;` 赋值（spec：移除全量缓存，不再需要）；
2. `renderVersions` 内删除 `const q = ...`、`const matching = ...`、`const slice = q ? matching : ...` 三行，改为 `const slice = versions.slice(0, 10);`，`RECENT` 常量（当前为 5）一并删除；
3. 删除渲染尾部搜索提示分支（`if (q && !matching.length) {...} else if (!q && versions.length > slice.length)`），只保留：

```js
    const tip = document.createElement('div');
    tip.className = 'empty';
    if (versions.length > slice.length) {
      tip.textContent = '共 ' + versions.length + ' 个版本，默认展示最近 ' + slice.length + ' 个；可输入版本号安装任意已发布版本';
    }
    if (tip.textContent) dshVersionsEl.appendChild(tip);
```

4. 删除文件末尾的搜索框监听：

```js
  // 搜索框输入 → 用缓存数据本地过滤重渲（不重新网络拉取）
  dshVersionSearch.addEventListener('input', () => {
    renderVersions(dshRender.versions, dshRender.current, dshRender.latest, dshRender.installing, dshRender.installed);
  });
```

（`renderVersions` 的 `dshRender` 引用全部移除后，`refreshDsh` 调用处 `renderVersions(st.versions, ...)` 签名不变，无需改。）

- [ ] **Step 3: JS——新增输入版本安装逻辑**

在 `updateDsh` 定义之后追加：

```js
  // 输入版本号安装：先校验版本存在（避免不存在的版本白白触发大下载），再走 updateDsh
  const dshVersionInput = $('dshVersionInput');
  const btnInstallVersion = $('btnInstallVersion');
  btnInstallVersion.addEventListener('click', async () => {
    const ver = dshVersionInput.value.trim();
    if (!ver) { setDshStatus('请输入版本号', 'err'); return; }
    btnInstallVersion.disabled = true;
    try {
      const exists = await invoke('version_exists_cmd', { ver });
      if (!exists) { setDshStatus('版本 ' + ver + ' 不存在', 'err'); return; }
      await updateDsh(ver);
    } catch (e) {
      setDshStatus('版本校验失败：' + (e.message || e), 'err');
    } finally {
      btnInstallVersion.disabled = false;
    }
  });
  dshVersionInput.addEventListener('keydown', (e) => { if (e.key === 'Enter') btnInstallVersion.click(); });
```

- [ ] **Step 4: 语法检查**

Run: `node --check apps/desktop/ui/shell.js`
Expected: 无输出（通过）。若 `node` 不可用，用 `apps/desktop/src-tauri/resources/node` 内捆绑 node 或 `scripts/` 下的 node 路径（见项目 README 环境说明）。

- [ ] **Step 5: 渲染验证（手动）**

Run: `cd apps/desktop && cargo tauri dev`（或按项目既有发版/调试方式）
验证点：
1. dsh tab「版本列表」无搜索框，显示「输入版本号安装」行；
2. 列表只显示最近 10 个版本，底部提示「共 N 个版本，默认展示最近 10 个…」；
3. 输入 `0.0.1`（不存在）→ 提示「版本 0.0.1 不存在」；输入当前最新版 → 安装流程正常；
4. 引导页「高级选项 → 版本」下拉仍能加载（Task 4 再改输入）。

- [ ] **Step 6: 提交**

```bash
git add apps/desktop/ui/shell.html apps/desktop/ui/shell.js
git commit -m "feat(shell): dsh tab 版本列表改最近 10 个 + 输入版本号安装（去搜索框）"
```

### Task 4: 引导页输入版本安装入口

**Files:**
- Modify: `apps/desktop/ui/shell.html:378-381`（`setupVer` 的 `label.field`）
- Modify: `apps/desktop/ui/shell.js:217-223`（`runSetup` 的 ver 获取处）

**Interfaces:**
- Consumes: `invoke('version_exists_cmd', { ver })`（Task 2）、`showSetupError(title, msg)`（shell.js:186 已有）、`setupVer` / `setupVerInput`
- Produces: 无新接口

- [ ] **Step 1: HTML——版本下拉下加输入框**

`shell.html` 中把：

```html
                <label class="field">
                  <span class="label-text">版本</span>
                  <select id="setupVer"><option value="">加载中…</option></select>
                </label>
```

替换为：

```html
                <label class="field">
                  <span class="label-text">版本</span>
                  <select id="setupVer"><option value="">加载中…</option></select>
                  <input class="input mono" id="setupVerInput" type="text" placeholder="或输入指定版本号（如 0.1.1-rc.2）" spellcheck="false" style="margin-top:8px" />
                </label>
```

- [ ] **Step 2: JS——runSetup 优先用输入值并校验**

`shell.js` 的 `runSetup` 中，把：

```js
    const ver = setupVer.value;
    if (!ver) {
      setupStarting = false;
      showSetupError('无法开始安装', '版本列表为空。请检查网络连接或更换 Registry 源后重试。');
      return;
    }
```

替换为：

```js
    const inputVer = setupVerInput.value.trim();
    const ver = inputVer || setupVer.value;
    if (!ver) {
      setupStarting = false;
      showSetupError('无法开始安装', '版本列表为空，或未输入版本号。请检查网络连接或更换 Registry 源后重试。');
      return;
    }
    if (inputVer) {
      try {
        const exists = await invoke('version_exists_cmd', { ver: inputVer });
        if (!exists) {
          setupStarting = false;
          showSetupError('版本不存在', '版本 ' + inputVer + ' 不存在，请检查后重试。');
          return;
        }
      } catch (e) {
        setupStarting = false;
        showSetupError('版本校验失败', (typeof e === 'string' ? e : (e && e.message) || String(e)));
        return;
      }
    }
```

（`setupStarting` 复位模式与现有 `!ver` 分支一致；后续 `invoke('setup_dsh_cmd', { ver, registry })` 无需改动，`ver` 已是最终值。）

- [ ] **Step 3: 语法检查 + 渲染验证**

Run: `node --check apps/desktop/ui/shell.js`
Expected: 通过。再 `cargo tauri dev` 手动验证：引导页输入不存在版本 → 报「版本不存在」；输入有效版本（如当前最新版）→ 安装流程使用该版本。

- [ ] **Step 4: 提交**

```bash
git add apps/desktop/ui/shell.html apps/desktop/ui/shell.js
git commit -m "feat(shell): 引导页支持输入指定版本号安装（下载前校验存在性）"
```

---

# 批次 2：V2 / V3 / V4（修复类）

### Task 5: V2 iframe 剪贴板权限

**Files:**
- Modify: `apps/desktop/ui/shell.html:308`

- [ ] **Step 1: 加 allow 属性**

把：

```html
        <iframe id="workbenchFrame" title="DeepSeek Harness 工作台"></iframe>
```

替换为：

```html
        <iframe id="workbenchFrame" allow="clipboard-write" title="DeepSeek Harness 工作台"></iframe>
```

- [ ] **Step 2: 渲染验证（手动）**

`cargo tauri dev` → 打开工作台 → 点 AI 回答文本块右上角复制按钮 → 粘贴验证内容已入剪贴板（此前 NotAllowedError）。
Expected: 复制成功。（如 macOS WKWebView 首次仍失败，检查是否缺少用户手势/焦点——按钮点击即手势，属 WebView 平台差异，记录到 PR 说明。）

- [ ] **Step 3: 提交**

```bash
git add apps/desktop/ui/shell.html
git commit -m "fix(shell): workbench iframe 加 allow=clipboard-write 修复复制按钮无反应"
```

### Task 6: V3 插件行卸载按钮 + 二次确认

**Files:**
- Modify: `apps/desktop/ui/shell.js:709-747`（`refreshPlugins` 渲染）

**Interfaces:**
- Consumes: `invoke('plugin_op', { op, pkg })`（plugin.rs:355 已有，op=remove 支持）、`setPluginStatus(text, kind)`（已有）、`pluginOutput`（已有）、`refreshPlugins()`（自身）
- Produces: 行内「卸载」→「确认卸载」交互（3 秒未再点自动还原）

- [ ] **Step 1: 渲染逻辑加卸载按钮**

`refreshPlugins` 的 `arr.forEach((p) => {...})` 中，在 `row.appendChild(meta);` 之后、`pluginListEl.appendChild(row);` 之前插入：

```js
        if (p.installed) {
          const btn = document.createElement('button');
          btn.className = 'sm danger-ghost';
          btn.textContent = '卸载';
          btn.addEventListener('click', async () => {
            // 二次确认：第一次点击变「确认卸载」，3 秒未再点自动还原；再点才执行
            if (btn.textContent !== '确认卸载') {
              btn.textContent = '确认卸载';
              const t0 = Date.now();
              const timer = setInterval(() => {
                if (btn.textContent === '确认卸载' && Date.now() - t0 >= 3000) {
                  btn.textContent = '卸载';
                  clearInterval(timer);
                }
              }, 500);
              return;
            }
            btn.disabled = true;
            try {
              const text = await invoke('plugin_op', { op: 'remove', pkg: p.name });
              pluginOutput.hidden = false;
              pluginOutput.textContent = text;
              setPluginStatus('已完成，工作台正在重启…', 'ok');
            } catch (e) {
              const msg = typeof e === 'string' ? e : (e && e.message) || String(e);
              pluginOutput.textContent = msg;
              setPluginStatus('卸载失败，详见下方输出', 'err');
            } finally {
              refreshPlugins(); // 重渲后按钮状态自然复位
            }
          });
          row.appendChild(btn);
        }
```

- [ ] **Step 2: 语法检查**

Run: `node --check apps/desktop/ui/shell.js`
Expected: 通过

- [ ] **Step 3: 渲染验证（手动）**

`cargo tauri dev` → 插件 tab：
1. 已安装插件行出现「卸载」按钮；未安装插件行无按钮；
2. 点「卸载」→ 变「确认卸载」→ 3 秒不动自动还原；
3. 点「确认卸载」→ 执行 remove → 列表刷新、输出区显示结果。

- [ ] **Step 4: 提交**

```bash
git add apps/desktop/ui/shell.js
git commit -m "feat(shell): 插件行内卸载按钮 + 二次确认（防误触）"
```

### Task 7: V4 壳页 Cmd/Ctrl+C 复制选中文字

**Files:**
- Modify: `apps/desktop/ui/shell.js:62-84`（全局 keydown）

**Interfaces:**
- Consumes: 无新依赖（`document.getSelection` / `navigator.clipboard.writeText` 均为 Web API）
- Produces: 壳页文本选中后 Cmd/Ctrl+C 可复制

- [ ] **Step 1: keydown 加复制分支**

在 `window.addEventListener('keydown', ...)`（约 62 行）回调开头、现有 `⌘K` 分支之前插入：

```js
    // V4：Cmd/Ctrl+C 复制选中文字（仅壳页：输入框内走浏览器默认；
    // iframe 内焦点时 keydown 不会冒泡到父文档，天然不拦截 iframe 内复制）
    if ((e.metaKey || e.ctrlKey) && (e.key === 'c' || e.key === 'C')) {
      const t = e.target;
      if (t && (t.tagName === 'INPUT' || t.tagName === 'TEXTAREA' || t.tagName === 'SELECT' || t.isContentEditable)) return;
      const sel = document.getSelection();
      const text = sel ? sel.toString() : '';
      if (!text) return;
      e.preventDefault();
      navigator.clipboard.writeText(text).catch(() => {});
      return;
    }
```

- [ ] **Step 2: 语法检查**

Run: `node --check apps/desktop/ui/shell.js`
Expected: 通过

- [ ] **Step 3: 渲染验证（手动）**

`cargo tauri dev` → 壳页（dsh tab 输出区/插件输出区）选中文字 → Cmd+C → 粘贴成功；输入框内 Cmd+C 不受影响；工作台 iframe 内复制不受影响。

- [ ] **Step 4: 提交**

```bash
git add apps/desktop/ui/shell.js
git commit -m "feat(shell): 壳页 Cmd/Ctrl+C 复制选中文字（不拦截输入框与 iframe）"
```

---

# 批次 3：V5 / V6 / V7 / V8（体验增强）

### Task 8: V5 折叠动画完整收起

**Files:**
- Modify: `apps/desktop/ui/shell.html:138-139`（折叠态 transform）

**Interfaces:** 无新接口。`--dsh-topbar-h` 默认 46px、折叠 28px，差值为 18px（shell.html:83 注释确认）。

- [ ] **Step 1: translateY 与高度差同步**

把：

```css
      .titlebar.collapsed .brand { transform: translateY(-10px); }
      .titlebar.collapsed .tabs { transform: translate(-50%, -10px); } /* 覆盖居中位移，避免横向漂移 */
```

替换为：

```css
      .titlebar.collapsed .brand { transform: translateY(-18px); } /* 46→28px 高度差 18px，内容完整缩入 */
      .titlebar.collapsed .tabs { transform: translate(-50%, -18px); } /* 覆盖居中位移，避免横向漂移 */
```

- [ ] **Step 2: 渲染验证（手动）**

`cargo tauri dev` → 点击顶栏把手折叠 → 品牌与 tab 内容随顶栏完整收起（不再露出 10px 残影）；展开动画正常。

- [ ] **Step 3: 提交**

```bash
git add apps/desktop/ui/shell.html
git commit -m "fix(shell): 折叠动画 translateY 与顶栏高度差同步（-10px→-18px）"
```

### Task 9: V6 brand 点击刷新工作台

**Files:**
- Modify: `apps/desktop/ui/shell.html:284`（brand 元素加 id）
- Modify: `apps/desktop/ui/shell.js`（loadWorkbench 定义后加监听）

**Interfaces:**
- Consumes: `loadWorkbench(url, force)`（shell.js:110 已有）、`lastUrl`（闭包变量，已有）
- Produces: `.brand` 点击 → 重载工作台

- [ ] **Step 1: HTML 加 id**

把：

```html
      <div class="brand">
```

替换为：

```html
      <div class="brand" id="brand" role="button" tabindex="0" aria-label="刷新工作台" title="刷新工作台">
```

- [ ] **Step 2: JS 加点击监听**

在 `loadWorkbench` 函数定义之后（`// 加载 dsh 工作台：以「轮询 get_dsh_url」为主通道` 注释之前）插入：

```js
  // V6：点击品牌（图标+App名）刷新工作台（等同右键 reload）；未就绪时无操作
  const brandEl = $('brand');
  brandEl.addEventListener('click', () => { if (lastUrl) loadWorkbench(lastUrl, true); });
  brandEl.addEventListener('keydown', (e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); if (lastUrl) loadWorkbench(lastUrl, true); } });
```

- [ ] **Step 3: 语法检查 + 渲染验证**

Run: `node --check apps/desktop/ui/shell.js`
Expected: 通过。`cargo tauri dev` → 工作台加载后点品牌 → iframe 重载（不切 tab）；未就绪时点击无副作用。

- [ ] **Step 4: 提交**

```bash
git add apps/desktop/ui/shell.html apps/desktop/ui/shell.js
git commit -m "feat(shell): 点击品牌刷新工作台"
```

### Task 10: V7 双击工作台 tab 系统浏览器打开

**Files:**
- Modify: `apps/desktop/src-tauri/src/main.rs`（新增命令 + 注册）
- Modify: `apps/desktop/ui/shell.js`（tab 双击监听）

**Interfaces:**
- Consumes: `get_dsh_url()`（main.rs:1194 已有，`#[tauri::command]` 普通函数可直调）、`open_url(&str)`（main.rs:891 已有，私有函数）、`ensure_shell_window(&window)`（已有）、`invoke_handler` 列表
- Produces: `#[tauri::command] fn open_workbench_url_cmd(window: WebviewWindow) -> Result<(), String>`——前端 `invoke('open_workbench_url_cmd')`

- [ ] **Step 1: 新增命令**

在 `open_browser_cmd`（约 1242 行）附近追加：

```rust
/// V7：双击「工作台」tab → 用系统浏览器打开当前工作台 URL。
/// URL 为空（dsh 未就绪/启动中）时静默返回：双击无效、不提示（已确认）。
#[tauri::command]
fn open_workbench_url_cmd(window: tauri::WebviewWindow) -> Result<(), String> {
    crate::ensure_shell_window(&window)?;
    if let Some(url) = get_dsh_url() {
        open_url(&url);
    }
    Ok(())
}
```

- [ ] **Step 2: 注册命令**

在 `invoke_handler` 列表中 `open_browser_cmd,` 之后追加：

```rust
            open_workbench_url_cmd,
```

- [ ] **Step 3: 编译验证**

Run: `cd apps/desktop/src-tauri && cargo build && cargo test && cargo clippy -D warnings`
Expected: 全过

- [ ] **Step 4: JS 加双击监听**

在 `tabs.forEach((t) => t.addEventListener('click', () => selectTab(t.dataset.tab)));`（约 60 行）之后追加：

```js
  // V7：双击「工作台」tab → 系统浏览器打开当前工作台（URL 未就绪时后端静默忽略）
  const workbenchTab = [...tabs].find((t) => t.dataset.tab === 'workbench');
  if (workbenchTab) {
    workbenchTab.addEventListener('dblclick', () => {
      invoke('open_workbench_url_cmd').catch(() => {});
    });
  }
```

- [ ] **Step 5: 语法检查 + 渲染验证**

Run: `node --check apps/desktop/ui/shell.js`
Expected: 通过。`cargo tauri dev` → 工作台已加载时双击「工作台」tab → 系统默认浏览器打开当前工作台 URL；未就绪（启动中）双击无反应。

- [ ] **Step 6: 提交**

```bash
git add apps/desktop/src-tauri/src/main.rs apps/desktop/ui/shell.js
git commit -m "feat: 双击工作台 tab 用系统浏览器打开当前工作台（未就绪静默）"
```

### Task 11: V8 select 下拉样式统一

**Files:**
- Modify: `apps/desktop/ui/theme.css:364-378`（`select.input, select` 规则）

**Interfaces:** 无

- [ ] **Step 1: 加 appearance:none + 自定义箭头**

在 `theme.css` 的 `select.input, select { ... }` 规则内追加：

```css
  appearance: none;
  -webkit-appearance: none;
  padding-right: 28px;
  background-image: url("data:image/svg+xml;charset=utf-8,%3Csvg xmlns='http://www.w3.org/2000/svg' width='10' height='6' viewBox='0 0 10 6'%3E%3Cpath d='M1 1l4 4 4-4' stroke='%23888' stroke-width='1.5' fill='none' stroke-linecap='round'/%3E%3C/svg%3E");
  background-repeat: no-repeat;
  background-position: right 10px center;
```

（保留原 `background: rgba(255,255,255,0.04)` 底色行；`background-image` 与 `background` 简写共存时，`background-image` 须写在 `background` 之后才能生效——若原规则里 `background` 简写在前面，把 `background-image` 系列行放到规则块末尾。）

- [ ] **Step 2: 渲染验证（手动）**

`cargo tauri dev` → 引导页「高级选项 → 版本」下拉与 dsh tab 相关下拉：macOS 与 Windows 均显示统一暗色样式 + 右侧自定义箭头，无原生蓝底白字 select。

- [ ] **Step 3: 提交**

```bash
git add apps/desktop/ui/theme.css
git commit -m "style(theme): select 加 appearance:none + 自定义箭头（跨平台统一）"
```

---

# 批次 4：V9 插件安装支持非 npm 包规格

### Task 12: `valid_pkg_name` 两级校验 + 测试扩展

**Files:**
- Modify: `apps/desktop/src-tauri/src/plugin.rs:61-72`（`valid_pkg_name` 现有实现）
- Test: `apps/desktop/src-tauri/src/plugin.rs` 的 `pkg_name_validation`（约 460 行）

**Interfaces:**
- Consumes: 现有调用方不变：`plugin_op`（plugin.rs:367 校验）、`extract_pkg_names`（plugin.rs:180 校验）——二者继续调 `valid_pkg_name`
- Produces: `fn valid_pkg_name(s: &str) -> bool`（签名不变，语义按 spec V9 两级校验）

**规则（spec V9 定稿，逐条实现）：**
- 第一级前缀白名单：`@`、字母数字、`github:`、`gitlab:`、`bitbucket:`、`git+ssh://`、`git+https://`、`https://`、`http://`
- 第二级字符白名单：字母数字 + `@ . _ - / ~ : + # &`；`< > = ^` 仅限 `#semver:` 段
- `#` 参数段：`#<ref>`（hash/分支/tag，含 `/`、`v` 前缀）、`#semver:<range>`（`^ ~ < <= > >= v` 前缀）、`#path:<dir>`（以 `/` 开头）；`&` 分隔多参数
- 禁止：以 `-`/`.`/`_` 开头、空白、控制字符、`?` query、`| ; $ 反引号 " ' ( ) [ ] { } , * !`、`file:`/`jsr:`/`workspace:`、`git://`/`git+http://`、空参数段、长度 > 214

- [ ] **Step 1: 写失败测试**

把 `pkg_name_validation` 测试（约 460 行）整体替换为：

```rust
    #[test]
    fn pkg_name_validation() {
        // 合法：npm 包名
        for ok in ["@linxin666/dsh-web-ui-all", "lodash", "@scope/pkg-name", "a.b-c_d", "x~y", "pkg@1.0.0", "pkg@next"] {
            assert!(valid_pkg_name(ok), "{ok} 应合法");
        }
        // 合法：Git 简写 / 托管商简写
        for ok in [
            "kevva/is-positive",
            "kevva/is-positive#master",
            "zkochan/is-negative#heads/canary",
            "zkochan/is-negative#2.0.1",
            "andreineculau/npm-publish-git#v0.0.7",
            "github:kevva/is-positive",
            "gitlab:pnpm/git-resolver",
            "bitbucket:pnpmjs/git-resolver",
        ] {
            assert!(valid_pkg_name(ok), "{ok} 应合法");
        }
        // 合法：完整 Git URL / tarball（含 #semver:、#path:、& 组合）
        for ok in [
            "git+ssh://git@github.com:zkochan/is-negative.git#2.0.1",
            "git+https://github.com/zkochan/is-negative.git",
            "git+https://github.com/zkochan/is-negative.git#semver:^2.0.0",
            "https://github.com/zkochan/is-negative.git#v0.0.7",
            "https://github.com/indexzero/forever/tarball/v0.5.6",
            "kevva/is-positive#semver:<=v0.0.7",
            "RexSkz/test-git-subfolder-fetch#path:/packages/simple-react-app",
            "RexSkz/test-git-subdir-fetch.git#beta&path:/packages/simple-react-app",
        ] {
            assert!(valid_pkg_name(ok), "{ok} 应合法");
        }
        // 非法：原有注入面
        for bad in [
            "",
            "a b",
            "a$b",
            "a;rm",
            "a\"b",
            "a`b",
            "a\\b",
            "a|b",
            "a>b",
            "a?x=1",
            ".x",
            "-g",
            "--dir=/tmp",
            "--prefix=x",
            "..",
            &"x".repeat(215),
        ] {
            assert!(!valid_pkg_name(bad), "{bad:?} 应非法");
        }
        // 非法：spec V9 明确不开放的格式
        for bad in [
            "git:kevva/is-positive",          // 裸 git: 前缀（pnpm 中不存在）
            "git://github.com/a/b",           // 明文 git 协议
            "git+http://github.com/a/b",      // 明文 http 组合
            "file:../local-dir",              // file: 协议
            "jsr:@hono/hono",                 // JSR
            "workspace:*",                    // workspace 协议
            "./local-dir",                    // 本地路径
            "https://a.com/b.tgz?token=123",  // ? query 不开放
            "pkg@\">=0.1.0 <0.2.0\"",         // 含空格版本范围
            "x#",                             // 空参数段
            "#only-param",                    // 无主体
            "a&b",                            // & 出现在主体（非参数段）
            "kevva/is-positive#semver:1.0.0 <2.0.0", // semver 含空格
            "https://a.com/b;rm",             // 分号注入
        ] {
            assert!(!valid_pkg_name(bad), "{bad:?} 应非法");
        }
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cd apps/desktop/src-tauri && cargo test pkg_name_validation`
Expected: FAIL（现有实现拒绝 `kevva/is-positive` 等 git 类格式）

- [ ] **Step 3: 实现两级校验**

把 `valid_pkg_name`（约 61 行）整体替换为：

```rust
/// npm/pnpm 包规格宽松校验（spec V9 两级白名单）：
/// 1) 第一级前缀白名单：@ / 字母数字 / github: / gitlab: / bitbucket: /
///    git+ssh:// / git+https:// / https:// / http://；
/// 2) 第二级字符白名单：字母数字与 @ . _ - / ~ : + # &（< > = ^ 仅限
///    #semver: 段），# 后参数段支持 <ref> / semver: / path:（& 分隔）。
/// 防 pnpm 参数混淆（-g、--dir= 等以 - 开头）、防参数注入/路径穿越
/// （空白、控制字符、shell 元字符、? query、file:/jsr:/workspace:、
/// 明文 git://、本地路径均拒绝）。
fn valid_pkg_name(s: &str) -> bool {
    if s.is_empty() || s.len() > 214 {
        return false;
    }
    let lower = s.to_ascii_lowercase();
    let first = s.as_bytes()[0];
    let prefix_ok = first == b'@'
        || first.is_ascii_alphanumeric()
        || lower.starts_with("github:")
        || lower.starts_with("gitlab:")
        || lower.starts_with("bitbucket:")
        || lower.starts_with("git+ssh://")
        || lower.starts_with("git+https://")
        || lower.starts_with("https://")
        || lower.starts_with("http://");
    if !prefix_ok {
        return false;
    }
    if s.starts_with('.') || s.starts_with('_') || s.starts_with('-') {
        return false;
    }
    // 按 # 切分主体与参数段（URL 中 # 只作 git 参数起始符）
    let (body, params) = match s.split_once('#') {
        Some((b, p)) => (b, Some(p)),
        None => (s, None),
    };
    if body.is_empty()
        || !body
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'@' | b'.' | b'_' | b'-' | b'/' | b'~' | b':' | b'+'))
    {
        return false;
    }
    if let Some(p) = params {
        for seg in p.split('&') {
            if seg.is_empty() {
                return false;
            }
            let ok = if let Some(r) = seg.strip_prefix("semver:") {
                !r.is_empty()
                    && r.bytes().all(|b| {
                        b.is_ascii_alphanumeric()
                            || matches!(b, b'.' | b'-' | b'_' | b'+' | b'^' | b'~' | b'<' | b'>' | b'=' | b'v')
                    })
            } else if let Some(d) = seg.strip_prefix("path:") {
                !d.is_empty()
                    && d.starts_with('/')
                    && d.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'.' | b'_' | b'-' | b'~'))
            } else {
                // 裸 ref：commit hash / 分支（可含 /）/ tag（可含 v 前缀）
                seg.bytes().all(|b| {
                    b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b'/')
                })
            };
            if !ok {
                return false;
            }
        }
    }
    true
}
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cd apps/desktop/src-tauri && cargo test pkg_name_validation`
Expected: PASS（新合法/非法各 20+ 断言全过）

- [ ] **Step 5: 全量测试 + clippy + 提交**

```bash
cd apps/desktop/src-tauri && cargo test && cargo clippy -D warnings
git add apps/desktop/src-tauri/src/plugin.rs
git commit -m "feat(plugin): valid_pkg_name 两级校验支持 git 类/tarball 包规格（保持防注入）"
```

---

## 收尾（全部批次完成后）

- [ ] **Step 1: 插件输入框提示文案更新（spec V9 交互项）**

`shell.html` 约 462 行，把：

```html
              <span class="card-hint">支持 npm 包名或 scoped 包名；操作完成后工作台自动重启</span>
```

替换为：

```html
              <span class="card-hint">支持 npm 包名（@scope/pkg）与 Git/tarball 源（owner/repo、github:owner/repo、git+ssh://…、git+https://…、https://…tgz），可用 #ref、#semver:、#path: 指定版本；操作完成后工作台自动重启</span>
```

- [ ] **Step 2: 全量回归 + 提交**

```bash
cd apps/desktop/src-tauri && cargo test && cargo clippy -D warnings && node --check apps/desktop/ui/shell.js
git add apps/desktop/ui/shell.html
git commit -m "docs(shell): 插件输入框提示更新为支持 git 类/tarball 源"
```

- [ ] **Step 3: 全量手动回归（渲染验证）**

`cargo tauri dev` 逐项验证 V1–V9 全部行为（版本管理、剪贴板、卸载二次确认、复制快捷键、折叠动画、品牌刷新、双击打开、select 样式、git 源安装——git 源安装可先用 `kevva/is-positive` 或本机可达的 tarball URL 试装）。

- [ ] **Step 4: 构建 v0.3.1 dmg 供实测（spec 第 3 节）**

按项目既有发版脚本（`scripts/` 目录）本地构建 dmg。
