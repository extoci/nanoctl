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
. "$PSScriptRoot\..\tailnet.ps1"
$taskName = "nanoctl Agent"
$sourceBinary = (Resolve-Path -LiteralPath $BinaryPath).Path
$resolvedConfig = (Resolve-Path -LiteralPath $ConfigPath).Path
$currentIdentity = [Security.Principal.WindowsIdentity]::GetCurrent()
$currentUser = $currentIdentity.Name
$installRoot = Join-Path $env:ProgramFiles "nanoctl"
$installedBinary = Join-Path $installRoot "nanoctl.exe"
$runnerPath = Join-Path $installRoot "run-agent.vbs"
$logPath = Join-Path $env:LOCALAPPDATA "nanoctl\agent.log"
$readyPath = Join-Path $env:LOCALAPPDATA "nanoctl\agent.ready"
$transactionId = [Guid]::NewGuid().ToString("N")
$temporary = Join-Path ([IO.Path]::GetTempPath()) ("nanoctl-managed-" + $transactionId)
$previousBinary = "$installedBinary.$transactionId.previous"
$failedBinary = "$installedBinary.$transactionId.failed"
$previousRunner = "$runnerPath.$transactionId.previous"
$previousTaskXml = Join-Path $temporary "previous-task.xml"
$candidateBinary = Join-Path $temporary "nanoctl.exe"
$lockPath = Join-Path $installRoot "install.lock"
$lockStream = $null
$existingTask = Get-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue
$hadExistingInstallation =
  [bool]($existingTask -or (Test-Path -LiteralPath $installedBinary -PathType Leaf))
$activated = $false
$taskBackedUp = $false

function Test-CurrentUserAdministrator {
  try {
    $principal = [Security.Principal.WindowsPrincipal]::new($currentIdentity)
    if ($principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
      return $true
    }
    $administratorSid = [Security.Principal.SecurityIdentifier]::new("S-1-5-32-544")
    return @($currentIdentity.Groups | Where-Object {
        $_.Value -eq $administratorSid.Value
      }).Count -gt 0
  } catch {
    return $false
  }
}

function Resolve-AccountSid {
  param([Parameter(Mandatory = $true)][string]$Account)

  if ($Account -match '^S-\d-') {
    return ([Security.Principal.SecurityIdentifier]$Account).Value
  }
  return ([Security.Principal.NTAccount]$Account).Translate(
    [Security.Principal.SecurityIdentifier]
  ).Value
}

function Test-ConfigFile {
  param([string]$Path)

  if (-not $Path -or -not (Test-Path -LiteralPath $Path -PathType Leaf)) {
    return $false
  }
  return $true
}

function Get-TaskConfigPath {
  param($Task)

  foreach ($action in @($Task.Actions)) {
    $arguments = [string]$action.Arguments
    if ($arguments -match '(?i)run-agent\.vbs"\s+"[^"]+"\s+"([^"]+)"') {
      return $Matches[1]
    }
    if ($arguments -match '(?i)--config\s+"([^"]+)"') {
      return $Matches[1]
    }
    if ($arguments -match '(?i)--config\s+([^\s]+)') {
      return $Matches[1]
    }
  }
  return $null
}

function Get-TaskBinaryPaths {
  param($Task)

  $paths = @()
  foreach ($action in @($Task.Actions)) {
    $execute = [string]$action.Execute
    try {
      if ($execute -and [IO.Path]::GetFileName($execute) -ieq "nanoctl.exe") {
        $paths += $execute
      }
    } catch {
      # A malformed action is not a reason to skip the known canonical path. The task owner and
      # configuration checks above still decide whether migration is allowed.
    }

    $arguments = [string]$action.Arguments
    if ($arguments -match '(?i)run-agent\.vbs"\s+"([^"]+)"') {
      $paths += $Matches[1]
    }
    if ($arguments -match '(?i)(?:-BinaryPath|--binary)\s+"([^"]+)"') {
      $paths += $Matches[1]
    }
    if ($arguments -match '(?i)(?:^|\s)"([^"]*\\nanoctl\.exe)"(?:\s|$)') {
      $paths += $Matches[1]
    }
  }
  return @($paths | Where-Object { $_ } | Select-Object -Unique)
}

