//! dsh 插件管理：web profile 已装插件列表 + 安装/卸载（复用原 main.rs 的 pnpm 逻辑）。
//!
//! `dsh plugin --profile web <op> <pkg>` = 在 ~/.dsh/profiles/web 里转发给
//! pnpm（dsh 闭包写死 spawnSync("pnpm") 从 PATH 找，见 plugin-9h8shc4d.js）。
//! 本实现：
//!   - 用内置 node 调内置 dsh bin.js，PATH 前置 resources/pnpm-bin（打包的
//!     pnpm JS 发行版 + shim，见 prepare-resources.sh/ps1），用户无需装 pnpm；
//!   - 安装前确保 profile 的 pnpm-workspace.yaml 含 allowBuilds（pnpm 11 构建
//!     脚本门禁）与 minimumReleaseAge: 0（新包发布年龄门禁），并在输出出现
//!     ERR_PNPM_IGNORED_BUILDS 时自动补写缺失包名重试；
//!   - 尊重 settings.registry（npm registry 覆盖）；
//!   - 装/卸成功后自动重启工作台生效（替代原「请手动重启」提示）。

use serde::Serialize;
use std::io::BufRead;
use tauri::{Emitter, Manager};

/// 插件操作串行锁：pnpm-workspace.yaml 的读-改-写与 dsh 的 reconcile 均非原子，
/// 并发触发（多窗口/远程）会撕裂文件；插件操作低频，全局串行化最简单可靠。
static PLUGIN_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// 单个已声明插件的展示信息。
#[derive(Serialize)]
pub struct PluginInfo {
    pub name: String,
    pub version: String,
    pub installed: bool,
}

/// 读取 `profile_dir/package.json` 的 dependencies + devDependencies 得到插件列表，
/// `installed` = `node_modules/<name>` 目录存在，按名称排序。
/// profile 缺失 / package.json 缺失或非法时返回空列表（不报错）。
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

/// npm/pnpm 包规格宽松校验（spec V9 两级白名单）：
/// 1) 第一级前缀白名单：@ / 字母数字（npm 包名、owner/repo）/ github: /
///    gitlab: / bitbucket: / git+ssh:// / git+https:// / https:// / http://；
///    非白名单协议形式（含 `:` 的其它开头，如 git:、git://、git+http://、
///    file:、jsr:、workspace:）一律拒绝——冒号检查仅针对 `#` 切分后的主体，
///    `#semver:` / `#path:` 参数段不受影响；
/// 2) 第二级字符白名单：字母数字与 @ . _ - / ~ : +（< > = ^ 仅限 #semver: 段），
///    # 后参数段支持 <ref> / semver: / path:（& 分隔）。
/// 防 pnpm 参数混淆（-g、--dir= 等以 - 开头）、防参数注入/路径穿越
/// （空白、控制字符、shell 元字符、? query、本地路径均拒绝）。
fn valid_pkg_name(s: &str) -> bool {
    if s.is_empty() || s.len() > 214 {
        return false;
    }
    let lower = s.to_ascii_lowercase();
    let first = s.as_bytes()[0];
    let protocol_ok = lower.starts_with("github:")
        || lower.starts_with("gitlab:")
        || lower.starts_with("bitbucket:")
        || lower.starts_with("git+ssh://")
        || lower.starts_with("git+https://")
        || lower.starts_with("https://")
        || lower.starts_with("http://");
    // 按 # 切分主体与参数段（URL 中 # 只作 git 参数起始符）
    let (body, params) = match s.split_once('#') {
        Some((b, p)) => (b, Some(p)),
        None => (s, None),
    };
    if !protocol_ok {
        // 非协议形式只允许 @scope/pkg 与 npm 包名/owner-repo（字母数字开头且无冒号）
        if !(first == b'@' || first.is_ascii_alphanumeric()) {
            return false;
        }
        if body.contains(':') {
            return false; // git: / git:// / file: / jsr: / workspace: 等非白名单协议
        }
    }
    if s.starts_with('.') || s.starts_with('_') || s.starts_with('-') {
        return false;
    }
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

/// 内置 pnpm 可执行文件名（prepare-resources 打包：macOS/Linux 产出 `pnpm`
/// shim，Windows 产出 `pnpm.cmd`——与 Rust 侧存在性检查必须保持一致）。
pub(crate) fn bundled_pnpm_file_name() -> &'static str {
    if cfg!(windows) {
        "pnpm.cmd"
    } else {
        "pnpm"
    }
}

