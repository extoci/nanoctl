[CmdletBinding()]
param(
  [string]$BinaryPath = (Join-Path $env:LOCALAPPDATA "nanoctl\nanoctl.exe"),

  [string]$ConfigPath
)

$ErrorActionPreference = "Stop"
$taskName = "nanoctl Agent"
$publicRoot = [IO.Path]::GetFullPath((Join-Path $env:LOCALAPPDATA "nanoctl"))
$managedRoot = [IO.Path]::GetFullPath((Join-Path $env:ProgramFiles "nanoctl"))
$publicBinary = [IO.Path]::GetFullPath((Join-Path $publicRoot "nanoctl.exe"))
$managedBinary = [IO.Path]::GetFullPath((Join-Path $managedRoot "nanoctl.exe"))
$logPath = Join-Path $env:LOCALAPPDATA "nanoctl\agent.log"

try {
  $requestedBinary = [IO.Path]::GetFullPath($BinaryPath)
} catch {
  throw "BinaryPath is not a valid Windows path."
}

# The command accepts an explicit path for managed installs, but it must never turn an arbitrary
# caller path into a recursive deletion target. A missing public install falls back to the
# documented Program Files layout so a partial migration can still be cleaned up.
if (-not $PSBoundParameters.ContainsKey("BinaryPath") -and
    -not (Test-Path -LiteralPath $requestedBinary -PathType Leaf) -and
    ((Test-Path -LiteralPath $managedBinary -PathType Leaf) -or
      (Test-Path -LiteralPath $managedRoot -PathType Container))) {
  $requestedBinary = $managedBinary
}
if (-not ([StringComparer]::OrdinalIgnoreCase.Equals($requestedBinary, $publicBinary) -or
          [StringComparer]::OrdinalIgnoreCase.Equals($requestedBinary, $managedBinary))) {
  throw "BinaryPath must be the standard nanoctl.exe in LocalAppData or Program Files."
}

$isManaged = [StringComparer]::OrdinalIgnoreCase.Equals($requestedBinary, $managedBinary)
if ($isManaged) {
  $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
  $principal = [Security.Principal.WindowsPrincipal]$identity
  if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw "Removing the managed Program Files installation requires an administrator PowerShell."
  }
}

$binaryExists = Test-Path -LiteralPath $requestedBinary -PathType Leaf
$resolvedBinary = if ($binaryExists) {
  (Resolve-Path -LiteralPath $requestedBinary).Path
} else {
  $requestedBinary
}

# Remove the task even when a binary was deleted or partially upgraded. Otherwise every logon
# would continue launching a broken entry and the user could never repair the installation.
$task = Get-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue
if ($task) {
  Stop-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue
  for ($attempt = 0; $attempt -lt 50; $attempt++) {
    $current = Get-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue
    if (-not $current -or $current.State -ne "Running") { break }
    Start-Sleep -Milliseconds 100
  }
  Unregister-ScheduledTask -TaskName $taskName -Confirm:$false
}

if ($binaryExists) {
  if (-not $ConfigPath) {
    $pathOutput = & $resolvedBinary paths
    if ($LASTEXITCODE -eq 0 -and $pathOutput -match '^config=(.+)$') {
      $ConfigPath = $Matches[1].Trim()
    }
  }
  if ($ConfigPath -and (Test-Path -LiteralPath $ConfigPath -PathType Leaf)) {
    & $resolvedBinary --config $ConfigPath unenroll
    if ($LASTEXITCODE -ne 0) {
      throw "nanoctl could not remove its local enrollment."
    }
  }
} else {
  Write-Warning "The installed nanoctl binary is missing; removing the broken task and install directory."
}

$installRoot = if ($isManaged) { $managedRoot } else { $publicRoot }
if (Test-Path -LiteralPath $installRoot -PathType Container) {
  $rootInfo = Get-Item -LiteralPath $installRoot
  if (($rootInfo.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
    throw "Refusing to remove a nanoctl install root that is a reparse point."
  }
  Remove-Item -LiteralPath $installRoot -Recurse -Force
  Write-Verbose "Removed $installRoot"
}
$readyPath = Join-Path $env:LOCALAPPDATA "nanoctl\agent.ready"
Remove-Item -LiteralPath $readyPath -Force -ErrorAction SilentlyContinue
Remove-Item -LiteralPath $logPath -Force -ErrorAction SilentlyContinue
Write-Host "nanoctl background agent, installed binary, and local enrollment removed."
