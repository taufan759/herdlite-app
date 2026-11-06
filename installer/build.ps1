# Bangun installer herdlite secara lokal.
# Butuh: Rust (cargo) dan Inno Setup 6 (ISCC.exe).
# Pemakaian:  powershell -ExecutionPolicy Bypass -File installer\build.ps1 [-Iscc "C:\path\ISCC.exe"]
param([string]$Iscc = '')
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
Set-Location $root

$version = (Select-String -Path Cargo.toml -Pattern '^version = "(.+)"').Matches[0].Groups[1].Value
cargo build --release
if ($LASTEXITCODE -ne 0) { throw 'cargo build gagal' }

if (-not $Iscc) {
    $cands = @(
        "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe",
        "$env:ProgramFiles\Inno Setup 6\ISCC.exe",
        "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe",
        'D:\DevTools\InnoSetup\ISCC.exe'
    )
    $Iscc = $cands | Where-Object { Test-Path $_ } | Select-Object -First 1
}
if (-not $Iscc) { throw 'ISCC.exe (Inno Setup 6) tidak ditemukan. Pasang dari https://jrsoftware.org/isdl.php atau beri -Iscc.' }

& $Iscc "/DAppVersion=$version" "$PSScriptRoot\herdlite.iss"
if ($LASTEXITCODE -ne 0) { throw 'ISCC gagal' }
Get-Item "$PSScriptRoot\Output\herdlite-setup-$version.exe" | Select-Object FullName, @{n='MB';e={[math]::Round($_.Length/1MB,2)}}
