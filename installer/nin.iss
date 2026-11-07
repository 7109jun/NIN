; Inno Setup script — build with: iscc installer\nin.iss  (expects nin.exe next to this file's ..\dist folder)
#define AppVer "0.1.0"
[Setup]
AppName=NIN Shell
AppVersion={#AppVer}
AppPublisher=NIN Shell
DefaultDirName={autopf}\NIN Shell
DefaultGroupName=NIN Shell
OutputDir=..\dist
OutputBaseFilename=nin-setup-{#AppVer}
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ChangesEnvironment=yes
Compression=lzma2
SolidCompression=yes
UninstallDisplayIcon={app}\nin.exe
[Tasks]
Name: "assoc"; Description: "Associate .nin files with NIN Shell"; Flags: checkedonce
Name: "path"; Description: "Add NIN Shell to PATH"; Flags: checkedonce
[Files]
Source: "..\dist\nin.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\examples\*"; DestDir: "{app}\examples"; Flags: recursesubdirs
Source: "..\docs\*"; DestDir: "{app}\docs"; Flags: recursesubdirs
[Icons]
Name: "{group}\NIN Shell"; Filename: "{app}\nin.exe"
Name: "{group}\Uninstall NIN Shell"; Filename: "{uninstallexe}"
[Run]
Filename: "{app}\nin.exe"; Parameters: "assoc install"; Flags: runhidden; Tasks: assoc
[UninstallRun]
Filename: "{app}\nin.exe"; Parameters: "assoc uninstall"; Flags: runhidden
[Registry]
Root: HKCU; Subkey: "Environment"; ValueType: expandsz; ValueName: "Path"; ValueData: "{olddata};{app}"; Tasks: path; Check: NeedsPath(ExpandConstant('{app}'))
[Code]
function NeedsPath(P: string): Boolean;
var O: string;
begin
  if not RegQueryStringValue(HKCU, 'Environment', 'Path', O) then begin Result := True; exit; end;
  Result := Pos(';' + Lowercase(P) + ';', ';' + Lowercase(O) + ';') = 0;
end;
