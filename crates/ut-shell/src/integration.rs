//! Shell-integration planner (§6.5, §6.6, Appendix A). `plan()` turns a launch request into
//! a rewritten command line, extra environment variables and timed stdin writes; the app
//! only executes the plan, it never builds script text.

use crate::kind::{detect_kind, file_stem_lower, split_exe_and_args, ShellKind};
use crate::quote::to_posix_path;
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use serde::{Deserialize, Serialize};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

// Appendix A, extracted verbatim from the spec.
const PS_HEAD: &str = include_str!("scripts/ps_head.ps1");
const PS_COLOR: &str = include_str!("scripts/ps_color.ps1");
const PS_START: &str = include_str!("scripts/ps_start.ps1");
const BASH: &str = include_str!("scripts/bash.sh");
const ZSH: &str = include_str!("scripts/zsh.sh");
const CMD_PROMPT: &str = include_str!("scripts/cmd_prompt.txt");

/// SGR default foreground: written locally to xterm when the typed input ends (§6.6).
pub const RESET_FG: &str = "\x1b[39m";
/// CreateProcess command-line limit in UTF-16 units (§4.2).
const MAX_CMDLINE_UNITS: usize = 32_767;
const DEFAULT_COLOR: (u8, u8, u8) = (255, 255, 255);

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum IntegrationMode {
    #[default]
    Auto,
    Off,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase", default)]
pub struct IntegrationRequest {
    /// The command line exactly as it would be passed to CreateProcessW.
    pub command_line: String,
    /// Session setting `integration`.
    #[serde(rename = "integration")]
    pub mode: IntegrationMode,
    /// Session `startingCommand` (blank = none).
    pub starting_command: Option<String>,
    /// `terminal.typedInputColor`, `#rrggbb` (anything else falls back to white).
    pub typed_input_color: String,
    /// `terminal.overridePsReadLineColors`.
    pub override_ps_readline: bool,
    /// The session's own environment overrides (to notice a user-defined `PROMPT`).
    pub session_env: Vec<(String, String)>,
    /// Where the Git-Bash init script is written; default: the system temp dir.
    pub temp_dir: Option<PathBuf>,
}

impl Default for IntegrationRequest {
    fn default() -> Self {
        Self {
            command_line: String::new(),
            mode: IntegrationMode::Auto,
            starting_command: None,
            typed_input_color: "#ffffff".into(),
            override_ps_readline: true,
            session_env: Vec::new(),
            temp_dir: None,
        }
    }
}

/// What the app must write to the PTY stdin, and when.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum PostSpawn {
    /// Wait for the first PTY output, then for `quiet_ms` without further output, then write
    /// `bytes`. If that has not happened `cap_ms` after spawn, write anyway (also when the
    /// shell printed nothing at all).
    WriteAfterFirstOutputQuiet { quiet_ms: u64, cap_ms: u64, bytes: Vec<u8> },
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct IntegrationPlan {
    pub kind: ShellKind,
    /// Command line to spawn (the request's, unless it was rewritten).
    pub command_line: String,
    /// Extra environment, layered after the parent env and before session overrides (§4.3).
    pub env: Vec<(String, String)>,
    pub post_spawn: Vec<PostSpawn>,
    /// Human-readable remarks for the pane (skips and fallbacks only).
    pub notes: Vec<String>,
    /// Shell integration (OSC 133/7) was set up by some mechanism.
    pub injected: bool,
    /// The UI should colour typed input itself (§6.6 mechanism 2): every shell but PowerShell,
    /// whose colour is applied by the injected script (mechanism 1).
    pub local_input_color: bool,
}

/// `ESC[38;2;r;g;bm` for `#rrggbb` (white when unparsable): written into xterm only (§6.6).
pub fn typed_input_escape(hex: &str) -> String {
    let (r, g, b) = rgb(hex);
    format!("\x1b[38;2;{r};{g};{b}m")
}

fn rgb(hex: &str) -> (u8, u8, u8) {
    let h = hex.trim().trim_start_matches('#');
    match u32::from_str_radix(h, 16) {
        Ok(v) if h.len() == 6 && h.bytes().all(|b| b.is_ascii_hexdigit()) => ((v >> 16) as u8, (v >> 8) as u8, v as u8),
        _ => DEFAULT_COLOR,
    }
}

/// Appendix A.1 with the colour substituted. The `__utApplyInputColor` definition is left
/// out when `override_psreadline` is false (the empty stub at the top stays); the
/// starting-command block (UTF-8 base64) only exists when `starting_command` is `Some`.
pub fn powershell_script(color_hex: &str, override_psreadline: bool, starting_command: Option<&str>) -> String {
    let (r, g, b) = rgb(color_hex);
    let mut parts = vec![PS_HEAD.trim_end().to_string()];
    if override_psreadline {
        parts.push(
            PS_COLOR
                .trim_end()
                .replace("{R}", &r.to_string())
                .replace("{G}", &g.to_string())
                .replace("{B}", &b.to_string()),
        );
    }
    if let Some(c) = starting_command {
        parts.push(PS_START.trim_end().replace("{STARTB64}", &B64.encode(c)));
    }
    parts.join("\n\n") + "\n"
}

pub fn plan(req: &IntegrationRequest) -> IntegrationPlan {
    let kind = detect_kind(&req.command_line);
    let (exe, args) = split_exe_and_args(&req.command_line);
    let stem = file_stem_lower(&exe);
    let start = req.starting_command.as_deref().filter(|s| !s.trim().is_empty());
    let mut p = IntegrationPlan {
        kind,
        command_line: req.command_line.clone(),
        env: Vec::new(),
        post_spawn: Vec::new(),
        notes: Vec::new(),
        injected: false,
        local_input_color: kind != ShellKind::PowerShell,
    };
    // `Some` while the starting command still needs the generic delivery below.
    let mut pending = start;
    if req.mode == IntegrationMode::Auto {
        match kind {
            ShellKind::PowerShell => {
                if inject_powershell(req, &mut p, &exe, &stem, &args, start) {
                    pending = None;
                }
            }
            ShellKind::Cmd => inject_cmd(req, &mut p),
            ShellKind::Posix | ShellKind::Wsl => {
                if inject_posix(req, &mut p, &exe, &stem, &args, start) {
                    pending = None;
                }
            }
            // §6.5: "we don't control the remote shell"
            ShellKind::Ssh | ShellKind::Unknown => {}
        }
    }
    if let Some(cmd) = pending {
        // §6.5.4. Ssh/Unknown use the [P1] variant: first output + 500 ms quiet, 10 s cap.
        let (eol, quiet, cap) = match kind {
            ShellKind::Cmd => ("\r", 150, 3_000),
            ShellKind::Posix | ShellKind::Wsl => ("\n", 150, 3_000),
            _ => ("\r", 500, 10_000),
        };
        queue(&mut p, quiet, cap, lines(cmd, eol));
    }
    p
}

fn queue(p: &mut IntegrationPlan, quiet_ms: u64, cap_ms: u64, text: String) {
    p.post_spawn.push(PostSpawn::WriteAfterFirstOutputQuiet { quiet_ms, cap_ms, bytes: text.into_bytes() });
}

/// CRLF/LF normalised; every line ended with `eol`.
fn lines(s: &str, eol: &str) -> String {
    s.replace("\r\n", "\n").replace('\r', "\n").lines().map(|l| format!("{l}{eol}")).collect()
}

// ------------------------------------------------------------------ PowerShell

/// Returns true when the starting command was baked into the script.
fn inject_powershell(req: &IntegrationRequest, p: &mut IntegrationPlan, exe: &str, stem: &str, args: &str, start: Option<&str>) -> bool {
    // `ssh host pwsh` is "PowerShell" by rule 1 but the process we launch is not one.
    if !matches!(stem, "pwsh" | "powershell") {
        return false;
    }
    if !ps_launch_applies(args) {
        p.notes.push("PowerShell integration skipped: the arguments run a file or command (-File/-Command/-EncodedCommand)".into());
        return false;
    }
    let mut tries = vec![start];
    if start.is_some() {
        tries.push(None); // too long with the starting command baked in: deliver it over stdin instead
    }
    for baked in tries {
        let script = powershell_script(&req.typed_input_color, req.override_ps_readline, baked);
        let cmd = ps_command(exe, args, &script);
        if cmd.encode_utf16().count() <= MAX_CMDLINE_UNITS {
            if baked.is_none() && start.is_some() {
                p.notes.push("Starting command is too long to embed in the PowerShell launch command; typing it instead".into());
            }
            p.command_line = cmd;
            p.injected = true;
            return baked.is_some();
        }
    }
    p.notes.push("PowerShell integration skipped: the launch command would exceed 32,767 characters".into());
    false
}

fn ps_command(exe: &str, args: &str, script: &str) -> String {
    let utf16le: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let args = if args.is_empty() { String::new() } else { format!("{args} ") };
    format!("\"{exe}\" {args}-NoExit -EncodedCommand {}", B64.encode(utf16le))
}

/// §6.5.1: no `-file`, `-command`, `-encodedcommand`, ` -c ` or leading `-c ` in the
/// (lowercased) arguments. Beyond the spec's substring test, PowerShell's parameter
/// abbreviations (`-f`, `-com`, `-ec`, `/c`, a bare `-`) and a trailing `-c` also disable it:
/// appending our own `-EncodedCommand` after them would be an error or run the wrong thing.
fn ps_launch_applies(args: &str) -> bool {
    let a = args.to_lowercase();
    if ["-file", "-command", "-encodedcommand", " -c "].iter().any(|s| a.contains(s)) || a.starts_with("-c ") || a == "-c" || a.ends_with(" -c") {
        return false;
    }
    !split_args(&a).iter().any(|t| {
        let Some(name) = t.strip_prefix(['-', '/']) else { return false };
        let name = name.trim_start_matches(['-', '/']);
        name.is_empty() || name == "ec" || ["file", "command", "encodedcommand"].iter().any(|full| full.starts_with(name))
    })
}

// ------------------------------------------------------------------------- cmd

fn inject_cmd(req: &IntegrationRequest, p: &mut IntegrationPlan) {
    // §6.5.2 [P1]: PROMPT in the environment block, unless the session defines its own.
    if req.session_env.iter().any(|(k, _)| k.eq_ignore_ascii_case("PROMPT")) {
        p.notes.push("cmd integration skipped: the session defines its own PROMPT".into());
        return;
    }
    p.env.push(("PROMPT".into(), CMD_PROMPT.trim().into()));
    p.injected = true;
}

// ----------------------------------------------------------------- bash / zsh

/// Returns true when the starting command went out with the init write.
fn inject_posix(req: &IntegrationRequest, p: &mut IntegrationPlan, exe: &str, stem: &str, args: &str, start: Option<&str>) -> bool {
    let toks = split_args(args);
    let wsl = p.kind == ShellKind::Wsl || is_system32(exe); // System32\bash.exe is the WSL launcher
    // Not an interactive shell: typing an init line into it would be wrong.
    let non_interactive = if p.kind == ShellKind::Wsl {
        toks.iter().any(|t| matches!(*t, "-e" | "--exec" | "--"))
    } else {
        toks.iter().any(|t| matches!(*t, "--init-file" | "--rcfile") || short_flags(t).is_some_and(|f| f.contains('c')))
    };
    if non_interactive {
        p.notes.push("Shell integration skipped: the arguments run a command instead of an interactive shell".into());
        return false;
    }
    // [P1] Git Bash / MSYS bash: launch-time `--init-file`.
    if stem == "bash" && !wsl {
        match bash_launch(req, exe, &toks) {
            Ok(cmd) => {
                p.command_line = cmd;
                p.injected = true;
                return false;
            }
            Err(e) => p.notes.push(format!("Could not write the bash init file ({e}); injecting through stdin")),
        }
    }
    // [P0] fallback: one init line on stdin (leading space: HISTCONTROL=ignorespace).
    let osc7 = if wsl { "" } else { "__UT_OSC7=1; " }; // a Linux path is no Windows directory
    let (zsh, bash) = (ZSH.trim(), BASH.trim());
    let mut text = format!(
        r#" {osc7}if [ -n "$ZSH_VERSION" ]; then {zsh} elif [ -n "$BASH_VERSION" ]; then {bash} fi; printf '\033[?2004h'; clear{}"#,
        "\n"
    );
    if let Some(s) = start {
        text.push_str(&lines(s, "\n"));
    }
    queue(p, 150, 3_000, text);
    p.injected = true;
    true
}

fn is_system32(exe: &str) -> bool {
    let l = exe.to_lowercase().replace('/', "\\");
    l.contains("\\windows\\system32\\") || l.contains("\\windows\\sysnative\\")
}

/// `bash --init-file <script> [other args] -i`, without `--login` (bash ignores
/// `--init-file` for login shells), the script re-creating what `--login` would have sourced.
fn bash_launch(req: &IntegrationRequest, exe: &str, toks: &[&str]) -> Result<String, String> {
    let login = toks.iter().any(|t| *t == "--login" || short_flags(t).is_some_and(|f| f.contains('l')));
    let mut extra = Vec::new();
    for t in toks {
        match short_flags(t) {
            _ if *t == "--login" => {}
            Some(f) => {
                let rest: String = f.chars().filter(|c| !matches!(c, 'i' | 'l')).collect();
                if !rest.is_empty() {
                    extra.push(format!("-{rest}"));
                }
            }
            None => extra.push(t.to_string()),
        }
    }
    let script = bash_init_script(login);
    let mut h = std::hash::DefaultHasher::new();
    script.hash(&mut h);
    let dir = req.temp_dir.clone().unwrap_or_else(std::env::temp_dir);
    let path = dir.join(format!("ut-bash-{:016x}.sh", h.finish()));
    write_if_changed(&path, script.as_bytes()).map_err(|e| e.to_string())?;
    let mut cmd = format!("\"{exe}\" --init-file \"{}\"", to_posix_path(&path.to_string_lossy()));
    for e in extra {
        cmd.push(' ');
        cmd.push_str(&e);
    }
    cmd.push_str(" -i");
    Ok(cmd)
}

/// Sources what bash would have (§6.5.3), then the A.2 snippet. LF endings only.
fn bash_init_script(login: bool) -> String {
    let user = if login {
        "[ -f /etc/profile ] && . /etc/profile\n\
         if [ -f ~/.bash_profile ]; then . ~/.bash_profile\n\
         elif [ -f ~/.bash_login ]; then . ~/.bash_login\n\
         elif [ -f ~/.profile ]; then . ~/.profile\n\
         fi\n"
    } else {
        "[ -f ~/.bashrc ] && . ~/.bashrc\n"
    };
    let script = format!("# Useless Terminal shell integration (generated)\n{user}__UT_OSC7=1\n{}\nprintf '\\033[?2004h'\n", BASH.trim());
    script.replace("\r\n", "\n")
}

/// Leaves an identical file alone, so a shell that is sourcing it right now is not disturbed.
fn write_if_changed(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if std::fs::read(path).is_ok_and(|old| old == bytes) {
        return Ok(());
    }
    ut_fs::write_atomic(path, bytes)
}

// -------------------------------------------------------------------- helpers

/// Whitespace-separated tokens of `args`, double quotes respected, original text kept.
fn split_args(args: &str) -> Vec<&str> {
    let (mut out, mut start, mut quoted) = (Vec::new(), None, false);
    for (i, c) in args.char_indices() {
        if c == '"' {
            quoted = !quoted;
        }
        match (c.is_whitespace() && !quoted, start) {
            (true, Some(s)) => {
                out.push(&args[s..i]);
                start = None;
            }
            (false, None) => start = Some(i),
            _ => {}
        }
    }
    if let Some(s) = start {
        out.push(&args[s..]);
    }
    out
}

/// `-il` → `il`; `--login`, `-`, `-x=1`, `-1` → `None`.
fn short_flags(tok: &str) -> Option<&str> {
    let f = tok.strip_prefix('-')?;
    (!f.is_empty() && f.bytes().all(|b| b.is_ascii_alphabetic())).then_some(f)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ShellKind::*;

    const PWSH_SP: &str = r"C:\Program Files\PowerShell\7\pwsh.exe";

    fn req(cmd: &str) -> IntegrationRequest {
        IntegrationRequest { command_line: cmd.into(), ..Default::default() }
    }
    fn with_start(cmd: &str, start: &str) -> IntegrationRequest {
        IntegrationRequest { starting_command: Some(start.into()), ..req(cmd) }
    }

    fn decode_encoded(cmdline: &str) -> String {
        let b64 = cmdline.rsplit("-EncodedCommand ").next().unwrap();
        let bytes = B64.decode(b64).unwrap();
        let units: Vec<u16> = bytes.chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        String::from_utf16(&units).unwrap()
    }

    fn written(p: &IntegrationPlan) -> (u64, u64, String) {
        assert_eq!(p.post_spawn.len(), 1, "{:?}", p.post_spawn);
        let PostSpawn::WriteAfterFirstOutputQuiet { quiet_ms, cap_ms, bytes } = &p.post_spawn[0];
        (*quiet_ms, *cap_ms, String::from_utf8(bytes.clone()).unwrap())
    }

    struct Tmp(PathBuf);
    impl Tmp {
        fn new(tag: &str) -> Tmp {
            let p = std::env::temp_dir().join(format!("ut shell integ {tag} {}", std::process::id()));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            Tmp(p)
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    // ------------------------------------------------------------ typed colour

    #[test]
    fn escape_and_reset() {
        assert_eq!(typed_input_escape("#ff8000"), "\x1b[38;2;255;128;0m");
        assert_eq!(typed_input_escape("00E5FF"), "\x1b[38;2;0;229;255m");
        assert_eq!(typed_input_escape("nonsense"), "\x1b[38;2;255;255;255m");
        assert_eq!(typed_input_escape("#12345"), "\x1b[38;2;255;255;255m");
        assert_eq!(typed_input_escape("#+12345"), "\x1b[38;2;255;255;255m");
        assert_eq!(RESET_FG, "\x1b[39m");
    }

    // -------------------------------------------------------------- PowerShell

    #[test]
    fn ps_applicability_matrix() {
        let yes = ["", "-NoLogo", "-NoProfile -ExecutionPolicy Bypass", "-NoLogo -WorkingDirectory C:\\a", "-Login", "-Interactive"];
        let no = [
            "-File x.ps1",
            "-file x.ps1",
            "-NoProfile -File \"C:\\a b\\x.ps1\"",
            "-Command Get-Date",
            "-COMMAND Get-Date",
            "-EncodedCommand AAAA",
            "-c Get-Date",
            "-NoLogo -c Get-Date",
            "-NoLogo -c",
            "-c",
            "-f x.ps1",
            "-com Get-Date",
            "-ec AAAA",
            "-e AAAA",
            "/c Get-Date",
            "-",
        ];
        for a in yes {
            assert!(ps_launch_applies(a), "{a:?} should allow injection");
            let p = plan(&req(&format!("pwsh {a}")));
            assert!(p.injected, "{a:?}");
        }
        for a in no {
            assert!(!ps_launch_applies(a), "{a:?} must not allow injection");
            let p = plan(&req(&format!("pwsh {a}")));
            assert!(!p.injected && p.command_line == format!("pwsh {a}"), "{a:?}");
            assert_eq!(p.notes.len(), 1);
        }
    }

    #[test]
    fn ps_command_shape_no_args() {
        let p = plan(&req("pwsh"));
        assert_eq!(p.kind, PowerShell);
        assert!(p.injected && p.post_spawn.is_empty() && p.env.is_empty() && p.notes.is_empty());
        assert!(!p.local_input_color);
        assert!(p.command_line.starts_with("\"pwsh\" -NoExit -EncodedCommand "), "{}", &p.command_line[..60]);
    }

    #[test]
    fn ps_command_shape_spaced_path_and_args() {
        for cmd in [format!("\"{PWSH_SP}\" -NoLogo"), format!("{PWSH_SP} -NoLogo")] {
            let p = plan(&req(&cmd));
            assert_eq!(p.kind, PowerShell);
            assert!(p.injected, "{cmd}");
            assert!(p.command_line.starts_with(&format!("\"{PWSH_SP}\" -NoLogo -NoExit -EncodedCommand ")), "{cmd}");
        }
        let p = plan(&req(&format!("\"{PWSH_SP}\"")));
        assert!(p.command_line.starts_with(&format!("\"{PWSH_SP}\" -NoExit -EncodedCommand ")));
        let p = plan(&req(r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe"));
        assert!(p.injected);
    }

    #[test]
    fn ps_script_content_and_colour() {
        let r = IntegrationRequest { typed_input_color: "#102030".into(), ..req("pwsh") };
        let s = decode_encoded(&plan(&r).command_line);
        for marker in [
            "$global:__utE = [string][char]27",
            "function global:__utApplyInputColor { }",
            "# __utEmitCwdMarker",
            "# __utReadLineMarker",
            "Register-EngineEvent -SourceIdentifier PowerShell.OnIdle",
            "']133;D;'",
            "']133;A'",
            "']133;B'",
            "']133;C'",
            "']7;file:///'",
            "'[?2004h'",
            "Import-Module PSReadLine -ErrorAction Stop",
            "$vt = $global:__utE + '[38;2;16;32;48m'",
            "Command = $vt; Default = $vt; Number = $vt; Parameter = $vt; Operator = $vt",
        ] {
            assert!(s.contains(marker), "missing {marker:?}");
        }
        assert!(!s.contains("{R}") && !s.contains("{G}") && !s.contains("{B}") && !s.contains("{STARTB64}"));
        assert!(!s.contains("Invoke-Expression"), "no start block without a starting command");
        // the stub comes first, the real definition later
        assert!(s.find("__utApplyInputColor { }").unwrap() < s.find("if ($global:__utInputColorApplied)").unwrap());
        // verbatim parts, byte for byte
        assert!(s.starts_with(PS_HEAD.trim_end()));
    }

    #[test]
    fn ps_override_off_omits_definition() {
        let r = IntegrationRequest { override_ps_readline: false, ..req("pwsh") };
        let s = decode_encoded(&plan(&r).command_line);
        assert!(s.contains("function global:__utApplyInputColor { }"));
        assert!(!s.contains("Set-PSReadLineOption") && !s.contains("__utInputColorApplied"));
        assert!(s.contains("__utWrap"), "the rest of the script is unchanged");
    }

    #[test]
    fn ps_starting_command_round_trips_utf8() {
        for start in ["Set-Location 'C:\\x y'; Write-Host 'héllo – ñandú 日本語 😀'", "echo 1\r\necho 2\n", "a'b\"c$d`e"] {
            let p = plan(&with_start("pwsh -NoLogo", start));
            assert!(p.injected && p.post_spawn.is_empty(), "baked in, nothing typed");
            let s = decode_encoded(&p.command_line);
            let b64 = s.split("FromBase64String('").nth(1).unwrap().split("')").next().unwrap();
            assert_eq!(String::from_utf8(B64.decode(b64).unwrap()).unwrap(), start);
            assert!(s.contains("Invoke-Expression $__utStart"));
            assert!(s.trim_end().ends_with("} catch {}"));
        }
    }

    #[test]
    fn ps_blank_starting_command_is_none() {
        let p = plan(&with_start("pwsh", "  \r\n "));
        assert!(p.post_spawn.is_empty());
        assert!(!decode_encoded(&p.command_line).contains("Invoke-Expression"));
    }

    #[test]
    fn ps_not_injected_still_types_starting_command() {
        let p = plan(&with_start("pwsh -File x.ps1", "echo hi\necho there"));
        assert!(!p.injected);
        assert_eq!(written(&p), (500, 10_000, "echo hi\recho there\r".to_string()));
        let off = IntegrationRequest { mode: IntegrationMode::Off, ..with_start("pwsh", "ls") };
        let p = plan(&off);
        assert!(!p.injected && p.command_line == "pwsh");
        assert_eq!(written(&p), (500, 10_000, "ls\r".to_string()));
    }

    #[test]
    fn ps_is_never_injected_into_a_non_powershell_exe() {
        let p = plan(&req("ssh host pwsh"));
        assert_eq!(p.kind, PowerShell);
        assert!(!p.injected && p.command_line == "ssh host pwsh" && p.notes.is_empty());
    }

    #[test]
    fn ps_overflow_retries_without_start_then_gives_up() {
        // base script is ~13 KB of base64; a ~7 KB starting command overflows 32,767 units
        let big = "x".repeat(7_500);
        let p = plan(&with_start("pwsh", &big));
        assert!(p.injected && p.command_line.encode_utf16().count() <= MAX_CMDLINE_UNITS);
        assert_eq!(p.notes.len(), 1);
        assert_eq!(written(&p).2, format!("{big}\r"));
        // args so long that even the bare script cannot fit
        let long_args = format!("-NoLogo -WorkingDirectory {}", "a".repeat(25_000));
        let cmd = format!("pwsh {long_args}");
        let p = plan(&req(&cmd));
        assert!(!p.injected && p.command_line == cmd);
        assert!(p.notes[0].contains("32,767"));
    }

    // --------------------------------------------------------------------- cmd

    #[test]
    fn cmd_sets_prompt_env() {
        for c in ["cmd", "cmd.exe", r"C:\Windows\System32\cmd.exe", "\"C:\\Windows\\System32\\cmd.exe\""] {
            let p = plan(&req(c));
            assert_eq!(p.kind, Cmd);
            assert!(p.injected && p.command_line == c && p.post_spawn.is_empty() && p.notes.is_empty(), "{c}");
            assert_eq!(p.env, vec![("PROMPT".to_string(), r"$e]133;D$e\$e]133;A$e\$e]7;file:///$P$e\$P$G$s$e]133;B$e\".to_string())]);
            assert!(p.local_input_color);
        }
    }

    #[test]
    fn cmd_respects_session_prompt() {
        let r = IntegrationRequest { session_env: vec![("Path".into(), "x".into()), ("prompt".into(), "$P$G".into())], ..req("cmd") };
        let p = plan(&r);
        assert!(!p.injected && p.env.is_empty());
        assert_eq!(p.notes.len(), 1);
    }

    #[test]
    fn cmd_starting_command_goes_to_stdin() {
        let p = plan(&with_start("cmd", "cd /d C:\\x\r\ndir\n\nver\n"));
        assert!(p.injected);
        assert_eq!(written(&p), (150, 3_000, "cd /d C:\\x\rdir\r\rver\r".to_string()));
        // still typed when integration is off, or the session owns PROMPT
        let off = IntegrationRequest { mode: IntegrationMode::Off, ..with_start("cmd", "dir") };
        let p = plan(&off);
        assert!(!p.injected && p.env.is_empty());
        assert_eq!(written(&p).2, "dir\r");
    }

    // ------------------------------------------------------------ bash / zsh

    #[test]
    fn posix_one_liner_composition() {
        let osc7 = format!(
            " __UT_OSC7=1; if [ -n \"$ZSH_VERSION\" ]; then {} elif [ -n \"$BASH_VERSION\" ]; then {} fi; printf '\\033[?2004h'; clear\n",
            ZSH.trim(),
            BASH.trim()
        );
        // zsh and generic sh go through stdin with OSC 7
        for c in [r"C:\msys64\usr\bin\zsh.exe", "sh -i", "bash -c"] {
            let p = plan(&req(c));
            if c == "bash -c" {
                assert!(!p.injected && p.post_spawn.is_empty());
                continue;
            }
            assert!(p.injected && p.command_line == c, "{c}");
            assert_eq!(written(&p), (150, 3_000, osc7.clone()), "{c}");
        }
        let (_, _, text) = written(&plan(&req("zsh")));
        assert!(text.starts_with(" __UT_OSC7=1; if [ -n \"$ZSH_VERSION\" ]; then __ut_seen=0; "));
        assert!(text.contains("; then __ut_mark=0; __ut_seen=0;"));
        assert!(text.ends_with("; fi; printf '\\033[?2004h'; clear\n"));
        assert_eq!(text.matches('\n').count(), 1);
        assert!(!text.contains('\r'));
    }

    #[test]
    fn wsl_one_liner_omits_osc7() {
        for c in ["wsl", "wsl.exe", r"C:\Windows\System32\wsl.exe -d Ubuntu", "wsl.exe -d \"Ubuntu-22.04\"", r"C:\Windows\System32\bash.exe"] {
            let p = plan(&req(c));
            assert!(p.injected && p.command_line == c, "{c}");
            let (q, cap, text) = written(&p);
            assert_eq!((q, cap), (150, 3_000));
            assert!(text.starts_with(" if [ -n \"$ZSH_VERSION\" ]; then ") && !text.contains("__UT_OSC7=1;"), "{c}");
            assert!(text.contains("elif [ -n \"$BASH_VERSION\" ]; then "));
        }
        assert_eq!(plan(&req("wsl")).kind, Wsl);
    }

    #[test]
    fn wsl_with_exec_command_is_left_alone() {
        for c in ["wsl.exe -d Ubuntu -- htop", "wsl -e top", "wsl --exec ls"] {
            let p = plan(&req(c));
            assert!(!p.injected && p.post_spawn.is_empty(), "{c}");
        }
    }

    #[test]
    fn posix_starting_command_appended_to_the_same_write() {
        let p = plan(&with_start("wsl.exe -d Ubuntu", "cd ~/src\r\nls -la\n"));
        let (_, _, text) = written(&p);
        assert!(text.ends_with("clear\ncd ~/src\nls -la\n"), "{text:?}");
        let p = plan(&with_start("zsh", "ls"));
        assert!(written(&p).2.ends_with("clear\nls\n"));
        // integration off: just the starting command
        let off = IntegrationRequest { mode: IntegrationMode::Off, ..with_start("wsl", "ls") };
        let p = plan(&off);
        assert!(!p.injected);
        assert_eq!(written(&p), (150, 3_000, "ls\n".to_string()));
    }

    #[test]
    fn git_bash_launch_time_login() {
        let t = Tmp::new("login");
        let r = IntegrationRequest {
            temp_dir: Some(t.0.clone()),
            ..req(r#""C:\Program Files\Git\bin\bash.exe" --login -i"#)
        };
        let p = plan(&r);
        assert_eq!(p.kind, Posix);
        assert!(p.injected && p.post_spawn.is_empty() && p.notes.is_empty());
        assert!(p.local_input_color);
        let posix_dir = to_posix_path(&t.0.to_string_lossy());
        assert!(p.command_line.starts_with(&format!("\"C:\\Program Files\\Git\\bin\\bash.exe\" --init-file \"{posix_dir}/ut-bash-")), "{}", p.command_line);
        assert!(p.command_line.ends_with(".sh\" -i"), "{}", p.command_line);
        assert!(!p.command_line.contains("--login"));
        // the script file exists with LF endings and sources the login files, then A.2
        let file = std::fs::read_dir(&t.0).unwrap().next().unwrap().unwrap().path();
        let s = std::fs::read_to_string(&file).unwrap();
        assert!(!s.contains('\r'));
        assert!(s.contains(". /etc/profile") && s.contains("~/.bash_profile") && s.contains("~/.bash_login") && s.contains("~/.profile"));
        assert!(!s.contains("~/.bashrc"));
        assert!(s.contains("__UT_OSC7=1\n") && s.contains(BASH.trim()));
        assert!(s.find("/etc/profile").unwrap() < s.find("__ut_precmd()").unwrap());
        // the path in the command line is the same file
        assert!(p.command_line.contains(file.file_name().unwrap().to_str().unwrap()));
        // second plan: same file name, content untouched
        let before = std::fs::metadata(&file).unwrap().modified().unwrap();
        let p2 = plan(&r);
        assert_eq!(p2.command_line, p.command_line);
        assert_eq!(std::fs::metadata(&file).unwrap().modified().unwrap(), before);
        assert_eq!(std::fs::read_dir(&t.0).unwrap().count(), 1);
    }

    #[test]
    fn git_bash_launch_time_non_login_and_args() {
        let t = Tmp::new("nonlogin");
        let r = IntegrationRequest { temp_dir: Some(t.0.clone()), ..with_start("bash.exe -i --noediting -il", "echo hi") };
        let p = plan(&r);
        assert!(p.injected);
        assert!(p.command_line.starts_with("\"bash.exe\" --init-file \""), "{}", p.command_line);
        assert!(p.command_line.ends_with(".sh\" --noediting -i"), "{}", p.command_line);
        // `-il` made it a login shell: the profile chain is sourced
        let s = std::fs::read_to_string(std::fs::read_dir(&t.0).unwrap().next().unwrap().unwrap().path()).unwrap();
        assert!(s.contains("/etc/profile"));
        // starting command is typed (no init line on stdin)
        assert_eq!(written(&p), (150, 3_000, "echo hi\n".to_string()));

        let t2 = Tmp::new("rc");
        let p = plan(&IntegrationRequest { temp_dir: Some(t2.0.clone()), ..req("bash") });
        assert!(p.command_line.starts_with("\"bash\" --init-file ") && p.command_line.ends_with(" -i"));
        let s = std::fs::read_to_string(std::fs::read_dir(&t2.0).unwrap().next().unwrap().unwrap().path()).unwrap();
        assert!(s.contains("[ -f ~/.bashrc ] && . ~/.bashrc") && !s.contains("/etc/profile"));
        assert!(p.post_spawn.is_empty());
    }

    #[test]
    fn git_bash_write_failure_falls_back_to_stdin() {
        let t = Tmp::new("fail");
        let blocker = t.0.join("file");
        std::fs::write(&blocker, b"x").unwrap();
        // temp_dir is a file: the script cannot be created
        let r = IntegrationRequest { temp_dir: Some(blocker), ..req("bash.exe --login -i") };
        let p = plan(&r);
        assert!(p.injected && p.command_line == "bash.exe --login -i");
        assert_eq!(p.notes.len(), 1);
        let (_, _, text) = written(&p);
        assert!(text.starts_with(" __UT_OSC7=1; if "), "git bash keeps OSC 7");
    }

    #[test]
    fn bash_with_command_or_rcfile_is_left_alone() {
        let t = Tmp::new("cmdc");
        for c in ["bash -c \"echo hi\"", "bash -lc ls", "bash --rcfile x -i", "bash --init-file x"] {
            let p = plan(&IntegrationRequest { temp_dir: Some(t.0.clone()), ..req(c) });
            assert!(!p.injected && p.post_spawn.is_empty() && p.command_line == c, "{c}");
        }
        assert_eq!(std::fs::read_dir(&t.0).unwrap().count(), 0);
    }

    // ------------------------------------------------------------ ssh / unknown

    #[test]
    fn ssh_and_unknown_are_never_injected() {
        for c in ["ssh user@host", r#""C:\Program Files\OpenSSH\ssh.exe" -p 22 u@h"#, "nu.exe", "python.exe -i"] {
            let p = plan(&req(c));
            assert!(!p.injected && p.command_line == c && p.env.is_empty() && p.post_spawn.is_empty() && p.notes.is_empty(), "{c}");
            assert!(p.local_input_color);
        }
        assert_eq!(plan(&req("ssh h")).kind, Ssh);
        assert_eq!(plan(&req("nu.exe")).kind, Unknown);
    }

    #[test]
    fn ssh_and_unknown_starting_command_waits_for_quiet() {
        for c in ["ssh user@host", "nu.exe"] {
            let p = plan(&with_start(c, "tmux attach\r\nls\n"));
            assert!(!p.injected);
            assert_eq!(written(&p), (500, 10_000, "tmux attach\rls\r".to_string()), "{c}");
        }
    }

    // ------------------------------------------------------------------ misc

    #[test]
    fn helper_splitting() {
        assert_eq!(split_args(r#"--a "b c" -d  "e f"g h"#), vec!["--a", "\"b c\"", "-d", "\"e f\"g", "h"]);
        assert!(split_args("   ").is_empty());
        assert_eq!(short_flags("-il"), Some("il"));
        assert_eq!(short_flags("--login"), None);
        assert_eq!(short_flags("-"), None);
        assert_eq!(short_flags("-1"), None);
        assert_eq!(lines("a\r\nb\rc\n\n", "\r"), "a\rb\rc\r\r");
        assert_eq!(lines("", "\n"), "");
    }

    #[test]
    fn request_and_plan_serde() {
        let r: IntegrationRequest = serde_json::from_str(r#"{"commandLine":"cmd","integration":"off","startingCommand":"dir"}"#).unwrap();
        assert_eq!(r.mode, IntegrationMode::Off);
        assert!(r.override_ps_readline && r.typed_input_color == "#ffffff");
        let j = serde_json::to_value(plan(&r)).unwrap();
        assert_eq!(j["kind"], "cmd");
        assert_eq!(j["postSpawn"][0]["type"], "writeAfterFirstOutputQuiet");
        assert_eq!(j["postSpawn"][0]["quietMs"], 150);
        assert_eq!(j["localInputColor"], true);
        let r: IntegrationRequest = serde_json::from_str("{}").unwrap();
        assert_eq!(r.mode, IntegrationMode::Auto);
    }

    #[test]
    fn a_script_with_non_ascii_colour_input_is_safe() {
        let r = IntegrationRequest { typed_input_color: "#ñññññ".into(), ..req("pwsh") };
        assert!(decode_encoded(&plan(&r).command_line).contains("[38;2;255;255;255m"));
    }
}
