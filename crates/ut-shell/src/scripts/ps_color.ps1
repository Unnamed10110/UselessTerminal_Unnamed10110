function global:__utApplyInputColor {
  if ($global:__utInputColorApplied) { return }
  try {
    Import-Module PSReadLine -ErrorAction Stop
    $vt = $global:__utE + '[38;2;{R};{G};{B}m'
    Set-PSReadLineOption -ErrorAction Stop -Colors @{
      Command = $vt; Default = $vt; Number = $vt; Parameter = $vt; Operator = $vt
      Member = $vt; Variable = $vt; Keyword = $vt; Type = $vt; String = $vt
    }
    $global:__utInputColorApplied = $true
  } catch {}
}
