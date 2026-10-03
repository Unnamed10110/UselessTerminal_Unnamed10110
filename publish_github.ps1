# Publishes UselessTerminal.exe as a GitHub release.
#   .\publish_github.ps1 v1.2.3                       build, then publish the exe (and its .sha256) as release v1.2.3
#   .\publish_github.ps1 v1.2.3 -SkipBuild            publish the exe already in dist\x64
#   .\publish_github.ps1 v1.2.3-beta.1                a "-suffix" tag is published as a pre-release
#   .\publish_github.ps1 v1.2.3 -Msi -Draft -Notes "text"
#   .\publish_github.ps1 v1.2.3 -WhatIf               checks everything and shows the plan; nothing is built, tagged or uploaded
#
# Needs the GitHub CLI, signed in:  gh auth login
# Publishing is outward-facing and hard to undo, so it asks for confirmation (-Confirm:$false skips the question).
#
# Two modes, picked automatically:
#   * inside a git repository: the tag is created on the current commit (annotated, if it does not exist yet) and pushed.
#     The commit must already be on the remote, otherwise the release would not contain the code the exe was built from.
#   * no git repository: pass -Repo owner/name; GitHub creates the tag itself on -Target (default: the default branch).
#
# Assets: UselessTerminal-<tag>-x64.exe, UselessTerminal-<tag>-x64.exe.sha256 and, with -Msi, the MSI.
# Refuses to publish when the exe's version does not match the tag, when the release already exists, or when the
# exe could not be rebuilt (see build.ps1: a running copy in dist\x64 blocks the update).
[CmdletBinding(SupportsShouldProcess, ConfirmImpact = 'High')]
param(
  [Parameter(Mandatory, Position = 0)][ValidatePattern('^v?\d+\.\d+\.\d+([-+][0-9A-Za-z.-]+)?$')][string]$Tag,
  [string]$Repo,            # owner/name; default: the GitHub remote of the current git repository
  [string]$Notes,           # release notes; default: GitHub's generated notes
  [string]$Target,          # branch or commit for the new tag when there is no local git repository
  [switch]$Draft,
  [switch]$Prerelease,
  [switch]$Msi,             # also attach the MSI installer
  [switch]$SkipBuild,       # use what is already in dist\
  [switch]$Force            # publish despite a dirty git tree or a version mismatch
)
$ErrorActionPreference = 'Stop'
Set-Location $PSScriptRoot

# Native commands that may legitimately fail: run them without turning stderr into an exception (Windows PowerShell 5.1).
function Test-Native([scriptblock]$Command) {
  $old = $ErrorActionPreference; $ErrorActionPreference = 'Continue'
  try { $null = & $Command 2>&1; return ($LASTEXITCODE -eq 0) } finally { $ErrorActionPreference = $old }
}
function Get-Native([scriptblock]$Command) {
  $old = $ErrorActionPreference; $ErrorActionPreference = 'Continue'
  try { $out = & $Command 2>$null; if ($LASTEXITCODE -ne 0) { return $null }; return ($out -join "`n").Trim() } finally { $ErrorActionPreference = $old }
}

# ---- tools ----
if (-not (Get-Command gh -ErrorAction SilentlyContinue)) { throw 'The GitHub CLI (gh) is not installed: https://cli.github.com/' }
if (-not (Test-Native { gh auth status })) { throw 'gh is not signed in. Run: gh auth login' }
$haveGit = [bool](Get-Command git -ErrorAction SilentlyContinue)
$inGit = $haveGit -and (Test-Native { git rev-parse --is-inside-work-tree })

# ---- versions ----
$cargo = Select-String -Path 'Cargo.toml' -Pattern '^version\s*=\s*"([^"]+)"' | Select-Object -First 1
if (-not $cargo) { throw 'no version found in Cargo.toml' }
$cargoVersion = $cargo.Matches[0].Groups[1].Value
$tagVersion = $Tag.TrimStart('v')
if ($tagVersion -ne $cargoVersion -and -not $Force) {
  throw "Tag $Tag does not match the version in Cargo.toml ($cargoVersion), which is what the exe reports. Bump the version first, or use -Force."
}
$pre = $Prerelease -or ($tagVersion -match '[-+]')

# ---- repository ----
if (-not $Repo) {
  $Repo = Get-Native { gh repo view --json nameWithOwner -q .nameWithOwner }
  if (-not $Repo) { throw 'Cannot tell which GitHub repository to publish to. Pass -Repo owner/name (or run this inside a clone of it).' }
}
if (Test-Native { gh release view $Tag --repo $Repo }) { throw "Release $Tag already exists in $Repo. Delete it on GitHub, or choose another tag." }

