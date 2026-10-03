// App-wide state that is loaded from the backend and pushed to every consumer.

import { ipc, listen, type AppInfo } from "./ipc";
import { Emitter, clamp, debounce } from "./dom";
import type { EffectiveTheme, SessionsSnapshot, Settings, SettingsBundle, ShellProfile, Workspace } from "./types";

export interface StoreEvents extends Record<string, unknown> {
  settings: { settings: Settings; prev: Settings };
  theme: EffectiveTheme;
  sessions: SessionsSnapshot;
  shells: ShellProfile[];
  workspaces: Workspace[];
  keymap: Record<string, string[]>;
}

export const store = {
  settings: undefined as unknown as Settings,
  theme: { terminal: {}, ui: {} } as EffectiveTheme,
  presets: [] as string[],
  keymap: {} as Record<string, string[]>,
  info: undefined as unknown as AppInfo,
  shells: [] as ShellProfile[],
  sessions: { folders: [], sessions: [], snippets: [], version: 0 } as SessionsSnapshot,
  workspaces: [] as Workspace[],
  loadError: null as string | null,
  /** URL of the configured background image (served by the utasset:// protocol), or null. */
  bgUrl: null as string | null,
  cssVars: [] as [string, string][],
  events: new Emitter<StoreEvents>(),
};

/** Merge a settings bundle (from `settings_get`, `settings_patch` or `settings:changed`) into the store. */
export function applyBundle(b: SettingsBundle) {
  const prev = store.settings;
  store.settings = b.settings;
  if (b.presets) store.presets = b.presets;
  if (b.loadError !== undefined) store.loadError = b.loadError ?? null;
  if (b.cssVars) {
    store.cssVars = b.cssVars;
    setCssVars(b.cssVars);
  }
  setUiScale(b.settings.ui);
  if (b.theme) {
    store.theme = b.theme;
    store.events.emit("theme", b.theme);
  }
  store.events.emit("settings", { settings: b.settings, prev });
}

function setCssVars(vars: [string, string][]) {
  const root = document.documentElement.style;
  for (const [k, v] of vars) root.setProperty(k, v);
}

/** UI scale is a root-level multiplier (§13.5) — never a raster transform. Font tokens are derived here. */
function setUiScale(ui: Settings["ui"]) {
  const root = document.documentElement.style;
  const s = clamp(ui.scale || 1, 0.75, 2);
  root.setProperty("--ui-scale", String(s));
  root.setProperty("--ui-font-family", `"${ui.fontFamily}", "Segoe UI", system-ui, sans-serif`);
  root.setProperty("--ui-font-size", `${ui.fontSize * s}px`);
  root.setProperty("--ui-font-size-small", `${(ui.fontSize - 2) * s}px`);
  root.setProperty("--ui-font-size-title", `${(ui.fontSize + 2) * s}px`);
  root.setProperty("--ui-font-size-tab", `${(ui.fontSize - 3) * s}px`);
  root.setProperty("--ui-font-weight", String(ui.fontWeight));
}

export async function loadStore() {
  const [bundle, keymap, info, shells, sessions, workspaces] = await Promise.all([
    ipc.settingsGet(),
    ipc.keybindingsGet(),
    ipc.appInfo(),
    ipc.shellsDetect(false),
    ipc.sessionsSnapshot(),
    ipc.workspacesList(),
  ]);
  store.info = info;
  store.keymap = keymap;
  store.shells = shells;
  store.sessions = sessions;
  store.workspaces = workspaces;
  applyBundle(bundle);

  await Promise.all([
    listen("settings:changed", (b) => applyBundle(b)),
    listen("sessions:changed", (s) => ((store.sessions = s), store.events.emit("sessions", s))),
    listen("workspaces:changed", (w) => ((store.workspaces = w), store.events.emit("workspaces", w))),
    listen("keybindings:changed", (k) => ((store.keymap = k), store.events.emit("keymap", k))),
  ]);
}

/** Patch settings. The backend validates/clamps and returns the effective bundle. */
export async function patchSettings(patch: unknown) {
  applyBundle(await ipc.settingsPatch(patch));
}

export async function refreshShells(force = false) {
  store.shells = await ipc.shellsDetect(force);
  store.events.emit("shells", store.shells);
  return store.shells;
}

export async function refreshSessions() {
  store.sessions = await ipc.sessionsSnapshot();
  store.events.emit("sessions", store.sessions);
}

/** Debounced persistence for the global zoom: panes update instantly, the backend gets one delta. */
export const persistFontSize = debounce((fontSize: number) => {
  void patchSettings({ terminal: { fontSize } });
}, 500);

export function defaultProfile(): ShellProfile | undefined {
  const want = store.settings.shells.defaultProfile;
  return (want !== "auto" && store.shells.find((s) => s.id === want)) || store.shells.find((s) => s.isDefault) || store.shells[0];
}

// ------------------------------------------------------------ theme preview
let saved: { theme: EffectiveTheme; vars: [string, string][] } | null = null;

/** Live-preview a theme (palette hover, settings) without saving it. */
export function previewTheme(theme: EffectiveTheme, vars: [string, string][]) {
  saved ??= { theme: store.theme, vars: store.cssVars };
  store.theme = theme;
  setCssVars(vars);
  setUiScale(store.settings.ui);
  store.events.emit("theme", theme);
}

export function endPreview() {
  if (!saved) return;
  const s = saved;
  saved = null;
  store.theme = s.theme;
  setCssVars(s.vars);
  setUiScale(store.settings.ui);
  store.events.emit("theme", s.theme);
}
