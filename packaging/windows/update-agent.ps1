#Requires -RunAsAdministrator
[CmdletBinding()]
param(
  [Parameter(Mandatory = $true)]
  [ValidateScript({ Test-Path -LiteralPath $_ -PathType Leaf })]
  [string]$BinaryPath,

  [Parameter(Mandatory = $true)]
  [ValidateScript({ Test-Path -LiteralPath $_ -PathType Leaf })]
  [string]$ConfigPath,

  [Parameter(Mandatory = $true)]
  [ValidateScript({ Test-Path -LiteralPath $_ -PathType Leaf })]
  [string]$ManifestPath,

  [Parameter(Mandatory = $true)]
  [ValidateNotNullOrEmpty()]
  [string]$PublicKey
)

$ErrorActionPreference = "Stop"
$taskName = "nanoctl Agent"
$resolvedBinary = (Resolve-Path -LiteralPath $BinaryPath).Path
$resolvedConfig = (Resolve-Path -LiteralPath $ConfigPath).Path
$currentIdentity = [Security.Principal.WindowsIdentity]::GetCurrent()
$configOwner = (Get-Acl -LiteralPath $resolvedConfig).Owner
$configOwnerSid = ([Security.Principal.NTAccount]$configOwner).Translate(
  [Security.Principal.SecurityIdentifier]
)
if ($configOwnerSid.Value -ne $currentIdentity.User.Value) {
  throw (
    "The elevated identity '$($currentIdentity.Name)' does not own the enrolled configuration. " +
    "Elevate the same account that enrolled nanoctl; do not supply another administrator."
  )
}
$resolvedManifest = (Resolve-Path -LiteralPath $ManifestPath).Path
$installRoot = Split-Path -Parent $resolvedBinary
$transactionId = [Guid]::NewGuid().ToString("N")
$candidate = "$resolvedBinary.$transactionId.candidate"
$previous = "$resolvedBinary.$transactionId.previous"
$failed = "$resolvedBinary.$transactionId.failed"
$previousTaskXml = "$resolvedBinary.$transactionId.previous-task.xml"
$runnerPath = Join-Path $installRoot "run-agent.vbs"
$logPath = Join-Path $env:LOCALAPPDATA "nanoctl\agent.log"
$readyPath = Join-Path $env:LOCALAPPDATA "nanoctl\agent.ready"
$lockPath = "$resolvedBinary.update.lock"
$lockStream = $null
$lockAcquired = $false
$activated = $false
$completed = $false
$taskBackedUp = $false

New-Item -ItemType Directory -Path (Split-Path -Parent $readyPath) -Force | Out-Null
New-Item -ItemType File -Path $logPath -Force | Out-Null

function Resolve-AccountSid {
  param([Parameter(Mandatory = $true)][string]$Account)

  if ($Account -match '^S-\d-') {
    return ([Security.Principal.SecurityIdentifier]$Account).Value
  }
  return ([Security.Principal.NTAccount]$Account).Translate(
    [Security.Principal.SecurityIdentifier]
  ).Value
}

function Assert-TaskOwner {
  param([Parameter(Mandatory = $true)]$Task)

  $taskUser = [string]$Task.Principal.UserId
  if (-not $taskUser -or $taskUser -match '(?i)^SYSTEM$|^NT AUTHORITY\\') {
    throw "The nanoctl task is not a per-user interactive task; refusing to update it."
  }
  $taskLogonType = [string]$Task.Principal.LogonType
  if ($taskLogonType -and $taskLogonType -notmatch '(?i)^Interactive$') {
    throw "The nanoctl task does not run in the interactive user session; refusing to update it."
  }
  try {
    $taskSid = Resolve-AccountSid -Account $taskUser
  } catch {
    throw "Could not resolve the nanoctl task owner '$taskUser'; refusing to update it."
  }
  if ($taskSid -ne $currentIdentity.User.Value) {
    throw (
      "The nanoctl task belongs to '$taskUser', but the update is running as " +
      "'$($currentIdentity.Name)'. Elevate the same account that enrolled nanoctl."
    )
  }
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
  Set-Content -LiteralPath $Path -Value $runner -Encoding UTF8 -Force
}

