# Builds the artifacts and publishes a GitHub release.
#   scripts\release.ps1 -Tag v1.2.3 [-Draft] [-Prerelease] [-Notes "text"] [-SkipBuild]
param(
  [Parameter(Mandatory)][string]$Tag,
  [switch]$Draft,
  [switch]$Prerelease,
  [string]$Notes,
  [switch]$SkipBuild
)
$ErrorActionPreference = 'Stop'
Set-Location "$PSScriptRoot\.."
if ($Tag -notmatch '^v?\d+\.\d+\.\d+') { throw "tag '$Tag' must look like vX.Y.Z" }
$ver = $Tag.TrimStart('v')

$out = 'dist'
if (-not $SkipBuild) {
  Remove-Item "$out\*-$ver-*" -ErrorAction SilentlyContinue
  foreach ($arch in 'x64', 'arm64') { & "$PSScriptRoot\package.ps1" -Arch $arch -OutDir $out -Version $ver }
}
$files = Get-ChildItem "$out\UselessTerminal-$ver-*" | Where-Object { $_.Extension -in '.msi', '.exe', '.zip' }
if (-not $files) { throw "no artifacts for $ver in $out (run without -SkipBuild)" }

if (-not (git tag --list $Tag)) { git tag -a $Tag -m $Tag; if ($LASTEXITCODE) { throw 'git tag failed' } }
git push origin $Tag
if ($LASTEXITCODE) { throw 'git push failed' }

$ghArgs = @('release', 'create', $Tag) + $files.FullName + @('--title', $Tag)
if ($Draft) { $ghArgs += '--draft' }
if ($Prerelease) { $ghArgs += '--prerelease' }
$ghArgs += if ($Notes) { @('--notes', $Notes) } else { '--generate-notes' }
gh @ghArgs
