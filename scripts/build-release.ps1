# Build the gobstopper release binary for Windows x86_64 and package it as
# artifacts\gobstopper-<version>-windows-x86_64.zip plus <archive>.sha256
# ("<sha256>  <archive>"). The zip holds exactly one entry, gobstopper.exe;
# scripts/install.ps1 refuses anything else. The Windows counterpart of
# scripts/build-release.sh.
#
# GOBSTOPPER_VERSION (default: Cargo.toml) must match the binary.
# GOBSTOPPER_BINARY packages an already built gobstopper.exe instead of
# running `cargo build --release`; CI uses it to test packaging and the
# installer with the debug binary its tests built.

Set-StrictMode -Version 3.0
$ErrorActionPreference = 'Stop'

function Fail([string] $Message) {
  Write-Error "error: $Message"
  exit 1
}

$root = Split-Path -Parent $PSScriptRoot
$cargo = if ($env:CARGO) { $env:CARGO } else { 'cargo' }

if ($env:GOBSTOPPER_VERSION) {
  $version = $env:GOBSTOPPER_VERSION
  if ($version.StartsWith('v')) { $version = $version.Substring(1) }
} else {
  $manifest = [System.IO.File]::ReadAllText((Join-Path $root 'Cargo.toml'))
  $match = [regex]::Match($manifest, '(?ms)^\[workspace\.package\][^\[]*?^version = "([^"]+)"')
  if (-not $match.Success) { Fail 'could not read version from workspace Cargo.toml' }
  $version = $match.Groups[1].Value
}
if ($version -cnotmatch '^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$') {
  Fail "version must be MAJOR.MINOR.PATCH (got '$version')"
}
if ([System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture -ne [System.Runtime.InteropServices.Architecture]::X64) {
  Fail 'Windows release builds are x86_64 only'
}

if ($env:GOBSTOPPER_BINARY) {
  $binary = $env:GOBSTOPPER_BINARY
} else {
  Push-Location $root
  try {
    & $cargo build --release --locked -p gobstopper
    if ($LASTEXITCODE -ne 0) { Fail 'cargo build failed' }
  } finally {
    Pop-Location
  }
  $targetRoot = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $root 'target' }
  $binary = Join-Path $targetRoot 'release\gobstopper.exe'
}
if (-not (Test-Path -LiteralPath $binary -PathType Leaf)) { Fail "expected $binary" }
$reported = ((& $binary --version) | Out-String).Trim()
if ($LASTEXITCODE -ne 0 -or $reported -cne "gobstopper $version") {
  Fail "binary reports '$reported', expected 'gobstopper $version'"
}

$artifacts = Join-Path $root 'artifacts'
New-Item -ItemType Directory -Force -Path $artifacts | Out-Null
$name = "gobstopper-$version-windows-x86_64.zip"
$archive = Join-Path $artifacts $name
Remove-Item -LiteralPath $archive, "$archive.sha256" -Force -ErrorAction SilentlyContinue

Add-Type -AssemblyName System.IO.Compression
Add-Type -AssemblyName System.IO.Compression.FileSystem
$zip = [System.IO.Compression.ZipFile]::Open($archive, 'Create')
try {
  [System.IO.Compression.ZipFileExtensions]::CreateEntryFromFile($zip, $binary, 'gobstopper.exe', 'Optimal') | Out-Null
} finally {
  $zip.Dispose()
}
$digest = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant()
[System.IO.File]::WriteAllText("$archive.sha256", "$digest  $name`n", (New-Object System.Text.UTF8Encoding $false))

# Re-admit the packaged bytes the way the installer will.
$work = Join-Path ([System.IO.Path]::GetTempPath()) ("gobstopper-admit-" + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $work | Out-Null
try {
  $zip = [System.IO.Compression.ZipFile]::OpenRead($archive)
  try {
    if ($zip.Entries.Count -ne 1 -or $zip.Entries[0].FullName -cne 'gobstopper.exe') {
      Fail "$name must contain exactly one entry, gobstopper.exe"
    }
    $extracted = Join-Path $work 'gobstopper.exe'
    [System.IO.Compression.ZipFileExtensions]::ExtractToFile($zip.Entries[0], $extracted)
  } finally {
    $zip.Dispose()
  }
  $admitted = ((& $extracted --version) | Out-String).Trim()
  if ($LASTEXITCODE -ne 0 -or $admitted -cne "gobstopper $version") {
    Fail "packaged binary reports '$admitted', expected 'gobstopper $version'"
  }
} finally {
  Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue
}
Write-Output "archive=$archive sha256=$digest"
