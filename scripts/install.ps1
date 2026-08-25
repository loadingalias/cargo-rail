[CmdletBinding()]
param(
  [string]$Version = ""
)

$ErrorActionPreference = "Stop"
$repository = "https://github.com/loadingalias/cargo-rail"
$embeddedVersion = "@CARGO_RAIL_VERSION@"
if (-not $Version) {
  $Version = $embeddedVersion
}
if ($Version -eq $embeddedVersion) {
  throw "usage: scripts/install.ps1 -Version <exact-version>"
}
if ($Version -notmatch '^[0-9]+\.[0-9]+\.[0-9]+(?:[+-][0-9A-Za-z.-]+)?$') {
  throw "version must be an exact release such as 1.2.3"
}
$isWindowsHost = if (Get-Variable IsWindows -ErrorAction SilentlyContinue) {
  $IsWindows
} else {
  $env:OS -eq "Windows_NT"
}
if (-not $isWindowsHost) {
  throw "the PowerShell installer supports Windows only"
}

$architecture = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString().ToUpperInvariant()
$target = switch ($architecture) {
  "X64" { "x86_64-pc-windows-msvc" }
  "ARM64" { "aarch64-pc-windows-msvc" }
  default { throw "no supported Cargo-Rail archive for Windows $architecture" }
}

$archive = "cargo-rail-$target.zip"
$temporary = Join-Path ([System.IO.Path]::GetTempPath()) "cargo-rail-install-$([Guid]::NewGuid())"
New-Item -ItemType Directory -Path $temporary | Out-Null

try {
  $archivePath = Join-Path $temporary $archive
  $checksumPath = Join-Path $temporary "SHA256SUMS"
  $release = "$repository/releases/download/v$Version"
  Invoke-WebRequest -UseBasicParsing -Uri "$release/$archive" -OutFile $archivePath
  Invoke-WebRequest -UseBasicParsing -Uri "$release/SHA256SUMS" -OutFile $checksumPath

  $escapedArchive = [Regex]::Escape($archive)
  $checksumLine = Get-Content $checksumPath | Where-Object {
    $_ -match "^[0-9A-Fa-f]{64}\s+\*?$escapedArchive$"
  } | Select-Object -First 1
  if (-not $checksumLine) {
    throw "SHA256SUMS has no entry for $archive"
  }
  $expected = ($checksumLine -split '\s+', 2)[0].ToLowerInvariant()
  $actual = (Get-FileHash -Algorithm SHA256 -Path $archivePath).Hash.ToLowerInvariant()
  if ($actual -ne $expected) {
    throw "checksum mismatch for $archive"
  }

  $extract = Join-Path $temporary "extract"
  Expand-Archive -Path $archivePath -DestinationPath $extract
  $manifestMatches = @(Get-ChildItem $extract -Recurse -File -Filter cargo-rail-components-v1.tsv)
  if ($manifestMatches.Count -ne 1) {
    throw "$archive must contain exactly one component manifest"
  }
  $manifest = $manifestMatches[0]
  $lines = @(Get-Content $manifest.FullName)
  if ($lines.Count -lt 2 -or $lines[0] -ne "cargo-rail-components-v1`t$Version`t$target") {
    throw "$archive component manifest has incompatible release authority"
  }

  $expectedNames = @(
    "cargo-rail.exe",
    "cargo-rail-compiler-observation.exe",
    "cargo-rail-distributed-worker.exe",
    "cargo-rail-fact-driver.exe",
    "cargo-rail-fact-driver-source-v1.json",
    "cargo-rail-native-rustc-worker.exe",
    "cargo-rail-native-rustc-wrapper.exe"
  )
  $components = @{}
  foreach ($line in $lines[1..($lines.Count - 1)]) {
    $fields = @($line -split "`t", 5)
    if ($fields.Count -ne 4) {
      throw "invalid component manifest entry"
    }
    $name, $digest, $bytes, $capability = $fields
    if ($components.ContainsKey($name) -or [IO.Path]::GetFileName($name) -ne $name) {
      throw "invalid component name in release manifest"
    }
    if ($digest -notmatch '^[0-9a-f]{64}$' -or $bytes -notmatch '^[0-9]+$' -or -not $capability) {
      throw "invalid component authority for $name"
    }
    $source = Join-Path $manifest.DirectoryName $name
    if (-not (Test-Path -LiteralPath $source -PathType Leaf)) {
      throw "$archive is missing $name"
    }
    $item = Get-Item -LiteralPath $source
    $actualDigest = (Get-FileHash -Algorithm SHA256 -LiteralPath $source).Hash.ToLowerInvariant()
    if ($item.Length -ne [UInt64]$bytes -or $actualDigest -ne $digest) {
      throw "component authority does not match $name"
    }
    $components[$name] = $source
  }
  $actualNames = @($components.Keys | Sort-Object) -join "`n"
  $requiredNames = @($expectedNames | Sort-Object) -join "`n"
  if ($actualNames -ne $requiredNames) {
    throw "$archive component manifest does not declare the exact platform inventory"
  }

  if ($env:CARGO_HOME) {
    $installDirectory = Join-Path $env:CARGO_HOME "bin"
  } elseif ($HOME) {
    $installDirectory = Join-Path $HOME ".cargo\bin"
  } else {
    throw "HOME or CARGO_HOME must be set"
  }

  New-Item -ItemType Directory -Force -Path $installDirectory | Out-Null
  $stage = Join-Path $installDirectory ".cargo-rail-install-$([Guid]::NewGuid())"
  New-Item -ItemType Directory -Path $stage | Out-Null
  try {
    foreach ($name in $expectedNames) {
      Copy-Item -LiteralPath $components[$name] -Destination (Join-Path $stage $name)
    }
    Copy-Item -LiteralPath $manifest.FullName -Destination (Join-Path $stage $manifest.Name)
    foreach ($name in $expectedNames) {
      Move-Item -Force -LiteralPath (Join-Path $stage $name) -Destination (Join-Path $installDirectory $name)
    }
    Move-Item -Force -LiteralPath (Join-Path $stage $manifest.Name) -Destination (Join-Path $installDirectory $manifest.Name)
  } finally {
    Remove-Item -Recurse -Force -ErrorAction SilentlyContinue $stage
  }

  $binary = Join-Path $installDirectory "cargo-rail.exe"
  $actualVersion = (& $binary rail --version | Out-String).Trim()
  if ($LASTEXITCODE -ne 0 -or $actualVersion -ne "cargo-rail $Version") {
    throw "installed binary reported '$actualVersion', expected 'cargo-rail $Version'"
  }

  Write-Host "Installed Cargo-Rail $Version in $installDirectory."
  Write-Host "Surface source authority is installed; exact-toolchain compiler support is prepared on first use."
} finally {
  Remove-Item -Recurse -Force -ErrorAction SilentlyContinue $temporary
}
