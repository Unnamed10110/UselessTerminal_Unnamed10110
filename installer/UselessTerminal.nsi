; Per-user installer (no admin). Built by scripts/package.ps1: makensis /DVERSION=x.y.z /DEXE=<path> /DICON=<path> /DOUT=<path>
Unicode true
!include "MUI2.nsh"

!ifndef VERSION
  !error "pass /DVERSION=x.y.z"
!endif

Name "Useless Terminal"
OutFile "${OUT}"
InstallDir "$LOCALAPPDATA\Programs\Useless Terminal"
InstallDirRegKey HKCU "Software\UselessTerminal\Installer" "InstallDir"
RequestExecutionLevel user
SetCompressor /SOLID lzma

!define MUI_ICON "${ICON}"
!define MUI_UNICON "${ICON}"
!define MUI_FINISHPAGE_RUN "$INSTDIR\UselessTerminal.exe"
!define MUI_FINISHPAGE_RUN_TEXT "Launch Useless Terminal"
!define MUI_FINISHPAGE_SHOWREADME ""
!define MUI_FINISHPAGE_SHOWREADME_TEXT "Create a desktop shortcut"
!define MUI_FINISHPAGE_SHOWREADME_FUNCTION CreateDesktopShortcut
!define MUI_FINISHPAGE_SHOWREADME_NOTCHECKED

!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"

Function CreateDesktopShortcut
  CreateShortcut "$DESKTOP\Useless Terminal.lnk" "$INSTDIR\UselessTerminal.exe"
FunctionEnd

Section "Install"
  SetOutPath "$INSTDIR"
  File "/oname=UselessTerminal.exe" "${EXE}"
  WriteUninstaller "$INSTDIR\Uninstall.exe"
  CreateShortcut "$SMPROGRAMS\Useless Terminal.lnk" "$INSTDIR\UselessTerminal.exe"

  ; Explorer: "Open in Useless Terminal" on folders and folder backgrounds (HKCU = per user)
  WriteRegStr HKCU "Software\Classes\Directory\shell\UselessTerminal" "" "Open in Useless Terminal"
  WriteRegStr HKCU "Software\Classes\Directory\shell\UselessTerminal" "Icon" "$INSTDIR\UselessTerminal.exe"
  WriteRegStr HKCU "Software\Classes\Directory\shell\UselessTerminal\command" "" '"$INSTDIR\UselessTerminal.exe" --cwd "%V"'
  WriteRegStr HKCU "Software\Classes\Directory\Background\shell\UselessTerminal" "" "Open in Useless Terminal"
  WriteRegStr HKCU "Software\Classes\Directory\Background\shell\UselessTerminal" "Icon" "$INSTDIR\UselessTerminal.exe"
  WriteRegStr HKCU "Software\Classes\Directory\Background\shell\UselessTerminal\command" "" '"$INSTDIR\UselessTerminal.exe" --cwd "%V"'

  WriteRegStr HKCU "Software\UselessTerminal\Installer" "InstallDir" "$INSTDIR"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\UselessTerminal" "DisplayName" "Useless Terminal"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\UselessTerminal" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\UselessTerminal" "DisplayIcon" "$INSTDIR\UselessTerminal.exe"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\UselessTerminal" "UninstallString" '"$INSTDIR\Uninstall.exe"'
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\UselessTerminal" "InstallLocation" "$INSTDIR"
  WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\UselessTerminal" "NoModify" 1
SectionEnd

; User data (%APPDATA%\UselessTerminal) is never touched by the uninstaller.
Section "Uninstall"
  Delete "$INSTDIR\UselessTerminal.exe"
  Delete "$INSTDIR\Uninstall.exe"
  RMDir "$INSTDIR"
  Delete "$SMPROGRAMS\Useless Terminal.lnk"
  Delete "$DESKTOP\Useless Terminal.lnk"
  DeleteRegKey HKCU "Software\Classes\Directory\shell\UselessTerminal"
  DeleteRegKey HKCU "Software\Classes\Directory\Background\shell\UselessTerminal"
  DeleteRegKey HKCU "Software\UselessTerminal\Installer"
  DeleteRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\UselessTerminal"
SectionEnd
