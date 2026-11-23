param([switch]$Worker)

$ErrorActionPreference = "Stop"
$repo = Split-Path $PSScriptRoot
$runner = Get-Content -LiteralPath "$repo\packaging\windows\run-agent.ps1" -Raw
$definition = [regex]::Match($runner, '(?s)Add-Type -TypeDefinition @"\r?\n(.*?)\r?\n"@').Groups[1].Value
Add-Type -TypeDefinition $definition
if ($Worker) {
  exit [NanoctlJob]::Run("$env:SystemRoot\System32\ping.exe", "-t 127.0.0.1")
}

. "$repo\packaging\windows\lifecycle.ps1"
Stop-AgentProcesses -Paths @()
Stop-AgentProcesses -Paths @("C:\not-a-nanoctl-install\nanoctl.exe")
if ([NanoctlJob]::Run("$env:SystemRoot\System32\cmd.exe", "/d /c exit 7") -ne 7) {
  throw "The runner lost its child's exit code."
}
$failed = $false
try { [NanoctlJob]::Run("C:\not-a-nanoctl-install\nanoctl.exe", "") | Out-Null } catch { $failed = $true }
if (-not $failed) { throw "The runner accepted a missing executable." }

$oldLocalAppData = $env:LOCALAPPDATA
$env:LOCALAPPDATA = Join-Path ([IO.Path]::GetTempPath()) ("nanoctl-stop-test-" + [Guid]::NewGuid().ToString("N"))
$global:nanoctlLifecycleUnregistered = $false
$global:nanoctlLifecycleFake = [pscustomobject]@{
  Path = (Join-Path $env:LOCALAPPDATA "nanoctl\nanoctl.exe")
  Id = -1
  HasExited = $false
}
$global:nanoctlLifecycleFake | Add-Member -MemberType ScriptMethod -Name Kill -Value { throw "injected stop failure" }
function Get-Process { $global:nanoctlLifecycleFake }
function Start-Process { throw "injected graceful stop failure" }
function Get-ScheduledTask { $null }
function Stop-ScheduledTask {}
function Unregister-ScheduledTask { $global:nanoctlLifecycleUnregistered = $true }
$stopFailed = $false
try {
  & "$repo\packaging\windows\uninstall-agent.ps1" -BinaryPath $global:nanoctlLifecycleFake.Path
} catch {
  if ($_ -notmatch "injected stop failure") { throw }
  $stopFailed = $true
} finally {
  $env:LOCALAPPDATA = $oldLocalAppData
  Remove-Item Function:\Get-Process, Function:\Start-Process, Function:\Get-ScheduledTask, Function:\Stop-ScheduledTask, Function:\Unregister-ScheduledTask
}
if (-not $stopFailed -or $global:nanoctlLifecycleUnregistered) {
  throw "A failed process stop must abort before unregistering the task."
}
Remove-Variable nanoctlLifecycleFake, nanoctlLifecycleUnregistered -Scope Global

$parent = Start-Process -FilePath "$env:SystemRoot\System32\WindowsPowerShell\v1.0\powershell.exe" `
  -ArgumentList "-NoProfile -NonInteractive -File `"$PSCommandPath`" -Worker" -WindowStyle Hidden -PassThru
$child = $null
try {
  $deadline = (Get-Date).AddSeconds(15)
  do {
    $childInfo = Get-CimInstance Win32_Process -Filter "Name = 'ping.exe' AND ParentProcessId = $($parent.Id)"
    if ($childInfo) {
      $child = Get-Process -Id $childInfo.ProcessId -ErrorAction Stop
      break
    }
    if ($parent.HasExited) { throw "The native job worker exited before starting its child." }
    Start-Sleep -Milliseconds 100
  } while ((Get-Date) -lt $deadline)
  if (-not $child) { throw "The native job worker never started its child." }
  $parent.Kill()
  if (-not $parent.WaitForExit(5000)) { throw "The test runner did not exit." }
  if (-not $child.WaitForExit(5000)) { throw "Stopping the runner orphaned its child." }
  "WINDOWS_LIFECYCLE_OK: exit code, missing executable, empty/nonmatching stop, failed-stop preservation, parent termination"
} finally {
  if (-not $parent.HasExited) { $parent.Kill(); $parent.WaitForExit() }
  if ($child -and -not $child.HasExited) { $child.Kill(); $child.WaitForExit() }
  $parent.Dispose()
  if ($child) { $child.Dispose() }
}
