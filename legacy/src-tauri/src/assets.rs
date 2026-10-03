//! Custom protocols (§5.12, §9.3, §18.3): `utasset://` serves ONLY the configured background image,
//! `uticon://` serves ONLY the icon cache. Neither ever touches an arbitrary path.

use crate::state::AppState;
use std::path::{Path, PathBuf};
use tauri::http::{Request, Response};
use tauri::{Manager, State, UriSchemeContext, UriSchemeResponder};

const MAX_BG: u64 = 15 * 1024 * 1024;
const BG_EXTS: [&str; 6] = ["png", "jpg", "jpeg", "gif", "webp", "bmp"];

fn mime(ext: &str) -> &'static str {
    match ext {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        _ => "application/octet-stream",
    }
}

fn respond(code: u16, ctype: &str, body: Vec<u8>) -> Response<Vec<u8>> {
    Response::builder()
        .status(code)
        .header("Content-Type", ctype)
        .header("Cache-Control", "no-store")
        .header("Access-Control-Allow-Origin", "*")
        .body(body)
        .unwrap_or_else(|_| Response::new(Vec::new()))
}

/// FNV-1a: a stable, dependency-free cache key (the spec's sha1 is only a cache key too).
fn fnv(s: &str) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

fn valid_bg(path: &str) -> Option<PathBuf> {
    let p = PathBuf::from(path.trim());
    let ext = p.extension()?.to_string_lossy().to_ascii_lowercase();
    let md = std::fs::metadata(&p).ok()?;
    (md.is_file() && md.len() <= MAX_BG && BG_EXTS.contains(&ext.as_str())).then_some(p)
}

/// URL for the configured background image (cache-busted by mtime), or `None` when unset/invalid.
#[tauri::command]
pub async fn background_url(state: State<'_, AppState>) -> Result<Option<String>, String> {
    let path = state.cfg().terminal.background_image.path;
    let Some(p) = valid_bg(&path) else { return Ok(None) };
    let mtime = std::fs::metadata(&p).and_then(|m| m.modified()).ok().map(|t| format!("{t:?}")).unwrap_or_default();
    Ok(Some(format!("http://utasset.localhost/bg/{}", fnv(&format!("{path}{mtime}")))))
}

pub fn asset_protocol(ctx: UriSchemeContext<'_, tauri::Wry>, _req: Request<Vec<u8>>, responder: UriSchemeResponder) {
    let app = ctx.app_handle().clone();
    std::thread::spawn(move || {
        let state = app.state::<AppState>();
        let path = state.cfg().terminal.background_image.path;
        let resp = match valid_bg(&path).and_then(|p| {
            let ext = p.extension()?.to_string_lossy().to_ascii_lowercase();
            std::fs::read(&p).ok().map(|b| (mime(&ext), b))
        }) {
            Some((m, bytes)) => respond(200, m, bytes),
            None => respond(404, "text/plain", Vec::new()),
        };
        responder.respond(resp);
    });
}

// ------------------------------------------------------------------ icons

fn icon_dir() -> PathBuf {
    ut_fs::local_data_dir().join("icon-cache")
}

pub fn icon_protocol(_ctx: UriSchemeContext<'_, tauri::Wry>, req: Request<Vec<u8>>, responder: UriSchemeResponder) {
    // Only `<hex>.png` inside the cache directory — no traversal possible.
    let name = req.uri().path().trim_start_matches('/').to_string();
    let ok = name.len() <= 40 && name.ends_with(".png") && name[..name.len() - 4].chars().all(|c| c.is_ascii_hexdigit());
    let resp = if ok {
        match std::fs::read(icon_dir().join(&name)) {
            Ok(b) => respond(200, "image/png", b),
            Err(_) => respond(404, "text/plain", Vec::new()),
        }
    } else {
        respond(400, "text/plain", Vec::new())
    };
    responder.respond(resp);
}

