; Remove only this installation's opt-in startup entry on a real uninstall.
; Keep it on Tauri updates, and never touch another install's registration.
;
; Two accepted spellings: this version registers the silent `--tray` form, while
; an entry created by 0.1.x has no argument. Both must point at exactly this EXE.
!macro NSIS_HOOK_PREUNINSTALL
  ${If} $UpdateMode <> 1
    Push $R0
    ReadRegStr $R0 HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "Pi Launcher"
    ${If} $R0 == '$\"$INSTDIR\${MAINBINARYNAME}.exe$\" --tray'
      DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "Pi Launcher"
    ${ElseIf} $R0 == '$\"$INSTDIR\${MAINBINARYNAME}.exe$\"'
      DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "Pi Launcher"
    ${EndIf}
    Pop $R0
  ${EndIf}
!macroend
