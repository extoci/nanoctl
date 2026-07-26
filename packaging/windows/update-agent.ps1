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
$candidate = "$resolvedBinary.candidate"
$previous = "$resolvedBinary.previous"
$failed = "$resolvedBinary.failed"
$activated = $false
$completed = $false

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

if ((Test-Path -LiteralPath $candidate) -or
    (Test-Path -LiteralPath $previous) -or
    (Test-Path -LiteralPath $failed)) {
  throw "A prior update transaction must be resolved before another update."
}

try {
  Stop-AgentTask

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
  Start-ScheduledTask -TaskName $taskName

  & $resolvedBinary --config $resolvedConfig doctor
  if ($LASTEXITCODE -ne 0) {
    throw "Updated nanoctl failed its health check."
  }

  Remove-Item -LiteralPath $previous
  Remove-Item -LiteralPath $stagedPath -ErrorAction SilentlyContinue
  $completed = $true
  Write-Host "nanoctl update activated and committed."
}
finally {
  if (-not $completed) {
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
}
