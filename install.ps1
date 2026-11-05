# Build herdlite lalu pasang hl.exe + herdlited.exe ke <home>\bin.
# Pemakaian:  powershell -ExecutionPolicy Bypass -File install.ps1 [-InstallDir D:\DevTools\herdlite]
param(
    [string]$InstallDir = $(if (Test-Path 'D:\') { 'D:\DevTools\herdlite' } else { Join-Path $env:USERPROFILE '.herdlite' })
)
$ErrorActionPreference = 'Stop'
Set-Location $PSScriptRoot

cargo build --release
if ($LASTEXITCODE -ne 0) { throw 'cargo build gagal' }

$bin = Join-Path $InstallDir 'bin'
New-Item -ItemType Directory -Force $bin | Out-Null

# Hentikan server lama supaya exe bisa ditimpa.
$hl = Join-Path $bin 'hl.exe'
$wasRunning = $false
if (Test-Path $hl) {
    $st = & $hl status 2>$null | Out-String
    if ($st -match 'berjalan') { $wasRunning = $true; & $hl stop | Out-Null }
}

Copy-Item target\release\hl.exe, target\release\herdlited.exe $bin -Force
Write-Host "Terpasang di $bin"
if ($wasRunning) { & $hl start }

Write-Host ''
Write-Host 'Langkah berikutnya:'
Write-Host "  $hl path:add            # tambahkan $bin ke PATH user"
Write-Host '  hl import-herd          # (opsional) salin park/link/isolate dari Herd'
Write-Host '  hl php:install 8.3      # pasang PHP resmi'
Write-Host '  hl start'
