# Shared native Windows provisioning. Run in an elevated PowerShell session.
param(
    [Parameter(Mandatory)][ValidateSet('aarch64-win', 'x86_64-win')][string]$Platform,
    [Parameter(Mandatory)][ValidateSet('ci', 'package')][string]$Operation
)
$ErrorActionPreference = 'Stop'
$env:PYTHONDONTWRITEBYTECODE = '1'
Set-StrictMode -Version Latest
if ([Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT) { throw 'This installer requires Windows.' }
$architecture = (Get-CimInstance Win32_Processor | Select-Object -First 1).Architecture
$expectedArchitecture = if ($Platform -eq 'aarch64-win') { 12 } else { 9 }
if ($architecture -ne $expectedArchitecture) { throw "$Platform requires native $expectedArchitecture hardware." }
$os = Get-CimInstance Win32_OperatingSystem
if ($Platform -eq 'aarch64-win') {
    if ($os.Caption -notmatch 'Windows 11 Enterprise') { throw 'Expected Windows 11 Enterprise ARM64.' }
} elseif ($os.Caption -notmatch 'Windows Server 2025') { throw 'Expected Windows Server 2025.' }
$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
if (-not ([Security.Principal.WindowsPrincipal]$identity).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'Run the installer from an elevated PowerShell session.'
}
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
$repoRoot = Split-Path (Split-Path $PSScriptRoot -Parent) -Parent
$catalogPath = Join-Path $repoRoot '.config/tooling.toml'
$prefix = Join-Path $env:LOCALAPPDATA 'Cargo-Rail\tooling'
New-Item -ItemType Directory -Force $prefix | Out-Null
$temporary = Join-Path ([IO.Path]::GetTempPath()) ([Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory $temporary | Out-Null

function Invoke-Native {
    param([string]$Command, [string[]]$Arguments)
    & $Command @Arguments
    if ($LASTEXITCODE -ne 0) { throw "$Command failed with exit code $LASTEXITCODE" }
}
function Get-PinnedDownload {
    param([string]$Url, [string]$Sha256, [string]$Destination)
    if ($Url -notmatch '^https://' -or $Sha256 -notmatch '^[0-9a-f]{64}$') { throw 'Invalid download pin.' }
    Invoke-WebRequest -UseBasicParsing -Uri $Url -OutFile $Destination
    if ((Get-FileHash -Algorithm SHA256 $Destination).Hash -ne $Sha256) {
        Remove-Item $Destination -Force
        throw "Checksum mismatch for $Url"
    }
}
function Install-Exe {
    param([string]$Path, [string[]]$Arguments)
    $process = Start-Process -FilePath $Path -ArgumentList $Arguments -Wait -PassThru
    if ($process.ExitCode -eq 3010) {
        throw 'Installation requires a reboot. Reboot and rerun this same installer.'
    }
    if ($process.ExitCode -ne 0) { throw "$Path failed with exit code $($process.ExitCode)" }
}
function Get-PinnedVisualStudio {
    param([string]$Path, [string]$Version, [string[]]$Components)
    $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
    if (-not (Test-Path $vswhere)) { return }
    $instances = & $vswhere -products Microsoft.VisualStudio.Product.BuildTools -requires $Components -format json | ConvertFrom-Json
    if ($LASTEXITCODE -ne 0) { throw 'Unable to inspect Visual Studio installation.' }
    $instances | Where-Object { $_.installationPath -eq $Path -and $_.installationVersion -eq $Version }
}
function Test-Native {
    param([string]$Command, [string[]]$Arguments)
    # Failures are reported on stderr, which Windows PowerShell would raise as errors.
    $ErrorActionPreference = 'Continue'
    if (-not (Get-Command $Command -CommandType Application -ErrorAction SilentlyContinue)) { return $false }
    & $Command @Arguments 2>&1 | Out-Null
    $LASTEXITCODE -eq 0
}
function Test-Pinned {
    param([string]$Uv, [string]$Helper, [string[]]$Names)
    Test-Native $Uv (@('run', 'python', $Helper, 'require') + $Names)
}

try {
    # Bootstrap only the two scalar strings of the uv archive; uv then runs the pinned Python.
    $text = Get-Content -Raw $catalogPath
    $section = [regex]::Match($text, '(?ms)^\[' + [regex]::Escape($Platform) + '\.assets\.uv\]\r?\n(?<body>.*?)(?=^\[|\z)')
    if (-not $section.Success) { throw 'Missing native uv bootstrap asset.' }
    $url = [regex]::Match($section.Groups['body'].Value, '(?m)^url = ("[^"\r\n]+")\r?$').Groups[1].Value | ConvertFrom-Json
    $sha = [regex]::Match($section.Groups['body'].Value, '(?m)^sha256 = ("[0-9a-f]{64}")\r?$').Groups[1].Value | ConvertFrom-Json
    # The layout and receipt match catalog.py install-archive.
    $uvDirectory = Join-Path $prefix ('uv\' + $sha.Substring(0, 16))
    $uv = Join-Path $uvDirectory 'uv.exe'
    if (-not (Test-Path (Join-Path $uvDirectory '.rail-installed'))) {
        $archive = Join-Path $temporary 'uv.zip'
        Get-PinnedDownload $url $sha $archive
        Remove-Item -Recurse -Force $uvDirectory -ErrorAction SilentlyContinue
        Expand-Archive -Path $archive -DestinationPath $uvDirectory -Force -ErrorAction Stop
        Set-Content -Path (Join-Path $uvDirectory '.rail-installed') -Value $sha -Encoding ascii
    }
    $env:UV_PROJECT = $repoRoot
    $env:UV_LOCKED = '1'
    Push-Location $repoRoot
    # `--default` also names it python3 in uv's bin directory: dev-machines runs python3 on the host.
    try { Invoke-Native $uv @('python', 'install', '--default') } finally { Pop-Location }
    $pythonBin = & $uv python dir --bin
    if ($LASTEXITCODE -ne 0) { throw 'Unable to locate the uv Python executables.' }
    $catalogHelper = Join-Path $PSScriptRoot 'catalog.py'
    $catalogJson = & $uv run python $catalogHelper json
    if ($LASTEXITCODE -ne 0) { throw 'Unable to parse tooling catalog.' }
    $catalog = $catalogJson | ConvertFrom-Json
    Invoke-Native $uv @('run', 'python', $catalogHelper, 'validate')
    $selection = & $uv run python $catalogHelper select $Platform $Operation
    if ($LASTEXITCODE -ne 0) { throw 'Unable to select operation tooling.' }
    $native = $selection | ConvertFrom-Json
    $channel = & $uv run python $catalogHelper rust-channel $Platform
    if ($LASTEXITCODE -ne 0) { throw 'Unable to resolve the runner Rust toolchain.' }
    $fingerprint = & $uv run python (Join-Path $PSScriptRoot 'fingerprint.py') $Platform $Operation
    if ($LASTEXITCODE -ne 0) { throw 'Unable to fingerprint the installation inputs.' }
    $fingerprintFile = Join-Path $prefix "fingerprint.$Operation"
    # A complete run records the fingerprint of its inputs. While it matches, a converged
    # host or image republishes its environment for this operation once the pins answer.
    $current = (Test-Path $fingerprintFile) -and (Get-Content -Raw $fingerprintFile).Trim() -eq $fingerprint

    $vsPath = Join-Path $prefix ('vs-' + $catalog.windows.'visual-studio')
    $vsToolset = if ($Platform -eq 'aarch64-win') { 'Microsoft.VisualStudio.Component.VC.Tools.ARM64' } else { 'Microsoft.VisualStudio.Component.VC.Tools.x86.x64' }
    $vsComponents = @($vsToolset, $catalog.windows.'sdk-component')
    if ($Platform -eq 'x86_64-win') { $vsComponents += 'Microsoft.VisualStudio.Component.VC.ASAN' }
    $vsVersion = $catalog.windows.'build-version'
    # vswhere reports the pinned build only while it holds every selected component.
    if (@(Get-PinnedVisualStudio -Path $vsPath -Version $vsVersion -Components $vsComponents).Count -ne 1) {
        $channelFile = Join-Path $prefix ('vs-channel-' + $catalog.windows.'channel-sha256' + '.json')
        Get-PinnedDownload $catalog.windows.'channel-url' $catalog.windows.'channel-sha256' $channelFile
        $bootstrap = Join-Path $temporary 'vs_buildtools.exe'
        Get-PinnedDownload $catalog.windows.'bootstrap-url' $catalog.windows.'bootstrap-sha256' $bootstrap
        $vsArguments = @('--quiet', '--wait', '--norestart', '--noUpdateInstaller',
            '--installPath', ('"' + $vsPath + '"'), '--installChannelUri', ('"' + $channelFile + '"'),
            '--channelUri', ('"' + $channelFile + '"'))
        foreach ($component in $vsComponents) { $vsArguments += @('--add', $component) }
        if (Test-Path (Join-Path $vsPath 'Common7\Tools\Microsoft.VisualStudio.DevShell.dll')) {
            $vsArguments = @('modify') + $vsArguments
        }
        $vsStarted = Get-Date
        try {
            Install-Exe $bootstrap $vsArguments
        } catch {
            Get-ChildItem -Path ([IO.Path]::GetTempPath()) -Filter 'dd_*.log' -File -ErrorAction SilentlyContinue |
                Where-Object { $_.LastWriteTime -ge $vsStarted } |
                Sort-Object LastWriteTime | Select-Object -Last 5 | ForEach-Object {
                    Write-Host "Visual Studio installer log: $($_.FullName)"
                    Get-Content -LiteralPath $_.FullName -Tail 80 -ErrorAction SilentlyContinue
                }
            throw
        }
        if (@(Get-PinnedVisualStudio -Path $vsPath -Version $vsVersion -Components $vsComponents).Count -ne 1) {
            throw 'Visual Studio does not match the pinned build.'
        }
    }
    # Module commands ignore the caller's preference in Windows PowerShell.
    Import-Module (Join-Path $vsPath 'Common7\Tools\Microsoft.VisualStudio.DevShell.dll') -ErrorAction Stop
    $vsArch = if ($Platform -eq 'aarch64-win') { 'arm64' } else { 'amd64' }

    # Git reports its own version; only a missing or different pin is installed.
    $gitDirectory = Join-Path $prefix ('git-' + $catalog.versions.git)
    $git = Join-Path $gitDirectory 'cmd\git.exe'
    if (-not ((Test-Path $git) -and (& $git --version) -eq "git version $($catalog.versions.git)")) {
        $gitInstaller = Join-Path $temporary 'git.exe'
        Get-PinnedDownload $native.assets.git.url $native.assets.git.sha256 $gitInstaller
        Install-Exe $gitInstaller @('/VERYSILENT', '/NORESTART', '/NOCANCEL', '/SP-', ('/DIR="' + $gitDirectory + '"'))
    }
    # Use the actual Bash binary. Git's bin/bash.exe launcher prepends usr/bin
    # again, shadowing MSVC's linker even after the developer shell is entered.
    $paths = @($uvDirectory, $pythonBin, (Join-Path $gitDirectory 'cmd'),
        (Join-Path $gitDirectory 'usr\bin'))
    foreach ($name in @('llvm', 'cmake', 'cargo-binstall')) {
        $directory = & $uv run python $catalogHelper install-archive $Platform $name $prefix
        if ($LASTEXITCODE -ne 0) { throw "Unable to install $name" }
        $toolBin = if (Test-Path (Join-Path $directory 'bin')) { Join-Path $directory 'bin' } else { $directory }
        $paths += $toolBin
        if ($name -eq 'llvm') { $env:LIBCLANG_PATH = $toolBin }
    }
    $cargoBin = Join-Path $env:USERPROFILE '.cargo\bin'
    $paths += $cargoBin
    $env:PATH = (@($paths + ($env:PATH -split ';')) | Select-Object -Unique) -join ';'
    # Git's usr/bin also contains link.exe. Keep the native compiler ahead of it.
    Enter-VsDevShell -VsInstallPath $vsPath -SkipAutomaticLocation -DevCmdArguments "-arch=$vsArch -host_arch=$vsArch" -ErrorAction Stop
    # A repeated run also finds the linker through the persisted user PATH.
    $linker = (Get-Command link.exe -CommandType Application | Select-Object -First 1).Source
    if (-not $linker.StartsWith($env:VCToolsInstallDir, [StringComparison]::OrdinalIgnoreCase)) {
        throw "Expected the provisioned MSVC linker; resolved $linker"
    }
    $env:RUSTUP_TOOLCHAIN = $channel
    # Only explicit installations change the toolchain, so a missing one fails its probe.
    $env:RUSTUP_AUTO_INSTALL = '0'
    $env:CARGO_BUILD_JOBS = [string]$catalog.windows.'cargo-build-jobs'
    Remove-Item Env:RUSTC_WRAPPER -ErrorAction SilentlyContinue
    Remove-Item Env:CARGO_ENCODED_RUSTFLAGS -ErrorAction SilentlyContinue
    if ($current -and -not ((Test-Native 'rustc' @("+$channel", '-vV')) -and
            (Test-Pinned -Uv $uv -Helper $catalogHelper -Names (@('rustup') + $native.cargo)))) {
        Write-Warning "Tooling fingerprint $fingerprint matches, but a pinned tool is missing; converging."
        $current = $false
    }
    # rustup and each Cargo tool report their own versions; only a missing or different pin is installed.
    if (-not (Test-Pinned -Uv $uv -Helper $catalogHelper -Names @('rustup'))) {
        $rustupInstaller = Join-Path $temporary 'rustup-init.exe'
        Get-PinnedDownload $native.assets.rustup.url $native.assets.rustup.sha256 $rustupInstaller
        Invoke-Native $rustupInstaller @('-y', '--no-modify-path', '--default-host', $native.'rust-host', '--default-toolchain', 'none')
    }
    # A converged installation compiled verify.py's probes when it recorded its fingerprint.
    if (-not $current) {
        $rustArguments = @('toolchain', 'install', $channel, '--profile', 'minimal')
        foreach ($component in $native.components) { $rustArguments += @('--component', $component) }
        foreach ($target in $native.targets) { $rustArguments += @('--target', $target) }
        Invoke-Native 'rustup' $rustArguments
        foreach ($tool in $native.cargo) {
            if (Test-Pinned -Uv $uv -Helper $catalogHelper -Names @($tool)) { continue }
            Invoke-Native 'cargo' @("+$channel", 'binstall', '--locked', '--no-confirm', '--targets', $native.'rust-host', "$tool@$($catalog.cargo.$tool)")
        }
        # Match the Bash environment used by Just's recipes as well as PowerShell.
        Invoke-Native 'bash' @('-c', ('uv run python "' + ($PSScriptRoot -replace '\\', '/') + '/verify.py" ' + $Platform + ' ' + $Operation))
    }

    # Persist the complete MSVC/SDK environment, not only the paths to installed executables.
    foreach ($name in @('PATH', 'RUSTUP_TOOLCHAIN', 'CARGO_BUILD_JOBS', 'INCLUDE', 'LIB', 'LIBPATH', 'LIBCLANG_PATH',
        'VSINSTALLDIR', 'VCINSTALLDIR', 'VCToolsInstallDir', 'VCToolsVersion',
        'VSCMD_VER', 'VSCMD_ARG_TGT_ARCH', 'VSCMD_ARG_HOST_ARCH',
        'WindowsSdkDir', 'WindowsSDKVersion', 'WindowsSdkBinPath', 'WindowsSdkVerBinPath',
        'UniversalCRTSdkDir', 'UCRTVersion', 'WindowsLibPath')) {
        $value = [Environment]::GetEnvironmentVariable($name, 'Process')
        if ($value) {
            [Environment]::SetEnvironmentVariable($name, $value, 'User')
            if ($name -eq 'PATH') {
                if ($env:GITHUB_PATH) {
                    # Actions resolves the step shell before applying GITHUB_ENV.
                    # It prepends these entries in reverse order; retain MSVC first.
                    $runnerPaths = @($value -split ';' | Where-Object { $_ })
                    [Array]::Reverse($runnerPaths)
                    Add-Content -Encoding UTF8 $env:GITHUB_PATH $runnerPaths
                }
            } elseif ($env:GITHUB_ENV) {
                Add-Content -Encoding UTF8 $env:GITHUB_ENV "$name=$value"
            }
        }
    }
    if ($current) {
        Write-Output "Tooling fingerprint $fingerprint matches; new shells inherit the configured compiler environment."
    } else {
        Set-Content -Path $fingerprintFile -Value $fingerprint -Encoding ascii
        Write-Host "Installed $Platform $Operation tooling. New shells inherit the configured compiler environment."
    }
} finally {
    Remove-Item -Recurse -Force $temporary -ErrorAction SilentlyContinue
}
