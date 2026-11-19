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
  $tailscale = Get-Command tailscale -ErrorAction SilentlyContinue
  $tailscalePath = if ($tailscale) { $tailscale.Source } else {
    Join-Path $env:ProgramFiles "Tailscale\tailscale.exe"
  }
  try {
    $status = & $tailscalePath status --json 2>$null | ConvertFrom-Json
    if ($LASTEXITCODE -ne 0 -or $status.BackendState -ne "Running") {
      throw "Tailscale is not running."
    }
  } catch {
    Write-Warning "Install/start Tailscale and sign in: https://tailscale.com/download"
  }
  try {
    & $BinaryPath --config $ConfigPath url
    if ($LASTEXITCODE -ne 0) { throw "URL unavailable." }
  } catch {
    Write-Warning "URL unavailable yet; run nanoctl url after Tailscale is running."
  }
}
