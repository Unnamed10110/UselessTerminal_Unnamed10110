use std::collections::BTreeMap;

/// Serialise an environment as `KEY=VALUE\0…\0\0` UTF-16 (§4.3).
/// Keys are case-insensitive (later entries win, keeping the winner's casing) and the block is sorted
/// case-insensitively, as `CreateProcessW` requires.
pub fn build_env_block(env: &[(String, String)]) -> Vec<u16> {
    let mut map: BTreeMap<Vec<u16>, (&str, &str)> = BTreeMap::new();
    for (k, v) in env {
        // BTreeMap<Vec<u16>> orders by UTF-16 code unit of the uppercased key, like the OS does.
        let key: Vec<u16> = k.to_uppercase().encode_utf16().collect();
        map.insert(key, (k, v));
    }
    let mut out = Vec::new();
    for (k, v) in map.values() {
        out.extend(k.encode_utf16());
        out.push(b'=' as u16);
        out.extend(v.encode_utf16());
        out.push(0);
    }
    if out.is_empty() {
        out.push(0); // an empty block still needs one extra terminator
    }
    out.push(0);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(b: &[u16]) -> Vec<String> {
        b.split(|&c| c == 0).filter(|s| !s.is_empty()).map(String::from_utf16_lossy).collect()
    }

    #[test]
    fn sorted_case_insensitive_and_last_wins() {
        let env = vec![
            ("zeta".into(), "1".into()),
            ("Path".into(), "a".into()),
            ("PATH".into(), "b".into()),
            ("TERM".into(), "xterm-256color".into()),
            ("alpha".into(), "ü€".into()),
        ];
        let b = build_env_block(&env);
        assert_eq!(decode(&b), vec!["alpha=ü€", "PATH=b", "TERM=xterm-256color", "zeta=1"]);
        assert_eq!(&b[b.len() - 2..], &[0, 0]);
    }

    #[test]
    fn empty_env_is_double_nul() {
        assert_eq!(build_env_block(&[]), vec![0, 0]);
    }
}
