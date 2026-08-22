# DSh Desktop 瘦壳重设计（v0.3）实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把 DSh Desktop 从「内置 340MB dsh 闭包的胖壳」重构为「只内置 node+npm+pnpm 的瘦壳」：dsh 首次运行按需安装、支持指定版本安装/回滚，App 内更新，插件列表展示，砍掉局域网远程连接等偏离功能。

**Architecture:** Tauri 2 + Rust 后端（模块化：registry/dsh/plugin/appupdate）+ 原生 HTML/JS 壳页（零构建链）。闭包安装在 `<app-data>/dsh/v<ver>/`，`current` 文本标记原子切换，保留上一版本回滚，GC 更旧版本。dsh 数据（`~/.dsh`）与终端共用。

**Tech Stack:** Rust（tauri 2 / serde / ureq 2，全部为现有依赖树成员，不新增第三方依赖）、原生 HTML/CSS/JS（壳页）、内置 node v24 + npm + pnpm（运行时资源）。

**Spec:** `docs/superpowers/specs/2026-08-22-thin-shell-redesign-design.md`

## Global Constraints

- 不新增任何第三方依赖（Rust crate / npm 包 / 前端库均不允许；只允许 `serde_json`/`ureq`/`std` 等现有依赖树成员）。
- 运行时自包含：内置 node + npm + pnpm，不依赖主机 node/npm/bun/python 等任何环境。
- 壳页零构建链、零前端依赖：`ui/` 下纯静态 HTML/CSS/JS，`frontendDist: ../ui` 直接嵌入。
- 平台：macOS（aarch64/x86_64）+ Windows x64；不支持 Linux。
- `~/.dsh`（会话/凭据/配置）永不自动删除；卸载提供「保留/全删」两档。
- dsh 更新默认手动确认（提示 → 用户点更新才安装）；不自动安装新版本。
- 闭包安装失败安全：`v<ver>-tmp` → 双重自检（`--version` + `--profile web --dump-default-config`）→ 原子切换 `current` 标记；任何失败不动当前版本。
- 保留当前 + 上一版本（回滚用），GC 更旧版本。
- npm registry 可配置：默认 `https://registry.npmjs.org`，支持 npmmirror。
- 测试一律 `cd apps/desktop/src-tauri && cargo test`；新逻辑必须带模块内 `#[cfg(test)]` 单测。
- 代码注释与 UI 文案用简体中文（沿用现状风格）。
- 产品功能边界（不得越界）：只做 免终端启动 / dsh 生命周期管理 / App 自身更新 / 插件管理 四件事；不做 dsh 内部功能、不做远程访问、不做工作目录/自启/手动重启/日志 UI。

---

### Task 1: registry.rs — npm registry 查询模块

**Files:**
- Create: `apps/desktop/src-tauri/src/registry.rs`
- Modify: `apps/desktop/src-tauri/src/main.rs`（加 `mod registry;`）
- Test: 模块内 `#[cfg(test)] mod tests`（registry.rs 内）

**Interfaces:**
- Produces:
  - `pub fn registry_url(registry: Option<&str>) -> String` — 规范 registry 基址（去尾斜杠，空值回退默认）
  - `pub fn cmp_versions(a: &str, b: &str) -> std::cmp::Ordering` — semver-ish 比较（处理 `0.1.0-rc.6`；release > prerelease）
  - `pub fn parse_versions(body: &str) -> Vec<String>` — 解析 registry 完整包元数据 JSON 的 `versions` 键，按 cmp_versions **倒序**（最新在前）
  - `pub fn latest_version(registry: &str) -> Result<String, String>` — GET `{registry}/@deepseek-ai/dsh/latest`，取 `version` 字段
  - `pub fn list_versions(registry: &str) -> Result<Vec<String>, String>` — GET `{registry}/@deepseek-ai/dsh`，返回 `parse_versions` 结果

- [ ] **Step 1: 写失败测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::cmp::Ordering;

    #[test]
    fn cmp_versions_orders_rc_and_release() {
        assert_eq!(cmp_versions("0.1.1-rc.2", "0.1.1-rc.1"), Ordering::Greater);
        assert_eq!(cmp_versions("0.1.0-rc.7", "0.1.1-rc.1"), Ordering::Less);
        assert_eq!(cmp_versions("0.1.0", "0.1.0-rc.1"), Ordering::Greater); // release > prerelease
        assert_eq!(cmp_versions("0.1.0-rc.10", "0.1.0-rc.9"), Ordering::Greater);
        assert_eq!(cmp_versions("0.1.0-rc.7", "0.1.0-rc.7"), Ordering::Equal);
    }

    #[test]
    fn parse_versions_sorts_desc() {
        let body = r#"{"versions":{
            "0.1.0-rc.2":{},"0.1.1-rc.1":{},"0.1.0-rc.6":{},
            "0.1.1-rc.2":{},"0.0.1-rc.1":{},"0.1.0":{}
        }}"#;
        assert_eq!(
            parse_versions(body),
            vec!["0.1.1-rc.2", "0.1.1-rc.1", "0.1.0", "0.1.0-rc.6", "0.1.0-rc.2", "0.0.1-rc.1"]
        );
    }

    #[test]
    fn parse_versions_tolerates_garbage() {
        assert!(parse_versions("not json").is_empty());
        assert!(parse_versions(r#"{"versions":null}"#).is_empty());
    }

    #[test]
    fn registry_url_normalizes() {
        assert_eq!(registry_url(None), "https://registry.npmjs.org");
        assert_eq!(registry_url(Some("https://registry.npmmirror.com/")), "https://registry.npmmirror.com");
        assert_eq!(registry_url(Some("  ")), "https://registry.npmjs.org");
    }
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cd apps/desktop/src-tauri && cargo test registry`
Expected: FAIL（`registry` 模块不存在）

- [ ] **Step 3: 实现 registry.rs**

```rust
//! npm registry 查询（dsh 版本发现）。仅依赖 std + serde_json + ureq（均已在依赖树）。

use std::cmp::Ordering;
use std::io::Read;
use std::time::Duration;

const PKG: &str = "@deepseek-ai/dsh";
const DEFAULT_REGISTRY: &str = "https://registry.npmjs.org";

/// Canonical registry base URL (no trailing slash).
pub fn registry_url(registry: Option<&str>) -> String {
    match registry {
        Some(r) if !r.trim().is_empty() => r.trim_end_matches('/').to_string(),
        _ => DEFAULT_REGISTRY.to_string(),
    }
}

/// Simple semver-ish comparator (handles `0.1.0-rc.6`).
pub fn cmp_versions(a: &str, b: &str) -> Ordering {
    let (am, asuf) = a.split_once('-').unwrap_or((a, ""));
    let (bm, bsuf) = b.split_once('-').unwrap_or((b, ""));
    let ap: Vec<u64> = am.split('.').filter_map(|s| s.parse().ok()).collect();
    let bp: Vec<u64> = bm.split('.').filter_map(|s| s.parse().ok()).collect();
    for (x, y) in ap.iter().zip(bp.iter()) {
        match x.cmp(y) {
            Ordering::Equal => {}
            o => return o,
        }
    }
    match ap.len().cmp(&bp.len()) {
        Ordering::Equal => {}
        o => return o,
    }
    // release (no suffix) > pre-release; pre-release suffix compared with
    // numeric segment awareness: rc.10 > rc.9 (string compare would say rc.9
    // > rc.10, which mis-orders real version lists)
    match (asuf.is_empty(), bsuf.is_empty()) {
        (true, true) => Ordering::Equal,
        (true, false) => Ordering::Greater,
        (false, true) => Ordering::Less,
        (false, false) => {
            let an: Vec<&str> = asuf.split('.').collect();
            let bn: Vec<&str> = bsuf.split('.').collect();
            for (x, y) in an.iter().zip(bn.iter()) {
                match (x.parse::<u64>(), y.parse::<u64>()) {
                    (Ok(a), Ok(b)) if a != b => return a.cmp(&b),
                    _ => match x.cmp(y) {
                        Ordering::Equal => {}
                        o => return o,
                    },
                }
            }
            an.len().cmp(&bn.len())
        }
    }
}

/// Parse `versions` keys from a full registry package document, newest first.
pub fn parse_versions(body: &str) -> Vec<String> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else {
        return vec![];
    };
    let Some(map) = v.get("versions").and_then(|x| x.as_object()) else {
        return vec![];
    };
    let mut vs: Vec<String> = map.keys().cloned().collect();
    vs.sort_by(|a, b| cmp_versions(b, a));
    vs
}

/// Query the registry for the `latest` dist-tag version.
pub fn latest_version(registry: &str) -> Result<String, String> {
    let url = format!("{registry}/{PKG}/latest");
    let resp = ureq::get(&url)
        .timeout(Duration::from_secs(20))
        .call()
        .map_err(|e| format!("查询 {url} 失败: {e}"))?;
    let mut body = String::new();
    resp.into_reader()
        .take(1 << 20)
        .read_to_string(&mut body)
        .map_err(|e| format!("读取 registry 响应失败: {e}"))?;
    let v: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| format!("解析 registry 响应失败: {e}"))?;
    v.get("version")
        .and_then(|x| x.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| "registry 响应缺少 version 字段".into())
}

