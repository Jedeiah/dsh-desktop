# 贡献指南

欢迎 issue 与 PR。**最需要的是平台实测反馈**：Windows 行为与 macOS 的差异、WebView2 环境问题、安装/更新/卸载在真实机器上的表现——这些在没有对应机器的开发环境里很难覆盖。

## 提交前

```sh
cargo clippy --all-targets          # 必须零告警
cargo test                          # 必须全绿
node --check apps/desktop/ui/*.js   # 壳页脚本语法检查
```

涉及交互的改动请对照 [`docs/regression-checklist.md`](docs/regression-checklist.md) 自查。

## 分支与提交

- 一个 PR 只做一件可描述的事；行为变化请在描述里写清"改之前 / 改之后"。
- 提交信息用 `<type>(<scope>): <说明>` 形式（`feat` / `fix` / `chore` / `docs` / `refactor`），与原仓库历史一致。
- 版本号只在发版时统一改（见 `scripts/bump-version.sh`），功能 PR 不要动版本号。

## 设计文档

设计取舍与实现细节在 [`docs/`](docs/) 下（含 `docs/superpowers/` 的设计记录）。要改架构请先开 issue 对齐思路，避免做完才发现方向不同。

## 安全相关

安全边界（只绑 loopback、IPC 最小授权、日志脱敏、卸载只清理精确路径）见 README「数据、隐私与安全」。**改动这些边界请在 PR 描述里显式说明**，并补上对应测试。

## 不接受的改动

- 在壳里注入代码、修改或 patch 官方 dsh（本项目的核心承诺是"零偏差"）。
- 收集/上报任何用户数据。
- 未经讨论就更换技术栈（Tauri → Electron 等）。
