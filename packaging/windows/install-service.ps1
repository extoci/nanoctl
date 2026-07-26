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
$sourceBinary = (Resolve-Path -LiteralPath $BinaryPath).Path
$resolvedConfig = (Resolve-Path -LiteralPath $ConfigPath).Path
$currentUser = [Security.Principal.WindowsIdentity]::GetCurrent().Name
$installRoot = Join-Path $env:ProgramFiles "nanoctl"
$installedBinary = Join-Path $installRoot "nanoctl.exe"

if (Get-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue) {
  throw "The nanoctl agent is already installed. Uninstall or update it explicitly."
}
if ((Test-Path -LiteralPath $installedBinary) -or (Test-Path -LiteralPath $installRoot)) {
  throw "The nanoctl install directory already exists. Remove it explicitly before reinstalling."
}

# Capture and input must execute in the interactive user's session. A LocalSystem service runs in
# Session 0 and cannot safely use that desktop or the enrolling user's Credential Manager entry.
try {
  New-Item -ItemType Directory -Path $installRoot | Out-Null
  Copy-Item -LiteralPath $sourceBinary -Destination $installedBinary

  # The publisher-controlled executable is read-only to the interactive agent identity. The
  # configuration remains writable only by that identity, SYSTEM, and local administrators.
  & icacls.exe $installRoot `
    /inheritance:r `
    /grant:r "*S-1-5-18:(OI)(CI)(F)" "*S-1-5-32-544:(OI)(CI)(F)" "${currentUser}:(OI)(CI)(RX)" `
    /quiet | Out-Null
  if ($LASTEXITCODE -ne 0) {
    throw "Could not apply the nanoctl installation ACL."
  }
  & icacls.exe $resolvedConfig `
    /inheritance:r `
    /grant:r "*S-1-5-18:(F)" "*S-1-5-32-544:(F)" "${currentUser}:(F)" `
    /quiet | Out-Null
  if ($LASTEXITCODE -ne 0) {
    throw "Could not apply the nanoctl configuration ACL."
  }

  & $installedBinary --config $resolvedConfig doctor
  if ($LASTEXITCODE -ne 0) {
    throw "nanoctl failed its pre-install health check."
  }

  $action = New-ScheduledTaskAction `
    -Execute $installedBinary `
    -Argument ('--config "{0}" run' -f $resolvedConfig)
  $trigger = New-ScheduledTaskTrigger -AtLogOn -User $currentUser
  $principal = New-ScheduledTaskPrincipal `
    -UserId $currentUser `
    -LogonType Interactive `
    -RunLevel Limited
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
}
catch {
  Unregister-ScheduledTask -TaskName $taskName -Confirm:$false -ErrorAction SilentlyContinue
  Remove-Item -LiteralPath $installRoot -Recurse -Force -ErrorAction SilentlyContinue
  throw
}
Write-Host "nanoctl background agent installed at $installedBinary and started for $currentUser."
