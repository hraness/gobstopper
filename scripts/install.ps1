# Install Gobstopper on Windows x86_64 for the current user.
#
#   irm https://gobstopper.sh/install.ps1 | iex
#   $env:GOBSTOPPER_VERSION = "<version>"; irm https://gobstopper.sh/install.ps1 | iex
#
# Downloads gobstopper-<version>-windows-x86_64.zip from the GitHub Release,
# checks it against the release's .sha256 file, and installs
# %LOCALAPPDATA%\Programs\gobstopper\bin\gobstopper.exe. Nothing runs as
# administrator. The binary is not Authenticode-signed, so Windows may show a
# SmartScreen prompt the first time it runs.
#
# Options (environment):
#   GOBSTOPPER_VERSION         exact version, MAJOR.MINOR.PATCH (default: the latest release)
#   GOBSTOPPER_INSTALL_PREFIX  install into <prefix>\bin (default: %LOCALAPPDATA%\Programs\gobstopper)
#   GOBSTOPPER_ADD_PATH=no     leave your user PATH unchanged
# Source: https://github.com/hraness/gobstopper/blob/main/scripts/install.ps1
#
# Everything is inside a script block, so a partial download runs nothing.

& {
  Set-StrictMode -Version 3.0
  $ErrorActionPreference = 'Stop'
  $ProgressPreference = 'SilentlyContinue'

  function Fail([string] $Message) {
    throw "gobstopper install: $Message"
  }
  function Get-Sha256([string] $Path) {
    (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
  }

  $repository = 'hraness/gobstopper'
  $sourceHelp = "build from source instead: cargo install --git https://github.com/$repository --locked gobstopper"

  $arch = $env:PROCESSOR_ARCHITECTURE
  if ($env:PROCESSOR_ARCHITEW6432) { $arch = $env:PROCESSOR_ARCHITEW6432 }
  if ($arch -ne 'AMD64') { Fail "there is no release build for Windows $arch; $sourceHelp" }

  # Windows PowerShell 5.1 still offers TLS 1.0 by default.
  [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

  $baseUrl = $null
  # Tests serve a locally built archive from loopback; nothing else may
  # replace the GitHub Release as the source.
  if ($env:GOBSTOPPER_RELEASE_BASE_URL) {
    if ($env:GOBSTOPPER_RELEASE_BASE_URL -cnotmatch '^http://127\.0\.0\.1:[0-9]{1,5}$') {
      Fail 'GOBSTOPPER_RELEASE_BASE_URL may only name a loopback test server'
    }
    if (-not $env:GOBSTOPPER_VERSION) { Fail 'GOBSTOPPER_RELEASE_BASE_URL needs GOBSTOPPER_VERSION' }
    $baseUrl = $env:GOBSTOPPER_RELEASE_BASE_URL
  }

  $version = $env:GOBSTOPPER_VERSION
  if (-not $version) {
    try {
      $version = (Invoke-RestMethod -UseBasicParsing -Uri "https://api.github.com/repos/$repository/releases/latest").tag_name
    } catch {
      Fail 'could not find the latest release; set GOBSTOPPER_VERSION'
    }
  }
  if ($version.StartsWith('v')) { $version = $version.Substring(1) }
  if ($version -cnotmatch '^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$') {
    Fail "GOBSTOPPER_VERSION must be an exact release version, MAJOR.MINOR.PATCH (got '$version')"
  }
  if (-not $baseUrl) { $baseUrl = "https://github.com/$repository/releases/download/v$version" }

  if (-not $env:GOBSTOPPER_INSTALL_PREFIX -and -not $env:LOCALAPPDATA) {
    Fail 'LOCALAPPDATA is not set; set GOBSTOPPER_INSTALL_PREFIX to choose where gobstopper goes'
  }
  $prefix = if ($env:GOBSTOPPER_INSTALL_PREFIX) { $env:GOBSTOPPER_INSTALL_PREFIX } else { Join-Path $env:LOCALAPPDATA 'Programs\gobstopper' }
  if (-not [System.IO.Path]::IsPathRooted($prefix)) { Fail 'GOBSTOPPER_INSTALL_PREFIX must be an absolute path' }
  $prefix = [System.IO.Path]::GetFullPath($prefix)
  $binDir = Join-Path $prefix 'bin'
  New-Item -ItemType Directory -Force -Path $binDir | Out-Null

  $stage = Join-Path $binDir (".gobstopper-install-" + [Guid]::NewGuid().ToString('N'))
  New-Item -ItemType Directory -Path $stage | Out-Null
  try {
    # A replaced gobstopper.exe that was still running during the last
    # install could not be removed then; it can be now, unless it still runs.
    Get-ChildItem -LiteralPath $binDir -Filter 'gobstopper.exe.old-*' -Force -ErrorAction SilentlyContinue |
      ForEach-Object { Remove-Item -LiteralPath $_.FullName -Force -ErrorAction SilentlyContinue }

    $asset = "gobstopper-$version-windows-x86_64.zip"
    $archive = Join-Path $stage $asset
    Write-Host "Installing gobstopper $version for Windows x86_64"
    try {
      Invoke-WebRequest -UseBasicParsing -Uri "$baseUrl/$asset" -OutFile $archive
    } catch {
      Fail "could not download $asset; v$version may have no Windows build (see https://github.com/$repository/releases/tag/v$version)"
    }
    try {
      Invoke-WebRequest -UseBasicParsing -Uri "$baseUrl/$asset.sha256" -OutFile "$archive.sha256"
    } catch {
      Fail "could not download $asset.sha256"
    }
    $expected = (([System.IO.File]::ReadAllText("$archive.sha256")).Trim() -split '\s+')[0]
    if ($expected -cnotmatch '^[0-9a-f]{64}$') { Fail "$asset.sha256 is not a SHA-256 checksum" }
    $actual = Get-Sha256 $archive
    if ($actual -ne $expected) { Fail "checksum mismatch for $asset (expected $expected, got $actual)" }

    # Admit exactly one entry named gobstopper.exe and copy its bytes to our
    # own path: archive paths never choose where files land.
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $candidate = Join-Path $stage 'gobstopper.exe'
    $zip = [System.IO.Compression.ZipFile]::OpenRead($archive)
    try {
      if ($zip.Entries.Count -ne 1 -or $zip.Entries[0].FullName -cne 'gobstopper.exe') {
        Fail "$asset must contain only gobstopper.exe"
      }
      $source = $zip.Entries[0].Open()
      try {
        $target = [System.IO.File]::Open($candidate, 'CreateNew', 'Write', 'None')
        try { $source.CopyTo($target) } finally { $target.Dispose() }
      } finally { $source.Dispose() }
    } finally { $zip.Dispose() }

    $reported = ((& $candidate --version) | Out-String).Trim()
    if ($LASTEXITCODE -ne 0) { Fail 'the downloaded gobstopper.exe does not run on this system' }
    if ($reported -cne "gobstopper $version") { Fail "the downloaded binary reports '$reported', expected 'gobstopper $version'" }

    # A running gobstopper.exe cannot be overwritten or deleted, but it can be
    # renamed: move it aside, then move the new one into place.
    $destination = Join-Path $binDir 'gobstopper.exe'
    if (Test-Path -LiteralPath $destination) {
      $aside = Join-Path $binDir ("gobstopper.exe.old-" + [Guid]::NewGuid().ToString('N'))
      Move-Item -LiteralPath $destination -Destination $aside
      Remove-Item -LiteralPath $aside -Force -ErrorAction SilentlyContinue
    }
    Move-Item -LiteralPath $candidate -Destination $destination
    Write-Host "Installed $destination ($actual)"

    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    $entries = @()
    if ($userPath) { $entries = @($userPath.Split(';') | Where-Object { $_ }) }
    if ($entries -notcontains $binDir) {
      if ($env:GOBSTOPPER_ADD_PATH -eq 'no') {
        Write-Host "$binDir is not on your PATH."
      } else {
        [Environment]::SetEnvironmentVariable('Path', (($entries + $binDir) -join ';'), 'User')
        $env:Path = "$env:Path;$binDir"
        Write-Host "Added $binDir to your user PATH; open a new terminal to use it everywhere."
      }
    }
    Write-Host ''
    Write-Host 'Next: gobstopper detect'
  } finally {
    Remove-Item -LiteralPath $stage -Recurse -Force -ErrorAction SilentlyContinue
  }
}
