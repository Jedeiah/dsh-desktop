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
// configurable (default official npmjs; npmmirror is a supported fast mirror).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;

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
// Task 7 删除其唯一生产调用后成为死代码（bin crate 内 pub 不豁免 dead_code）；
// 函数与单测仍有价值，保留并抑制 lint，避免扩大改动面。
#[allow(dead_code)]
pub fn installed_versions(p: &Paths) -> Vec<String> {
    let mut out = vec![];
    if let Ok(entries) = std::fs::read_dir(p.app_data.join("dsh")) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if let Some(ver) = name.strip_prefix('v') {
                if !ver.ends_with("-tmp") && !ver.ends_with(".old") && e.path().is_dir() {
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
    progress: &dyn Fn(&str),
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
    // npm 输出流式转发（真实进度行让用户看到"正在下载/解析依赖"而非无反馈等待）：
    // --loglevel=info 输出 fetch/reify 行；每行经 progress 回调推送 UI 与 SETUP_PROGRESS。
    let mut child = cmd
        .arg(&npm)
        .arg("install")
        .arg("--prefix")
        .arg(target)
        .arg(format!("@deepseek-ai/dsh@{ver}"))
        .args(["--ignore-scripts", "--no-audit", "--no-fund", "--loglevel=info"])
        .arg("--registry")
        .arg(registry)
        .arg("--cache")
        .arg(p.app_data.join("dsh/npm-cache"))
        .env("NODE_OPTIONS", "--max-old-space-size=6144")
        .current_dir(target)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("运行内置 npm 失败: {e}"))?;
    // 记录子进程 PID 供 setup_cancel_cmd 取消；无论 wait 成败都先清空
    *SETUP_CHILD.lock().unwrap() = Some(child.id());
    // 流式读 stderr（npm 进度/警告输出地；stdout 同读，先到为准）
    if let Some(mut err) = child.stderr.take() {
        let mut lines = String::new();
        let mut buf = [0u8; 4096];
        loop {
            match std::io::Read::read(&mut err, &mut buf) {
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
    let status = wait.map_err(|e| format!("等待内置 npm 失败: {e}"))?;
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

    // current 标记已原子切换成功：此刻起旧版本目录不再被 current 引用，
    // 才可安全删除 .old。若切换失败（磁盘满/权限等），旧目录仍在原位
    // （或可被恢复），保证任何失败都不动当前可用版本（规格 6）。
    if old.exists() {
        let _ = std::fs::remove_dir_all(&old);
    }

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
        // 规格 4.3 布局：闭包在 <app-data>/dsh/v<ver>/
        for v in ["v0.1.0-rc.7", "v0.1.1-rc.1", "v0.1.1-rc.2"] {
            std::fs::create_dir_all(root.join("dsh").join(v)).unwrap();
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
