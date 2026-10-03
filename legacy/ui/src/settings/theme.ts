// "Prompt & output" (§14.3 #4): preset dropdown, 13-row colour grid, 16 ANSI slots, live sample pane.
// Persisted model (§13.1): `theme = { preset, overrides }`; typed-input colour is a personal setting, never part of it.

import { h } from "../dom";
import { store } from "../store";
import { btn, check, colorRow, get, group, nest, reg, row, select, send, type Json } from "./fields";

const DEFAULT_TYPED = "#ffffff";
type Overrides = Record<string, unknown> & { ansi?: Record<string, string>; ui?: Record<string, string> };
export const overrides = (): Overrides => (store.settings.theme.overrides ?? {}) as Overrides;
export const countOverrides = () => { const o = overrides(); return Object.entries(o).reduce((n, [k, v]) => n + (k === "ansi" || k === "ui" ? Object.keys(v as object).length : 1), 0); };
/** Patch that deletes every current override (RFC 7386: `{}` would merge, not clear). */
export const clearAll = (): Json => Object.fromEntries(Object.keys(overrides()).map((k) => [k, null]));

/** [label, override key, effective terminal colour, ANSI slot that would mask the override] */
const ROWS: [string, string, string, string?][] = [
  ["Background", "background", "background"],
  ["Default text", "foreground", "foreground", "white"],
  ["Muted text", "muted", "brightBlack", "brightBlack"],
  ["Errors", "error", "red", "red"],
  ["Warnings", "warning", "yellow", "yellow"],
  ["Commands & success", "command", "green", "green"],
  ["Info", "message", "cyan", "cyan"],
  ["Paths & links", "accent", "blue", "blue"],
  ["Highlights", "highlight", "magenta", "magenta"],
  ["Cursor", "cursor", "cursor"],
  ["Selection background", "selectionBackground", "selectionBackground"],
  ["Selection foreground", "selectionForeground", "selectionForeground"],
];
const ANSI = ["black", "red", "green", "yellow", "blue", "magenta", "cyan", "white"];

const term = (k: string) => store.theme.terminal[k] ?? "#000000";
const ovr = (k: string) => overrides()[k] != null;
const setTheme = (o: Json): Json => ({ theme: { overrides: o } });

function semanticRow([label, key, eff, slot]: (typeof ROWS)[number]) {
  return colorRow({
    label,
    value: () => term(eff),
    // An explicit choice beats a leftover ANSI-slot override that would hide it (§13.2).
    apply: (hex) => setTheme({ [key]: hex, ...(slot && overrides().ansi?.[slot] ? { ansi: { [slot]: null } } : {}) }),
    reset: () => setTheme({ [key]: null }),
    overridden: () => ovr(key),
  });
}

const typedRow = () =>
  colorRow({
    label: "Typed input",
    tag: "personal",
    value: () => get("terminal.typedInputColor"),
    apply: (hex) => nest("terminal.typedInputColor", hex),
    reset: () => nest("terminal.typedInputColor", DEFAULT_TYPED),
    overridden: () => get("terminal.typedInputColor") !== DEFAULT_TYPED,
  });

const ansiRow = (name: string, bright: boolean) => {
  const slot = bright ? "bright" + name[0].toUpperCase() + name.slice(1) : name;
  return colorRow({
    label: (bright ? "bright " : "") + name,
    value: () => term(slot),
    apply: (hex) => setTheme({ ansi: { [slot]: hex } }),
    reset: () => setTheme({ ansi: { [slot]: null } }),
    overridden: () => !!overrides().ansi?.[slot],
  });
};

