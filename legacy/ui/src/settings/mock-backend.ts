// Browser-mock only (used by ui/src/mock.ts): real behaviour for the settings / theme / keybinding commands, so the
// settings panel can be exercised without Tauri. A small port of crates/ut-core (theme.rs, keybindings.rs, RFC 7386 patch).

import { luma, rgbOf, toHex } from "../color";
import type { EffectiveTheme, Settings, SettingsBundle } from "../types";

type Json = Record<string, any>;
const isObj = (v: unknown): v is Json => !!v && typeof v === "object" && !Array.isArray(v);

// ------------------------------------------------------------------ theme (subset of Appendix B + §13.3 factories)
const mix = (a: string, b: string, t: number) => {
  const x = rgbOf(a), y = rgbOf(b);
  return toHex(x[0] + (y[0] - x[0]) * t, x[1] + (y[1] - x[1]) * t, x[2] + (y[2] - x[2]) * t);
};
// bg fg muted err warn cmd msg acc hi cursor selBg selFg
const D: Record<string, [string[], boolean?]> = {
  Default: [["#000000", "#ffffff", "#888888", "#ff2b7b", "#ffef5c", "#b4fb00", "#56ffef", "#6be5ff", "#c47cff", "#ffffff", "#ffffff", "#000000"]],
  Dracula: [["#282a36", "#f8f8f2", "#6272a4", "#ff5555", "#f1fa8c", "#50fa7b", "#8be9fd", "#bd93f9", "#ff79c6", "#f8f8f2", "#44475a", "#f8f8f2"]],
  Nord: [["#2e3440", "#d8dee9", "#4c566a", "#bf616a", "#ebcb8b", "#a3be8c", "#88c0d0", "#81a1c1", "#b48ead", "#d8dee9", "#434c5e", "#eceff4"]],
  "Solarized Dark": [["#002b36", "#839496", "#586e75", "#dc322f", "#b58900", "#859900", "#2aa198", "#268bd2", "#d33682", "#839496", "#073642", "#93a1a1"]],
  Monokai: [["#272822", "#f8f8f2", "#75715e", "#f92672", "#e6db74", "#a6e22e", "#66d9ef", "#ae81ff", "#fd971f", "#f8f8f0", "#49483e", "#f8f8f2"]],
  "AMOLED Green": [["#000000", "#f2f2f2", "#6b6b6b", "#ff1744", "#c6ff00", "#39ff14", "#18ffff", "#00e676", "#e040fb", "#f2f2f2", "#39ff14", "#000000"], true],
  Light: [["#f7f7f8", "#1a1a1a", "#6b7280", "#c62828", "#2e7d32", "#1565c0", "#00838f", "#00838f", "#6a1b9a", "#1565c0", "#1565c0", "#ffffff"]],
};
export const PRESET_NAMES = Object.keys(D);

