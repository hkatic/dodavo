; Inno Setup script for the Dodavo installer.
;
; Build it with installer\build.ps1, which builds the program for every
; supported Windows version and architecture, passes the version from
; Cargo.toml and signs everything when a certificate is set. One installer
; carries up to four builds of Dodavo.exe and installs the one that fits
; (see [Files]):
;
;   AppExe         x64, Windows 10 and later (stable Rust)
;   AppExeX86      x86, Windows 10 and later (stable Rust)
;   AppExeX64Win7  x64, Windows 7 SP1 to 8.1 (Tier 3 win7 target)
;   AppExeX86Win7  x86, Windows 7 SP1 to 8.1 (Tier 3 win7 target)
;
; Compiling this file directly with ISCC works too, once
; target\release\Dodavo.exe exists; that gives an installer for 64-bit
; Windows 10 and later only, and the version is read from the program.
;
; Needs Inno Setup 6.6 or later (for "WizardStyle=modern dynamic"), and the
; 32-bit edition of Inno Setup, whose Setup program also runs on 32-bit Windows.

#if Ver < EncodeVer(6, 6, 0)
  #error Inno Setup 6.6 or later is required
#endif

#define AppName "Dodavo"
#define AppExeName "Dodavo.exe"
; build.ps1 passes a staged (and possibly signed) copy of the program.
#ifndef AppExe
  #define AppExe SourcePath + "..\target\release\" + AppExeName
#endif

#ifdef AppExeX64Win7
  #ifndef AppExeX86Win7
    #error Give both Windows 7 builds (AppExeX64Win7 and AppExeX86Win7) or neither
  #endif
  #ifndef AppExeX86
    #error The Windows 7 builds need the 32-bit Windows 10 build (AppExeX86) too
  #endif
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

; With a 32-bit build, Setup runs on every architecture (ArchitecturesAllowed
; is left at its default, like in Inno Setup's own 64BitTwoArch example) and
; installs in 64-bit mode on x64 and on Windows 11 on ARM, in 32-bit mode
; elsewhere.
#ifndef AppExeX86
ArchitecturesAllowed=x64compatible
#endif
ArchitecturesInstallIn64BitMode=x64compatible
; Windows 7 SP1 when the win7 builds are included, else Windows 10.
#ifdef AppExeX64Win7
MinVersion=6.1sp1
#else
MinVersion=10.0
#endif

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
; Exactly one of these is installed as Dodavo.exe, chosen by the Windows
; version (MinVersion / OnlyBelowVersion: 10.0 means Windows 10 and later)
; and by the install mode (64-bit or 32-bit, see above). 'solidbreak' lets
; Setup skip the builds it doesn't need without decompressing them.
Source: "{#AppExe}"; DestDir: "{app}"; DestName: "{#AppExeName}"; Check: Is64BitInstallMode; MinVersion: 10.0; Flags: ignoreversion
#ifdef AppExeX86
Source: "{#AppExeX86}"; DestDir: "{app}"; DestName: "{#AppExeName}"; Check: not Is64BitInstallMode; MinVersion: 10.0; Flags: ignoreversion solidbreak
#endif
#ifdef AppExeX64Win7
Source: "{#AppExeX64Win7}"; DestDir: "{app}"; DestName: "{#AppExeName}"; Check: Is64BitInstallMode; OnlyBelowVersion: 10.0; Flags: ignoreversion solidbreak
Source: "{#AppExeX86Win7}"; DestDir: "{app}"; DestName: "{#AppExeName}"; Check: not Is64BitInstallMode; OnlyBelowVersion: 10.0; Flags: ignoreversion solidbreak
#endif

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