function Assert-ExistingTaskOwner {
  param([Parameter(Mandatory = $true)]$Task)

  $taskUser = [string]$Task.Principal.UserId
  if (-not $taskUser -or $taskUser -match '(?i)^SYSTEM$|^NT AUTHORITY\\') {
    throw "The existing nanoctl task is not a per-user interactive task; refusing to migrate it."
  }
  $taskLogonType = [string]$Task.Principal.LogonType
  if ($taskLogonType -and $taskLogonType -notmatch '(?i)^(Interactive|InteractiveToken)$') {
    throw "The existing nanoctl task does not run in the interactive user session; refusing to migrate it."
  }
  try {
    $taskSid = Resolve-AccountSid -Account $taskUser
  } catch {
    throw "Could not resolve the existing nanoctl task owner '$taskUser'; refusing to migrate it."
  }
  if ($taskSid -ne $currentIdentity.User.Value) {
    throw (
      "The existing nanoctl task belongs to '$taskUser', but this installer is running as " +
      "'$currentUser'. Sign in as the account that owns the task before reinstalling."
    )
  }
}

if ($existingTask) {
  Assert-ExistingTaskOwner -Task $existingTask
  $taskConfigPath = Get-TaskConfigPath -Task $existingTask
  if ($taskConfigPath -and (Test-ConfigFile -Path $taskConfigPath)) {
    $resolvedConfig = (Resolve-Path -LiteralPath $taskConfigPath).Path
    Write-Host "Preserving the existing configuration at $resolvedConfig."
  }
}

$configOwner = (Get-Acl -LiteralPath $resolvedConfig).Owner
try {
  $configOwnerSid = Resolve-AccountSid -Account $configOwner
} catch {
  throw "Could not resolve the configuration owner '$configOwner'."
}
if ($configOwnerSid -ne $currentIdentity.User.Value -and
    -not ($configOwnerSid -eq "S-1-5-32-544" -and (Test-CurrentUserAdministrator))) {
  throw (
    "The elevated identity '$currentUser' does not own the configuration. " +
    "Sign in as '$configOwner' and elevate that same account; do not supply another administrator."
  )
}

