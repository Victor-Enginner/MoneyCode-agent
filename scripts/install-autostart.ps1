# SPDX-License-Identifier: Apache-2.0
# Registra o watchdog do MONSTER no Agendador de Tarefas do usuario (sem admin):
# sobe no logon e, se o proprio watchdog morrer, o Windows o reinicia a cada 1 min.
# Remover:  Unregister-ScheduledTask -TaskName 'MONSTER Watchdog' -Confirm:$false
$root = 'O:\Archives\Eistein\FUI AGENT ROULLET'
$action = New-ScheduledTaskAction -Execute 'powershell.exe' `
    -Argument "-NoProfile -WindowStyle Hidden -ExecutionPolicy Bypass -File `"$root\scripts\monster-watch.ps1`"" `
    -WorkingDirectory $root
$trigger = New-ScheduledTaskTrigger -AtLogOn -User $env:USERNAME
$settings = New-ScheduledTaskSettingsSet -RestartCount 999 -RestartInterval (New-TimeSpan -Minutes 1) `
    -ExecutionTimeLimit ([TimeSpan]::Zero) -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -StartWhenAvailable
Register-ScheduledTask -TaskName 'MONSTER Watchdog' -Action $action -Trigger $trigger -Settings $settings `
    -Description 'Mantem Chrome/captura/core/Telegram do MONSTER vivos 24/48h' -Force | Out-Null
Write-Host "[MONSTER] tarefa 'MONSTER Watchdog' registrada: sobe no logon e se reinicia se cair."
Write-Host "[MONSTER] o modo (dry-run/live) e o ultimo escolhido no start-monster (logs\monster-mode.txt)."
