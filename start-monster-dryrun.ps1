# SPDX-License-Identifier: Apache-2.0
# Inicia o ecossistema MONSTER (Fases 0-5) em modo --dry-run (Chrome + Sidecar Go + Core Rust).
# Tudo gravado estritamente no disco O: (zero escrita no disco C:).

$ErrorActionPreference = "Stop"
$TargetUrl  = "https://www.jogatudo.com/pt-BR/game/playtech-brasileira.roleta-brasileira"
$ProfileDir = "O:\Archives\Eistein\roulette-profile"
$RootDir    = "O:\Archives\Eistein\FUI AGENT ROULLET"
$GoCacheDir = "$RootDir\target\go-cache"
$GoTmpDir   = "$RootDir\target\go-tmp"
$CargoTarget = "$RootDir\target"

foreach ($dir in @($ProfileDir, $GoCacheDir, $GoTmpDir, $CargoTarget)) {
    if (-not (Test-Path $dir)) {
        New-Item -ItemType Directory -Force -Path $dir | Out-Null
    }
}

$env:GOCACHE          = $GoCacheDir
$env:GOTMPDIR         = $GoTmpDir
$env:CARGO_TARGET_DIR = $CargoTarget
$env:RUSTUP_TOOLCHAIN = "stable-x86_64-pc-windows-msvc"
$env:TEMP             = $GoTmpDir
$env:TMP              = $GoTmpDir

Write-Host "================================================================" -ForegroundColor Cyan
Write-Host " MONSTER (Fases 0-5) - DAG + Risco + Executor --dry-run" -ForegroundColor Cyan
Write-Host "================================================================" -ForegroundColor Cyan

$ChromeExe = "O:\Archives\Eistein\chromium\chrome.exe"
if (-not (Test-Path $ChromeExe)) {
    Write-Error "Chromium dedicado nao encontrado em $ChromeExe"
}

$RunningChromium = Get-Process chrome -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $ChromeExe }
if (-not $RunningChromium) {
    # Se havia um Chrome comum segurando a porta 9222, libera para subir a Roleta no Chromium dedicado
    $Port9222 = Get-NetTCPConnection -LocalPort 9222 -State Listen -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($Port9222) {
        Stop-Process -Id $Port9222.OwningProcess -Force -ErrorAction SilentlyContinue
        Start-Sleep -Seconds 1
    }
    $CdpAlive = $false
} else {
    try {
        $null = Invoke-RestMethod -Uri "http://127.0.0.1:9222/json/version" -TimeoutSec 1
        $CdpAlive = $true
    } catch {
        $CdpAlive = $false
    }
}

if (-not $CdpAlive) {
    $SingletonLock = Join-Path $ProfileDir "SingletonLock"
    if (Test-Path $SingletonLock) {
        Remove-Item -Force -ErrorAction SilentlyContinue $SingletonLock
    }

    Write-Host "[MONSTER] Abrindo MESA DA ROLETA no Chromium dedicado ($ChromeExe)..." -ForegroundColor Cyan
    Start-Process -FilePath $ChromeExe -ArgumentList @(
        "--remote-debugging-port=9222",
        "--user-data-dir=$ProfileDir",
        "--disable-blink-features=AutomationControlled",
        "--test-type",
        "--no-first-run",
        "--no-default-browser-check",
        "--start-maximized",
        $TargetUrl
    )
    Start-Sleep -Seconds 3
}

$HealthAlive = $false
try {
    $null = Invoke-RestMethod -Uri "http://127.0.0.1:8790/health" -TimeoutSec 1
    $HealthAlive = $true
} catch {
    $HealthAlive = $false
}

if (-not $HealthAlive) {
    Write-Host "[MONSTER] Iniciando Sidecar Go em segundo plano (porta 8787)..." -ForegroundColor Green
    Start-Process powershell -ArgumentList @(
        "-NoProfile",
        "-ExecutionPolicy", "Bypass",
        "-Command", "`$env:GOCACHE='$GoCacheDir'; `$env:GOTMPDIR='$GoTmpDir'; Set-Location '$RootDir\sidecar'; go run ./cmd/capture --ws-probe"
    )
    Start-Sleep -Seconds 3
}

Write-Host "[MONSTER] Abrindo painel FUI em http://127.0.0.1:8791 e iniciando monster-core (--dry-run)..." -ForegroundColor Yellow
Start-Process "http://127.0.0.1:8791"
Set-Location $RootDir
cargo run --offline -p monster-core -- --dry-run
