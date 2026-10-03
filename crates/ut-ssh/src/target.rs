//! `SshTarget` and the argv parser (§10.4), `ssh -G` alias resolution, remote-cwd helpers (§10.6, §11.2).

use crate::quick::parse_destination;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::process::Command;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SshTarget {
    pub host: String,
    pub user: Option<String>,
    pub port: Option<u16>,
    pub identity: Option<String>,
    pub config: Option<String>,
    pub jump: Option<String>,
    pub control_path: Option<String>,
    /// `-o K=V` options kept for helper commands (extra `-i` files appear as `IdentityFile=…`).
    pub extra_options: Vec<String>,
    /// The pane's own command is ssh (as opposed to ssh found among descendants / OSC 7 host).
    pub is_primary_shell: bool,
}

/// Flags that consume a value (§10.4).
const VALUE_FLAGS: &str = "BbcDEeFIiJLlmOopQRSWw";

/// Windows-style tokenizer: `"` toggles quoted mode, whitespace splits outside quotes, no escapes.
pub fn tokenize(s: &str) -> Vec<String> {
    let (mut out, mut cur, mut quoted, mut in_tok) = (Vec::new(), String::new(), false, false);
    for c in s.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                in_tok = true;
            }
            c if c.is_whitespace() && !quoted => {
                if in_tok {
                    out.push(std::mem::take(&mut cur));
                    in_tok = false;
                }
            }
            c => {
                cur.push(c);
                in_tok = true;
            }
        }
    }
    if in_tok {
        out.push(cur);
    }
    out
}

/// `~`, `~/x`, `~\x` -> `%USERPROFILE%…`.
pub fn expand_tilde(p: &str) -> String {
    match p.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with(['/', '\\']) => {
            let s = format!("{}{rest}", ut_fs::home_dir().display());
            if cfg!(windows) { s.replace('/', "\\") } else { s }
        }
        _ => p.to_string(),
    }
}

fn is_ssh(argv0: &str) -> bool {
    let name = argv0.rsplit(['\\', '/']).next().unwrap_or("");
    let stem = name.rsplit_once('.').map_or(name, |(s, _)| s);
    stem.eq_ignore_ascii_case("ssh")
}

/// Parse a command line as an ssh invocation (§10.4). `None` when argv[0] is not ssh or no destination
/// is present. Like OpenSSH, options are accepted before the destination and directly after it; the
/// first non-option token after the destination starts the remote command, which is ignored.
pub fn parse_argv(cmdline: &str) -> Option<SshTarget> {
    let argv = tokenize(cmdline);
    if !is_ssh(argv.first()?) {
        return None;
    }
    let mut t = SshTarget::default();
    let (mut dest, mut login_user): (Option<&str>, Option<String>) = (None, None);
    let mut i = 1;
    while i < argv.len() {
        let a = argv[i].as_str();
        i += 1;
        if a == "--" {
            if dest.is_none() {
                dest = argv.get(i).map(String::as_str);
            }
            break;
        }
        if !a.starts_with('-') || a.len() == 1 {
            if dest.is_some() {
                break;
            }
            dest = Some(a);
            continue;
        }
        // A cluster such as `-tt`, `-vvv`, `-p2222`, `-oStrictHostKeyChecking=no`: walk the letters; the
        // first value-taking flag consumes the rest of the cluster or the next token.
        let flags = &a[1..];
        for (pos, c) in flags.char_indices() {
            if !VALUE_FLAGS.contains(c) {
                continue;
            }
            let rest = &flags[pos + c.len_utf8()..];
            let val = if rest.is_empty() {
                i += 1;
                argv.get(i - 1).cloned()
            } else {
                Some(rest.to_string())
            };
            if let Some(v) = val {
                apply_flag(&mut t, &mut login_user, c, v);
            }
            break;
        }
    }
    let d = dest?;
    let (uri_user, host, uri_port) = if d.len() >= 6 && d[..6].eq_ignore_ascii_case("ssh://") {
        parse_destination(d)?
    } else {
        match d.rfind('@') {
            Some(k) => (Some(d[..k].to_string()), d[k + 1..].to_string(), None),
            None => (None, d.to_string(), None),
        }
    };
    if host.is_empty() {
        return None;
    }
    t.host = host;
    t.user = login_user.or(uri_user).filter(|u| !u.is_empty());
    t.port = t.port.or(uri_port);
    Some(t)
}

