[CmdletBinding()]
param()

$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"
$Repository = if ($env:NANOCTL_REPOSITORY) { $env:NANOCTL_REPOSITORY } else { "extoci/nanoctl" }
$Version = if ($env:NANOCTL_VERSION) { $env:NANOCTL_VERSION } else { "latest" }
$ControlPlane = if ($env:NANOCTL_CONTROL_PLANE) {
  $env:NANOCTL_CONTROL_PLANE
} else {
  "https://nanoctl.vercel.app"
}

if (-not $IsWindows -and $PSVersionTable.PSEdition -eq "Core") {
  throw "This installer supports Windows. On Linux or macOS, use install.sh."
}

$architecture = if ($env:PROCESSOR_ARCHITEW6432) {
  $env:PROCESSOR_ARCHITEW6432
} else {
  $env:PROCESSOR_ARCHITECTURE
}
$target = switch ($architecture.ToUpperInvariant()) {
  "AMD64" { "windows-x64" }
  "ARM64" { "windows-arm64" }
  default { throw "Unsupported Windows architecture: $architecture" }
}
$baseUrl = if ($Version -eq "latest") {
  "https://github.com/$Repository/releases/latest/download"
} else {
  "https://github.com/$Repository/releases/download/$Version"
}
$asset = "nanoctl-$target.exe"
$temporary = Join-Path ([IO.Path]::GetTempPath()) ("nanoctl-" + [Guid]::NewGuid().ToString("N"))
$installRoot = Join-Path $env:LOCALAPPDATA "nanoctl"
$binaryPath = Join-Path $installRoot "nanoctl.exe"
$candidatePath = Join-Path $installRoot "nanoctl.installing.exe"
$taskName = "nanoctl Agent"
$currentUser = [Security.Principal.WindowsIdentity]::GetCurrent().Name
$transactionId = [Guid]::NewGuid().ToString("N")
$previousPath = Join-Path $installRoot ("nanoctl.{0}.previous.exe" -f $transactionId)
$failedPath = Join-Path $installRoot ("nanoctl.{0}.failed.exe" -f $transactionId)
$runnerPath = Join-Path $installRoot "run-agent.vbs"
$logPath = Join-Path $installRoot "agent.log"
$readyPath = Join-Path $installRoot "agent.ready"
$lockPath = Join-Path $installRoot "install.lock"
$legacyTaskXmlPath = Join-Path $temporary "legacy-task.xml"
$lockStream = $null
$lockAcquired = $false
$activated = $false
$completed = $false
$configPath = $null
$existingTask = $null

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

function New-HeadlessTaskAction {
  param([Parameter(Mandatory = $true)][string]$ConfigPath)

  Ensure-HeadlessRunner -Path $runnerPath
  $wscript = Join-Path $env:SystemRoot "System32\wscript.exe"
  $arguments = '//B //NoLogo "{0}" "{1}" "{2}" "{3}" "{4}"' -f `
    $runnerPath, $binaryPath, $ConfigPath, $logPath, $readyPath
  New-ScheduledTaskAction -Execute $wscript -Argument $arguments
}

function Register-AgentTask {
  param([Parameter(Mandatory = $true)][string]$ConfigPath)

  if (Get-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue) {
    Unregister-ScheduledTask -TaskName $taskName -Confirm:$false
  }
  $action = New-HeadlessTaskAction -ConfigPath $ConfigPath
  $trigger = New-ScheduledTaskTrigger -AtLogOn -User $currentUser
  $principal = New-ScheduledTaskPrincipal `
    -UserId $currentUser `
    -LogonType Interactive `
    -RunLevel Limited
  # Task Scheduler requires RestartOnFailure intervals of at least one minute.
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
    -Description "nanoctl remote desktop agent for the current desktop user." | Out-Null
  $script:taskReplaced = $true
}

function Wait-AgentTask {
  param([Parameter(Mandatory = $true)][string]$ReadyPath)

  Start-ScheduledTask -TaskName $taskName
  Start-Sleep -Seconds 2
  $startedTask = Get-ScheduledTask -TaskName $taskName
  if ($startedTask.State -ne "Running" -or -not (Test-Path -LiteralPath $ReadyPath -PathType Leaf)) {
    $taskInfo = Get-ScheduledTaskInfo -TaskName $taskName
    throw "The nanoctl agent did not become ready (task result: $($taskInfo.LastTaskResult)). Run '$binaryPath doctor' for diagnostics."
  }
  # A task can briefly report Running while the process is still starting. Keep the previous
  # binary until this short stability window has passed.
  Start-Sleep -Seconds 3
  $stableTask = Get-ScheduledTask -TaskName $taskName
  if ($stableTask.State -ne "Running" -or -not (Test-Path -LiteralPath $ReadyPath -PathType Leaf)) {
    throw "The nanoctl agent exited during its startup stability window. Run '$binaryPath doctor' for diagnostics."
  }
}

