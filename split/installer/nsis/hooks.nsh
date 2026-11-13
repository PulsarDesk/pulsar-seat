; SPDX-License-Identifier: GPL-3.0-only
;
; Pulsar Split - Tauri 2 NSIS installer hooks (bundle.windows.nsis.installerHooks).
; Equivalent of ../wix/PulsarSplit.wxs for the optional -setup.exe:
;   PREINSTALL    arch guard, reference-product notice, stop our service (upgrade)
;   POSTINSTALL   `install --register-service` then `reconcile` (service sub-commands)
;   PREUNINSTALL  ask about user data (Turkish), `uninstall --unregister-service`
; Command line: /REMOVEUSERDATA removes user data in a silent uninstall (spec 90 §9.1).
;
; Only our own binary is run, with an argv we build here; no secrets, no certificates,
; no AV exclusions, nothing copied to System32, no firewall rules.
; Not executed or tested by the package (see ../README.md).

!include "x64.nsh"
!include "FileFunc.nsh"
!include "LogicLib.nsh"
!include "StrFunc.nsh"
${StrLoc}

!define PS_SERVICE_EXE "$INSTDIR\pulsar-split-service.exe"
!define PS_REF_ARP "SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\{DB5AA12E-1299-4244-8651-F363406E89F6}_is1"

!macro PS_RUN_SETUP ARGS
  ; nsExec keeps the console hidden; the exit code ends up on the stack.
  nsExec::ExecToLog '"${PS_SERVICE_EXE}" ${ARGS}'
  Pop $0
  ${If} $0 == "1"
    DetailPrint "Pulsar Split kurulum adımı başarısız oldu (${ARGS}). Ayrıntılar: %ProgramData%\PulsarSplit\logs\setup.log"
  ${ElseIf} $0 == "2"
    DetailPrint "Pulsar Split kurulum adımı kısmen tamamlandı (${ARGS})."
  ${ElseIf} $0 == "3010"
    DetailPrint "Değişikliklerin tamamlanması için bilgisayarı yeniden başlatın."
    SetRebootFlag true
  ${EndIf}
!macroend

!macro NSIS_HOOK_PREINSTALL
  ; --- architecture guard: the package must match the native machine -------------------
  ${If} ${IsNativeARM64}
    !if "${ARCH}" != "arm64"
      MessageBox MB_ICONSTOP|MB_OK "Bu kurulum paketi x64 bilgisayarlar içindir. Lütfen ARM64 paketini kullanın." /SD IDOK
      Abort
    !endif
  ${Else}
    !if "${ARCH}" == "arm64"
      MessageBox MB_ICONSTOP|MB_OK "Bu kurulum paketi ARM64 bilgisayarlar içindir. Lütfen x64 paketini kullanın." /SD IDOK
      Abort
    !endif
  ${EndIf}

  ; --- reference multi-seat product: install allowed, service runs passive ------------
  SetRegView 64
  ReadRegStr $1 HKLM "${PS_REF_ARP}" "DisplayVersion"
  ${If} $1 != ""
    MessageBox MB_ICONINFORMATION|MB_OK "Bu bilgisayarda başka bir çoklu oturum ürünü kurulu (sürüm $1).$\r$\n$\r$\nPulsar Split kurulacak, ancak o ürün kurulu olduğu sürece pasif modda çalışır: Uzak Masaüstü, oturum, ekran ve denetleyici ayarlarına dokunmaz ve koltuk başlatmaz.$\r$\n$\r$\nPulsar Split o ürünün dosyalarını, ayarlarını, sertifikasını veya sürücülerini asla değiştirmez." /SD IDOK
  ${EndIf}

  ; --- upgrade: stop our service before files are replaced ------------------------------
  ; Wait until SCM reports STOPPED (or the service is gone, sc exit 1060), at most 60 s,
  ; like WinServices::remove. Overwriting a still-running service exe fails or leaves the
  ; old binary, and install / reconcile would then run old code.
  ${If} ${FileExists} "${PS_SERVICE_EXE}"
    nsExec::ExecToLog '"$SYSDIR\sc.exe" stop PulsarSplit'
    Pop $0
    StrCpy $3 0
    ps_wait_stop:
      nsExec::ExecToStack '"$SYSDIR\sc.exe" query PulsarSplit'
      Pop $0 ; exit code
      Pop $1 ; output (state names are not localised)
      ${If} $0 == "1060"
        Goto ps_stopped
      ${EndIf}
      ${StrLoc} $2 $1 "STOPPED" ">"
      ${If} $2 != ""
        Goto ps_stopped
      ${EndIf}
      IntOp $3 $3 + 1
      ${If} $3 >= 60
        MessageBox MB_ICONSTOP|MB_OK "Pulsar Split hizmeti 60 saniye içinde durmadı. Kurulum iptal edildi.$\r$\n$\r$\nBilgisayarı yeniden başlatıp kurulumu tekrar çalıştırın." /SD IDOK
        Abort
      ${EndIf}
      Sleep 1000
      Goto ps_wait_stop
    ps_stopped:
  ${EndIf}
