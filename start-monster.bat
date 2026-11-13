@echo off
REM SPDX-License-Identifier: Apache-2.0
REM Atalho de duplo clique para start-monster.ps1. Repassa os argumentos:
REM   start-monster.bat           -> observa (--dry-run)
REM   start-monster.bat -Live     -> aposta dinheiro real
REM   start-monster.bat -Probe    -> diagnostico de WS, sem core
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0start-monster.ps1" %*
if errorlevel 1 pause
