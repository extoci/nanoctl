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
    $url = & $BinaryPath --config $ConfigPath url --wait
    if ($LASTEXITCODE -ne 0 -or -not $url) { throw "URL unavailable." }
    Write-Host "Open on your Tailscale-connected iPhone or other device:"
    Write-Host "  $url"
  } catch {
    Write-Warning "URL unavailable yet. Check that Tailscale (https://tailscale.com/download) is running and signed in, then retry nanoctl url --wait."
  }
}
