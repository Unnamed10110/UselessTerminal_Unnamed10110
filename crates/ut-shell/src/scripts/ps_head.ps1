$ErrorActionPreference = 'SilentlyContinue'
$global:__utE = [string][char]27
$global:__utB = [string][char]7
function global:__utApplyInputColor { }

function global:__utWrap {
    $cur = (Get-Item Function:prompt -ErrorAction SilentlyContinue).ScriptBlock
    if ($cur -and $cur.ToString() -match '__utEmitCwdMarker') { return }
    if ($cur) { $global:__utOrigPrompt = $cur }
    function global:prompt {
        # __utEmitCwdMarker
        $__utOk  = $?
        $__utLec = $global:LASTEXITCODE
        try { __utApplyInputColor } catch {}
        try { __utWrapReadLine } catch {}

        $e = $global:__utE; $b = $global:__utB
        $out = ''

        $__utHid = -1
        try { $__utH = Get-History -Count 1 -ErrorAction SilentlyContinue; if ($__utH) { $__utHid = $__utH.Id } } catch {}
        if ($global:__utPromptSeen) {
            if ($__utHid -ne -1 -and $__utHid -eq $global:__utLastHistoryId) {
                $out += $e + ']133;D' + $b
            } else {
                $__utCode = 0
                if (-not $__utOk) {
                    if ($null -ne $__utLec -and "$__utLec" -ne '') { $__utCode = $__utLec } else { $__utCode = 1 }
                }
                $out += $e + ']133;D;' + $__utCode + $b
            }
        }
        $global:__utPromptSeen = $true
        $global:__utLastHistoryId = $__utHid

        $out += $e + ']133;A' + $b
        $p = $PWD.Path -replace '\\','/'
        $out += $e + ']7;file:///' + $p + $b

        $orig = ''
        if ($global:__utOrigPrompt) {
            try { $orig = & $global:__utOrigPrompt } catch { $orig = 'PS ' + $PWD.Path + '> ' }
        } else {
            $orig = 'PS ' + $PWD.Path + '> '
        }
        $out += [string]$orig

        $out += $e + ']133;B' + $b
        $out
    }
}

function global:__utWrapReadLine {
    $rl = Get-Item Function:PSConsoleHostReadLine -ErrorAction SilentlyContinue
    if (-not $rl) { return }
    if ($rl.ScriptBlock.ToString() -match '__utReadLineMarker') { return }
    $global:__utOrigReadLine = $rl.ScriptBlock
    function global:PSConsoleHostReadLine {
        # __utReadLineMarker
        $line = & $global:__utOrigReadLine
        try { [Console]::Write($global:__utE + ']133;C' + $global:__utB) } catch {}
        $line
    }
}

__utWrap

if (-not $global:__utOnIdleRegistered) {
    try {
        $null = Register-EngineEvent -SourceIdentifier PowerShell.OnIdle -Action {
            __utWrap
            try { __utApplyInputColor } catch {}
        }
        $global:__utOnIdleRegistered = $true
    } catch {}
}

[Console]::Write($global:__utE + ']7;file:///' + ($PWD.Path -replace '\\','/') + $global:__utB)
[Console]::Write($global:__utE + '[?2004h')
