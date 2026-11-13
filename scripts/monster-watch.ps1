# SPDX-License-Identifier: Apache-2.0
# Watchdog 24/48h do MONSTER: mantem vivo o Chrome dedicado (CDP 9222), a captura (8787),
# o core (8791) e o Telegram, e detecta mesa TRAVADA (processo vivo, giros parados).
# Nao clica na mesa. Para parar de vez: crie logs\monster-stop.flag.

$ErrorActionPreference = 'Continue'
$root = 'O:\Archives\Eistein\FUI AGENT ROULLET'
Set-Location $root

$log      = Join-Path $root 'logs\watchdog.log'
$lock     = Join-Path $root 'logs\watchdog.lock'
$stopFlag = Join-Path $root 'logs\monster-stop.flag'
$modeFile = Join-Path $root 'logs\monster-mode.txt'
$chrome   = 'O:\Archives\Eistein\chromium\chrome.exe'
$profile  = 'O:\Archives\Eistein\chromium-profile'
$url      = 'https://www.jogatudo.com/pt-BR/game/playtech-brasileira.roleta-brasileira'

# Mesa gira a cada ~45-60 s. Sem giro novo por STALE_MIN minutos = mesa travada.
$STALE_MIN        = 5
# Operador observou: quando a sessao da mesa cai, so fechar e reabrir o navegador resolve.
# Recarregar a aba nao basta, entao vamos direto ao reinicio do Chrome dedicado.
$MAX_TAB_RELOADS  = 0
$BACKOFF_MAX_S    = 120

function Write-Log([string]$msg) {
    $line = '{0} {1}' -f (Get-Date -Format 'yyyy-MM-dd HH:mm:ss'), $msg
    Add-Content -Path $log -Value $line
    $fi = Get-Item $log -ErrorAction SilentlyContinue
    if ($fi -and $fi.Length -gt 5MB) { Move-Item $log "$log.1" -Force }
}

function Test-Port([int]$port) {
    $client = New-Object System.Net.Sockets.TcpClient
    try {
        $iar = $client.BeginConnect('127.0.0.1', $port, $null, $null)
        if (-not $iar.AsyncWaitHandle.WaitOne(800)) { return $false }
        $client.EndConnect($iar)
        return $true
    } catch { return $false } finally { $client.Close() }
}

function Get-Mode {
    # Dinheiro real so com 'live' gravado explicitamente pelo start-monster -Live (§1.5).
    $m = (Get-Content $modeFile -ErrorAction SilentlyContinue | Select-Object -First 1)
    if ($m -and $m.Trim() -eq 'live') { return 'live' }
    return 'dry-run'
}

# Mantem o PC acordado enquanto o watchdog roda (sem mexer em configuracao de energia).
Add-Type -Namespace Win32 -Name Power -MemberDefinition '[DllImport("kernel32.dll")] public static extern uint SetThreadExecutionState(uint esFlags);' -ErrorAction SilentlyContinue
function Stay-Awake { try { [void][Win32.Power]::SetThreadExecutionState([uint32]'0x80000001') } catch { } }  # CONTINUOUS|SYSTEM_REQUIRED

# Backoff por componente: 5 s, 10 s, 20 s ... ate 120 s; zera quando o componente fica de pe.
$backoff = @{}; $nextTry = @{}
function Can-Try([string]$k) { -not $nextTry.ContainsKey($k) -or (Get-Date) -ge $nextTry[$k] }
function Failed([string]$k) {
    $s = if ($backoff.ContainsKey($k)) { [Math]::Min($backoff[$k] * 2, $BACKOFF_MAX_S) } else { 5 }
    $backoff[$k] = $s; $nextTry[$k] = (Get-Date).AddSeconds($s)
    return $s
}
function Healthy([string]$k) { $backoff.Remove($k); $nextTry.Remove($k) }

