; Installer herdlite (Inno Setup 6).
; Build:  ISCC.exe /DAppVersion=0.2.0 installer\herdlite.iss
; Hasil:  installer\Output\herdlite-setup-<versi>.exe
;
; Tidak butuh hak admin: dipasang per user. Langkah yang butuh izin (hosts file,
; sertifikat HTTPS) memunculkan dialog Windows sendiri saat setup berjalan.

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif

[Setup]
AppId={{6E3C2B1A-7F4D-4C8E-9B21-5A0D3E8F4C71}
AppName=herdlite
AppVersion={#AppVersion}
AppVerName=herdlite {#AppVersion}
AppPublisher=Taufan
AppPublisherURL=https://github.com/taufan759/herdlite-app
AppSupportURL=https://github.com/taufan759/herdlite-app/issues
DefaultDirName={localappdata}\herdlite
DisableProgramGroupPage=yes
DisableReadyPage=no
PrivilegesRequired=lowest
OutputDir=Output
OutputBaseFilename=herdlite-setup-{#AppVersion}
Compression=lzma2/ultra64
SolidCompression=yes
WizardStyle=modern
WizardSizePercent=110
UninstallDisplayName=herdlite (server PHP lokal)
UninstallDisplayIcon={app}\bin\herdlited.exe
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0.17763
LicenseFile=..\LICENSE
CloseApplications=no

[Messages]
; Terjemahan bagian utama wizard (Inno Setup belum punya bahasa Indonesia resmi).
ButtonBack=< &Kembali
ButtonNext=&Lanjut >
ButtonInstall=&Pasang
ButtonCancel=Batal
ButtonFinish=&Selesai
ButtonBrowse=&Pilih...
WelcomeLabel1=Selamat datang di herdlite
WelcomeLabel2=herdlite adalah server PHP lokal yang ringan, pengganti XAMPP, Laragon, dan Laravel Herd.%n%nSetelah terpasang, setiap folder di folder proyekmu bisa langsung dibuka di http://<nama-folder>.test.%n%nInstaller akan mengunduh PHP resmi dari windows.php.net (±30 MB), jadi pastikan internet tersambung.
WizardLicense=Lisensi
LicenseLabel=Baca lisensi berikut sebelum melanjutkan.
LicenseLabel3=herdlite gratis dan open source (lisensi MIT).
LicenseAccepted=Saya &setuju
LicenseNotAccepted=Saya &tidak setuju
WizardSelectDir=Lokasi program
SelectDirDesc=Di mana herdlite (dan PHP yang diunduh) disimpan?
SelectDirLabel3=herdlite akan dipasang di folder berikut. Butuh sekitar 200 MB (PHP, Composer, Node.js).
SelectDirBrowseLabel=Klik Lanjut untuk memakai folder ini, atau Pilih... untuk folder lain.
WizardSelectTasks=Pilihan tambahan
SelectTasksDesc=Komponen apa saja yang ikut dipasang?
SelectTasksLabel2=Centang yang kamu butuhkan, lalu klik Lanjut.
WizardReady=Siap memasang
ReadyLabel1=herdlite siap dipasang.
ReadyLabel2a=Klik Pasang. Di akhir akan muncul dua dialog Windows (sertifikat HTTPS dan izin untuk mendaftarkan nama .test). Klik Yes pada keduanya.
WizardInstalling=Memasang
InstallingLabel=Tunggu sebentar, herdlite sedang dipasang dan disiapkan...
FinishedHeadingLabel=herdlite sudah terpasang
FinishedLabelNoIcons=herdlite sudah siap dipakai. Dashboard akan terbuka otomatis.%n%nNanti, buka "herdlite" dari Start Menu kapan saja.
FinishedLabel=herdlite sudah siap dipakai. Dashboard akan terbuka otomatis.%n%nNanti, buka "herdlite" dari Start Menu kapan saja.
ConfirmUninstall=Hapus herdlite dari komputer ini?%n%nFolder proyekmu TIDAK akan dihapus.
UninstalledAll=herdlite sudah dihapus. Folder proyekmu tetap utuh.

[CustomMessages]
SitesTitle=Folder proyek
SitesDesc=Di mana kamu menyimpan proyek PHP/Laravel?
SitesLabel=Setiap folder di dalam folder ini otomatis menjadi situs. Contoh: folder "toko" → http://toko.test%n%nKalau folder masih kosong, herdlite membuat contoh situs "halo".
TaskNode=Pasang Node.js LTS (untuk npm, Vite, dan starter kit Laravel)
TaskLaravel=Pasang Laravel installer (perintah "laravel new")
TaskDesktop=Buat shortcut di Desktop
RunSetup=Menyiapkan herdlite: mengunduh PHP, Composer, dan lainnya...

[Tasks]
Name: "node"; Description: "{cm:TaskNode}"
Name: "laravel"; Description: "{cm:TaskLaravel}"
Name: "desktop"; Description: "{cm:TaskDesktop}"; Flags: unchecked

[Files]
Source: "..\target\release\hl.exe"; DestDir: "{app}\bin"; Flags: ignoreversion
Source: "..\target\release\herdlited.exe"; DestDir: "{app}\bin"; Flags: ignoreversion
Source: "..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"; Flags: ignoreversion
Source: "..\README.md"; DestDir: "{app}"; Flags: ignoreversion

[Run]
; Jendela konsol sengaja terlihat supaya progres unduhan dan pesan error bisa dibaca.
Filename: "{app}\bin\hl.exe"; Parameters: "setup --installer --php=latest --sites=""{code:GetSitesDir}"" {code:SetupFlags}"; StatusMsg: "{cm:RunSetup}"; Flags: waituntilterminated

[UninstallRun]
; Bersihkan autostart, shortcut, PATH, CA HTTPS, dan blok hosts (bisa muncul dialog Windows).
Filename: "{app}\bin\hl.exe"; Parameters: "uninstall --yes"; RunOnceId: "herdliteCleanup"; Flags: waituntilterminated

[UninstallDelete]
; Hanya file milik herdlite; folder proyek tidak pernah disentuh.
Type: filesandordirs; Name: "{app}\php"
Type: filesandordirs; Name: "{app}\node"
Type: filesandordirs; Name: "{app}\certs"
Type: filesandordirs; Name: "{app}\logs"
Type: filesandordirs; Name: "{app}\run"
Type: filesandordirs; Name: "{app}\etc"
Type: filesandordirs; Name: "{app}\downloads"
Type: filesandordirs; Name: "{app}\bin"
Type: files; Name: "{app}\config.json"
Type: files; Name: "{app}\config.json.tmp"
Type: dirifempty; Name: "{app}"

[Code]
var
  SitesPage: TInputDirWizardPage;

procedure InitializeWizard;
begin
  SitesPage := CreateInputDirPage(wpSelectDir, CustomMessage('SitesTitle'), CustomMessage('SitesDesc'),
    CustomMessage('SitesLabel'), False, '');
  SitesPage.Add('');
  SitesPage.Values[0] := GetPreviousData('SitesDir', ExpandConstant('{%USERPROFILE}\Sites'));
end;

procedure RegisterPreviousData(PreviousDataKey: Integer);
begin
  SetPreviousData(PreviousDataKey, 'SitesDir', SitesPage.Values[0]);
end;

function GetSitesDir(Param: String): String;
begin
  { Jangan diakhiri "\" karena "\"" akan dibaca sebagai tanda kutip ter-escape. }
  Result := RemoveBackslashUnlessRoot(SitesPage.Values[0]);
  if Copy(Result, Length(Result), 1) = '\' then
    Result := Result + '.';
end;

function SetupFlags(Param: String): String;
begin
  Result := '';
  if not WizardIsTaskSelected('node') then Result := Result + ' --no-node';
  if not WizardIsTaskSelected('laravel') then Result := Result + ' --no-laravel';
  if WizardIsTaskSelected('desktop') then Result := Result + ' --desktop';
end;

{ Hentikan server lama sebelum file exe ditimpa (saat update). }
function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  Code: Integer;
  Hl: String;
begin
  Result := '';
  Hl := ExpandConstant('{app}\bin\hl.exe');
  if FileExists(Hl) then
    Exec(Hl, 'stop', '', SW_HIDE, ewWaitUntilTerminated, Code);
end;
