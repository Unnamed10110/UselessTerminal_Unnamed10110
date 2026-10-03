//! The only web engine of the app: a child WebView2 hosted through `wry` (feature `browser-panel`).
//!
//! * lazy: nothing exists until [`Web::create`] (first time the panel is shown),
//! * isolated: its own user-data folder (`WebView2Browser-rs` under `ut_fs::local_data_dir()`, never the folders
//!   the original WPF app holds), and NO bridge to the app: no IPC handler, no custom protocol, no init script,
//! * `window.open` / `target=_blank` navigate in the same view, devtools and context menus on, autofill on.
//!
//! Without the feature a stub with the same API keeps the rest of the panel compiling.

use super::url::Bounds;
use std::path::Path;

/// What the page did since the last call to [`Web::take_events`].
#[derive(Default, Debug, PartialEq)]
pub struct Events {
    /// A top-level navigation started or a page finished loading: its URL.
    pub navigated: Option<String>,
    /// `window.open` / `target=_blank`: open it in this view.
    pub new_window: Option<String>,
    /// An app shortcut (see `url::PAGE_SHORTCUTS`) was pressed while the page had the keyboard.
    pub action: Option<String>,
}

/// The shortcut table the page's key handler reads (refreshed whenever the keymap changes).
pub type Shortcuts = std::rc::Rc<std::cell::RefCell<Vec<super::url::Shortcut>>>;

/// Cheap state read from the page (polled, since SPA route changes raise no navigation event).
#[derive(Default, Debug, PartialEq, Clone)]
pub struct Page {
    pub url: Option<String>,
    pub can_back: bool,
    pub can_forward: bool,
}

#[cfg(feature = "browser-panel")]
pub use engine::Web;
#[cfg(not(feature = "browser-panel"))]
pub use stub::Web;

#[cfg(feature = "browser-panel")]
mod engine {
    use super::*;
    use raw_window_handle::{HandleError, HasWindowHandle, RawWindowHandle, Win32WindowHandle, WindowHandle};
    use std::cell::{Cell, RefCell};
    use std::num::NonZeroIsize;
    use std::rc::Rc;
    use webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2Environment;
    use wry::{dpi, NewWindowResponse, PageLoadEvent, Rect, WebContext, WebView, WebViewBuilder, WebViewBuilderExtWindows, WebViewExtWindows};

    /// The profile folder, then a fallback for when another process still holds it (error 0x8007139F).
    const PROFILES: [&str; 2] = ["WebView2Browser-rs", "WebView2Browser-rs-alt"];

    /// The main window's HWND as a parent handle (the app only keeps the raw value).
    struct Parent(isize);

