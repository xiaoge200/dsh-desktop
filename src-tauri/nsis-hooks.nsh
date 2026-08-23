# Tauri NSIS installer hooks（FR-13：卸载时询问是否保留用户数据）
#
# 钩子格式：Tauri 生成的 NSIS 会检查 `!ifmacrodef NSIS_HOOK_XXX` 并
# `!insertmacro NSIS_HOOK_XXX`，本文件通过 `!macro` 定义这些钩子。
#
# 这里用 NSIS_HOOK_PREUNINSTALL：卸载删文件之前弹窗询问，
# 用户选择「是」保留数据（默认），「否」则在卸载后删除 %APPDATA%\com.dsh.desktop。
# 多语言：按 NSIS 当前语言（$(^Language)）选择询问文案。

!macro NSIS_HOOK_PREUNINSTALL
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
