//! 桌面壳（Rust 侧）的中/英文案表。
//!
//! 流程：语种只有一个来源——dsh 的 `~/.dsh/settings.yaml` 里的 `locale.preference`，
//! 由 main.rs 的 `resolve_locale` 解析（读不到就回退系统语言、再回退 zh）。本模块把
//! 解析结果缓存在进程内（`LOCALE` / `LIVE`），所有 Rust 侧的用户可见文案都走
//! [`tr`] / [`tr_args`] / [`tr_static`] 取当前语种的字符串，壳页那半边的字典在
//! `apps/desktop/ui/i18n.js`（同键同义同英文文案）。
//!
//! 三个入口的分工：
//! - [`tr`]：普通取词（返回 `String`，可拼 format!/Err/弹窗文案）；
//! - [`tr_args`]：带 `{name}` 占位符的取词（与前端 `T(key, vars)` 语义一致）；
//! - [`tr_static`]：只能放 `&'static str` 的位置（如 `const`、`&str` 字段），
//!   做法是对同一张静态表再做一次查表直接返回表里的 `&'static str`——
//!   没有 `Box::leak`、没有 `unsafe`。
//!
//! 语言切换：`locale()` 只在首次访问时读盘（`OnceLock`）；壳页每次拉 `get_shell_state`
//! 会调 [`refresh_locale`] 重新读盘并更新生效值，所以改 `locale.preference` 后
//! 壳页一刷新就跟着切。**托盘菜单是启动时一次性构建的**，改语言后要重启 App 才会变
//! （见 main.rs 托盘构建处的注释）。

use std::sync::{Mutex, OnceLock};