function Set-HeadlessTaskAction {
  Ensure-HeadlessRunner -Path $runnerPath
  $wscript = Join-Path $env:SystemRoot "System32\wscript.exe"
  $arguments = '//B //NoLogo "{0}" "{1}" "{2}" "{3}" "{4}" "{5}"' -f `
    $runnerPath, $resolvedBinary, $resolvedConfig, $logPath, $readyPath, $transactionId
  $action = New-ScheduledTaskAction `
    -Execute $wscript `
    -Argument $arguments `
    -WorkingDirectory $installRoot
  $settings = New-ScheduledTaskSettingsSet `
    -Hidden `
    -AllowStartIfOnBatteries `
    -DontStopIfGoingOnBatteries `
    -ExecutionTimeLimit ([TimeSpan]::Zero) `
    -MultipleInstances IgnoreNew `
    -RestartCount 999 `
    -RestartInterval (New-TimeSpan -Minutes 1) `
    -StartWhenAvailable
  Set-ScheduledTask -TaskName $taskName -Action $action -Settings $settings | Out-Null
}

function Restore-PreviousTask {
  if (-not $taskBackedUp -or -not (Test-Path -LiteralPath $previousTaskXml -PathType Leaf)) {
    throw "The previous nanoctl task definition is unavailable; refusing to start an unverified task."
  }
  if (Get-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue) {
    Unregister-ScheduledTask -TaskName $taskName -Confirm:$false
  }
  $xml = Get-Content -LiteralPath $previousTaskXml -Raw
  Register-ScheduledTask -TaskName $taskName -Xml $xml -Force | Out-Null
}

function Stop-AgentTask {
  Stop-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue
  for ($attempt = 0; $attempt -lt 50; $attempt++) {
    $task = Get-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue
    if (-not $task -or $task.State -ne "Running") {
      return
    }
    Start-Sleep -Milliseconds 100
  }
  throw "Timed out while stopping the nanoctl Scheduled Task."
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

function Wait-AgentProcessExit {
  for ($attempt = 0; $attempt -lt 300; $attempt++) {
    $running = @(Get-Process -Name nanoctl -ErrorAction SilentlyContinue | Where-Object {
        $_.Path -eq $resolvedBinary
      })
    if ($running.Count -eq 0) { return }
    Start-Sleep -Milliseconds 100
  }
  throw "Timed out while waiting for the previous nanoctl process to exit."
}

function Get-ReadyAgentProcess {
  param(
    [Parameter(Mandatory = $true)][string]$BinaryPath,
    [Parameter(Mandatory = $true)][string]$ReadyToken,
    [Parameter(Mandatory = $true)][string]$ReadyVersion
  )

  if (-not (Test-Path -LiteralPath $readyPath -PathType Leaf)) {
    return $null
  }
  $marker = Get-Content -LiteralPath $readyPath -Raw -ErrorAction SilentlyContinue
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
    # candidate binary is the process that reached readiness.
    return $null
  }
  return $process
}

function Wait-AgentReady {
  param(
    [Parameter(Mandatory = $true)][string]$BinaryPath,
    [Parameter(Mandatory = $true)][string]$ReadyVersion
  )

  if (Test-Path -LiteralPath $readyPath -PathType Leaf) {
    Remove-Item -LiteralPath $readyPath -Force -ErrorAction Stop
  }
  $deadline = [DateTime]::UtcNow.AddSeconds(90)
  $readyProcess = $null
  while ([DateTime]::UtcNow -lt $deadline) {
    $readyProcess = Get-ReadyAgentProcess `
      -BinaryPath $BinaryPath `
      -ReadyToken $transactionId `
      -ReadyVersion $ReadyVersion
    if ($readyProcess) {
      break
    }
    Start-Sleep -Milliseconds 100
  }
  if (-not $readyProcess) {
    throw "Updated nanoctl did not become ready. $(Get-AgentFailureDetails)"
  }
  Start-Sleep -Seconds 5
  if (-not (Get-ReadyAgentProcess `
      -BinaryPath $BinaryPath `
      -ReadyToken $transactionId `
      -ReadyVersion $ReadyVersion)) {
    throw "Updated nanoctl exited during its startup stability window. $(Get-AgentFailureDetails)"
  }
}

