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
/// 路径 `<app-data>/logs/install.log`（与 launcher.log 并排），追加模式保留历史。
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

/// pnpm 的条形进度行：`Packages: +502` 之后跟的那条纯 `+` 串（移除依赖时是 `-`）。
/// 安装页把每行当作**阶段文案**直接显示，这行会让用户看到「一行加号」。
/// 只认「长度 ≥ 2 且全部是 + / -」：`+ @deepseek-ai/dsh 0.1.5-rc.2` 这类真实输出含其它
/// 字符，不会被误伤。
fn is_pnpm_bar_line(line: &str) -> bool {
    line.len() >= 2 && line.chars().all(|c| c == '+' || c == '-')
}

/// 关闭安装日志句柄。卸载路径专用：Windows 共享锁下句柄不关，`logs/install.log`
/// 就删不掉，进而整个 app_data 删除失败（便携版卸载会"看似清理完成"却留下 logs/。
/// 实测过的同类问题：launcher.log 句柄已有对应的复位）。
/// 幂等：没装过 dsh 就是 no-op。
pub fn close_install_log() {
    if let Ok(mut g) = INSTALL_LOG.lock() {
        *g = None;
    }
}

/// Cancel a running install (kill the npm child; install_version then fails,
/// cleans tmp, and the caller can retry). Best-effort per platform.
pub fn cancel_install() {
    let pid = SETUP_CHILD.lock().unwrap().take();
    install_log(&format!("取消安装请求（目标 pid={pid:?}）"));
    if let Some(pid) = pid {
        #[cfg(unix)]
        {
            let _ = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
            // npm 在 CPU 密集阶段（idealTree/reify）信号处理会被事件循环延迟——
            // SIGTERM 可能迟迟不生效（用户实测"点好几次取消才取消"）。5 秒后补 SIGKILL。
            // **补刀前必须确认该 pid 仍是我们的 pnpm**：pid 会被系统复用，盲杀可能命中
            // 无关进程（"安装刚起来就被 SIGKILL"的可疑现象就是这么来的）。
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_secs(5));
                if pid_is_our_pnpm(pid) {
                    let _ = unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
                }
            });
        }
        #[cfg(windows)]
        {
            if pid_is_our_pnpm(pid) {
                let _ = crate::no_console(std::process::Command::new("taskkill"))
                    .args(["/PID", &pid.to_string(), "/T", "/F"])
                    .spawn();
            }
        }
    }
}

/// 退出路径专用：立刻结束安装子进程（SIGTERM → 最多等 600ms → SIGKILL）。
///
/// 为什么要有：托盘/菜单「退出」原来只调 kill_dsh()（只管 dsh 服务子进程），安装用的
/// pnpm 从没被结束过——安装中点退出，pnpm 会变成孤儿继续跑，还留着 `v<ver>-tmp`
/// 目录，下次安装可能撞同一个 tmp/共享 store（锁）。返回前最多阻塞 600ms，不拖慢退出。
/// 两种 pid 补刀都先过 pid_is_our_pnpm 身份校验（pid 复用防线，见 cancel_install）。
pub fn kill_setup_child_blocking() {
    let Some(pid) = SETUP_CHILD.lock().unwrap().take() else {
        return;
    };
    install_log(&format!("退出应用：结束安装子进程 pid={pid}"));
    #[cfg(unix)]
    {
        let _ = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
        for _ in 0..6 {
            std::thread::sleep(std::time::Duration::from_millis(100));
            if unsafe { libc::kill(pid as libc::pid_t, 0) } != 0 {
                return; // 已退出（ESRCH）
            }
        }
        if pid_is_our_pnpm(pid) {
            let _ = unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
        }
    }
    #[cfg(windows)]
    {
        if pid_is_our_pnpm(pid) {
            let _ = crate::no_console(std::process::Command::new("taskkill"))
                .args(["/PID", &pid.to_string(), "/T", "/F"])
                .spawn();
        }
    }
}

