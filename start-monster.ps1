# SPDX-License-Identifier: Apache-2.0
# Iniciador unico do MONSTER. O watchdog (scripts\monster-watch.ps1) e o UNICO supervisor de
# captura e core: este script so compila, abre a mesa, grava o modo e liga o watchdog.
# (Antes ele subia captura/core por conta propria e brigava com o watchdog pela mesma porta.)
#
#   start-monster                  observa a mesa (--dry-run, default, NAO aposta)
#   start-monster -Live            executa de verdade (aposta dinheiro real)
#   start-monster -Probe           so o sidecar em --ws-probe (diagnostico de WS, sem core)
#
# Fechar esta janela NAO para o MONSTER. Para parar: stop-monster.bat

param(
    [switch]$Live,
    [switch]$Probe
)

$ErrorActionPreference = "Stop"
$RootDir     = $PSScriptRoot
# Caminhos desta maquina: config\paths.ps1 (fora do Git). Sem ele, valem os padroes abaixo.
$ChromeExe  = "C:\Program Files\Google\Chrome\Application\chrome.exe"
$ProfileDir = Join-Path $RootDir "chrome-profile"
$TargetUrl  = "https://www.jogatudo.com/pt-BR/game/playtech-brasileira.roleta-brasileira"
$PathsFile  = Join-Path $RootDir "config\paths.ps1"
if (Test-Path $PathsFile) { . $PathsFile }
$LogsDir     = "$RootDir\logs"
$GoCacheDir  = "$RootDir\target\go-cache"
$GoTmpDir    = "$RootDir\target\go-tmp"
$CargoTarget = "$RootDir\target"

# Parada limpa: o kill switch do core bloqueia NOVAS apostas na hora; a que ja estiver em
# andamento termina (cliques + conferencia + confirmacao). So depois os processos sao mortos.
# Antes o core seguia vivo ~8 s no reinicio e foi morto no meio de um gale (R$ 1,60 sem registro).
function Stop-MonsterSafely([string]$reason) {
    $wasUp = $false
    try { $null = Invoke-RestMethod -Method Post -Uri "http://127.0.0.1:8791/api/kill" -TimeoutSec 2; $wasUp = $true } catch { }
    Set-Content -Path "$LogsDir\monster-stop.flag" -Value $reason
    if ($wasUp) { Start-Sleep -Seconds 6 }
}

function Fail([string]$msg) {
    Write-Host ""
    Write-Host "[MONSTER] ERRO: $msg" -ForegroundColor Red
    Write-Host "[MONSTER] Nada foi iniciado. Corrija e rode de novo." -ForegroundColor Red
    Read-Host "Enter para fechar"
    exit 1
}

if ($Probe) { $Live = $false }
foreach ($dir in @($ProfileDir, $GoCacheDir, $GoTmpDir, $CargoTarget, $LogsDir)) {
    if (-not (Test-Path $dir)) { New-Item -ItemType Directory -Force -Path $dir | Out-Null }
}
$env:GOCACHE          = $GoCacheDir
$env:GOTMPDIR         = $GoTmpDir
$env:CARGO_TARGET_DIR = $CargoTarget
$env:RUSTUP_TOOLCHAIN = "stable-x86_64-pc-windows-msvc"

$ModeLabel = if ($Probe) { "WS-PROBE (diagnostico, sem core)" } elseif ($Live) { "LIVE (DINHEIRO REAL)" } else { "DRY-RUN (observa, nao aposta)" }
Write-Host "================================================================" -ForegroundColor Cyan
Write-Host " MONSTER - Roleta Brasileira" -ForegroundColor Cyan
Write-Host " Modo: $ModeLabel" -ForegroundColor $(if ($Live) { "Red" } else { "Cyan" })
Write-Host "================================================================" -ForegroundColor Cyan
if (-not (Test-Path $ChromeExe)) { Fail "Chrome nao encontrado em $ChromeExe. Ajuste `$ChromeExe em config\paths.ps1 (modelo: config\paths.example.ps1)." }

