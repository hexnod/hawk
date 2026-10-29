; Copyright 2026 Mark West
; SPDX-License-Identifier: Apache-2.0
;
; Hawk NSIS installer hooks (bundle > windows > nsis > installerHooks).
;
; Adds three checkboxes to the finish page: Explorer context menu, PATH,
; and default app for .md. Each starts checked if already in place;
; unchecking removes it. Uninstall removes all three, except on update.
;
; Tauri has no finish-page hook, so this redefines MUI2's MUI_PAGE_FINISH
; (same body as NSIS's) to add SHOW/LEAVE callbacks. Tauri inserts it
; after its !defines, so ${MAINBINARYNAME} etc. can be used in the
; functions defined there; this file is included before those !defines.
;
; Registry writes use SHCTX: HKCU per-user, HKLM per-machine.

!include "LogicLib.nsh"
!include "WinMessages.nsh"

!define HAWK_PROGID "Hawk.md"
!define HAWK_CONTEXT_KEY "Software\Classes\*\shell\Hawk"
!define HAWK_CAPABILITIES_KEY "Software\Hawk\Capabilities"
!define HAWK_ENV_USER_KEY "Environment"
!define HAWK_ENV_MACHINE_KEY "SYSTEM\CurrentControlSet\Control\Session Manager\Environment"
!define HAWK_USERCHOICE_KEY "Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts\.md\UserChoice"

Var HawkCbContextMenu
Var HawkCbPath
Var HawkCbDefaultMd

; ---------------------------------------------------------------------------
; Uninstall: remove whatever the finish page added. Skipped for updates, so
; the user's choices survive the updater's uninstall-then-install.
!macro NSIS_HOOK_POSTUNINSTALL
  ${If} $UpdateMode <> 1
    Call un.HawkContextMenuRemove
    Call un.HawkPathRemove
    Call un.HawkMdUnassociate
  ${EndIf}
!macroend

; Sets the Path value in ROOT\KEY to $R0 (deleted if $R0 is empty).
!macro _HAWK_PATH_SET ROOT KEY
  ${If} $R0 == ""
    DeleteRegValue ${ROOT} "${KEY}" "Path"
  ${Else}
    WriteRegExpandStr ${ROOT} "${KEY}" "Path" $R0
  ${EndIf}
!macroend

; ---------------------------------------------------------------------------
; Helpers, emitted twice (installer and "un." uninstaller copies) from
; inside MUI_PAGE_FINISH below. PREFIX is "" or "un.".

!macro HAWK_DEFINE_HELPERS PREFIX

  ; Quoted exe path, and the command line used by every verb.
  !define /ifndef HAWK_EXE "$INSTDIR\${MAINBINARYNAME}.exe"
  !define /ifndef HAWK_COMMAND "$\"$INSTDIR\${MAINBINARYNAME}.exe$\" $\"%1$\""
  !define /ifndef HAWK_ICON "$\"$INSTDIR\${MAINBINARYNAME}.exe$\",0"

  ; ---- Context menu ----
  Function ${PREFIX}HawkContextMenuRemove
    ; Only remove the entry if it's ours (points at this install).
    ReadRegStr $0 SHCTX "${HAWK_CONTEXT_KEY}\command" ""
    ${If} $0 == "${HAWK_COMMAND}"
      DeleteRegKey SHCTX "${HAWK_CONTEXT_KEY}"
    ${EndIf}
  FunctionEnd

  ; ---- PATH ----
  ; Reads the PATH this install mode uses into $R0.
  ; $R1 = "ok", "missing" (no Path value yet) or "unreadable" (a Path value
  ; exists but is longer than NSIS can hold, or isn't a string). Callers
  ; must not write PATH when "unreadable", or it would be truncated.
  Function ${PREFIX}HawkPathRead
    StrCpy $R1 "ok"
    ClearErrors
    !if "${INSTALLMODE}" == "perMachine"
      ReadRegStr $R0 HKLM "${HAWK_ENV_MACHINE_KEY}" "Path"
    !else if "${INSTALLMODE}" == "both"
      ${If} $MultiUser.InstallMode == "AllUsers"
        ReadRegStr $R0 HKLM "${HAWK_ENV_MACHINE_KEY}" "Path"
      ${Else}
        ReadRegStr $R0 HKCU "${HAWK_ENV_USER_KEY}" "Path"
      ${EndIf}
    !else
      ReadRegStr $R0 HKCU "${HAWK_ENV_USER_KEY}" "Path"
    !endif
    ${IfNot} ${Errors}
      Return
    ${EndIf}
    ; Read failed: tell "doesn't exist" apart from "too long to read".
    StrCpy $R0 ""
    StrCpy $R1 "missing"
    StrCpy $R2 0
    ${Do}
      !if "${INSTALLMODE}" == "perMachine"
        EnumRegValue $R3 HKLM "${HAWK_ENV_MACHINE_KEY}" $R2
      !else if "${INSTALLMODE}" == "both"
        ${If} $MultiUser.InstallMode == "AllUsers"
          EnumRegValue $R3 HKLM "${HAWK_ENV_MACHINE_KEY}" $R2
        ${Else}
          EnumRegValue $R3 HKCU "${HAWK_ENV_USER_KEY}" $R2
        ${EndIf}
      !else
        EnumRegValue $R3 HKCU "${HAWK_ENV_USER_KEY}" $R2
      !endif
      ${If} $R3 == ""
        ${Break}
      ${EndIf}
      ${If} $R3 == "Path" ; StrCmp is case-insensitive
        StrCpy $R1 "unreadable"
        ${Break}
      ${EndIf}
      IntOp $R2 $R2 + 1
    ${Loop}
  FunctionEnd

  ; Writes $R0 as the PATH (REG_EXPAND_SZ, as Windows uses; an empty
  ; result deletes the value) and tells running programs (Explorer) so new
  ; consoles see it without a sign-out.
  Function ${PREFIX}HawkPathWrite
    !if "${INSTALLMODE}" == "perMachine"
      !insertmacro _HAWK_PATH_SET HKLM "${HAWK_ENV_MACHINE_KEY}"
    !else if "${INSTALLMODE}" == "both"
      ${If} $MultiUser.InstallMode == "AllUsers"
        !insertmacro _HAWK_PATH_SET HKLM "${HAWK_ENV_MACHINE_KEY}"
      ${Else}
        !insertmacro _HAWK_PATH_SET HKCU "${HAWK_ENV_USER_KEY}"
      ${EndIf}
    !else
      !insertmacro _HAWK_PATH_SET HKCU "${HAWK_ENV_USER_KEY}"
    !endif
    SendMessage ${HWND_BROADCAST} ${WM_SETTINGCHANGE} 0 "STR:Environment" /TIMEOUT=5000
  FunctionEnd

  ; Splits $R0 on ";" and drops every entry equal to $INSTDIR (with or
  ; without a trailing "\", case-insensitive). Other entries, including
  ; empty ones, are kept as-is and in order.
  ; Out: $R2 = PATH without Hawk's entries, $R3 = 1 if any were found.
  Function ${PREFIX}HawkPathScan
    StrCpy $R2 ""
    StrCpy $R3 0
    StrCpy $R4 $R0   ; not yet scanned
    StrCpy $R8 1     ; next kept entry is the first one
    ${Do}
      ; $R6 = next entry, $R4 = what follows its ";"
      StrLen $R7 $R4
      StrCpy $R5 0
      StrCpy $R9 0   ; 1 = a ";" ended this entry
      ${Do}
        ${If} $R5 >= $R7
          ${Break}
        ${EndIf}
        StrCpy $R6 $R4 1 $R5
        ${If} $R6 == ";"
          StrCpy $R9 1
          ${Break}
        ${EndIf}
        IntOp $R5 $R5 + 1
      ${Loop}
      StrCpy $R6 $R4 $R5
      IntOp $R5 $R5 + 1
      StrCpy $R4 $R4 "" $R5

      ${If} $R6 == "$INSTDIR"
      ${OrIf} $R6 == "$INSTDIR\"
        StrCpy $R3 1
      ${ElseIf} $R8 = 1
        StrCpy $R2 $R6
        StrCpy $R8 0
      ${Else}
        StrCpy $R2 "$R2;$R6"
      ${EndIf}

      ${If} $R9 = 0
        ${Break}
      ${EndIf}
    ${Loop}
  FunctionEnd

  Function ${PREFIX}HawkPathRemove
    Call ${PREFIX}HawkPathRead
    ${If} $R1 != "ok"
      Return
    ${EndIf}
    Call ${PREFIX}HawkPathScan
    ${If} $R3 = 1
      StrCpy $R0 $R2
      Call ${PREFIX}HawkPathWrite
    ${EndIf}
  FunctionEnd

  ; ---- .md default app ----
  Function ${PREFIX}HawkMdUnassociate
    ; Hand .md back to whatever it pointed at before, if it's still ours.
    ReadRegStr $0 SHCTX "Software\Classes\.md" ""
    ${If} $0 == "${HAWK_PROGID}"
      ReadRegStr $1 SHCTX "Software\Classes\.md" "Hawk_backup"
      ${If} $1 == ""
        DeleteRegValue SHCTX "Software\Classes\.md" ""
      ${Else}
        WriteRegStr SHCTX "Software\Classes\.md" "" $1
      ${EndIf}
    ${EndIf}
    DeleteRegValue SHCTX "Software\Classes\.md" "Hawk_backup"
    DeleteRegValue SHCTX "Software\Classes\.md\OpenWithProgids" "${HAWK_PROGID}"
    DeleteRegKey /ifempty SHCTX "Software\Classes\.md\OpenWithProgids"
    DeleteRegKey /ifempty SHCTX "Software\Classes\.md"
    DeleteRegKey SHCTX "Software\Classes\${HAWK_PROGID}"
    DeleteRegValue SHCTX "Software\RegisteredApplications" "Hawk"
    DeleteRegKey SHCTX "${HAWK_CAPABILITIES_KEY}"
    DeleteRegKey /ifempty SHCTX "Software\Hawk"
    System::Call 'shell32::SHChangeNotify(i 0x08000000, i 0x1000, p 0, p 0)' ; SHCNE_ASSOCCHANGED, SHCNF_FLUSH
  FunctionEnd

!macroend

; ---------------------------------------------------------------------------
; Installer-only functions: the finish page callbacks and the "add" side.

; A finish-page checkbox styled like MUI's own ones (same colors, same
; high-contrast fix).
!macro HAWK_FINISHPAGE_CHECKBOX OUTVAR TOP TEXT
  ${NSD_CreateCheckbox} 120u ${TOP} 195u 10u "${TEXT}"
  Pop ${OUTVAR}
  SetCtlColors ${OUTVAR} "${MUI_TEXTCOLOR}" "${MUI_BGCOLOR}"
  !ifdef IsHighContrastModeActive
    ${If} ${IsHighContrastModeActive}
      System::Call 'UXTHEME::SetWindowTheme(p${OUTVAR},w" ",w" ")'
    ${EndIf}
  !endif
!macroend

!macro HAWK_DEFINE_INSTALLER_FUNCTIONS

  Function HawkContextMenuAdd
    WriteRegStr SHCTX "${HAWK_CONTEXT_KEY}" "" "Edit with Hawk"
    WriteRegStr SHCTX "${HAWK_CONTEXT_KEY}" "Icon" "${HAWK_ICON}"
    WriteRegStr SHCTX "${HAWK_CONTEXT_KEY}\command" "" "${HAWK_COMMAND}"
  FunctionEnd

  Function HawkPathAdd
    Call HawkPathRead
    ${If} $R1 == "unreadable"
      MessageBox MB_OK|MB_ICONEXCLAMATION "Hawk was not added to the PATH: the existing PATH is too long for the installer to update safely.$\r$\n$\r$\nYou can add this folder to the PATH yourself:$\r$\n$INSTDIR"
      Return
    ${EndIf}
    Call HawkPathScan
    ${If} $R3 = 1
      Return ; already there
    ${EndIf}
    ; Make sure the result still fits in an NSIS string before writing it.
    StrLen $R4 $R0
    StrLen $R5 "$INSTDIR"
    IntOp $R4 $R4 + $R5
    IntOp $R4 $R4 + 2
    ${If} $R4 >= ${NSIS_MAX_STRLEN}
      MessageBox MB_OK|MB_ICONEXCLAMATION "Hawk was not added to the PATH: the result would be longer than the installer can write safely.$\r$\n$\r$\nYou can add this folder to the PATH yourself:$\r$\n$INSTDIR"
      Return
    ${EndIf}
    ${If} $R0 == ""
      StrCpy $R0 "$INSTDIR"
    ${Else}
      StrCpy $R5 $R0 1 -1
      ${If} $R5 == ";"
        StrCpy $R0 "$R0$INSTDIR"
      ${Else}
        StrCpy $R0 "$R0;$INSTDIR"
      ${EndIf}
    ${EndIf}
    Call HawkPathWrite
  FunctionEnd

  ; Registers Hawk as the .md handler: a "Hawk.md" ProgID, .md's default
  ; class, its Open With list, and a RegisteredApplications entry so Hawk is
  ; listed for .md in Settings > Default apps.
  ;
  ; Windows doesn't let installers override a default the user picked
  ; themselves (the per-user "UserChoice", which is hash-protected). If one
  ; points elsewhere, offer to open Default apps so the user can switch.
  Function HawkMdAssociate
    ReadRegStr $0 SHCTX "Software\Classes\.md" ""
    ${If} $0 != "${HAWK_PROGID}"
      WriteRegStr SHCTX "Software\Classes\.md" "Hawk_backup" $0
    ${EndIf}
    WriteRegStr SHCTX "Software\Classes\.md" "" "${HAWK_PROGID}"
    WriteRegStr SHCTX "Software\Classes\.md\OpenWithProgids" "${HAWK_PROGID}" ""
    WriteRegStr SHCTX "Software\Classes\${HAWK_PROGID}" "" "Markdown Document"
    WriteRegStr SHCTX "Software\Classes\${HAWK_PROGID}\DefaultIcon" "" "${HAWK_ICON}"
    WriteRegStr SHCTX "Software\Classes\${HAWK_PROGID}\shell\open\command" "" "${HAWK_COMMAND}"
    WriteRegStr SHCTX "${HAWK_CAPABILITIES_KEY}" "ApplicationName" "Hawk"
    WriteRegStr SHCTX "${HAWK_CAPABILITIES_KEY}" "ApplicationDescription" "A simple notepad-style text editor."
    WriteRegStr SHCTX "${HAWK_CAPABILITIES_KEY}\FileAssociations" ".md" "${HAWK_PROGID}"
    WriteRegStr SHCTX "Software\RegisteredApplications" "Hawk" "${HAWK_CAPABILITIES_KEY}"
    System::Call 'shell32::SHChangeNotify(i 0x08000000, i 0x1000, p 0, p 0)' ; SHCNE_ASSOCCHANGED, SHCNF_FLUSH

    ReadRegStr $1 HKCU "${HAWK_USERCHOICE_KEY}" "ProgId"
    ${If} $1 != ""
    ${AndIf} $1 != "${HAWK_PROGID}"
      MessageBox MB_YESNO|MB_ICONINFORMATION "Hawk is registered for .md files, but Windows keeps the default app you chose earlier until you change it yourself.$\r$\n$\r$\nOpen Default apps settings now?" IDNO hawk_md_done
      !if "${INSTALLMODE}" == "perMachine"
        ExecShell "open" "ms-settings:defaultapps?registeredAppMachine=Hawk"
      !else
        ExecShell "open" "ms-settings:defaultapps?registeredAppUser=Hawk"
      !endif
    ${EndIf}
    hawk_md_done:
  FunctionEnd

  ; Below MUI's own "Run" (90u) and "desktop shortcut" (110u) checkboxes.
  Function HawkFinishPageShow
    !insertmacro HAWK_FINISHPAGE_CHECKBOX $HawkCbContextMenu 130u "Add $\"Edit with Hawk$\" to the Explorer context menu"
    ReadRegStr $0 SHCTX "${HAWK_CONTEXT_KEY}\command" ""
    ${If} $0 == "${HAWK_COMMAND}"
      ${NSD_Check} $HawkCbContextMenu
    ${EndIf}

    !insertmacro HAWK_FINISHPAGE_CHECKBOX $HawkCbPath 150u "Add Hawk to the PATH environment variable"
    Call HawkPathRead
    ${If} $R1 == "ok"
      Call HawkPathScan
      ${If} $R3 = 1
        ${NSD_Check} $HawkCbPath
      ${EndIf}
    ${EndIf}

    !insertmacro HAWK_FINISHPAGE_CHECKBOX $HawkCbDefaultMd 170u "Make Hawk the default app for .md files"
    ReadRegStr $0 SHCTX "Software\Classes\.md" ""
    ${If} $0 == "${HAWK_PROGID}"
      ${NSD_Check} $HawkCbDefaultMd
    ${EndIf}
  FunctionEnd

  ; Runs when Finish is clicked, before MUI's own Run/shortcut handling.
  Function HawkFinishPageLeave
    ${NSD_GetState} $HawkCbContextMenu $0
    ${If} $0 = ${BST_CHECKED}
      Call HawkContextMenuAdd
    ${Else}
      Call HawkContextMenuRemove
    ${EndIf}

    ${NSD_GetState} $HawkCbPath $0
    ${If} $0 = ${BST_CHECKED}
      Call HawkPathAdd
    ${Else}
      Call HawkPathRemove
    ${EndIf}

    ${NSD_GetState} $HawkCbDefaultMd $0
    ${If} $0 = ${BST_CHECKED}
      Call HawkMdAssociate
    ${Else}
      ReadRegStr $0 SHCTX "Software\Classes\.md" ""
      ${If} $0 == "${HAWK_PROGID}"
        Call HawkMdUnassociate
      ${EndIf}
    ${EndIf}
  FunctionEnd

!macroend

; ---------------------------------------------------------------------------
; MUI_PAGE_FINISH override (see top of file).
!macroundef MUI_PAGE_FINISH
!macro MUI_PAGE_FINISH
  !insertmacro HAWK_DEFINE_HELPERS ""
  !insertmacro HAWK_DEFINE_HELPERS "un."
  !insertmacro HAWK_DEFINE_INSTALLER_FUNCTIONS

  !ifdef MUI_PAGE_CUSTOMFUNCTION_SHOW | MUI_PAGE_CUSTOMFUNCTION_LEAVE
    !error "hooks.nsh: Tauri's template now sets its own finish-page SHOW/LEAVE callback; update the MUI_PAGE_FINISH override."
  !endif
  !define MUI_PAGE_CUSTOMFUNCTION_SHOW HawkFinishPageShow
  !define MUI_PAGE_CUSTOMFUNCTION_LEAVE HawkFinishPageLeave

  !verbose push
  !verbose ${MUI_VERBOSE}
  !insertmacro MUI_PAGE_INIT
  !insertmacro MUI_PAGEDECLARATION_FINISH
  !verbose pop
!macroend
