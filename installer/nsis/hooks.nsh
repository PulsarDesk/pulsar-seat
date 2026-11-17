; SPDX-License-Identifier: GPL-3.0-only
;
; Pulsar Seat - Tauri 2 NSIS installer hooks (bundle.windows.nsis.installerHooks).
; Adapted from pulsar-split (installer/nsis/hooks.nsh, same author, GPL-3.0).
; Equivalent of ../wix/PulsarSeat.wxs for the optional -setup.exe:
;   PREINSTALL    arch guard, Windows build, coexistence (refuse beside the reference
;                 product, warn on a non-default TermService DLL), full pre-flight gate
;                 run from $PLUGINSDIR BEFORE any file is laid down, stop seats on upgrade
;   POSTINSTALL   optional choices (restore point, tweaks), `install`, Run entry, start
;   PREUNINSTALL  ask about user data, `uninstall` (reverts every journaled change)
;
; NOT SHIPPED (installer/README.md): Tauri's NSIS stub is a 32-bit x86 program, which
; breaks the "native on ARM64 and x64" rule (docs/agents/rules.md). The MSI is the
; deliverable on both architectures; these hooks stay correct for the day the maintainer
; approves an exception. The service CLI it calls (`preflight`, `install
; --register-service ...`, `uninstall --unregister-service ...`) is implemented in
; crates/seat-service/src/win/setup.rs (untested elevated).
;
; Command line (silent installs answer every question with its default = "no"):
;   /RESTOREPOINT                 create a System Restore point before system settings change
;   /TWEAKS=fast_startup_off,sleep_off,small_crash_dumps   (only a-z, _ and , accepted)
;   /REMOVEUSERDATA               uninstall also deletes %ProgramData%\Pulsar Seat
;
; Only our own binary is run, with an argv built here (every variable part quoted and
; validated). No firewall allow rules, no Defender exclusions, no certificates, nothing
; copied to System32 (the service's own firewall block rule for the passwordless-seat
; listener is removed by `uninstall`, ../README.md "Never"). Not executed (see ../README.md).

!include "x64.nsh"
!include "FileFunc.nsh"
!include "LogicLib.nsh"
!include "StrFunc.nsh"
${StrLoc}

!define PS_SERVICE_EXE "$INSTDIR\pulsar-seat-service.exe"
!define PS_SERVICE "PulsarSeat"
!define PS_RUN_KEY "SOFTWARE\Microsoft\Windows\CurrentVersion\Run"
!define PS_SETUP_LOG "$COMMONAPPDATA\Pulsar Seat\logs\setup.log"

