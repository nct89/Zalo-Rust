; Optional Inno Setup script for Zalo-Rust on Windows.
; Invoked automatically by build-windows.ps1 when `iscc` is on PATH.

#ifndef MyAppVersion
  #define MyAppVersion "0.2.2"
#endif
#ifndef MyArch
  #define MyArch "x86_64"
#endif

[Setup]
AppId={{7A6C4B91-3A6F-4E2A-9F53-89C4E1A2D6F0}
AppName=Zalo-Rust
AppVersion={#MyAppVersion}
AppPublisher=Zalo-Rust
AppPublisherURL=https://github.com/nct89/Zalo-Rust
DefaultDirName={localappdata}\Programs\Zalo-Rust
DefaultGroupName=Zalo-Rust
UninstallDisplayIcon={app}\zalo-gpui.exe
SetupIconFile=zalo.ico
Compression=lzma2/max
SolidCompression=yes
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir={#MyOutputDir}
OutputBaseFilename=Zalo-{#MyAppVersion}-windows-{#MyArch}-setup
WizardStyle=modern

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "Create a &desktop shortcut"; GroupDescription: "Additional icons:"
Name: "autostart"; Description: "Start Zalo-Rust automatically when Windows starts"; GroupDescription: "Startup:"; Flags: unchecked

[Files]
Source: "{#MyStageDir}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{group}\Zalo-Rust"; Filename: "{app}\zalo-gpui.exe"
Name: "{autodesktop}\Zalo-Rust"; Filename: "{app}\zalo-gpui.exe"; Tasks: desktopicon

[Registry]
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "ZaloRust"; ValueData: """{app}\zalo-gpui.exe"" --hidden"; Flags: uninsdeletevalue; Tasks: autostart

[Run]
Filename: "{app}\zalo-gpui.exe"; Description: "Launch Zalo-Rust"; Flags: nowait postinstall skipifsilent
