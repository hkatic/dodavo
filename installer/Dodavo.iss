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
; Windows 10 and later only, the version is read from the program, and the
; license texts keep the Unix line ends of the repository.
;
; Needs Inno Setup 6.6 or later (for "WizardStyle=modern dynamic"), and the
; 32-bit edition of Inno Setup, whose Setup program also runs on 32-bit Windows.

#if Ver < EncodeVer(6, 6, 0)
  #error Inno Setup 6.6 or later is required
#endif

#define AppName "Dodavo"
#define AppExeName "Dodavo.exe"
; Held by every running instance (see src/main.rs); Setup and the uninstaller
; use it to tell that Dodavo is running.
#define AppMutexName "Local\DodavoRunning"
; build.ps1 passes a staged (and possibly signed) copy of the program.
#ifndef AppExe
  #define AppExe SourcePath + "..\target\release\" + AppExeName
#endif
; build.ps1 passes copies with Windows line ends, which Notepad on Windows 7
; to Windows 10 1803 needs; the files in the repository have Unix ones.
#ifndef LicenseFile
  #define LicenseFile SourcePath + "..\LICENSE"
#endif
#ifndef ThirdPartyLicensesFile
  #define ThirdPartyLicensesFile SourcePath + "..\res\third-party-licenses.txt"
#endif