; UI copy: English default, Turkish locale (Tauri loads both NSIS languages; NSIS picks
; the one of the Windows UI language). 1033 = English, 1055 = Turkish. Every id has both
; (installer/scripts/wix-locales.test.ts). HYPOTHESIS: the hooks are included after the
; languages are inserted (never built, see the header).
LangString PsStepFailed 1033 "Pulsar Seat setup step failed"
LangString PsStepFailed 1055 "Pulsar Seat kurulum adımı başarısız oldu"
LangString PsStepPartial 1033 "Pulsar Seat setup step partly done"
LangString PsStepPartial 1055 "Pulsar Seat kurulum adımı kısmen tamamlandı"
LangString PsChecksBlockedLog 1033 "The pre-flight checks blocked the installation. Details: ${PS_SETUP_LOG}"
LangString PsChecksBlockedLog 1055 "Ön denetimler kurulumu engelledi. Ayrıntılar: ${PS_SETUP_LOG}"
LangString PsRebootNeeded 1033 "Restart the PC to finish the changes."
LangString PsRebootNeeded 1055 "Değişikliklerin tamamlanması için bilgisayarı yeniden başlatın."
LangString PsServiceStuck 1033 "The Pulsar Seat service did not stop within 60 seconds. Restart the PC and try again."
LangString PsServiceStuck 1055 "Pulsar Seat hizmeti 60 saniye içinde durmadı. Bilgisayarı yeniden başlatıp tekrar deneyin."
LangString PsArchX64Only 1033 "This installer is for x64 PCs. Please use the ARM64 package."
LangString PsArchX64Only 1055 "Bu kurulum paketi x64 bilgisayarlar içindir. Lütfen ARM64 paketini kullanın."
LangString PsArchArm64Only 1033 "This installer is for ARM64 PCs. Please use the x64 package."
LangString PsArchArm64Only 1055 "Bu kurulum paketi ARM64 bilgisayarlar içindir. Lütfen x64 paketini kullanın."
LangString PsOsTooOld 1033 "Pulsar Seat needs Windows 10 22H2 (build 19045) or newer."
LangString PsOsTooOld 1055 "Pulsar Seat için Windows 10 22H2 (derleme 19045) veya daha yeni bir Windows gerekir."
LangString PsCoexistence 1033 "Another multiseat product is installed on this PC. Two multiseat products cannot run at the same time.$\r$\n$\r$\nFirst uninstall that product and restart the PC. Pulsar Seat does not touch that product's files or settings."
LangString PsCoexistence 1055 "Bu bilgisayarda başka bir çoklu koltuk ürünü kurulu. İki çoklu koltuk ürünü aynı anda çalışamaz.$\r$\n$\r$\nÖnce o ürünü kaldırın ve bilgisayarı yeniden başlatın. Pulsar Seat o ürünün dosyalarına veya ayarlarına dokunmaz."
LangString PsTermSrv 1033 "The Remote Desktop service (TermService) was changed by other software:$\r$\n$1$\r$\n$\r$\nPulsar Seat will be installed, but seats based on Remote Desktop will not be used on this PC. Pulsar Seat does not touch this setting."
LangString PsTermSrv 1055 "Uzak Masaüstü hizmeti (TermService) başka bir yazılım tarafından değiştirilmiş:$\r$\n$1$\r$\n$\r$\nPulsar Seat kurulacak, ancak Uzak Masaüstü tabanlı koltuklar bu bilgisayarda kullanılmayacak. Pulsar Seat bu ayara dokunmaz."
LangString PsChecksBlocked 1033 "The pre-flight checks blocked the installation; nothing was installed.$\r$\n$\r$\n$1"
LangString PsChecksBlocked 1055 "Ön denetimler kurulumu engelledi; hiçbir şey kurulmadı.$\r$\n$\r$\n$1"
LangString PsChecksFailed 1033 "The pre-flight checks could not run (exit code $0); nothing was installed.$\r$\n$\r$\n$1"
LangString PsChecksFailed 1055 "Ön denetimler çalıştırılamadı (çıkış kodu $0); hiçbir şey kurulmadı.$\r$\n$\r$\n$1"
LangString PsChecksWarn 1033 "Pre-flight check warnings:$\r$\n$\r$\n$1$\r$\nContinue the installation?"
LangString PsChecksWarn 1055 "Ön denetim uyarıları:$\r$\n$\r$\n$1$\r$\nKuruluma devam edilsin mi?"
LangString PsStopSeats 1033 "Pulsar Seat will be updated. All running seats will stop and the sessions on the seats will close.$\r$\n$\r$\nContinue?"
LangString PsStopSeats 1055 "Pulsar Seat güncellenecek. Çalışan tüm koltuklar durdurulacak ve koltuklardaki oturumlar kapanacak.$\r$\n$\r$\nDevam edilsin mi?"
LangString PsAskRestorePoint 1033 "Create a restore point before system settings change?"
LangString PsAskRestorePoint 1055 "Sistem ayarları değiştirilmeden önce bir geri yükleme noktası oluşturulsun mu?"
LangString PsBadTweaks 1033 "The /TWEAKS value is invalid (only lowercase letters, _ and commas are allowed); no system setting will change."
LangString PsBadTweaks 1055 "/TWEAKS değeri geçersiz (yalnızca küçük harf, _ ve virgül kullanılabilir); sistem ayarı değiştirilmeyecek."
LangString PsAskFastStartup 1033 "Turn off fast startup?$\r$\n$\r$\nAll seats start clean after a shutdown. The old setting comes back when Pulsar Seat is removed."
LangString PsAskFastStartup 1055 "Hızlı başlatma kapatılsın mı?$\r$\n$\r$\nKapatıp açınca tüm koltuklar temiz başlar. Pulsar Seat kaldırılınca eski ayar geri yüklenir."
LangString PsAskSleep 1033 "Turn off sleep and display off on mains power?$\r$\n$\r$\nWhile nobody works on one seat, the other seats do not sleep. The active power plan changes; it is restored at uninstall."
LangString PsAskSleep 1055 "Prizdeyken uyku ve ekran kapatma devre dışı bırakılsın mı?$\r$\n$\r$\nBir koltukta kimse çalışmıyorken diğer koltuklar uyumaz. Etkin güç planı değişir; kaldırınca geri yüklenir."
LangString PsAskDumps 1033 "Switch the system crash dump to a small dump? (Saves disk space.)"
LangString PsAskDumps 1055 "Sistem çökme dökümü küçük döküme çevrilsin mi? (Diskte yer kazandırır.)"
LangString PsSetupLog 1033 "Setup log: ${PS_SETUP_LOG}"
LangString PsSetupLog 1055 "Kurulum günlüğü: ${PS_SETUP_LOG}"
LangString PsAskRemoveData 1033 "Also delete the seat settings, logs and diagnostic reports (%ProgramData%\Pulsar Seat)?$\r$\n$\r$\nSaved seat passwords are always deleted."
LangString PsAskRemoveData 1055 "Koltuk ayarları, günlükler ve tanılama raporları (%ProgramData%\Pulsar Seat) da silinsin mi?$\r$\n$\r$\nKayıtlı koltuk parolaları her durumda silinir."
LangString PsDetails 1033 "Details:"
LangString PsDetails 1055 "Ayrıntılar:"

