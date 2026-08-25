$ErrorActionPreference = "Stop"
$temporary = Join-Path $env:RUNNER_TEMP "cargo-rail-installer-$([Guid]::NewGuid())"
$server = $null
$previousCargoHome = $env:CARGO_HOME
$previousPath = $env:PATH
New-Item -ItemType Directory -Path $temporary | Out-Null

try {
  $versionLine = Get-Content Cargo.toml | Where-Object { $_ -match '^version = "([^"]+)"$' } | Select-Object -First 1
  if (-not $versionLine -or $versionLine -notmatch '^version = "([^"]+)"$') {
    throw "could not read the package version from Cargo.toml"
  }
  $version = $Matches[1]
  $architecture = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString().ToUpperInvariant()
  $target = switch ($architecture) {
    "X64" { "x86_64-pc-windows-msvc" }
    "ARM64" { "aarch64-pc-windows-msvc" }
    default { throw "installer test does not support Windows $architecture" }
  }

  $archive = "cargo-rail-$target.zip"
  $release = Join-Path $temporary "repository\releases\download\v$version"
  $payload = Join-Path $temporary "payload\cargo-rail-$target"
  New-Item -ItemType Directory -Path $release, $payload | Out-Null

  $binary = Resolve-Path "target\debug\cargo-rail.exe"
  $executables = @(
    "cargo-rail.exe",
    "cargo-rail-compiler-observation.exe",
    "cargo-rail-native-rustc-wrapper.exe",
    "cargo-rail-native-rustc-worker.exe",
    "cargo-rail-distributed-worker.exe",
    "cargo-rail-fact-driver.exe"
  )
  foreach ($executable in $executables) {
    Copy-Item $binary (Join-Path $payload $executable)
  }
  Set-Content -NoNewline -Path (Join-Path $payload "cargo-rail-fact-driver-source-v1.json") -Value '{"version":1,"files":[]}'

  $archivePath = Join-Path $release $archive
  Compress-Archive -Path $payload -DestinationPath $archivePath
  python scripts/package-release-archive.py $archivePath --target $target --version $version --surface true
  if ($LASTEXITCODE -ne 0) {
    throw "could not bind installer fixture component authority"
  }
  $digest = (Get-FileHash -Algorithm SHA256 -Path $archivePath).Hash.ToLowerInvariant()
  Set-Content -NoNewline -Path (Join-Path $release "SHA256SUMS") -Value "$digest  $archive`n"

  $python = if (Get-Command python3 -ErrorAction SilentlyContinue) { "python3" } else { "python" }
  $serverOutput = Join-Path $temporary "server.stdout"
  $serverError = Join-Path $temporary "server.stderr"
  $serverPort = Join-Path $temporary "server.port"
  $server = Start-Process $python -ArgumentList @(
    "-u", "scripts/ci/http-fixture-server.py",
    "--directory", (Join-Path $temporary "repository"),
    "--port-file", $serverPort
  ) -RedirectStandardOutput $serverOutput -RedirectStandardError $serverError -PassThru -NoNewWindow

  $port = $null
  for ($attempt = 0; $attempt -lt 100; $attempt++) {
    if (Test-Path $serverPort) {
      $reportedPort = (Get-Content $serverPort -Raw).Trim()
      $parsedPort = 0
      if ([int]::TryParse($reportedPort, [ref]$parsedPort) -and $parsedPort -ge 1 -and $parsedPort -le 65535) {
        $port = $parsedPort
        break
      }
    }
    if ($server.HasExited) {
      throw "local installer fixture server exited early"
    }
    Start-Sleep -Milliseconds 100
  }
  if (-not $port) {
    $diagnostic = if (Test-Path $serverError) { (Get-Content $serverError -Raw).Trim() } else { "no server stderr" }
    throw "local installer fixture server did not report a valid port: $diagnostic"
  }

  $installer = Join-Path $temporary "install.ps1"
  $content = Get-Content scripts/install.ps1 -Raw
  $content = $content.Replace(
    'https://github.com/loadingalias/cargo-rail',
    "http://127.0.0.1:$port"
  )
  Set-Content -NoNewline -Path $installer -Value $content

  $env:CARGO_HOME = Join-Path $temporary "cargo-home"
  & $installer -Version $version
  foreach ($executable in $executables) {
    if (-not (Test-Path (Join-Path $env:CARGO_HOME "bin\$executable"))) {
      throw "installer did not install $executable"
    }
  }
  foreach ($component in @("cargo-rail-fact-driver-source-v1.json", "cargo-rail-components-v1.tsv")) {
    if (-not (Test-Path (Join-Path $env:CARGO_HOME "bin\$component"))) {
      throw "installer did not install $component"
    }
  }
  Set-Content -NoNewline -Path (Join-Path $env:CARGO_HOME "bin\cargo-rail-native-rustc-worker.exe") -Value "damaged"
  & $installer -Version $version
  if ((Get-Item (Join-Path $env:CARGO_HOME "bin\cargo-rail-native-rustc-worker.exe")).Length -ne (Get-Item $binary).Length) {
    throw "installer did not repair a partial cached installation"
  }

  Set-Content -NoNewline -Path (Join-Path $release "SHA256SUMS") -Value "$('0' * 64)  $archive`n"
  $env:CARGO_HOME = Join-Path $temporary "bad-home"
  $rejected = $false
  try {
    & $installer -Version $version
  } catch {
    $rejected = $_.Exception.Message -match "checksum mismatch for $([Regex]::Escape($archive))"
  }
  if (-not $rejected) {
    throw "installer accepted an invalid archive checksum"
  }
  if (Test-Path (Join-Path $env:CARGO_HOME "bin\cargo-rail.exe")) {
    throw "installer wrote files after rejecting an invalid checksum"
  }
} finally {
  $env:CARGO_HOME = $previousCargoHome
  $env:PATH = $previousPath
  if ($server -and -not $server.HasExited) {
    Stop-Process -Id $server.Id -Force
  }
  Remove-Item -Recurse -Force -ErrorAction SilentlyContinue $temporary
}
