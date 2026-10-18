# Install the decoy binary from GitHub Releases (Windows).
#
#   irm https://raw.githubusercontent.com/decoyhq/decoy/main/scripts/install.ps1 | iex
#   $env:DECOY_VERSION = '0.1.0-alpha.6'; irm ... | iex      # a specific version
#
# Env: DECOY_VERSION, DECOY_INSTALL_DIR (default ~\.decoy\bin), GITHUB_TOKEN (optional).
$ErrorActionPreference = 'Stop'

$repo = 'decoyhq/decoy'
$version = if ($env:DECOY_VERSION) { $env:DECOY_VERSION } else { 'latest' }
$installDir = if ($env:DECOY_INSTALL_DIR) { $env:DECOY_INSTALL_DIR } else { Join-Path $HOME '.decoy\bin' }

if ([System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture -ne 'X64') {
  throw "decoy-install: only x64 Windows builds are published"
}
$target = 'x86_64-pc-windows-msvc'

if ($version -eq 'latest') {
  # releases/latest skips prereleases, so take the newest release from the list instead.
  $headers = @{ Accept = 'application/vnd.github+json' }
  if ($env:GITHUB_TOKEN) { $headers.Authorization = "Bearer $env:GITHUB_TOKEN" }
  $version = (Invoke-RestMethod -Headers $headers "https://api.github.com/repos/$repo/releases?per_page=1")[0].tag_name
}
$version = $version.TrimStart('v')

$name = "decoy-$version-$target"
$base = "https://github.com/$repo/releases/download/v$version"
$tmp = Join-Path ([System.IO.Path]::GetTempPath()) ([System.IO.Path]::GetRandomFileName())
New-Item -ItemType Directory -Path $tmp | Out-Null

try {
  Write-Host "Installing decoy $version ($target)"
  $zip = Join-Path $tmp "$name.zip"
  Invoke-WebRequest -UseBasicParsing -OutFile $zip "$base/$name.zip"
  $sums = (Invoke-WebRequest -UseBasicParsing "$base/SHA256SUMS").Content
  if ($sums -is [byte[]]) { $sums = [System.Text.Encoding]::UTF8.GetString($sums) }

  $line = $sums -split "`n" | Where-Object { ($_ -split '\s+')[1] -eq "$name.zip" } | Select-Object -First 1
  if (-not $line) { throw "decoy-install: $name.zip is not listed in SHA256SUMS" }
  $expected = ($line -split '\s+')[0]
  $actual = (Get-FileHash -Algorithm SHA256 $zip).Hash
  if ($actual -ne $expected) { throw "decoy-install: checksum mismatch for $name.zip" }

  Expand-Archive -Path $zip -DestinationPath $tmp
  New-Item -ItemType Directory -Force -Path $installDir | Out-Null
  $exe = Join-Path $installDir 'decoy.exe'
  # A running decoy.exe (decoy upgrade) cannot be overwritten, but it can be renamed aside.
  if (Test-Path $exe) {
    Remove-Item -Force -ErrorAction SilentlyContinue "$exe.old"
    Move-Item -Force $exe "$exe.old"
  }
  Move-Item -Force (Join-Path $tmp "$name\decoy.exe") $exe
} finally {
  Remove-Item -Recurse -Force $tmp
}

$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if (($userPath -split ';') -notcontains $installDir) {
  [Environment]::SetEnvironmentVariable('Path', "$installDir;$userPath", 'User')
  $env:Path = "$installDir;$env:Path"
  Write-Host "Added $installDir to your user PATH (restart open terminals to pick it up)"
}
Write-Host "Installed $(& (Join-Path $installDir 'decoy.exe') --version) to $installDir\decoy.exe"
