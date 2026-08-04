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
$resolvedManifest = (Resolve-Path -LiteralPath $ManifestPath).Path
$installRoot = Split-Path -Parent $resolvedBinary
$transactionId = [Guid]::NewGuid().ToString("N")
$candidate = "$resolvedBinary.$transactionId.candidate"
$previous = "$resolvedBinary.$transactionId.previous"
$failed = "$resolvedBinary.$transactionId.failed"
$runnerPath = Join-Path $installRoot "run-agent.vbs"
$logPath = Join-Path $installRoot "agent.log"
$readyPath = Join-Path $env:LOCALAPPDATA "nanoctl\agent.ready"
$lockPath = "$resolvedBinary.update.lock"
$lockStream = $null
$lockAcquired = $false
$activated = $false
$completed = $false

New-Item -ItemType Directory -Path (Split-Path -Parent $readyPath) -Force | Out-Null

function Ensure-HeadlessRunner {
  param([Parameter(Mandatory = $true)][string]$Path)

  $runner = @'
Option Explicit
On Error Resume Next

Dim shell, fso, binaryPath, configPath, logPath, readyPath, inner, command, exitCode
Set shell = CreateObject("WScript.Shell")
Set fso = CreateObject("Scripting.FileSystemObject")
binaryPath = WScript.Arguments(0)
configPath = WScript.Arguments(1)
logPath = WScript.Arguments(2)
readyPath = WScript.Arguments(3)
If fso.FileExists(readyPath) Then fso.DeleteFile readyPath, True
inner = Chr(34) & binaryPath & Chr(34) & " --config " & Chr(34) & configPath & Chr(34) & _
  " --ready-file " & Chr(34) & readyPath & Chr(34) & " run >> " & Chr(34) & logPath & Chr(34) & " 2>&1"
command = "cmd.exe /d /s /c " & Chr(34) & inner & Chr(34)
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

function Set-HeadlessTaskAction {
  Ensure-HeadlessRunner -Path $runnerPath
  $wscript = Join-Path $env:SystemRoot "System32\wscript.exe"
  $arguments = '//B //NoLogo "{0}" "{1}" "{2}" "{3}" "{4}"' -f `
    $runnerPath, $resolvedBinary, $resolvedConfig, $logPath, $readyPath
  $action = New-ScheduledTaskAction -Execute $wscript -Argument $arguments
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

function Wait-AgentProcessExit {
  for ($attempt = 0; $attempt -lt 50; $attempt++) {
    $running = @(Get-Process -Name nanoctl -ErrorAction SilentlyContinue | Where-Object {
        $_.Path -eq $resolvedBinary
      })
    if ($running.Count -eq 0) { return }
    Start-Sleep -Milliseconds 100
  }
  throw "Timed out while waiting for the previous nanoctl process to exit."
}

function Wait-AgentReady {
  for ($attempt = 0; $attempt -lt 150; $attempt++) {
    $task = Get-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue
    if ($task -and $task.State -eq "Running" -and
        (Test-Path -LiteralPath $readyPath -PathType Leaf)) {
      return
    }
    Start-Sleep -Milliseconds 100
  }
  $taskInfo = Get-ScheduledTaskInfo -TaskName $taskName -ErrorAction SilentlyContinue
  throw "Updated nanoctl did not become ready (task result: $($taskInfo.LastTaskResult))."
}

try {
  $lockStream = [IO.File]::Open(
    $lockPath,
    [IO.FileMode]::OpenOrCreate,
    [IO.FileAccess]::ReadWrite,
    [IO.FileShare]::None
  )
  $lockAcquired = $true
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

  Move-Item -LiteralPath $resolvedBinary -Destination $previous
  try {
    Move-Item -LiteralPath $candidate -Destination $resolvedBinary
  }
  catch {
    Move-Item -LiteralPath $previous -Destination $resolvedBinary
    throw
  }
  $activated = $true
  Remove-Item -LiteralPath $readyPath -Force -ErrorAction SilentlyContinue
  Set-HeadlessTaskAction
  Start-ScheduledTask -TaskName $taskName
  Wait-AgentReady

  & $resolvedBinary --config $resolvedConfig doctor
  if ($LASTEXITCODE -ne 0) {
    throw "Updated nanoctl failed its health check."
  }

  Start-Sleep -Seconds 5
  if ((Get-ScheduledTask -TaskName $taskName).State -ne "Running" -or
      -not (Test-Path -LiteralPath $readyPath -PathType Leaf)) {
    throw "Updated nanoctl did not remain ready during its startup stability window."
  }

  Remove-Item -LiteralPath $previous -Force
  Remove-Item -LiteralPath $stagedPath -ErrorAction SilentlyContinue
  $completed = $true
  Write-Host "nanoctl update activated and committed."
}
finally {
  if (-not $completed -and $lockAcquired) {
    try {
      Stop-AgentTask
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
      Remove-Item -LiteralPath $candidate -ErrorAction SilentlyContinue
    }
  finally {
      Start-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue
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
