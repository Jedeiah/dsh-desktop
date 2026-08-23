# Changelog

## 0.3.0 — 瘦壳重设计（2026-08-23）

**产品定位重构**：从「内置 340MB dsh 闭包的胖壳」变为「只内置 node + npm + pnpm 的瘦壳」（安装包约 50–180MB），dsh 按需从 npm registry 安装。

### 新增

- **首次引导安装**：无 dsh 闭包时显示引导页——默认安装 latest，可展开高级选项（registry 源 + 指定版本），实时进度、可取消、失败重试
- **dsh 版本管理**（dsh 页）：当前版本、启动静默检查新版、一键更新到 latest、版本列表（registry 全部版本）任意安装/切换、自动回滚到上一版本；registry 源可持久化配置（npmjs / npmmirror）
- **App 自身更新**（关于页）：检查 GitHub Releases、一键下载安装（macOS DMG / Windows NSIS 静默）、「在浏览器打开下载页」手动兜底
- **插件管理**（插件页）：已安装插件列表（web profile，与终端共用）、安装/卸载、实时输出、完成后自动重启工作台
- **CLI 无头工具**保留：`dsh-desktop --self-update-check` / `--self-apply-update <ver>`

### 移除

- 局域网扫码远程连接（lan-proxy / mDNS / 二维码）
- 常规页（工作目录选择、登录自启、手动重启、日志查看 UI）
- 内置 dsh 闭包（resources/dsh）与上游自动发版（upstream-check workflow）

### 内部

- 模块化：`registry.rs`（npm 查询）/ `dsh.rs`（闭包管理：tmp→双重自检→原子切换→GC 保留上一版）/ `plugin.rs` / `appupdate.rs`
- 失败安全：更新/安装任何失败不动当前版本；`~/.dsh` 永不自动删除
- 壳页重构：4 Tab（工作台 / dsh / 插件 / 关于）+ 首次引导视图，设计稿由 open-design 生成（紫粉蓝光斑玻璃拟态 + 对比度工程）

### 修复

- 版本比较 rc 数字感知（`rc.10 > rc.9`）
- 安装切换失败窗口（`.old` 延迟删除）
- dsh 版本参数白名单校验（防 npm 参数注入）
- macOS x86_64 无 CI 产物时 App 更新走手动兜底