fn apply_flag(t: &mut SshTarget, login_user: &mut Option<String>, flag: char, v: String) {
    match flag {
        'p' => {
            if let Ok(p) = v.parse::<u16>() {
                t.port = Some(p);
            }
        }
        'l' => *login_user = Some(v),
        'i' => {
            let v = expand_tilde(&v);
            if t.identity.is_none() {
                t.identity = Some(v);
            } else {
                t.extra_options.push(format!("IdentityFile={v}"));
            }
        }
        'F' => t.config = Some(expand_tilde(&v)),
        'J' => t.jump = Some(v),
        'o' => apply_option(t, &v),
        _ => {}
    }
}

fn apply_option(t: &mut SshTarget, opt: &str) {
    let is_sep = |c: char| c == '=' || c.is_whitespace();
    let (k, v) = match opt.split_once(is_sep) {
        Some((k, v)) => (k.trim(), v.trim_start_matches(is_sep)),
        None => (opt.trim(), ""),
    };
    match k.to_ascii_lowercase().as_str() {
        "" | "controlmaster" | "controlpersist" | "batchmode" => {}
        "controlpath" => {
            if t.control_path.is_none() {
                t.control_path = Some(v.to_string());
            }
        }
        _ => t.extra_options.push(format!("{k}={v}")),
    }
}

impl SshTarget {
    /// Minimal target for a remote host known only from OSC 7.
    pub fn from_host(host: &str) -> Self {
        Self { host: host.to_string(), ..Self::default() }
    }

    /// `user@host` or `host`.
    pub fn target_spec(&self) -> String {
        match &self.user {
            Some(u) => format!("{u}@{}", self.host),
            None => self.host.clone(),
        }
    }

    /// Connection options for helper `ssh` commands: `-o ControlPath=… -p -i -F -J -o…`.
    pub fn conn_args(&self) -> Vec<String> {
        self.args("-p")
    }

    /// Same, spelled for `scp` / `sftp` (`-P` for the port).
    pub fn scp_args(&self) -> Vec<String> {
        self.args("-P")
    }

    fn args(&self, port_flag: &str) -> Vec<String> {
        let mut a: Vec<String> = Vec::new();
        if let Some(c) = &self.control_path {
            a.extend(["-o".into(), format!("ControlPath={c}")]);
        }
        if let Some(p) = self.port {
            a.extend([port_flag.into(), p.to_string()]);
        }
        for (flag, v) in [("-i", &self.identity), ("-F", &self.config), ("-J", &self.jump)] {
            if let Some(v) = v {
                a.extend([flag.into(), v.clone()]);
            }
        }
        for o in &self.extra_options {
            a.extend(["-o".into(), o.clone()]);
        }
        a
    }
}

// ------------------------------------------------------------------ ssh -G

/// Effective values reported by `ssh -G` (keys are lowercase in its output).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedHost {
    pub hostname: String,
    pub user: Option<String>,
    pub port: Option<u16>,
    pub identityfile: Vec<String>,
    pub proxyjump: Option<String>,
}

pub fn parse_ssh_g(out: &str) -> Option<ResolvedHost> {
    let mut r = ResolvedHost::default();
    let mut found = false;
    for line in out.lines() {
        let Some((k, v)) = line.trim().split_once(' ') else { continue };
        let v = v.trim();
        match k {
            "hostname" => {
                r.hostname = v.to_string();
                found = true;
            }
            "user" => r.user = Some(v.to_string()),
            "port" => r.port = v.parse().ok(),
            "identityfile" => r.identityfile.push(v.to_string()),
            "proxyjump" if !v.eq_ignore_ascii_case("none") => r.proxyjump = Some(v.to_string()),
            _ => {}
        }
    }
    found.then_some(r)
}

