//! Backend clipboard access (§5.6): text, file lists (CF_HDROP) and "has an image" — more reliable than
//! `navigator.clipboard` inside WebView2.

use serde::Serialize;

pub fn set_text(text: &str) {
    if let Ok(mut c) = arboard::Clipboard::new() {
        let _ = c.set_text(text.to_string());
    }
}

pub fn get_text() -> Option<String> {
    arboard::Clipboard::new().ok()?.get_text().ok().filter(|t| !t.is_empty())
}

#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ClipboardInfo {
    pub text: Option<String>,
    pub files: Vec<String>,
    pub has_image: bool,
}

#[cfg(windows)]
fn formats() -> (Vec<String>, bool) {
    use windows::Win32::System::DataExchange::{
        CloseClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard, RegisterClipboardFormatW,
    };
    use windows::Win32::UI::Shell::{DragQueryFileW, HDROP};
    const CF_DIB: u32 = 8;
    const CF_HDROP: u32 = 15;
    const CF_DIBV5: u32 = 17;
    let mut files = Vec::new();
    let mut image = false;
    unsafe {
        let png = RegisterClipboardFormatW(windows::core::w!("PNG"));
        image = IsClipboardFormatAvailable(CF_DIB).is_ok()
            || IsClipboardFormatAvailable(CF_DIBV5).is_ok()
            || (png != 0 && IsClipboardFormatAvailable(png).is_ok())
            || image;
        if IsClipboardFormatAvailable(CF_HDROP).is_ok() && OpenClipboard(None).is_ok() {
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
    (files, image)
}

#[tauri::command]
pub async fn clipboard_read() -> ClipboardInfo {
    let text = get_text();
    let (files, has_image) = formats();
    ClipboardInfo { text, files, has_image }
}

#[tauri::command]
pub async fn clipboard_write(text: String) {
    set_text(&text);
}
