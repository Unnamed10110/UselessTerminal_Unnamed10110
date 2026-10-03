//! Colour maths for theming (§13.3). Tolerant by design: bad input never panics.

fn parse(hex: &str) -> Option<[u8; 3]> {
    let digits: Vec<u8> = hex
        .trim()
        .strip_prefix('#')?
        .chars()
        .map(|c| c.to_digit(16).map(|d| d as u8))
        .collect::<Option<_>>()?;
    match digits[..] {
        [r, g, b] => Some([r * 17, g * 17, b * 17]),
        [a, b, c, d, e, f] => Some([a * 16 + b, c * 16 + d, e * 16 + f]),
        _ => None,
    }
}

/// `#rgb` / `#rrggbb` → channels; anything else is `#000000` (§13.3).
pub fn rgb(hex: &str) -> [u8; 3] {
    parse(hex).unwrap_or([0; 3])
}

/// Channels → uppercase `#RRGGBB`.
pub fn to_hex(c: [u8; 3]) -> String {
    format!("#{:02X}{:02X}{:02X}", c[0], c[1], c[2])
}

/// `clamp(round_half_even(a + (b - a) * t), 0, 255)` per channel, uppercase `#RRGGBB`.
pub fn mix(a: &str, b: &str, t: f64) -> String {
    let (a, b) = (rgb(a), rgb(b));
    let ch = |i: usize| {
        (f64::from(a[i]) + (f64::from(b[i]) - f64::from(a[i])) * t).round_ties_even().clamp(0.0, 255.0) as u8
    };
    to_hex([ch(0), ch(1), ch(2)])
}

/// Perceived brightness of the gamma-encoded value, 0..=1.
pub fn luma(hex: &str) -> f64 {
    let [r, g, b] = rgb(hex);
    (0.2126 * f64::from(r) + 0.7152 * f64::from(g) + 0.0722 * f64::from(b)) / 255.0
}

pub fn is_light(hex: &str) -> bool {
    luma(hex) > 0.55
}

/// Readable foreground for a given background.
pub fn contrast_fg(bg: &str) -> &'static str {
    if is_light(bg) { "#111111" } else { "#ffffff" }
}

/// Lowercase `#rrggbb` for saved settings; `None` when the input is not a valid `#rgb`/`#rrggbb`.
pub fn normalize_hex(s: &str) -> Option<String> {
    parse(s).map(|[r, g, b]| format!("#{r:02x}{g:02x}{b:02x}"))
}

/// Like [`normalize_hex`] but also accepts WPF's `#AARRGGBB` (alpha dropped); used by the migrations.
pub fn from_wpf(s: &str) -> Option<String> {
    let s = s.trim();
    match s.strip_prefix('#') {
        Some(h) if h.len() == 8 && h.is_ascii() => normalize_hex(&format!("#{}", &h[2..])),
        _ => normalize_hex(s),
    }
}

/// CSS `rgba(r,g,b,a)`; `accent-dim` is `with_alpha(accent, 0.14)` (§13.5).
pub fn with_alpha(hex: &str, alpha: f64) -> String {
    let [r, g, b] = rgb(hex);
    format!("rgba({r},{g},{b},{alpha})")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgb_is_tolerant() {
        assert_eq!(rgb("#fff"), [255, 255, 255]);
        assert_eq!(rgb("#6BE5ff"), [0x6b, 0xe5, 0xff]);
        assert_eq!(rgb(" #abc "), [0xaa, 0xbb, 0xcc]);
        for bad in ["zzz", "", "#", "#zzzzzz", "#12", "#1234567", "ffffff", "#ééé", "#éé🙂ab", "\u{0}"] {
            assert_eq!(rgb(bad), [0, 0, 0], "{bad:?}");
        }
    }

    #[test]
    fn mix_rounds_half_even_and_clamps() {
        assert_eq!(mix("#000000", "#010101", 0.5), "#000000"); // 0.5 -> 0
        assert_eq!(mix("#000000", "#030303", 0.5), "#020202"); // 1.5 -> 2
        assert_eq!(mix("#000000", "#ffffff", 0.15), "#262626"); // 38.25
        assert_eq!(mix("#102030", "#ffffff", 2.0), "#FFFFFF");
        assert_eq!(mix("#102030", "#000000", 5.0), "#000000");
        assert_eq!(mix("nope", "#ffffff", 1.0), "#FFFFFF");
        assert_eq!(mix("#000000", "#6be5ff", 0.12), "#0D1B1F");
    }

    #[test]
    fn luma_contrast_and_normalize() {
        assert!(!is_light("#6be5ff") || contrast_fg("#6be5ff") == "#111111");
        assert_eq!(contrast_fg("#6be5ff"), "#111111");
        assert_eq!(contrast_fg("#00838f"), "#ffffff");
        assert_eq!(contrast_fg("#000"), "#ffffff");
        assert!(is_light("#f7f7f8") && !is_light("#282a36"));
        assert!((luma("#ffffff") - 1.0).abs() < 1e-9);
        assert_eq!(normalize_hex("#ABC").as_deref(), Some("#aabbcc"));
        assert_eq!(normalize_hex("#6BE5FF").as_deref(), Some("#6be5ff"));
        assert_eq!(normalize_hex("blue"), None);
        assert_eq!(from_wpf("#FF6BE5FF").as_deref(), Some("#6be5ff"));
        assert_eq!(from_wpf("#6BE5FF").as_deref(), Some("#6be5ff"));
        assert_eq!(from_wpf("#FF6BE5F"), None);
    }

    #[test]
    fn alpha_string() {
        assert_eq!(with_alpha("#6be5ff", 0.14), "rgba(107,229,255,0.14)");
    }
}