# 1. Para o que estiver rodando (watchdog, core, captura), para nunca existirem dois cores.
Write-Host "[MONSTER] Parando instancias anteriores (sem cortar aposta em andamento)..." -ForegroundColor DarkGray
Stop-MonsterSafely "restart"
# Janelas antigas do MONSTER LIVE (so mostravam o log): fecha para nao acumular. Matar o
# processo nao passa pelo finally do Ctrl+C, entao nada alem da janela e afetado.
Get-CimInstance Win32_Process -ErrorAction SilentlyContinue |
    Where-Object { $_.Name -eq 'powershell.exe' -and $_.ProcessId -ne $PID -and $_.CommandLine -like '*start-monster.ps1*' } |
    ForEach-Object {
        $parent = $_.ParentProcessId
        Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue
        $pp = Get-CimInstance Win32_Process -Filter "ProcessId=$parent" -ErrorAction SilentlyContinue
        if ($pp -and $pp.Name -eq 'cmd.exe' -and $pp.CommandLine -like '*start-monster*') { Stop-Process -Id $parent -Force -ErrorAction SilentlyContinue }
    }
$deadline = (Get-Date).AddSeconds(15)
while ((Test-Path "$LogsDir\watchdog.lock") -and (Get-Date) -lt $deadline) {
    $wpid = Get-Content "$LogsDir\watchdog.lock" -ErrorAction SilentlyContinue
    if (-not ($wpid -and (Get-Process -Id $wpid -ErrorAction SilentlyContinue))) { break }
    Start-Sleep -Milliseconds 500
}
Get-Process monster-core, monster-capture -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
# Captura do iniciador antigo ("go run ./cmd/capture" vira capture.exe no go-cache): fora do watchdog.
Get-CimInstance Win32_Process -ErrorAction SilentlyContinue |
    Where-Object { ($_.Name -eq 'capture.exe' -and $_.ExecutablePath -like "$GoCacheDir*") -or
                   ($_.Name -eq 'go.exe' -and $_.CommandLine -like '*cmd/capture*') } |
    ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }
Start-Sleep -Seconds 1
if (Get-Process monster-core -ErrorAction SilentlyContinue) { Fail "nao consegui encerrar o monster-core anterior" }
Remove-Item "$LogsDir\watchdog.lock" -ErrorAction SilentlyContinue

# 2. Compila core e captura ANTES de abrir qualquer coisa: erro de build aparece aqui e para.
Set-Location $RootDir
Write-Host "[MONSTER] Compilando o core (Rust)..." -ForegroundColor Cyan
$ErrorActionPreference = "Continue"
cargo build -p monster-core
if ($LASTEXITCODE -ne 0) { Fail "cargo build do monster-core falhou (veja as mensagens acima)" }
Write-Host "[MONSTER] Compilando a captura (Go)..." -ForegroundColor Cyan
Push-Location "$RootDir\sidecar"
go build -o "$CargoTarget\monster-capture.exe" ./cmd/capture
$goExit = $LASTEXITCODE
if ($goExit -eq 0) { go build -o "$CargoTarget\monster-telegram.exe" ./telegram; $goExit = $LASTEXITCODE }
Pop-Location
if ($goExit -ne 0) { Fail "go build da captura falhou (veja as mensagens acima)" }
$ErrorActionPreference = "Stop"

# 3. Chrome dedicado com CDP.
$CdpAlive = $false
try {
    $null = Invoke-RestMethod -Uri "http://127.0.0.1:9222/json/version" -TimeoutSec 2
    $CdpAlive = $true
    Write-Host "[MONSTER] Chromium dedicado ja esta na porta 9222 - reaproveitando a sessao logada." -ForegroundColor Green
} catch { $CdpAlive = $false }
if (-not $CdpAlive) {
    Remove-Item -Force -ErrorAction SilentlyContinue (Join-Path $ProfileDir "SingletonLock")
    Write-Host "[MONSTER] Abrindo a mesa no Chromium dedicado..." -ForegroundColor Cyan
    Start-Process -FilePath $ChromeExe -ArgumentList @(
        "--remote-debugging-port=9222", "--user-data-dir=$ProfileDir", "--no-sandbox",
        "--disable-features=RendererCodeIntegrity", "--disable-blink-features=AutomationControlled",
        "--test-type", "--no-first-run", "--no-default-browser-check", "--start-maximized", $TargetUrl
    )
    Start-Sleep -Seconds 3
}

