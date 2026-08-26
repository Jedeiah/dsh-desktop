// DSh Desktop — closure management (thin shell)
//
// Manages the `@deepseek-ai/dsh` closure in the app data dir:
//
//   <app-data>/dsh/
//     current            plain-text marker -> active version dir name
//     v<ver>/            npm-installed closure (+ VERSION marker)
//     npm-cache/         npm cache (nothing leaks to ~/.npm)
//
// The bundled resources stay read-only (thin shell: node + npm only, no
// bundled closure); every closure install lands in the app-data dir (removed
// on uninstall). A new closure is boot-verified (--version and
// --dump-default-config must pass) BEFORE the `current` marker is switched,
// so a failed install never breaks the running version. The registry is
// configurable (default npmmirror for CN users; official npmjs is supported).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::Instant;

use crate::Paths;

/// Resolve the app data dir (shared by GUI and CLI paths).
/// macOS: `~/Library/Application Support/<id>`; Windows: `%APPDATA%/<id>`
/// (matches Tauri's own `app_data_dir()` so GUI and CLI stay consistent).
pub fn app_data_from_home() -> PathBuf {
    dirs::data_dir()
        .map(|d| d.join(crate::APP_ID))
        .unwrap_or_else(|| crate::home_dir().join(".dsh-desktop"))
}

/// Read the closure's version from its `VERSION` marker (fallback: package.json).
pub fn closure_version(dir: &Path) -> Option<String> {
    let marker = dir.join("VERSION");
    if let Ok(s) = std::fs::read_to_string(&marker) {
        let v = s.trim();
        if !v.is_empty() {
            return Some(v.to_string());
        }
    }
    let pj = dir.join("package.json");
    if let Ok(raw) = std::fs::read_to_string(&pj) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) {
            if let Some(ver) = v.get("version").and_then(|x| x.as_str()) {
                return Some(ver.to_string());
            }
        }
    }
    None
}

/// 判断 `dir` 是否为**可用**的目标版本闭包目录：版本标记匹配 `ver` 且入口文件
/// `node_modules/@deepseek-ai/dsh/lib/bin.js` 存在。`install_version` 的"复用已有
/// 目录"和 `installed_versions` 的"已安装可切换"都用同一判定，避免 UI 显示「切换」
/// 但实际目录残缺、点按却走了重装（文案误导）。校验不过 fall through 到正常安装。
fn closure_is_usable(dir: &Path, ver: &str) -> bool {
    dir.is_dir()
        && closure_version(dir).as_deref() == Some(ver)
        && dir
            .join("node_modules/@deepseek-ai/dsh/lib/bin.js")
            .is_file()
}

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
/// 前端区分「切换」/「安装」时用（get_dsh_state 返回 installed）。
/// 仅统计**可用**的已装版本（closure_is_usable：版本匹配 + 入口完整），与
/// install_version 的「复用已有目录」判定一致，避免残缺目录被当作可「切换」。
pub fn installed_versions(p: &Paths) -> Vec<String> {
    let mut out = vec![];
    if let Ok(entries) = std::fs::read_dir(p.app_data.join("dsh")) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if let Some(ver) = name.strip_prefix('v') {
                if !ver.ends_with("-tmp") && !ver.ends_with(".old") && closure_is_usable(&e.path(), ver) {
                    out.push(ver.to_string());
                }
            }
        }
    }
    out.sort_by(|a, b| crate::registry::cmp_versions(b, a));
    out
}

/// The npm install child PID while an install is running (for cancel).
static SETUP_CHILD: Mutex<Option<u32>> = Mutex::new(None);

/// 安装日志（诊断安装慢/卡死）：npm 输出逐行落盘，带相对时间戳（自本次安装开始）。
/// 路径 `<app-data>/dsh/install.log`，每次安装覆盖写新段落。
/// 用法：安装慢/卡住后读取该文件，定位卡在哪一阶段、哪一行之后无输出。
static INSTALL_LOG: Mutex<Option<std::fs::File>> = Mutex::new(None);
static INSTALL_LOG_START: Mutex<Option<Instant>> = Mutex::new(None);