; The author, the copyright and the website come from Cargo.toml, through the
; version resource of the program (build.rs, res/app.rc).
#define AppPublisher GetStringFileInfo(AppExe, "CompanyName")
#define AppCopyright GetStringFileInfo(AppExe, "LegalCopyright")
#define AppURL GetStringFileInfo(AppExe, "Website")
#if AppPublisher == "" || AppCopyright == "" || AppURL == ""
  #error The version resource of the program lacks CompanyName, LegalCopyright or Website
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
AppPublisher={#AppPublisher}
AppPublisherURL={#AppURL}
AppSupportURL={#AppURL}/issues
AppUpdatesURL={#AppURL}/releases
; Also the copyright of Setup's own version resource.
AppCopyright={#AppCopyright}
VersionInfoVersion={#AppVersionNumeric}
VersionInfoProductVersion={#AppVersionNumeric}
VersionInfoProductTextVersion={#AppVersion}
VersionInfoDescription={#AppName} Setup

; Per-user install without UAC by default (%LOCALAPPDATA%\Programs\Dodavo).
; The wizard offers "install for all users" too, which asks for elevation.
; /CURRENTUSER and /ALLUSERS choose the mode on the command line; an
; automatic update passes the mode of the installation it updates. Without
; them, an existing installation decides (UsePreviousPrivileges).
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog commandline
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


; Follows the Windows light/dark app mode, like the program itself.
WizardStyle=modern dynamic
; No Welcome page (Inno Setup's default) and no license page, on purpose:
; the GNU GPL does not need to be accepted to install or run the program
; (section 9), Dodavo shows it in Help > License and About, and Setup
; installs it next to the program (see [Files]). Every page is one more
; stop for keyboard and screen reader users, and automatic updates run
; silently, without pages, anyway.
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

[CustomMessages]
; Shown when Dodavo did not close in time for Setup or the uninstaller.
english.StillRunning=Dodavo is still running and could not be closed. Exit Dodavo, then try again.
croatian.StillRunning=Dodavo je još uvijek pokrenut i nije ga moguće zatvoriti. Izađite iz programa Dodavo pa pokušajte ponovno.
serbianlatin.StillRunning=Dodavo je i dalje pokrenut i nije moguće zatvoriti ga. Izađite iz programa Dodavo, pa pokušajte ponovo.
slovenian.StillRunning=Dodavo se še vedno izvaja in ga ni bilo mogoče zapreti. Končajte Dodavo in poskusite znova.

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
; The GNU GPL, which asks for a copy to go with the program, and the licenses
; of the components built into it (installer\licenses.ps1). The program shows
; both in Help > License as well. 'solidbreak' keeps them out of the block of
; the last build above, which Setup would otherwise have to unpack for them.
Source: "{#LicenseFile}"; DestDir: "{app}"; DestName: "LICENSE.txt"; Flags: ignoreversion solidbreak
Source: "{#ThirdPartyLicensesFile}"; DestDir: "{app}"; DestName: "THIRD-PARTY-LICENSES.txt"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\{#AppName}"; Filename: "{app}\{#AppExeName}"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExeName}"; Tasks: desktopicon

[Registry]
; "Start with Windows" is written by the program itself (src/ui/shell.rs);
; remove it on uninstall so Windows doesn't try to start a missing program.
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueName: "Dodavo"; ValueType: none; Flags: uninsdeletevalue
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueName: "QuickAndEasyFileTransfer"; ValueType: none; Flags: uninsdeletevalue
; "Send with Dodavo" in File Explorer's context menu is written by the program
; too. Setup only removes it: an empty verb must not appear before Dodavo runs.
Root: HKCU; Subkey: "Software\Classes\*\shell\Dodavo.Send"; Flags: dontcreatekey uninsdeletekey
Root: HKCU; Subkey: "Software\Classes\Directory\shell\Dodavo.Send"; Flags: dontcreatekey uninsdeletekey

[UninstallDelete]
; The same for the entry in File Explorer's Send to menu.
Type: files; Name: "{usersendto}\Dodavo.lnk"

[Run]
Filename: "{app}\{#AppExeName}"; Description: "{cm:LaunchProgram,{#AppName}}"; Flags: nowait postinstall skipifsilent
; Automatic update (Setup was started by Dodavo itself, see [Code]): start the
; new version again with the arguments Dodavo passed, as the logged-on user
; even when Setup had to elevate for an all-users installation.
Filename: "{app}\{#AppExeName}"; Parameters: "{param:DODAVOARGS|}"; Flags: nowait runasoriginaluser; Check: IsAutoUpdate

; Settings, recipients and the identity key in %APPDATA%\Dodavo are kept on
; uninstall on purpose: reinstalling keeps the same Contact ID.

[Code]
{ Automatic updates. Dodavo downloads this Setup, checks it and starts it with
    /VERYSILENT /SUPPRESSMSGBOXES ... /CURRENTUSER|/ALLUSERS
    /DODAVOUPDATE=<its process id> /DODAVOARGS="<args>"
  and keeps running. Setup elevates itself first when the installation is for
  all users (Windows asks the user), so everything below runs with the rights
  the installation needs. InitializeSetup then asks every running instance
  of Dodavo (all profiles hold the same mutex) to exit by posting the window
  message Dodavo_ExitForUpdate to their main windows, and waits for them.
  PrepareToInstall does so once more, right before the files are replaced,
  and the second [Run] entry above starts Dodavo again afterwards.

  Setup started by the user and the uninstaller close Dodavo in the same way,
  when the user has chosen Install or confirmed: PrepareToInstall, and
  CurUninstallStepChanged below. Transfers are suspended, not cancelled, and
  continue at the next start, so there is nothing to ask. That is why there is
  no AppMutex directive: Setup would check it at its start, and ask the user
  to close Dodavo before even showing the wizard.

  The message name, the window class name and the parameters are a contract
  with src/update and src/ui/main_window.rs: keep them in step. }

const
  SYNCHRONIZE = $00100000;
  { 240 x 250 ms = 60 seconds. }
  UpdateWaitTicks = 240;
  DodavoMainClass = 'Dodavo_Main';
  ExitForUpdateMessage = 'Dodavo_ExitForUpdate';

function OpenProcess(dwDesiredAccess: DWORD; bInheritHandle: BOOL; dwProcessId: DWORD): THandle;
  external 'OpenProcess@kernel32.dll stdcall';
function WaitForSingleObject(hHandle: THandle; dwMilliseconds: DWORD): DWORD;
  external 'WaitForSingleObject@kernel32.dll stdcall';
function CloseHandle(hObject: THandle): BOOL;
  external 'CloseHandle@kernel32.dll stdcall';
function RegisterWindowMessageW(lpString: String): UINT;
  external 'RegisterWindowMessageW@user32.dll stdcall';
{ lpszWindow is declared as a number so that 0 (any title) can be passed. }
function FindWindowExW(hWndParent, hWndChildAfter: HWND; lpszClass: String; lpszWindow: Cardinal): HWND;
  external 'FindWindowExW@user32.dll stdcall';

{ True when Setup was started by Dodavo's own update. }
function IsAutoUpdate: Boolean;
begin
  Result := ExpandConstant('{param:DODAVOUPDATE|}') <> '';
end;

{ Asks every running instance of Dodavo to exit. }
procedure AskDodavoToExit;
var
  Msg: UINT;
  Wnd: HWND;
  Count: Integer;
begin
  Msg := RegisterWindowMessageW(ExitForUpdateMessage);
  Count := 0;
  Wnd := FindWindowExW(0, 0, DodavoMainClass, 0);
  while Wnd <> 0 do
  begin
    PostMessage(Wnd, Msg, 0, 0);
    Count := Count + 1;
    Wnd := FindWindowExW(0, Wnd, DodavoMainClass, 0);
  end;
  Log(Format('Asked %d Dodavo window(s) to exit', [Count]));
end;

{ Waits for the Dodavo that started the update and for every other instance
  to exit. Returns False if one is still running after a minute. }
function WaitForDodavoToExit: Boolean;
var
  Pid: Integer;
  Process: THandle;
  Ticks: Integer;
begin
  Pid := StrToIntDef(ExpandConstant('{param:DODAVOUPDATE|0}'), 0);
  if Pid > 0 then
  begin
    Process := OpenProcess(SYNCHRONIZE, False, Pid);
    if Process <> 0 then
    begin
      Log(Format('Waiting for Dodavo (process %d) to exit', [Pid]));
      WaitForSingleObject(Process, UpdateWaitTicks * 250);
      CloseHandle(Process);
    end;
  end;
  Ticks := 0;
  while CheckForMutexes('{#AppMutexName}') and (Ticks < UpdateWaitTicks) do
  begin
    if Ticks = 0 then
      Log('Waiting for the instances of Dodavo to exit');
    Sleep(250);
    Ticks := Ticks + 1;
  end;
  Result := not CheckForMutexes('{#AppMutexName}');
end;

{ An automatic update (after any elevation): tells Dodavo to exit now and
  waits, so that Dodavo can tell from Setup ending early that it did not start. }
function InitializeSetup: Boolean;
begin
  Result := True;
  if IsAutoUpdate then
  begin
    AskDodavoToExit;
    if not WaitForDodavoToExit then
    begin
      Log('Dodavo is still running; giving up');
      Result := False;
    end;
  end;
end;

{ Runs when the user has chosen Install, before Setup checks for files in use:
  closes Dodavo, or, in an update, an instance that was started meanwhile. }
function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  Result := '';
  AskDodavoToExit;
  if not WaitForDodavoToExit then
  begin
    Log('Dodavo is still running; giving up');
    Result := CustomMessage('StillRunning');
  end;
end;

{ Runs after the user has confirmed uninstalling, before anything is removed. }
procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usAppMutexCheck then
  begin
    AskDodavoToExit;
    if not WaitForDodavoToExit then
      RaiseException(CustomMessage('StillRunning'));
  end;
end;
