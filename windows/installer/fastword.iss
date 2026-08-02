; FastWord for Windows — Inno Setup script.
; Per-user install (no admin prompt): binaries go to
; %LOCALAPPDATA%\Programs\FastWord. Speech models are NOT bundled — the app
; offers the download in its Models tab on first launch.
;
; Build:  iscc windows\installer\fastword.iss
; (CI builds it automatically; Inno Setup 6 is preinstalled on GitHub
; windows runners.)

#define AppVersion "0.7.2"

[Setup]
AppId={{1A8161D4-A910-4857-AD86-254EB4ED4690}
AppName=FastWord
AppVersion={#AppVersion}
AppPublisher=VasenevEA
AppPublisherURL=https://github.com/VasenevEA/FastWord
AppSupportURL=https://github.com/VasenevEA/FastWord/issues
DefaultDirName={autopf}\FastWord
DefaultGroupName=FastWord
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
OutputDir=..\target\installer
OutputBaseFilename=FastWord-Setup-{#AppVersion}
SetupIconFile=..\assets\fastword.ico
UninstallDisplayIcon={app}\fastword-win.exe
Compression=lzma2
SolidCompression=yes
ArchitecturesInstallIn64BitMode=x64compatible
CloseApplications=yes

[Languages]
Name: "en"; MessagesFile: "compiler:Default.isl"
Name: "ru"; MessagesFile: "compiler:Languages\Russian.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; Flags: unchecked
Name: "autostart"; Description: "{cm:AutoStartProgram,FastWord}"; Flags: unchecked

[Files]
Source: "..\target\release\fastword-win.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\target\release\fastword-sidecar.exe"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\FastWord"; Filename: "{app}\fastword-win.exe"
Name: "{autodesktop}\FastWord"; Filename: "{app}\fastword-win.exe"; Tasks: desktopicon

[Registry]
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; \
    ValueType: string; ValueName: "FastWord"; ValueData: """{app}\fastword-win.exe"""; \
    Flags: uninsdeletevalue; Tasks: autostart

[Run]
Filename: "{app}\fastword-win.exe"; Description: "{cm:LaunchProgram,FastWord}"; \
    Flags: nowait postinstall skipifsilent

[UninstallRun]
Filename: "taskkill"; Parameters: "/f /im fastword-win.exe"; Flags: runhidden; RunOnceId: "KillApp"
Filename: "taskkill"; Parameters: "/f /im fastword-sidecar.exe"; Flags: runhidden; RunOnceId: "KillSidecar"
