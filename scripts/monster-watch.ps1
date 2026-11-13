# SPDX-License-Identifier: Apache-2.0
# Watchdog do MONSTER: mantem vivo o Chrome com CDP 9222, a captura (8787),
# o core (8791) e o sidecar de Telegram. Nao clica na mesa, so sobe processo.
# Para parar de vez: crie logs\monster-stop.flag.

$ErrorActionPreference = 'Continue'
$root = 'O:\Archives\Eistein\FUI AGENT ROULLET'
Set-Location $root

$log = Join-Path $root 'logs\watchdog.log'
$lock = Join-Path $root 'logs\watchdog.lock'
$stopFlag = Join-Path $root 'logs\monster-stop.flag'
$chrome = 'O:\Archives\Eistein\chromium\chrome.exe'
$profile = 'O:\Archives\Eistein\chromium-profile'
$url = 'https://www.jogatudo.com/pt-BR/game/playtech-brasileira.roleta-brasileira'

function Write-Log([string]$msg) {
    $line = '{0} {1}' -f (Get-Date -Format 'yyyy-MM-dd HH:mm:ss'), $msg
    Add-Content -Path $log -Value $line
}

function Test-Port([int]$port) {
    $client = New-Object System.Net.Sockets.TcpClient
    try {
        $iar = $client.BeginConnect('127.0.0.1', $port, $null, $null)
        if (-not $iar.AsyncWaitHandle.WaitOne(800)) { return $false }
        $client.EndConnect($iar)
        return $true
    } catch {
        return $false
    } finally {
        $client.Close()
    }
}

# Cada componente tem um .cmd que sobe uma unica vez: o laco de reinicio e este
# watchdog, para nao existirem dois supervisores brigando pelo mesmo processo.
function Start-Component([string]$script, [string]$arguments) {
    $file = Join-Path $root ('scripts\' + $script)
    if ($arguments) {
        Start-Process -FilePath $file -ArgumentList $arguments -WorkingDirectory $root -WindowStyle Hidden | Out-Null
    } else {
        Start-Process -FilePath $file -WorkingDirectory $root -WindowStyle Hidden | Out-Null
    }
}

if (Test-Path $lock) {
    $oldPid = Get-Content $lock -ErrorAction SilentlyContinue
    if ($oldPid -and (Get-Process -Id $oldPid -ErrorAction SilentlyContinue)) { exit }
}
$PID | Set-Content $lock
Write-Log "watchdog iniciado (pid $PID)"

$lastChromeWarn = Get-Date
$loop = 0
while ($true) {
    if (Test-Path $stopFlag) {
        Write-Log 'stop flag encontrado - watchdog encerrando'
        Remove-Item $lock -ErrorAction SilentlyContinue
        exit
    }

    if (-not (Test-Port 9222)) {
        $chromeProc = Get-Process chrome -ErrorAction SilentlyContinue
        if ($chromeProc) {
            if (((Get-Date) - $lastChromeWarn).TotalMinutes -ge 5) {
                Write-Log 'Chrome rodando sem CDP 9222 - feche a janela do navegador para reabrir com depuracao'
                $lastChromeWarn = Get-Date
            }
        } else {
            Write-Log 'Chrome fora do ar - abrindo com CDP 9222'
            Start-Process -FilePath $chrome -ArgumentList @(
                '--remote-debugging-port=9222',
                "--user-data-dir=$profile",
                '--no-sandbox',
                '--disable-blink-features=AutomationControlled',
                '--test-type',
                $url
            ) | Out-Null
            Start-Sleep -Seconds 10
            continue
        }
    }

    if (-not (Test-Port 8787)) {
        Write-Log 'captura fora do ar - reiniciando monster-capture'
        Start-Component 'capture-once.cmd' ''
        Start-Sleep -Seconds 6
        continue
    }

    if (-not (Test-Port 8791)) {
        $bank = 5000
        try {
            $p = Invoke-RestMethod -Uri 'http://127.0.0.1:8787/act/preview' -TimeoutSec 3
            if ($p.balance_cents -and [int]$p.balance_cents -gt 0) { $bank = [int]$p.balance_cents }
        } catch { }
        Write-Log "core fora do ar - subindo com banca ${bank}c"
        Start-Component 'core-once.cmd' "$bank"
        Start-Sleep -Seconds 8
        continue
    }

    if (-not (Get-Process monster-telegram -ErrorAction SilentlyContinue)) {
        Write-Log 'telegram fora do ar - reiniciando'
        Start-Component 'telegram-once.cmd' ''
    }

    $loop++
    if ($loop % 60 -eq 0) {
        Write-Log 'heartbeat: chrome/captura/core/telegram no ar'
    }
    Start-Sleep -Seconds 10
}
