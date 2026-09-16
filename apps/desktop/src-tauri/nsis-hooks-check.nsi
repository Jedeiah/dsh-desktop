; =============================================================================
; NSIS 钩子「编译检查」脚本 —— **不参与打包**，只给 CI 用。
;
; 为什么需要：installer-hooks.nsh 只在 Windows 打包时装进 tauri-bundler 的 installer.nsi
; 参与编译，本地改坏语法要等到发版（Windows job 构建 NSIS 安装器）才会炸——那时已经晚了。
; 这个脚本把真实钩子放进**最小但等价**的上下文里编译一次：
;   · 与模板同样的 include 顺序（MUI2 → FileFunc → 钩子；模板 installer.nsi:22-35），
;     所以 ${If}/${FileExists} 这些宏在这里的可见性与真实构建一致；
;   · 定义模板会给的宏（MAINBINARYNAME / BUNDLEID / MANU* / PRODUCTNAME）；
;   · 在 Section Uninstall 里按模板的位置（installer.nsi:779 / 887）插入两个钩子。
; 本地跑（需要 makensis）：
;   cp nsis-hooks-check.nsi installer-hooks.nsh "$(mktemp -d)"/ && cd "$_" && makensis nsis-hooks-check.nsi
; 注意 OutFile 是相对**脚本所在目录**解析的（不是 cwd）：在仓库里直接跑会在本目录留下
; hook-check.exe，所以上面先拷到临时目录再编；该产物也已在 .gitignore 里。
; =============================================================================

Unicode true
Name "DeepSeek Harness Desktop"
OutFile "hook-check.exe"
InstallDir "$LOCALAPPDATA\DeepSeek Harness Desktop"
RequestExecutionLevel user

; 与 tauri-bundler 生成的 installer.nsi 一致的宏
!define MAINBINARYNAME "dsh-desktop"
!define PRODUCTNAME "DeepSeek Harness Desktop"
!define MANUFACTURER "dsh-desktop"
!define BUNDLEID "com.dsh-desktop.app"
!define MANUKEY "Software\${MANUFACTURER}"
!define MANUPRODUCTKEY "${MANUKEY}\${PRODUCTNAME}"
!define UNINSTKEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\${PRODUCTNAME}"

; include 顺序照抄模板：MUI2（带进 LogicLib）→ FileFunc（${FileExists}）→ 真实钩子
!include MUI2.nsh
!include FileFunc.nsh
!include "installer-hooks.nsh"

Section "Install"
  SetShellVarContext current
  SetOutPath "$INSTDIR"
  WriteUninstaller "$INSTDIR\uninstall.exe"
SectionEnd

Section "Uninstall"
  ; 模板里的两个插入点（installer.nsi:779 / 887）
  !insertmacro NSIS_HOOK_PREUNINSTALL
  SetShellVarContext current
  RMDir /r "$INSTDIR"
  DeleteRegKey SHCTX "${UNINSTKEY}"
  !insertmacro NSIS_HOOK_POSTUNINSTALL
SectionEnd