/// 在命令 PATH 最前面插入 dir（读取 cmd 已设置的环境，兼容 apply_user_env 的覆盖）。
fn prepend_path(cmd: &mut std::process::Command, dir: &std::path::Path) {
    let mut cur: Option<std::ffi::OsString> = None;
    for (k, v) in cmd.get_envs() {
        if k == "PATH" {
            cur = v.map(|x| x.to_os_string());
        }
    }
    let cur = cur.unwrap_or_else(|| std::env::var_os("PATH").unwrap_or_default());
    let sep = if cfg!(windows) { ";" } else { ":" };
    let mut joined = dir.as_os_str().to_os_string();
    joined.push(sep);
    joined.push(&cur);
    cmd.env("PATH", joined);
}

/// 确保 profile 的 pnpm-workspace.yaml 包含 pnpm 11 门禁配置：
/// allowBuilds（默认已知构建脚本包 + extra_builds）与 minimumReleaseAge: 0。
/// 文件不存在时按 dsh initProfile 模板创建（dsh 检测到缺 package.json 仍会
/// initProfile，且"已有文件不覆盖"，故预写内容会被保留）。
fn ensure_pnpm_workspace(profile: &std::path::Path, extra_builds: &[String]) -> std::io::Result<()> {
    std::fs::create_dir_all(profile)?;
    let yaml_path = profile.join("pnpm-workspace.yaml");
    let mut lines: Vec<String> = if yaml_path.exists() {
        std::fs::read_to_string(&yaml_path)?
            .lines()
            .map(String::from)
            .collect()
    } else {
        vec![
            "packages:".into(),
            "  - .".into(),
            "nodeLinker: hoisted".into(),
            "autoInstallPeers: false".into(),
        ]
    };
    let mut changed = false;

    let mut builds: Vec<String> = vec![
        "cloudflared".into(),
        "ssh2".into(),
        "cpu-features".into(),
    ];
    for b in extra_builds {
        if !builds.contains(b) {
            builds.push(b.clone());
        }
    }
    let allow_idx = lines
        .iter()
        .position(|l| l.trim_end() == "allowBuilds:" || l.starts_with("allowBuilds:"));
    match allow_idx {
        Some(idx) => {
            let mut end = idx + 1;
            while end < lines.len() && lines[end].starts_with("  ") {
                end += 1;
            }
            let existing: Vec<String> = lines[idx + 1..end]
                .iter()
                .filter_map(|l| l.trim_start().split(':').next().map(|s| s.trim().to_string()))
                .collect();
            for b in &builds {
                if !existing.contains(b) {
                    lines.insert(end, format!("  {b}: true"));
                    end += 1;
                    changed = true;
                }
            }
        }
        None => {
            lines.push("allowBuilds:".into());
            for b in &builds {
                lines.push(format!("  {b}: true"));
            }
            changed = true;
        }
    }
    if !lines.iter().any(|l| l.starts_with("minimumReleaseAge:")) {
        lines.push("minimumReleaseAge: 0".into());
        changed = true;
    }
    if changed {
        std::fs::write(&yaml_path, lines.join("\n") + "\n")?;
    }
    Ok(())
}

/// 从 pnpm 输出提取 "Ignored build scripts: a, b" 中的包名（自动补 allowBuilds 用）。
/// 取 "build scripts:" 后到第一个英文句号之间的逗号分隔片段，避免把错误消息
/// 里的普通单词（Run/pnpm/approve-builds…）误当包名。
fn extract_pkg_names(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in text.lines() {
        if let Some(idx) = line.find("build scripts:") {
            let rest = &line[idx + "build scripts:".len()..];
            let end = rest.find('.').unwrap_or(rest.len());
            for tok in rest[..end].split(',') {
                let t = tok.trim().trim_matches('"').trim();
                if valid_pkg_name(t) && !out.iter().any(|x| x == t) {
                    out.push(t.to_string());
                }
            }
        }
    }
    out
}

/// 保留输出尾部 max 字符（输出可能很大，尾部最有价值；按 char 安全截断）。
fn tail_text(s: &str, max: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max {
        s.to_string()
    } else {
        let kept: String = chars[chars.len() - max..].iter().collect();
        format!("…（输出过长，截断前 {} 字符）\n{kept}", chars.len())
    }
}

/// 递归删除空目录（自叶子向上；非空目录 remove_dir 失败被忽略）。
/// pnpm remove 会留下 0B 的空目录骨架（如 @scope 残留），卸载后清扫。
fn remove_empty_tree(dir: &std::path::Path) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                remove_empty_tree(&e.path());
            }
        }
    }
    let _ = std::fs::remove_dir(dir); // 非空（或占用）时静默忽略
}

