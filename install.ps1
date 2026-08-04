[CmdletBinding()]
param()

$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"
$Repository = if ($env:NANOCTL_REPOSITORY) { $env:NANOCTL_REPOSITORY } else { "extoci/nanoctl" }
$RequestedVersion = if ($env:NANOCTL_VERSION) { $env:NANOCTL_VERSION.Trim() } else { "latest" }
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
$resolvedVersion = if ($RequestedVersion -eq "latest") {
  $release = Invoke-RestMethod `
    -UseBasicParsing `
    -Headers @{ Accept = "application/vnd.github+json"; "User-Agent" = "nanoctl-installer" } `
    -Uri "https://api.github.com/repos/$Repository/releases/latest"
  [string]$release.tag_name
} elseif ($RequestedVersion.StartsWith("v")) {
  $RequestedVersion
} else {
  "v$RequestedVersion"
}
if ($resolvedVersion -notmatch "^v\d+\.\d+\.\d+$") {
  throw "The release version '$resolvedVersion' is not a stable nanoctl version."
}
$displayVersion = $resolvedVersion.Substring(1)
$baseUrl = "https://github.com/$Repository/releases/download/$resolvedVersion"
$asset = "nanoctl-$target.exe"
$temporary = Join-Path ([IO.Path]::GetTempPath()) ("nanoctl-" + [Guid]::NewGuid().ToString("N"))
$installRoot = Join-Path $env:LOCALAPPDATA "nanoctl"
$binaryPath = Join-Path $installRoot "nanoctl.exe"
$candidatePath = Join-Path $installRoot "nanoctl.installing.exe"
$taskName = "nanoctl Agent"
$currentIdentity = [Security.Principal.WindowsIdentity]::GetCurrent()
$currentUser = $currentIdentity.Name
$currentUserSid = $currentIdentity.User.Value
$transactionId = [Guid]::NewGuid().ToString("N")
$previousPath = Join-Path $installRoot ("nanoctl.{0}.previous.exe" -f $transactionId)
$failedPath = Join-Path $installRoot ("nanoctl.{0}.failed.exe" -f $transactionId)
$runnerPath = Join-Path $installRoot "run-agent.vbs"
$logPath = Join-Path $env:LOCALAPPDATA "nanoctl\agent.log"
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

function New-HeadlessTaskAction {
  param([Parameter(Mandatory = $true)][string]$ConfigPath)

  Ensure-HeadlessRunner -Path $runnerPath
  $wscript = Join-Path $env:SystemRoot "System32\wscript.exe"
  $arguments = '//B //NoLogo "{0}" "{1}" "{2}" "{3}" "{4}" "{5}"' -f `
    $runnerPath, $binaryPath, $ConfigPath, $logPath, $readyPath, $transactionId
  New-ScheduledTaskAction -Execute $wscript -Argument $arguments -WorkingDirectory $installRoot
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

function Get-AgentFailureDetails {
  $task = Get-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue
  $taskInfo = Get-ScheduledTaskInfo -TaskName $taskName -ErrorAction SilentlyContinue
  $action = if ($task) {
    (@($task.Actions) | ForEach-Object { "$($_.Execute) $($_.Arguments)" }) -join "`n"
  } else {
    "(task definition unavailable)"
  }
  $logTail = if (Test-Path -LiteralPath $logPath -PathType Leaf) {
    (Get-Content -LiteralPath $logPath -Tail 40 -ErrorAction SilentlyContinue | Out-String).Trim()
  } else {
    "(agent log was not created at $logPath)"
  }
  "task state: $($task.State)`nprincipal: $($task.Principal.UserId)`naction: $action`nlast run: $($taskInfo.LastRunTime)`nlast result: $($taskInfo.LastTaskResult)`nlog: $logPath`n$logTail"
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
    # Readiness is fail-closed: without an executable path, the marker cannot prove that this
    # transaction's binary is the live process.
    return $null
  }
  return $process
}

