// Keybinding matching (§15.1). Chords arrive from the backend as `Ctrl+Shift+Alt+<Key>` strings.

import { store } from "./store";

interface Chord {
  ctrl: boolean;
  shift: boolean;
  alt: boolean;
  key: string; // canonical key name from the spec (e.g. "T", "Comma", "Arrow", "Numpad1")
}

const CODE: Record<string, string[]> = {
  Tab: ["Tab"], Enter: ["Enter", "NumpadEnter"], Esc: ["Escape"], Space: ["Space"], Backspace: ["Backspace"],
  Delete: ["Delete"], Insert: ["Insert"], Home: ["Home"], End: ["End"], PageUp: ["PageUp"], PageDown: ["PageDown"],
  Up: ["ArrowUp"], Down: ["ArrowDown"], Left: ["ArrowLeft"], Right: ["ArrowRight"],
  Arrow: ["ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight"],
  Comma: ["Comma"], Period: ["Period"], Minus: ["Minus", "NumpadSubtract"], Equal: ["Equal", "NumpadAdd"],
  Backquote: ["Backquote"], Slash: ["Slash"], Backslash: ["Backslash"], BracketLeft: ["BracketLeft"],
  BracketRight: ["BracketRight"], Semicolon: ["Semicolon"], Quote: ["Quote"],
};

// Characters that also satisfy a punctuation chord on layouts where the physical key moves (es-ES: `+` ≠ Equal).
const CHARS: Record<string, string> = {
  Comma: ",", Period: ".", Minus: "-", Equal: "=+", Backquote: "`", Slash: "/", Backslash: "\\",
  BracketLeft: "[", BracketRight: "]", Semicolon: ";", Quote: "'",
};

export function parseChord(s: string): Chord | null {
  const parts = s.split("+").map((p) => p.trim()).filter(Boolean);
  if (!parts.length) return null;
  const c: Chord = { ctrl: false, shift: false, alt: false, key: "" };
  for (const p of parts.slice(0, -1)) {
    const l = p.toLowerCase();
    if (l === "ctrl" || l === "control") c.ctrl = true;
    else if (l === "shift") c.shift = true;
    else if (l === "alt") c.alt = true;
    else return null; // `Win` chords are global hotkeys only
  }
  c.key = parts[parts.length - 1];
  return c;
}

function keyMatches(c: Chord, e: KeyboardEvent): boolean {
  const k = c.key;
  if (/^[A-Za-z]$/.test(k)) return e.key.toLowerCase() === k.toLowerCase();
  if (/^[0-9]$/.test(k)) return e.code === `Digit${k}`;
  if (/^Numpad[0-9]$/i.test(k)) return e.code === `Numpad${k.slice(6)}`;
  if (/^F([1-9]|1[0-9]|2[0-4])$/.test(k)) return e.code === k;
  const codes = CODE[k];
  if (!codes) return false;
  if (codes.includes(e.code)) return true;
  const ch = CHARS[k];
  return !!ch && ch.includes(e.key) && e.key.length === 1;
}

export function chordMatches(c: Chord, e: KeyboardEvent): boolean {
  if (e.getModifierState("AltGraph")) return false; // AltGr is text input, never a shortcut (§5.5)
  if (e.ctrlKey !== c.ctrl || e.shiftKey !== c.shift || e.altKey !== c.alt) return false;
  if (e.metaKey) return false;
  return keyMatches(c, e);
}

let compiled: [string, Chord][] = [];
export function compileKeymap(map: Record<string, string[]> = store.keymap) {
  compiled = [];
  for (const [action, chords] of Object.entries(map)) {
    for (const s of chords) {
      const c = parseChord(s);
      if (c) compiled.push([action, c]);
    }
  }
}

/** First matching action for a keydown, or null. Never matches during IME composition. */
export function matchAction(e: KeyboardEvent): string | null {
  if (e.isComposing || e.keyCode === 229) return null;
  for (const [action, c] of compiled) if (chordMatches(c, e)) return action;
  return null;
}

/** Display form of the first chord bound to an action (command palette, tooltips). */
export function shortcutLabel(action: string): string {
  const s = store.keymap[action]?.[0];
  return s ? s.replace("Comma", ",").replace("Equal", "=").replace("Minus", "-").replace("Backquote", "`").replace("Arrow", "Arrow keys") : "";
}

/** Chord string for a keydown while recording a binding in the settings UI. */
export function eventToChord(e: KeyboardEvent): string | null {
  if (["Control", "Shift", "Alt", "Meta", "AltGraph"].includes(e.key)) return null;
  const parts: string[] = [];
  if (e.ctrlKey) parts.push("Ctrl");
  if (e.shiftKey) parts.push("Shift");
  if (e.altKey) parts.push("Alt");
  let key: string | undefined;
  if (/^Key[A-Z]$/.test(e.code)) key = e.code.slice(3);
  else if (/^Digit[0-9]$/.test(e.code)) key = e.code.slice(5);
  else if (/^Numpad[0-9]$/.test(e.code) || /^F\d+$/.test(e.code)) key = e.code;
  else {
    key = Object.entries(CODE).find(([n, codes]) => n !== "Arrow" && codes[0] === e.code)?.[0] ??
      (e.code.startsWith("Arrow") ? e.code.slice(5) : undefined);
  }
  if (!key) return null;
  parts.push(key);
  return parts.join("+");
}