export function effectiveTheme(ref: { preset: string; overrides: Json }): EffectiveTheme {
  const [c, neon] = D[ref.preset] ?? D.Default;
  const [bg, fg, muted, err, warn, cmd, msg, acc, hi, cursor, selBg, selFg] = c;
  const light = ref.preset === "Light";
  const o = ref.overrides ?? {}, ansi: Json = o.ansi ?? {};
  const s = (k: string, d: string): string => o[k] ?? d;
  const B = s("background", bg), F = s("foreground", fg), M = s("muted", muted);
  const base = ["#000000", s("error", err), s("command", cmd), s("warning", warn), s("accent", acc), s("highlight", hi), s("message", msg), F];
  const names = ["black", "red", "green", "yellow", "blue", "magenta", "cyan", "white"];
  const terminal: Record<string, string> = {};
  names.forEach((n, i) => {
    const b = ansi[n] ?? base[i];
    terminal[n] = b;
    const bn = "bright" + n[0].toUpperCase() + n.slice(1);
    terminal[bn] = ansi[bn] ?? (i === 0 ? M : i === 7 && !light ? mix(b, "#ffffff", 0.25) : mix(b, light ? "#000000" : "#ffffff", 0.15));
  });
  Object.assign(terminal, { background: B, foreground: F, cursor: s("cursor", cursor), cursorAccent: s("cursorAccent", B), selectionBackground: s("selectionBackground", selBg), selectionForeground: s("selectionForeground", selFg) });

  const t = (a: number, b: number) => (neon ? b : a);
  const ui: Record<string, string> = light
    ? { foreground: "#111827", foregroundMuted: "#6b7280", tabFg: "#111827", inputFg: "#111827", inputBg: "#ffffff", cardBg: "#ffffff", chromeBg: mix("#f8fafc", acc, 0.1), cardBorder: mix("#cbd5e1", acc, 0.4), folderSelected: mix("#ffffff", acc, 0.16), statusBg: mix("#eef2f7", acc, 0.14), hoverBg: mix("#ffffff", acc, 0.12), splitter: mix("#94a3b8", acc, 0.45) }
    : { foreground: fg, foregroundMuted: muted, tabFg: fg, inputFg: fg, chromeBg: bg, inputBg: mix(bg, fg, 0.08), cardBg: mix(bg, acc, t(0.12, 0.1)), cardBorder: mix(bg, acc, t(0.28, 0.42)), folderSelected: mix(bg, acc, t(0.2, 0.24)), statusBg: mix(bg, acc, t(0.14, 0.18)), hoverBg: mix(bg, acc, t(0.16, 0.22)), splitter: mix(bg, acc, t(0.4, 0.55)) };
  Object.assign(ui, { accent: acc, highlight: hi, success: cmd, warning: warn, error: err, tabSelectedBg: acc, icon: acc, tabSelectedFg: luma(acc) > 0.55 ? "#111111" : "#ffffff" }, o.ui ?? {});
  const [r, g, b] = rgbOf(ui.accent);
  ui.accentDim ??= `rgba(${r},${g},${b},0.14)`;
  ui.terminalBg ??= B;
  return { terminal, ui };
}

const kebab = (k: string) => k.replace(/[A-Z]/g, (c) => "-" + c.toLowerCase());
const cssVars = (t: EffectiveTheme): [string, string][] => Object.entries(t.ui).map(([k, v]) => [`--ui-${kebab(k)}`, v]);

// ------------------------------------------------------------------ settings state (RFC 7386 merge patch)
function merge(t: Json, p: Json) {
  for (const [k, v] of Object.entries(p)) {
    if (v === null) delete t[k];
    else if (isObj(v) && isObj(t[k])) merge(t[k], v);
    else t[k] = v;
  }
}
/** Missing keys fall back to the defaults, like `#[serde(default)]` on every struct. */
function fill(d: Json, t: Json) {
  for (const [k, v] of Object.entries(d)) {
    if (!(k in t)) t[k] = structuredClone(v);
    else if (isObj(v) && isObj(t[k])) fill(v, t[k]);
  }
}

// ------------------------------------------------------------------ keybindings (§15.2 defaults)
const DEFAULT_KEYS: Record<string, string[]> = {
  newTab: ["Ctrl+T"], closePane: ["Ctrl+W"], togglePanel: ["Ctrl+B"], toggleBrowser: ["Ctrl+Shift+B"], settings: ["Ctrl+Comma"], nextTab: ["Ctrl+Tab"], prevTab: ["Ctrl+Shift+Tab"],
  newSession: ["Ctrl+Shift+N"], duplicateTab: ["Ctrl+Shift+D"], commandPalette: ["Ctrl+Shift+P"], quickConnect: ["Ctrl+Shift+O"], movePaneFocus: ["Ctrl+Shift+Arrow"],
  prevCommand: ["Ctrl+Alt+Up"], nextCommand: ["Ctrl+Alt+Down"], search: ["Ctrl+Shift+F"], exportBuffer: ["Ctrl+Shift+S"], copy: ["Ctrl+Shift+C", "Ctrl+Insert"],
  paste: ["Ctrl+V", "Ctrl+Shift+V", "Shift+Insert"], splitRight: ["Shift+Alt+Equal"], splitDown: ["Shift+Alt+Minus"], zoomIn: ["Ctrl+Equal"], zoomOut: ["Ctrl+Minus"],
  zoomReset: ["Ctrl+0"], scrollPageUp: ["Shift+PageUp"], scrollPageDown: ["Shift+PageDown"], quake: ["Win+Backquote"],
  ...Object.fromEntries([1, 2, 3, 4, 5, 6, 7, 8, 9].map((n) => [`selectTab${n}`, [`Ctrl+${n}`]])),
  ...Object.fromEntries([0, 1, 2, 3, 4, 5, 6, 7, 8, 9].map((n) => [`selectTabNumpad${n}`, [`Ctrl+Alt+Numpad${n}`]])),
};
const SHELL_SAFE: Record<string, string[]> = { newTab: ["Ctrl+Shift+T"], closePane: ["Ctrl+Shift+W"], togglePanel: ["Ctrl+Shift+E"] };
let userKeys: Record<string, string[]> = {}; // overrides; [] = unbound
const effectiveKeys = () => ({ ...DEFAULT_KEYS, ...userKeys });

