; =============================================================================
; DSh Desktop — NSIS 自定义卸载钩子（完全卸载）
; 被 tauri-bundler 的 installer.nsi 在编译期 include（bundle.windows.nsis.installerHooks）。
; 这里只定义宏，实际执行点在 NSIS 的 Uninstall 段（NSIS_HOOK_PREUNINSTALL 位于
; 删文件与 CheckIfAppIsRunning 之前）。
;
; 解决的问题（用户反馈"右键→卸载 无反应 / uninstall.exe 不齐全"）：
;   - App 常驻托盘、主窗隐藏：从系统卸载时 dsh-desktop.exe 与其 node/dsh/lan 子进程
;     仍在运行，占用 $INSTDIR 程序文件与 WebView2 数据 → NSIS 删文件必然失败且无反馈。
;   - 默认 NSIS 卸载器不清理 %LOCALAPPDATA%\<id>（WebView2 用户数据）与 %APPDATA%\<id>
;     （app 数据：dsh 闭包 / 日志 / 设置）。
;
; 方案：PREUNINSTALL 先调用本应用卸载 sidecar `--self-uninstall-full`：
;   - 结束其它运行实例（进程树杀）→ 释放文件锁；
;   - 复用现有 uninstall_teardown 清理用户数据（app 数据 / WebView2，可选 ~/.dsh）。
;   - sidecar 失败/退出码非 0 不阻断 NSIS 删程序文件（容忍清理）。
; 注意：sidecar 由 NSIS 直接以系统账户权限运行（同用户态），无需提权脚本。
; =============================================================================

!macro NSIS_HOOK_PREUNINSTALL
  ; 逃生舱（默认不可用）：若需跳过卸载 sidecar，可在打包命令注入
  ; `-DSH_SKIP_SELF_UNINSTALL`（makensis 定义），此处即不执行。正常构建不定义。
  !ifndef DSH_SKIP_SELF_UNINSTALL
    ; App 内点击"卸载"会以 /S 静默方式启动本卸载器（App 侧已弹过确认窗）：静默模式下
    ; 模板的确认页被跳过，其"删除应用数据"勾选框（$DeleteAppDataCheckboxState）恒为 0，
    ; 所以 app 数据 / WebView2 缓存由下面的 sidecar 无条件清理，不依赖该勾选框。
    ; 从系统"已安装的应用"卸载时确认页会出现，但该勾选框同样不影响本钩子——数据清理
    ; 始终发生（app 数据视为可再生成，见 uninstall_teardown）。若将来要尊重勾选，
    ; 需在此把状态作为参数传给 sidecar。
    ; 结束运行实例 + 清理用户数据。ExecWait 同步等待 sidecar 结束并取退出码，
    ; 失败（非 0）不中断卸载、直接继续删 $INSTDIR。注意：ExecWait **没有超时**——
    ; sidecar 若挂住，卸载器会一起等下去（正常路径是它自己清完数据就退出）。
    ; ${MAINBINARYNAME} 在本处可用（installer.nsi:52 已定义；宏在 Section
    ; Uninstall 处展开时符号已就绪），指向本应用主程序。
    ExecWait '"$INSTDIR\${MAINBINARYNAME}.exe" --self-uninstall-full' $0
    ; $0 = 退出码：0=数据清理完成；非 0=清理失败，NSIS 仍继续删 $INSTDIR。
  !endif
!macroend

; POSTUNINSTALL：卸载收尾。两件事：
;
; ① 兜底提示"没清干净"。NSIS 的 Delete / RMDir 失败**完全是静默的**——只置内部错误位，
;    不弹框、不中断（核对过 NSIS 源码：util.c 的 myDelete 失败路径只 `exec_error++`；
;    只有 File 指令才有 AllowSkipFiles 那套对话框，而它作用于安装期写文件）。所以若删
;    文件那一刻仍有进程占着（node/dsh 子进程没退干净、杀毒软件正在扫描），对应文件会被
;    跳过，而非递归的 `RMDir "$INSTDIR"` 也就删不掉目录——用户看到的是"卸载成功"、
;    目录却还在，且没有任何提示。
;    本 App 内卸载是**静默**的（/S，App 已退出、没有界面可回显），这是唯一能给用户反馈的
;    通道：MessageBox 不带 /SD 时在静默模式也会显示（NSIS 源码 util.c 的 my_MessageBox：
;    只有 `type>>21` 有默认值时才被抑制）。为避免打扰正常卸载，**只在真有残留时**才弹。
;
; ② 幂等清理自启注册表项（本 App 目前**没有**开机自启功能，该键不会被创建；保留此删除
;    是防止将来加了自启却忘了卸载时清理）。
!macro NSIS_HOOK_POSTUNINSTALL
  ; 三处关键位置任一仍存在即判定"没清干净"：程序目录（$INSTDIR 是非递归 RMDir，被占用的
  ; 文件会让它删不掉）、%APPDATA%\<id>（app 数据：dsh 闭包/日志/设置）、
  ; %LOCALAPPDATA%\<id>（WebView2 用户数据与缓存）。后两处由 PREUNINSTALL 的 sidecar 清，
  ; sidecar 没跑成（被安全软件拦下等）时它们会原样留下——正是这个提示要覆盖的情形。
  ; `${FileExists}` 由 LogicLib.nsh 提供（`:339`，宏体是 `IfFileExists` 指令）；用
  ; `"$dir\*.*"` 判断"目录是否存在"是 NSIS 自带分发包里的惯用法（FileFunc.nsh 内部多处
  ; 直接用 `IfFileExists` 指令这么写，如 `:329/:453/:546`）。两者都在模板的 include 里
  ; （installer.nsi:22 MUI2 → 带进 LogicLib；:23 FileFunc），且都早于钩子插入点。
  ${If} ${FileExists} "$INSTDIR\*.*"
  ${OrIf} ${FileExists} "$APPDATA\${BUNDLEID}\*.*"
  ${OrIf} ${FileExists} "$LOCALAPPDATA\${BUNDLEID}\*.*"
    MessageBox MB_ICONEXCLAMATION|MB_OK "卸载未完全清理：仍有文件被占用（例如 node 进程未退出、杀毒软件正在扫描）。$\r$\n$\r$\n重启电脑后手动删除以下残留目录即可：$\r$\n・$INSTDIR$\r$\n・$APPDATA\${BUNDLEID}$\r$\n・$LOCALAPPDATA\${BUNDLEID}"
  ${EndIf}
  DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "DeepSeek Harness"
!macroend
