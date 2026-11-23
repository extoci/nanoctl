function Remove-LegacyCredential {
  param([string]$ConfigPath)
  try {
    if ($ConfigPath -and (Test-Path -LiteralPath $ConfigPath -PathType Leaf)) {
      $contents = Get-Content -LiteralPath $ConfigPath -Raw
      if ($contents -match '(?m)^\s*device_id\s*=\s*["'']([^"'']+)["'']') {
        & "$env:SystemRoot\System32\cmdkey.exe" "/delete:$($Matches[1]).dev.nanoctl.agent" *> $null
      }
    }
  } catch {
    Write-Verbose "Legacy credential cleanup was unavailable."
  }
}

function Show-TailnetUrl {
  param([string]$BinaryPath, [string]$ConfigPath)
  try {
    $ErrorActionPreference = "Continue"
    $urlOutput = & $BinaryPath --config $ConfigPath url 2>&1
    $exitCode = $LASTEXITCODE
    $urlOutput | ForEach-Object { Write-Host $_ }
    if ($exitCode -ne 0) { throw "URL unavailable." }
  } catch {
    Write-Warning "Run nanoctl url to finish Tailscale sign-in or nanoctl doctor to diagnose the connection."
  }
}