/// 清扫 node_modules 下的空目录（不动 node_modules 本身）。
fn clean_empty_dirs_under(root: &std::path::Path) {
    if let Ok(entries) = std::fs::read_dir(root) {
        for e in entries.flatten() {
            let p = e.path();
            if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                remove_empty_tree(&p);
            }
        }
    }
}

/// 执行一次 `dsh plugin --profile web <op> <pkg>`，返回（合并输出, 是否成功）。
fn run_dsh_plugin(
    app: &tauri::AppHandle,
    op: &str,
    pkg: &str,
    extra_builds: &[String],
) -> Result<(String, bool), String> {
    let p = crate::paths_from_app(app);
    let node = crate::node_bin(&p.resources);
    let closure = crate::dsh::current_closure(&p)
        .ok_or_else(|| format!("dsh 闭包未找到：{}", p.resources.display()))?;
    let bin = closure.join("node_modules/@deepseek-ai/dsh/lib/bin.js");
    let home = crate::home_dir();
    let profile = home.join(".dsh/profiles/web");
    // 内置 pnpm（JS 发行版 + shim，由 prepare-resources 打包：macOS 产出
    // pnpm shim，Windows 产出 pnpm.cmd）；缺失时给明确错误
    let pnpm_bin = p.resources.join("pnpm-bin").join(bundled_pnpm_file_name());
    if !pnpm_bin.exists() {
        return Err(format!(
            "内置 pnpm 缺失：{}\n请重新安装 App（或自行安装 pnpm 后重试）",
            pnpm_bin.display()
        ));
    }
    // 插件操作串行化（ensure 的读-改-写与 dsh reconcile 非原子）
    let _guard = crate::mlock(&PLUGIN_LOCK);
    ensure_pnpm_workspace(&profile, extra_builds)
        .map_err(|e| format!("写入 pnpm-workspace.yaml 失败：{e}"))?;

    let mut cmd = std::process::Command::new(&node);
    #[cfg(target_os = "windows")]
    {
        cmd = crate::no_console(cmd); // node.exe 是控制台程序，避免弹出控制台窗口
    }
    // 用户环境合并（macOS）：PATH 里可能有用户自己的 pnpm；随后 prepend 内置 pnpm-bin
    #[cfg(target_os = "macos")]
    crate::apply_user_env(&mut cmd);
    // 内置 pnpm 优先于系统 pnpm，行为可预期
    prepend_path(&mut cmd, &p.resources.join("pnpm-bin"));
    // 尊重用户设置的 npm registry 覆盖
    if let Some(reg) = crate::load_settings(app).registry {
        cmd.env("npm_config_registry", reg);
    }
    cmd.arg(&bin)
        .arg("plugin")
        .arg("--profile")
        .arg("web")
        .arg(op)
        .arg(pkg)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    if cfg!(windows) {
        cmd.env("USERPROFILE", &home).env("HOME", &home);
    } else {
        cmd.env("HOME", &home);
    }
    // 逐行读取 stdout/stderr：实时 emit 到前端（plugin-output 事件）并收集全文。
    // pnpm 可能运行数分钟，一次性 output() 会让前端长时间只有静态"正在…"。
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("执行 dsh plugin 失败：{e}"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "无法读取 dsh plugin stdout".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "无法读取 dsh plugin stderr".to_string())?;
    let emit_app = app.clone();
    let out_thread = std::thread::spawn(move || {
        let reader = std::io::BufReader::new(stdout);
        let mut buf = String::new();
        for line in reader.lines() {
            let Ok(l) = line else { break };
            let l = l.trim_end_matches('\r').to_string();
            buf.push_str(&l);
            buf.push('\n');
            // 收敛到插件管理目标：壳页主窗。窗口已销毁则仅收集全文。
            if let Some(w) = emit_app.get_webview_window(crate::WINDOW_LABEL) {
                let _ = w.emit("dsh:plugin-output", &l);
            }
        }
        buf
    });
    let emit_app2 = app.clone();
    let err_thread = std::thread::spawn(move || {
        let reader = std::io::BufReader::new(stderr);
        let mut buf = String::new();
        for line in reader.lines() {
            let Ok(l) = line else { break };
            let l = l.trim_end_matches('\r').to_string();
            buf.push_str(&l);
            buf.push('\n');
            if let Some(w) = emit_app2.get_webview_window(crate::WINDOW_LABEL) {
                let _ = w.emit("dsh:plugin-output", &l);
            }
        }
        buf
    });
    let status = match child.wait() {
        Ok(st) => st,
        Err(e) => {
            // wait 失败（罕见）：先回收读线程再返回，避免瞬时线程泄漏
            let _ = out_thread.join();
            let _ = err_thread.join();
            return Err(format!("等待 dsh plugin 退出失败：{e}"));
        }
    };
    let success = status.success();
    let mut text = out_thread.join().unwrap_or_default();
    text.push_str(&err_thread.join().unwrap_or_default());
    // pnpm remove 会留下 0B 空目录骨架：卸载（无论成败）后清扫一次
    if op == "remove" {
        clean_empty_dirs_under(&profile.join("node_modules"));
    }
    crate::logln(&format!("[plugin] dsh plugin {op} {pkg} -> exit {status}"));
    Ok((format!("退出码 {status}\n\n{}", tail_text(&text, 60000)), success))
}

