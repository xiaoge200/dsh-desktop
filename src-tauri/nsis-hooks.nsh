# Tauri NSIS installer hooks
#
# 钩子格式：Tauri 生成的 NSIS 会检查 `!ifmacrodef NSIS_HOOK_XXX` 并
# `!insertmacro NSIS_HOOK_XXX`，本文件通过 `!macro` 定义这些钩子。
#
# - NSIS_HOOK_PREINSTALL：覆盖/升级安装前结束应用进程树（含 node 服务进程），
#   否则 node 子进程存活会锁住文件导致覆盖失败（「node 被占用」）。
# - NSIS_HOOK_PREUNINSTALL：卸载删文件之前结束进程树，并弹窗询问是否保留
#   用户数据（FR-13）——用户选择「是」保留数据（默认），「否」则删除。
#   多语言：按 NSIS 当前语言（$(^Language)）选择询问文案。

!macro NSIS_HOOK_PREINSTALL
  ; 覆盖/升级安装前：整棵树结束正在运行的应用及其 node 服务进程。
  ; 安装器默认只杀主进程（dsh-desktop.exe），其子进程 node 服务会存活并
  ; 锁住 resources\node\... 正在运行的文件，导致覆盖时报「node 被占用」。
  nsExec::ExecToLog 'taskkill /F /T /IM "${MAINBINARYNAME}.exe"'
  Sleep 500
  ; 兜底：清理命令行含本应用安装目录的残留 node 进程（旧版本遗留的孤儿进程）
  nsExec::ExecToLog "powershell.exe -NoProfile -Command Get-CimInstance Win32_Process | Where-Object { $$_.Name -eq $\'node.exe$\' -and $$_.ExecutablePath -like $\'$INSTDIR*$\' } | ForEach-Object { Stop-Process -Id $$_.ProcessId -Force }"
  Sleep 500
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  ; 卸载前同样先结束进程树，否则 node 服务进程存活会锁住文件导致删除失败
  nsExec::ExecToLog 'taskkill /F /T /IM "${MAINBINARYNAME}.exe"'
  Sleep 500
  nsExec::ExecToLog "powershell.exe -NoProfile -Command Get-CimInstance Win32_Process | Where-Object { $$_.Name -eq $\'node.exe$\' -and $$_.ExecutablePath -like $\'$INSTDIR*$\' } | ForEach-Object { Stop-Process -Id $$_.ProcessId -Force }"
  Sleep 500
  ; 默认保留（$R0=0 保留，$R0=1 删除）
  StrCpy $R0 "0"
  ; 静默卸载（/S）时跳过弹窗，默认保留数据（防挂起）
  IfSilent +9
  ; 简体中文 (2052)
  ${If} $(^Language) = 2052
    MessageBox MB_YESNO|MB_ICONQUESTION|MB_DEFBUTTON1 \
      "是否保留 DSH 工作台的数据（会话记录、配置等）？$\r$\n$\r$\n选择「是」保留数据，选择「否」一并删除。" \
      IDYES +2
    StrCpy $R0 "1"
  ${ElseIf} $(^Language) = 1028
    MessageBox MB_YESNO|MB_ICONQUESTION|MB_DEFBUTTON1 \
      "是否保留 DSH 工作台的資料（會話記錄、設定等）？$\r$\n$\r$\n選擇「是」保留資料，選擇「否」一併刪除。" \
      IDYES +2
    StrCpy $R0 "1"
  ${Else}
    MessageBox MB_YESNO|MB_ICONQUESTION|MB_DEFBUTTON1 \
      "Keep DSH Workspace data (sessions, settings, etc.)?$\r$\n$\r$\nChoose 'Yes' to keep data, 'No' to delete everything." \
      IDYES +2
    StrCpy $R0 "1"
  ${EndIf}
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  ; 用户选择删除时，清理用户数据目录
  StrCmp $R0 "1" 0 +2
  RMDir /r "$APPDATA\com.dsh.desktop"
!macroend
