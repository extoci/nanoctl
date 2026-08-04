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
$currentIdentity = [Security.Principal.WindowsIdentity]::GetCurrent()
$currentUser = $currentIdentity.Name
$configOwner = (Get-Acl -LiteralPath $resolvedConfig).Owner
$configOwnerSid = ([Security.Principal.NTAccount]$configOwner).Translate(
  [Security.Principal.SecurityIdentifier]
)
$installRoot = Join-Path $env:ProgramFiles "nanoctl"
$installedBinary = Join-Path $installRoot "nanoctl.exe"
$runnerPath = Join-Path $installRoot "run-agent.vbs"
$logPath = Join-Path $env:LOCALAPPDATA "nanoctl\agent.log"
$readyPath = Join-Path $env:LOCALAPPDATA "nanoctl\agent.ready"

if ($configOwnerSid.Value -ne $currentIdentity.User.Value) {
  throw (
    "The elevated identity '$currentUser' does not own the enrolled configuration. " +
    "Sign in as '$configOwner' and elevate that same account; do not supply another administrator."
  )
}
if (Get-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue) {
  throw "The nanoctl agent is already installed. Uninstall or update it explicitly."
}
if ((Test-Path -LiteralPath $installedBinary) -or (Test-Path -LiteralPath $installRoot)) {
  throw "The nanoctl install directory already exists. Remove it explicitly before reinstalling."
}

function Ensure-HeadlessRunner {
  param([Parameter(Mandatory = $true)][string]$Path)

  $runner = @'
Option Explicit
On Error Resume Next

Dim shell, fso, binaryPath, configPath, logPath, readyPath, command, exitCode
Set shell = CreateObject("WScript.Shell")
Set fso = CreateObject("Scripting.FileSystemObject")
binaryPath = WScript.Arguments(0)
configPath = WScript.Arguments(1)
logPath = WScript.Arguments(2)
readyPath = WScript.Arguments(3)
If fso.FileExists(readyPath) Then fso.DeleteFile readyPath, True
command = Chr(34) & binaryPath & Chr(34) & " --config " & Chr(34) & configPath & Chr(34) & _
  " --log-file " & Chr(34) & logPath & Chr(34) & " --ready-file " & Chr(34) & readyPath & Chr(34) & " run"
Err.Clear
exitCode = shell.Run(command, 0, True)
If Err.Number <> 0 Then
  Dim stream
  Set stream = fso.OpenTextFile(logPath, 8, True)
  stream.WriteLine "headless runner error " & Err.Description
  stream.Close
  exitCode = 1
End If
WScript.Quit exitCode
'@
  Set-Content -LiteralPath $Path -Value $runner -Encoding UTF8 -Force
}

function Get-AgentFailureDetails {
  $taskInfo = Get-ScheduledTaskInfo -TaskName $taskName -ErrorAction SilentlyContinue
  $logTail = if (Test-Path -LiteralPath $logPath -PathType Leaf) {
    (Get-Content -LiteralPath $logPath -Tail 40 -ErrorAction SilentlyContinue | Out-String).Trim()
  } else {
    "(agent log was not created at $logPath)"
  }
  "task result: $($taskInfo.LastTaskResult)`nlog: $logPath`n$logTail"
}

function Wait-AgentTask {
  if (Test-Path -LiteralPath $readyPath -PathType Leaf) {
    Remove-Item -LiteralPath $readyPath -Force -ErrorAction Stop
  }
  Start-ScheduledTask -TaskName $taskName
  $deadline = [DateTime]::UtcNow.AddSeconds(30)
  $ready = $false
  while ([DateTime]::UtcNow -lt $deadline) {
    $task = Get-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue
    if ($task -and $task.State -eq "Running" -and
        (Test-Path -LiteralPath $readyPath -PathType Leaf)) {
      $ready = $true
      break
    }
    Start-Sleep -Milliseconds 200
  }
  if (-not $ready) {
    throw "nanoctl did not become ready. $(Get-AgentFailureDetails)"
  }
  Start-Sleep -Seconds 3
  $stableTask = Get-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue
  if (-not $stableTask -or $stableTask.State -ne "Running" -or
      -not (Test-Path -LiteralPath $readyPath -PathType Leaf)) {
    throw "nanoctl exited during its startup stability window. $(Get-AgentFailureDetails)"
  }
}

