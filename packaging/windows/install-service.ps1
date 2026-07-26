#Requires -RunAsAdministrator
[CmdletBinding()]
param(
  [Parameter(Mandatory = $true)]
  [ValidateScript({ Test-Path -LiteralPath $_ -PathType Leaf })]
  [string]$BinaryPath,

  [Parameter(Mandatory = $true)]
  [ValidateScript({ Test-Path -LiteralPath $_ -PathType Leaf })]
  [string]$ConfigPath
)

$ErrorActionPreference = "Stop"
$taskName = "nanoctl Agent"
$resolvedBinary = (Resolve-Path -LiteralPath $BinaryPath).Path
$resolvedConfig = (Resolve-Path -LiteralPath $ConfigPath).Path
$currentUser = [Security.Principal.WindowsIdentity]::GetCurrent().Name

if (Get-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue) {
  throw "The nanoctl agent is already installed. Uninstall or update it explicitly."
}

# Capture and input must execute in the interactive user's session. A LocalSystem service runs in
# Session 0 and cannot safely use that desktop or the enrolling user's Credential Manager entry.
$action = New-ScheduledTaskAction `
  -Execute $resolvedBinary `
  -Argument ('--config "{0}" run' -f $resolvedConfig)
$trigger = New-ScheduledTaskTrigger -AtLogOn -User $currentUser
$principal = New-ScheduledTaskPrincipal `
  -UserId $currentUser `
  -LogonType Interactive `
  -RunLevel Highest
$settings = New-ScheduledTaskSettingsSet `
  -AllowStartIfOnBatteries `
  -DontStopIfGoingOnBatteries `
  -ExecutionTimeLimit ([TimeSpan]::Zero) `
  -RestartCount 3 `
  -RestartInterval (New-TimeSpan -Minutes 1)

Register-ScheduledTask `
  -TaskName $taskName `
  -Action $action `
  -Trigger $trigger `
  -Principal $principal `
  -Settings $settings `
  -Description "Headless device-owner-authorized nanoctl remote desktop agent." | Out-Null
Start-ScheduledTask -TaskName $taskName
Write-Host "nanoctl background agent installed and started for $currentUser."
