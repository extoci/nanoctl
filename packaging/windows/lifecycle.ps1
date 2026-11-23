function Stop-AgentProcesses {
  param([Parameter(Mandatory = $true)][AllowEmptyCollection()][string[]]$Paths)

  $normalizedPaths = @($Paths | Where-Object { $_ } | ForEach-Object {
    [IO.Path]::GetFullPath($_)
  })
  $running = @(Get-Process -Name nanoctl -ErrorAction SilentlyContinue | Where-Object {
    $_.Path -and $normalizedPaths -contains [IO.Path]::GetFullPath($_.Path)
  })
  if ($running.Count -eq 0) { return }
  # The stop protocol is per-user, not per-path. Never send it while an unrelated binary is running.
  $unrelated = @(Get-Process -Name nanoctl -ErrorAction SilentlyContinue | Where-Object {
    -not $_.Path -or $normalizedPaths -notcontains [IO.Path]::GetFullPath($_.Path)
  })
  if ($unrelated.Count -eq 0) {
    $stopCommand = $null
    try {
      $stopCommand = Start-Process -FilePath $running[0].Path -ArgumentList "stop" `
        -WindowStyle Hidden -PassThru
      if (-not $stopCommand.WaitForExit(70000)) {
        $stopCommand.Kill()
        $stopCommand.WaitForExit()
      }
    } catch {
      Write-Warning "Graceful nanoctl stop was unavailable; stopping only the managed executable."
    } finally {
      if ($stopCommand) { $stopCommand.Dispose() }
    }
  }
  foreach ($process in $running) {
    if ($process.HasExited) { continue }
    try {
      $process.Kill()
      if (-not $process.WaitForExit(30000)) {
        throw "Timed out waiting for nanoctl process $($process.Id) to exit."
      }
    } catch {
      if (-not $process.HasExited) { throw }
    }
  }
}