/// List every published version of `@deepseek-ai/dsh`, newest first.
pub fn list_versions(registry: &str) -> Result<Vec<String>, String> {
    let url = format!("{registry}/{PKG}");
    let resp = ureq::get(&url)
        .timeout(Duration::from_secs(20))
        .call()
        .map_err(|e| format!("查询 {url} 失败: {e}"))?;
    let mut body = String::new();
    resp.into_reader()
        .take(4 << 20)
        .read_to_string(&mut body)
        .map_err(|e| format!("读取 registry 响应失败: {e}"))?;
    Ok(parse_versions(&body))
}
```

- [ ] **Step 4: 在 main.rs 声明模块并运行测试**

Modify `apps/desktop/src-tauri/src/main.rs`：在 `mod update;`（第 29 行）旁加 `mod registry;`

Run: `cd apps/desktop/src-tauri && cargo test registry`
Expected: PASS（4 个测试全绿）

- [ ] **Step 5: Commit**

```bash
cd /Users/chj/agentProjects/dsh-desktop
git add apps/desktop/src-tauri/src/registry.rs apps/desktop/src-tauri/src/main.rs
git commit -m "feat: registry 查询模块（latest + 全版本列表，semver 倒序）"
```

---

### Task 2: dsh.rs — 闭包管理（从 update.rs 迁移并通用化）

**Files:**
- Create: `apps/desktop/src-tauri/src/dsh.rs`
- Delete: `apps/desktop/src-tauri/src/update.rs`
- Modify: `apps/desktop/src-tauri/src/main.rs`（`mod update;` → `mod dsh;`；`update::` 引用全部改 `crate::dsh::`；`notify` 函数迁入 main.rs）
- Test: dsh.rs 模块内测试

**Interfaces:**
- Consumes: `crate::Paths`、`crate::node_bin`、`crate::no_console`（main.rs 需改为 `pub(crate)`）
- Produces:
  - `pub fn app_data_from_home() -> PathBuf` — 从 update.rs 原样迁移
  - `pub fn closure_version(dir: &Path) -> Option<String>` — 从 update.rs 原样迁移
  - `pub fn current_closure(p: &Paths) -> Option<PathBuf>` — 从 update.rs `active_closure` + main.rs `resolve_closure`/`closure_marker` 迁移合并（读 `<app-data>/dsh/current` 标记 → `v<ver>` 目录）
  - `pub fn installed_versions(p: &Paths) -> Vec<String>` — 新增：扫 `<app-data>/dsh/` 下 `v*` 目录名，倒序
  - `pub fn install_version(p: &Paths, ver: &str, registry: &str, progress: &dyn Fn(&str)) -> Result<(), String>` — 通用安装：`v<ver>-tmp` → npm install（内置 node+npm，`NODE_OPTIONS=--max-old-space-size=6144`）→ 双重自检 → VERSION 标记 → promote → 原子切换 `current` → GC（keep = 新版 + 上一版）
  - `pub fn check_update(p: &Paths, registry: Option<&str>) -> Result<Option<(String, String)>, String>` — 从 update.rs 迁移（`(current, latest)`）

- [ ] **Step 1: 写失败测试（dsh.rs 核心纯逻辑）**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tmp() -> PathBuf {
        std::env::temp_dir().join(format!("dsh-test-{}", std::process::id()))
    }

    #[test]
    fn closure_version_reads_marker_then_package_json() {
        let root = tmp().join("closure_version");
        let _ = std::fs::remove_dir_all(&root);
        let dir = root.join("v0.1.1-rc.2");
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(closure_version(&dir), None);
        std::fs::write(dir.join("VERSION"), "0.1.1-rc.2\n").unwrap();
        assert_eq!(closure_version(&dir).as_deref(), Some("0.1.1-rc.2"));
        std::fs::write(
            dir.join("package.json"),
            r#"{"name":"dsh-closure","version":"0.1.0-rc.7"}"#,
        )
        .unwrap();
        // VERSION marker 优先
        assert_eq!(closure_version(&dir).as_deref(), Some("0.1.1-rc.2"));
        std::fs::remove_file(dir.join("VERSION")).unwrap();
        assert_eq!(closure_version(&dir).as_deref(), Some("0.1.0-rc.7"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn installed_versions_lists_desc() {
        let root = tmp().join("installed");
        let _ = std::fs::remove_dir_all(&root);
        // 规格 4.3 布局：闭包在 <app-data>/dsh/v<ver>/
        for v in ["v0.1.0-rc.7", "v0.1.1-rc.1", "v0.1.1-rc.2"] {
            std::fs::create_dir_all(root.join("dsh").join(v)).unwrap();
        }
        std::fs::create_dir_all(root.join("dsh/npm-cache")).unwrap(); // 非 v* 应忽略
        let p = crate::Paths {
            resources: tmp(),
            app_data: root.clone(),
        };
        assert_eq!(
            installed_versions(&p),
            vec!["0.1.1-rc.2", "0.1.1-rc.1", "0.1.0-rc.7"]
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn current_closure_requires_valid_dir() {
        let root = tmp().join("current_marker");
        let _ = std::fs::remove_dir_all(&root);
        // current 标记 + 闭包目录 + node_modules/@deepseek-ai/dsh 三者齐备才算有效
        let ver_dir = root.join("dsh/v0.1.1-rc.2");
        std::fs::create_dir_all(ver_dir.join("node_modules/@deepseek-ai/dsh")).unwrap();
        std::fs::create_dir_all(root.join("dsh/npm-cache")).unwrap();
        std::fs::write(root.join("dsh/current"), "v0.1.1-rc.2\n").unwrap();
        let p = crate::Paths { resources: tmp(), app_data: root.clone() };
        assert_eq!(
            current_closure(&p).unwrap().file_name().unwrap().to_string_lossy(),
            "v0.1.1-rc.2"
        );
        std::fs::write(root.join("dsh/current"), "v0.9.9\n").unwrap(); // 指向不存在 → None
        assert!(current_closure(&p).is_none());
        std::fs::write(root.join("dsh/current"), "v0.1.1-rc.2\n").unwrap();
        std::fs::remove_dir_all(ver_dir.join("node_modules")).unwrap(); // 无 node_modules → None
        assert!(current_closure(&p).is_none());
        let _ = std::fs::remove_dir_all(&root);
    }
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cd apps/desktop/src-tauri && cargo test dsh::`
Expected: FAIL（`dsh` 模块不存在）

- [ ] **Step 3: 实现 dsh.rs**

从 `update.rs` 迁移以下函数（**原样复制**，仅把 `use crate::Paths;` 保留）：`app_data_from_home`、`closure_version`、`registry_url` 不需要（registry.rs 已有）、`cmp_versions` 不需要、`latest_version` 不需要、`install_and_verify`（改造为内部函数，加 `progress` 回调）、`apply_update`（改造为 `install_version`）、`check_update`、`notify`（**迁入 main.rs**，见下）。

`install_version` 的完整实现（替代原 `apply_update` + 内联 `install_and_verify`）：

```rust
/// Install `@deepseek-ai/dsh@ver` into the app-data dir and switch `current`
/// to it atomically. `progress` is called at stage transitions so the shell
/// UI can render meaningful steps (npm output is not streamed).
pub fn install_version(
    p: &Paths,
    ver: &str,
    registry: &str,
    progress: &dyn Fn(&str),
) -> Result<(), String> {
    let dsh_dir = p.app_data.join("dsh");
    std::fs::create_dir_all(&dsh_dir).map_err(|e| format!("创建目录失败: {e}"))?;

    let tmp = dsh_dir.join(format!("v{ver}-tmp"));
    let final_dir = dsh_dir.join(format!("v{ver}"));
    if tmp.exists() {
        std::fs::remove_dir_all(&tmp).map_err(|e| format!("清理临时目录失败: {e}"))?;
    }
    std::fs::create_dir_all(&tmp).map_err(|e| format!("创建临时目录失败: {e}"))?;

    progress(&format!("正在下载并安装 dsh v{ver}（约 300MB，首次可能需要几分钟）…"));
    if let Err(e) = install_and_verify(p, &tmp, ver, registry) {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(e);
    }

    progress("正在校验新版本…");
    std::fs::write(tmp.join("VERSION"), ver).map_err(|e| format!("写版本标记失败: {e}"))?;

    // promote tmp -> v<ver> with overwrite safety: the existing dir is moved
    // aside first, so any failure below never leaves the running install
    // half-removed (spec 6: 任何失败不动当前可用版本)。
    let old = dsh_dir.join(format!("v{ver}.old"));
    if final_dir.exists() {
        let _ = std::fs::remove_dir_all(&old);
        std::fs::rename(&final_dir, &old).map_err(|e| format!("移开旧版本目录失败: {e}"))?;
    }
    if let Err(e) = std::fs::rename(&tmp, &final_dir) {
        // 恢复被移开的旧目录，保证当前版本仍然可用
        if old.exists() {
            let _ = std::fs::rename(&old, &final_dir);
        }
        return Err(format!("发布新版本目录失败: {e}"));
    }
    if old.exists() {
        let _ = std::fs::remove_dir_all(&old);
    }

    let cur_marker = dsh_dir.join("current");
    let prev_ver = std::fs::read_to_string(&cur_marker)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let tmp_marker = dsh_dir.join("current.tmp");
    std::fs::write(&tmp_marker, format!("v{ver}\n"))
        .map_err(|e| format!("写 current 标记失败: {e}"))?;
    std::fs::rename(&tmp_marker, &cur_marker)
        .map_err(|e| format!("切换 current 标记失败: {e}"))?;

    // GC: keep the new version + the previous one (rollback), drop older ones.
    if let Ok(entries) = std::fs::read_dir(&dsh_dir) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if !name.starts_with('v') || name.ends_with("-tmp") || name.ends_with(".old") {
                continue;
            }
            let keep = name == format!("v{ver}") || Some(&name) == prev_ver.as_ref();
            if !keep && e.path().is_dir() {
                let _ = std::fs::remove_dir_all(e.path());
                eprintln!("[dsh] dropped stale closure {name}");
            }
        }
    }
    progress("完成");
    Ok(())
}
```