// ------------------------------------------------------------------ command dispatcher
export interface MockCtx {
  defaults: Settings;
  settings: Settings;
  emit: (ev: string, payload: unknown) => void;
}

/** Returns the command's result, or `undefined` when the command is not a settings one. */
export function settingsMock(cmd: string, a: Json, ctx: MockCtx): unknown {
  const bundle = (full: boolean): SettingsBundle => {
    const out: SettingsBundle = { settings: structuredClone(ctx.settings), loadError: null };
    if (full) {
      const theme = effectiveTheme(ctx.settings.theme as never);
      Object.assign(out, { theme, cssVars: cssVars(theme), presets: PRESET_NAMES });
    }
    return out;
  };
  const keysChanged = () => { const e = effectiveKeys(); ctx.emit("keybindings:changed", e); return e; };

  switch (cmd) {
    case "settings_get": return bundle(true);
    case "settings_patch": {
      const p = a.patch as Json;
      merge(ctx.settings as never, p);
      fill(ctx.defaults as never, ctx.settings as never);
      const ov = ctx.settings.theme.overrides as Json; // the backend drops emptied ansi/ui maps
      for (const k of ["ansi", "ui"]) if (isObj(ov[k]) && !Object.keys(ov[k]).length) delete ov[k];
      const b = bundle("theme" in p || "ui" in p);
      queueMicrotask(() => ctx.emit("settings:changed", { ...b, patch: p }));
      return b;
    }
    case "settings_reset_with_backup": ctx.settings = structuredClone(ctx.defaults); return bundle(true);
    case "theme_preview": {
      const t = effectiveTheme({ preset: a.preset as string, overrides: (a.overrides as Json) ?? {} });
      return { theme: t, cssVars: cssVars(t) };
    }
    case "fonts_monospace": return ["Cascadia Code", "Cascadia Mono", "Consolas", "Courier New", "Fira Code", "JetBrains Mono", "Lucida Console", "IBM Plex Mono"];
    case "keybindings_get": return effectiveKeys();
    case "keybindings_set": {
      const action = a.action as string, chords = (a.chords as string[] | null) ?? [];
      if (!(action in DEFAULT_KEYS)) throw `unknown action '${action}'`;
      if (chords.some((c) => c.startsWith("Win+")) && action !== "quake") throw `'${chords[0]}': Win chords are only valid for global hotkeys`;
      if (JSON.stringify(chords) === JSON.stringify(DEFAULT_KEYS[action])) delete userKeys[action];
      else userKeys[action] = chords;
      return keysChanged();
    }
    case "keybindings_reset": {
      if (a.action) delete userKeys[a.action as string];
      else userKeys = {};
      return keysChanged();
    }
    case "keybindings_keymap": userKeys = a.name === "shellSafe" ? structuredClone(SHELL_SAFE) : {}; return keysChanged();
    case "key_label": return a.code === "Backquote" ? "Ñ" : a.code; // es-ES: VK_OEM_3 is Ñ (§7.8)
    case "quake_set_hotkey": merge(ctx.settings as never, { quake: { hotkey: a.hotkey } }); return a.hotkey;
    default: return undefined;
  }
}
