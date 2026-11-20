; wStats installer hooks (Tauri NSIS). The installer runs elevated (perMachine), so the
; sensor service and the PawnIO driver are set up here, under its single UAC prompt.
; This file is UTF-8 with a BOM: NSIS needs the BOM to read the accents right.
; Text follows the installer's language: Brazilian Portuguese (1046), English otherwise.

!macro NSIS_HOOK_POSTINSTALL
  ; Start with Windows. Also replaces a stale path (a development build, an old folder).
  ; Not when the user turned it off in wStats (Autostart = 0): installing an update over
  ; the old version would switch it back on.
  ${If} $UpdateMode <> 1
    ClearErrors
    ReadRegDWORD $0 HKCU "Software\wStats" "Autostart"
    ${If} ${Errors}
    ${OrIf} $0 <> 0
      WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "${PRODUCTNAME}" "$INSTDIR\${MAINBINARYNAME}.exe"
    ${EndIf}
  ${EndIf}

  ; Sensors. Already set up (an update): refresh the service. First install: ask, yes by
  ; default. `wstats-sensors install` also installs PawnIO from $INSTDIR\pawnio if missing.
  ClearErrors
  ReadRegDWORD $0 HKLM "SYSTEM\CurrentControlSet\Services\wStatsSensors" "Start"
  ${If} ${Errors}
    ${If} $LANGUAGE = 1046
      StrCpy $1 "Ativar a temperatura da CPU e as ventoinhas?$\r$\n$\r$\nO wStats instala um pequeno serviço do Windows que lê esses sensores e, se ainda não estiver presente, o driver PawnIO (assinado, o mesmo usado pelo LibreHardwareMonitor e pelo FanControl).$\r$\n$\r$\nDá para ligar ou desligar depois em Configurações › Temperatura."
    ${Else}
      StrCpy $1 "Turn on CPU temperature and fans?$\r$\n$\r$\nwStats installs a small Windows service that reads these sensors and, if it is not there yet, the PawnIO driver (signed, the same LibreHardwareMonitor and FanControl use).$\r$\n$\r$\nYou can turn this on or off later in Settings › Temperature."
    ${EndIf}
    MessageBox MB_YESNO|MB_ICONQUESTION "$1" /SD IDYES IDNO wstats_sensors_done
  ${EndIf}
  DetailPrint "wstats-sensors install"
  nsExec::Exec '"$INSTDIR\wstats-sensors.exe" install'
  Pop $0
  ${If} $0 <> 0
    DetailPrint "wstats-sensors install: $0"
  ${EndIf}
  wstats_sensors_done:
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  ; An update keeps the service; a real uninstall removes it before its files go.
  ${If} $UpdateMode <> 1
  ${AndIf} ${FileExists} "$INSTDIR\wstats-sensors.exe"
    DetailPrint "wstats-sensors uninstall"
    nsExec::Exec '"$INSTDIR\wstats-sensors.exe" uninstall'
    Pop $0
  ${EndIf}
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  ${If} $UpdateMode <> 1
    ; History and logs live outside the identifier folders the template cleans up. The
    ; DLLs Explorer still holds go on the next restart.
    ${If} $DeleteAppDataCheckboxState = 1
      SetShellVarContext current
      RMDir /r /REBOOTOK "$LOCALAPPDATA\wStats"
      DeleteRegKey HKCU "Software\wStats"
    ${EndIf}

    ; PawnIO is shared with other tools, so it stays unless asked otherwise.
    ${If} ${FileExists} "$PROGRAMFILES64\PawnIO\uninstall.exe"
      ${If} $LANGUAGE = 1046
        StrCpy $1 "Remover também o driver PawnIO?$\r$\n$\r$\nOutros programas (LibreHardwareMonitor, FanControl e outros) podem usá-lo. Na dúvida, mantenha."
      ${Else}
        StrCpy $1 "Remove the PawnIO driver too?$\r$\n$\r$\nOther programs (LibreHardwareMonitor, FanControl and others) may use it. If unsure, keep it."
      ${EndIf}
      MessageBox MB_YESNO|MB_ICONQUESTION|MB_DEFBUTTON2 "$1" /SD IDNO IDNO wstats_pawnio_kept
      nsExec::Exec '"$PROGRAMFILES64\PawnIO\uninstall.exe" -uninstall -silent'
      Pop $0
      wstats_pawnio_kept:
    ${EndIf}
  ${EndIf}
!macroend