!macroend

!macro NSIS_HOOK_POSTINSTALL
  ; Driver selection / consent can be passed on the setup command line:
  ;   /DRIVERS=vdd,usbip-win2  /CONSENT=usbip-win2  (/DRIVERS=none = no driver)
  ; Without /DRIVERS (upgrade, repair) `install` / `reconcile` reuse the choice saved at
  ; the first install (HKLM\SOFTWARE\PulsarSplit\Setup\Drivers); the manifest defaults
  ; apply only to a first install.
  ${GetParameters} $R0
  ClearErrors
  ${GetOptions} $R0 "/DRIVERS=" $R1
  ${If} ${Errors}
    StrCpy $R1 ""
  ${EndIf}
  ClearErrors
  ${GetOptions} $R0 "/CONSENT=" $R2
  ${If} ${Errors}
    StrCpy $R2 ""
  ${EndIf}
  !insertmacro PS_RUN_SETUP 'install --register-service --drivers=$R1 --consent=$R2'
  !insertmacro PS_RUN_SETUP 'reconcile --drivers=$R1 --consent=$R2'
  nsExec::ExecToLog '"$SYSDIR\sc.exe" start PulsarSplit'
  Pop $0
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  ; HYPOTHESIS: the Tauri NSIS updater runs the old uninstaller with /UPDATE; keep all
  ; state then (the new installer's reconcile takes over).
  ${GetParameters} $R0
  ClearErrors
  ${GetOptions} $R0 "/UPDATE" $R1
  ${IfNot} ${Errors}
    nsExec::ExecToLog '"$SYSDIR\sc.exe" stop PulsarSplit'
    Pop $0
    Goto ps_preuninstall_done
  ${EndIf}

  StrCpy $R3 ""
  ClearErrors
  ${GetOptions} $R0 "/REMOVEUSERDATA" $R1
  ${IfNot} ${Errors}
    StrCpy $R3 "--remove-user-data"
  ${Else}
    ; /SD IDNO: a silent uninstall keeps user data unless /REMOVEUSERDATA is given.
    MessageBox MB_ICONQUESTION|MB_YESNO|MB_DEFBUTTON2 "Koltuk hesapları, profilleri ve %ProgramData%\PulsarSplit içindeki ayarlar ve anahtarlar da silinsin mi?$\r$\n$\r$\nKayıtlı koltuk parolaları her durumda silinir." /SD IDNO IDNO ps_keep_data
    StrCpy $R3 "--remove-user-data"
    ps_keep_data:
  ${EndIf}

  MessageBox MB_ICONEXCLAMATION|MB_OK "Kaldırma sırasında Uzak Masaüstü ayarları geri yüklenir. Bu bilgisayardaki tüm uzak ve koltuk oturumlarının bağlantısı kesilecek." /SD IDOK
  !insertmacro PS_RUN_SETUP 'uninstall --unregister-service $R3'
  ps_preuninstall_done:
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
!macroend