/// Icon URL for a command line: resolved executable → shell icon → cached PNG (§9.3). Empty string = fallback.
#[tauri::command]
pub async fn icon_for(command: String) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let Some(exe) = ut_shell::resolve_exe(&command).or_else(|| existing_file(&command)) else { return String::new() };
        let mtime = std::fs::metadata(&exe).and_then(|m| m.modified()).ok().map(|t| format!("{t:?}")).unwrap_or_default();
        let key = fnv(&format!("{}{mtime}", exe.display()));
        let file = icon_dir().join(format!("{key}.png"));
        if !file.exists() {
            let Some(png) = extract_icon_png(&exe) else { return String::new() };
            if std::fs::create_dir_all(icon_dir()).is_err() || ut_fs::write_atomic(&file, &png).is_err() {
                return String::new();
            }
        }
        format!("http://uticon.localhost/{key}.png")
    })
    .await
    .map_err(|e| e.to_string())
}

fn existing_file(s: &str) -> Option<PathBuf> {
    let p = Path::new(s.trim().trim_matches('"'));
    p.is_file().then(|| p.to_path_buf())
}

/// Large shell icon of `exe` as a PNG.
fn extract_icon_png(exe: &Path) -> Option<Vec<u8>> {
    use windows::core::HSTRING;
    use windows::Win32::Graphics::Gdi::{
        CreateCompatibleDC, DeleteDC, DeleteObject, GetDIBits, GetObjectW, BITMAP, BITMAPINFO, BITMAPINFOHEADER,
        BI_RGB, DIB_RGB_COLORS, HGDIOBJ,
    };
    use windows::Win32::Storage::FileSystem::FILE_FLAGS_AND_ATTRIBUTES;
    use windows::Win32::UI::Shell::{SHGetFileInfoW, SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON};
    use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, GetIconInfo, ICONINFO};
    unsafe {
        let mut fi = SHFILEINFOW::default();
        let r = SHGetFileInfoW(&HSTRING::from(exe.as_os_str()), FILE_FLAGS_AND_ATTRIBUTES(0), Some(&mut fi), size_of::<SHFILEINFOW>() as u32, SHGFI_ICON | SHGFI_LARGEICON);
        if r == 0 || fi.hIcon.is_invalid() {
            return None;
        }
        let mut info = ICONINFO::default();
        let mut out = None;
        if GetIconInfo(fi.hIcon, &mut info).is_ok() {
            let mut bm = BITMAP::default();
            if GetObjectW(HGDIOBJ(info.hbmColor.0), size_of::<BITMAP>() as i32, Some(&mut bm as *mut _ as *mut _)) != 0 {
                let (w, h) = (bm.bmWidth, bm.bmHeight);
                let mut bi = BITMAPINFO {
                    bmiHeader: BITMAPINFOHEADER {
                        biSize: size_of::<BITMAPINFOHEADER>() as u32,
                        biWidth: w,
                        biHeight: -h, // top-down
                        biPlanes: 1,
                        biBitCount: 32,
                        biCompression: BI_RGB.0,
                        ..Default::default()
                    },
                    ..Default::default()
                };
                let mut px = vec![0u8; (w * h * 4) as usize];
                let dc = CreateCompatibleDC(None);
                let got = GetDIBits(dc, info.hbmColor, 0, h as u32, Some(px.as_mut_ptr() as *mut _), &mut bi, DIB_RGB_COLORS);
                let _ = DeleteDC(dc);
                if got != 0 {
                    // BGRA → RGBA. Icons without an alpha channel come back all-zero alpha: make them opaque.
                    let opaque = px.chunks_exact(4).all(|p| p[3] == 0);
                    for p in px.chunks_exact_mut(4) {
                        p.swap(0, 2);
                        if opaque {
                            p[3] = 255;
                        }
                    }
                    out = encode_png(&px, w as u32, h as u32);
                }
            }
            let _ = DeleteObject(HGDIOBJ(info.hbmColor.0));
            let _ = DeleteObject(HGDIOBJ(info.hbmMask.0));
        }
        let _ = DestroyIcon(fi.hIcon);
        out
    }
}

fn encode_png(rgba: &[u8], w: u32, h: u32) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut wr = enc.write_header().ok()?;
    wr.write_image_data(rgba).ok()?;
    wr.finish().ok()?;
    Some(out)
}
