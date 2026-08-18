; =============================================================================
; DSh Desktop — NSIS 自定义卸载钩子（完全卸载）
; 被 tauri-bundler 的 installer.nsi 在编译期 include（bundle.windows.nsis.installerHooks）。
; 这里只定义宏，实际执行点在 NSIS 的 Uninstall 段（NSIS_HOOK_PREUNINSTALL 位于
; 删文件与 CheckIfAppIsRunning 之前）。
;
; 解决的问题（用户反馈"右键→卸载 无反应 / uninstall.exe 不齐全"）：
;   - App 常驻托盘、主窗隐藏：从系统卸载时 dsh-desktop.exe 与其 node/dsh/lan 子进程
;     仍在运行，占用 $INSTDIR 程序文件与 WebView2 数据 → NSIS 删文件必然失败且无反馈。
;   - 默认 NSIS 卸载器不清理 %LOCALAPPDATA%\<id>、WebView2 缓存、登录自启。
;
; 方案：PREUNINSTALL 先调用本应用卸载 sidecar `--self-uninstall-full`：
;   - 结束其它运行实例（进程树杀）→ 释放文件锁；
;   - 复用现有 uninstall_teardown 清理用户数据（app 数据 / WebView2 / 登录自启，
;     可选 ~/.dsh）。
;   - sidecar 失败/退出码非 0 不阻断 NSIS 删程序文件（容忍清理）。
; 注意：sidecar 由 NSIS 直接以系统账户权限运行（同用户态），无需提权脚本。
; =============================================================================

!macro NSIS_HOOK_PREUNINSTALL
  ; 逃生舱（默认不可用）：若需跳过卸载 sidecar，可在打包命令注入
  ; `-DSH_SKIP_SELF_UNINSTALL`（makensis 定义），此处即不执行。正常构建不定义。
  !ifndef DSH_SKIP_SELF_UNINSTALL
    ; 结束运行实例 + 清理用户数据。nsExec::Exec 同步等待 sidecar 结束，
    ; 但失败（退出码 / 超时）不中断卸载：用 ExecWait 拿退出码后忽略。
    ; ${MAINBINARYNAME} 在本处可用（installer.nsi:52 已定义；宏在 Section
    ; Uninstall 处展开时符号已就绪），指向本应用主程序。
    ExecWait '"$INSTDIR\${MAINBINARYNAME}.exe" --self-uninstall-full' $0
    ; $0 = 退出码：0=数据清理完成；非 0=清理失败，NSIS 仍继续删 $INSTDIR。
  !endif
!macroend

; POSTUNINSTALL：兜底清扫（可选）。当前默认卸载器已删除开始菜单快捷方式与
; Uninstall 注册表项；此处额外清理可能残留的登录自启注册表项（幂等）。
!macro NSIS_HOOK_POSTUNINSTALL
  DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "DeepSeek Harness"
!macroend
