# Rig Hub installer for Windows:
#   irm https://raw.githubusercontent.com/SusieeTheLinuxUser/rig-hub/main/install.ps1 | iex
$ErrorActionPreference = 'Stop'
$repo = 'SusieeTheLinuxUser/rig-hub'
$dir = Join-Path $env:LOCALAPPDATA 'Programs\RigHub'
$zip = Join-Path $env:TEMP 'rig-hub.zip'

Write-Host 'Downloading Rig Hub...'
Invoke-WebRequest "https://github.com/$repo/releases/latest/download/rig-hub-x86_64-windows.zip" -OutFile $zip
New-Item -ItemType Directory -Force -Path $dir | Out-Null
Expand-Archive $zip -DestinationPath $dir -Force
Remove-Item $zip

$lnk = Join-Path ([Environment]::GetFolderPath('Programs')) 'Rig Hub.lnk'
$s = (New-Object -ComObject WScript.Shell).CreateShortcut($lnk)
$s.TargetPath = Join-Path $dir 'rig-hub.exe'
$s.Save()

Write-Host "Installed to $dir - find 'Rig Hub' in the Start menu."
Write-Host 'Headset support needs HeadsetControl: https://github.com/Sapd/HeadsetControl'