; App API helper DLL for every app architecture this PC runs (same layout as the MSI:
; apihelper\<x86|x64[|arm64|arm64ec]>\pulsar-seat-api.dll, crates/seat-compat/apihelper).
!macro PS_INSTALL_APIHELPER
  SetOutPath "$INSTDIR\apihelper\x86"
  File "$%PULSAR_SEAT_BIN%\apihelper\x86\pulsar-seat-api.dll"
  SetOutPath "$INSTDIR\apihelper\x64"
  File "$%PULSAR_SEAT_BIN%\apihelper\x64\pulsar-seat-api.dll"
  !if "${ARCH}" == "arm64"
    SetOutPath "$INSTDIR\apihelper\arm64"
    File "$%PULSAR_SEAT_BIN%\apihelper\arm64\pulsar-seat-api.dll"
    SetOutPath "$INSTDIR\apihelper\arm64ec"
    File "$%PULSAR_SEAT_BIN%\apihelper\arm64ec\pulsar-seat-api.dll"
  !endif
  SetOutPath "$INSTDIR"
!macroend

!macro PS_REMOVE_APIHELPER_ARCH A
  Delete "$INSTDIR\apihelper\${A}\pulsar-seat-api.dll"
  RMDir "$INSTDIR\apihelper\${A}"
!macroend

!macro PS_REMOVE_APIHELPER
  !insertmacro PS_REMOVE_APIHELPER_ARCH "x86"
  !insertmacro PS_REMOVE_APIHELPER_ARCH "x64"
  !insertmacro PS_REMOVE_APIHELPER_ARCH "arm64"
  !insertmacro PS_REMOVE_APIHELPER_ARCH "arm64ec"
  RMDir "$INSTDIR\apihelper"
!macroend

Var PsRestore
Var PsTweaks

!macro PS_RUN_SETUP ARGS
  nsExec::ExecToLog '"${PS_SERVICE_EXE}" ${ARGS}'
  Pop $0
  ${If} $0 == "1"
    DetailPrint "$(PsStepFailed) (${ARGS}). $(PsDetails) ${PS_SETUP_LOG}"
  ${ElseIf} $0 == "2"
    DetailPrint "$(PsStepPartial) (${ARGS}). $(PsDetails) ${PS_SETUP_LOG}"
  ${ElseIf} $0 == "4"
    MessageBox MB_ICONSTOP|MB_OK "$(PsChecksBlockedLog)" /SD IDOK
  ${ElseIf} $0 == "3010"
    DetailPrint "$(PsRebootNeeded)"
    SetRebootFlag true
  ${EndIf}
!macroend

; Stop the service (it stops every seat first) and wait for STOPPED, at most 60 s.
!macro PS_STOP_SERVICE
  nsExec::ExecToLog '"$SYSDIR\sc.exe" stop ${PS_SERVICE}'
  Pop $0
  StrCpy $3 0
  ps_wait_stop_${__LINE__}:
    nsExec::ExecToStack '"$SYSDIR\sc.exe" query ${PS_SERVICE}'
    Pop $0
    Pop $1
    ${If} $0 == "1060"
      Goto ps_stopped_${__LINE__}
    ${EndIf}
    ${StrLoc} $2 $1 "STOPPED" ">"
    ${If} $2 != ""
      Goto ps_stopped_${__LINE__}
    ${EndIf}
    IntOp $3 $3 + 1
    ${If} $3 >= 60
      MessageBox MB_ICONSTOP|MB_OK "$(PsServiceStuck)" /SD IDOK
      Abort
    ${EndIf}
    Sleep 1000
    Goto ps_wait_stop_${__LINE__}
  ps_stopped_${__LINE__}:
