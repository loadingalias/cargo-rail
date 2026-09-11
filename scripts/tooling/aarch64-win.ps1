param([Parameter(Mandatory)][ValidateSet('ci', 'package')][string]$Operation)
$ErrorActionPreference = 'Stop'
if ($args.Count -ne 0) { throw 'Usage: scripts/tooling/aarch64-win.ps1 -Operation {ci|package}' }
& "$PSScriptRoot/windows.ps1" -Platform aarch64-win -Operation $Operation