function Wait-AgentProcessExit {
  param([Parameter(Mandatory = $true)][string[]]$Paths)

  for ($attempt = 0; $attempt -lt 50; $attempt++) {
    $running = @(Get-Process -Name nanoctl -ErrorAction SilentlyContinue | Where-Object {
        $processPath = $_.Path
        $processPath -and ($Paths -contains $processPath)
      })
    if ($running.Count -eq 0) { return }
    Start-Sleep -Milliseconds 100
  }
  throw "Timed out while waiting for the previous nanoctl process to exit."
}

function Get-TaskConfigPath {
  param($Task)

  foreach ($action in @($Task.Actions)) {
    $arguments = [string]$action.Arguments
    if ($arguments -match '(?i)run-agent\.vbs"\s+"[^"]+"\s+"([^"]+)"') {
      return $Matches[1]
    }
    if ($arguments -match '(?i)(?:--config|-ConfigPath)\s+"([^"]+)"') {
      return $Matches[1]
    }
    if ($arguments -match '(?i)(?:--config|-ConfigPath)\s+([^\s]+)') {
      return $Matches[1]
    }
  }
  return $null
}

function Get-TaskBinaryPath {
  param($Task)

  foreach ($action in @($Task.Actions)) {
    $arguments = [string]$action.Arguments
    if ($arguments -match '(?i)run-agent\.vbs"\s+"([^"]+)"') {
      return $Matches[1]
    }
    if ($arguments -match '(?i)(?:-BinaryPath)\s+"([^"]+)"') {
      return $Matches[1]
    }
    $execute = [string]$action.Execute
    if ($execute -match '(?i)\\nanoctl\.exe$') {
      return $execute
    }
  }
  return $null
}

function Restore-LegacyTask {
  if (-not (Test-Path -LiteralPath $legacyTaskXmlPath -PathType Leaf)) {
    return $false
  }
  $xml = Get-Content -LiteralPath $legacyTaskXmlPath -Raw
  Register-ScheduledTask -TaskName $taskName -Xml $xml -Force | Out-Null
  return $true
}

function Start-RestoredTask {
  Start-ScheduledTask -TaskName $taskName
  Start-Sleep -Seconds 2
  $restored = Get-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue
  if (-not $restored -or $restored.State -ne "Running") {
    throw "The previous nanoctl task could not be restarted."
  }
}

