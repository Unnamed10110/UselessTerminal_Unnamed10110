//! Shell icons (§9.3): the resolved executable's icon via `SHGetFileInfoW`, cached as egui textures.

use egui::{ColorImage, Context, TextureHandle, TextureOptions};
use std::collections::HashMap;
use std::path::Path;

#[derive(Default)]
pub struct IconCache {
    map: HashMap<String, Option<TextureHandle>>,
}

impl IconCache {
    /// Texture for a command line / path, or `None` (fallback glyph). Extraction is cached per executable.
    pub fn get(&mut self, ctx: &Context, command: &str) -> Option<&TextureHandle> {
        let key = ut_shell::file_stem_lower(command);
        if !self.map.contains_key(&key) {
            let tex = ut_shell::resolve_exe(command)
                .or_else(|| Some(Path::new(command.trim().trim_matches('"')).to_path_buf()).filter(|p| p.is_file()))
                .and_then(|exe| extract(&exe))
                .map(|(rgba, w, h)| ctx.load_texture(format!("icon-{key}"), ColorImage::from_rgba_unmultiplied([w, h], &rgba), TextureOptions::LINEAR));
            self.map.insert(key.clone(), tex);
        }
        self.map.get(&key).and_then(|t| t.as_ref())
    }
}

/// Large shell icon of `exe` as RGBA.
fn extract(exe: &Path) -> Option<(Vec<u8>, usize, usize)> {
    use windows::core::HSTRING;
    use windows::Win32::Graphics::Gdi::{CreateCompatibleDC, DeleteDC, DeleteObject, GetDIBits, GetObjectW, BITMAP, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HGDIOBJ};
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
                    bmiHeader: BITMAPINFOHEADER { biSize: size_of::<BITMAPINFOHEADER>() as u32, biWidth: w, biHeight: -h, biPlanes: 1, biBitCount: 32, biCompression: BI_RGB.0, ..Default::default() },
                    ..Default::default()
                };
                let mut px = vec![0u8; (w * h * 4) as usize];
                let dc = CreateCompatibleDC(None);
                let got = GetDIBits(dc, info.hbmColor, 0, h as u32, Some(px.as_mut_ptr() as *mut _), &mut bi, DIB_RGB_COLORS);
                let _ = DeleteDC(dc);
                if got != 0 {
                    // BGRA → RGBA; icons without an alpha channel come back all-zero alpha: make them opaque.
                    let opaque = px.chunks_exact(4).all(|p| p[3] == 0);
                    for p in px.chunks_exact_mut(4) {
                        p.swap(0, 2);
                        if opaque {
                            p[3] = 255;
                        }
                    }
                    out = Some((px, w as usize, h as usize));
                }
            }
            let _ = DeleteObject(HGDIOBJ(info.hbmColor.0));
            let _ = DeleteObject(HGDIOBJ(info.hbmMask.0));
        }
        let _ = DestroyIcon(fi.hIcon);
        out
    }
}
