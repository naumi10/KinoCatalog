; Inno Setup script for the KinoCatalog x64 Windows installer.
;
; The release workflow compiles this with:
;   ISCC.exe /DAppVersion="<version>" installer\windows\KinoCatalog.iss
; AppVersion defaults to 0.0.0 so the script can also be compiled locally
; without the /D switch.

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif

#define AppName "KinoCatalog"
#define AppExe "KinoCatalog.exe"
#define AppPublisher "KinoCatalog"
#define AppUrl "https://github.com/naumi10/KinoCatalog"
#define BuildDir "..\..\target\x86_64-pc-windows-msvc\release"

[Setup]
; Keep this GUID stable so upgrades replace the previous installation.
AppId={{b82eeb12-ce53-4fe2-9ef3-7a7d45b9e30e}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher={#AppPublisher}
AppPublisherURL={#AppUrl}
AppSupportURL={#AppUrl}/issues
AppUpdatesURL={#AppUrl}/releases/latest
DefaultDirName={userpf}\{#AppName}
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
UninstallDisplayIcon={app}\{#AppExe}
OutputBaseFilename=KinoCatalog-windows-x64-setup
SetupIconFile=..\..\icon.ico
Compression=lzma2
SolidCompression=yes
WizardStyle=modern

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#BuildDir}\{#AppExe}"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\{#AppName}"; Filename: "{app}\{#AppExe}"; WorkingDir: "{app}"
Name: "{group}\{cm:UninstallProgram,{#AppName}}"; Filename: "{uninstallexe}"; WorkingDir: "{app}"
Name: "{userdesktop}\{#AppName}"; Filename: "{app}\{#AppExe}"; WorkingDir: "{app}"; Tasks: desktopicon

[Run]
Filename: "{app}\{#AppExe}"; Description: "{cm:LaunchProgram,{#StringChange(AppName, '&', '&&')}}"; Flags: nowait postinstall skipifsilent