if ($Probe) {
    Write-Host "[MONSTER] Sidecar em --ws-probe --strict --dump-frames=3 --dump-op (Ctrl+C encerra)." -ForegroundColor Yellow
    Remove-Item "$LogsDir\monster-stop.flag" -ErrorAction SilentlyContinue
    & "$CargoTarget\monster-capture.exe" --ws-probe --strict --dump-frames=3 --dump-op
    exit 0
}

# 4. Grava o modo e liga o watchdog: ele sobe captura e core (e os reinicia se cairem).
Set-Content -Path "$LogsDir\monster-mode.txt" -Value $(if ($Live) { "live" } else { "dry-run" })
Remove-Item "$LogsDir\monster-stop.flag" -ErrorAction SilentlyContinue
# Pela tarefa agendada (quando registrada) o watchdog nasce fora da arvore desta janela:
# fechar/matar este terminal nunca leva o watchdog e o core junto.
$task = Get-ScheduledTask -TaskName 'MONSTER Watchdog' -ErrorAction SilentlyContinue
if ($task) {
    Start-ScheduledTask -TaskName 'MONSTER Watchdog'
} else {
    Start-Process powershell -WindowStyle Hidden -ArgumentList @(
        "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", "`"$RootDir\scripts\monster-watch.ps1`""
    ) | Out-Null
}
if ($Live) { Write-Host "[MONSTER] ATENCAO: -Live ativo, o executor VAI APOSTAR DINHEIRO REAL." -ForegroundColor Red }

# 5. Espera o core responder (o watchdog leva alguns segundos para subir captura -> core).
Write-Host "[MONSTER] Aguardando captura e core..." -ForegroundColor Cyan
$deadline = (Get-Date).AddSeconds(90)
$ok = $false
while ((Get-Date) -lt $deadline) {
    try { $null = Invoke-RestMethod -Uri "http://127.0.0.1:8791/api/tick" -TimeoutSec 2; $ok = $true; break } catch { Start-Sleep -Seconds 2 }
}
if (-not $ok) {
    Write-Host "[MONSTER] O core nao respondeu em 90 s. Ultimas linhas:" -ForegroundColor Red
    Get-Content "$LogsDir\watchdog.log" -Tail 8 -ErrorAction SilentlyContinue
    Get-Content "$LogsDir\core-live.log" -Tail 15 -ErrorAction SilentlyContinue
    Read-Host "Enter para fechar (o watchdog continua tentando em segundo plano)"
    exit 1
}
Write-Host "[MONSTER] Tudo no ar. Painel: http://127.0.0.1:8791" -ForegroundColor Green
Write-Host "[MONSTER] Fechar esta janela NAO para o MONSTER. Para parar: stop-monster.bat" -ForegroundColor Green
Start-Process "http://127.0.0.1:8791"

# 6. Mostra o log do core ao vivo (so leitura).
Write-Host "[MONSTER] Ctrl+C aqui PARA o MONSTER (watchdog, core, captura). Fechar no X mantem rodando." -ForegroundColor Yellow
Write-Host "---------------------------- log do core ----------------------------" -ForegroundColor DarkGray
$stopped = $false
try {
    Get-Content "$LogsDir\core-live.log" -Tail 20 -Wait
} finally {
    # Ctrl+C interrompe o Get-Content e cai aqui: para tudo (o Chrome fica, sessao logada preservada).
    # Fechar a janela no X mata o processo sem passar por aqui: o MONSTER continua rodando.
    if (-not $stopped) {
        $stopped = $true
        Write-Host "[MONSTER] Parando sem cortar aposta em andamento..." -ForegroundColor Yellow
        Stop-MonsterSafely "ctrl-c"
        Get-Process monster-core, monster-capture, monster-telegram -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
        Write-Host ""
        Write-Host "[MONSTER] Parado por Ctrl+C. Para voltar: atalho MONSTER LIVE." -ForegroundColor Yellow
    }
}
