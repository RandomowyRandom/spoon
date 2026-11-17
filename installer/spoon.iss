; Inno Setup script for Spoon. Build the release exe first, then:
;   iscc installer\spoon.iss
; Output: target\installer\Spoon-<version>-x64-setup.exe

#define AppExe "..\target\release\spoon.exe"
; Single source of truth for the version: Cargo.toml, via the exe's version resource.
#define AppVersion GetStringFileInfo(AppExe, "ProductVersion")
#if AppVersion == ""
  #error spoon.exe has no ProductVersion. Run "cargo build --release" first.
#endif

[Setup]
; Never change AppId: it is how upgrades and uninstall find an existing install.
AppId={{10FB3B53-28CB-40E7-A073-37D840B6C851}
AppName=Spoon
AppVersion={#AppVersion}
AppPublisher=Spoon contributors
AppPublisherURL=https://github.com/RandomowyRandom/spoon
DefaultDirName={autopf}\Spoon
DisableProgramGroupPage=yes
; Per-user by default (no admin prompt); the first page lets the user pick all users.
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir=..\target\installer
OutputBaseFilename=Spoon-{#AppVersion}-x64-setup
SetupIconFile=..\assets\spoon.ico
UninstallDisplayIcon={app}\spoon.exe
UninstallDisplayName=Spoon
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
; Lets an upgrade replace spoon.exe while Spoon is running.
CloseApplications=yes

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#AppExe}"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"; Flags: ignoreversion
Source: "..\THIRD-PARTY-LICENSES.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\assets\fonts\OFL.txt"; DestDir: "{app}"; DestName: "JetBrainsMono-OFL.txt"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\Spoon"; Filename: "{app}\spoon.exe"
Name: "{autodesktop}\Spoon"; Filename: "{app}\spoon.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\spoon.exe"; Description: "{cm:LaunchProgram,Spoon}"; Flags: nowait postinstall skipifsilent

[Code]
// Spoon drives the git command line, so say so up front when git is missing.
function InitializeSetup(): Boolean;
begin
  Result := True;
  if FileSearch('git.exe', GetEnv('PATH')) = '' then
    Result := SuppressibleMsgBox(
      'Git was not found on this computer.' + #13#10 + #13#10 +
      'Spoon needs Git for Windows (https://git-scm.com) to work. ' +
      'You can install it before or after Spoon.' + #13#10 + #13#10 +
      'Continue installing Spoon?',
      mbConfirmation, MB_YESNO, IDYES) = IDYES;
end;
