@echo off
rem SPDX-License-Identifier: Apache-2.0
rem Sobe o sidecar de Telegram uma vez; o watchdog reinicia se cair.
cd /d "%~dp0.."
".\target\monster-telegram.exe">>logs\telegram.log 2>&1
