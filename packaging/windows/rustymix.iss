; Installateur Windows de Rustymix (Inno Setup 6).
;
; Compilation (après `cargo build --release`) :
;   iscc /DAppVersion=0.2.0 packaging\windows\rustymix.iss
; Produit dist\rustymix-setup-x64.exe.
;
; Installation pour l'utilisateur courant par défaut (sans droits
; administrateur), ou pour tous les utilisateurs si on le choisit.

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif

[Setup]
AppId={{74C2B57F-8BD6-40F8-9E65-AA7EA3E10368}
AppName=Rustymix
AppVersion={#AppVersion}
AppVerName=Rustymix {#AppVersion}
AppPublisher=Aexll
AppPublisherURL=https://github.com/Aexll/Rustymix
AppSupportURL=https://github.com/Aexll/Rustymix/issues
AppUpdatesURL=https://github.com/Aexll/Rustymix/releases
DefaultDirName={autopf}\Rustymix
DefaultGroupName=Rustymix
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ArchitecturesAllowed=x64
ArchitecturesInstallIn64BitMode=x64
ShowLanguageDialog=auto
OutputDir=..\..\dist
OutputBaseFilename=rustymix-setup-x64
SetupIconFile=..\..\assets\logo.ico
UninstallDisplayIcon={app}\rustymix.exe
UninstallDisplayName=Rustymix
WizardStyle=modern
WizardImageFile=wizard.bmp,wizard-2x.bmp
WizardSmallImageFile=wizard-small.bmp,wizard-small-2x.bmp
Compression=lzma2/max
SolidCompression=yes
CloseApplications=yes
RestartApplications=no
VersionInfoVersion={#AppVersion}
VersionInfoProductName=Rustymix

[Languages]
Name: "french"; MessagesFile: "compiler:Languages\French.isl"
Name: "english"; MessagesFile: "compiler:Default.isl"

[CustomMessages]
french.ToolsGroup=Outils facultatifs :
english.ToolsGroup=Optional tools:
french.ToolsTask=Installer yt-dlp et FFmpeg (pour installer des sons depuis YouTube) avec winget
english.ToolsTask=Install yt-dlp and FFmpeg (to install sounds from YouTube) with winget
french.ToolsStatus=Installation de yt-dlp et FFmpeg…
english.ToolsStatus=Installing yt-dlp and FFmpeg…
french.LaunchApp=Lancer Rustymix
english.LaunchApp=Launch Rustymix

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked
Name: "tools"; Description: "{cm:ToolsTask}"; GroupDescription: "{cm:ToolsGroup}"; Flags: unchecked; Check: WingetAvailable

[Files]
Source: "..\..\target\release\rustymix.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\Rustymix"; Filename: "{app}\rustymix.exe"
Name: "{autodesktop}\Rustymix"; Filename: "{app}\rustymix.exe"; Tasks: desktopicon

[Run]
Filename: "{code:WingetPath}"; Parameters: "install --id yt-dlp.yt-dlp --exact --silent --accept-source-agreements --accept-package-agreements"; StatusMsg: "{cm:ToolsStatus}"; Flags: runhidden waituntilterminated; Tasks: tools
Filename: "{code:WingetPath}"; Parameters: "install --id Gyan.FFmpeg --exact --silent --accept-source-agreements --accept-package-agreements"; StatusMsg: "{cm:ToolsStatus}"; Flags: runhidden waituntilterminated; Tasks: tools
Filename: "{app}\rustymix.exe"; Description: "{cm:LaunchApp}"; Flags: nowait postinstall skipifsilent

; Les réglages, la bibliothèque et les transitions (%APPDATA%\Rustymix) sont
; conservés à la désinstallation.

[Code]
// winget (Gestionnaire de paquets Windows), s'il est installé.
function WingetPath(Param: String): String;
begin
  Result := ExpandConstant('{localappdata}\Microsoft\WindowsApps\winget.exe');
end;

function WingetAvailable: Boolean;
begin
  Result := FileExists(WingetPath(''));
end;