function Start-Component([string]$script, [string]$arguments) {
    $file = Join-Path $root ('scripts\' + $script)
    if ($arguments) {
        Start-Process -FilePath $file -ArgumentList $arguments -WorkingDirectory $root -WindowStyle Hidden | Out-Null
    } else {
        Start-Process -FilePath $file -WorkingDirectory $root -WindowStyle Hidden | Out-Null
    }
}

# So o Chrome do perfil dedicado: nunca derruba o navegador pessoal do usuario.
function Get-DedicatedChrome {
    Get-CimInstance Win32_Process -Filter "Name='chrome.exe'" -ErrorAction SilentlyContinue |
        Where-Object { $_.CommandLine -and $_.CommandLine -like "*$profile*" }
}

function Start-DedicatedChrome {
    Start-Process -FilePath $chrome -ArgumentList @(
        '--remote-debugging-port=9222', "--user-data-dir=$profile", '--no-sandbox',
        '--disable-blink-features=AutomationControlled', '--test-type',
        '--disable-background-timer-throttling', '--disable-renderer-backgrounding',
        '--disable-backgrounding-occluded-windows', $url
    ) | Out-Null
}

# Recarrega SO a aba da roleta via CDP HTTP (fecha e reabre), sem matar o Chrome.
function Reload-TableTab {
    try {
        $targets = Invoke-RestMethod -Uri 'http://127.0.0.1:9222/json' -TimeoutSec 3
        $pages = @($targets | Where-Object { $_.type -eq 'page' -and $_.url -like '*jogatudo*' })
        Invoke-RestMethod -Method Put -Uri ('http://127.0.0.1:9222/json/new?' + $url) -TimeoutSec 5 | Out-Null
        foreach ($p in $pages) { Invoke-RestMethod -Uri ('http://127.0.0.1:9222/json/close/' + $p.id) -TimeoutSec 3 | Out-Null }
        return $true
    } catch { Write-Log "falha ao recarregar aba via CDP: $_"; return $false }
}

function Get-LastSpinAgeMin {
    try {
        $t = Invoke-RestMethod -Uri 'http://127.0.0.1:8791/api/tick' -TimeoutSec 3
        if (-not $t.spins -or $t.spins.Count -eq 0) { return $null }
        $ms = [int64]$t.spins[0].ts
        $now = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
        return [Math]::Round(($now - $ms) / 60000.0, 1)
    } catch { return $null }
}

if (Test-Path $lock) {
    $oldPid = Get-Content $lock -ErrorAction SilentlyContinue
    if ($oldPid -and (Get-Process -Id $oldPid -ErrorAction SilentlyContinue)) { exit }
}
$PID | Set-Content $lock
Write-Log "watchdog iniciado (pid $PID, modo $(Get-Mode))"

$lastChromeWarn = (Get-Date).AddMinutes(-10)
$tabReloads = 0
$staleRestarts = 0
$staleGraceUntil = (Get-Date).AddMinutes($STALE_MIN)
$loop = 0
while ($true) {
    Stay-Awake
    if (Test-Path $stopFlag) {
        Write-Log 'stop flag encontrado - watchdog encerrando'
        Remove-Item $lock -ErrorAction SilentlyContinue
        exit
    }

    # 1. Chrome dedicado com CDP
    if (-not (Test-Port 9222)) {
        if (Get-DedicatedChrome) {
            if (((Get-Date) - $lastChromeWarn).TotalMinutes -ge 5) {
                Write-Log 'Chrome dedicado vivo sem CDP 9222 - reiniciando o Chrome dedicado'
                $lastChromeWarn = Get-Date
                Get-DedicatedChrome | ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }
            }
        } elseif (Can-Try 'chrome') {
            $s = Failed 'chrome'
            Write-Log "Chrome fora do ar - abrindo com CDP 9222 (proxima tentativa em ${s}s)"
            Start-DedicatedChrome
            $staleGraceUntil = (Get-Date).AddMinutes($STALE_MIN)
        }
        Start-Sleep -Seconds 5; continue
    }
    Healthy 'chrome'

    # 2. Captura
    if (-not (Test-Port 8787)) {
        if (Can-Try 'capture') {
            $s = Failed 'capture'
            Write-Log "captura fora do ar - reiniciando monster-capture (proxima tentativa em ${s}s)"
            Start-Component 'capture-once.cmd' ''
        }
        Start-Sleep -Seconds 5; continue
    }
    Healthy 'capture'

    # 3. Core, no modo que o operador escolheu (dry-run se nada foi gravado)
    if (-not (Test-Port 8791)) {
        if (Can-Try 'core') {
            $bank = 4000
            try {
                $p = Invoke-RestMethod -Uri 'http://127.0.0.1:8787/act/preview' -TimeoutSec 3
                if ($p.balance_cents -and [int]$p.balance_cents -gt 0) { $bank = [int]$p.balance_cents }
            } catch { }
            $mode = Get-Mode
            $s = Failed 'core'
            Write-Log "core fora do ar - subindo em $mode com banca ${bank}c (proxima tentativa em ${s}s)"
            Start-Component 'core-once.cmd' "$bank $mode"
            $staleGraceUntil = (Get-Date).AddMinutes($STALE_MIN)
        }
        Start-Sleep -Seconds 5; continue
    }
    Healthy 'core'

    # 4. Mesa travada: tudo de pe, mas sem giro novo ha STALE_MIN minutos
    if ((Get-Date) -ge $staleGraceUntil) {
        $age = Get-LastSpinAgeMin
        if ($age -ne $null -and $age -ge $STALE_MIN) {
            if ($tabReloads -lt $MAX_TAB_RELOADS) {
                $tabReloads++
                Write-Log "mesa parada ha $age min - recarregando a aba da roleta ($tabReloads/$MAX_TAB_RELOADS)"
                if (Reload-TableTab) {
                    # captura re-anexa no iframe novo; reinicia para garantir observer/WS limpos
                    Get-Process monster-capture -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
                }
            } else {
                Write-Log "mesa parada ha $age min - fechando e reabrindo o Chrome dedicado + captura (reinicio $($staleRestarts + 1))"
                Get-DedicatedChrome | ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }
                Get-Process monster-capture -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
                Start-Sleep -Seconds 3
                Remove-Item (Join-Path $profile 'SingletonLock') -Force -ErrorAction SilentlyContinue
                $tabReloads = 0
                $staleRestarts++
            }
            # Mesa real so para em manutencao (aviso na tela + dealers): se reiniciar nao trouxe giro,
            # espera cada vez mais (5 -> 10 -> 20 -> 30 min) em vez de reiniciar a cada 5 min.
            $graceMin = @(5, 10, 20, 30)[[Math]::Min($staleRestarts, 3)]
            $staleGraceUntil = (Get-Date).AddMinutes($graceMin)
            if ($staleRestarts -gt 1) { Write-Log "sem giro apos reinicio - provavel manutencao da mesa; proxima checagem em $graceMin min" }
        } elseif ($age -ne $null -and $age -lt 2) {
            if ($tabReloads -gt 0 -or $staleRestarts -gt 0) { Write-Log "mesa voltou a girar (ultimo giro ha $age min)" }
            $tabReloads = 0
            $staleRestarts = 0
        }
    }

    # 5. Telegram (opcional)
    if (-not (Get-Process monster-telegram -ErrorAction SilentlyContinue) -and (Can-Try 'telegram')) {
        $s = Failed 'telegram'
        Write-Log "telegram fora do ar - reiniciando (proxima tentativa em ${s}s)"
        Start-Component 'telegram-once.cmd' ''
    } elseif (Get-Process monster-telegram -ErrorAction SilentlyContinue) { Healthy 'telegram' }

    $loop++
    if ($loop % 60 -eq 0) {
        $age = Get-LastSpinAgeMin
        Write-Log "heartbeat: tudo no ar · modo $(Get-Mode) · ultimo giro ha $age min"
    }
    Start-Sleep -Seconds 10
}
