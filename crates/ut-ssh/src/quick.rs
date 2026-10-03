//! Quick connect grammar (§10.2): `[ssh://][user@]host[:port]`, IPv6 `[user@][addr]:port` or a bare
//! IPv6 address; input starting with `-` or containing whitespace is raw ssh arguments.
//! (WPF split on the last colon, which broke IPv6: §24 #21.)

use crate::target::tokenize;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuickConnect {
    /// ssh arguments (no exe): `["-p", "2222", "user@host"]`, or the tokenized raw input.
    pub args: Vec<String>,
    /// The argument text to put after the quoted exe (raw input is kept verbatim).
    pub command_tail: String,
    /// Tab title: `SSH: {input}`.
    pub display_title: String,
    pub raw: bool,
}

/// `None` for blank or malformed input (bad port, empty host/user, host starting with `-`).
pub fn parse_quick_connect(input: &str) -> Option<QuickConnect> {
    let input = input.trim();
    if input.is_empty() {
        return None;
    }
    let display_title = format!("SSH: {input}");
    if input.starts_with('-') || input.contains(char::is_whitespace) {
        return Some(QuickConnect { args: tokenize(input), command_tail: input.to_string(), display_title, raw: true });
    }
    let (user, host, port) = parse_destination(input)?;
    let mut args = Vec::new();
    if let Some(p) = port {
        args.extend(["-p".to_string(), p.to_string()]);
    }
    args.push(match user {
        Some(u) => format!("{u}@{host}"),
        None => host,
    });
    Some(QuickConnect { command_tail: args.join(" "), args, display_title, raw: false })
}

/// `"<ssh>" [-p <port>] [user@]host`, or `"<ssh>" <raw args>`.
pub fn quick_command_line(ssh_exe: &Path, input: &str) -> Option<String> {
    let q = parse_quick_connect(input)?;
    Some(format!("\"{}\" {}", ssh_exe.display(), q.command_tail))
}

/// Split `[ssh://][user@]host[:port]` into `(user, host, port)`; the host is returned unbracketed.
/// The user is everything before the LAST `@`. A single colon means `host:port`; two or more
/// colons mean a bare IPv6 address (no port); `[addr]:port` is the bracketed form.
pub(crate) fn parse_destination(s: &str) -> Option<(Option<String>, String, Option<u16>)> {
    let s = match s.get(..6) {
        Some(p) if p.eq_ignore_ascii_case("ssh://") => s[6..].trim_end_matches('/'),
        _ => s,
    };
    let (user, hp) = match s.rfind('@') {
        Some(i) => (Some(&s[..i]), &s[i + 1..]),
        None => (None, s),
    };
    let (host, port) = if let Some(rest) = hp.strip_prefix('[') {
        let (h, after) = rest.split_once(']')?;
        (h, if after.is_empty() { None } else { Some(after.strip_prefix(':')?) })
    } else if hp.matches(':').count() == 1 {
        let (h, p) = hp.split_once(':')?;
        (h, Some(p))
    } else {
        (hp, None)
    };
    let port = match port {
        None => None,
        Some(p) => Some(p.parse::<u16>().ok().filter(|&p| p != 0)?),
    };
    let ok = |t: &str| !t.is_empty() && !t.contains(|c: char| c.is_whitespace() || c == '"' || c.is_control());
    if !ok(host) || host.starts_with('-') || user.is_some_and(|u| !ok(u)) {
        return None;
    }
    Some((user.map(str::to_string), host.to_string(), port))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tail(input: &str) -> Option<String> {
        parse_quick_connect(input).map(|q| q.command_tail)
    }

    #[test]
    fn plain_forms() {
        assert_eq!(tail("host").unwrap(), "host");
        assert_eq!(tail("user@host").unwrap(), "user@host");
        assert_eq!(tail("user@host:2222").unwrap(), "-p 2222 user@host");
        assert_eq!(tail("host:22").unwrap(), "-p 22 host");
        assert_eq!(tail("ssh://bob@example.com:2200").unwrap(), "-p 2200 bob@example.com");
        assert_eq!(tail("SSH://example.com/").unwrap(), "example.com");
        assert_eq!(tail("a@b@host").unwrap(), "a@b@host"); // last '@' splits the user
        assert_eq!(tail("  user@host  ").unwrap(), "user@host");
    }

    #[test]
    fn ipv6_forms() {
        assert_eq!(tail("::1").unwrap(), "::1");
        assert_eq!(tail("2001:db8::1").unwrap(), "2001:db8::1"); // bare: no port
        assert_eq!(tail("root@2001:db8::1").unwrap(), "root@2001:db8::1");
        assert_eq!(tail("fe80::1%eth0").unwrap(), "fe80::1%eth0");
        assert_eq!(tail("[::1]:2222").unwrap(), "-p 2222 ::1");
        assert_eq!(tail("[::1]").unwrap(), "::1");
        assert_eq!(tail("user@[2001:db8::1]:22").unwrap(), "-p 22 user@2001:db8::1");
        assert_eq!(tail("ssh://root@[fe80::2]:2022").unwrap(), "-p 2022 root@fe80::2");
        assert_eq!(tail("::1:2222").unwrap(), "::1:2222"); // a valid address, not host+port
    }

    #[test]
    fn malformed_is_rejected() {
        for bad in ["", "   ", "host:", "host:abc", "host:0", "host:70000", "@host", "user@", "[::1", "[::1]x", "[::1]:", "u@-evil", "a\"b"] {
            assert!(parse_quick_connect(bad).is_none(), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn raw_passthrough() {
        let q = parse_quick_connect("-p 2222 -i \"C:\\my key\" user@host").unwrap();
        assert!(q.raw);
        assert_eq!(q.command_tail, "-p 2222 -i \"C:\\my key\" user@host");
        assert_eq!(q.args, ["-p", "2222", "-i", "C:\\my key", "user@host"]);
        let q = parse_quick_connect("user@host uptime").unwrap(); // contains a space -> raw
        assert!(q.raw);
        assert_eq!(q.args, ["user@host", "uptime"]);
        assert!(!parse_quick_connect("user@host").unwrap().raw);
    }

    #[test]
    fn title_and_command_line() {
        let q = parse_quick_connect(" bob@h:22 ").unwrap();
        assert_eq!(q.display_title, "SSH: bob@h:22");
        let exe = Path::new(r"C:\Program Files\Git\usr\bin\ssh.exe");
        assert_eq!(
            quick_command_line(exe, "bob@h:22").unwrap(),
            r#""C:\Program Files\Git\usr\bin\ssh.exe" -p 22 bob@h"#
        );
        assert_eq!(quick_command_line(exe, "-V").unwrap(), r#""C:\Program Files\Git\usr\bin\ssh.exe" -V"#);
        assert_eq!(quick_command_line(exe, ""), None);
    }
}
