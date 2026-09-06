; Termior Windows 安装包脚本（Inno Setup 6）。
; 由 scripts/package-release.ps1 在 release 打包时调用：
;   ISCC /DAppVersion=<version> packaging\windows\termior.iss
; 版本号必须通过 /DAppVersion 显式传入，与 Cargo.toml workspace 版本一致。

#define MyAppName "Termior"
#define MyAppPublisher "Termior Contributors"
#define MyAppURL "https://github.com/willmove/termior"

#ifndef AppVersion
#define AppVersion "0.0.0"
#endif

[Setup]
AppId={{7C1E9A64-2B8D-4E05-A3F7-58C0D9E14B26}
AppName={#MyAppName}
AppVersion={#AppVersion}
AppPublisher={#MyAppPublisher}
AppPublisherURL={#MyAppURL}
AppSupportURL={#MyAppURL}
DefaultDirName={autopf}\{#MyAppName}
DefaultGroupName={#MyAppName}
DisableProgramGroupPage=yes
LicenseFile=..\..\LICENSE
OutputDir=..\..\dist
OutputBaseFilename=termior-{#AppVersion}-windows-x86_64-setup
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
ArchitecturesInstallIn64BitMode=x64
UninstallDisplayIcon={app}\termior.ico

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; \
    GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "..\..\dist\termior-{#AppVersion}-windows-x86_64\termior.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\dist\termior-{#AppVersion}-windows-x86_64\termior.ico"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\{#MyAppName}"; Filename: "{app}\termior.exe"
Name: "{autodesktop}\{#MyAppName}"; Filename: "{app}\termior.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\termior.exe"; Description: "{cm:LaunchProgram,{#MyAppName}}"; \
    Flags: nowait postinstall skipifsilent
