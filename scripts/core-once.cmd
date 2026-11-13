@echo off
rem SPDX-License-Identifier: Apache-2.0
rem Sobe o core uma vez; o watchdog reinicia se cair.
rem   core-once.cmd <banca_centavos> <dry-run|live>
rem Sem o segundo argumento = --dry-run (dinheiro real so com "live" explicito, regra 1.5).
setlocal
cd /d "O:\Archives\Eistein\FUI AGENT ROULLET"
set BANK=%~1
if "%BANK%"=="" set BANK=4000
set MODE=--dry-run
if /I "%~2"=="live" set MODE=--live
".\target\debug\monster-core.exe" %MODE% --bank-cents %BANK%>>logs\core-live.log 2>&1