function Ensure-HeadlessRunner {
  param([Parameter(Mandatory = $true)][string]$Path)

  $runner = @'
Option Explicit
On Error Resume Next

Dim shell, fso, binaryPath, configPath, logPath, readyPath, readyToken, command, exitCode, stream
Set shell = CreateObject("WScript.Shell")
Set fso = CreateObject("Scripting.FileSystemObject")
binaryPath = WScript.Arguments(0)
configPath = WScript.Arguments(1)
logPath = WScript.Arguments(2)
readyPath = WScript.Arguments(3)
readyToken = WScript.Arguments(4)
Set stream = fso.OpenTextFile(logPath, 8, True)
stream.WriteLine "headless runner started"
stream.Close
If fso.FileExists(readyPath) Then fso.DeleteFile readyPath, True
command = Chr(34) & binaryPath & Chr(34) & " --config " & Chr(34) & configPath & Chr(34) & _
  " --log-file " & Chr(34) & logPath & Chr(34) & " --ready-file " & Chr(34) & readyPath & Chr(34) & _
  " --ready-token " & Chr(34) & readyToken & Chr(34) & " run"
Err.Clear
exitCode = shell.Run(command, 0, True)
If Err.Number <> 0 Then
  Set stream = fso.OpenTextFile(logPath, 8, True)
  stream.WriteLine "headless runner error " & Err.Description
  stream.Close
  exitCode = 1
Else
  Set stream = fso.OpenTextFile(logPath, 8, True)
  stream.WriteLine "headless runner child exited with code " & exitCode
  stream.Close
End If
WScript.Quit exitCode
'@
  # Keep the WSH runner ASCII-only; Windows PowerShell 5.1 UTF8 output includes a BOM that can
  # make wscript.exe reject the script before it can write diagnostics.
  Set-Content -LiteralPath $Path -Value $runner -Encoding ASCII -Force
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

function Get-ReadyAgentProcess {
  param(
    [Parameter(Mandatory = $true)][string]$ReadyPath,
    [Parameter(Mandatory = $true)][string]$BinaryPath,
    [Parameter(Mandatory = $true)][string]$ReadyToken,
    [Parameter(Mandatory = $true)][string]$ReadyVersion
  )

  if (-not (Test-Path -LiteralPath $ReadyPath -PathType Leaf)) {
    return $null
  }
  $marker = Get-Content -LiteralPath $ReadyPath -Raw -ErrorAction SilentlyContinue
  if ($marker -notmatch '(?m)^\s*pid=(\d+)\s*$') {
    return $null
  }
  $agentProcessIdText = $Matches[1]
  if ($marker -notmatch "(?m)^\s*token=$([regex]::Escape($ReadyToken))\s*$") {
    return $null
  }
  if ($marker -notmatch "(?m)^\s*version=$([regex]::Escape($ReadyVersion))\s*$") {
    return $null
  }
  [int]$agentProcessId = 0
  if (-not [int]::TryParse($agentProcessIdText, [ref]$agentProcessId)) {
    return $null
  }
  $process = Get-Process -Id $agentProcessId -ErrorAction SilentlyContinue
  if (-not $process) {
    return $null
  }
  try {
    $processPath = [IO.Path]::GetFullPath([string]$process.Path)
    $expectedPath = [IO.Path]::GetFullPath($BinaryPath)
    if (-not [String]::Equals($processPath, $expectedPath, [StringComparison]::OrdinalIgnoreCase)) {
      return $null
    }
  } catch {
    # Readiness is fail-closed: a PID without an executable path cannot prove that the
    # installed binary is the process that reached readiness.
    return $null
  }
  return $process
}

function Wait-AgentTask {
  param([Parameter(Mandatory = $true)][string]$ReadyVersion)

  if (Test-Path -LiteralPath $readyPath -PathType Leaf) {
    Remove-Item -LiteralPath $readyPath -Force -ErrorAction Stop
  }
  Start-ScheduledTask -TaskName $taskName
  $deadline = [DateTime]::UtcNow.AddSeconds(90)
  $readyProcess = $null
  while ([DateTime]::UtcNow -lt $deadline) {
    $readyProcess = Get-ReadyAgentProcess `
      -ReadyPath $readyPath `
      -BinaryPath $installedBinary `
      -ReadyToken $transactionId `
      -ReadyVersion $ReadyVersion
    if ($readyProcess) {
      break
    }
    Start-Sleep -Milliseconds 200
  }
  if (-not $readyProcess) {
    throw "nanoctl did not become ready. $(Get-AgentFailureDetails)"
  }
  Start-Sleep -Seconds 5
  if (-not (Get-ReadyAgentProcess `
      -ReadyPath $readyPath `
      -BinaryPath $installedBinary `
      -ReadyToken $transactionId `
      -ReadyVersion $ReadyVersion)) {
    throw "nanoctl exited during its startup stability window. $(Get-AgentFailureDetails)"
  }
}

function Wait-AgentProcessExit {
  param([Parameter(Mandatory = $true)][string[]]$Paths)

  $normalizedPaths = @(
    foreach ($path in $Paths) {
      if (-not $path) { continue }
      try {
        [IO.Path]::GetFullPath([string]$path)
      } catch {
        continue
      }
    }
  )
  for ($attempt = 0; $attempt -lt 600; $attempt++) {
    $running = @(Get-Process -Name nanoctl -ErrorAction SilentlyContinue | Where-Object {
        if (-not $_.Path) { return $false }
        try {
          $processPath = [IO.Path]::GetFullPath([string]$_.Path)
        } catch {
          return $false
        }
        foreach ($expectedPath in $normalizedPaths) {
          if ([String]::Equals($processPath, $expectedPath, [StringComparison]::OrdinalIgnoreCase)) {
            return $true
          }
        }
        return $false
      })
    if ($running.Count -eq 0) { return }
    Start-Sleep -Milliseconds 100
  }
  throw "Timed out while waiting for the previous nanoctl process to exit."
}

# Capture and input must execute in the interactive user's session. A LocalSystem service runs in
# Session 0 and cannot safely use that desktop.
try {
  New-Item -ItemType Directory -Path $installRoot -Force | Out-Null
  $lockStream = [IO.File]::Open(
    $lockPath,
    [IO.FileMode]::OpenOrCreate,
    [IO.FileAccess]::ReadWrite,
    [IO.FileShare]::None
  )
  New-Item -ItemType Directory -Path $temporary -Force | Out-Null
  New-Item -ItemType Directory -Path (Split-Path -Parent $readyPath) -Force | Out-Null
  if ($existingTask) {
    $previousProcessPaths = @($installedBinary) + @(Get-TaskBinaryPaths -Task $existingTask)
    Export-ScheduledTask -TaskName $taskName |
      Set-Content -LiteralPath $previousTaskXml -Encoding UTF8
    $taskBackedUp = $true
    Stop-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue
    # Legacy tasks may still launch a binary from the old per-user install directory. Wait for
    # the executable declared by the task as well as the canonical path before replacing files;
    # otherwise capture/encoder resources can remain owned by the old process after migration.
    Wait-AgentProcessExit -Paths $previousProcessPaths
    Unregister-ScheduledTask -TaskName $taskName -Confirm:$false
  }
  Copy-Item -LiteralPath $sourceBinary -Destination $candidateBinary -Force
  if (Test-Path -LiteralPath $installedBinary -PathType Leaf) {
    Move-Item -LiteralPath $installedBinary -Destination $previousBinary -Force
  }
  if (Test-Path -LiteralPath $runnerPath -PathType Leaf) {
    Move-Item -LiteralPath $runnerPath -Destination $previousRunner -Force
  }
  Move-Item -LiteralPath $candidateBinary -Destination $installedBinary -Force
  $activated = $true
  Ensure-HeadlessRunner -Path $runnerPath
  New-Item -ItemType File -Path $logPath -Force | Out-Null

  # The publisher-controlled executable is read-only to the interactive agent identity. The
  # configuration remains writable only by that identity, SYSTEM, and local administrators.
  & icacls.exe $installRoot `
    /inheritance:r `
    /grant:r "*S-1-5-18:(OI)(CI)(F)" "*S-1-5-32-544:(OI)(CI)(F)" "${currentUser}:(OI)(CI)(RX)" | Out-Null
  if ($LASTEXITCODE -ne 0) {
    throw "Could not apply the nanoctl installation ACL."
  }
  & icacls.exe $logPath `
    /inheritance:r `
    /grant:r "*S-1-5-18:(F)" "*S-1-5-32-544:(F)" "${currentUser}:(M)" | Out-Null
  if ($LASTEXITCODE -ne 0) {
    throw "Could not apply the nanoctl log ACL."
  }
  & icacls.exe $resolvedConfig `
    /inheritance:r `
    /grant:r "*S-1-5-18:(F)" "*S-1-5-32-544:(F)" "${currentUser}:(F)" | Out-Null
  if ($LASTEXITCODE -ne 0) {
    throw "Could not apply the nanoctl configuration ACL."
  }

  $installedVersionOutput = (& $installedBinary --log-file $logPath --version 2>&1 | Out-String).Trim()
  if ($LASTEXITCODE -ne 0 -or
      $installedVersionOutput -notmatch '(?m)^nanoctl\s+(\d+\.\d+\.\d+(?:-[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?)\s*$') {
    throw "The nanoctl executable is incompatible with the headless service runner."
  }
  $installedVersion = $Matches[1]
  & $installedBinary --config $resolvedConfig config | Out-Null
  if ($LASTEXITCODE -ne 0) {
    throw "nanoctl failed its pre-install health check."
  }

  $wscript = Join-Path $env:SystemRoot "System32\wscript.exe"
  $actionArguments = '//B //NoLogo "{0}" "{1}" "{2}" "{3}" "{4}" "{5}"' -f `
    $runnerPath, $installedBinary, $resolvedConfig, $logPath, $readyPath, $transactionId
  $action = New-ScheduledTaskAction `
    -Execute $wscript `
    -Argument $actionArguments `
    -WorkingDirectory $installRoot
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
  Wait-AgentTask -ReadyVersion $installedVersion
}
catch {
  try {
    if (($taskBackedUp -or -not $existingTask) -and
        (Get-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue)) {
      Stop-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue
      Unregister-ScheduledTask -TaskName $taskName -Confirm:$false -ErrorAction SilentlyContinue
    }
    if ($activated) {
      Wait-AgentProcessExit -Paths @($installedBinary)
    }
    if (Test-Path -LiteralPath $previousBinary -PathType Leaf) {
      if (Test-Path -LiteralPath $installedBinary -PathType Leaf) {
        Move-Item -LiteralPath $installedBinary -Destination $failedBinary -Force
      }
      Move-Item -LiteralPath $previousBinary -Destination $installedBinary -Force
    } elseif ($activated -and -not $hadExistingInstallation) {
      Remove-Item -LiteralPath $installedBinary -Force -ErrorAction SilentlyContinue
    }
    if (Test-Path -LiteralPath $previousRunner -PathType Leaf) {
      if (Test-Path -LiteralPath $runnerPath -PathType Leaf) {
        Remove-Item -LiteralPath $runnerPath -Force -ErrorAction SilentlyContinue
      }
      Move-Item -LiteralPath $previousRunner -Destination $runnerPath -Force
    }
    if ($taskBackedUp) {
      $xml = Get-Content -LiteralPath $previousTaskXml -Raw
      Register-ScheduledTask -TaskName $taskName -Xml $xml -Force | Out-Null
      Start-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue
    } elseif (-not $hadExistingInstallation) {
      Remove-Item -LiteralPath $installRoot -Recurse -Force -ErrorAction SilentlyContinue
    }
  } catch {
    Write-Warning "nanoctl reinstall rollback could not fully restore the previous task and binary: $($_.Exception.Message)"
  }
  Remove-Item -LiteralPath $readyPath -Force -ErrorAction SilentlyContinue
  Remove-Item -LiteralPath $candidateBinary -Force -ErrorAction SilentlyContinue
  Remove-Item -LiteralPath $temporary -Recurse -Force -ErrorAction SilentlyContinue
  throw
}
finally {
  if ($lockStream) {
    $lockStream.Dispose()
    $lockStream = $null
  }
  Remove-Item -LiteralPath $lockPath -Force -ErrorAction SilentlyContinue
}
Remove-Item -LiteralPath $previousBinary -Force -ErrorAction SilentlyContinue
Remove-Item -LiteralPath $previousRunner -Force -ErrorAction SilentlyContinue
Remove-Item -LiteralPath $previousTaskXml -Force -ErrorAction SilentlyContinue
Remove-Item -LiteralPath $temporary -Recurse -Force -ErrorAction SilentlyContinue
Write-Host "nanoctl background agent installed at $installedBinary and started for $currentUser."
Remove-LegacyCredential -ConfigPath $resolvedConfig
Show-TailnetUrl -BinaryPath $installedBinary -ConfigPath $resolvedConfig