/// `(key, zh, en)`：键沿用 `ui/i18n.js` 的 snake_case 命名；前端已有的短语直接复用前端的键与英文文案。
static DICT: &[(&str, &str, &str)] = &[
    // ---- 复用 ui/i18n.js 的键：键名 / 语义 / 英文文案与前端逐字一致 ----
    ("app_updating_wait", "应用正在更新，请稍候再操作", "The app is updating — please wait"),
    ("cancel", "取消", "Cancel"),
    ("install_already_running", "已有一个安装正在进行中", "An install is already in progress"),
    ("not_installed", "未安装", "Not installed"),
    ("read_failed", "读取失败", "Read failed"),
    ("registry_empty", "Registry 源不能为空", "Registry cannot be empty"),
    ("retry", "重试", "Retry"),
    ("uninstall_full_b", "完全卸载", "Uninstall everything"),
    ("uninstall_incomplete", "卸载未完成", "Uninstall incomplete"),
    ("unknown", "未知", "Unknown"),
    // ---- 仅 Rust 侧出现的短语（壳页没有对应用法） ----
    ("err_shell_webview_only", "该操作仅限壳页窗口使用", "This operation is only available from the shell window"),
    ("err_modal_webview_only", "该操作仅限弹窗窗口使用", "This operation is only available from the modal window"),
    ("err_save_settings_serialize", "序列化设置失败：{e}", "Could not serialize the settings: {e}"),
    ("err_save_settings_write", "写入设置失败：{e}", "Could not write the settings: {e}"),
    ("err_invalid_version", "版本号不合法", "Invalid version number"),
    ("err_invalid_version_hint", "版本号不合法（应为 x.y.z 或 x.y.z-pre，如 0.1.1-rc.2）", "Invalid version number (expected x.y.z or x.y.z-pre, e.g. 0.1.1-rc.2)"),
    ("setup_preparing", "准备安装…", "Preparing to install…"),
    ("err_install_thread", "安装线程异常：{e}", "Install thread failed: {e}"),
    ("err_verify_thread", "校验线程异常：{e}", "Verification thread failed: {e}"),
    ("err_spawn_dsh", "无法启动内置 dsh：\n{e}\n\n日志位置：\n{path}", "Could not start the bundled dsh:\n{e}\n\nLogs:\n{path}"),
    ("dialog_startup_failed", "DeepSeek Harness Desktop 启动失败", "DeepSeek Harness Desktop failed to start"),
    ("err_dsh_crash_loop", "dsh 连续崩溃 {n} 次，已停止自动重启。\n\n日志位置：\n{path}\n\n其中 dsh.log 记录了 dsh 的异常输出。", "dsh crashed {n} times in a row; automatic restarts have stopped.\n\nLogs:\n{path}\n\ndsh.log records dsh's error output."),
    ("dialog_runtime_error", "DeepSeek Harness Desktop 运行异常", "DeepSeek Harness Desktop runtime error"),
    ("dialog_fatal_hint", "{msg}\n\n点「退出」关闭应用；点「重试」重新拉起 dsh（✕ 等同于重试）。", "{msg}\n\nClick “Quit” to close the app; click “Retry” to start dsh again (✕ behaves like Retry)."),
    ("quit", "退出", "Quit"),
    ("err_modal_no_content", "无待显示内容", "Nothing to display"),
    ("err_registry_scheme", "Registry 源必须以 http:// 或 https:// 开头", "The registry must start with http:// or https://"),
    ("err_registry_too_long", "Registry 源过长（最多 {n} 字符）", "The registry is too long (at most {n} characters)"),
    ("err_modal_thread", "弹窗线程异常：{e}", "Modal thread failed: {e}"),
    ("uninstall_confirm", "确认卸载", "Confirm uninstall"),
    ("uninstall_confirm_full_body", "将删除 ~/.dsh 全部数据（会话与凭据），此操作不可撤销。", "This deletes all data in ~/.dsh (sessions and credentials) and cannot be undone."),
    ("uninstall_confirm_keep_body", "将卸载应用，保留 ~/.dsh 配置与数据（可随时重新安装）。", "The app will be uninstalled; your ~/.dsh config and data are kept (you can reinstall at any time)."),
    ("err_remove_dsh_home", "删除 ~/.dsh 失败: {e}", "Could not delete ~/.dsh: {e}"),
    ("err_uninstall_thread", "卸载线程异常：{e}", "Uninstall thread failed: {e}"),
    ("notify_app_not_trashed", "应用未移入废纸篓", "The app was not moved to the Trash"),
    ("notify_app_not_trashed_body", "应用数据已清理。App 本体未能自动移入废纸篓，请手动把它拖进废纸篓。", "App data was cleaned up. The app itself could not be moved to the Trash automatically — please drag it there manually."),
    ("uninstall_wiped_note", "\n（已按你的选择删除 ~/.dsh；程序文件与应用数据保持原样）", "\n(~/.dsh was deleted as you chose; the program files and app data are unchanged)"),
    ("err_spawn_uninstaller", "无法启动系统卸载器：{e}\n可重试，或从「设置 → 应用」中卸载。{wiped}", "Could not start the system uninstaller: {e}\nYou can retry, or uninstall from “Settings → Apps”.{wiped}"),
    ("notify_portable_cleaned", "便携版：数据已清理", "Portable build: data cleaned up"),
    ("notify_portable_cleaned_body", "程序文件未自动删除。便携版直接删除所在文件夹即可。", "The program files were not removed automatically. For a portable build, just delete its folder."),
    ("notify_use_system_uninstall", "请通过系统卸载", "Uninstall through the system"),
    ("notify_use_system_uninstall_body", "应用数据已清理。请通过系统卸载 DeepSeek Harness Desktop。", "App data was cleaned up. Please uninstall DeepSeek Harness Desktop through the system."),
    ("uninstall_leftovers_body", "以下数据被占用未能删除，重启电脑后即可手动清理：\n{list}", "These items were in use and could not be deleted; you can remove them manually after restarting:\n{list}"),
    ("notify_leftovers", "部分数据将延迟清理", "Some data will be cleaned up later"),
    ("tray_show_main_window", "显示主窗口", "Show main window"),
    ("about_app", "关于 DeepSeek Harness Desktop", "About DeepSeek Harness Desktop"),
    ("about_credits", "作者 Jedeiah\n项目主页 github.com/Jedeiah/dsh-desktop", "by Jedeiah\nProject home github.com/Jedeiah/dsh-desktop"),
    ("menu_edit", "编辑", "Edit"),
    ("menu_manage_panel", "管理面板", "Manage panel"),
    ("menu_view", "视图", "View"),
    ("err_bundled_pnpm_missing", "内置 pnpm 缺失: {path}", "Bundled pnpm is missing: {path}"),
    ("err_run_bundled_pnpm", "运行内置 pnpm 失败: {e}", "Could not run the bundled pnpm: {e}"),
    ("err_wait_bundled_pnpm", "等待内置 pnpm 失败: {e}", "Could not wait for the bundled pnpm: {e}"),
    ("err_pnpm_install_failed", "pnpm install @deepseek-ai/dsh@{ver} 失败 (exit {status}){tail}", "pnpm install @deepseek-ai/dsh@{ver} failed (exit {status}){tail}"),
    ("err_verify_closure_version", "校验新闭包版本失败: {e}", "Could not verify the new closure version: {e}"),
    ("err_closure_version_mismatch", "新闭包版本校验失败: 期望 {ver}, 实际 {got}", "New closure version check failed: expected {ver}, got {got}"),
    ("err_verify_web_profile", "校验 web profile 组合失败: {e}", "Could not verify the web profile composition: {e}"),
    ("err_web_profile_compose", "新闭包无法组合 web profile", "The new closure cannot compose the web profile"),
    ("err_write_current_marker", "写 current 标记失败: {e}", "Could not write the current marker: {e}"),
    ("err_switch_current_marker", "切换 current 标记失败: {e}", "Could not switch the current marker: {e}"),
    ("err_invalid_version_value", "版本号不合法：{ver}", "Invalid version number: {ver}"),
    ("err_create_dir", "创建目录失败: {e}", "Could not create the directory: {e}"),
    ("progress_switching_existing", "dsh v{ver} 已存在，直接切换…", "dsh v{ver} already exists — switching to it…"),
    ("progress_done", "完成", "Done"),
    ("err_clean_tmp", "清理临时目录失败: {e}", "Could not clean the temporary directory: {e}"),
    ("err_create_tmp", "创建临时目录失败: {e}", "Could not create the temporary directory: {e}"),
    ("progress_downloading_dsh", "正在下载并安装 dsh v{ver}（约 300MB，首次可能需要几分钟）…", "Downloading and installing dsh v{ver} (~300MB; the first run may take a few minutes)…"),
    ("progress_verifying_new_version", "正在校验新版本…", "Verifying the new version…"),
    ("err_write_version_marker", "写版本标记失败: {e}", "Could not write the version marker: {e}"),
    ("err_move_old_version", "移开旧版本目录失败: {e}", "Could not move the old version directory aside: {e}"),
    ("err_publish_new_version", "发布新版本目录失败: {e}", "Could not publish the new version directory: {e}"),
    ("err_query_failed", "查询 {url} 失败: {e}", "Request to {url} failed: {e}"),
    ("err_parse_version_from_url", "无法从响应 URL 解析版本：{url}", "Could not parse the version from the response URL: {url}"),
    ("err_download_checksum", "下载校验和 {url} 失败: {e}", "Could not download the checksum {url}: {e}"),
    ("err_read_checksum", "读取校验和失败: {e}", "Could not read the checksum: {e}"),
    ("err_checksum_format", "校验和文件格式异常: {body}", "Malformed checksum file: {body}"),
    ("err_download_failed", "下载失败: {e}", "Download failed: {e}"),
    ("err_create_temp_file", "创建临时文件失败: {e}", "Could not create the temporary file: {e}"),
    ("err_write_failed", "写入失败: {e}", "Write failed: {e}"),
    ("err_download_incomplete", "下载不完整: 期望 {exp} 字节, 实际 {n}", "Incomplete download: expected {exp} bytes, got {n}"),
    ("err_hash_installer", "计算安装包 SHA-256 失败: {e}", "Could not compute the installer SHA-256: {e}"),
    ("err_installer_checksum_mismatch", "安装包校验失败: SHA-256 不符(期望 {want}, 实际 {got})", "Installer verification failed: SHA-256 mismatch (expected {want}, got {got})"),
    ("err_update_check_thread", "检查更新线程异常：{e}", "Update check thread failed: {e}"),
    ("err_auto_install_unsupported", "当前平台暂不支持自动安装", "Automatic install is not supported on this platform yet"),
    ("err_unknown_installer_type", "未知安装包类型", "Unknown installer type"),
    ("err_update_in_progress", "已有更新任务在进行中，请稍候", "An update is already in progress — please wait"),
    ("err_update_thread", "更新线程异常：{e}", "Update thread failed: {e}"),
    ("err_mount_dmg", "挂载 DMG 失败: {e}", "Could not mount the DMG: {e}"),
    ("err_parse_mount_point", "无法解析挂载点:\n{out}", "Could not parse the mount point:\n{out}"),
    ("err_read_dmg", "读取 DMG 内容失败: {e}", "Could not read the DMG contents: {e}"),
    ("err_dmg_no_app", "DMG 中未找到 .app", "No .app found in the DMG"),
    ("err_copy_to_applications", "复制到 /Applications 失败（无写入权限且提权被取消）", "Could not copy to /Applications (no write permission and the elevation prompt was cancelled)"),
    ("err_installer_missing", "安装包不存在（可能被杀毒软件隔离）：{path}", "The installer is missing (it may have been quarantined by antivirus): {path}"),
    ("err_spawn_update_helper", "启动更新助手失败: {e}", "Could not start the update helper: {e}"),
    ("err_update_helper_not_ready", "更新助手未就绪（PowerShell 可能被安全策略禁用），已取消本次更新；可到 Releases 手动下载安装", "The update helper did not become ready (PowerShell may be blocked by security policy); this update was cancelled — you can download it manually from Releases"),
    ("err_update_not_applied", "上次更新到 {ver} 未生效（当前仍为 v{cur}）。可能是安装被安全软件拦下或安装包失效；可重试，或到 Releases 手动下载安装。\n日志：{log}", "The previous update to {ver} did not take effect (still on v{cur}). The installer may have been blocked by security software, or the package is invalid; you can retry, or download it manually from Releases.\nLog: {log}"),
    ("notify_update_not_applied", "更新未生效", "The update did not take effect"),
    ("plugin_source_local", "本地 · {path}", "Local · {path}"),
    ("plugin_source_git", "Git 源", "Git source"),
    ("output_too_long_truncated", "…（输出过长，截断前 {n} 字符）\n{kept}", "… (output too long — the first {n} characters were dropped)\n{kept}"),
    ("err_dsh_closure_not_found", "dsh 闭包未找到：{path}", "dsh closure not found: {path}"),
    ("err_bundled_pnpm_missing_hint", "内置 pnpm 缺失：{path}\n请重新安装 App（或自行安装 pnpm 后重试）", "Bundled pnpm is missing: {path}\nPlease reinstall the app (or install pnpm yourself and retry)"),
    ("err_write_pnpm_workspace", "写入 pnpm-workspace.yaml 失败：{e}", "Could not write pnpm-workspace.yaml: {e}"),
    ("err_run_dsh_plugin", "执行 dsh plugin 失败：{e}", "Could not run dsh plugin: {e}"),
    ("err_read_plugin_stdout", "无法读取 dsh plugin stdout", "Could not read dsh plugin stdout"),
    ("err_read_plugin_stderr", "无法读取 dsh plugin stderr", "Could not read dsh plugin stderr"),
    ("err_wait_dsh_plugin", "等待 dsh plugin 退出失败：{e}", "Could not wait for dsh plugin to exit: {e}"),
    ("plugin_exit_code", "退出码 {status}\n\n{output}", "Exit code {status}\n\n{output}"),
    ("err_unsupported_plugin_op", "不支持的插件操作：{op}（仅支持 add / remove）", "Unsupported plugin operation: {op} (only add / remove are supported)"),
    ("err_invalid_plugin_target", "安装目标不合法：支持 npm 包名（@scope/pkg）、Git/tarball 源（owner/repo、github:owner/repo、git+ssh://…、git+https://…、https://…tgz）或本地插件目录的绝对路径（如 D:\\plugins\\my-plugin）；路径不能含引号/通配符等特殊字符，任何形式都不能以 - 开头，也不支持相对路径", "Invalid install target: use an npm package name (@scope/pkg), a Git/tarball source (owner/repo, github:owner/repo, git+ssh://…, git+https://…, https://…tgz), or the absolute path of a local plugin directory (e.g. D:\\plugins\\my-plugin); paths must not contain quotes or wildcards, nothing may start with -, and relative paths are not supported"),
    ("err_plugin_thread", "插件操作线程异常：{e}", "Plugin operation thread failed: {e}"),
    ("plugin_output_capped", "…（输出过长，仅保留末尾部分；完整日志见 launcher.log）", "… (output too long — only the tail is kept; see launcher.log for the full log)"),
    ("err_plugin_update_thread", "检查插件更新线程异常：{e}", "Plugin update check thread failed: {e}"),
    ("err_read_registry_response", "读取 registry 响应失败: {e}", "Could not read the registry response: {e}"),
    ("err_parse_registry_response", "解析 registry 响应失败: {e}", "Could not parse the registry response: {e}"),
    ("err_registry_no_version_field", "registry 响应缺少 version 字段", "The registry response has no version field"),
    ("err_query_version_existence", "查询版本存在性失败: {e}", "Could not check whether the version exists: {e}"),
    ("workbench_window_title", "DeepSeek Harness 工作台", "DeepSeek Harness Workbench"),
    ("err_dsh_busy", "正在安装/更新 dsh，请稍后再试", "dsh is being installed/updated — please wait"),
];