fn install_log(msg: &str) {
    let rel = INSTALL_LOG_START
        .lock()
        .ok()
        .and_then(|s| s.map(|t| {
            let d = t.elapsed();
            format!("{:02}:{:02}", d.as_secs() / 60, d.as_secs() % 60)
        }))
        .unwrap_or_else(|| "--:--".to_string());
    if let Ok(mut g) = INSTALL_LOG.lock() {
        if let Some(f) = g.as_mut() {
            let _ = writeln!(f, "[{rel}] {msg}");
        }
    }
}

/// Cancel a running install (kill the npm child; install_version then fails,
/// cleans tmp, and the caller can retry). Best-effort per platform.
pub fn cancel_install() {
    let pid = SETUP_CHILD.lock().unwrap().take();
    if let Some(pid) = pid {
        #[cfg(unix)]
        {
            let _ = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
            // npm 在 CPU 密集阶段（idealTree/reify）信号处理会被事件循环延迟——
            // SIGTERM 可能迟迟不生效（用户实测"点好几次取消才取消"）。5 秒后强制
            // SIGKILL 兜底；npm 若已正常退出，kill 返回 ESRCH 忽略。
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_secs(5));
                let _ = unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
            });
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
    progress: &dyn Fn(&str),
) -> Result<(), String> {
    // 闭包安装用**内置 pnpm**（实测：npm 11 装 678 包依赖树 ~10 分钟 CPU；
    // pnpm 同包 ~21 秒，内容寻址 store + 硬链接）。pnpm-bin 内含 shim
    // （mac/linux `pnpm`、Windows `pnpm.cmd`，均用内置 node 执行）。
    let node = crate::node_bin(&p.resources); // 自检（--version 等）用
    let pnpm = p
        .resources
        .join("pnpm-bin")
        .join(crate::plugin::bundled_pnpm_file_name());
    if !pnpm.is_file() {
        return Err(format!("内置 pnpm 缺失: {}", pnpm.display()));
    }
    let store = p.app_data.join("dsh/pnpm-store");
    let mut cmd = Command::new(&pnpm);
    #[cfg(target_os = "windows")]
    {
        cmd = crate::no_console(cmd);
    }
    install_log(&format!(
        "cmd: {} install @deepseek-ai/dsh@{ver} --ignore-scripts --reporter=append-only --registry {registry} --store-dir {} (cwd={})",
        pnpm.display(),
        store.display(),
        target.display()
    ));
    // pnpm 进度/警告全走 stdout（stderr 基本为空，实测）；--reporter=append-only
    // 避免交互式进度条污染管道。
    let mut child = cmd
        .arg("install")
        .arg(format!("@deepseek-ai/dsh@{ver}"))
        .args(["--ignore-scripts", "--reporter=append-only"])
        .arg("--registry")
        .arg(registry)
        .arg("--store-dir")
        .arg(&store)
        .current_dir(target)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("运行内置 pnpm 失败: {e}"))?;
    // 记录子进程 PID 供 setup_cancel_cmd 取消；无论 wait 成败都先清空
    *SETUP_CHILD.lock().unwrap() = Some(child.id());
    // 流式读 stdout（pnpm 进度行），每行经 progress 回调推送 UI 与日志
    let mut lines = String::new();
    if let Some(mut out) = child.stdout.take() {
        let mut buf = [0u8; 4096];
        loop {
            match std::io::Read::read(&mut out, &mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    lines.push_str(&String::from_utf8_lossy(&buf[..n]));
                    while let Some(i) = lines.find('\n') {
                        let line = lines[..i].trim().to_string();
                        lines.drain(..=i);
                        if !line.is_empty() {
                            progress(&line);
                        }
                    }
                }
                Err(_) => break,
            }
        }
    }
    let wait = child.wait();
    *SETUP_CHILD.lock().unwrap() = None;
    let status = wait.map_err(|e| format!("等待内置 pnpm 失败: {e}"))?;
    if !status.success() {
        // 失败时补读 stderr（量小：警告/错误）进日志与错误信息
        let mut err_out = String::new();
        if let Some(mut err) = child.stderr.take() {
            let _ = std::io::Read::read_to_string(&mut err, &mut err_out);
        }
        let err_tail = err_out.trim();
        install_log(&format!("pnpm install 退出码异常: {status}"));
        if !err_tail.is_empty() {
            install_log(&format!("pnpm stderr: {err_tail}"));
        }
        return Err(format!(
            "pnpm install @deepseek-ai/dsh@{ver} 失败 (exit {status}){}",
            if err_tail.is_empty() { String::new() } else { format!(": {err_tail}") }
        ));
    }
    install_log("pnpm install 完成，开始自检…");
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
    install_log(&format!("自检 --version => {got:?}"));
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