const ALIAS_TTL: Duration = Duration::from_secs(60);
const ALIAS_TIMEOUT: Duration = Duration::from_secs(5);
type AliasCache = Mutex<HashMap<String, (Instant, Option<ResolvedHost>)>>;
static ALIAS_CACHE: OnceLock<AliasCache> = OnceLock::new();

fn g_args(t: &SshTarget) -> Vec<String> {
    let mut a = vec!["-G".to_string()];
    a.extend(t.conn_args());
    a.extend(["--".into(), t.target_spec()]);
    a
}

/// Resolve the destination through `ssh -G` (5 s timeout, killed on timeout), cached 60 s per
/// destination + connection options. Failures are cached too, so a dead config is not retried per event.
pub fn resolve_alias(target: &SshTarget, ssh_exe: &Path) -> Option<ResolvedHost> {
    resolve_alias_with(target, ssh_exe, Instant::now(), &|exe, args| {
        let o = crate::run(Command::new(exe).args(args), None, ALIAS_TIMEOUT, None).ok()?;
        (o.code == Some(0)).then_some(o.stdout)
    })
}

pub(crate) fn resolve_alias_with(
    target: &SshTarget,
    ssh_exe: &Path,
    now: Instant,
    runner: &dyn Fn(&Path, &[String]) -> Option<String>,
) -> Option<ResolvedHost> {
    let args = g_args(target);
    let key = format!("{}\0{}", ssh_exe.display(), args.join("\0"));
    let cache = ALIAS_CACHE.get_or_init(Default::default);
    if let Some((at, v)) = cache.lock().ok()?.get(&key) {
        if now.saturating_duration_since(*at) < ALIAS_TTL {
            return v.clone();
        }
    }
    let r = runner(ssh_exe, &args).and_then(|o| parse_ssh_g(&o));
    if let Ok(mut m) = cache.lock() {
        m.insert(key, (now, r.clone()));
    }
    r
}

// -------------------------------------------------------------- remote cwd

/// Title fallback (§10.6): text after the first `:`, trimmed, accepted if it starts with `~`, `/` or `.`
/// (matches `user@host: ~/dir`).
pub fn title_cwd(title: &str) -> Option<String> {
    let rest = title.split_once(':')?.1.trim();
    rest.starts_with(['~', '/', '.']).then(|| rest.to_string())
}

/// "Usable remote cwd" (§11.2): non-blank and (the OSC 7 host is not local, or it starts with `~`, or it
/// starts with `/` and is not an existing local directory). Pass `osc7_host_is_local = true` when no
/// remote OSC 7 host was seen.
pub fn is_remote_cwd_usable(cwd: &str, osc7_host_is_local: bool, local_dir_exists: bool) -> bool {
    let c = cwd.trim();
    !c.is_empty() && (!osc7_host_is_local || c.starts_with('~') || (c.starts_with('/') && !local_dir_exists))
}

