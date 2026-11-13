# SPDX-License-Identifier: Apache-2.0
# Fecha SO o Chrome do perfil dedicado do MONSTER (nunca o navegador pessoal).
# O login fica salvo no perfil: o proximo MONSTER LIVE abre ja logado.
$RootDir    = Split-Path -Parent $PSScriptRoot
$ProfileDir = Join-Path $RootDir "chrome-profile"
$PathsFile  = Join-Path $RootDir "config\paths.ps1"
if (Test-Path $PathsFile) { . $PathsFile }
$n = 0
Get-CimInstance Win32_Process -Filter "Name='chrome.exe'" -ErrorAction SilentlyContinue |
    Where-Object { $_.CommandLine -and $_.CommandLine -like "*$ProfileDir*" } |
    ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue; $n++ }
if ($n -gt 0) { Write-Host "[MONSTER] Chrome do MONSTER fechado ($n processos)." }
