@echo off
rem SPDX-License-Identifier: Apache-2.0
rem Sobe a captura uma vez; o watchdog (monster-watch.ps1) reinicia se cair.
cd /d "%~dp0.."
".\target\monster-capture.exe">>logs\capture.log 2>&1
