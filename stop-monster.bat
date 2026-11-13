@echo off
REM SPDX-License-Identifier: Apache-2.0
REM Para o MONSTER de vez: avisa o watchdog (stop flag) e encerra core, captura e Telegram.
REM O Chrome dedicado fica aberto (a sessao logada e preservada).
cd /d "%~dp0"
if not exist logs mkdir logs
echo stop> logs\monster-stop.flag
taskkill /IM monster-core.exe /F >nul 2>&1
taskkill /IM monster-capture.exe /F >nul 2>&1
taskkill /IM monster-telegram.exe /F >nul 2>&1
echo [MONSTER] parado. Watchdog encerra em ate 10 s. Para voltar: start-monster.bat
