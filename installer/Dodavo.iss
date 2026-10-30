; Inno Setup script for the Dodavo installer.
;
; Build it with installer\build.ps1, which builds the release program, passes
; the version from Cargo.toml and signs everything when a certificate is set.
; Compiling this file directly with ISCC works too, once
; target\release\Dodavo.exe exists; the version is then read from the program.
;
; Needs Inno Setup 6.6 or later (for "WizardStyle=modern dynamic").

#if Ver < EncodeVer(6, 6, 0)
  #error Inno Setup 6.6 or later is required
#endif

#define AppName "Dodavo"
#define AppExeName "Dodavo.exe"
; build.ps1 passes a staged (and possibly signed) copy of the program.
#ifndef AppExe
  #define AppExe SourcePath + "..\target\release\" + AppExeName
#endif

#ifndef AppVersion
  #define AppVersion GetStringFileInfo(AppExe, "ProductVersion")
#endif
; Windows version resources only take numbers (no "-beta" suffix).
#ifndef AppVersionNumeric
  #define AppVersionNumeric GetVersionNumbersString(AppExe)
#endif

[Setup]
; Never change AppId: Windows uses it to recognize upgrades and the uninstaller.
AppId={{34D1793E-8FBB-46FC-BC27-09F31F071943}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher=Hrvoje Katić
VersionInfoVersion={#AppVersionNumeric}
VersionInfoProductVersion={#AppVersionNumeric}
VersionInfoProductTextVersion={#AppVersion}
VersionInfoDescription={#AppName} Setup

; Per-user install without UAC by default (%LOCALAPPDATA%\Programs\Dodavo).
; The wizard offers "install for all users" too, which asks for elevation.
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
DefaultDirName={autopf}\{#AppName}
DisableProgramGroupPage=yes
; The uninstall step below touches HKCU, also in an all-users install.
UsedUserAreasWarning=no

ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0

; Every running instance (of any profile) holds this mutex, so Setup and the
; uninstaller ask the user to close Dodavo first. See src/main.rs.
AppMutex=Local\DodavoRunning

; Follows the Windows light/dark app mode, like the program itself.
WizardStyle=modern dynamic
SetupIconFile=..\res\app.ico
UninstallDisplayIcon={app}\{#AppExeName}
UninstallDisplayName={#AppName}

OutputDir=..\target\installer
OutputBaseFilename={#AppName}-{#AppVersion}-setup
Compression=lzma2/max
SolidCompression=yes

; Picks the Windows display language; asks only when none of these matches.
ShowLanguageDialog=auto
LanguageDetectionMethod=uilanguage

#ifdef Sign
; The command is defined by build.ps1 (ISCC /Ssigntool=...).
SignTool=signtool
SignedUninstaller=yes
#endif

[Languages]
; Keep in step with src/i18n. Croatian and Serbian share the primary language
; ID; when there is no exact match, Inno takes the first one listed, so any
; Serbian variant gets Serbian (Latin), like the program does.
Name: "english"; MessagesFile: "compiler:Default.isl"
Name: "serbianlatin"; MessagesFile: "languages\SerbianLatin.isl"
Name: "croatian"; MessagesFile: "languages\Croatian.isl"
Name: "slovenian"; MessagesFile: "compiler:Languages\Slovenian.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#AppExe}"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\{#AppName}"; Filename: "{app}\{#AppExeName}"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExeName}"; Tasks: desktopicon

[Registry]
; "Start with Windows" is written by the program itself (src/ui/shell.rs);
; remove it on uninstall so Windows doesn't try to start a missing program.
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueName: "Dodavo"; ValueType: none; Flags: uninsdeletevalue
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueName: "QuickAndEasyFileTransfer"; ValueType: none; Flags: uninsdeletevalue

[Run]
Filename: "{app}\{#AppExeName}"; Description: "{cm:LaunchProgram,{#AppName}}"; Flags: nowait postinstall skipifsilent

; Settings, recipients and the identity key in %APPDATA%\Dodavo are kept on
; uninstall on purpose: reinstalling keeps the same Contact ID.
