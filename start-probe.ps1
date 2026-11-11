# SPDX-License-Identifier: Apache-2.0
# Abre direto a mesa da Roleta Brasileira na jogatudo.com com perfil no disco O:
# e inicia o sidecar Go em modo --ws-probe (reaproveita o Chrome se a porta 9222 já estiver aberta).

$ErrorActionPreference = "Stop"
$TargetUrl = "https://www.jogatudo.com/pt-BR/game/playtech-brasileira.roleta-brasileira"
$ProfileDir = "O:\Archives\Eistein\roulette-profile"
$GoCacheDir = "O:\Archives\Eistein\FUI AGENT ROULLET\target\go-cache"
$GoTmpDir   = "O:\Archives\Eistein\FUI AGENT ROULLET\target\go-tmp"

foreach ($dir in @($ProfileDir, $GoCacheDir, $GoTmpDir)) {
    if (-not (Test-Path $dir)) {
        New-Item -ItemType Directory -Force -Path $dir | Out-Null
    }
}

$env:GOCACHE  = $GoCacheDir
$env:GOTMPDIR = $GoTmpDir

Write-Host "================================================================" -ForegroundColor Cyan
Write-Host " MONSTER WS-PROBE - Roleta Brasileira (jogatudo.com)" -ForegroundColor Cyan
Write-Host " Perfil salvo em: $ProfileDir" -ForegroundColor Cyan
Write-Host "================================================================" -ForegroundColor Cyan

$CdpAlive = $false
try {
    $null = Invoke-RestMethod -Uri "http://127.0.0.1:9222/json/version" -TimeoutSec 1
    $CdpAlive = $true
} catch {
    $CdpAlive = $false
}

if ($CdpAlive) {
    Write-Host "[MONSTER] Chrome já está aberto na porta 9222 — conectando direto na mesa aberta!" -ForegroundColor Green
} else {
    $SingletonLock = Join-Path $ProfileDir "SingletonLock"
    if (Test-Path $SingletonLock) {
        Remove-Item -Force -ErrorAction SilentlyContinue $SingletonLock
    }

    $ChromeCandidates = @(
        "C:\Program Files\Google\Chrome\Application\chrome.exe",
        "C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
        "$env:LOCALAPPDATA\Google\Chrome\Application\chrome.exe"
    )
    $ChromeExe = $ChromeCandidates | Where-Object { Test-Path $_ } | Select-Object -First 1
    if (-not $ChromeExe) {
        Write-Error "Google Chrome nao encontrado nos caminhos padrao."
    }

    Start-Process -FilePath $ChromeExe -ArgumentList @(
        "--remote-debugging-port=9222",
        "--user-data-dir=$ProfileDir",
        "--disable-blink-features=AutomationControlled",
        "--start-maximized",
        $TargetUrl
    )
    Start-Sleep -Seconds 3
}

Write-Host "[MONSTER] Conectando sidecar ao Chrome (porta 9222)..." -ForegroundColor Green
Write-Host "[MONSTER] Modos ativos: --ws-probe --strict --dump-frames=3 --dump-op" -ForegroundColor Yellow
Set-Location "$PSScriptRoot\sidecar"
go run ./cmd/capture --ws-probe --strict --dump-frames=3 --dump-op
