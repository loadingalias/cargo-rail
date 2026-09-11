param([Parameter(Mandatory)][ValidateSet('ci', 'package')][string]$Operation)
$ErrorActionPreference = 'Stop'
if ($args.Count -ne 0) { throw 'Usage: scripts/tooling/x86_64-win.ps1 -Operation {ci|package}' }
& "$PSScriptRoot/windows.ps1" -Platform x86_64-win -Operation $Operation
