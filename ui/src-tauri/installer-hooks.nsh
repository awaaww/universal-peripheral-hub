; Runs inside the Tauri NSIS installer.
;
; Install: register the per-user Run value and start the core, so the first
; thing the user sees after installing is a working tray icon.
; Uninstall: stop the processes and remove the Run value; user data in
; %APPDATA%\UniversalPeripheralHub is deliberately kept.

!macro NSIS_HOOK_POSTINSTALL
  DetailPrint "Registering Universal Peripheral Hub for this user"
  ; `nsExec::Exec` waits for the child to exit, and the resident hubd never
  ; exits, so the long-running start must be async. hubd hides its own console
  ; window when it starts with no arguments, so the flash is momentary.
  ExecWait '"$INSTDIR\hubd.exe" --autostart on'
  Exec '"$INSTDIR\hubd.exe"'
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  DetailPrint "Stopping Universal Peripheral Hub"
  ExecWait '"$INSTDIR\hubd.exe" --autostart off'
  nsExec::Exec 'taskkill /IM hubd.exe /F'
  nsExec::Exec 'taskkill /IM atk-adapter.exe /F'
  nsExec::Exec 'taskkill /IM via-adapter.exe /F'
!macroend