/// Remote destination directory for a drop: the cwd if usable, else `~` (§11.2).
pub fn remote_dest(cwd: Option<&str>, osc7_host_is_local: bool) -> String {
    match cwd {
        Some(c) if is_remote_cwd_usable(c, osc7_host_is_local, Path::new(c.trim()).is_dir()) => c.trim().to_string(),
        _ => "~".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> SshTarget {
        parse_argv(s).unwrap_or_else(|| panic!("should parse: {s}"))
    }

    #[test]
    fn tokenizer() {
        assert_eq!(tokenize(r#"a  "b c" d"e f"g  "" "#), ["a", "b c", "de fg", ""]);
        assert_eq!(tokenize(r#""C:\Program Files\ssh.exe" h"#), [r"C:\Program Files\ssh.exe", "h"]);
        assert_eq!(tokenize(r"a\ b"), [r"a\", "b"]); // no backslash escapes
        assert!(tokenize("   ").is_empty());
    }

    #[test]
    fn argv0_must_be_ssh() {
        assert!(parse_argv("ssh host").is_some());
        assert!(parse_argv("ssh.exe host").is_some());
        assert!(parse_argv(r#""C:\Windows\System32\OpenSSH\ssh.exe" host"#).is_some());
        assert!(parse_argv(r"C:\Git\usr\bin\SSH.EXE host").is_some());
        assert!(parse_argv("C:/Git/usr/bin/ssh host").is_some());
        assert!(parse_argv("ssh-keygen host").is_none());
        assert!(parse_argv("scp host").is_none());
        assert!(parse_argv("pwsh -NoExit").is_none());
        assert!(parse_argv("").is_none());
        assert!(parse_argv("ssh").is_none());
        assert!(parse_argv("ssh -v").is_none());
    }

    #[test]
    fn basic_options_and_attached_values() {
        let t = p("ssh -p 2222 -l bob -J jump@h -F cfg -i key host");
        assert_eq!(t.host, "host");
        assert_eq!(t.user.as_deref(), Some("bob"));
        assert_eq!(t.port, Some(2222));
        assert_eq!(t.jump.as_deref(), Some("jump@h"));
        assert_eq!(t.config.as_deref(), Some("cfg"));
        assert_eq!(t.identity.as_deref(), Some("key"));
        assert!(!t.is_primary_shell);

        let t = p("ssh -p2222 -lbob -Jj -oStrictHostKeyChecking=no user@host");
        assert_eq!((t.port, t.user.as_deref(), t.jump.as_deref()), (Some(2222), Some("bob"), Some("j")));
        assert_eq!(t.extra_options, ["StrictHostKeyChecking=no"]);
        assert_eq!(t.host, "host");
    }

    #[test]
    fn destination_rules() {
        let t = p("ssh a@b@host"); // last '@'
        assert_eq!((t.user.as_deref(), t.host.as_str()), (Some("a@b"), "host"));
        let t = p("ssh -l winner user@host"); // -l wins
        assert_eq!(t.user.as_deref(), Some("winner"));
        let t = p("ssh -p 22 -- -weird"); // after `--`
        assert_eq!(t.host, "-weird");
        let t = p("ssh -v -- user@host ls -l"); // remote command ignored
        assert_eq!((t.user.as_deref(), t.host.as_str(), t.port), (Some("user"), "host", None));
        let t = p("ssh host ls -l -p 99"); // options after the remote command start belong to it
        assert_eq!((t.host.as_str(), t.port), ("host", None));
        let t = p("ssh host -p 2200 uptime"); // options directly after the destination count
        assert_eq!(t.port, Some(2200));
        let t = p("ssh ::1");
        assert_eq!(t.host, "::1");
    }

    #[test]
    fn value_flags_are_skipped() {
        let t = p("ssh -L 8080:localhost:80 -D 1080 -W h:p -b 1.2.3.4 -c aes -e ^ -m hmac -S ctl -w 0:0 -E log -I lib -O check -Q cipher -B eth0 -R 9:h:9 host");
        assert_eq!(t.host, "host");
        let t = p("ssh -N -tt -vvv -4 -A host");
        assert_eq!(t.host, "host");
        let t = p("ssh -NL8080:localhost:80 host"); // cluster ending in a value flag
        assert_eq!(t.host, "host");
        let t = p("ssh -L 80:h:80 -p 2 host");
        assert_eq!(t.port, Some(2));
    }

    #[test]
    fn o_handling() {
        let t = p("ssh -o ControlPath=/x/%C -o ControlMaster=auto -o ControlPersist=yes -o BatchMode=yes -o ServerAliveInterval=30 -o \"ProxyCommand ssh -W %h:%p j\" -ocompression=yes host");
        assert_eq!(t.control_path.as_deref(), Some("/x/%C"));
        assert_eq!(t.extra_options, ["ServerAliveInterval=30", "ProxyCommand=ssh -W %h:%p j", "compression=yes"]);
        let t = p("ssh -o \"ControlPath C:/a b/%C\" -o controlmaster=no host");
        assert_eq!(t.control_path.as_deref(), Some("C:/a b/%C"));
        assert!(t.extra_options.is_empty());
        let t = p("ssh -o ControlPath=first -o ControlPath=second host");
        assert_eq!(t.control_path.as_deref(), Some("first"));
    }

    #[test]
    fn tilde_expansion_for_i_and_f() {
        let home = ut_fs::home_dir().display().to_string();
        let t = p(r#"ssh -i ~/.ssh/id_ed25519 -F ~\cfg host"#);
        assert_eq!(t.identity.unwrap(), format!(r"{home}\.ssh\id_ed25519"));
        assert_eq!(t.config.unwrap(), format!(r"{home}\cfg"));
        assert_eq!(expand_tilde("~"), home);
        assert_eq!(expand_tilde("~bob/x"), "~bob/x");
        assert_eq!(expand_tilde("C:/k"), "C:/k");
        // second -i is kept as an option
        let t = p("ssh -i a -i b host");
        assert_eq!((t.identity.as_deref(), t.extra_options.as_slice()), (Some("a"), ["IdentityFile=b".to_string()].as_slice()));
    }

    #[test]
    fn invalid_port_is_ignored() {
        assert_eq!(p("ssh -p abc host").port, None);
        assert_eq!(p("ssh -p 99999 host").port, None);
    }

    #[test]
    fn ssh_uri_destination() {
        let t = p("ssh ssh://bob@example.com:2200");
        assert_eq!((t.user.as_deref(), t.host.as_str(), t.port), (Some("bob"), "example.com", Some(2200)));
        let t = p("ssh ssh://[::1]:2222/");
        assert_eq!((t.host.as_str(), t.port), ("::1", Some(2222)));
        let t = p("ssh -p 1 -l me ssh://bob@h:2200"); // explicit options win
        assert_eq!((t.user.as_deref(), t.port), (Some("me"), Some(1)));
        assert!(parse_argv("ssh ssh://").is_none());
    }

    #[test]
    fn conn_args_and_spec() {
        let t = p("ssh -p 2222 -i k -F c -J j -o ControlPath=/m/%C -o X=1 bob@h");
        assert_eq!(t.target_spec(), "bob@h");
        assert_eq!(
            t.conn_args(),
            ["-o", "ControlPath=/m/%C", "-p", "2222", "-i", "k", "-F", "c", "-J", "j", "-o", "X=1"]
        );
        assert_eq!(t.scp_args()[2], "-P");
        let m = SshTarget::from_host("example");
        assert_eq!(m.target_spec(), "example");
        assert!(m.conn_args().is_empty());
        let json = serde_json::to_value(&t).unwrap();
        assert_eq!(json["controlPath"], "/m/%C");
        assert_eq!(json["isPrimaryShell"], false);
        assert_eq!(json["extraOptions"][0], "X=1");
    }

    #[test]
    fn remote_cwd_helpers() {
        assert_eq!(title_cwd("user@host: ~/dir").as_deref(), Some("~/dir"));
        assert_eq!(title_cwd("user@host: /var/log ").as_deref(), Some("/var/log"));
        assert_eq!(title_cwd("x: ./rel").as_deref(), Some("./rel"));
        assert_eq!(title_cwd("user@host: vim file"), None);
        assert_eq!(title_cwd("no colon here"), None);
        assert_eq!(title_cwd(r"C:\Users\me"), None);

        assert!(!is_remote_cwd_usable("", false, false));
        assert!(!is_remote_cwd_usable("   ", false, false));
        assert!(is_remote_cwd_usable("relative", false, false)); // OSC 7 host is not local
        assert!(!is_remote_cwd_usable("relative", true, false));
        assert!(is_remote_cwd_usable("~/x", true, false));
        assert!(is_remote_cwd_usable("/srv", true, false));
        assert!(!is_remote_cwd_usable("/srv", true, true)); // exists locally -> it's a local path
        assert!(!is_remote_cwd_usable("C:\\x", true, true));

        assert_eq!(remote_dest(None, true), "~");
        assert_eq!(remote_dest(Some("~/proj"), true), "~/proj");
        assert_eq!(remote_dest(Some("garbage"), true), "~");
        assert_eq!(remote_dest(Some(" /srv/app "), false), "/srv/app");
    }

    #[test]
    fn parse_ssh_g_output() {
        let out = "user bob\r\nhostname 10.1.2.3\r\nport 2222\r\nidentityfile ~/.ssh/k1\r\nidentityfile ~/.ssh/k2\r\nproxyjump jumper\r\nforwardagent no\r\n";
        let r = parse_ssh_g(out).unwrap();
        assert_eq!(r.hostname, "10.1.2.3");
        assert_eq!((r.user.as_deref(), r.port, r.proxyjump.as_deref()), (Some("bob"), Some(2222), Some("jumper")));
        assert_eq!(r.identityfile, ["~/.ssh/k1", "~/.ssh/k2"]);
        assert_eq!(parse_ssh_g("garbage"), None);
        assert_eq!(parse_ssh_g("hostname h\nproxyjump none\n").unwrap().proxyjump, None);
    }

    #[test]
    fn alias_cache_hits_for_60s() {
        use std::cell::Cell;
        let t = p("ssh -p 4242 cache-test-alias");
        let exe = Path::new("fake-ssh");
        let calls = Cell::new(0);
        let runner = |_: &Path, args: &[String]| {
            calls.set(calls.get() + 1);
            assert_eq!(args[0], "-G");
            assert_eq!(args.last().unwrap(), "cache-test-alias");
            Some("hostname real.example\nport 4242\n".to_string())
        };
        let t0 = Instant::now();
        let a = resolve_alias_with(&t, exe, t0, &runner).unwrap();
        assert_eq!(a.hostname, "real.example");
        resolve_alias_with(&t, exe, t0 + Duration::from_secs(59), &runner).unwrap();
        assert_eq!(calls.get(), 1);
        resolve_alias_with(&t, exe, t0 + Duration::from_secs(61), &runner).unwrap();
        assert_eq!(calls.get(), 2);
        // failures are cached too
        let t2 = p("ssh failing-alias");
        let bad = |_: &Path, _: &[String]| {
            calls.set(calls.get() + 1);
            None
        };
        assert!(resolve_alias_with(&t2, exe, t0, &bad).is_none());
        assert!(resolve_alias_with(&t2, exe, t0, &bad).is_none());
        assert_eq!(calls.get(), 3);
    }

    #[test]
    fn resolve_alias_with_real_ssh() {
        let Some(ssh) = crate::locate::find_ssh() else { return };
        let dir = std::env::temp_dir().join(format!("ut ssh target {}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = dir.join("cfg file");
        std::fs::write(&cfg, "Host myalias\n  HostName 10.1.2.3\n  User bob\n  Port 2222\n  ProxyJump jumper\n").unwrap();
        let t = p(&format!("ssh -F \"{}\" myalias", cfg.display()));
        let r = resolve_alias(&t, &ssh).expect("ssh -G should work");
        assert_eq!(r.hostname, "10.1.2.3");
        assert_eq!((r.user.as_deref(), r.port, r.proxyjump.as_deref()), (Some("bob"), Some(2222), Some("jumper")));
    }
}
