# Builds the release binary and the MSI installer.
#   .\build.ps1 [-Arch x64|arm64] [-OutDir dist] [-SkipBuild] [-NoMsi]
#
# Output (under -OutDir, default .\dist):
#   <arch>\UselessTerminal.exe              the program, runs as is (no installer needed)
#   UselessTerminal-<version>-<arch>.msi    per-machine installer
#
# Needs: Rust (cargo) and, for the MSI, WiX 5 with its UI and Util extensions:
#   dotnet tool install -g wix --version 5.0.2
#   wix extension add -g WixToolset.UI.wixext/5.0.2
#   wix extension add -g WixToolset.Util.wixext/5.0.2
# (Pinned on purpose: `dotnet tool update -g wix` jumps to v7, which needs a paid EULA and breaks the extensions.)
#
# WARNING: the MSI deliberately shares its UpgradeCode with the old WPF installer. Installing it REPLACES the WPF
# Useless Terminal on that machine.
#
# Build output is several GB. If this folder is synced (OneDrive), build elsewhere:
#   $env:CARGO_TARGET_DIR = "$env:LOCALAPPDATA\ut-target"; .\build.ps1
param(
  [ValidateSet('x64', 'arm64')][string]$Arch = 'x64',
  [string]$OutDir = 'dist',
  [switch]$SkipBuild,
  [switch]$NoMsi
)
$ErrorActionPreference = 'Stop'
Set-Location $PSScriptRoot

# ---- version (MSI ProductVersion must be numeric major.minor.build; a -suffix is dropped for it) ----
$line = Select-String -Path 'Cargo.toml' -Pattern '^version\s*=\s*"([^"]+)"' | Select-Object -First 1
if (-not $line) { throw 'no version found in Cargo.toml' }
$version = $line.Matches[0].Groups[1].Value
if ($version -notmatch '^(\d+)\.(\d+)\.(\d+)') { throw "version '$version' is not major.minor.patch[-suffix]" }
$msiVersion = "$($Matches[1]).$($Matches[2]).$($Matches[3])"

# ---- binary ----
$native = $Arch -eq 'x64'
$triple = if ($native) { 'x86_64-pc-windows-msvc' } else { 'aarch64-pc-windows-msvc' }
$targetDir = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $PSScriptRoot 'target' }
if (-not $SkipBuild) {
  if (-not $env:CARGO_TARGET_DIR -and $PSScriptRoot -match 'OneDrive') {
    Write-Warning 'Building inside a OneDrive folder: the target directory will be several GB. Set CARGO_TARGET_DIR to a folder outside it.'
  }
  if (-not $native) { rustup target add $triple; if ($LASTEXITCODE) { throw 'rustup target add failed' } }
  $cargoArgs = @('build', '--release', '-p', 'useless-terminal')
  if (-not $native) { $cargoArgs += @('--target', $triple) }
  & cargo @cargoArgs
  if ($LASTEXITCODE) { throw 'cargo build failed' }
}
$built = if ($native) { Join-Path $targetDir 'release\UselessTerminal.exe' } else { Join-Path $targetDir "$triple\release\UselessTerminal.exe" }
if (-not (Test-Path $built)) { throw "release binary not found: $built (run without -SkipBuild)" }

$archDir = Join-Path $OutDir $Arch
New-Item -ItemType Directory -Force $archDir | Out-Null
$exe = Join-Path $archDir 'UselessTerminal.exe'
$locked = $false
try { Copy-Item $built $exe -Force -ErrorAction Stop }
catch {
  # Windows cannot overwrite an exe that is running (e.g. the copy in dist\ you are using): keep the new one beside it.
  $locked = $true
  $exe = Join-Path $archDir 'UselessTerminal.new.exe'
  Copy-Item $built $exe -Force
  Write-Warning "$archDir\UselessTerminal.exe is in use (a running instance?). The new binary was saved as UselessTerminal.new.exe: close the app, then re-run '.\build.ps1 -SkipBuild' (or rename it yourself)."
}
Write-Host "binary: $((Resolve-Path $exe).Path)  ($([math]::Round((Get-Item $exe).Length / 1MB, 1)) MB)"

# ---- MSI ----
if ($NoMsi) { return }
if (-not (Get-Command wix -ErrorAction SilentlyContinue)) {
  throw "wix not found, so no MSI was built. Install it with: dotnet tool install -g wix --version 5.0.2   (the binary above is ready)"
}
$msi = Join-Path $OutDir "UselessTerminal-$version-$Arch.msi"
$utilCA = if ($native) { 'X64' } else { 'A64' }
& wix build 'installer\UselessTerminal.wxs' -arch $Arch -ext WixToolset.UI.wixext -ext WixToolset.Util.wixext `
  -d "Version=$msiVersion" -d "ExePath=$((Resolve-Path $built).Path)" -d "IconPath=$((Resolve-Path 'crates\ut-app\assets\app.ico').Path)" -d "UtilCA=$utilCA" -o $msi
if ($LASTEXITCODE) { throw 'wix build failed (are the WixToolset.UI.wixext and WixToolset.Util.wixext 5.0.2 extensions installed? see the header of this script)' }
Remove-Item "$([IO.Path]::ChangeExtension($msi, 'wixpdb'))" -ErrorAction SilentlyContinue
Write-Host "msi:    $((Resolve-Path $msi).Path)  ($([math]::Round((Get-Item $msi).Length / 1MB, 1)) MB, ProductVersion $msiVersion)"
if ($locked) { exit 2 }   # the MSI is fine, but dist's exe could not be updated: say so through the exit code
