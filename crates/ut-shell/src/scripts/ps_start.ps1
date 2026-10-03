# Only when a starting command is set:
try {
  $__utStart = [System.Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('{STARTB64}'))
  if ($__utStart) { Invoke-Expression $__utStart }
} catch {}
