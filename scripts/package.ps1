# Builds the release exe and the three artifacts: portable ZIP, per-machine MSI (WiX), per-user NSIS installer.
#   scripts\package.ps1 [-Arch x64|arm64] [-SkipBuild] [-OutDir dist] [-Version 1.2.3-beta1]
# The MSI ProductVersion is numeric major.minor.build (suffix stripped); file names keep the full version.
# NEVER install the MSI on a machine whose WPF Useless Terminal you still want: it shares its UpgradeCode and replaces it.
param(
  [ValidateSet('x64', 'arm64')][string]$Arch = 'x64',
  [switch]$SkipBuild,
  [string]$OutDir = 'dist',
  [string]$Version
)
$ErrorActionPreference = 'Stop'
$root = Resolve-Path "$PSScriptRoot\.."
Set-Location $root

if (-not $Version) {
  $m = Select-String -Path 'Cargo.toml' -Pattern '^version\s*=\s*"([^"]+)"' | Select-Object -First 1
  $Version = $m.Matches[0].Groups[1].Value
}
if ($Version -notmatch '^v?(\d+)\.(\d+)\.(\d+)') { throw "version '$Version' is not major.minor.patch[-suffix]" }
$Version = $Version.TrimStart('v')
$msiVersion = "$($Matches[1]).$($Matches[2]).$($Matches[3])"

$triple = @{ x64 = 'x86_64-pc-windows-msvc'; arm64 = 'aarch64-pc-windows-msvc' }[$Arch]
if (-not $SkipBuild) {
  if ($Arch -eq 'arm64') { rustup target add $triple }
  cargo build --release -p useless-terminal --target $triple
  if ($LASTEXITCODE) { throw 'cargo build failed' }
}
$tdir = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $root 'target' }
$exe = Join-Path $tdir "$triple\release\UselessTerminal.exe"
if (-not (Test-Path $exe)) { $exe = Join-Path $tdir 'release\UselessTerminal.exe' } # default-target builds
if (-not (Test-Path $exe)) { throw "release exe not found under $tdir" }
$ico = Join-Path $root 'crates\ut-app\assets\app.ico'

New-Item -ItemType Directory -Force $OutDir | Out-Null
$base = "UselessTerminal-$Version-$Arch"

# Portable ZIP
$zip = Join-Path $OutDir "$base-portable.zip"
Remove-Item $zip -ErrorAction SilentlyContinue
Compress-Archive -Path $exe -DestinationPath $zip
"portable: $zip"

# MSI (WiX 5; pinned on purpose — `dotnet tool update -g wix` jumped to v7, which needs a paid EULA)
$wix = Get-Command wix -ErrorAction SilentlyContinue
if ($wix) {
  $msi = Join-Path $OutDir "$base.msi"
  $utilCA = @{ x64 = 'X64'; arm64 = 'A64' }[$Arch]
  wix build "$root\installer\UselessTerminal.wxs" -arch $Arch -ext WixToolset.UI.wixext -ext WixToolset.Util.wixext `
    -d Version=$msiVersion -d "ExePath=$exe" -d "IconPath=$ico" -d UtilCA=$utilCA -o $msi
  if ($LASTEXITCODE) { throw 'wix build failed' }
  "msi:      $msi"
} else { Write-Warning 'wix not found (dotnet tool install -g wix --version 5.0.2): MSI skipped' }

# NSIS
$nsis = Get-Command makensis -ErrorAction SilentlyContinue
if (-not $nsis) { $nsis = Get-ChildItem "${env:ProgramFiles(x86)}\NSIS\makensis.exe", "$env:ProgramFiles\NSIS\makensis.exe" -ErrorAction SilentlyContinue | Select-Object -First 1 }
if ($nsis) {
  $setup = Join-Path (Resolve-Path $OutDir) "$base-setup.exe"
  & $(if ($nsis.Source) { $nsis.Source } else { $nsis.FullName }) /V2 "/DVERSION=$msiVersion" "/DEXE=$exe" "/DICON=$ico" "/DOUT=$setup" "$root\installer\UselessTerminal.nsi"
  if ($LASTEXITCODE) { throw 'makensis failed' }
  "nsis:     $setup"
} else { Write-Warning 'makensis not found (install NSIS 3): NSIS installer skipped' }