/// 把 `<app-data>/dsh/v<ver>` 原子切换为 current（写 current 标记 + GC 保留新
/// 与上一个、删除更老）。装完新版本 promote 后、以及"复用已存在可用版本目录"
/// 时共用，避免两处重复、保证切换语义一致。
fn activate_closure(dsh_dir: &Path, ver: &str) -> Result<(), String> {
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
    if let Ok(entries) = std::fs::read_dir(dsh_dir) {
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
    Ok(())
}

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

    // 安装日志：**追加模式**（保留多次安装历史——覆盖写会丢上次失败原因；
    // 每次安装写段落头 + 分隔线）。路径 `<app-data>/dsh/install.log`。
    if let Ok(mut g) = INSTALL_LOG.lock() {
        *g = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dsh_dir.join("install.log"))
            .ok();
    }
    if let Ok(mut s) = INSTALL_LOG_START.lock() {
        *s = Some(Instant::now());
    }
    install_log(&format!("===== 安装 dsh@{ver} registry={registry} ====="));
    // progress 包装：每行转发到日志（npm 输出经 progress 逐行回调，落盘可见卡点）
    let orig = progress;
    let progress = &|msg: &str| {
        install_log(msg);
        orig(msg);
    };

    let final_dir = dsh_dir.join(format!("v{ver}"));
    // 目标版本目录已存在且可用（closure_is_usable：版本匹配 + 入口完整）：直接复用，
    // 只切换 current，不重跑 npm install——切回已安装版本是秒切，也避免超大依赖树
    // 全量 resolve 极慢。校验不过 fall through 到下方 npm install（不静默跳过一次重装）。
    if closure_is_usable(&final_dir, ver) {
        progress(&format!("dsh v{ver} 已存在，直接切换…"));
        activate_closure(&dsh_dir, ver)?;
        progress("完成");
        return Ok(());
    }

    let tmp = dsh_dir.join(format!("v{ver}-tmp"));
    if tmp.exists() {
        std::fs::remove_dir_all(&tmp).map_err(|e| format!("清理临时目录失败: {e}"))?;
    }
    std::fs::create_dir_all(&tmp).map_err(|e| format!("创建临时目录失败: {e}"))?;

    progress(&format!("正在下载并安装 dsh v{ver}（约 300MB，首次可能需要几分钟）…"));
    // 安装（下载）文案覆盖 install_and_verify 内的 npm install 阶段；
    // 自检在安装成功后进行，故先提示再进入双重自检。
    progress("正在校验新版本…");
    if let Err(e) = install_and_verify(p, &tmp, ver, registry, progress) {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(e);
    }

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

    // 切 current 标记 + GC（与「复用已存在版本目录」共用同一逻辑）。
    // 若切换失败，current 未变、当前激活版本不受影响（规格 6）。
    activate_closure(&dsh_dir, ver)?;
    // current 标记已原子切换成功：此刻起旧版本目录不再被 current 引用，
    // 才可安全删除 .old（顺序：先切 current 再删 .old，失败时可回滚）。
    if old.exists() {
        let _ = std::fs::remove_dir_all(&old);
    }
    progress("完成");
    Ok(())
}

/// Run a full check: returns Some((current, latest)) when an update is
/// available.
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
        // 规格 4.3 布局：闭包在 <app-data>/dsh/v<ver>/。构造**可用**闭包
        //（VERSION 标记 + 入口 lib/bin.js），匹配 closure_is_usable 判定。
        for v in ["v0.1.0-rc.7", "v0.1.1-rc.1", "v0.1.1-rc.2"] {
            let dir = root.join("dsh").join(v);
            std::fs::create_dir_all(dir.join("node_modules/@deepseek-ai/dsh/lib")).unwrap();
            std::fs::write(dir.join("VERSION"), v.trim_start_matches('v')).unwrap();
            std::fs::write(dir.join("node_modules/@deepseek-ai/dsh/lib/bin.js"), "// entry").unwrap();
        }
        std::fs::create_dir_all(root.join("dsh/npm-cache")).unwrap(); // 非 v* 应忽略
        std::fs::create_dir_all(root.join("dsh/v0.1.0-rc.6.old")).unwrap(); // .old 残留应忽略
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