/// 前端插件管理 command：安装(add)/卸载(remove) 插件。
/// pnpm 11 构建脚本门禁失败时自动补写 allowBuilds 并重试（最多 2 轮）。
/// 最终仍失败时返回 Err（输出为错误信息），前端按失败态展示。
///
/// 安全：仅接受来自壳页主窗口（label == WINDOW_LABEL）的调用。远程工作台
/// 页面（http://127.0.0.1，含第三方插件 bundle）也能拿到 window.__TAURI__
/// （withGlobalTauri），本校验把 plugin_op 的授权面收回到壳页窗口，防止远程
/// 内容诱导安装任意 npm 包并执行其构建脚本。
/// 瘦壳后主窗口为「壳页 + iframe」：远程 dsh 工作台在 iframe 内，拿不到
/// window.__TAURI__（Tauri 仅往主 frame 注入），因此只有壳页能调 IPC。
#[tauri::command]
pub async fn plugin_op(
    app: tauri::AppHandle,
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
        return Err("包名不合法：支持 npm 包名（@scope/pkg）、Git 源（owner/repo、github:owner/repo、git+ssh://…、git+https://…、https://…）与 tarball URL；不允许空白、shell 特殊字符、以 - 开头的参数或本地路径".to_string());
    }
    // pnpm 可能运行数分钟：移到阻塞线程池执行，避免占用 Tauri 主线程
    // （否则安装期间 App UI / 托盘冻结）。PLUGIN_LOCK 在阻塞线程内获取释放。
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
        let _ = app2.clone().run_on_main_thread(move || crate::restart_dsh(&app2));
        Ok(output)
    })
    .await
    .map_err(|e| format!("插件操作线程异常：{e}"))?
}

