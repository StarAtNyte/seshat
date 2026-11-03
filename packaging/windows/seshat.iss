; Inno Setup script for the Seshat Windows installer.
#define AppVersion GetEnv("SESHAT_VERSION")

[Setup]
AppId={{6C1F3E2A-9B47-4D52-A1E3-5E5A7A3C2B10}
AppName=Seshat
AppVersion={#AppVersion}
AppPublisher=Seshat
DefaultDirName={autopf}\Seshat
DefaultGroupName=Seshat
UninstallDisplayIcon={app}\seshat.exe
OutputBaseFilename=Seshat-{#AppVersion}-windows-x64-setup
OutputDir=..\..\dist
Compression=lzma2
SolidCompression=yes
ArchitecturesInstallIn64BitMode=x64compatible
PrivilegesRequiredOverridesAllowed=dialog
SetupIconFile=..\..\assets\icons\seshat.ico
WizardStyle=modern

[Tasks]
Name: "desktopicon"; Description: "Create a desktop shortcut"; Flags: unchecked

[Files]
Source: "..\..\target\release\seshat.exe"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\Seshat"; Filename: "{app}\seshat.exe"
Name: "{autodesktop}\Seshat"; Filename: "{app}\seshat.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\seshat.exe"; Description: "Launch Seshat"; Flags: nowait postinstall skipifsilent
