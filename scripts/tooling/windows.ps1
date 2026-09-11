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

try {
    # Bootstrap only two scalar strings; Python's standard TOML parser reads the catalog afterward.
    $text = Get-Content -Raw $catalogPath
    $section = [regex]::Match($text, '(?ms)^\[' + [regex]::Escape($Platform) + '\.assets\.python\]\r?\n(?<body>.*?)(?=^\[|\z)')
    if (-not $section.Success) { throw 'Missing native Python bootstrap asset.' }
    $url = [regex]::Match($section.Groups['body'].Value, '(?m)^url = ("[^"\r\n]+")\r?$').Groups[1].Value | ConvertFrom-Json
    $sha = [regex]::Match($section.Groups['body'].Value, '(?m)^sha256 = ("[0-9a-f]{64}")\r?$').Groups[1].Value | ConvertFrom-Json
    $pythonDirectory = Join-Path $prefix ('python\' + $sha.Substring(0, 16))
    $python = Join-Path $pythonDirectory 'python.exe'
    if (-not (Test-Path $python)) {
        $archive = Join-Path $temporary 'python.zip'
        Get-PinnedDownload $url $sha $archive
        Expand-Archive -Path $archive -DestinationPath $pythonDirectory -Force
    }
    # The embedded interpreter is isolated; bind sibling imports to this checkout on every run.
    $pythonPathFile = (Get-ChildItem $pythonDirectory -Filter 'python*._pth' | Select-Object -First 1).FullName
    $pythonPaths = @(Get-Content $pythonPathFile | Where-Object { -not [IO.Path]::IsPathRooted($_) })
    [IO.File]::WriteAllLines($pythonPathFile, ($pythonPaths + $PSScriptRoot), [Text.UTF8Encoding]::new($false))
    $python3 = Join-Path $pythonDirectory 'python3.exe'
    if (-not (Test-Path $python3)) { New-Item -ItemType HardLink -Path $python3 -Target $python | Out-Null }
    $catalogHelper = Join-Path $PSScriptRoot 'catalog.py'
    $catalogJson = & $python $catalogHelper json
    if ($LASTEXITCODE -ne 0) { throw 'Unable to parse tooling catalog.' }
    $catalog = $catalogJson | ConvertFrom-Json
    Invoke-Native $python @($catalogHelper, 'validate')
    $selection = & $python $catalogHelper select $Platform $Operation
    if ($LASTEXITCODE -ne 0) { throw 'Unable to select operation tooling.' }
    $native = $selection | ConvertFrom-Json
    $channel = & $python $catalogHelper rust-channel $Platform
    if ($LASTEXITCODE -ne 0) { throw 'Unable to resolve the runner Rust toolchain.' }

    $channelFile = Join-Path $prefix ('vs-channel-' + $catalog.windows.'channel-sha256' + '.json')
    Get-PinnedDownload $catalog.windows.'channel-url' $catalog.windows.'channel-sha256' $channelFile
    $bootstrap = Join-Path $temporary 'vs_buildtools.exe'
    Get-PinnedDownload $catalog.windows.'bootstrap-url' $catalog.windows.'bootstrap-sha256' $bootstrap
    $vsPath = Join-Path $prefix ('vs-' + $catalog.windows.'visual-studio')
    $vsToolset = if ($Platform -eq 'aarch64-win') { 'Microsoft.VisualStudio.Component.VC.Tools.ARM64' } else { 'Microsoft.VisualStudio.Component.VC.Tools.x86.x64' }
    $vsArguments = @('--quiet', '--wait', '--norestart', '--noUpdateInstaller',
        '--installPath', ('"' + $vsPath + '"'), '--installChannelUri', ('"' + $channelFile + '"'),
        '--channelUri', ('"' + $channelFile + '"'), '--add', $vsToolset,
        '--add', $catalog.windows.'sdk-component')
    if ($Platform -eq 'x86_64-win') { $vsArguments += @('--add', 'Microsoft.VisualStudio.Component.VC.ASAN') }
    if (Test-Path (Join-Path $vsPath 'Common7\Tools\Microsoft.VisualStudio.DevShell.dll')) {
        $vsArguments = @('modify') + $vsArguments
    }
    Install-Exe $bootstrap $vsArguments
    $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
    $instances = & $vswhere -products Microsoft.VisualStudio.Product.BuildTools -format json | ConvertFrom-Json
    if ($LASTEXITCODE -ne 0) { throw 'Unable to inspect Visual Studio installation.' }
    $instance = @($instances | Where-Object { $_.installationPath -eq $vsPath -and $_.installationVersion -eq $catalog.windows.'build-version' })
    if ($instance.Count -ne 1) { throw 'Visual Studio does not match the pinned build.' }
    Import-Module (Join-Path $vsPath 'Common7\Tools\Microsoft.VisualStudio.DevShell.dll')
    $vsArch = if ($Platform -eq 'aarch64-win') { 'arm64' } else { 'amd64' }

    $gitInstaller = Join-Path $temporary 'git.exe'
    Get-PinnedDownload $native.assets.git.url $native.assets.git.sha256 $gitInstaller
    $gitDirectory = Join-Path $prefix ('git-' + $catalog.versions.git)
    Install-Exe $gitInstaller @('/VERYSILENT', '/NORESTART', '/NOCANCEL', '/SP-', ('/DIR="' + $gitDirectory + '"'))
    # Use the actual Bash binary. Git's bin/bash.exe launcher prepends usr/bin
    # again, shadowing MSVC's linker even after the developer shell is entered.
    $paths = @($pythonDirectory, (Join-Path $gitDirectory 'cmd'),
        (Join-Path $gitDirectory 'usr\bin'))
    foreach ($name in @('llvm', 'cmake', 'cargo-binstall')) {
        $directory = & $python (Join-Path $PSScriptRoot 'catalog.py') install-archive $Platform $name $prefix
        if ($LASTEXITCODE -ne 0) { throw "Unable to install $name" }
        $toolBin = if (Test-Path (Join-Path $directory 'bin')) { Join-Path $directory 'bin' } else { $directory }
        $paths += $toolBin
        if ($name -eq 'llvm') { $env:LIBCLANG_PATH = $toolBin }
    }
    $cargoBin = Join-Path $env:USERPROFILE '.cargo\bin'
    $paths += $cargoBin
    $env:PATH = (@($paths + ($env:PATH -split ';')) | Select-Object -Unique) -join ';'
    # Git's usr/bin also contains link.exe. Keep the native compiler ahead of it.
    Enter-VsDevShell -VsInstallPath $vsPath -SkipAutomaticLocation -DevCmdArguments "-arch=$vsArch -host_arch=$vsArch"
    $linker = (Get-Command link.exe -CommandType Application).Source
    if (-not $linker.StartsWith($env:VCToolsInstallDir, [StringComparison]::OrdinalIgnoreCase)) {
        throw "Expected the provisioned MSVC linker; resolved $linker"
    }
    $rustupInstaller = Join-Path $temporary 'rustup-init.exe'
    Get-PinnedDownload $native.assets.rustup.url $native.assets.rustup.sha256 $rustupInstaller
    Invoke-Native $rustupInstaller @('-y', '--no-modify-path', '--default-host', $native.'rust-host', '--default-toolchain', 'none')
    $rustArguments = @('toolchain', 'install', $channel, '--profile', 'minimal')
    foreach ($component in $native.components) { $rustArguments += @('--component', $component) }
    Invoke-Native 'rustup' $rustArguments
    $env:RUSTUP_TOOLCHAIN = $channel
    $env:CARGO_BUILD_JOBS = [string]$catalog.windows.'cargo-build-jobs'
    Remove-Item Env:RUSTC_WRAPPER -ErrorAction SilentlyContinue
    Remove-Item Env:CARGO_ENCODED_RUSTFLAGS -ErrorAction SilentlyContinue
    foreach ($tool in $native.cargo) {
        Invoke-Native 'cargo' @("+$channel", 'binstall', '--locked', '--no-confirm', '--targets', $native.'rust-host', "$tool@$($catalog.cargo.$tool)")
    }
    # Match the Bash environment used by Just's recipes as well as PowerShell.
    Invoke-Native 'bash' @('-c', ('python3 "' + ($PSScriptRoot -replace '\\', '/') + '/verify.py" ' + $Platform + ' ' + $Operation))

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
    Write-Host "Installed $Platform $Operation tooling. New shells inherit the configured compiler environment."
} finally {
    Remove-Item -Recurse -Force $temporary -ErrorAction SilentlyContinue
}