`install_and_verify`（迁移自 update.rs，加 `progress` 回调；其余逻辑原样）：

```rust
/// The npm install child PID while an install is running (for cancel).
static SETUP_CHILD: Mutex<Option<u32>> = Mutex::new(None);

/// Cancel a running install (kill the npm child; install_version then fails,
/// cleans tmp, and the caller can retry). Best-effort per platform.
pub fn cancel_install() {
    let pid = SETUP_CHILD.lock().unwrap().take();
    if let Some(pid) = pid {
        #[cfg(unix)]
        {
            let _ = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
        }
        #[cfg(windows)]
        {
            let _ = std::process::Command::new("taskkill")
                .args(["/PID", &pid.to_string(), "/T", "/F"])
                .spawn();
        }
    }
}

fn install_and_verify(
    p: &Paths,
    target: &Path,
    ver: &str,
    registry: &str,
) -> Result<(), String> {
    let node = crate::node_bin(&p.resources);
    let npm = p.resources.join("npm/bin/npm-cli.js");
    if !node.is_file() || !npm.is_file() {
        return Err(format!(
            "内置 node/npm 缺失 (node={}, npm={})",
            node.display(),
            npm.display()
        ));
    }
    let mut cmd = Command::new(&node);
    #[cfg(target_os = "windows")]
    {
        cmd = crate::no_console(cmd);
    }
    let mut child = cmd
        .arg(&npm)
        .arg("install")
        .arg("--prefix")
        .arg(target)
        .arg(format!("@deepseek-ai/dsh@{ver}"))
        .args(["--ignore-scripts", "--no-audit", "--no-fund", "--loglevel=error"])
        .arg("--registry")
        .arg(registry)
        .arg("--cache")
        .arg(p.app_data.join("dsh/npm-cache"))
        .env("NODE_OPTIONS", "--max-old-space-size=6144")
        .current_dir(target)
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| format!("运行内置 npm 失败: {e}"))?;
    // 记录子进程 PID 供 setup_cancel_cmd 取消；wait 结束后清除
    *SETUP_CHILD.lock().unwrap() = child.id();
    let status = child.wait().map_err(|e| format!("等待内置 npm 失败: {e}"))?;
    *SETUP_CHILD.lock().unwrap() = None;
    if !status.success() {
        return Err(format!("npm install @deepseek-ai/dsh@{ver} 失败 (exit {status})"));
    }
    let bin = target.join("node_modules/@deepseek-ai/dsh/lib/bin.js");
    let mut ver_cmd = Command::new(&node);
    #[cfg(target_os = "windows")]
    {
        ver_cmd = crate::no_console(ver_cmd);
    }
    let out = ver_cmd
        .arg(&bin)
        .arg("--version")
        .output()
        .map_err(|e| format!("校验新闭包版本失败: {e}"))?;
    let got = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if got != ver {
        return Err(format!("新闭包版本校验失败: 期望 {ver}, 实际 {got}"));
    }
    let mut comp_cmd = Command::new(&node);
    #[cfg(target_os = "windows")]
    {
        comp_cmd = crate::no_console(comp_cmd);
    }
    let composed = comp_cmd
        .arg(&bin)
        .arg("--profile")
        .arg("web")
        .arg("--dump-default-config")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| format!("校验 web profile 组合失败: {e}"))?;
    if !composed.success() {
        return Err("新闭包无法组合 web profile".into());
    }
    Ok(())
}
```

`current_closure`（合并 main.rs 的 `active_closure`/`resolve_closure`/`closure_marker`）：

```rust
/// The active closure dir: `<app-data>/dsh/current` marker -> `v<ver>/`.
pub fn current_closure(p: &Paths) -> Option<PathBuf> {
    let marker = p.app_data.join("dsh/current");
    let name = std::fs::read_to_string(&marker).ok()?.trim().to_string();
    if name.is_empty() || !name.starts_with('v') {
        return None;
    }
    let dir = p.app_data.join("dsh").join(name);
    if !dir.join("node_modules/@deepseek-ai/dsh").is_dir() {
        return None;
    }
    Some(dir)
}

/// Version dir names under `<app-data>/dsh/`, newest first.
pub fn installed_versions(p: &Paths) -> Vec<String> {
    let mut out = vec![];
    if let Ok(entries) = std::fs::read_dir(p.app_data.join("dsh")) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if let Some(ver) = name.strip_prefix('v') {
                if !ver.ends_with("-tmp") && e.path().is_dir() {
                    out.push(ver.to_string());
                }
            }
        }
    }
    out.sort_by(|a, b| crate::registry::cmp_versions(b, a));
    out
}
```

`check_update` 从 update.rs 迁移（改为调用 `crate::registry::latest_version`，其余原样）：

```rust
pub fn check_update(
    p: &Paths,
    registry: Option<&str>,
) -> Result<Option<(String, String)>, String> {
    let reg = crate::registry::registry_url(registry);
    let latest = crate::registry::latest_version(&reg)?;
    let current = current_closure(p)
        .and_then(|c| closure_version(&c))
        .unwrap_or_else(|| "unknown".into());
    Ok(if crate::registry::cmp_versions(&current, &latest) == std::cmp::Ordering::Less {
        Some((current, latest))
    } else {
        None
    })
}
```

- [ ] **Step 4: 改造 main.rs 引用并删除 update.rs**

1. `mod update;` → `mod dsh;`；`use std::process::{Command, Stdio};` 保留在 dsh.rs。
2. `update::` 引用逐一改写：
   - `update::app_data_from_home`（main.rs:141/309）→ `crate::dsh::app_data_from_home`
   - `update::closure_version`（main.rs:1457）→ `crate::dsh::closure_version`
   - `update::check_update`（main.rs:1542/1704/2580）→ `crate::dsh::check_update`
   - `update::registry_url`（main.rs:1551/2594）→ `crate::registry::registry_url`
   - `update::apply_update`（main.rs:1552/2595）→ `crate::dsh::install_version(&p, &ver, &reg, &|_msg| {})`（`check_update_cmd` 与 24h 检查的「装完重启」行为在 Task 4 的 `update_dsh_cmd` 统一承载；本任务先以 `install_version` 替换、保持编译与运行语义：装完调用 `restart_dsh` 的重启逻辑保留在 `check_update_cmd` 内）
   - `update::notify`（main.rs:1554/1557/1645/1672/1681/1705/2012/2017/2266/2457）→ `notify`（迁入 main.rs 的 `pub(crate) fn notify`，内容原样）
3. `notify` 函数从 update.rs 迁入 main.rs（改成 `pub(crate) fn notify(title: &str, body: &str)`，内容原样），main.rs 内原 `update::notify(...)` 调用改 `notify(...)`。
4. main.rs 中 `active_closure`、`resolve_closure`、`closure_marker`、`resolve_current`（main.rs:545）四个函数删除（已被 `dsh::current_closure` 替代）；调用点改 `crate::dsh::current_closure`。
5. 让 `paths_from_app`/`paths_from_cli`/`node_bin`/`no_console`/`mlock`/`logln`/`home_dir` 以及 `boot`/`spawn_dsh`/`kill_dsh`/`restart_dsh`/`reveal_main_window`/`WINDOW_LABEL`/`APP_ID` 变为 `pub(crate)`（供 dsh.rs、plugin.rs、appupdate.rs 使用）。`logln` 宏（main.rs:205 `macro_rules! logln`）定义在 `mod` 声明之后、跨模块不可见——**外部模块一律用 `crate::logln(&format!(...))` 函数形式**，不依赖宏导出。
6. `run_cli_hooks`（main.rs:2576）改写：`--self-update-check` → `crate::dsh::check_update`（输出格式不变）；`--self-apply-update` → `crate::dsh::install_version(&p, &ver, &reg, &|_msg| {})`（输出 `APPLIED {ver}` 不变）。`--self-login-item` 分支与 `set_login_item_core` 保留到 Task 7 随登录自启一并删除。保留理由：README 记录的无头验证工具，App 内版本管理上线后仍可用于脚本化验证。
7. 删除 `apps/desktop/src-tauri/src/update.rs`。
8. 此时 `check_update_cmd`（main.rs 1538 行附近）与 24h 定时检查、卸载处的调用全部保持原逻辑（引用已改），**编译通过即可**——这些 command 的最终形态在 Task 4/7 处理。

- [ ] **Step 5: 编译 + 测试**

Run: `cd apps/desktop/src-tauri && cargo test`
Expected: PASS（原有 main.rs 测试 + 新增 3 个 dsh 测试；`cargo build` 无警告错误）

- [ ] **Step 6: Commit**

```bash
cd /Users/chj/agentProjects/dsh-desktop
git add -A apps/desktop/src-tauri
git commit -m "refactor: update.rs 迁移为 dsh.rs 闭包管理（install_version 通用化 + installed_versions + current_closure）"
```

---

### Task 3: 首次引导安装流程（main.rs boot 改造 + setup command）

**Files:**
- Modify: `apps/desktop/src-tauri/src/main.rs`、`apps/desktop/src-tauri/src/dsh.rs`
- Test: 无新单测（引导流程为进程级行为，靠 Task 9 的 UI 手动回归；本任务以编译 + 事件正确发行为验收）