/// 进程内首次解析结果（`OnceLock`：`locale()` 只在首次访问时读一次盘）。
static LOCALE: OnceLock<String> = OnceLock::new();

/// 当前生效语种（`None` = 还没解析过）。`refresh_locale` 只更新这里：
/// `OnceLock` 写入后不可覆盖，而语种在运行期可以变（用户在设置里切语言）。
/// 取值只有 `"zh"` / `"en"` 两个 `&'static str` 字面量，所以 `locale()` 能返回
/// `&'static str` 而不需要任何 leak/unsafe。
static LIVE: Mutex<Option<&'static str>> = Mutex::new(None);

/// 把 `resolve_locale` 的原始取值（`zh` / `en` / `en-US` 这类）归一到 `"zh"` / `"en"`，
/// 判定与前端 `DSH_I18N.setLocale` 一致（`en` 开头即英文，其余中文）。
fn normalize(raw: &str) -> &'static str {
    if raw.to_ascii_lowercase().starts_with("en") { "en" } else { "zh" }
}

/// 查表（不依赖语种）：命中返回整行，未命中返回 `None`。
fn row(key: &str) -> Option<&'static (&'static str, &'static str, &'static str)> {
    DICT.iter().find(|r| r.0 == key)
}

/// 按语种取行内文案；英文列为空时回落中文。
fn pick(locale: &str, r: &'static (&'static str, &'static str, &'static str)) -> &'static str {
    if locale == "en" && !r.2.is_empty() { r.2 } else { r.1 }
}

