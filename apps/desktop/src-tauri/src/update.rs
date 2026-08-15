// DSh Desktop — update subsystem (M3)
//
// Checks the upstream `@deepseek-ai/dsh` version and applies an update by
// installing a fresh closure with the bundled npm into the app data dir:
//
//   <app-data>/dsh/
//     current            symlink -> active version
//     v<new>/            npm-installed closure (+ VERSION marker)
//     v<prev>/           previous version kept for one generation
//
// The bundled resources stay read-only; every update lands in the app-data
// dir (removed on uninstall). The new closure is boot-verified (--version and
// --dump-default-config must pass) BEFORE the `current` symlink is switched,
// so a failed update never breaks the running install. The registry is
// configurable (default official npmjs; npmmirror is a supported fast mirror).

use std::cmp::Ordering;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::Paths;

const PKG: &str = "@deepseek-ai/dsh";
const DEFAULT_REGISTRY: &str = "https://registry.npmjs.org";

/// Canonical registry base URL (no trailing slash).
pub fn registry_url(registry: Option<&str>) -> String {
    match registry {
        Some(r) if !r.trim().is_empty() => r.trim_end_matches('/').to_string(),
        _ => DEFAULT_REGISTRY.to_string(),
    }
}

/// Query the registry for the latest published version.
pub fn latest_version(registry: &str) -> Result<String, String> {
    let url = format!("{registry}/{PKG}/latest");
    let resp = ureq::get(&url)
        .timeout(std::time::Duration::from_secs(20))
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
    // release (no suffix) > pre-release
    match (asuf.is_empty(), bsuf.is_empty()) {
        (true, true) => Ordering::Equal,
        (true, false) => Ordering::Greater,
        (false, true) => Ordering::Less,
        (false, false) => asuf.cmp(&bsuf),
    }
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

/// The dsh version directory in the app data dir.
fn app_dsh_dir(p: &Paths) -> PathBuf {
    p.app_data.join("dsh")
}

/// Read the version dir name recorded in the `current` version-marker file.
fn read_current_marker(dsh_dir: &Path) -> Option<String> {
    std::fs::read_to_string(dsh_dir.join("current"))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Install `@deepseek-ai/dsh@ver` into `target` using the bundled node + npm,
/// then verify it boots. Returns an error string on any failure.
fn install_and_verify(p: &Paths, target: &Path, ver: &str, registry: &str) -> Result<(), String> {
    let node = crate::node_bin(&p.resources);
    let npm = p.resources.join("npm/bin/npm-cli.js");
    if !node.is_file() || !npm.is_file() {
        return Err(format!(
            "内置 node/npm 缺失 (node={}, npm={})",
            node.display(),
            npm.display()
        ));
    }

    let status = Command::new(&node)
        .arg(&npm)
        .arg("install")
        .arg("--prefix")
        .arg(target)
        .arg(format!("{PKG}@{ver}"))
        .args(["--ignore-scripts", "--no-audit", "--no-fund", "--loglevel=error"])
        .arg("--registry")
        .arg(registry)
        // keep npm cache/logs inside the app data dir (nothing leaks to ~/.npm)
        .arg("--cache")
        .arg(p.app_data.join("dsh/npm-cache"))
        .current_dir(target)
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .status()
        .map_err(|e| format!("运行内置 npm 失败: {e}"))?;
    if !status.success() {
        return Err(format!("npm install {PKG}@{ver} 失败 (exit {status})"));
    }

    let bin = target.join("node_modules/@deepseek-ai/dsh/lib/bin.js");
    // boot-verify 1: version
    let out = Command::new(&node)
        .arg(&bin)
        .arg("--version")
        .output()
        .map_err(|e| format!("校验新闭包版本失败: {e}"))?;
    let got = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if got != ver {
        return Err(format!("新闭包版本校验失败: 期望 {ver}, 实际 {got}"));
    }
    // boot-verify 2: the web profile must compose
    let composed = Command::new(&node)
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

/// Apply an update to `new_ver` atomically:
/// 1. install into `v<new>-tmp` (boot-verified)
/// 2. promote to `v<new>`
/// 3. switch the `current` version marker (previous version dir is kept)
///
/// On any failure before the switch, the running install is untouched.
pub fn apply_update(p: &Paths, new_ver: &str, registry: &str) -> Result<(), String> {
    let dsh_dir = app_dsh_dir(p);
    std::fs::create_dir_all(&dsh_dir).map_err(|e| format!("创建目录失败: {e}"))?;

    let tmp = dsh_dir.join(format!("v{new_ver}-tmp"));
    let final_dir = dsh_dir.join(format!("v{new_ver}"));
    if tmp.exists() {
        std::fs::remove_dir_all(&tmp).map_err(|e| format!("清理临时目录失败: {e}"))?;
    }
    std::fs::create_dir_all(&tmp).map_err(|e| format!("创建临时目录失败: {e}"))?;

    if let Err(e) = install_and_verify(p, &tmp, new_ver, registry) {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(e);
    }

    // write the version marker into the promoted closure
    std::fs::write(tmp.join("VERSION"), new_ver).map_err(|e| format!("写版本标记失败: {e}"))?;

    // promote tmp -> v<new>
    if final_dir.exists() {
        std::fs::remove_dir_all(&final_dir).map_err(|e| format!("清理旧版本目录失败: {e}"))?;
    }
    std::fs::rename(&tmp, &final_dir).map_err(|e| format!("发布新版本目录失败: {e}"))?;

    // atomically switch `current` to v<new> via a version-marker file.
    // A plain text file + atomic rename works on Windows (no symlink privilege
    // needed) and on unix alike, so we drop the old `current` symlink scheme.
    let cur_marker = dsh_dir.join("current");
    // remember the previous version so the GC below can keep one rollback
    let prev_ver = read_current_marker(&dsh_dir);
    let tmp_marker = dsh_dir.join("current.tmp");
    std::fs::write(&tmp_marker, format!("v{new_ver}\n"))
        .map_err(|e| format!("写 current 标记失败: {e}"))?;
    std::fs::rename(&tmp_marker, &cur_marker)
        .map_err(|e| format!("切换 current 标记失败: {e}"))?;

    // GC: keep the new version + the previous one (rollback), drop older ones
    // (each full closure is ~340MB; without this, every update leaks a version).
    if let Ok(entries) = std::fs::read_dir(&dsh_dir) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if !name.starts_with('v') || name.ends_with("-tmp") {
                continue;
            }
            let keep = name == format!("v{new_ver}") || Some(&name) == prev_ver.as_ref();
            if !keep && e.path().is_dir() {
                let _ = std::fs::remove_dir_all(e.path());
                log_cleanup(&format!("dropped stale closure {name}"));
            }
        }
    }

    Ok(())
}

/// Small log helper so update.rs stays dependency-free (writes to stderr).
fn log_cleanup(msg: &str) {
    eprintln!("[dsh-update] {msg}");
}

/// Run a full check: returns Some(new_version) when an update is available.
pub fn check_update(p: &Paths, registry: Option<&str>) -> Result<Option<(String, String)>, String> {
    let reg = registry_url(registry);
    let latest = latest_version(&reg)?;
    let current = crate::active_closure(p)
        .and_then(|c| closure_version(&c))
        .unwrap_or_else(|| "unknown".into());
    Ok(if cmp_versions(&current, &latest) == Ordering::Less {
        Some((current, latest))
    } else {
        None
    })
}

/// Post a desktop notification (best-effort, per-platform mechanism).
pub fn notify(title: &str, body: &str) {
    #[cfg(target_os = "macos")]
    {
        let script = format!(
            "display notification \"{}\" with title \"{}\"",
            body.replace('"', "'"),
            title.replace('"', "'")
        );
        let _ = Command::new("osascript").args(["-e", &script]).spawn();
    }
    #[cfg(target_os = "windows")]
    {
        // PowerShell 无窗口气球提示（NotifyIcon），尽力而为、失败静默。
        let ps = format!(
            "Add-Type -AssemblyName System.Windows.Forms; \
             $n = New-Object System.Windows.Forms.NotifyIcon; \
             $n.Icon = [System.Drawing.SystemIcons]::Information; \
             $n.Visible = $true; \
             $n.ShowBalloonTip(5000, '{title}', '{body}', [System.Windows.Forms.ToolTipIcon]::Info); \
             Start-Sleep -Milliseconds 6000; \
             $n.Dispose()",
            title = title.replace('\'', "''"),
            body = body.replace('\'', "''")
        );
        let _ = Command::new("powershell.exe")
            .arg("-NoProfile")
            .arg("-NonInteractive")
            .arg("-WindowStyle")
            .arg("Hidden")
            .arg("-Command")
            .arg(&ps)
            .spawn();
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let _ = (title, body);
}

/// Resolve the app data dir (shared by GUI and CLI paths).
/// macOS: `~/Library/Application Support/<id>`; Windows: `%APPDATA%/<id>`
/// (matches Tauri's own `app_data_dir()` so GUI and CLI stay consistent).
pub fn app_data_from_home() -> PathBuf {
    dirs::data_dir()
        .map(|d| d.join(crate::APP_ID))
        .unwrap_or_else(|| crate::home_dir().join(".dsh-desktop"))
}
