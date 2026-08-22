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
        let dest = installer.clone();
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

#[cfg(test)]
mod tests {
    use super::*;
    // brief 测试函数名与实现同名：本地定义会遮蔽 glob 导入的同名实现，
    // 故以显式别名导入解除遮蔽（断言内容与 brief 完全一致）。
    use super::parse_tag_from_effective_url as parse_tag_impl;

    #[test]
    fn parse_tag_from_effective_url() {
        assert_eq!(
            parse_tag_impl("https://github.com/Jedeiah/dsh-desktop/releases/tag/v0.3.1").as_deref(),
            Some("0.3.1")
        );
        assert_eq!(parse_tag_impl("https://github.com/Jedeiah/dsh-desktop/releases/tag/v0.3.1?foo=1").as_deref(), Some("0.3.1"));
        assert_eq!(parse_tag_impl("https://github.com/other/releases/tag/v1.0.0").as_deref(), Some("1.0.0"));
        assert_eq!(parse_tag_impl("https://example.com/404"), None);
        assert_eq!(parse_tag_impl(""), None);
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
