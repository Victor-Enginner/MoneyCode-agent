@echo off
rem SPDX-License-Identifier: Apache-2.0
rem Supervisor do sidecar de Telegram: reinicia em 5s se o processo morrer.
rem Sobe no logon pela pasta Startup do usuario; se o processo morrer, volta em 5s.
setlocal
cd /d "O:\Archives\Eistein\FUI AGENT ROULLET"
:loop
echo [service] %DATE% %TIME% iniciando monster-telegram>>logs\telegram.log
".\target\monster-telegram.exe">>logs\telegram.log 2>&1
echo [service] %DATE% %TIME% saiu (codigo %ERRORLEVEL%), reiniciando em 5s>>logs\telegram.log
timeout /t 5 /nobreak >nul
goto loop
