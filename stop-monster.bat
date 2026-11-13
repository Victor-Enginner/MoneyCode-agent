@echo off
REM SPDX-License-Identifier: Apache-2.0
REM Para o MONSTER de vez, sem cortar aposta em andamento: aciona o kill switch do core
REM (bloqueia novas apostas), espera 6 s a aposta corrente terminar e encerra os processos.
REM O Chrome dedicado fica aberto (a sessao logada e preservada).
cd /d "%~dp0"
if not exist logs mkdir logs
powershell -NoProfile -Command "try { Invoke-RestMethod -Method Post -Uri 'http://127.0.0.1:8791/api/kill' -TimeoutSec 2 | Out-Null; Start-Sleep -Seconds 6 } catch { }"
echo stop> logs\monster-stop.flag
taskkill /IM monster-core.exe /F >nul 2>&1
taskkill /IM monster-capture.exe /F >nul 2>&1
taskkill /IM monster-telegram.exe /F >nul 2>&1
echo [MONSTER] parado. Para voltar: atalho MONSTER LIVE
