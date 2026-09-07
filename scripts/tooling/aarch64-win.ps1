$ErrorActionPreference = 'Stop'
if ($args.Count -ne 0) { throw 'Usage: scripts/tooling/aarch64-win.ps1' }
& "$PSScriptRoot/windows.ps1" -Platform aarch64-win