try {
  $lockStream = [IO.File]::Open(
    $lockPath,
    [IO.FileMode]::OpenOrCreate,
    [IO.FileAccess]::ReadWrite,
    [IO.FileShare]::None
  )
  $lockAcquired = $true
  $task = Get-ScheduledTask -TaskName $taskName -ErrorAction Stop
  Assert-TaskOwner -Task $task
  Export-ScheduledTask -TaskName $taskName |
    Set-Content -LiteralPath $previousTaskXml -Encoding UTF8
  $taskBackedUp = $true
  Stop-AgentTask
  Wait-AgentProcessExit

  $stageOutput = & $resolvedBinary `
    --config $resolvedConfig `
    stage-update $resolvedManifest `
    --public-key $PublicKey `
    --json
  if ($LASTEXITCODE -ne 0) {
    throw "nanoctl rejected or could not stage the update."
  }
  $stage = $stageOutput | ConvertFrom-Json
  $stagedPath = [string]$stage.path
  if (-not (Test-Path -LiteralPath $stagedPath -PathType Leaf)) {
    throw "nanoctl did not produce a regular staged update."
  }

  Copy-Item -LiteralPath $stagedPath -Destination $candidate
  $candidateFile = Get-Item -LiteralPath $candidate
  $candidateHash = (Get-FileHash -LiteralPath $candidate -Algorithm SHA256).Hash
  if ($candidateFile.Length -ne [Int64]$stage.artifact.size -or
      $candidateHash -ine [string]$stage.artifact.sha256) {
    throw "Copied update bytes do not match the signed manifest."
  }
  $candidateVersionOutput = (& $candidate --log-file $logPath --version 2>&1 | Out-String).Trim()
  if ($LASTEXITCODE -ne 0 -or
      $candidateVersionOutput -notmatch '(?m)^nanoctl\s+(\d+\.\d+\.\d+)\s*$') {
    throw "The staged nanoctl executable is incompatible with the headless service runner."
  }
  $candidateVersion = $Matches[1]

  Move-Item -LiteralPath $resolvedBinary -Destination $previous
  try {
    Move-Item -LiteralPath $candidate -Destination $resolvedBinary
  }
  catch {
    Move-Item -LiteralPath $previous -Destination $resolvedBinary
    throw
  }
  $activated = $true
  Set-HeadlessTaskAction
  Start-ScheduledTask -TaskName $taskName
  Wait-AgentReady -BinaryPath $resolvedBinary -ReadyVersion $candidateVersion

  & $resolvedBinary --config $resolvedConfig doctor
  if ($LASTEXITCODE -ne 0) {
    throw "Updated nanoctl failed its health check."
  }

  if (-not (Get-ReadyAgentProcess `
      -BinaryPath $resolvedBinary `
      -ReadyToken $transactionId `
      -ReadyVersion $candidateVersion)) {
    throw "Updated nanoctl did not remain ready during its startup stability window. $(Get-AgentFailureDetails)"
  }

  $completed = $true
  Remove-Item -LiteralPath $previous -Force -ErrorAction SilentlyContinue
  Remove-Item -LiteralPath $stagedPath -ErrorAction SilentlyContinue
  Remove-Item -LiteralPath $previousTaskXml -Force -ErrorAction SilentlyContinue
  Write-Host "nanoctl update activated and committed."
}
finally {
  if (-not $completed -and $lockAcquired) {
    try {
      Stop-AgentTask
      Wait-AgentProcessExit
      if ($activated -and (Test-Path -LiteralPath $previous -PathType Leaf)) {
        Move-Item -LiteralPath $resolvedBinary -Destination $failed
        try {
          Move-Item -LiteralPath $previous -Destination $resolvedBinary
        }
        catch {
          Move-Item -LiteralPath $failed -Destination $resolvedBinary
          throw
        }
      }
      Restore-PreviousTask
      Remove-Item -LiteralPath $candidate -ErrorAction SilentlyContinue
      Start-ScheduledTask -TaskName $taskName -ErrorAction Stop
    }
    catch {
      Write-Warning "nanoctl rollback could not fully restore the previous task and binary: $($_.Exception.Message)"
    }
  }
  if ($lockStream) {
    $lockStream.Dispose()
    $lockStream = $null
  }
  if ($lockAcquired) {
    Remove-Item -LiteralPath $lockPath -Force -ErrorAction SilentlyContinue
  }
}
