# SPDX-License-Identifier: Apache-2.0
# Copie para config\paths.ps1 e ajuste para o seu computador (paths.ps1 nao vai para o Git).
# Sem este arquivo, os valores abaixo ja sao o padrao.

# Navegador usado SO pelo MONSTER (Chrome/Chromium). Precisa abrir com depuracao (porta 9222).
$ChromeExe  = "C:\Program Files\Google\Chrome\Application\chrome.exe"

# Perfil dedicado (fica logado no cassino). Desde o Chrome 136 a depuracao exige perfil proprio.
$ProfileDir = Join-Path $RootDir "chrome-profile"

# Mesa operada.
$TargetUrl  = "https://www.jogatudo.com/pt-BR/game/playtech-brasileira.roleta-brasileira"
