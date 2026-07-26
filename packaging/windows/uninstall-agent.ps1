#Requires -RunAsAdministrator
[CmdletBinding()]
param(
  [Parameter(Mandatory = $true)]
  [ValidateScript({ Test-Path -LiteralPath $_ -PathType Leaf })]
  [string]$BinaryPath,

  [Parameter(Mandatory = $true)]
  [string]$ConfigPath
)

$ErrorActionPreference = "Stop"
$taskName = "nanoctl Agent"
$task = Get-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue
if ($task) {
  Stop-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue
  Unregister-ScheduledTask -TaskName $taskName -Confirm:$false
}

$resolvedBinary = (Resolve-Path -LiteralPath $BinaryPath).Path
if (Test-Path -LiteralPath $ConfigPath -PathType Leaf) {
  & $resolvedBinary --config $ConfigPath unenroll
}
Write-Host "nanoctl background agent registration and local enrollment removed."
