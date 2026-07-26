[CmdletBinding()]
param()

$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"
$Repository = if ($env:NANOCTL_REPOSITORY) { $env:NANOCTL_REPOSITORY } else { "extoci/nanoctl" }
$Version = if ($env:NANOCTL_VERSION) { $env:NANOCTL_VERSION } else { "latest" }
$ControlPlane = if ($env:NANOCTL_CONTROL_PLANE) {
  $env:NANOCTL_CONTROL_PLANE
} else {
  "https://dapper-hornet-380.convex.site"
}

if (-not $IsWindows -and $PSVersionTable.PSEdition -eq "Core") {
  throw "This installer supports Windows. On Linux or macOS, use install.sh."
}

$architecture = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture
$target = switch ($architecture) {
  "X64" { "windows-x64" }
  "Arm64" { "windows-arm64" }
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

try {
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
    Stop-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue
    for ($attempt = 0; $attempt -lt 50; $attempt++) {
      $state = (Get-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue).State
      if ($state -ne "Running") { break }
      Start-Sleep -Milliseconds 100
    }
  }

  New-Item -ItemType Directory -Path $installRoot -Force | Out-Null
  Copy-Item -LiteralPath $download -Destination $candidatePath -Force
  Move-Item -LiteralPath $candidatePath -Destination $binaryPath -Force

  $pathOutput = & $binaryPath paths
  if ($LASTEXITCODE -ne 0 -or $pathOutput -notmatch "^config=(.+)$") {
    throw "The installed binary returned an invalid configuration path."
  }
  $configPath = $Matches[1]
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

  if ($existingTask) {
    Unregister-ScheduledTask -TaskName $taskName -Confirm:$false
  }
  $currentUser = [Security.Principal.WindowsIdentity]::GetCurrent().Name
  $action = New-ScheduledTaskAction -Execute $binaryPath -Argument "run"
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
    -Description "nanoctl remote desktop agent for the current desktop user." | Out-Null
  Start-ScheduledTask -TaskName $taskName

  $userPath = [Environment]::GetEnvironmentVariable("Path", "User")
  $pathEntries = @($userPath -split ";" | Where-Object { $_ })
  if ($pathEntries -notcontains $installRoot) {
    $newPath = (@($pathEntries) + $installRoot) -join ";"
    [Environment]::SetEnvironmentVariable("Path", $newPath, "User")
    $env:Path = "$installRoot;$env:Path"
  }

  Write-Host ""
  Write-Host "nanoctl is installed, enrolled, and running."
  Write-Host "Run this installer again at any time to update."
}
finally {
  Remove-Item -LiteralPath $candidatePath -Force -ErrorAction SilentlyContinue
  Remove-Item -LiteralPath $temporary -Recurse -Force -ErrorAction SilentlyContinue
}