function Wait-AgentTask {
  param(
    [Parameter(Mandatory = $true)][string]$ReadyPath,
    [Parameter(Mandatory = $true)][string]$BinaryPath,
    [Parameter(Mandatory = $true)][string]$ReadyToken,
    [Parameter(Mandatory = $true)][string]$ReadyVersion
  )

  if (Test-Path -LiteralPath $ReadyPath -PathType Leaf) {
    Remove-Item -LiteralPath $ReadyPath -Force -ErrorAction Stop
  }
  Start-ScheduledTask -TaskName $taskName
  $deadline = [DateTime]::UtcNow.AddSeconds(90)
  $readyProcess = $null
  while ([DateTime]::UtcNow -lt $deadline) {
    $readyProcess = Get-ReadyAgentProcess `
      -ReadyPath $ReadyPath `
      -BinaryPath $BinaryPath `
      -ReadyToken $ReadyToken `
      -ReadyVersion $ReadyVersion
    if ($readyProcess) {
      break
    }
    Start-Sleep -Milliseconds 200
  }
  if (-not $readyProcess) {
    throw "The nanoctl agent did not become ready. $((Get-AgentFailureDetails))"
  }
  # Start-ScheduledTask is asynchronous. The agent's PID marker is authoritative; Task Scheduler
  # can report Ready while a windowless runner owns the live child process.
  Start-Sleep -Seconds 5
  if (-not (Get-ReadyAgentProcess `
      -ReadyPath $ReadyPath `
      -BinaryPath $BinaryPath `
      -ReadyToken $ReadyToken `
      -ReadyVersion $ReadyVersion)) {
    throw "The nanoctl agent exited during its startup stability window. $((Get-AgentFailureDetails))"
  }
}