/// 前端插件列表 command：读 `~/.dsh/profiles/web` 的 package.json。
#[tauri::command]
pub fn plugin_list_cmd(app: tauri::AppHandle) -> Vec<PluginInfo> {
    let _ = &app;
    let profile = crate::home_dir().join(".dsh/profiles/web");
    list_installed_plugins(&profile)
}

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

    // ---- 以下测试随 pnpm 插件函数自 main.rs 迁移（保留原有覆盖） ----

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

    #[test]
    fn extract_ignored_builds() {
        let out = "ERR_PNPM_IGNORED_BUILDS Ignored build scripts: cloudflared, ssh2. Run \"pnpm approve-builds\" to pick which dependencies should be allowed to run scripts.";
        let names = extract_pkg_names(out);
        assert!(names.contains(&"cloudflared".to_string()));
        assert!(names.contains(&"ssh2".to_string()));
        assert_eq!(names.len(), 2);
        // scope 包名 + 无句点结尾的 pnpm 11 真实格式（[ERR_...] 前缀、行尾无句点）
        let real = "[ERR_PNPM_IGNORED_BUILDS] Ignored build scripts: @scope/pkg, cpu-features, ssh2";
        let names2 = extract_pkg_names(real);
        assert!(names2.contains(&"@scope/pkg".to_string()));
        assert!(names2.contains(&"cpu-features".to_string()));
        assert_eq!(names2.len(), 3);
        assert!(extract_pkg_names("no matches here").is_empty());
    }

    #[test]
    fn workspace_gate_config() {
        let dir = std::env::temp_dir().join(format!("dsh-ws-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // 首次：文件不存在 → 生成模板 + 门禁配置
        ensure_pnpm_workspace(&dir, &["cloudflared".into()]).unwrap();
        let yaml = std::fs::read_to_string(dir.join("pnpm-workspace.yaml")).unwrap();
        assert!(yaml.contains("nodeLinker: hoisted"));
        assert!(yaml.contains("allowBuilds:"));
        assert!(yaml.contains("  cloudflared: true"));
        assert!(yaml.contains("minimumReleaseAge: 0"));
        // 幂等：再次调用不重复
        ensure_pnpm_workspace(&dir, &["cloudflared".into()]).unwrap();
        let again = std::fs::read_to_string(dir.join("pnpm-workspace.yaml")).unwrap();
        assert_eq!(yaml, again);
        // 补充新包名（allowBuilds 已存在时插入块尾）
        ensure_pnpm_workspace(&dir, &["ssh2".into(), "some-other".into()]).unwrap();
        let third = std::fs::read_to_string(dir.join("pnpm-workspace.yaml")).unwrap();
        assert!(third.contains("  ssh2: true"));
        assert!(third.contains("  some-other: true"));
        assert!(!third.contains("  ssh2: true\n  ssh2: true"));
        // minimumReleaseAge 已存在时保持幂等、不重复
        assert_eq!(third.matches("minimumReleaseAge:").count(), 1);
        ensure_pnpm_workspace(&dir, &[]).unwrap();
        let fourth = std::fs::read_to_string(dir.join("pnpm-workspace.yaml")).unwrap();
        assert_eq!(fourth.matches("minimumReleaseAge:").count(), 1);
        assert_eq!(fourth, third); // 无新增时内容不变
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tail_limits_output() {
        assert_eq!(tail_text("short", 100), "short");
        let long = "x".repeat(1000);
        let t = tail_text(&long, 100);
        assert!(t.contains("截断"));
        assert!(t.ends_with("x".repeat(100).as_str()));
        // char 边界安全（中文）
        let cn = "中".repeat(500);
        let t2 = tail_text(&cn, 100);
        assert!(t2.ends_with("中".repeat(100).as_str()));
    }

    #[test]
    fn clean_empty_dirs_removes_only_empty() {
        let root = std::env::temp_dir().join(format!("dsh-clean-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let scoped = root.join("node_modules/@linxin666/dsh-web-ui-all");
        std::fs::create_dir_all(&scoped).unwrap();
        let keep = root.join("node_modules/dsh-base");
        std::fs::create_dir_all(&keep).unwrap();
        std::fs::write(keep.join("index.js"), "x").unwrap();
        clean_empty_dirs_under(&root.join("node_modules"));
        assert!(!scoped.exists(), "空目录应被删除");
        assert!(keep.exists(), "非空目录应保留");
        assert!(root.join("node_modules").exists(), "node_modules 本身不应被删");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn pkg_name_length_boundary() {
        // 214 是上限（npm 包名最大长度），215 必须拒绝（已在非法列表）
        let max_ok = "x".repeat(214);
        assert!(valid_pkg_name(&max_ok));
    }

    #[test]
    fn bundled_pnpm_name_matches_platform() {
        // 与 prepare-resources.sh/ps1 的产物名保持一致：
        // macOS/Linux 生成 `pnpm` shim，Windows 生成 `pnpm.cmd`。
        let name = bundled_pnpm_file_name();
        #[cfg(windows)]
        assert_eq!(name, "pnpm.cmd");
        #[cfg(not(windows))]
        assert_eq!(name, "pnpm");
    }

    #[test]
    fn prepend_path_prefixes_and_keeps_existing() {
        use std::process::Command;
        let sep = if cfg!(windows) { ";" } else { ":" };
        // 已有 PATH 时：前置 + 保留原值
        let mut cmd = Command::new("true");
        cmd.env("PATH", "/usr/bin:/bin");
        prepend_path(&mut cmd, std::path::Path::new("/opt/pnpm-bin"));
        let mut paths: Vec<String> = cmd
            .get_envs()
            .filter(|(k, _)| k.to_str() == Some("PATH"))
            .map(|(_, v)| v.map(|x| x.to_string_lossy().into_owned()).unwrap_or_default())
            .collect();
        assert_eq!(paths.len(), 1);
        let p = paths.pop().unwrap();
        assert!(p.starts_with(&format!("/opt/pnpm-bin{sep}")), "前置失败: {p}");
        assert!(p.contains("/usr/bin"), "原 PATH 丢失: {p}");
        // 空 PATH 时：前置 + 兜底分隔符（不 panic）
        let mut cmd2 = Command::new("true");
        cmd2.env("PATH", "");
        prepend_path(&mut cmd2, std::path::Path::new("/opt/pnpm-bin"));
        let p2: String = cmd2
            .get_envs()
            .filter(|(k, _)| k.to_str() == Some("PATH"))
            .map(|(_, v)| v.map(|x| x.to_string_lossy().into_owned()).unwrap_or_default())
            .next()
            .unwrap();
        assert!(p2.starts_with(&format!("/opt/pnpm-bin{sep}")), "空 PATH 前置失败: {p2}");
    }
}
