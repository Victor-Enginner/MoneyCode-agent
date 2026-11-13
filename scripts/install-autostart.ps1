# SPDX-License-Identifier: Apache-2.0
# Registra o watchdog do MONSTER no Agendador de Tarefas do usuario (sem admin):
# sobe no logon e, se o proprio watchdog morrer, o Windows o reinicia a cada 1 min.
# Remover:  Unregister-ScheduledTask -TaskName 'MONSTER Watchdog' -Confirm:$false
$root = 'O:\Archives\Eistein\FUI AGENT ROULLET'
# conhost --headless: o watchdog roda SEM janela. Com janela, fechar o console preto matava o
# watchdog (codigo 0xC000013A) sem nenhum log.
$action = New-ScheduledTaskAction -Execute 'conhost.exe' `
    -Argument "--headless powershell.exe -NoProfile -ExecutionPolicy Bypass -File `"$root\scripts\monster-watch.ps1`"" `
    -WorkingDirectory $root
$trigger = New-ScheduledTaskTrigger -AtLogOn -User $env:USERNAME
# Rechecagem a cada 5 min: se o watchdog morrer (console fechado, crash), o Windows o religa.
# O lock do proprio watchdog faz a instancia extra sair na hora.
$trigger.Repetition = (New-ScheduledTaskTrigger -Once -At (Get-Date) -RepetitionInterval (New-TimeSpan -Minutes 5)).Repetition
$settings = New-ScheduledTaskSettingsSet -RestartCount 999 -RestartInterval (New-TimeSpan -Minutes 1) `
    -ExecutionTimeLimit ([TimeSpan]::Zero) -MultipleInstances IgnoreNew -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -StartWhenAvailable
Register-ScheduledTask -TaskName 'MONSTER Watchdog' -Action $action -Trigger $trigger -Settings $settings `
    -Description 'Mantem Chrome/captura/core/Telegram do MONSTER vivos 24/48h' -Force | Out-Null
Write-Host "[MONSTER] tarefa 'MONSTER Watchdog' registrada: sobe no logon e se reinicia se cair."
Write-Host "[MONSTER] o modo (dry-run/live) e o ultimo escolhido no start-monster (logs\monster-mode.txt)."
