//! Clipboard access (§5.6): text, file lists (CF_HDROP) and "has an image".

#[derive(Default, Clone)]
pub struct ClipInfo {
    pub text: Option<String>,
    pub files: Vec<String>,
    pub has_image: bool,
}

pub fn set_text(text: &str) {
    if let Ok(mut c) = arboard::Clipboard::new() {
        let _ = c.set_text(text.to_string());
    }
}

pub fn get_text() -> Option<String> {
    // Another process (a clipboard manager, the app that just copied) can hold the clipboard for a few ms: retry briefly
    // instead of reporting "nothing to paste".
    for attempt in 0..8 {
        match arboard::Clipboard::new().and_then(|mut c| c.get_text()) {
            Ok(t) => return Some(t).filter(|t| !t.is_empty()),
            Err(arboard::Error::ClipboardOccupied) if attempt < 7 => std::thread::sleep(std::time::Duration::from_millis(20)),
            Err(_) => return None,
        }
    }
    None
}

/// Text first, then a file list, then "image only" (the caller decides what each means).
pub fn read() -> ClipInfo {
    use windows::Win32::System::DataExchange::{CloseClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard, RegisterClipboardFormatW};
    use windows::Win32::UI::Shell::{DragQueryFileW, HDROP};
    const CF_DIB: u32 = 8;
    const CF_HDROP: u32 = 15;
    const CF_DIBV5: u32 = 17;
    let text = get_text();
    let mut files = Vec::new();
    let image;
    unsafe {
        let png = RegisterClipboardFormatW(windows::core::w!("PNG"));
        image = IsClipboardFormatAvailable(CF_DIB).is_ok() || IsClipboardFormatAvailable(CF_DIBV5).is_ok() || (png != 0 && IsClipboardFormatAvailable(png).is_ok());
        if text.is_none() && IsClipboardFormatAvailable(CF_HDROP).is_ok() && OpenClipboard(None).is_ok() {
            if let Ok(h) = GetClipboardData(CF_HDROP) {
                let drop = HDROP(h.0);
                let n = DragQueryFileW(drop, u32::MAX, None);
                for i in 0..n {
                    let len = DragQueryFileW(drop, i, None) as usize;
                    let mut buf = vec![0u16; len + 1];
                    let got = DragQueryFileW(drop, i, Some(&mut buf)) as usize;
                    files.push(String::from_utf16_lossy(&buf[..got]));
                }
            }
            let _ = CloseClipboard();
        }
    }
    ClipInfo { text, files, has_image: image }
}

/// Sanitize pasted text (§5.6): ESC becomes U+241B so a pasted `ESC[201~` cannot break out of bracketed paste;
/// C1 controls are stripped.
pub fn sanitize_paste(text: &str) -> String {
    text.chars().filter(|c| !('\u{80}'..='\u{9f}').contains(c)).map(|c| if c == '\u{1b}' { '\u{241b}' } else { c }).collect()
}

/// Bytes to write for a paste: line endings → `\r`, wrapped in `ESC[200~ … ESC[201~` only when the program
/// enabled DECSET 2004 (§5.6, never forced on).
pub fn paste_bytes(text: &str, bracketed: bool) -> Vec<u8> {
    let clean = sanitize_paste(text);
    let body = clean.replace("\r\n", "\r").replace('\n', "\r");
    let mut out = Vec::with_capacity(body.len() + 12);
    if bracketed {
        out.extend_from_slice(b"\x1b[200~");
    }
    out.extend_from_slice(body.as_bytes());
    if bracketed {
        out.extend_from_slice(b"\x1b[201~");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paste_is_sanitized_and_only_bracketed_on_request() {
        let evil = "a\u{1b}[201~rm -rf\u{85}\nb\r\nc";
        let plain = String::from_utf8(paste_bytes(evil, false)).unwrap();
        assert_eq!(plain, "a\u{241b}[201~rm -rf\rb\rc");
        let br = paste_bytes("x\ny", true);
        assert!(br.starts_with(b"\x1b[200~") && br.ends_with(b"\x1b[201~"));
        assert_eq!(&br[6..br.len() - 6], b"x\ry");
    }
}