**Interfaces:**
- Consumes: `dsh::install_version`、`dsh::current_closure`
- Produces（command，注册进 `invoke_handler`）:
  - `#[tauri::command] async fn setup_dsh_cmd(app: AppHandle, ver: String, registry: String) -> Result<(), String>` — 安装指定版本；进度经 `app.emit("dsh:setup-progress", msg)` 推送；成功后 `boot(app)` 启动工作台
  - `#[tauri::command] fn setup_state_cmd(app: AppHandle) -> SetupState` — `{ installing: bool, current: Option<String> }`（installing 为全局 `SETUP_BUSY` 状态）
  - `#[tauri::command] fn list_dsh_versions_cmd(app: AppHandle) -> Vec<String>` — `registry::list_versions`（失败返回空 vec）
  - `#[tauri::command] fn setup_cancel_cmd()` — 调用 `crate::dsh::cancel_install()` 终止进行中的 npm 安装（规格 5.1「可取消」）；取消后安装返回 Err → tmp 清理 → 引导页可重试
- 事件：`"dsh:need-setup"`（payload `()`）——boot 发现无闭包时发出（仅辅助；shell.js 初始化先查 `setup_state_cmd`，见 Task 9，避免事件在监听前发出而丢失）
- 全局状态：`static SETUP_BUSY: AtomicBool`（防重复安装）

- [ ] **Step 1: 修复资源根探测（瘦壳后无 `resources/dsh` 目录）**

`resource_dir` 与 `paths_from_cli` 目前用 `cand.join("dsh").is_dir()` 判断资源根。改为检测内置 node：

```rust
/// Resources root contains the bundled node runtime (node/bin/node or
/// node/node.exe). No dsh closure is bundled anymore (thin shell).
fn has_runtime(dir: &Path) -> bool {
    dir.join("node/bin/node").is_file() || dir.join("node/node.exe").is_file()
}
```

`resource_dir` 内 `if cand.join("dsh").is_dir()` → `if has_runtime(&cand)`；`paths_from_cli` 的 `.find(|p| p.join("dsh").is_dir())` → `.find(|p| has_runtime(p))`。

- [ ] **Step 2: boot() 无闭包时进入引导模式**

`boot()` 开头（`spawn_dsh` 之前）插入：

```rust
fn boot(app: AppHandle) {
    // thin shell: no bundled closure — first run must install dsh first
    let p = paths_from_app(&app);
    if crate::dsh::current_closure(&p).is_none() {
        logln!("no dsh closure installed; entering setup mode");
        let _ = app.emit("dsh:need-setup", ());
        reveal_main_window(&app, None);
        return;
    }
    let mut child = match spawn_dsh(&app) { /* 原逻辑不变 */ };
    // ... 原逻辑不变
}
```

（`reveal_main_window` 需在 `boot` 中可见——已存在；`emit` 需要 `use tauri::Emitter;`，确认 main.rs 顶部有该 use，没有则补。）

- [ ] **Step 3: 新增 SetupState + 三个 command**

```rust
#[derive(Serialize)]
struct SetupState {
    installing: bool,
    current: Option<String>,
}

static SETUP_BUSY: AtomicBool = AtomicBool::new(false);

#[tauri::command]
async fn setup_dsh_cmd(app: AppHandle, ver: String, registry: String) -> Result<(), String> {
    if SETUP_BUSY.swap(true, Ordering::SeqCst) {
        return Err("已有一个安装正在进行中".to_string());
    }
    let result = tauri::async_runtime::spawn_blocking(move || {
        let p = paths_from_app(&app);
        let reg = crate::registry::registry_url(Some(&registry));
        let app2 = app.clone();
        crate::dsh::install_version(&p, &ver, &reg, &|msg| {
            let _ = app2.emit("dsh:setup-progress", msg);
        })
    })
    .await;
    // join 失败（线程 panic）也必须在返回前释放锁，否则永久锁死（审查项）
    SETUP_BUSY.store(false, Ordering::SeqCst);
    result.map_err(|e| format!("安装线程异常：{e}"))??;
    if let Err(e) = result {
        return Err(e);
    }
    // 安装成功 → 启动工作台（boot 会 emit dsh:url，壳页自动切入工作台）
    let app2 = app.clone();
    let _ = app2.run_on_main_thread(move || boot(app2));
    Ok(())
}

#[tauri::command]
fn setup_state_cmd(app: AppHandle) -> SetupState {
    let p = paths_from_app(&app);
    SetupState {
        installing: SETUP_BUSY.load(Ordering::SeqCst),
        current: crate::dsh::current_closure(&p).and_then(|d| crate::dsh::closure_version(&d)),
    }
}

#[tauri::command]
fn list_dsh_versions_cmd(app: AppHandle) -> Vec<String> {
    let p = paths_from_app(&app);
    let reg = crate::registry::registry_url(load_settings_at(&p.app_data).registry.as_deref());
    crate::registry::list_versions(&reg).unwrap_or_default()
}
```

- [ ] **Step 4: 注册 command**

`invoke_handler` 的 `generate_handler![...]` 中加入 `setup_dsh_cmd, setup_state_cmd, list_dsh_versions_cmd`。

- [ ] **Step 5: 编译**

Run: `cd apps/desktop/src-tauri && cargo build`
Expected: 编译通过（`cargo test` 依旧全绿）

- [ ] **Step 6: Commit**

```bash
cd /Users/chj/agentProjects/dsh-desktop
git add apps/desktop/src-tauri/src
git commit -m "feat: 首次引导安装流程（无闭包→need-setup→setup_dsh_cmd 安装→boot）"
```

---

### Task 4: dsh 版本管理 command + 启动静默检查

**Files:**
- Modify: `apps/desktop/src-tauri/src/main.rs`、`apps/desktop/src-tauri/src/dsh.rs`
- Test: 无新单测（registry/dsh 纯逻辑已在 Task 1/2 覆盖；本任务为 command 装配）

**Interfaces:**
- Produces:
  - `static LATEST_DSH: Mutex<Option<String>>` — 启动时异步查询的 latest 缓存
  - `#[tauri::command] fn get_dsh_state(app: AppHandle) -> DshState` — `{ current: String, latest: Option<String>, versions: Vec<String>, installing: bool }`
  - `#[tauri::command] async fn update_dsh_cmd(app: AppHandle, ver: String) -> Result<(), String>` — 安装指定版本 → `restart_dsh(app)` 自动重启工作台
  - 删除旧 `check_update_cmd`（main.rs 1538 行附近，含其 24h 自动检查与 notify 逻辑——静默检查改为启动时一次，见 Step 2）

```rust
#[derive(Serialize)]
struct DshState {
    current: String,
    latest: Option<String>,
    versions: Vec<String>,
    installing: bool,
}
```

- [ ] **Step 1: 启动静默检查（替代旧 24h 定时检查）**

在 `main.rs` 的 `.setup()` 中（或 boot 首次调用处）加入：

```rust
static LATEST_DSH: Mutex<Option<String>> = Mutex::new(None);

// setup() 内：启动时异步查一次 latest，失败静默（离线/断网不打扰）
{
    let app = app.handle().clone();
    std::thread::spawn(move || {
        let p = paths_from_app(&app);
        let reg = crate::registry::registry_url(load_settings_at(&p.app_data).registry.as_deref());
        if let Ok(v) = crate::registry::latest_version(&reg) {
            *mlock(&LATEST_DSH) = Some(v);
        }
    });
}
```

- [ ] **Step 2: 实现 get_dsh_state / update_dsh_cmd，删除 check_update_cmd**

```rust
#[tauri::command]
fn get_dsh_state(app: AppHandle) -> DshState {
    let p = paths_from_app(&app);
    let reg = crate::registry::registry_url(load_settings_at(&p.app_data).registry.as_deref());
    let current = crate::dsh::current_closure(&p)
        .and_then(|d| crate::dsh::closure_version(&d))
        .unwrap_or_else(|| "未安装".into());
    DshState {
        latest: mlock(&LATEST_DSH).clone(),
        current,
        versions: crate::registry::list_versions(&reg).unwrap_or_default(),
        installing: SETUP_BUSY.load(Ordering::SeqCst),
    }
}

#[tauri::command]
async fn update_dsh_cmd(app: AppHandle, ver: String) -> Result<(), String> {
    if SETUP_BUSY.swap(true, Ordering::SeqCst) {
        return Err("已有一个安装正在进行中".to_string());
    }
    let result = tauri::async_runtime::spawn_blocking(move || {
        let p = paths_from_app(&app);
        let reg = crate::registry::registry_url(load_settings_at(&p.app_data).registry.as_deref());
        crate::dsh::install_version(&p, &ver, &reg, &|_msg| {})
    })
    .await;
    // join 失败（线程 panic）也必须在返回前释放锁（审查项）
    SETUP_BUSY.store(false, Ordering::SeqCst);
    result.map_err(|e| format!("安装线程异常：{e}"))??;
    // 自动重启工作台（新版本生效）
    let app2 = app.clone();
    let _ = app2.run_on_main_thread(move || restart_dsh(&app2));
    Ok(())
}
```

删除 `check_update_cmd` 函数及其调用的 `update::check_update`/`apply_update` 残留（Task 2 后这些是 `dsh::` 引用）；`invoke_handler` 中 `check_update_cmd` 移除，加入 `get_dsh_state, update_dsh_cmd`。

- [ ] **Step 3: 编译 + 全量测试**

Run: `cd apps/desktop/src-tauri && cargo test`
Expected: PASS

- [ ] **Step 4: Commit**

