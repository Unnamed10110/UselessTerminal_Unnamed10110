//! The contract between the app shell and its feature panels (sidebar, settings, palette, browser).
//!
//! Panels are plain structs with a `show`/`ui` method called every frame. They get a [`PanelCtx`] (read access to
//! stores/theme, a command queue back to the app) and never touch tabs directly: they push [`Cmd`]s which the
//! app executes after the frame's panels have been drawn. Settings are changed by patching `core.settings`
//! (`core.settings.patch(json)`); the app notices the change next frame and re-applies theme/fonts/panes.

use crate::core::Core;
use crate::fonts::Fonts;
use crate::icons::IconCache;
use crate::kit::Toasts;
use crate::theme::Theme;
use std::collections::HashSet;
use std::sync::Arc;
use ut_core::EffectiveTheme;
use ut_data::{Session, Snippet, Workspace};
use ut_shell::ShellProfile;

/// Things a panel can ask the app to do.
pub enum Cmd {
    /// Open a saved session in a new tab (`admin`: separate elevated console when the app is not elevated, §4.9).
    OpenSession { id: String, admin: bool },
    OpenProfile(ShellProfile),
    OpenCommand { command: String, title: String },
    RunSnippet(Snippet),
    OpenWorkspace(Workspace),
    /// Any action id of the keymap / palette (`newTab`, `splitRight`, `toggleBrowser`, …).
    Action(String),
    /// Live-preview a theme (palette hover, settings) without saving; `None` reverts.
    PreviewTheme(Option<EffectiveTheme>),
    /// The settings window / palette / session editor want the terminal to regain focus.
    FocusTerminal,
    /// Show the text of the focused pane's selection in the browser panel.
    SendSelectionToBrowser,
    /// Re-read shell detection (cache refresh).
    RefreshShells,
    /// `core.keys` changed (settings window): rebuild the keyboard router's shortcut table from the live keymap.
    ReloadKeymap,
}

pub struct TabInfo {
    pub id: u64,
    pub title: String,
}

pub struct PanelCtx<'a> {
    pub core: &'a Arc<Core>,
    pub theme: &'a Theme,
    pub fonts: &'a Fonts,
    /// Session ids that currently have an open tab (live dots, §8.2 — event-driven, no polling).
    pub live_sessions: &'a HashSet<String>,
    pub tabs: &'a [TabInfo],
    pub icons: &'a mut IconCache,
    pub cmds: &'a mut Vec<Cmd>,
    pub toasts: &'a mut Toasts,
    pub elevated: bool,
}

impl PanelCtx<'_> {
    pub fn open_session(&mut self, s: &Session, admin: bool) {
        self.cmds.push(Cmd::OpenSession { id: s.id.clone(), admin });
    }
}