function Wait-AgentProcessExit {
  param([Parameter(Mandatory = $true)][string[]]$Paths)

  for ($attempt = 0; $attempt -lt 300; $attempt++) {
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

function Get-BinaryConfigPath {
  param([string]$Path)

  if (-not $Path -or -not (Test-Path -LiteralPath $Path -PathType Leaf)) {
    return $null
  }
  $pathOutput = & $Path paths 2>$null
  if ($LASTEXITCODE -eq 0) {
    foreach ($line in @($pathOutput)) {
      if ([string]$line -match "^config=(.+)$") {
        return $Matches[1].Trim()
      }
    }
  }
  return $null
}

function Test-ConfigEnrolled {
  param([string]$Path)

  if (-not $Path -or -not (Test-Path -LiteralPath $Path -PathType Leaf)) {
    return $false
  }
  $contents = Get-Content -LiteralPath $Path -Raw -ErrorAction SilentlyContinue
  return [bool]($contents -match '(?m)^\s*device_id\s*=\s*["''][^"'']+["'']\s*$')
}

function Assert-ConfigOwner {
  param([Parameter(Mandatory = $true)][string]$Path)

  if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
    return
  }
  $owner = (Get-Acl -LiteralPath $Path).Owner
  try {
    $ownerSid = Resolve-AccountSid -Account $owner
  } catch {
    throw "Could not resolve the enrolled configuration owner '$owner'. Run the installer as the account that enrolled nanoctl."
  }
  if ($ownerSid -eq $currentUserSid) {
    return
  }
  if ($ownerSid -eq "S-1-5-32-544" -and (Test-CurrentUserAdministrator)) {
    return
  }
  if ($ownerSid -eq "S-1-5-32-544") {
    throw (
      "The enrolled configuration is owned by 'BUILTIN\Administrators', but '$currentUser' " +
      "is not running with an administrator token. Run the installer once from an elevated " +
      "PowerShell window to repair this legacy enrollment."
    )
  }
  if ($ownerSid -ne $currentUserSid) {
    throw (
      "The enrolled configuration belongs to '$owner', but this installer is running as '$currentUser'. " +
      "Sign in as the account that enrolled nanoctl; do not update another Windows user's installation."
    )
  }
}

function Assert-ExistingTaskOwner {
  param($Task)

  $taskUser = [string]$Task.Principal.UserId
  if (-not $taskUser -or $taskUser -match '(?i)^SYSTEM$|^NT AUTHORITY\\') {
    throw "The existing nanoctl task is not a per-user interactive task. Remove the managed installation with its administrator script before using this installer."
  }
  $taskLogonType = [string]$Task.Principal.LogonType
  if ($taskLogonType -and $taskLogonType -notmatch '(?i)^Interactive$') {
    throw "The existing nanoctl task does not run in the interactive user session; refusing to migrate it."
  }
  try {
    $taskSid = Resolve-AccountSid -Account $taskUser
  } catch {
    throw "Could not resolve the existing nanoctl task owner '$taskUser'. Run the installer as the account that owns the task."
  }
  if ($taskSid -ne $currentUserSid) {
    throw (
      "The existing nanoctl task belongs to '$taskUser', but this installer is running as '$currentUser'. " +
      "Sign in as the account that owns the task before upgrading."
    )
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

function Ensure-ConfigUserAccess {
  param([Parameter(Mandatory = $true)][string]$Path)

  if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
    return
  }
  $owner = (Get-Acl -LiteralPath $Path).Owner
  $ownerSid = Resolve-AccountSid -Account $owner
  if ($ownerSid -ne "S-1-5-32-544") {
    return
  }
  & icacls.exe $Path `
    /grant:r "*${currentUserSid}:(F)" `
    /quiet | Out-Null
  if ($LASTEXITCODE -ne 0) {
    throw (
      "The enrolled configuration is owned by 'BUILTIN\Administrators' and could not be " +
      "made writable by '$currentUser'. Run the installer from an elevated PowerShell window."
    )
  }
}

function Set-OwnerProtectedAcl {
  & icacls.exe $installRoot `
    /inheritance:r `
    /grant:r "*${currentUserSid}:(OI)(CI)(M)" "*S-1-5-18:(OI)(CI)(F)" `
    /quiet | Out-Null
  if ($LASTEXITCODE -ne 0) {
    throw "Could not protect the nanoctl installation directory."
  }
  & icacls.exe $logPath `
    /inheritance:r `
    /grant:r "*${currentUserSid}:(M)" "*S-1-5-18:(F)" `
    /quiet | Out-Null
  if ($LASTEXITCODE -ne 0) {
    throw "Could not protect the nanoctl agent log."
  }
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
  New-Item -ItemType File -Path $logPath -Force | Out-Null
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
  Write-Host "Downloading nanoctl $displayVersion for $target..."
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
  $downloadVersion = (& $download --version 2>&1 | Out-String).Trim()
  if ($LASTEXITCODE -ne 0) {
    throw "The downloaded nanoctl executable did not start."
  }
  if ($downloadVersion -notmatch "(?m)^nanoctl\s+$([regex]::Escape($displayVersion))\s*$") {
    throw "The downloaded nanoctl version '$downloadVersion' does not match the requested release $displayVersion."
  }
  $probeLogPath = Join-Path $temporary "log-probe.txt"
  & $download --log-file $probeLogPath --version | Out-Null
  if ($LASTEXITCODE -ne 0) {
    throw "The downloaded nanoctl executable is incompatible with this installer."
  }
  Remove-Item -LiteralPath $probeLogPath -Force -ErrorAction SilentlyContinue

  $existingTask = Get-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue
  if ($existingTask) {
    Assert-ExistingTaskOwner -Task $existingTask
    Export-ScheduledTask -TaskName $taskName | Set-Content -LiteralPath $legacyTaskXmlPath -Encoding UTF8
  }
  $legacyConfigPath = if ($existingTask) { Get-TaskConfigPath -Task $existingTask } else { $null }
  $legacyBinaryPath = if ($existingTask) { Get-TaskBinaryPath -Task $existingTask } else { $null }
  if (-not $legacyConfigPath) {
    $legacyConfigPath = Get-BinaryConfigPath -Path $legacyBinaryPath
  }
  if ($legacyConfigPath -and (Test-ConfigEnrolled -Path $legacyConfigPath)) {
    Assert-ConfigOwner -Path $legacyConfigPath
    Ensure-ConfigUserAccess -Path $legacyConfigPath
  }
  # Do not change an existing installation's ACL until its task/configuration ownership has
  # passed the migration checks above. A rejected cross-user upgrade must be side-effect free.
  Set-OwnerProtectedAcl
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
  $defaultConfigPath = $Matches[1].Trim()
  $configPath = $defaultConfigPath
  if ($legacyConfigPath -and (Test-Path -LiteralPath $legacyConfigPath -PathType Leaf)) {
    if (Test-ConfigEnrolled -Path $legacyConfigPath) {
      # Preserve an explicit v1.0.9 --config location. A new default config can exist from a
      # partially failed attempt and must not silently replace the enrolled configuration.
      $configPath = (Resolve-Path -LiteralPath $legacyConfigPath).Path
      if ($configPath -ne $defaultConfigPath) {
        Write-Host "Preserving the existing configuration at $configPath."
      }
    } elseif (-not (Test-ConfigEnrolled -Path $defaultConfigPath)) {
      New-Item -ItemType Directory -Path (Split-Path -Parent $defaultConfigPath) -Force | Out-Null
      Copy-Item -LiteralPath $legacyConfigPath -Destination $defaultConfigPath
      Write-Host "Migrated the existing configuration from $legacyConfigPath."
    }
  }
  Assert-ConfigOwner -Path $configPath
  Ensure-ConfigUserAccess -Path $configPath
  if (-not (Test-ConfigEnrolled -Path $configPath)) {
    $setupCode = $env:NANOCTL_ENROLL_CODE
    if (-not $setupCode) {
      $setupCode = Read-Host "Setup code"
    }
    if (-not $setupCode) {
      throw "The setup code cannot be empty."
    }
    & $binaryPath --config $configPath enroll $setupCode --control-plane $ControlPlane
    if ($LASTEXITCODE -ne 0) {
      throw "nanoctl enrollment failed."
    }
  }

  Register-AgentTask -ConfigPath $configPath
  Wait-AgentTask `
    -ReadyPath $readyPath `
    -BinaryPath $binaryPath `
    -ReadyToken $transactionId `
    -ReadyVersion $displayVersion
  & $binaryPath --config $configPath doctor
  if ($LASTEXITCODE -ne 0) {
    throw "nanoctl installed but failed its health check. See $logPath for service diagnostics."
  }

  try {
    $userPath = [Environment]::GetEnvironmentVariable("Path", "User")
    $pathEntries = @($userPath -split ";" | Where-Object {
        $_ -and -not [String]::Equals($_, $installRoot, [StringComparison]::OrdinalIgnoreCase)
      })
    $newPath = (@($installRoot) + $pathEntries) -join ";"
    [Environment]::SetEnvironmentVariable("Path", $newPath, "User")
    # Persistent environment changes are inherited only by new Windows processes. Always update the
    # PowerShell process running the installer too, and move the install root ahead of any stale
    # 1.0.9 entry that may already be present in this process's PATH.
    $processPathEntries = @($env:Path -split ";" | Where-Object {
        $_ -and -not [String]::Equals($_, $installRoot, [StringComparison]::OrdinalIgnoreCase)
      })
    $env:Path = (@($installRoot) + $processPathEntries) -join ";"
  } catch {
    Write-Warning "nanoctl is running, but the installer could not update PATH: $($_.Exception.Message)"
  }

  $resolvedCommand = Get-Command nanoctl -All -ErrorAction SilentlyContinue | Select-Object -First 1
  $resolvedCommandPath = if ($resolvedCommand) {
    if ($resolvedCommand.Source) { [string]$resolvedCommand.Source } else { [string]$resolvedCommand.Path }
  } else {
    $null
  }
  $resolvedCommandMatchesInstall = $false
  if ($resolvedCommandPath) {
    try {
      $resolvedCommandMatchesInstall = [String]::Equals(
        [IO.Path]::GetFullPath($resolvedCommandPath),
        [IO.Path]::GetFullPath($binaryPath),
        [StringComparison]::OrdinalIgnoreCase
      )
    } catch {
      $resolvedCommandMatchesInstall = $false
    }
  }
  if (-not $resolvedCommandMatchesInstall) {
    throw (
      "nanoctl installed at '$binaryPath', but command resolution still points to " +
      "'$resolvedCommandPath'. Close this shell and remove any older nanoctl entry from PATH."
    )
  }
  $resolvedCommandVersion = (& $resolvedCommandPath --version 2>&1 | Out-String).Trim()
  if ($LASTEXITCODE -ne 0 -or
      $resolvedCommandVersion -notmatch "(?m)^nanoctl\s+$([regex]::Escape($displayVersion))\s*$") {
    throw "The installed nanoctl command did not report version $displayVersion."
  }

  if (Test-Path -LiteralPath $previousPath -PathType Leaf) {
    Remove-Item -LiteralPath $previousPath -Force -ErrorAction SilentlyContinue
  }
  $completed = $true

  Write-Host ""
  Write-Host "nanoctl is installed, enrolled, and running."
  Write-Host "Installed version: $(& $binaryPath --version)"
  Write-Host "Command path: $resolvedCommandPath"
  Write-Host "nanoctl is available in this PowerShell session and in newly opened terminals."
  Write-Host "Run this installer again at any time to update."
}
catch {
  try {
    if ($script:taskReplaced -and (Get-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue)) {
      Stop-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue
      Unregister-ScheduledTask -TaskName $taskName -Confirm:$false -ErrorAction SilentlyContinue
    }
    if ($activated) {
      Wait-AgentProcessExit -Paths @($binaryPath)
    }
    if (Test-Path -LiteralPath $previousPath -PathType Leaf) {
      if (Test-Path -LiteralPath $binaryPath -PathType Leaf) {
        Move-Item -LiteralPath $binaryPath -Destination $failedPath -Force -ErrorAction SilentlyContinue
      }
      Move-Item -LiteralPath $previousPath -Destination $binaryPath -Force
    } elseif ($activated) {
      Remove-Item -LiteralPath $binaryPath -Force -ErrorAction SilentlyContinue
    }
    Remove-Item -LiteralPath $readyPath -Force -ErrorAction SilentlyContinue
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