!macroend

; Push a tweak list; returns "1" if it holds only a-z, "_" and ",", else "0".
; (NSIS comparisons ignore case, so upper case passes here; the service's parser
; refuses it.) Spaces, quotes, dashes, slashes and backslashes never pass.
Function PsValidTweaks
  Exch $R5
  Push $R6
  Push $R7
  Push $R8
  StrCpy $R8 "1"
  StrCpy $R6 0
  ps_vt_loop:
    StrCpy $R7 $R5 1 $R6
    StrCmp $R7 "" ps_vt_done
    ${StrLoc} $R9 "abcdefghijklmnopqrstuvwxyz_," $R7 ">"
    StrCmp $R9 "" 0 ps_vt_next
      StrCpy $R8 "0"
      Goto ps_vt_done
    ps_vt_next:
    IntOp $R6 $R6 + 1
    Goto ps_vt_loop
  ps_vt_done:
  StrCpy $R5 $R8
  Pop $R8
  Pop $R7
  Pop $R6
  Exch $R5
FunctionEnd

!macro NSIS_HOOK_PREINSTALL
  ; --- architecture guard (native machine, not the WOW64 variable) ----------------------
  ${If} ${IsNativeARM64}
    !if "${ARCH}" != "arm64"
      MessageBox MB_ICONSTOP|MB_OK "$(PsArchX64Only)" /SD IDOK
      Abort
    !endif
  ${Else}
    !if "${ARCH}" == "arm64"
      MessageBox MB_ICONSTOP|MB_OK "$(PsArchArm64Only)" /SD IDOK
      Abort
    !endif
  ${EndIf}

  SetRegView 64

  ; --- Windows build >= 19045 ------------------------------------------------------------
  ReadRegStr $1 HKLM "SOFTWARE\Microsoft\Windows NT\CurrentVersion" "CurrentBuildNumber"
  ${If} $1 != ""
  ${AndIf} $1 < 19045
    MessageBox MB_ICONSTOP|MB_OK "$(PsOsTooOld)" /SD IDOK
    Abort
  ${EndIf}

  ; --- coexistence: refuse beside the reference product (interop identifiers) ----------
  ClearErrors
  ReadRegDWORD $1 HKLM "SYSTEM\CurrentControlSet\Services\MUTESV_SERVICE" "Type"
  ${IfNot} ${Errors}
    Goto ps_reference_found
  ${EndIf}
  ClearErrors
  ReadRegDWORD $1 HKLM "SYSTEM\CurrentControlSet\Services\MUTENX_SERVICE" "Type"
  ${IfNot} ${Errors}
    Goto ps_reference_found
  ${EndIf}
  Goto ps_reference_done
  ps_reference_found:
    MessageBox MB_ICONSTOP|MB_OK "$(PsCoexistence)" /SD IDOK
    Abort
  ps_reference_done:

  ; --- coexistence: wrapped TermService -> warn (RDS seats are refused at run time) ------
  ; Full expanded path, as seat-compat's is_stock_termservice_dll (a termsrv.dll in
  ; another folder is not stock). ReadRegStr returns REG_EXPAND_SZ unexpanded.
  ReadRegStr $1 HKLM "SYSTEM\CurrentControlSet\Services\TermService\Parameters" "ServiceDll"
  ${If} $1 != ""
    ExpandEnvStrings $2 $1
    ; Strip surrounding quotes, if any.
    StrCpy $4 $2 1
    ${If} $4 == '"'
      StrCpy $2 $2 "" 1
      StrCpy $2 $2 -1
    ${EndIf}
    ; NSIS string comparison is case-insensitive.
    ${If} $2 != "$WINDIR\System32\termsrv.dll"
      MessageBox MB_ICONEXCLAMATION|MB_OK "$(PsTermSrv)" /SD IDOK
    ${EndIf}
  ${EndIf}

  ; --- full pre-flight gate BEFORE any file is laid down ----------------------------------
  ; Our service binary is extracted to the private plugins dir and asked for the gate
  ; only (no system change). Exit 4 = refused; stdout = warning lines (console language).
  InitPluginsDir
  SetOutPath "$PLUGINSDIR"
  File "$%PULSAR_SEAT_BIN%\pulsar-seat-service.exe"
  nsExec::ExecToStack '"$PLUGINSDIR\pulsar-seat-service.exe" preflight'
  Pop $0
  Pop $1
  ${If} $0 == "4"
    MessageBox MB_ICONSTOP|MB_OK "$(PsChecksBlocked)" /SD IDOK
    Abort
  ${ElseIf} $0 != "0"
    MessageBox MB_ICONSTOP|MB_OK "$(PsChecksFailed)" /SD IDOK
    Abort
  ${ElseIf} $1 != ""
    ; Warnings never stop the install (spec 90 §11 item 4: warn).
    MessageBox MB_ICONEXCLAMATION|MB_OKCANCEL "$(PsChecksWarn)" /SD IDOK IDOK ps_warn_ok
    Abort
    ps_warn_ok:
  ${EndIf}

  ; --- upgrade: seats must stop before files are replaced --------------------------------
  ${If} ${FileExists} "${PS_SERVICE_EXE}"
    nsExec::ExecToStack '"$SYSDIR\sc.exe" query ${PS_SERVICE}'
    Pop $0
    Pop $1
    ${StrLoc} $2 $1 "RUNNING" ">"
    ${If} $2 != ""
      MessageBox MB_ICONQUESTION|MB_YESNO "$(PsStopSeats)" /SD IDYES IDYES ps_stop_ok
      Abort
      ps_stop_ok:
    ${EndIf}
    !insertmacro PS_STOP_SERVICE
  ${EndIf}