// ------------------------------------------------------------------ live sample (§14.3 [P1])
/** [text, colour key | "$input", flag]: b = bold, sel = selection, cur = cursor block. */
type Span = [string, string?, ("b" | "sel" | "cur")?];
const SAMPLE: Span[][] = [
  [["PS ", "green"], ["C:\\src\\useless", "blue"], ["> ", "foreground"], ["git status", "$input"], [" ", "foreground", "cur"]],
  [["On branch ", "foreground"], ["main", "magenta"], ["  (up to date)", "brightBlack"]],
  [["Changes to be committed:", "green"]],
  [["        modified:   src/main.rs", "green"]],
  [["Changes not staged for commit:", "red"]],
  [["        modified:   Cargo.toml", "red"]],
  [["PS C:\\src\\useless> ", "green"], ["ls", "$input"]],
  [["src/", "blue", "b"], ["  target/", "blue", "b"], ["  run.sh", "green", "b"], ["  build.zip", "red", "b"], ["  logo.png", "magenta"]],
  [["link -> src", "cyan"], ["  README.md", "foreground"], ["  12 files, 4 dirs", "brightBlack"]],
  [["error[E0425]: cannot find value `x`", "red", "b"]],
  [["warning: unused variable: `y`", "yellow", "b"]],
  [["info: 3 packages updated", "cyan"], ["  ", "foreground"], ["selected text", "foreground", "sel"]],
];

function sample() {
  const pre = h("pre", { class: "s-sample", "aria-label": "Colour sample", role: "img" });
  return reg(pre, () => {
    const t = store.settings.terminal;
    const c = (k?: string) => (k === "$input" ? t.typedInputColor : term(k ?? "foreground"));
    Object.assign(pre.style, { background: term("background"), color: term("foreground"), fontFamily: t.fontFamily, fontSize: `${Math.min(t.fontSize, 16)}px`, fontWeight: String(t.fontWeight), lineHeight: String(t.lineHeight) });
    pre.replaceChildren(...SAMPLE.flatMap((line) => [...line.map(([text, k, f]) => {
      const st: Partial<CSSStyleDeclaration> = { color: c(k) };
      if (f === "b") st.fontWeight = "bold";
      if (f === "sel") Object.assign(st, { background: term("selectionBackground"), color: term("selectionForeground") });
      if (f === "cur") Object.assign(st, { background: term("cursor"), color: term("cursorAccent") });
      return h("span", { style: st }, text);
    }), "\n"]));
  });
}

// ------------------------------------------------------------------ section
export function themeSection() {
  const presetSel = select("theme.preset", () => store.presets.map((p) => [p, p] as [string, string]), {
    // Selecting a preset drops all overrides and never touches terminal.typedInputColor (§13.2, §23.18).
    change: (preset) => send({ theme: { preset, overrides: clearAll() } }),
  });
  const resetAll = reg(btn("Reset colours to preset", () => send(setTheme(clearAll()))), (b) => (b.hidden = !countOverrides()));
  const counter = reg(h("span", { class: "s-hint" }), (s) => (s.textContent = countOverrides() ? `${countOverrides()} colour override(s) on top of the preset` : "No overrides: this is the preset as designed"));

  const advanced = h("details", { class: "s-adv" },
    h("summary", null, "Advanced: all 16 ANSI colours"),
    h("div", { class: "s-colors" }, ANSI.map((n) => ansiRow(n, false)), ANSI.map((n) => ansiRow(n, true))));

  return h("div", null,
    group("Theme",
      row("Preset", h("div", { class: "s-inline" }, presetSel, resetAll), "Shows the preset in use. Your colour edits are stored as overrides on top of it."),
      row("", counter)),
    h("div", { class: "s-theme" },
      h("div", null,
        group("Colours",
          h("div", { class: "s-colors" }, ROWS.slice(0, 2).map(semanticRow), typedRow(), ROWS.slice(2).map(semanticRow))),
        advanced,
        check("terminal.overridePsReadLineColors", "Apply the typed-input colour to PowerShell (PSReadLine) tokens, in new sessions")),
      h("div", { class: "s-sticky" }, h("h3", { class: "s-h3" }, "Sample"), sample())));
}