# Capture and input must execute in the interactive user's session. A LocalSystem service runs in
# Session 0 and cannot safely use that desktop or the enrolling user's Credential Manager entry.
try {
  New-Item -ItemType Directory -Path $installRoot | Out-Null
  New-Item -ItemType Directory -Path (Split-Path -Parent $readyPath) -Force | Out-Null
  Copy-Item -LiteralPath $sourceBinary -Destination $installedBinary
  Ensure-HeadlessRunner -Path $runnerPath
  New-Item -ItemType File -Path $logPath -Force | Out-Null

  # The publisher-controlled executable is read-only to the interactive agent identity. The
  # configuration remains writable only by that identity, SYSTEM, and local administrators.
  & icacls.exe $installRoot `
    /inheritance:r `
    /grant:r "*S-1-5-18:(OI)(CI)(F)" "*S-1-5-32-544:(OI)(CI)(F)" "${currentUser}:(OI)(CI)(RX)" `
    /quiet | Out-Null
  if ($LASTEXITCODE -ne 0) {
    throw "Could not apply the nanoctl installation ACL."
  }
  & icacls.exe $logPath `
    /inheritance:r `
    /grant:r "*S-1-5-18:(F)" "*S-1-5-32-544:(F)" "${currentUser}:(M)" `
    /quiet | Out-Null
  if ($LASTEXITCODE -ne 0) {
    throw "Could not apply the nanoctl log ACL."
  }
  & icacls.exe $resolvedConfig `
    /inheritance:r `
    /grant:r "*S-1-5-18:(F)" "*S-1-5-32-544:(F)" "${currentUser}:(F)" `
    /quiet | Out-Null
  if ($LASTEXITCODE -ne 0) {
    throw "Could not apply the nanoctl configuration ACL."
  }

  & $installedBinary --log-file $logPath --version | Out-Null
  if ($LASTEXITCODE -ne 0) {
    throw "The nanoctl executable is incompatible with the headless service runner."
  }
  & $installedBinary --config $resolvedConfig doctor
  if ($LASTEXITCODE -ne 0) {
    throw "nanoctl failed its pre-install health check."
  }

  $wscript = Join-Path $env:SystemRoot "System32\wscript.exe"
  $actionArguments = '//B //NoLogo "{0}" "{1}" "{2}" "{3}" "{4}"' -f `
    $runnerPath, $installedBinary, $resolvedConfig, $logPath, $readyPath
  $action = New-ScheduledTaskAction `
    -Execute $wscript `
    -Argument $actionArguments
  $trigger = New-ScheduledTaskTrigger -AtLogOn -User $currentUser
  $principal = New-ScheduledTaskPrincipal `
    -UserId $currentUser `
    -LogonType Interactive `
    -RunLevel Limited
  $settings = New-ScheduledTaskSettingsSet `
    -Hidden `
    -AllowStartIfOnBatteries `
    -DontStopIfGoingOnBatteries `
    -ExecutionTimeLimit ([TimeSpan]::Zero) `
    -MultipleInstances IgnoreNew `
    -RestartCount 999 `
    -RestartInterval (New-TimeSpan -Minutes 1) `
    -StartWhenAvailable

  Register-ScheduledTask `
    -TaskName $taskName `
    -Action $action `
    -Trigger $trigger `
    -Principal $principal `
    -Settings $settings `
    -Description "Headless device-owner-authorized nanoctl remote desktop agent." | Out-Null
  Wait-AgentTask
}
catch {
  Unregister-ScheduledTask -TaskName $taskName -Confirm:$false -ErrorAction SilentlyContinue
  Remove-Item -LiteralPath $readyPath -Force -ErrorAction SilentlyContinue
  Remove-Item -LiteralPath $installRoot -Recurse -Force -ErrorAction SilentlyContinue
  throw
}
Write-Host "nanoctl background agent installed at $installedBinary and started for $currentUser."