$tagExistsLocally = $false
if ($inGit) {
  if (-not $Force -and (Get-Native { git status --porcelain })) { throw 'The git working tree has uncommitted changes. Commit them first, or use -Force.' }
  if (-not (Get-Native { git branch -r --contains HEAD })) { throw 'The current commit is not on any remote branch. Push it first: the release must contain the code the exe was built from.' }
  $tagExistsLocally = Test-Native { git rev-parse -q --verify "refs/tags/$Tag" }
} else {
  Write-Warning "No git repository here: GitHub will create tag $Tag on $(if ($Target) { $Target } else { 'the default branch of ' + $Repo }), not on the code in this folder."
}

# ---- build ----
$arch = 'x64'
if (-not $SkipBuild) {
  $global:LASTEXITCODE = 0
  if ($WhatIfPreference) { Write-Host "What if: .\build.ps1 -Arch $arch$(if (-not $Msi) { ' -NoMsi' })" }
  else {
    $buildArgs = @{ Arch = $arch }
    if (-not $Msi) { $buildArgs.NoMsi = $true }
    & "$PSScriptRoot\build.ps1" @buildArgs
    if ($LASTEXITCODE) { throw "build.ps1 did not finish cleanly (exit $LASTEXITCODE). Close any running UselessTerminal.exe from dist\$arch and retry." }
  }
}
$exe = Join-Path $PSScriptRoot "dist\$arch\UselessTerminal.exe"
if (-not (Test-Path $exe)) { throw "$exe not found. Run .\build.ps1 first (or drop -SkipBuild)." }
$fileVersion = (Get-Item $exe).VersionInfo.FileVersion
if ($fileVersion -and -not $fileVersion.StartsWith($cargoVersion) -and -not $Force) {
  throw "dist\$arch\UselessTerminal.exe reports version $fileVersion but Cargo.toml says ${cargoVersion}: it is stale. Rebuild (drop -SkipBuild) or use -Force."
}

# ---- stage the assets (local files only: these run even under -WhatIf) ----
$stage = Join-Path $PSScriptRoot "dist\release\$Tag"
if (Test-Path $stage) { Remove-Item -LiteralPath $stage -Recurse -Force -WhatIf:$false }
New-Item -ItemType Directory -Force $stage -WhatIf:$false | Out-Null
$assetName = "UselessTerminal-$Tag-$arch.exe"
Copy-Item $exe (Join-Path $stage $assetName) -WhatIf:$false
# .NET, not Get-FileHash: in Windows PowerShell 5.1 that cmdlet inherits -WhatIf and returns nothing
$sha = [Security.Cryptography.SHA256]::Create()
try { $hash = -join ($sha.ComputeHash([IO.File]::ReadAllBytes((Join-Path $stage $assetName))) | ForEach-Object { $_.ToString('x2') }) } finally { $sha.Dispose() }
Set-Content -Path (Join-Path $stage "$assetName.sha256") -Value "$hash  $assetName" -Encoding ASCII -WhatIf:$false
$assets = @((Join-Path $stage $assetName), (Join-Path $stage "$assetName.sha256"))
if ($Msi) {
  $msiFile = Join-Path $PSScriptRoot "dist\UselessTerminal-$cargoVersion-$arch.msi"
  if (-not (Test-Path $msiFile)) { throw "$msiFile not found. Run .\build.ps1 first (or drop -SkipBuild)." }
  Copy-Item $msiFile $stage -WhatIf:$false
  $assets += (Join-Path $stage (Split-Path $msiFile -Leaf))
}

Write-Host ''
Write-Host "Repository : $Repo"
Write-Host "Tag        : $Tag$(if ($pre) { '  (pre-release)' })$(if ($Draft) { '  (draft)' })"
Write-Host "Exe        : $exe  ($([math]::Round((Get-Item $exe).Length / 1MB, 1)) MB, built $((Get-Item $exe).LastWriteTime.ToString('yyyy-MM-dd HH:mm')))"
Write-Host "SHA-256    : $hash"
Write-Host 'Assets     :'; $assets | ForEach-Object { Write-Host "  $(Split-Path $_ -Leaf)" }
Write-Host ''

# ---- publish (the only outward-facing part) ----
if (-not $PSCmdlet.ShouldProcess($Repo, "publish release $Tag")) { Write-Host 'Nothing was published.'; return }

if ($inGit) {
  if (-not $tagExistsLocally) { & git tag -a $Tag -m "Useless Terminal $Tag"; if ($LASTEXITCODE) { throw 'git tag failed' } }
  & git push origin $Tag
  if ($LASTEXITCODE) { throw "git push of tag $Tag failed" }
}
$ghArgs = @('release', 'create', $Tag) + $assets + @('--repo', $Repo, '--title', "Useless Terminal $Tag")
$ghArgs += if ($Notes) { @('--notes', $Notes) } else { '--generate-notes' }
$ghArgs += if ($inGit) { '--verify-tag' } elseif ($Target) { @('--target', $Target) } else { @() }
if ($Draft) { $ghArgs += '--draft' }
if ($pre) { $ghArgs += '--prerelease' }
& gh @ghArgs
if ($LASTEXITCODE) { throw 'gh release create failed' }
Write-Host ''
Write-Host (Get-Native { gh release view $Tag --repo $Repo --json url -q .url })
