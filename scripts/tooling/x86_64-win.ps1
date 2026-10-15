param([string]$Command, [string]$Selected, [ValidateSet('ci', 'package')][string]$Operation)
$ErrorActionPreference = 'Stop'
# Any host, including the macOS workstation, can name the inputs of an installation.
if ($Command -eq 'fingerprint' -and $Selected -and -not $Operation -and $args.Count -eq 0) {
    $env:PYTHONDONTWRITEBYTECODE = '1'
    $env:UV_PROJECT = Split-Path (Split-Path $PSScriptRoot -Parent) -Parent
    $env:UV_LOCKED = '1'
    & uv run python "$PSScriptRoot/fingerprint.py" x86_64-win $Selected
    exit $LASTEXITCODE
}
if ($Command -or $Selected -or -not $Operation -or $args.Count -ne 0) {
    throw 'Usage: scripts/tooling/x86_64-win.ps1 -Operation {ci|package} | fingerprint OPERATION'
}
& "$PSScriptRoot/windows.ps1" -Platform x86_64-win -Operation $Operation
