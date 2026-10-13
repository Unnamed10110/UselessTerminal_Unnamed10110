function global:__utApplyInputColor {
  if ($global:__utInputColorApplied) { return }
  try {
    Import-Module PSReadLine -ErrorAction Stop
    $vt = $global:__utE + '[38;2;{R};{G};{B}m'
    # Only the plain-text token: commands, keywords, parameters, strings... keep PSReadLine's own syntax colours
    # (or the ones the user's profile set), otherwise everything typed would look the same.
    Set-PSReadLineOption -ErrorAction Stop -Colors @{ Default = $vt }
    $global:__utInputColorApplied = $true
  } catch {}
}
