#Requires -RunAsAdministrator
[CmdletBinding()]
param(
  [Parameter(Mandatory = $true)]
  [ValidateScript({ Test-Path -LiteralPath $_ -PathType Leaf })]
  [string]$BinaryPath
)

$ErrorActionPreference = "Stop"
$serviceName = "nanoctl"
$resolvedBinary = (Resolve-Path -LiteralPath $BinaryPath).Path
$quotedBinary = '"{0}" run' -f $resolvedBinary

if (Get-Service -Name $serviceName -ErrorAction SilentlyContinue) {
  throw "The nanoctl service is already installed. Uninstall or update it explicitly."
}

New-Service `
  -Name $serviceName `
  -BinaryPathName $quotedBinary `
  -DisplayName "nanoctl Remote Desktop Agent" `
  -Description "Provides device-owner-authorized remote desktop access." `
  -StartupType Automatic

sc.exe failure $serviceName reset= 86400 actions= restart/5000/restart/15000/none/0 | Out-Null
sc.exe failureflag $serviceName 1 | Out-Null
Start-Service -Name $serviceName
Write-Host "nanoctl service installed and started."