!macroend

!macro NSIS_HOOK_POSTINSTALL
  SetRegView 64
  ; Our binaries next to the manager (read at compile time from PULSAR_SEAT_BIN; the MSI
  ; gets them from the WiX fragment instead).
  SetOutPath "$INSTDIR"
  File "$%PULSAR_SEAT_BIN%\pulsar-seat-service.exe"
  File "$%PULSAR_SEAT_BIN%\pulsar-seat-host.exe"
  File "$%PULSAR_SEAT_BIN%\pulsar-seat-agent.exe"
  File "$%PULSAR_SEAT_BIN%\pulsar-seat-credprov.dll"
  !insertmacro PS_INSTALL_APIHELPER
  CreateDirectory "$INSTDIR\drivers"
  ${GetParameters} $R0

  ; --- optional choices ------------------------------------------------------------------
  StrCpy $PsRestore "0"
  ClearErrors
  ${GetOptions} $R0 "/RESTOREPOINT" $R1
  ${IfNot} ${Errors}
    StrCpy $PsRestore "1"
  ${Else}
    MessageBox MB_ICONQUESTION|MB_YESNO "$(PsAskRestorePoint)" /SD IDNO IDNO ps_no_restore
    StrCpy $PsRestore "1"
    ps_no_restore:
  ${EndIf}

  ClearErrors
  ${GetOptions} $R0 "/TWEAKS=" $PsTweaks
  ${IfNot} ${Errors}
    Push $PsTweaks
    Call PsValidTweaks
    Pop $1
    ${If} $1 != "1"
      MessageBox MB_ICONEXCLAMATION|MB_OK "$(PsBadTweaks)" /SD IDOK
      StrCpy $PsTweaks ""
    ${EndIf}
  ${Else}
    StrCpy $PsTweaks ""
    ; Fast startup: offered only when it is on (HiberbootEnabled absent or 1).
    ClearErrors
    ReadRegDWORD $1 HKLM "SYSTEM\CurrentControlSet\Control\Session Manager\Power" "HiberbootEnabled"
    ${If} ${Errors}
    ${OrIf} $1 != "0"
      MessageBox MB_ICONQUESTION|MB_YESNO "$(PsAskFastStartup)" /SD IDNO IDNO ps_no_fast
      StrCpy $PsTweaks "$PsTweaksfast_startup_off,"
      ps_no_fast:
    ${EndIf}
    MessageBox MB_ICONQUESTION|MB_YESNO "$(PsAskSleep)" /SD IDNO IDNO ps_no_sleep
    StrCpy $PsTweaks "$PsTweakssleep_off,"
    ps_no_sleep:
    MessageBox MB_ICONQUESTION|MB_YESNO "$(PsAskDumps)" /SD IDNO IDNO ps_no_dump
    StrCpy $PsTweaks "$PsTweakssmall_crash_dumps,"
    ps_no_dump:
  ${EndIf}

  ; --- service record + compat steps (gate again, restore point, tweaks, drivers) -------
  ; Every variable part is quoted; $PsTweaks is validated above, $PsRestore is 0 / 1.
  !insertmacro PS_RUN_SETUP 'install --register-service --restore-point=$PsRestore "--tweak=$PsTweaks" "--drivers=$INSTDIR\drivers"'
  ${If} $0 == "4"
    ; Refused (the PC changed since PREINSTALL): undo what this run laid down, so no
    ; half-installed product stays behind (service record, binaries, uninstall entry).
    nsExec::ExecToLog '"${PS_SERVICE_EXE}" uninstall --unregister-service --remove-user-data=0'
    Pop $1
    Delete "$INSTDIR\pulsar-seat-service.exe"
    Delete "$INSTDIR\pulsar-seat-host.exe"
    Delete "$INSTDIR\pulsar-seat-agent.exe"
    !insertmacro PS_REMOVE_APIHELPER
    RMDir "$INSTDIR\drivers"
    ExecWait '"$INSTDIR\uninstall.exe" /S _?=$INSTDIR'
    Delete "$INSTDIR\uninstall.exe"
    RMDir "$INSTDIR"
    Abort
  ${EndIf}

  ; Warnings from the install step (also shown by the manager on its first start).
  ${If} ${FileExists} "${PS_SETUP_LOG}"
    DetailPrint "$(PsSetupLog)"
  ${EndIf}

  ; --- manager autostart in every interactive session ---------------------------------
  WriteRegStr HKLM "${PS_RUN_KEY}" "PulsarSeatManager" '"$INSTDIR\pulsar-seat-manager.exe" --tray'
  WriteRegStr HKLM "SOFTWARE\Pulsar Seat" "InstallPath" "$INSTDIR"
  WriteRegStr HKLM "SOFTWARE\Pulsar Seat" "Version" "${VERSION}"

  nsExec::ExecToLog '"$SYSDIR\sc.exe" start ${PS_SERVICE}'
  Pop $0
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  SetRegView 64
  ${GetParameters} $R0
  ; HYPOTHESIS: the Tauri NSIS updater runs the old uninstaller with /UPDATE; keep all
  ; state then (the new installer's `install` takes over).
  ClearErrors
  ${GetOptions} $R0 "/UPDATE" $R1
  ${IfNot} ${Errors}
    !insertmacro PS_STOP_SERVICE
    Goto ps_preuninstall_done
  ${EndIf}

  StrCpy $R3 "0"
  ClearErrors
  ${GetOptions} $R0 "/REMOVEUSERDATA" $R1
  ${IfNot} ${Errors}
    StrCpy $R3 "1"
  ${Else}
    MessageBox MB_ICONQUESTION|MB_YESNO|MB_DEFBUTTON2 "$(PsAskRemoveData)" /SD IDNO IDNO ps_keep_data
    StrCpy $R3 "1"
    ps_keep_data:
  ${EndIf}

  ; Stops every seat, reverts every journaled change (tweaks, per-user palm checks,
  ; addresses, driver packages we published), removes the LSA secrets, the Event Log
  ; source and the service record.
  !insertmacro PS_RUN_SETUP 'uninstall --unregister-service --remove-user-data=$R3'
  DeleteRegValue HKLM "${PS_RUN_KEY}" "PulsarSeatManager"
  DeleteRegKey HKLM "SOFTWARE\Pulsar Seat"
  ps_preuninstall_done:
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  ; Files the Tauri uninstaller does not know about (ours, added in POSTINSTALL).
  ; On /UPDATE the new installer overwrites them anyway.
  Delete "$INSTDIR\pulsar-seat-service.exe"
  Delete "$INSTDIR\pulsar-seat-host.exe"
  Delete "$INSTDIR\pulsar-seat-agent.exe"
  !insertmacro PS_REMOVE_APIHELPER
  RMDir "$INSTDIR\drivers"
  RMDir "$INSTDIR"
!macroend
