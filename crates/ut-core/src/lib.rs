//! `ut-core`: theming (§13), settings (§14), keybindings (§15), window state (§16.3) and the
//! legacy WPF migrations (§16.5, Appendix C). Pure data + atomic file IO (via `ut-fs`); no UI,
//! no timers. All IO functions are plain synchronous fns.

pub mod color;
pub mod keybindings;
pub mod migrate;
pub mod presets;
pub mod settings;
pub mod theme;
pub mod windowstate;

pub use keybindings::{Chord, Conflict, InvalidBinding, KeyLoad, Keybindings};
pub use presets::Preset;
pub use settings::{apply_patch, Settings, SettingsStore};
pub use theme::{
    effective, effective_with, load_user_themes, preset_names, ui_css_vars, EffectiveTheme, ThemeOverrides, ThemeRef,
};
pub use windowstate::{clamp_bounds, PaneLayout, Rect, TabState, WindowState};

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{Map, Value};

/// How a persisted file was found at startup (§14.1, §16.5).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum LoadStatus {
    Loaded,
    /// First run: defaults are in use, nothing on disk yet.
    Missing,
    /// A legacy WPF file was converted; the original was copied to `backup`.
    Migrated { backup: String },
    /// The file could not be read/parsed (or migrated). It is untouched on disk.
    Corrupt { error: String },
}

pub(crate) fn to_json<T: Serialize>(v: &T) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

/// Build a `T` from arbitrary JSON: start from `T::default()` and overlay `user` leaf by leaf,
/// dropping every leaf the schema rejects (wrong type, unknown enum string, ...). Partial and
/// old files therefore always load; a bad value silently falls back to its default.
pub(crate) fn lenient<T: Default + Serialize + DeserializeOwned>(user: &Value) -> T {
    fn collect(v: &Value, path: &mut Vec<String>, out: &mut Vec<(Vec<String>, Value)>) {
        match v {
            Value::Object(m) => {
                for (k, x) in m {
                    path.push(k.clone());
                    collect(x, path, out);
                    path.pop();
                }
            }
            _ => out.push((path.clone(), v.clone())),
        }
    }
    fn set(root: &mut Value, path: &[String], v: Value) {
        let mut cur = root;
        for k in path {
            if !cur.is_object() {
                *cur = Value::Null; // IndexMut turns Null into an object
            }
            cur = &mut cur[k.as_str()];
        }
        *cur = v;
    }
    let mut cur = to_json(&T::default());
    let mut leaves = Vec::new();
    collect(user, &mut Vec::new(), &mut leaves);
    for (path, v) in leaves {
        let mut cand = cur.clone();
        set(&mut cand, &path, v);
        if serde_json::from_value::<T>(cand.clone()).is_ok() {
            cur = cand;
        }
    }
    serde_json::from_value(cur).unwrap_or_default()
}

/// RFC 7386 JSON merge patch: objects merge recursively, `null` deletes, everything else replaces.
pub(crate) fn merge_patch(target: &mut Value, patch: &Value) {
    let Value::Object(pm) = patch else {
        *target = patch.clone();
        return;
    };
    if !target.is_object() {
        *target = Value::Object(Map::new());
    }
    if let Value::Object(tm) = target {
        for (k, v) in pm {
            if v.is_null() {
                tm.remove(k);
            } else {
                merge_patch(tm.entry(k.clone()).or_insert(Value::Null), v);
            }
        }
    }
}

/// The merge patch that turns `a` into `b`; `None` when they are equal.
pub(crate) fn diff(a: &Value, b: &Value) -> Option<Value> {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            let mut out = Map::new();
            for (k, bv) in y {
                match x.get(k) {
                    Some(av) => out.extend(diff(av, bv).map(|d| (k.clone(), d))),
                    None => {
                        out.insert(k.clone(), bv.clone());
                    }
                }
            }
            out.extend(x.keys().filter(|k| !y.contains_key(*k)).map(|k| (k.clone(), Value::Null)));
            (!out.is_empty()).then_some(Value::Object(out))
        }
        _ => (a != b).then(|| b.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn merge_patch_rfc7386() {
        let mut t = json!({"a": {"b": 1, "c": 2}, "d": [1, 2], "e": 5});
        merge_patch(&mut t, &json!({"a": {"b": null, "x": {"y": 1}}, "d": [3], "e": null}));
        assert_eq!(t, json!({"a": {"c": 2, "x": {"y": 1}}, "d": [3]}));
        merge_patch(&mut t, &json!("scalar"));
        assert_eq!(t, json!("scalar"));
    }

    #[test]
    fn diff_roundtrips_through_merge_patch() {
        let a = json!({"a": {"b": 1, "c": 2}, "k": "x", "gone": true});
        let b = json!({"a": {"b": 1, "c": 3}, "k": "x", "new": {"n": 1}});
        let d = diff(&a, &b).unwrap();
        assert_eq!(d, json!({"a": {"c": 3}, "gone": null, "new": {"n": 1}}));
        let mut t = a.clone();
        merge_patch(&mut t, &d);
        assert_eq!(t, b);
        assert_eq!(diff(&a, &a), None);
    }
}
