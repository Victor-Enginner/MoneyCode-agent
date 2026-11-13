# SPDX-License-Identifier: Apache-2.0
# Iniciador unico do MONSTER: Chrome dedicado + sidecar Go + core Rust.
#
#   start-monster                  observa a mesa (--dry-run, default, NAO aposta)
#   start-monster -Live            executa de verdade (aposta dinheiro real)
#   start-monster -Probe           so o sidecar em --ws-probe (diagnostico de WS, sem core)
#
# Tudo gravado estritamente no disco O: (zero escrita no disco C:).

param(
    [switch]$Live,
    [switch]$Probe
)

$ErrorActionPreference = "Stop"
$TargetUrl   = "https://www.jogatudo.com/pt-BR/game/playtech-brasileira.roleta-brasileira"
$ProfileDir  = "O:\Archives\Eistein\chromium-profile"
$RootDir     = "O:\Archives\Eistein\FUI AGENT ROULLET"
$GoCacheDir  = "$RootDir\target\go-cache"
$GoTmpDir    = "$RootDir\target\go-tmp"
$CargoTarget = "$RootDir\target"
$ChromeExe   = "O:\Archives\Eistein\chromium\chrome.exe"

# `-Probe` nao sobe o core, entao `-Live` nao tem efeito nenhum ali.
if ($Probe) { $Live = $false }

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

$ModeLabel = if ($Probe) { "WS-PROBE (diagnostico, sem core)" } elseif ($Live) { "LIVE (DINHEIRO REAL)" } else { "DRY-RUN (observa, nao aposta)" }
Write-Host "================================================================" -ForegroundColor Cyan
Write-Host " MONSTER - Roleta Brasileira" -ForegroundColor Cyan
Write-Host " Modo: $ModeLabel" -ForegroundColor $(if ($Live) { "Red" } else { "Cyan" })
Write-Host "================================================================" -ForegroundColor Cyan

if (-not (Test-Path $ChromeExe)) {
    Write-Error "Chromium dedicado nao encontrado em $ChromeExe"
}

# Um segundo core brigaria pela porta 8791 e gravaria decisoes em paralelo.
if (-not $Probe) {
    $RunningCore = Get-Process monster-core -ErrorAction SilentlyContinue
    if ($RunningCore) {
        Write-Host "[MONSTER] ja existe um monster-core rodando (PID $($RunningCore[0].Id))." -ForegroundColor Red
        Write-Host "[MONSTER] feche antes de iniciar outro:  taskkill /PID $($RunningCore[0].Id) /F" -ForegroundColor Red
        exit 1
    }
}

$CdpAlive = $false
try {
    $ver = Invoke-RestMethod -Uri "http://127.0.0.1:9222/json/version" -TimeoutSec 1
    if ($ver.Browser -match "131\.") {
        $CdpAlive = $true
        Write-Host "[MONSTER] Chromium dedicado ja esta na porta 9222 - conectando na mesa." -ForegroundColor Green
    } else {
        $Port9222 = Get-NetTCPConnection -LocalPort 9222 -State Listen -ErrorAction SilentlyContinue | Select-Object -First 1
        if ($Port9222) {
            Stop-Process -Id $Port9222.OwningProcess -Force -ErrorAction SilentlyContinue
            Start-Sleep -Seconds 1
        }
    }
} catch {
    $CdpAlive = $false
}

if (-not $CdpAlive) {
    $SingletonLock = Join-Path $ProfileDir "SingletonLock"
    if (Test-Path $SingletonLock) {
        Remove-Item -Force -ErrorAction SilentlyContinue $SingletonLock
    }

    Write-Host "[MONSTER] Abrindo a mesa no Chromium dedicado ($ChromeExe)..." -ForegroundColor Cyan
    Start-Process -FilePath $ChromeExe -ArgumentList @(
        "--remote-debugging-port=9222",
        "--user-data-dir=$ProfileDir",
        "--no-sandbox",
        "--disable-features=RendererCodeIntegrity",
        "--disable-blink-features=AutomationControlled",
        "--test-type",
        "--no-first-run",
        "--no-default-browser-check",
        "--start-maximized",
        $TargetUrl
    )
    Start-Sleep -Seconds 3
}

if ($Probe) {
    Write-Host "[MONSTER] Sidecar em --ws-probe --strict --dump-frames=3 --dump-op (Ctrl+C encerra)." -ForegroundColor Yellow
    Set-Location "$RootDir\sidecar"
    go run ./cmd/capture --ws-probe --strict --dump-frames=3 --dump-op
    exit 0
}

$HealthAlive = $false
try {
    $null = Invoke-RestMethod -Uri "http://127.0.0.1:8790/health" -TimeoutSec 1
    $HealthAlive = $true
    Write-Host "[MONSTER] Sidecar ja esta vivo na porta 8787 - reaproveitando." -ForegroundColor Green
} catch {
    $HealthAlive = $false
}

if (-not $HealthAlive) {
    Write-Host "[MONSTER] Iniciando sidecar Go em segundo plano (porta 8787)..." -ForegroundColor Green
    Start-Process powershell -ArgumentList @(
        "-NoProfile",
        "-ExecutionPolicy", "Bypass",
        "-Command", "`$env:GOCACHE='$GoCacheDir'; `$env:GOTMPDIR='$GoTmpDir'; Set-Location '$RootDir\sidecar'; go run ./cmd/capture --ws-probe"
    )
    Start-Sleep -Seconds 3
}

# Watchdog 24/48h: grava o modo escolhido (o watchdog reinicia o core NESTE modo) e sobe o
# supervisor em segundo plano. -Probe nao liga watchdog.
if (-not $Probe) {
    $LogsDir = "$RootDir\logs"
    if (-not (Test-Path $LogsDir)) { New-Item -ItemType Directory -Force -Path $LogsDir | Out-Null }
    Set-Content -Path "$LogsDir\monster-mode.txt" -Value $(if ($Live) { "live" } else { "dry-run" })
    Remove-Item "$LogsDir\monster-stop.flag" -ErrorAction SilentlyContinue
    Start-Process powershell -WindowStyle Hidden -ArgumentList @(
        "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", "`"$RootDir\scripts\monster-watch.ps1`""
    ) | Out-Null
    Write-Host "[MONSTER] Watchdog ligado (reinicia Chrome/captura/core e recarrega a mesa travada)." -ForegroundColor Green
    Write-Host "[MONSTER] Para parar tudo de vez: stop-monster.bat" -ForegroundColor Green
}

Write-Host "[MONSTER] Painel: http://127.0.0.1:8791" -ForegroundColor Yellow
Start-Process "http://127.0.0.1:8791"
Set-Location $RootDir
if ($Live) {
    Write-Host "[MONSTER] ATENCAO: -Live ativo, o executor VAI APOSTAR DINHEIRO REAL." -ForegroundColor Red
    cargo run --offline -p monster-core -- --live
} else {
    cargo run --offline -p monster-core -- --dry-run
}