/// 当前语种（`"zh"` / `"en"`）：首次调用读一次盘并缓存，之后全进程复用。
pub fn locale() -> &'static str {
    if let Some(v) = *crate::mlock(&LIVE) {
        return v;
    }
    let cached = LOCALE.get_or_init(|| crate::resolve_locale(&crate::home_dir()));
    normalize(cached)
}

/// 重新解析语种并更新进程内缓存，返回本次解析到的原始取值。
/// 壳页每次拉 `get_shell_state` 都会调用它，所以改完 `locale.preference` 刷新壳页即生效。
/// 直接把当前语种设为 `raw` 归一化后的值。**返回 true 表示确实变了**——
/// 工作台事件链路据此判断要不要重建托盘/菜单栏（语种没变时一次菜单都不建）。
pub fn set_locale(raw: &str) -> bool {
    let want = normalize(raw);
    let mut live = crate::mlock(&LIVE);
    if *live == Some(want) {
        return false;
    }
    *live = Some(want);
    true
}

pub fn refresh_locale() -> String {
    let raw = crate::resolve_locale(&crate::home_dir());
    // 首次访问顺手填充「只读一次盘」的缓存（之后 locale() 不再碰磁盘）。
    let _ = LOCALE.get_or_init(|| raw.clone());
    *crate::mlock(&LIVE) = Some(normalize(&raw));
    raw
}

