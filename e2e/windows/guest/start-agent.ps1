# Starts the desktop agent in the interactive session of -User (default: the user
# of this SSH session) through a one-off scheduled task with an Interactive
# principal. -RunLevel Limited gives it (and everything it starts) the unelevated
# token a Start-menu click gets; Highest gives the elevated one (UAC on) for
# scenarios that need it. Prints the agent's pid once it listens.
param(
    [Parameter(Mandatory = $true)][int]$Port,
    [Parameter(Mandatory = $true)][string]$Root,
    [string]$User = "$env:COMPUTERNAME\$env:USERNAME",
    [ValidateSet('Limited', 'Highest')][string]$RunLevel = 'Limited',
    [string]$TaskName = 'superba-wintest-agent',
    [int]$TimeoutSec = 90
)
$ErrorActionPreference = 'Stop'
$tokenFile = Join-Path $Root 'agent.token'
if (-not (Test-Path -LiteralPath $tokenFile)) { throw "token file missing: $tokenFile" }
$log = Join-Path $Root 'agent.log'
Remove-Item -Force -ErrorAction SilentlyContinue $log
# The token file is read and deleted by the agent; let the desktop user read it.
& icacls.exe $Root /grant "*S-1-5-32-545:(OI)(CI)M" /T /Q | Out-Null
$ps = "$env:SystemRoot\System32\WindowsPowerShell\v1.0\powershell.exe"
$arg = "--headless `"$ps`" -NoLogo -NoProfile -ExecutionPolicy Bypass -File `"$Root\agent.ps1`" -Port $Port -TokenFile `"$tokenFile`" -Root `"$Root`""
$action = New-ScheduledTaskAction -Execute "$env:SystemRoot\System32\conhost.exe" -Argument $arg -WorkingDirectory $Root
$principal = New-ScheduledTaskPrincipal -UserId $User -LogonType Interactive -RunLevel $RunLevel
$settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -ExecutionTimeLimit ([TimeSpan]::Zero) -MultipleInstances IgnoreNew
Register-ScheduledTask -TaskName $TaskName -Action $action -Principal $principal -Settings $settings -Force | Out-Null
Start-ScheduledTask -TaskName $TaskName
$deadline = (Get-Date).AddSeconds($TimeoutSec)
while ((Get-Date) -lt $deadline) {
    if (Test-Path -LiteralPath $log) {
        $t = Get-Content -Raw -LiteralPath $log
        if ($t -match 'listening on 127\.0\.0\.1:\d+ pid (\d+)') { "agent pid $($Matches[1])"; exit 0 }
        if ($t -match 'agent failed') { $t; exit 1 }
    }
    Start-Sleep -Milliseconds 300
}
"agent did not start within $TimeoutSec s (task state: $((Get-ScheduledTask -TaskName $TaskName).State))"
if (Test-Path -LiteralPath $log) { Get-Content -LiteralPath $log }
exit 1
