; The Windows installer: dist/onIfy (from bundle.sh) into Program Files, or
; the user's own folder when they aren't an admin, with a Start menu entry.
; Build: ISCC /DAppVersion=0.1.0 packaging\windows\onIfy.iss

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif

[Setup]
AppId={{6E0C1F7A-2D44-4B8E-9C61-0F1D5A7B3E92}
AppName=onIfy
AppVersion={#AppVersion}
AppVerName=onIfy {#AppVersion}
AppPublisher=orqz
DefaultDirName={autopf}\onIfy
DefaultGroupName=onIfy
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir=..\..\dist
OutputBaseFilename=onIfy-setup-x86_64
SetupIconFile=onify.ico
UninstallDisplayIcon={app}\onify.ico
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
CloseApplications=force
RestartApplications=no

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "..\..\dist\onIfy\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{autoprograms}\onIfy"; Filename: "{app}\bin\onify.exe"; IconFilename: "{app}\onify.ico"
Name: "{autodesktop}\onIfy"; Filename: "{app}\bin\onify.exe"; IconFilename: "{app}\onify.ico"; Tasks: desktopicon

[Run]
Filename: "{app}\bin\onify.exe"; Description: "{cm:LaunchProgram,onIfy}"; Flags: nowait postinstall skipifsilent
; onIfy's own updater runs this installer silently, then expects onIfy back.
Filename: "{app}\bin\onify.exe"; Flags: nowait; Check: WizardSilent

[Code]
// A running onIfy (maybe only in the tray) keeps its files locked, and the
// new one would just hand over to it, so close it first.
function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  ResultCode: Integer;
begin
  Exec(ExpandConstant('{sys}\taskkill.exe'), '/f /im onify.exe', '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
  Result := '';
end;