    impl HasWindowHandle for Parent {
        fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
            let h = Win32WindowHandle::new(NonZeroIsize::new(self.0).ok_or(HandleError::Unavailable)?);
            // SAFETY: the HWND belongs to the eframe window, which outlives the panel (it is dropped in `shutdown`).
            Ok(unsafe { WindowHandle::borrow_raw(RawWindowHandle::Win32(h)) })
        }
    }

    fn rect(b: Bounds) -> Rect {
        Rect { position: dpi::PhysicalPosition::new(b.x, b.y).into(), size: dpi::PhysicalSize::new(b.w, b.h).into() }
    }

    /// Raise the app's own shortcuts from the page's key stream (WebView2 `AcceleratorKeyPressed`): the key router
    /// lives on the main window and never sees keys typed into the child. Handled keys do not reach the page.
    fn forward_shortcuts(view: &WebView, shortcuts: Shortcuts, events: Rc<RefCell<Events>>, ctx: egui::Context) {
        use webview2_com::AcceleratorKeyPressedEventHandler;
        use webview2_com::Microsoft::Web::WebView2::Win32::{COREWEBVIEW2_KEY_EVENT_KIND, COREWEBVIEW2_KEY_EVENT_KIND_KEY_DOWN, COREWEBVIEW2_KEY_EVENT_KIND_SYSTEM_KEY_DOWN};
        use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_CONTROL, VK_MENU, VK_RMENU, VK_SHIFT};
        let handler = AcceleratorKeyPressedEventHandler::create(Box::new(move |_, args| {
            let Some(args) = args else { return Ok(()) };
            let mut kind = COREWEBVIEW2_KEY_EVENT_KIND::default();
            let mut vk = 0u32;
            unsafe {
                args.KeyEventKind(&mut kind)?;
                args.VirtualKey(&mut vk)?;
            }
            if kind != COREWEBVIEW2_KEY_EVENT_KIND_KEY_DOWN && kind != COREWEBVIEW2_KEY_EVENT_KIND_SYSTEM_KEY_DOWN {
                return Ok(());
            }
            let down = |k: windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY| unsafe { GetAsyncKeyState(k.0 as i32) < 0 };
            let (ctrl, shift, alt) = (down(VK_CONTROL), down(VK_SHIFT), down(VK_MENU));
            let altgr = ctrl && down(VK_RMENU);
            let table = shortcuts.try_borrow();
            if let Some(action) = table.ok().and_then(|t| crate::browser::url::match_shortcut(&t,vk as u16, ctrl, shift, alt, altgr).map(str::to_string)) {
                unsafe { args.SetHandled(true)? };
                if let Ok(mut e) = events.try_borrow_mut() {
                    e.action = Some(action);
                }
                ctx.request_repaint();
            }
            Ok(())
        }));
        let mut token = 0i64;
        let r = unsafe { view.controller().add_AcceleratorKeyPressed(&handler, &mut token) };
        crate::debug::log(&format!("page key handler registered: {r:?}"));
    }

    /// §17.1: this is the user's own browser profile, so the browser's password manager is on (WebView2 ships it off).
    fn enable_password_save(view: &WebView) {
        use webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2Settings4;
        use windows::core::Interface;
        if let Ok(s) = unsafe { view.webview().Settings() } {
            if let Ok(s4) = s.cast::<ICoreWebView2Settings4>() {
                let _ = unsafe { s4.SetIsPasswordAutosaveEnabled(true) };
                let _ = unsafe { s4.SetIsGeneralAutofillEnabled(true) };
            }
        }
    }

    #[derive(Default)]
    pub struct Web {
        view: Option<WebView>,
        // Kept alive with the view (it owns the data-folder path).
        _context: Option<Box<WebContext>>,
        events: Rc<RefCell<Events>>,
        loaded: Rc<Cell<bool>>,
        placed: Option<Bounds>,
        shown: bool,
        /// The WebView2 environment (browser process group) of the first view: a replacement view reuses it, because a
        /// brand-new environment on the same profile waits for the old browser process to exit (~25 s).
        env: Option<ICoreWebView2Environment>,
    }

    impl Web {
        pub fn exists(&self) -> bool {
            self.view.is_some()
        }

        /// Has the first page finished loading (the "send selection" paste needs a page to paste into)?
        pub fn loaded(&self) -> bool {
            self.loaded.get()
        }

        /// Create the child webview at `b` showing `url`. Blocks for a moment (WebView2 environment start-up).
        pub fn create(&mut self, parent: isize, url: &str, b: Bounds, bg: [u8; 4], ctx: &egui::Context, local_data: &Path, shortcuts: &Shortcuts) -> Result<(), String> {
            let parent = Parent(parent);
            let mut last = String::from("no profile folder available");
            for name in PROFILES {
                let started = std::time::Instant::now();
                crate::debug::log(&format!("browser: creating view (profile {name}, reusing environment: {})", self.env.is_some()));
                let mut context = Box::new(WebContext::new(Some(local_data.join(name))));
                let (nav, win, load, repaint) = (self.events.clone(), self.events.clone(), self.loaded.clone(), ctx.clone());
                let (r1, r2, r3) = (repaint.clone(), repaint.clone(), repaint);
                let mut builder = WebViewBuilder::new_with_web_context(&mut context);
                if let Some(e) = self.env.clone() {
                    builder = builder.with_environment(e);
                }
                let built = builder
                    .with_url(url)
                    .with_bounds(rect(b))
                    .with_background_color((bg[0], bg[1], bg[2], bg[3]))
                    .with_devtools(true)
                    .with_hotkeys_zoom(true)
                    .with_general_autofill_enabled(true)
                    // opening the panel (or restoring it at start-up) must not take the keyboard from the terminal
                    .with_focused(false)
                    // wry's default also switches Edge SmartScreen off; this is the user's browser, keep it on.
                    .with_additional_browser_args("--disable-features=msWebOOUI,msPdfOOUI")
                    .with_navigation_handler(move |u| {
                        if let Ok(mut e) = nav.try_borrow_mut() {
                            e.navigated = Some(u);
                        }
                        r1.request_repaint();
                        true
                    })
                    .with_new_window_req_handler(move |u, _| {
                        if let Ok(mut e) = win.try_borrow_mut() {
                            e.new_window = Some(u);
                        }
                        r2.request_repaint();
                        NewWindowResponse::Deny
                    })
                    .with_on_page_load_handler(move |ev, _| {
                        if matches!(ev, PageLoadEvent::Finished) {
                            load.set(true);
                        }
                        r3.request_repaint();
                    })
                    .build_as_child(&parent);
                crate::debug::log(&format!("browser: view creation took {:.1} s -> {}", started.elapsed().as_secs_f32(), if built.is_ok() { "ok".to_string() } else { format!("{:?}", built.as_ref().err()) }));
                match built {
                    Ok(v) => {
                        forward_shortcuts(&v, shortcuts.clone(), self.events.clone(), ctx.clone());
                        enable_password_save(&v);
                        self.env = Some(v.environment());
                        self.view = Some(v);
                        self._context = Some(context);
                        self.placed = Some(b);
                        self.shown = true;
                        return Ok(());
                    }
                    Err(e) => last = e.to_string(),
                }
            }
            Err(last)
        }

        /// Show the view at `want` (moving it only when it changed) or hide it (`None`).
        pub fn place(&mut self, want: Option<Bounds>) {
            let Some(v) = &self.view else { return };
            match want {
                Some(b) => {
                    if self.placed != Some(b) {
                        let _ = v.set_bounds(rect(b));
                        self.placed = Some(b);
                    }
                    if !self.shown {
                        let _ = v.set_visible(true);
                        self.shown = true;
                    }
                }
                None => {
                    if self.shown {
                        let _ = v.set_visible(false);
                        self.shown = false;
                    }
                }
            }
        }

        pub fn shown(&self) -> bool {
            self.shown
        }

        /// A page that calls `window.close()` makes WebView2 destroy the child window (wry closes it on
        /// `WindowCloseRequested`): the view is then dead and has to be created again.
        pub fn alive(&self) -> bool {
            self.view.as_ref().is_none_or(|v| unsafe { windows::Win32::UI::WindowsAndMessaging::IsWindow(Some(v.hwnd())).as_bool() })
        }

        pub fn navigate(&self, url: &str) {
            if let Some(v) = &self.view {
                let _ = v.load_url(url);
            }
        }

        pub fn back(&self) {
            if let Some(v) = &self.view {
                let _ = v.go_back();
            }
        }

        pub fn forward(&self) {
            if let Some(v) = &self.view {
                let _ = v.go_forward();
            }
        }

        pub fn reload(&self) {
            if let Some(v) = &self.view {
                let _ = v.reload();
            }
        }

        /// Keyboard focus into the page.
        pub fn focus(&self) {
            if let Some(v) = &self.view {
                let _ = v.focus();
            }
        }

        /// Keyboard focus back to the main window (a click outside the page must not leave keys going to it).
        pub fn focus_parent(&self) {
            if let Some(v) = &self.view {
                let _ = v.focus_parent();
            }
        }

        pub fn page(&self) -> Page {
            match &self.view {
                Some(v) => Page { url: v.url().ok(), can_back: v.can_go_back().unwrap_or(false), can_forward: v.can_go_forward().unwrap_or(false) },
                None => Page::default(),
            }
        }

        pub fn take_events(&self) -> Events {
            self.events.try_borrow_mut().map(|mut e| std::mem::take(&mut *e)).unwrap_or_default()
        }

        /// The child window as Windows sees it, for the `UT_DUMP` hooks: `x,y,WxH` in the parent's client area.
        pub fn os_state(&self) -> String {
            use windows::Win32::Foundation::{POINT, RECT};
            use windows::Win32::Graphics::Gdi::ScreenToClient;
            use windows::Win32::UI::WindowsAndMessaging::{GetParent, GetWindowRect, IsWindowVisible};
            let Some(v) = &self.view else { return "none".into() };
            let h = v.hwnd();
            unsafe {
                let mut r = RECT::default();
                let _ = GetWindowRect(h, &mut r);
                let mut p = POINT { x: r.left, y: r.top };
                if let Ok(parent) = GetParent(h) {
                    let _ = ScreenToClient(parent, &mut p);
                }
                format!("{},{},{}x{} visible={} hwnd={:x}", p.x, p.y, r.right - r.left, r.bottom - r.top, IsWindowVisible(h).as_bool(), h.0 as isize)
            }
        }

        /// Drop the (dead) view but keep the browser process group, so a replacement starts at once.
        pub fn reset(&mut self) {
            self.view = None;
            self._context = None;
            self.shown = false;
            self.placed = None;
            self.loaded.set(false);
        }

        /// Destroy the child window, the controller and the browser process group (before the main window goes away).
        pub fn shutdown(&mut self) {
            self.reset();
            self.env = None;
        }
    }
}

#[cfg(not(feature = "browser-panel"))]
mod stub {
    use super::*;

    #[derive(Default)]
    pub struct Web;

    #[allow(dead_code)]
    impl Web {
        pub fn exists(&self) -> bool {
            false
        }
        pub fn loaded(&self) -> bool {
            false
        }
        pub fn create(&mut self, _: isize, _: &str, _: Bounds, _: [u8; 4], _: &egui::Context, _: &Path, _: &Shortcuts) -> Result<(), String> {
            Err("The browser panel is not available in this build".into())
        }
        pub fn place(&mut self, _: Option<Bounds>) {}
        pub fn shown(&self) -> bool {
            false
        }
        pub fn alive(&self) -> bool {
            true
        }
        pub fn navigate(&self, _: &str) {}
        pub fn back(&self) {}
        pub fn forward(&self) {}
        pub fn reload(&self) {}
        pub fn focus(&self) {}
        pub fn focus_parent(&self) {}
        pub fn page(&self) -> Page {
            Page::default()
        }
        pub fn take_events(&self) -> Events {
            Events::default()
        }
        pub fn os_state(&self) -> String {
            "off".into()
        }
        pub fn reset(&mut self) {}
        pub fn shutdown(&mut self) {}
    }
}
