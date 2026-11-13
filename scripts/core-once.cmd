@echo off
rem SPDX-License-Identifier: Apache-2.0
rem Sobe o core uma vez com a banca recebida; o watchdog reinicia se cair.
setlocal
cd /d "O:\Archives\Eistein\FUI AGENT ROULLET"
set BANK=%~1
if "%BANK%"=="" set BANK=5000
".\target\debug\monster-core.exe" --live --bank-cents %BANK%>>logs\core-live.log 2>&1