/// 该 pid 现在是否仍是「本次安装的 pnpm 进程」——pid 复用防线。
/// 只认同时包含 `pnpm` 与 `deepseek-ai/dsh` 的命令行：被复用的无关进程几乎不可能两者都中。
fn pid_is_our_pnpm(pid: u32) -> bool {
    #[cfg(unix)]
    {
        match std::process::Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "command="])
            .output()
        {
            Ok(o) if o.status.success() => {
                let s = String::from_utf8_lossy(&o.stdout);
                s.contains("pnpm") && s.contains("deepseek-ai/dsh")
            }
            _ => false,
        }
    }
    #[cfg(windows)]
    {
        let script = format!(
            "(Get-CimInstance Win32_Process -Filter \"ProcessId = {pid}\" -ErrorAction SilentlyContinue).CommandLine"
        );
        match crate::no_console(std::process::Command::new("powershell"))
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .output()
        {
            Ok(o) => {
                let s = String::from_utf8_lossy(&o.stdout);
                s.contains("pnpm") && s.contains("deepseek-ai/dsh")
            }
            Err(_) => false,
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
        return Err(crate::i18n::tr_args(
            "err_bundled_pnpm_missing",
            &[("path", &pnpm.display().to_string())],
        ));
    }
    let store = p.app_data.join("dsh/pnpm-store");
    let mut cmd = Command::new(&pnpm);
    #[cfg(target_os = "windows")]
    {
        cmd = crate::no_console(cmd);
    }
    install_log(&format!(
        "cmd: {} install @deepseek-ai/dsh@{ver} --ignore-scripts --reporter=append-only --registry {} --store-dir {} (cwd={})",
        pnpm.display(),
        crate::redact_url(registry),
        store.display(),
        target.display()
    ));
    // pnpm 进度/警告全走 stdout（stderr 基本为空，实测）；--reporter=append-only
    // 避免交互式进度条污染管道。
    let mut child = cmd
        .arg("install")
        .arg(format!("@deepseek-ai/dsh@{ver}"))
        // node-linker=hoisted：平铺真实文件树（硬链接至 store，性能优势保留）。
        // **必须**：默认 isolated linker 在 Windows 无特权环境用 NTFS junction
        // （绝对路径指向 -tmp 目录），promote rename tmp→v<ver> 后链接全部断裂
        // →「安装成功但工作台永远起不来」死循环（与 plugin.rs profile 的
        // nodeLinker: hoisted 选择一致）。
        .arg("--config.node-linker=hoisted")
        .args(["--ignore-scripts", "--reporter=append-only"])
        .arg("--registry")
        .arg(registry)
        .arg("--store-dir")
        .arg(&store)
        .current_dir(target)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| crate::i18n::tr_args("err_run_bundled_pnpm", &[("e", &e.to_string())]))?;
    // 记录子进程 PID 供 setup_cancel_cmd 取消；无论 wait 成败都先清空
    *SETUP_CHILD.lock().unwrap() = Some(child.id());
    // stderr 必须**持续 drain**（管道缓冲 ~64KB，pnpm 警告堆积超限会阻塞子进程
    // → 安装假死）；内容入日志，失败时已可用。
    let stderr_handle = std::thread::spawn({
        let mut err = child.stderr.take();
        move || {
            let mut out = String::new();
            if let Some(e) = err.as_mut() {
                use std::io::Read;
                let mut buf = [0u8; 4096];
                loop {
                    match e.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            out.push_str(&String::from_utf8_lossy(&buf[..n]));
                        }
                    }
                }
            }
            out
        }
    });
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
                        if !line.is_empty() && !is_pnpm_bar_line(&line) {
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
    let err_out = stderr_handle.join().unwrap_or_default();
    let status = wait
        .map_err(|e| crate::i18n::tr_args("err_wait_bundled_pnpm", &[("e", &e.to_string())]))?;
    if !status.success() {
        // 失败时 stderr（警告/错误）已由 drain 线程收集
        let err_tail = err_out.trim();
        install_log(&format!("pnpm install 退出码异常: {status}"));
        if !err_tail.is_empty() {
            install_log(&format!("pnpm stderr: {err_tail}"));
        }
        let tail = if err_tail.is_empty() { String::new() } else { format!(": {err_tail}") };
        return Err(crate::i18n::tr_args(
            "err_pnpm_install_failed",
            &[("ver", ver), ("status", &status.to_string()), ("tail", &tail)],
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
        .map_err(|e| crate::i18n::tr_args("err_verify_closure_version", &[("e", &e.to_string())]))?;
    let got = String::from_utf8_lossy(&out.stdout).trim().to_string();
    install_log(&format!("自检 --version => {got:?}"));
    if got != ver {
        return Err(crate::i18n::tr_args(
            "err_closure_version_mismatch",
            &[("ver", ver), ("got", &got)],
        ));
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
        .map_err(|e| crate::i18n::tr_args("err_verify_web_profile", &[("e", &e.to_string())]))?;
    if !composed.success() {
        return Err(crate::i18n::tr("err_web_profile_compose"));
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
    // 临时标记名带 pid：两个进程同时切换（GUI 安装 + `--self-apply-update` CLI）
    // 共用 `current.tmp` 时，后写者会盖掉先写者的内容、先写者的 rename 拿到的是
    // 别人的版本（甚至 rename 失败报"切换失败"）。pid 后缀让各自的写-改-名自成一体。
    let tmp_marker = dsh_dir.join(format!("current.{}.tmp", std::process::id()));
    std::fs::write(&tmp_marker, format!("v{ver}\n"))
        .map_err(|e| crate::i18n::tr_args("err_write_current_marker", &[("e", &e.to_string())]))?;
    std::fs::rename(&tmp_marker, &cur_marker)
        .map_err(|e| crate::i18n::tr_args("err_switch_current_marker", &[("e", &e.to_string())]))?;

    // GC: keep the new version + the previous one (rollback), drop older ones.
    if let Ok(entries) = std::fs::read_dir(dsh_dir) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            // 跳过非版本目录与**在途临时目录**：`-tmp` / `.tmp`（并发安装时另一个
            // 进程正在写入，名字带 pid，删它会砸掉别人的安装）/ `.old`（回滚备份）。
            if !name.starts_with('v')
                || name.ends_with("-tmp")
                || name.ends_with(".tmp")
                || name.ends_with(".old")
            {
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
    // 版本号在函数入口统一校验（不只靠调用方）：ver 会拼进目录名（`v{ver}` /
    // `v{ver}-tmp`）并用于 `remove_dir_all(tmp)`。GUI 两个入口本来都校验，但
    // `--self-apply-update <ver>`（main.rs 的 CLI 钩子）把命令行参数直接透传进来，
    // 未校验的 `../../x` 会把写入/删除解析到 app data 之外。
    if !crate::registry::valid_version(ver) {
        return Err(crate::i18n::tr_args("err_invalid_version_value", &[("ver", ver)]));
    }
    let dsh_dir = p.app_data.join("dsh");
    std::fs::create_dir_all(&dsh_dir)
        .map_err(|e| crate::i18n::tr_args("err_create_dir", &[("e", &e.to_string())]))?;

    // 安装日志：**追加模式**（保留多次安装历史——覆盖写会丢上次失败原因；
    // 每次安装写段落头 + 分隔线）。路径 `<app-data>/logs/install.log`——
    // 与 launcher.log 并排（日志归日志目录，不混入 dsh 闭包管理目录）。
    let log_dir = dsh_dir.parent().map(|p| p.join("logs")).unwrap_or_else(|| dsh_dir.clone());
    let _ = std::fs::create_dir_all(&log_dir);
    if let Ok(mut g) = INSTALL_LOG.lock() {
        *g = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_dir.join("install.log"))
            .ok();
    }
    if let Ok(mut s) = INSTALL_LOG_START.lock() {
        *s = Some(Instant::now());
    }
    install_log(&format!(
        "===== 安装 dsh@{ver} registry={} =====",
        crate::redact_url(registry)
    ));
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
        progress(&crate::i18n::tr_args("progress_switching_existing", &[("ver", ver)]));
        activate_closure(&dsh_dir, ver)?;
        progress(&crate::i18n::tr("progress_done"));
        return Ok(());
    }

    // 临时目录名带 pid：同一版本被两个进程（GUI 安装 + CLI `--self-apply-update`）
    // 同时安装时，共用 `v{ver}-tmp` 会让后到者的 remove_dir_all/create_dir_all 删掉
    // 先到者正在写入的内容，双双失败或装出半成品。
    let tmp = dsh_dir.join(format!("v{ver}-{}.tmp", std::process::id()));
    if tmp.exists() {
        std::fs::remove_dir_all(&tmp)
            .map_err(|e| crate::i18n::tr_args("err_clean_tmp", &[("e", &e.to_string())]))?;
    }
    std::fs::create_dir_all(&tmp)
        .map_err(|e| crate::i18n::tr_args("err_create_tmp", &[("e", &e.to_string())]))?;

    progress(&crate::i18n::tr_args("progress_downloading_dsh", &[("ver", ver)]));
    // 安装（下载）文案覆盖 install_and_verify 内的 npm install 阶段；
    // 自检在安装成功后进行，故先提示再进入双重自检。
    progress(&crate::i18n::tr("progress_verifying_new_version"));
    if let Err(e) = install_and_verify(p, &tmp, ver, registry, progress) {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(e);
    }

    if let Err(e) = std::fs::write(tmp.join("VERSION"), ver) {
        let _ = std::fs::remove_dir_all(&tmp); // 写标记失败即放弃本次安装：不留 tmp 残骸
        return Err(crate::i18n::tr_args("err_write_version_marker", &[("e", &e.to_string())]));
    }

    // promote tmp -> v<ver> with overwrite safety: the existing dir is moved
    // aside first, so any failure below never leaves the running install
    // half-removed (spec 6: 任何失败不动当前可用版本)。
    let old = dsh_dir.join(format!("v{ver}.old"));
    if final_dir.exists() {
        let _ = std::fs::remove_dir_all(&old);
        std::fs::rename(&final_dir, &old)
            .map_err(|e| crate::i18n::tr_args("err_move_old_version", &[("e", &e.to_string())]))?;
    }
    if let Err(e) = std::fs::rename(&tmp, &final_dir) {
        // 恢复被移开的旧目录，保证当前版本仍然可用
        if old.exists() {
            let _ = std::fs::rename(&old, &final_dir);
        }
        // 本次装好的 tmp 也不再有用（下一次安装会重下）：删掉，避免数百 MB 残骸
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(crate::i18n::tr_args("err_publish_new_version", &[("e", &e.to_string())]));
    }

    // 切 current 标记 + GC（与「复用已存在版本目录」共用同一逻辑）。
    // 若切换失败，current 未变、当前激活版本不受影响（规格 6）。
    activate_closure(&dsh_dir, ver)?;
    // current 标记已原子切换成功：此刻起旧版本目录不再被 current 引用，
    // 才可安全删除 .old（顺序：先切 current 再删 .old，失败时可回滚）。
    if old.exists() {
        let _ = std::fs::remove_dir_all(&old);
    }
    progress(&crate::i18n::tr("progress_done"));
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

    /// 每个测试一个独立顶层目录（`dsh-test-<pid>-<name>`）。不用共享的
    /// `dsh-test-<pid>` 父目录：那会让并发跑测试时互相删掉对方的目录，而且
    /// 末尾只删子目录会把这个空父目录留在临时区（实测每次 cargo test 漏一个）。
    fn test_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("dsh-test-{}-{name}", std::process::id()))
    }

    #[test]
    fn pnpm_bar_lines_are_filtered() {
        // 实测首装输出：`Packages: +N` 之后是纯 `+` 条形进度（append-only 模式逐行输出；
        // 观测到 60 与 80 两种长度——宽度由 pnpm 按终端列宽决定，与过滤逻辑无关）——
        // 安装页会把它当阶段文案，显示成「一行加号」。
        assert!(is_pnpm_bar_line(&"+".repeat(60)));
        assert!(is_pnpm_bar_line(&"+".repeat(80)));
        assert!(is_pnpm_bar_line(&"-".repeat(60)));
        assert!(is_pnpm_bar_line("++"));
        // 真实内容行/单字符/空串一律保留
        assert!(!is_pnpm_bar_line("+ @deepseek-ai/dsh 0.1.5-rc.2"));
        assert!(!is_pnpm_bar_line("Packages: +502"));
        assert!(!is_pnpm_bar_line("Progress: resolved 564, reused 489, downloaded 0, added 0"));
        assert!(!is_pnpm_bar_line("Done in 7.3s using pnpm v11.22.0"));
        assert!(!is_pnpm_bar_line("+"));
        assert!(!is_pnpm_bar_line(""));
    }

    #[test]
    fn closure_version_reads_marker_then_package_json() {
        let root = test_dir("closure_version");
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
        let root = test_dir("installed");
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
            resources: root.join("resources"),
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
        let root = test_dir("current_marker");
        let _ = std::fs::remove_dir_all(&root);
        // current 标记 + 闭包目录 + node_modules/@deepseek-ai/dsh 三者齐备才算有效
        let ver_dir = root.join("dsh/v0.1.1-rc.2");
        std::fs::create_dir_all(ver_dir.join("node_modules/@deepseek-ai/dsh")).unwrap();
        std::fs::create_dir_all(root.join("dsh/npm-cache")).unwrap();
        std::fs::write(root.join("dsh/current"), "v0.1.1-rc.2\n").unwrap();
        let p = crate::Paths { resources: root.join("resources"), app_data: root.clone() };
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

    /// 手动端到端（**需要网络**，会走一次真实 pnpm 安装，约 300MB）：断言推给 UI 的
    /// 进度行里没有 pnpm 的纯符号条形进度（用户反馈的「一行加号」）。
    /// 默认不跑（CI 无网/耗时）；手动运行：
    ///   cd apps/desktop/src-tauri && cargo test -- --ignored --nocapture setup_install_stream
    /// 全程只写临时目录，不碰用户 ~/.dsh 与 App 真实 app-data。
    #[test]
    #[ignore]
    fn setup_install_stream_has_no_bar_lines() {
        let root = test_dir("e2e_install_stream");
        let _ = std::fs::remove_dir_all(&root);
        let app_data = root.join("app-data");
        std::fs::create_dir_all(&app_data).unwrap();
        let resources = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources");
        let pnpm = resources
            .join("pnpm-bin")
            .join(crate::plugin::bundled_pnpm_file_name());
        if !pnpm.is_file() {
            eprintln!("跳过：resources 未就绪（先跑 scripts/prepare-resources.sh）");
            return;
        }
        let p = crate::Paths {
            resources,
            app_data,
        };
        let seen: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
        let res = install_version(
            &p,
            "0.1.5-rc.1",
            "https://registry.npmmirror.com",
            &|m: &str| seen.lock().unwrap().push(m.to_string()),
        );
        let lines = seen.into_inner().unwrap();
        println!("进度行 {} 条", lines.len());
        for l in lines.iter().filter(|l| is_pnpm_bar_line(l)) {
            println!("  [本应被过滤] {l}");
        }
        assert!(
            !lines.iter().any(|l| is_pnpm_bar_line(l)),
            "进度行里仍有纯符号条形进度（UI 会显示成「一行加号」）"
        );
        // 反向确认这次安装确实走到了 pnpm 的"包计数"阶段。注意：这只证明走到了该阶段
        // （同一条 reporter message 里 `Packages: +N` 与条形行相邻输出），**不是**"确实
        // 产出过条形行"的硬证明——过滤发生在 install_and_verify 内部，测试看不到原始流；
        // 真正的把关是上面的过滤断言 + is_pnpm_bar_line 的单测。
        assert!(
            lines.iter().any(|l| l.starts_with("Packages: +")),
            "没有看到 Packages: +N 行——安装可能没走 pnpm，断言无效。行：{lines:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
        res.unwrap();
    }
}