```bash
cd /Users/chj/agentProjects/dsh-desktop
git add apps/desktop/src-tauri/src
git commit -m "feat: dsh 版本管理 command（get_dsh_state/update_dsh_cmd + 启动静默检查）"
```

---

### Task 5: plugin.rs — 插件列表 + 自动重启

**Files:**
- Create: `apps/desktop/src-tauri/src/plugin.rs`
- Modify: `apps/desktop/src-tauri/src/main.rs`（`mod plugin;`；删除原 plugin 相关函数）
- Test: plugin.rs 模块内测试

**Interfaces:**
- Consumes: `crate::Paths`、`crate::home_dir`、`crate::restart_dsh`、`crate::logln`
- Produces:
  - `pub struct PluginInfo { pub name: String, pub version: String, pub installed: bool }`
  - `pub fn list_installed_plugins(profile_dir: &Path) -> Vec<PluginInfo>` — 读 `profile_dir/package.json` 的 `dependencies`+`devDependencies`，`installed` = `node_modules/<name>` 目录存在，按名称排序
  - `#[tauri::command] async fn plugin_op(app: AppHandle, window: tauri::WebviewWindow, op: String, pkg: String) -> Result<String, String>` — 从 main.rs 原样迁移；**成功后自动 `crate::restart_dsh(&app)` 重启工作台**（替代原「请重启工作台生效」提示）
  - `#[tauri::command] fn plugin_list_cmd(app: AppHandle) -> Vec<PluginInfo>` — 读 `home_dir().join(".dsh/profiles/web")`