try {
  New-Item -ItemType Directory -Path $installRoot -Force | Out-Null
  $lockStream = [IO.File]::Open(
    $lockPath,
    [IO.FileMode]::OpenOrCreate,
    [IO.FileAccess]::ReadWrite,
    [IO.FileShare]::None
  )
  $lockAcquired = $true
  New-Item -ItemType Directory -Path $temporary | Out-Null
  $download = Join-Path $temporary "nanoctl.exe"
  $checksum = Join-Path $temporary "nanoctl.exe.sha256"
  Write-Host "Downloading nanoctl for $target..."
  Invoke-WebRequest -UseBasicParsing "$baseUrl/$asset" -OutFile $download
  Invoke-WebRequest -UseBasicParsing "$baseUrl/$asset.sha256" -OutFile $checksum

  $expected = ((Get-Content -LiteralPath $checksum -Raw) -split "\s+")[0]
  if ($expected -notmatch "^[0-9a-fA-F]{64}$") {
    throw "The release checksum is invalid."
  }
  $actual = (Get-FileHash -LiteralPath $download -Algorithm SHA256).Hash
  if ($actual -ine $expected) {
    throw "The release checksum did not match."
  }
  & $download --version | Out-Null
  if ($LASTEXITCODE -ne 0) {
    throw "The downloaded nanoctl executable did not start."
  }

  $existingTask = Get-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue
  if ($existingTask) {
    Export-ScheduledTask -TaskName $taskName | Set-Content -LiteralPath $legacyTaskXmlPath -Encoding UTF8
  }
  $legacyConfigPath = if ($existingTask) { Get-TaskConfigPath -Task $existingTask } else { $null }
  $legacyBinaryPath = if ($existingTask) { Get-TaskBinaryPath -Task $existingTask } else { $null }
  if ($existingTask) {
    Stop-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue
    for ($attempt = 0; $attempt -lt 50; $attempt++) {
      $state = (Get-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue).State
      if ($state -ne "Running") { break }
      Start-Sleep -Milliseconds 100
    }
    Wait-AgentProcessExit -Paths @($binaryPath, $legacyBinaryPath)
  }

  New-Item -ItemType Directory -Path $installRoot -Force | Out-Null
  Copy-Item -LiteralPath $download -Destination $candidatePath -Force
  if (Test-Path -LiteralPath $binaryPath -PathType Leaf) {
    Move-Item -LiteralPath $binaryPath -Destination $previousPath
  }
  Move-Item -LiteralPath $candidatePath -Destination $binaryPath -Force
  $activated = $true

  $pathOutput = & $binaryPath paths
  if ($LASTEXITCODE -ne 0 -or $pathOutput -notmatch "^config=(.+)$") {
    throw "The installed binary returned an invalid configuration path."
  }
  $configPath = $Matches[1]
  if ($legacyConfigPath -and (Test-Path -LiteralPath $legacyConfigPath -PathType Leaf) -and
      -not (Test-Path -LiteralPath $configPath -PathType Leaf)) {
    New-Item -ItemType Directory -Path (Split-Path -Parent $configPath) -Force | Out-Null
    Copy-Item -LiteralPath $legacyConfigPath -Destination $configPath
    Write-Host "Migrated the existing configuration from $legacyConfigPath."
  }
  if (-not (Test-Path -LiteralPath $configPath -PathType Leaf)) {
    $setupCode = $env:NANOCTL_ENROLL_CODE
    if (-not $setupCode) {
      $setupCode = Read-Host "Setup code"
    }
    if (-not $setupCode) {
      throw "The setup code cannot be empty."
    }
    & $binaryPath enroll $setupCode --control-plane $ControlPlane
    if ($LASTEXITCODE -ne 0) {
      throw "nanoctl enrollment failed."
    }
  }

  Register-AgentTask -ConfigPath $configPath
  Wait-AgentTask -ReadyPath $readyPath
  if (Test-Path -LiteralPath $previousPath -PathType Leaf) {
    Remove-Item -LiteralPath $previousPath -Force -ErrorAction SilentlyContinue
  }
  $completed = $true

  $userPath = [Environment]::GetEnvironmentVariable("Path", "User")
  $pathEntries = @($userPath -split ";" | Where-Object { $_ })
  if ($pathEntries -notcontains $installRoot) {
    $newPath = (@($pathEntries) + $installRoot) -join ";"
    [Environment]::SetEnvironmentVariable("Path", $newPath, "User")
  }
  # Persistent environment changes are inherited only by new Windows processes. Always update the
  # PowerShell process running the installer too, including when this is a reinstall and the
  # persistent user PATH entry already exists.
  $processPathEntries = @($env:Path -split ";" | Where-Object { $_ })
  if ($processPathEntries -notcontains $installRoot) {
    $env:Path = "$installRoot;$env:Path"
  }

  Write-Host ""
  Write-Host "nanoctl is installed, enrolled, and running."
  Write-Host "nanoctl is available in this PowerShell session and in newly opened terminals."
  Write-Host "Run this installer again at any time to update."
}
catch {
  try {
    if ($script:taskReplaced -and (Get-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue)) {
      Stop-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue
      Unregister-ScheduledTask -TaskName $taskName -Confirm:$false -ErrorAction SilentlyContinue
    }
    if (Test-Path -LiteralPath $previousPath -PathType Leaf) {
      if (Test-Path -LiteralPath $binaryPath -PathType Leaf) {
        Move-Item -LiteralPath $binaryPath -Destination $failedPath -Force -ErrorAction SilentlyContinue
      }
      Move-Item -LiteralPath $previousPath -Destination $binaryPath -Force
    } elseif ($activated) {
      Remove-Item -LiteralPath $binaryPath -Force -ErrorAction SilentlyContinue
    }
    if ($existingTask) {
      if (Restore-LegacyTask) {
        Start-RestoredTask
      } elseif (Get-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue) {
        Start-ScheduledTask -TaskName $taskName
      }
    }
  } catch {
    Write-Warning "nanoctl rollback could not fully restore the previous task and binary: $($_.Exception.Message)"
  }
  throw
}
finally {
  if (-not $completed -and (Test-Path -LiteralPath $candidatePath -PathType Leaf)) {
    Remove-Item -LiteralPath $candidatePath -Force -ErrorAction SilentlyContinue
  }
  if ($lockStream) {
    $lockStream.Dispose()
    $lockStream = $null
  }
  if ($lockAcquired) {
    Remove-Item -LiteralPath $lockPath -Force -ErrorAction SilentlyContinue
  }
  Remove-Item -LiteralPath $temporary -Recurse -Force -ErrorAction SilentlyContinue
}
