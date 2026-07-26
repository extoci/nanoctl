#Requires -RunAsAdministrator
[CmdletBinding()]
param(
  [string]$BinaryPath = (Join-Path $env:ProgramFiles "nanoctl\nanoctl.exe"),

  [string]$ConfigPath
)

$ErrorActionPreference = "Stop"
$taskName = "nanoctl Agent"
$task = Get-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue
if ($task) {
  Stop-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue
  Unregister-ScheduledTask -TaskName $taskName -Confirm:$false
}

if (-not (Test-Path -LiteralPath $BinaryPath -PathType Leaf)) {
  Write-Warning "The installed nanoctl binary is missing; the Scheduled Task was removed."
  return
}
$resolvedBinary = (Resolve-Path -LiteralPath $BinaryPath).Path
if (-not $ConfigPath) {
  $pathOutput = & $resolvedBinary paths
  $ConfigPath = [string]($pathOutput -replace '^config=', '')
}
if ($ConfigPath -and (Test-Path -LiteralPath $ConfigPath -PathType Leaf)) {
  & $resolvedBinary --config $ConfigPath unenroll
  if ($LASTEXITCODE -ne 0) {
    throw "nanoctl could not remove its local enrollment."
  }
}
$installRoot = Split-Path -Parent $resolvedBinary
Remove-Item -LiteralPath $resolvedBinary -Force
if ((Split-Path -Leaf $installRoot) -eq "nanoctl") {
  Remove-Item -LiteralPath $installRoot -Force -ErrorAction SilentlyContinue
}
Write-Host "nanoctl background agent, installed binary, and local enrollment removed."
