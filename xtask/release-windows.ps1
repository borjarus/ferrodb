<#
.SYNOPSIS
    Build FerroDb release packages on Windows (NSIS installer).

.DESCRIPTION
    Builds ferrodb-app in release mode and packages it with cargo-packager.
    If invoked from a UNC path (such as Parallels \\Mac\Home\...), the script
    automatically mirrors the source to C:\ferrodb-build first because CMake
    (used by bundled duckdb) does not support UNC working directories.

.PARAMETER Targets
    One or more Rust target triples to build.
    Defaults to "x86_64-pc-windows-msvc".
    For native Windows on ARM64, pass "aarch64-pc-windows-msvc".

.PARAMETER OutDir
    Directory where packaged installers are written.
    Defaults to "<repo>\target\releases\windows".

.PARAMETER LocalMirrorDir
    Local directory used when mirroring from a UNC path.
    Defaults to "C:\ferrodb-build".

.EXAMPLE
    .\xtask\release-windows.ps1

.EXAMPLE
    .\xtask\release-windows.ps1 -Targets aarch64-pc-windows-msvc

.EXAMPLE
    .\xtask\release-windows.ps1 -Targets x86_64-pc-windows-msvc, aarch64-pc-windows-msvc
#>
[CmdletBinding()]
param(
    [string[]]$Targets = @("x86_64-pc-windows-msvc"),
    [string]$OutDir = "",
    [string]$LocalMirrorDir = "C:\ferrodb-build"
)

$ErrorActionPreference = "Stop"

function Write-Step([string]$Message) {
    Write-Host "`n==> $Message" -ForegroundColor Cyan
}

function Fail([string]$Message) {
    Write-Error $Message
    exit 1
}

$RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path

if ([string]::IsNullOrWhiteSpace($OutDir)) {
    $OutDir = Join-Path $RepoRoot "target\releases\windows"
}

# Ensure cargo / rustup are on PATH
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    $CargoBin = Join-Path $env:USERPROFILE ".cargo\bin"
    if (Test-Path $CargoBin) {
        $env:Path = "$CargoBin;$env:Path"
    }
}
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    Fail "cargo not found. Install Rust via https://rustup.rs/ first."
}
if (-not (Get-Command rustup -ErrorAction SilentlyContinue)) {
    Fail "rustup not found. Install Rust via https://rustup.rs/ first."
}

# Warn if Visual Studio C++ Build Tools (needed for duckdb / ring) are not obvious
$VsWhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
if (-not (Get-Command cl.exe -ErrorAction SilentlyContinue) -and -not (Test-Path $VsWhere)) {
    Write-Warning "Visual Studio C++ Build Tools not detected. Bundled C/C++ crates (duckdb, rusqlite) require MSVC + CMake."
}

# If running from a UNC path (e.g. Parallels \\Mac\Home\...), mirror to local disk
$WorkDir = $RepoRoot
if ($RepoRoot.StartsWith("\\")) {
    Write-Step "UNC path detected ($RepoRoot). Mirroring to $LocalMirrorDir..."
    robocopy $RepoRoot $LocalMirrorDir /MIR /XD target .git .github spikes /NFL /NDL /NJH /NJS | Out-Null
    if ($LASTEXITCODE -ge 8) {
        Fail "robocopy failed with exit code $LASTEXITCODE"
    }
    $WorkDir = $LocalMirrorDir
}

# Install cargo-packager if missing
if (-not (Get-Command cargo-packager -ErrorAction SilentlyContinue)) {
    Write-Step "Installing cargo-packager..."
    cargo install cargo-packager --locked
    if ($LASTEXITCODE -ne 0) { Fail "Failed to install cargo-packager" }
}

$InstalledTargets = rustup target list --installed

New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$StageOut = Join-Path $WorkDir "target\packages-staging"
New-Item -ItemType Directory -Force -Path $StageOut | Out-Null
Remove-Item -Force -Recurse (Join-Path $StageOut "*") -ErrorAction SilentlyContinue

Push-Location $WorkDir
try {
    foreach ($Target in $Targets) {
        if (-not ($InstalledTargets -contains $Target)) {
            Write-Step "Adding Rust target $Target..."
            rustup target add $Target
            if ($LASTEXITCODE -ne 0) { Fail "Failed to add target $Target" }
        }

        Write-Step "Building ferrodb-app ($Target)..."
        cargo build --release --locked --target $Target -p ferrodb-app
        if ($LASTEXITCODE -ne 0) { Fail "cargo build failed for $Target" }

        Write-Step "Packaging ferrodb-app ($Target)..."
        cargo packager --release --target $Target -p ferrodb-app --out-dir $StageOut
        if ($LASTEXITCODE -ne 0) { Fail "cargo packager failed for $Target" }
    }
}
finally {
    Pop-Location
}

Write-Step "Copying artifacts to $OutDir..."
Copy-Item -Force -Recurse (Join-Path $StageOut "*") -Destination $OutDir

Write-Step "Done! Release artifacts:"
Get-ChildItem -Path $OutDir -File -Recurse | ForEach-Object {
    Write-Host "  $($_.FullName)"
}