- [ ] **Step 1: 写失败测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tmp() -> PathBuf {
        std::env::temp_dir().join(format!("plugin-test-{}", std::process::id()))
    }

    #[test]
    fn list_plugins_reads_profile_package_json() {
        let root = tmp().join("web");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("node_modules/@linxin666/dsh-web-ui-all")).unwrap();
        std::fs::write(
            root.join("package.json"),
            r#"{
                "dependencies": {
                    "@linxin666/dsh-web-ui-all": "^1.2.0",
                    "dsh-plugin-a": "0.5.0"
                },
                "devDependencies": { "dsh-plugin-dev": "0.1.0" }
            }"#,
        )
        .unwrap();
        let list = list_installed_plugins(&root);
        assert_eq!(list.len(), 3);
        let ui = list.iter().find(|p| p.name == "@linxin666/dsh-web-ui-all").unwrap();
        assert_eq!(ui.version, "^1.2.0");
        assert!(ui.installed);
        let a = list.iter().find(|p| p.name == "dsh-plugin-a").unwrap();
        assert!(!a.installed); // node_modules 里没有
        // 按名称排序
        let names: Vec<&str> = list.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["@linxin666/dsh-web-ui-all", "dsh-plugin-a", "dsh-plugin-dev"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn list_plugins_tolerates_missing_profile() {
        let root = tmp().join("nonexistent");
        let _ = std::fs::remove_dir_all(&root);
        assert!(list_installed_plugins(&root).is_empty());
        std::fs::create_dir_all(&root).unwrap();
        assert!(list_installed_plugins(&root).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cd apps/desktop/src-tauri && cargo test plugin::`
Expected: FAIL（`plugin` 模块不存在）

- [ ] **Step 3: 实现 plugin.rs**

```rust
//! dsh 插件管理：web profile 已装插件列表 + 安装/卸载（复用原 main.rs 的 pnpm 逻辑）。

use serde::Serialize;

#[derive(Serialize)]
pub struct PluginInfo {
    pub name: String,
    pub version: String,
    pub installed: bool,
}

/// Read installed plugins from `~/.dsh/profiles/web/package.json`
/// (dependencies + devDependencies), newest-independent, sorted by name.
pub fn list_installed_plugins(profile_dir: &std::path::Path) -> Vec<PluginInfo> {
    let mut out = vec![];
    let Ok(raw) = std::fs::read_to_string(profile_dir.join("package.json")) else {
        return out;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return out;
    };
    for section in ["dependencies", "devDependencies"] {
        let Some(deps) = v.get(section).and_then(|x| x.as_object()) else {
            continue;
        };
        for (name, ver) in deps {
            let installed = profile_dir.join("node_modules").join(name).is_dir();
            out.push(PluginInfo {
                name: name.clone(),
                version: ver.as_str().unwrap_or("").to_string(),
                installed,
            });
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

// 以下从 main.rs 迁移（内容原样，仅改 crate 路径与自动重启）：
//   valid_pkg_name, ensure_pnpm_workspace, extract_pkg_names,
//   prepend_path, bundled_pnpm_file_name, clean_empty_dirs_under,
//   remove_empty_tree, tail_text（run_dsh_plugin 依赖，必须一并迁移！）, run_dsh_plugin
// 迁移后这些函数保持私有（fn 而非 pub），plugin_op 内部使用。
// 注意：logln 宏跨模块不可见（宏定义在 main.rs 的 mod 声明之后），
// 本模块内一律用 `crate::logln(&format!(...))` 函数形式。
```

`plugin_op`（迁移后改造尾部）：

```rust
#[tauri::command]
pub async fn plugin_op(
    app: AppHandle,
    window: tauri::WebviewWindow,
    op: String,
    pkg: String,
) -> Result<String, String> {
    if !matches!(window.label(), crate::WINDOW_LABEL) {
        return Err("该操作仅限壳页使用".to_string());
    }
    if op != "add" && op != "remove" {
        return Err(format!("不支持的插件操作：{op}（仅支持 add / remove）"));
    }
    if !valid_pkg_name(&pkg) {
        return Err("包名不合法（仅允许字母、数字与 @ . _ - / ~，且不能以 - 开头）".to_string());
    }
    tauri::async_runtime::spawn_blocking(move || {
        let (mut output, mut success) = run_dsh_plugin(&app, &op, &pkg, &[])?;
        for _ in 0..2 {
            if success {
                break;
            }
            let hits_ignored = output.contains("ERR_PNPM_IGNORED_BUILDS")
                || output.to_lowercase().contains("ignored build scripts");
            if !hits_ignored {
                break;
            }
            let extra = extract_pkg_names(&output);
            if extra.is_empty() {
                break;
            }
            crate::logln(&format!("[plugin] auto-approving build scripts: {extra:?}"));
            (output, success) = run_dsh_plugin(&app, &op, &pkg, &extra)?;
        }
        if !success {
            return Err(output);
        }
        // 插件变更后自动重启工作台生效（替代原「请手动重启」提示）
        let app2 = app.clone();
        let _ = app2.run_on_main_thread(move || crate::restart_dsh(&app2));
        Ok(output)
    })
    .await
    .map_err(|e| format!("插件操作线程异常：{e}"))?
}
```

`plugin_list_cmd`：

```rust
#[tauri::command]
pub fn plugin_list_cmd(app: AppHandle) -> Vec<PluginInfo> {
    let profile = crate::home_dir().join(".dsh/profiles/web");
    list_installed_plugins(&profile)
}
```

- [ ] **Step 4: main.rs 删除原 plugin 函数并注册 command**

删除 main.rs 中：`plugin_op`（1216 行附近）、`valid_pkg_name`、`ensure_pnpm_workspace`、`extract_pkg_names`、`tail_text`、`remove_empty_tree`、`clean_empty_dirs_under`、`prepend_path`、`bundled_pnpm_file_name`、`run_dsh_plugin` 及其依赖的静态量（如 `PLUGIN_LOCK`）。`invoke_handler` 中 `plugin_op` 路径改为 `plugin::plugin_op`，并加入 `plugin::plugin_list_cmd`。

注意：`main.rs` 中 `mod plugin;` 声明；plugin.rs 需要访问 `crate::home_dir`/`crate::logln`/`crate::restart_dsh`/`crate::WINDOW_LABEL`——确认这些为 `pub(crate)`。

- [ ] **Step 5: 编译 + 测试**

Run: `cd apps/desktop/src-tauri && cargo test`
Expected: PASS（plugin 2 个新测试 + 原有测试；build 无错）

- [ ] **Step 6: Commit**

```bash
cd /Users/chj/agentProjects/dsh-desktop
git add apps/desktop/src-tauri/src
git commit -m "feat: plugin.rs 插件列表 + 装/卸后自动重启工作台"
```

---

### Task 6: appupdate.rs — App 自身更新

**Files:**
- Create: `apps/desktop/src-tauri/src/appupdate.rs`
- Modify: `apps/desktop/src-tauri/src/main.rs`（`mod appupdate;` + 注册 command）
- Test: 模块内可测纯逻辑（URL 构造、tag 解析）

**Interfaces:**
- Produces:
  - `pub fn parse_tag_from_effective_url(final_url: &str) -> Option<String>` — 从跳转后 URL 解析 tag（`.../releases/tag/v0.3.0` → `0.3.0`）
  - `pub fn asset_url(ver: &str) -> Option<String>` — 按平台构造下载 URL：
    - macOS aarch64: `https://github.com/Jedeiah/dsh-desktop/releases/download/v{ver}/DeepSeek.Harness_{ver}_aarch64.dmg`
    - macOS x86_64: `.../DeepSeek.Harness_{ver}_x86_64.dmg`
    - Windows: `.../DeepSeek.Harness_{ver}_x64-setup.exe`
    - 其他平台 → None
  - `pub fn latest_app_version() -> Result<String, String>` — GET `https://github.com/Jedeiah/dsh-desktop/releases/latest`（跟随跳转），从 `resp.get_url()` 解析 tag
  - `pub fn download_installer(url: &str, dest: &Path) -> Result<u64, String>` — ureq 流式下载到 dest，返回字节数；>2GB 拒绝
  - `#[tauri::command] fn check_app_update_cmd() -> Option<String>` — `latest_app_version()` 成功返回 Some(ver)，失败/无新版返回 None（UI 兜底提示）
  - `#[tauri::command] async fn app_update_cmd(app: AppHandle) -> Result<(), String>` — 下载 → 安装 → 退出当前实例 → 启动新版

- [ ] **Step 1: 写失败测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_tag_from_effective_url() {
        assert_eq!(
            parse_tag_from_effective_url("https://github.com/Jedeiah/dsh-desktop/releases/tag/v0.3.1").as_deref(),
            Some("0.3.1")
        );
        assert_eq!(parse_tag_from_effective_url("https://github.com/Jedeiah/dsh-desktop/releases/tag/v0.3.1?foo=1").as_deref(), Some("0.3.1"));
        assert_eq!(parse_tag_from_effective_url("https://github.com/other/releases/tag/v1.0.0").as_deref(), Some("1.0.0"));
        assert_eq!(parse_tag_from_effective_url("https://example.com/404"), None);
        assert_eq!(parse_tag_from_effective_url(""), None);
    }

    #[test]
    fn asset_url_built_per_platform() {
        // macOS CI 仅出 arm64 产物；x86_64 mac 无产物 → None（手动兜底）
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        {
            let url = asset_url("0.3.1").unwrap();
            assert!(url.contains("DeepSeek.Harness_0.3.1_aarch64.dmg"), "macOS arm64: {url}");
        }
        #[cfg(all(target_os = "macos", not(target_arch = "aarch64")))]
        assert!(asset_url("0.3.1").is_none(), "macOS x86_64 无 CI 产物");
        #[cfg(target_os = "windows")]
        assert!(
            asset_url("0.3.1").unwrap().ends_with("DeepSeek.Harness_0.3.1_x64-setup.exe"),
            "Windows: {}",
            asset_url("0.3.1").unwrap()
        );
    }
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cd apps/desktop/src-tauri && cargo test appupdate::`
Expected: FAIL

- [ ] **Step 3: 实现 appupdate.rs**

```rust
//! App 自身更新：GitHub Releases 检查 + 下载安装（macOS DMG / Windows NSIS）。
//! 失败安全：下载/安装任何一步失败都不影响当前运行版本。

use std::io::Read;
use std::path::Path;
use std::time::Duration;

const REPO: &str = "Jedeiah/dsh-desktop";

pub fn parse_tag_from_effective_url(final_url: &str) -> Option<String> {
    let idx = final_url.find("/releases/tag/")?;
    let tag = &final_url[idx + "/releases/tag/".len()..];
    let tag = tag.split(['?', '#']).next().unwrap_or(tag);
    tag.strip_prefix('v').map(|s| s.to_string())
}

/// Download URL for the current platform's installer (naming mirrors
/// release.yml + scripts/install.sh: GitHub replaces spaces with dots).
/// macOS CI 仅构建 arm64（macos-14 runner）——x86_64 无对应产物，返回 None，
/// 用户走「关于页手动下载」兜底（规格 5.3）。
pub fn asset_url(ver: &str) -> Option<String> {
    let base = format!("https://github.com/{REPO}/releases/download/v{ver}");
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    let name = format!("DeepSeek.Harness_{ver}_aarch64.dmg");
    #[cfg(target_os = "windows")]
    let name = format!("DeepSeek.Harness_{ver}_x64-setup.exe");
    #[cfg(not(any(
        all(target_os = "macos", target_arch = "aarch64"),
        target_os = "windows"
    )))]
    let name = return None;
    Some(format!("{base}/{name}"))
}

/// Resolve the newest release tag via the `/releases/latest` redirect.
pub fn latest_app_version() -> Result<String, String> {
    let url = format!("https://github.com/{REPO}/releases/latest");
    let resp = ureq::get(&url)
        .timeout(Duration::from_secs(20))
        .call()
        .map_err(|e| format!("查询 {url} 失败: {e}"))?;
    let final_url = resp.get_url().to_string();
    parse_tag_from_effective_url(&final_url)
        .ok_or_else(|| format!("无法从响应 URL 解析版本：{final_url}"))
}

/// Stream-download `url` to `dest`; verifies size against Content-Length when
/// the server provides it (规格 5.3：校验大小与 asset 一致；缺失/不符即失败清理，
/// 杜绝静默截断)。大文件下载用 30 分钟总超时——ureq 的 timeout 覆盖整个请求，
/// 数十秒的默认值必然中断数百 MB 的安装包下载。
pub fn download_installer(url: &str, dest: &Path) -> Result<u64, String> {
    let resp = ureq::get(url)
        .timeout(Duration::from_secs(1800))
        .call()
        .map_err(|e| format!("下载失败: {e}"))?;
    let expected = resp
        .header("Content-Length")
        .and_then(|s| s.parse::<u64>().ok());
    let mut reader = resp.into_reader().take(2 << 30);
    let mut f = std::fs::File::create(dest).map_err(|e| format!("创建临时文件失败: {e}"))?;
    let n = std::io::copy(&mut reader, &mut f).map_err(|e| format!("写入失败: {e}"))?;
    if let Some(exp) = expected {
        if n != exp {
            let _ = std::fs::remove_file(dest);
            return Err(format!("下载不完整: 期望 {exp} 字节, 实际 {n}"));
        }
    }
    Ok(n)
}

#[tauri::command]
pub fn check_app_update_cmd() -> Option<String> {
    latest_app_version().ok()
}

#[tauri::command]
pub async fn app_update_cmd(app: tauri::AppHandle) -> Result<(), String> {
    let ver = latest_app_version()?;
    let url = asset_url(&ver).ok_or_else(|| "当前平台暂不支持自动安装".to_string())?;
    let tmp = std::env::temp_dir().join(format!("dsh-desktop-update-{ver}"));
    let installer = match asset_url(&ver) {
        Some(u) if u.ends_with(".dmg") => tmp.with_extension("dmg"),
        Some(u) if u.ends_with(".exe") => tmp.with_extension("exe"),
        _ => return Err("未知安装包类型".to_string()),
    };
    let _ = std::fs::remove_file(&installer);

    tauri::async_runtime::spawn_blocking(move || {
        let mut dest = installer.clone();
        download_installer(&url, &dest)?;
        #[cfg(target_os = "macos")]
        install_macos(&installer)?;
        #[cfg(target_os = "windows")]
        install_windows(&installer)?;
        Ok::<(), String>(())
    })
    .await
    .map_err(|e| format!("更新线程异常：{e}"))??;

    // 安装成功 → 退出当前实例（安装器/新版会负责启动）
    app.exit(0);
    Ok(())
}
```

`install_macos` / `install_windows`（同文件内私有函数）：

```rust
#[cfg(target_os = "macos")]
fn install_macos(dmg: &Path) -> Result<(), String> {
    use std::process::Command;
    // 1. mount
    let out = Command::new("hdiutil")
        .args(["attach", "-nobrowse", "-readonly"])
        .arg(dmg)
        .output()
        .map_err(|e| format!("挂载 DMG 失败: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let mount = stdout
        .lines()
        .rev()
        .find_map(|l| l.split_whitespace().last().map(|s| s.to_string()))
        .ok_or_else(|| format!("无法解析挂载点:\n{stdout}"))?;
    // 2. find .app
    let app_name = std::fs::read_dir(&mount)
        .map_err(|e| format!("读取 DMG 内容失败: {e}"))?
        .flatten()
        .find(|e| e.path().extension().map(|x| x == "app").unwrap_or(false))
        .map(|e| e.file_name().to_string_lossy().to_string())
        .ok_or_else(|| "DMG 中未找到 .app".to_string())?;
    let src = std::path::Path::new(&mount).join(&app_name);
    let dst = std::path::Path::new("/Applications").join(&app_name);
    // 3. copy (plain first; escalate via osascript if permission denied)
    let cp = Command::new("ditto").arg(&src).arg(&dst).status();
    if !matches!(cp, Ok(s) if s.success()) {
        // 路径含单引号时按 shell 单引号规则转义（' → '\''），防提权脚本损坏
        let esc = |p: &Path| p.display().to_string().replace('\'', "'\\''");
        let script = format!(
            "do shell script \"ditto '{}' '{}'\" with administrator privileges",
            esc(&src),
            esc(&dst)
        );
        let ok = Command::new("osascript")
            .args(["-e", &script])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok {
            return Err("复制到 /Applications 失败（无写入权限且提权被取消）".to_string());
        }
    }
    // 4. detach (best-effort)
    let _ = Command::new("hdiutil").args(["detach"]).arg(&mount).output();
    let _ = Command::new("open").arg(&dst).spawn();
    Ok(())
}

#[cfg(target_os = "windows")]
fn install_windows(exe: &Path) -> Result<(), String> {
    use std::process::Command;
    let status = crate::no_console(Command::new(exe))
        .arg("/S")
        .status()
        .map_err(|e| format!("启动安装器失败: {e}"))?;
    if !status.success() {
        return Err(format!("安装器退出码异常: {status}"));
    }
    Ok(())
}
```

- [ ] **Step 4: main.rs 声明模块并注册 command**

`mod appupdate;`；`invoke_handler` 加入 `appupdate::check_app_update_cmd, appupdate::app_update_cmd`。

- [ ] **Step 5: 编译 + 测试**

Run: `cd apps/desktop/src-tauri && cargo test`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
cd /Users/chj/agentProjects/dsh-desktop
git add apps/desktop/src-tauri/src
git commit -m "feat: App 自身更新（Releases 检查 + DMG/EXE 下载安装，失败安全）"
```

---

### Task 7: 功能清理（LAN / 常规 / settings / 资源 / CI / 依赖）

**Files:**
- Modify: `apps/desktop/src-tauri/src/main.rs`、`apps/desktop/src-tauri/Cargo.toml`、`scripts/prepare-resources.sh`、`scripts/prepare-resources.ps1`、`.github/workflows/release.yml`
- Delete: `.github/workflows/upstream-check.yml`、`dsh.version`、`apps/desktop/src-tauri/lan-proxy.js`、`apps/desktop/src-tauri/mdns-advertise.js`、`apps/desktop/src-tauri/qrcode.js`、`apps/desktop/src-tauri/resources/lan-proxy.js`、`apps/desktop/src-tauri/resources/mdns-advertise.js`、`apps/desktop/src-tauri/resources/qrcode.js`、`apps/desktop/ui/qrcode.js`、`apps/desktop/src-tauri/resources/dsh/`（整个目录）
- Test: `cargo test` 全绿 + `cargo build` 无未使用告警

- [ ] **Step 1: main.rs 删除 LAN 全部代码**

按 grep 清单删除（均在 main.rs）：`LAN_CHILD`、`LAN_MUTEX`、`LAN_ON`、`LAN_GEN`、`LAN_ITEM`、`LAN_IP_LAST`、`LAN_IP_WATCH_GEN`、`LAN_LABEL` 静态量与常量；`lan_state`、`lan_toggle`、`lan_token`、`lan_ip`、`ensure_lan_on_ready`、`kill_lan`、`lan_status` 等函数；`Settings` 中 `lan_enabled`/`lan_port`/`lan_token`/`lan_pair` 字段与读写处；`boot()` 中 `ensure_lan_on_ready` 调用；`kill_dsh()` 中 `kill_lan()` 调用；`invoke_handler` 中 `lan_state, lan_toggle`；托盘菜单中 LAN 相关项。

- [ ] **Step 2: main.rs 删除「常规」相关**

删除：`login_item_enabled`/`set_login_item`/`set_login_item_core` 及登录自启逻辑、`choose_workspace_cmd`、`open_workspace_cmd`、`open_logs_cmd`、`restart_dsh_cmd`、`set_login_cmd`（**保留内部** `kill_dsh`/`restart_dsh`/`spawn_dsh`/`boot`/`open_browser_cmd`）；`run_cli_hooks` 中 `--self-login-item` 分支删除；`Settings` 中 `default_cwd` 字段与读写；`ShellState` 中 `login_on` 字段（注意：`login_on` 是 ShellState 的字段，不在 Settings 里）与读写；`ShellState` 精简为：

```rust
#[derive(Serialize)]
struct ShellState {
    app_version: String,
    dsh_version: String,
    registry: String,
}
```

`get_shell_state` 相应精简（`registry` 取 settings.registry 规范化值）。

- [ ] **Step 3: 清理 Cargo.toml 依赖**

删除 `arboard`（LAN 复制令牌专用）。`libc`（非阻塞 stdout + kill，保留）、`trash`（Windows 卸载回收站，保留）、`windows Win32_System_Power`（LAN 阻止休眠用——删除该 feature；若 `windows` crate 仍被 tauri/wry 传递引入则无需改动依赖树，仅移除 features 声明）、`rfd`（自绘弹窗已替代？确认 main.rs 无 `rfd::` 引用后删除）。逐一 grep 确认后再删，保持 `cargo build` 零告警。

- [ ] **Step 4: prepare-resources.sh / .ps1 瘦身**

删除：dsh 闭包打包段（"2. dsh closure"）、LAN proxy/mDNS/二维码段（"2b/2b2/2b2b"）、closure self-check 段；保留 node 运行时、npm、pnpm 段。脚本开头 `DSH_VERSION` 变量与 `VER` 逻辑删除。`CLOSURE_SRC` 相关逻辑删除。

- [ ] **Step 5: 删除资源与 CI**

- 删除 `resources/dsh/`、`resources/lan-proxy.js`、`resources/mdns-advertise.js`、`resources/qrcode.js`、`src-tauri/lan-proxy.js`、`src-tauri/mdns-advertise.js`、`src-tauri/qrcode.js`、`ui/qrcode.js`
- 删除 `.github/workflows/upstream-check.yml`、根目录 `dsh.version`
- `release.yml`：删除 "Resolve latest upstream dsh version (DSH_VERSION)" 与 closure 安装步骤（改为只准备 node/npm/pnpm：`NODE_SRC="$(command -v node)" ./scripts/prepare-resources.sh`）、删除 "Track released dsh baseline (dsh.version)" 步骤、release body 保留

- [ ] **Step 6: 编译 + 全量测试 + 清理告警**

Run: `cd apps/desktop/src-tauri && cargo test && cargo build 2>&1 | grep -c warning || true`
Expected: test PASS；warning 计数为 0（未使用 import/函数需删净，`#![allow(dead_code)]` 不允许新增）

- [ ] **Step 7: Commit**

```bash
cd /Users/chj/agentProjects/dsh-desktop
git add -A
git commit -m "refactor: 移除 LAN 远程连接/常规管理/内置 dsh 闭包/upstream 自动发版（瘦壳）"
```

---

### Task 8: open-design UI 设计（用户确认点）

> 本任务是**设计产出 + 用户确认 checkpoint**，不写 Rust/JS 实现。产出经用户确认后，Task 9 按其落地。

**Files:**
- Create: `docs/design/2026-08-22-shell-ui/`（设计稿 HTML/CSS 预览 + 说明）
- 使用: open-design MCP（本机已配置，状态 disabled → 需先连接）

**Interfaces:**
- Consumes: 规格第 7 节 UI 结构、Task 3/4/5/6 已定 command/事件名
- Produces（Task 9 的输入）:
  - 壳页 4 Tab 布局稿：工作台 / dsh / 插件 / 关于
  - 首次引导页布局稿（主窗首屏：进度 + 高级版本选择 + 错误重试）
  - 设计 token（配色/圆角/间距）落地清单 → `ui/theme.css`

- [ ] **Step 1: 启用 open-design MCP**

调用 `use_capability(action=call, capability_id="mcp-server:open-design")` 连接（authorized 但 disabled）。若连接失败，记录错误并降级：手工在 `docs/design/2026-08-22-shell-ui/` 用 HTML/CSS 搭设计稿（沿用现有紫粉蓝光斑玻璃拟态基调），并在提交信息中注明降级原因。

- [ ] **Step 2: 生成设计稿**

用 open-design 生成（或手工搭）以下页面的布局与视觉稿，保存到 `docs/design/2026-08-22-shell-ui/`：
1. `main-shell.html` — 壳页：顶栏 4 Tab（工作台/dsh/插件/关于）+ 工作台 iframe 区
2. `dsh-page.html` — dsh 管理：当前版本卡片、检查更新、版本列表（latest 徽标 + 全版本下拉/列表 + 安装/回滚按钮）
3. `plugins-page.html` — 插件列表（名称/版本/已安装状态）+ 安装输入 + 实时输出区
4. `about-page.html` — App 版本、检查更新、一键安装、卸载入口（两档）
5. `setup-page.html` — 首次引导：进度条/阶段文案、高级（registry 源 + 版本选择）、错误重试
6. `design-tokens.md` — 配色/圆角/间距/字体 token 清单（供 Task 9 落地 theme.css）

- [ ] **Step 3: 提交设计稿并展示**

```bash
cd /Users/chj/agentProjects/dsh-desktop
git add docs/design
git commit -m "design: 壳页 UI 设计稿（open-design）— 工作台/dsh/插件/关于/引导页"
```

- [ ] **Step 4: 用户确认（硬门槛）**

向用户展示设计稿要点（可截图或描述），**等待用户确认**后再进入 Task 9。用户提出修改 → 在本任务内迭代修改 → 重新提交 → 再确认。

---

### Task 9: 壳页 UI 实现（4 Tab + 引导页）

**Files:**
- Modify: `apps/desktop/ui/shell.html`、`apps/desktop/ui/shell.js`、`apps/desktop/ui/theme.css`（按 Task 8 设计稿落地）
- Test: 手动回归（浏览器打开 `ui/shell.html` 无法独立跑——依赖 Tauri IPC；验收 = `cargo tauri build` 产物运行 + 回归清单）

**Interfaces:**
- Consumes: `get_dsh_state`、`update_dsh_cmd`、`setup_dsh_cmd`、`setup_state_cmd`、`setup_cancel_cmd`、`list_dsh_versions_cmd`、`plugin_op`、`plugin_list_cmd`、`check_app_update_cmd`、`app_update_cmd`、`get_shell_state`、`get_dsh_url`、`open_browser_cmd`、`uninstall_run`、事件 `dsh:url`、`dsh:need-setup`、`dsh:setup-progress`、`dsh:plugin-output`、`shell:tab`

- [ ] **Step 1: shell.html 重构为 4 Tab + 引导视图**

- Tab 列表改为：`工作台 / dsh / 插件 / 关于`（删 `常规`、`网络`、`更新`、`卸载` Tab；卸载入口移入关于页）
- 工作台 activity 区内新增引导视图 `#setupView`（与 `#wbPlaceholder` 同层）：标题、阶段进度文案、进度条、错误区（`#setupError` + 重试按钮）、「取消安装」按钮（→ `setup_cancel_cmd`，规格 5.1 可取消）、高级区（registry 输入 + 版本下拉 + 安装按钮）
- 关于页包含：App 版本、`检查更新`/`下载并安装` 按钮 + 状态、**「在浏览器打开下载页」按钮**（`open_browser_cmd` 打开 GitHub releases 页——规格 5.3 手动下载兜底入口）、卸载两档按钮
- dsh 页包含：当前版本、`检查更新`、版本列表（每行：版本号 + latest 徽标 + 安装/切换到该版本按钮）、registry 源设置
- 插件页包含：插件列表（名称/版本/状态徽标）、安装输入 + 安装/卸载按钮、实时输出 `pre.out`
- 按 Task 8 设计稿调整结构类名与布局

- [ ] **Step 2: shell.js 重写**

- Tab 切换表 `['workbench','dsh','plugins','about']`；`⌘K` 循环 + Esc 回工作台保留
- 引导流程：**初始化先调 `setup_state_cmd`，`current` 为 None 即直接显示 `#setupView`**（`dsh:need-setup` 事件可能在 webview 挂监听前发出而丢失，仅作辅助触发）→ 调 `list_dsh_versions_cmd` 预填版本下拉 → 点安装 → `setup_dsh_cmd` → 监听 `dsh:setup-progress` 更新进度 → 成功后（`dsh:url` 事件）自动切回工作台；错误显示 + 重试；「取消安装」→ `setup_cancel_cmd`（取消后回到可重试状态）
- 工作台加载：保留现有轮询 `get_dsh_url` + `dsh:url` 事件逻辑
- dsh 页：`get_dsh_state` 渲染（current/latest/versions/installing）；有新版（`latest > current`）显示「更新到 vX」按钮 → `update_dsh_cmd(latest)`；版本列表每行「安装」→ `update_dsh_cmd(ver)`（当前版本行显示「当前」徽标、禁用）；`installing` 期间全部禁用 + 状态文案
- 插件页：进入时 `plugin_list_cmd` 渲染列表；安装/卸载沿用 `plugin_op` + `dsh:plugin-output` 实时输出；完成后无需手动重启（后端已自动重启，状态文案改为「已完成，工作台正在重启…」）
- 关于页：`get_shell_state` 显示 app_version；`check_app_update_cmd` 显示最新版本或「已是最新」；「下载并安装」→ `app_update_cmd`；卸载沿用 `uninstall_run`
- 删除全部 LAN/常规/旧更新逻辑

- [ ] **Step 3: theme.css 落地设计 token**

按 `docs/design/2026-08-22-shell-ui/design-tokens.md` 更新 `--accent`/`--bg`/圆角/间距等变量与组件样式（列表行、徽标、进度条、按钮变体沿用现有 `.sec/.row/.opt-danger/.status` 体系，仅补充缺失组件）。

- [ ] **Step 4: 构建 + 冒烟**

Run: `cd apps/desktop/src-tauri && cargo tauri build`（或 `cargo build` + 本地 `cargo tauri dev`）
Expected: 编译通过；`cargo tauri dev` 启动后壳页渲染正常（无闭包时出现引导页；手动放一个 `v<ver>` 闭包到 app-data 后正常进工作台）

- [ ] **Step 5: Commit**

```bash
cd /Users/chj/agentProjects/dsh-desktop
git add apps/desktop/ui
git commit -m "feat: 壳页重构（工作台/dsh/插件/关于 + 首次引导页）"
```

---

### Task 10: README 重写 + 回归清单 + 全量验证

**Files:**
- Modify: `README.md`、`docs/superpowers/specs/2026-08-22-thin-shell-redesign-design.md`（如需同步修订）
- Test: 全量

- [ ] **Step 1: README 重写**

- 定位改为「瘦壳」：不内置 dsh 闭包；首次运行自动安装；dsh 版本管理（latest/指定版本/回滚）；App 内更新；插件列表
- 删除：局域网远程连接章节、常规（工作目录/自启/日志）章节、内置闭包体积描述
- 保留/更新：安装（install.sh/install.ps1 不变）、平台差异、卸载、托盘、崩溃自愈说明
- 更新目录结构说明（registry.rs/dsh.rs/plugin.rs/appupdate.rs；resources 只含 node/npm/pnpm-bin）
- 更新「更新机制」描述：`<app-data>/dsh/` 闭包管理（tmp→自检→原子切换→GC 保留上一版）

- [ ] **Step 2: 编写回归清单（docs/regression-checklist.md）**

覆盖：首次引导安装（含断网/失败重试、指定版本、**安装中取消**）、dsh 更新到 latest、指定版本安装、回滚到上一版本、插件列表展示/安装/卸载 + 自动重启、App 更新检查（有新版/无新版/断网）、**macOS x86_64 上 App 更新走「关于页手动下载」兜底**（无 CI 产物）、卸载两档、崩溃自愈、托盘/单实例、Windows 路径（taskkill/NSIS 静默安装）。

- [ ] **Step 3: 全量验证**

Run: `cd apps/desktop/src-tauri && cargo test && cargo build`
Expected: 全绿；`git status` 干净（除 `.reasonix/`）

- [ ] **Step 4: 按回归清单手工冒烟（macOS 本机）**

Run: `cd apps/desktop/src-tauri && cargo tauri dev`
Expected: 引导页出现 → 安装 dsh → 工作台加载；dsh 页版本列表可见；插件页空列表正常

- [ ] **Step 5: Commit**

```bash
cd /Users/chj/agentProjects/dsh-desktop
git add README.md docs/regression-checklist.md
git commit -m "docs: 瘦壳 README 重写 + 回归清单"
```

---

## Self-Review 记录

- **规格覆盖**：瘦壳（Task 7 资源/CI 清理 + Task 3 引导）、dsh 版本管理/指定版本/回滚（Task 2/4）、App 更新（Task 6）、插件列表+装/卸+自动重启（Task 5）、砍 LAN/常规（Task 7）、UI 4 Tab + 引导页（Task 8/9）、README（Task 10）、错误处理（各任务内嵌 tmp→自检→原子切换 + 静默失败）、registry 可配（Task 1/4/9）。
- **占位符**：无 TBD/TODO；Task 8 的 open-design 连接失败降级路径已写明。
- **类型一致性**：`install_version(p, ver, registry, progress)` 在 Task 2 定义、Task 3/4 消费一致；`current_closure`/`closure_version`/`installed_versions` 签名一致；`registry::cmp_versions/registry_url/list_versions/latest_version` 跨任务一致；command 名与 shell.js 调用一一对应。

## 审查修正记录（2026-08-22，review 技能审查后）

按 review 结果逐项修正，全部已落实到上文任务正文：

**Blocking（不修必挂）**
1. Task 1 `cmp_versions`：prerelease 后缀改为数字段感知比较（`rc.10 > rc.9`），修正字符串比较排序错误。
2. Task 2 两个测试与实现目录结构对齐：闭包目录统一 `<app-data>/dsh/v<ver>/`，`current` 标记在 `dsh/current`，`current_closure` 要求 `node_modules/@deepseek-ai/dsh` 存在（补「无 node_modules → None」断言）。

**Should-fix**
3. Task 2 Step 4 改写清单补全：`update::registry_url` → `crate::registry::registry_url`；`update::apply_update` 调用点 → `install_version(&p,&ver,&reg,&|_|{})`；`run_cli_hooks`（`--self-update-check`/`--self-apply-update`）改写为调新接口并保留（README 无头验证工具），`--self-login-item` 随 Task 7 删除；`resolve_current`（main.rs:545）加入删除清单。
4. Task 5 迁移清单补 `tail_text`（`run_dsh_plugin` 依赖）；`crate::logln!` 宏跨模块不可见（宏定义在 `mod` 声明之后）→ 一律用 `crate::logln(&format!(...))` 函数形式。
5. Task 2 `install_version` promote 改为「旧目录移开 → rename → 失败恢复旧目录」（rename final→`v<ver>.old`，GC 跳过 `.old`），满足规格失败安全。
6. Task 2/3/4 新增安装取消：`dsh.rs` 记录 npm 子进程 PID（`SETUP_CHILD`）+ `cancel_install()`（unix SIGTERM / windows taskkill），`setup_cancel_cmd` command；`SETUP_BUSY` 在 `spawn_blocking` join 失败路径也释放（`result.map_err(...)??` 前先复位）。
7. Task 6：下载总超时 30s → 1800s（数百 MB 安装包）；`Content-Length` 缺失/不符即失败清理（规格 5.3 大小校验）；`asset_url` 限 macOS aarch64（CI 仅 macos-14 arm64 产物，x86_64 返回 None 走手动兜底），测试按平台 cfg 断言；osascript 提权路径单引号转义。
8. Task 9：引导视图加「取消安装」按钮；初始化先查 `setup_state_cmd`（`dsh:need-setup` 事件可能先于监听发出而丢失，只作辅助）；关于页加「在浏览器打开下载页」手动兜底（规格 5.3）；Interfaces 补 `setup_cancel_cmd`/`open_browser_cmd`。
9. Task 10 回归清单补「安装中取消」「macOS x86_64 App 更新手动兜底」。
10. Task 7 nits：Files 去掉 shell.html/js（Task 9 才动）；`login_on` 归属修正（ShellState 字段，非 Settings）；规格模块树补 `apps/desktop/` 前缀。

**已验证为正面的事实**：asset 命名与 install.sh:42/install.ps1:42 一致；计划引用的 main.rs 行号与代码相符；`use tauri::Emitter` 已存在（main.rs:45）；Paths/Settings 字段与清理清单吻合；无新增第三方依赖。