/// 指定语种取词（纯函数，便于测试与排查）：命中英文列就用英文，
/// 否则回落中文，两者都空才返回键名本身（让漏翻一眼可见）。
pub fn tr_in(locale: &str, key: &str) -> String {
    match row(key) {
        Some(r) => pick(locale, r).to_string(),
        None => key.to_string(),
    }
}

/// 当前语种取词。
pub fn tr(key: &str) -> String {
    tr_in(locale(), key)
}

/// 带 `{name}` 占位符的取词（与前端 `T(key, vars)` 同语义）：找不到的占位符原样保留。
pub fn tr_args(key: &str, args: &[(&str, &str)]) -> String {
    let mut s = tr(key);
    for (name, value) in args {
        s = s.replace(&format!("{{{name}}}"), value);
    }
    s
}

/// 只能放 `&'static str` 的位置用（`const` / `&str` 字段 / `starts_with` 比较）：
/// 对同一张静态表再做一次查表，命中的是表里的 `&'static str` 字面量本身——
/// 不分配、不 leak、无 unsafe。键也必须是 `&'static str`，这样未命中时能原样返回。
pub fn tr_static(key: &'static str) -> &'static str {
    match row(key) {
        Some(r) => pick(locale(), r),
        None => key,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 键唯一、且中英两列都非空（漏填英文列会静默回落中文，必须在测试里挡住）。
    #[test]
    fn dict_keys_unique_and_translated() {
        let mut seen = std::collections::HashSet::new();
        for (k, zh, en) in DICT.iter() {
            assert!(seen.insert(*k), "重复的键: {k}");
            assert!(!zh.is_empty(), "{k} 缺中文");
            assert!(!en.is_empty(), "{k} 缺英文");
        }
    }

    /// 复用前端键的行必须与 `ui/i18n.js` 文案逐字一致（键同义同文案）。
    #[test]
    fn reused_keys_match_frontend_wording() {
        assert_eq!(tr_in("zh", "unknown"), "未知");
        assert_eq!(tr_in("en", "unknown"), "Unknown");
        assert_eq!(tr_in("zh", "not_installed"), "未安装");
        assert_eq!(tr_in("en", "not_installed"), "Not installed");
        assert_eq!(tr_in("en", "uninstall_incomplete"), "Uninstall incomplete");
        assert_eq!(tr_in("en", "install_already_running"), "An install is already in progress");
        assert_eq!(tr_in("en", "registry_empty"), "Registry cannot be empty");
        assert_eq!(tr_in("en", "uninstall_full_b"), "Uninstall everything");
        assert_eq!(tr_in("en", "retry"), "Retry");
        assert_eq!(tr_in("en", "cancel"), "Cancel");
    }

    /// 未命中 → 键名本身；占位符替换与前端 `T(key, vars)` 同语义；`tr_static` 与 `tr` 同源。
    #[test]
    fn lookups_fall_back_and_fill_placeholders() {
        assert_eq!(tr_in("zh", "no_such_key"), "no_such_key");
        assert_eq!(tr_in("en", "no_such_key"), "no_such_key");
        assert_eq!(tr_in("zh", "err_create_dir"), "创建目录失败: {e}");
        assert_eq!(tr_args("progress_switching_existing", &[("ver", "0.1.5")]),
                   tr_in(locale(), "progress_switching_existing").replace("{ver}", "0.1.5"));
        assert_eq!(tr_static("workbench_window_title"), tr("workbench_window_title"));
    }

    /// 语种归一：`resolve_locale` 可能给出 `en-US` / `zh-CN` 这类取值，
    /// 判定与前端 `DSH_I18N.setLocale` 一致（`en` 开头即英文，其余中文）。
    #[test]
    fn locale_normalization_matches_frontend() {
        assert_eq!(normalize("en"), "en");
        assert_eq!(normalize("en-US"), "en");
        assert_eq!(normalize("EN"), "en");
        assert_eq!(normalize("zh"), "zh");
        assert_eq!(normalize("zh-CN"), "zh");
        assert_eq!(normalize(""), "zh");
    }
}
