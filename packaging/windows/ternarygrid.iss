; TernaryGrid's Windows installer (Inno Setup 6), built by scripts/bundle-windows.ps1:
;
;   ISCC.exe /DAppVersion=1.2.3 /DSourceExe=...\ternarygrid.exe /DOutputDir=...\dist ^
;            /DOutputBaseFilename=TernaryGrid-1.2.3-windows-x64-setup packaging\windows\ternarygrid.iss
;
; Installs for the current user (no administrator rights) unless "all users"
; is picked, adds a Start menu shortcut (a desktop one when asked) and an
; uninstaller. Settings, connections and scripts (in %APPDATA%) stay when
; TernaryGrid is uninstalled. Installing a new version over the old one updates it.

#ifndef AppVersion
  #error Define AppVersion (/DAppVersion=1.2.3)
#endif
#ifndef SourceExe
  #error Define SourceExe (/DSourceExe=path\to\ternarygrid.exe)
#endif
#ifndef OutputDir
  #define OutputDir "..\..\dist"
#endif
#ifndef OutputBaseFilename
  #define OutputBaseFilename "TernaryGrid-" + AppVersion + "-windows-x64-setup"
#endif

[Setup]
AppId={{7B8482FA-D36D-440D-A231-6D721F322382}
AppName=TernaryGrid
AppVersion={#AppVersion}
AppVerName=TernaryGrid {#AppVersion}
VersionInfoVersion={#AppVersion}
AppPublisher=TernaryGrid
AppPublisherURL=https://github.com/ilhamnadhif/ternarygrid
AppSupportURL=https://github.com/ilhamnadhif/ternarygrid/issues
AppUpdatesURL=https://github.com/ilhamnadhif/ternarygrid/releases
DefaultDirName={autopf}\TernaryGrid
DefaultGroupName=TernaryGrid
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
OutputDir={#OutputDir}
OutputBaseFilename={#OutputBaseFilename}
SetupIconFile=ternarygrid.ico
UninstallDisplayIcon={app}\ternarygrid.exe
UninstallDisplayName=TernaryGrid
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
; A running TernaryGrid is closed before its files are replaced.
CloseApplications=yes

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#SourceExe}"; DestDir: "{app}"; DestName: "ternarygrid.exe"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\TernaryGrid"; Filename: "{app}\ternarygrid.exe"
Name: "{autodesktop}\TernaryGrid"; Filename: "{app}\ternarygrid.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\ternarygrid.exe"; Description: "{cm:LaunchProgram,TernaryGrid}"; Flags: nowait postinstall skipifsilent
