// Building blocks of the settings panel (§14.3). Every control is bound to a settings path: edits are sent as
// deltas (live preview, debounced for sliders/text), and a registered `sync` pulls the value back from the
// store after any change (own edit with backend clamping, "Reset defaults", Cancel, hot reload).

import { clamp, debounce, h, isHex, uid } from "../dom";
import { rgbOf, toHex } from "../color";
import { ipc } from "../ipc";
import { patchSettings, store } from "../store";
import { toast } from "../dialogs/menu";

export type Json = Record<string, unknown>;
const isObj = (v: unknown): v is Json => !!v && typeof v === "object" && !Array.isArray(v);

// ------------------------------------------------------------------ patch pipeline
let pending: Json | null = null;
let chain: Promise<unknown> = Promise.resolve();

function merge(t: Json, p: Json): Json {
  for (const [k, v] of Object.entries(p)) t[k] = isObj(v) && isObj(t[k]) ? merge(t[k] as Json, v) : v;
  return t;
}

function flushNow() {
  if (pending) {
    const p = pending;
    pending = null;
    chain = chain.then(() => patchSettings(p)).catch((e) => toast(`Settings: ${e}`, "error"));
  }
  return chain;
}
const flushSoon = debounce(flushNow, 150);

/** Send a delta now, or after 150 ms of quiet (sliders, typing). Deltas reach the backend strictly in order. */
export function send(patch: Json, soon = false) {
  pending = merge(pending ?? {}, patch);
  if (soon) flushSoon();
  else void flush();
}
export const flush = () => (flushSoon.cancel(), flushNow());
/** Forget unsent edits (Cancel) and wait for the ones already in flight. */
export const drop = () => (flushSoon.cancel(), (pending = null), chain);

/** Merge patch that turns `cur` into `orig` (null deletes a key). `undefined` = already equal. */
export function diff(cur: unknown, orig: unknown): unknown {
  if (isObj(cur) && isObj(orig)) {
    const out: Json = {};
    for (const k of new Set([...Object.keys(cur), ...Object.keys(orig)])) {
      if (!(k in orig)) out[k] = null;
      else if (!(k in cur)) out[k] = orig[k];
      else {
        const d = diff(cur[k], orig[k]);
        if (d !== undefined) out[k] = d;
      }
    }
    return Object.keys(out).length ? out : undefined;
  }
  return JSON.stringify(cur) === JSON.stringify(orig) ? undefined : orig;
}

// ------------------------------------------------------------------ sync / cleanup registries
const syncs = new Set<() => void>();
export const syncAll = () => syncs.forEach((f) => f());
export const cleanup = new Set<() => void>();

/** Run `f` now and after every settings/theme change; returns `el` for chaining. */
export function reg<T extends HTMLElement>(el: T, f: (el: T) => void): T {
  syncs.add(() => f(el));
  f(el);
  return el;
}

export function resetRegistries() {
  cleanup.forEach((f) => f());
  cleanup.clear();
  syncs.clear();
}

// ------------------------------------------------------------------ paths
export const get = (p: string): any => p.split(".").reduce<any>((o, k) => o?.[k], store.settings);
export const nest = (p: string, v: unknown): Json => p.split(".").reduceRight<unknown>((a, k) => ({ [k]: a }), v) as Json;
export const set = (p: string, v: unknown, soon = false) => send(nest(p, v), soon);
const focused = (el: Element) => document.activeElement === el;

// ------------------------------------------------------------------ layout
export function row(label: string, ctl: Node, hint?: string) {
  const id = uid("lbl"); // the visible label names every control in the row
  const c = h("div", { class: "s-ctl" }, ctl, hint ? h("div", { class: "s-hint" }, hint) : null);
  c.querySelectorAll("input:not([aria-labelledby]),select").forEach((e) => e.setAttribute("aria-labelledby", id));
  return h("div", { class: "s-row" }, h("div", { class: "s-label", id }, label), c);
}

export const group = (title: string, ...rows: (Node | null)[]) => h("section", { class: "s-group" }, h("h3", null, title), ...rows);

export const btn = (label: string, onClick: () => void, title?: string) =>
  h("button", { class: "btn btn-small", type: "button", title, on: { click: onClick } }, label);

// ------------------------------------------------------------------ controls
export function check(path: string, label: string, def = false) {
  const i = h("input", { type: "checkbox", on: { change: () => set(path, i.checked) } });
  reg(i, () => (i.checked = !!(get(path) ?? def)));
  return h("label", { class: "s-check" }, i, label);
}

export type Opt = [value: string, label: string];

export function select(path: string, opts: Opt[] | (() => Opt[]), o: { conv?: { to: (s: string) => unknown; from: (v: unknown) => string }; change?: (v: string) => void } = {}) {
  const s = h("select", { class: "input", on: { change: () => (o.change ? o.change(s.value) : set(path, o.conv ? o.conv.to(s.value) : s.value)) } });
  let key = "";
  return reg(s, () => {
    const cur = o.conv ? o.conv.from(get(path)) : String(get(path));
    const list = typeof opts === "function" ? opts() : [...opts];
    if (!list.some(([v]) => v === cur)) list.push([cur, cur]); // never hide the active value (e.g. preset "Custom")
    const k = JSON.stringify(list);
    if (k !== key) {
      key = k;
      s.replaceChildren(...list.map(([v, l]) => h("option", { value: v }, l)));
    }
    s.value = cur;
  });
}

export function slider(path: string, { min, max, step, k = 1, unit = "" }: { min: number; max: number; step: number; k?: number; unit?: string }) {
  const out = h("output", { class: "s-out" });
  const i = h("input", { type: "range", min, max, step, on: { input: () => ((out.textContent = i.value + unit), set(path, +i.value / k, true)) } });
  reg(i, () => {
    if (!focused(i)) i.value = String(Math.round(get(path) * k * 1e4) / 1e4);
    out.textContent = i.value + unit;
  });
  return h("div", { class: "s-slider" }, i, out);
}

export function num(path: string, min: number, max: number, step = 1) {
  const i: HTMLInputElement = h("input", { class: "input s-num", type: "number", min, max, step, on: {
    input: () => i.value !== "" && Number.isFinite(+i.value) && set(path, clamp(+i.value, min, max), true),
    change: () => (i.value = String(clamp(+i.value || min, min, max))),
  } });
  return reg(i, () => focused(i) || (i.value = String(get(path))));
}

export function text(path: string, placeholder = "") {
  const i = h("input", { class: "input", type: "text", spellcheck: false, placeholder, on: { input: () => set(path, i.value, true) } });
  return reg(i, () => focused(i) || (i.value = get(path) ?? ""));
}

/** Text field + Browse (+ Clear) buttons. `pick` returns the chosen path or null. */
export function pathField(path: string, placeholder: string, pick: () => Promise<string | null>, clearable = false) {
  const browse = btn("Browse…", () => void pick().then((p) => p != null && set(path, p)).catch((e) => toast(String(e), "error")));
  return h("div", { class: "s-inline" }, text(path, placeholder), browse, clearable ? btn("Clear", () => set(path, "")) : null);
}
export const pickImage = () => ipc.pickFile("Choose a background image", [{ name: "Images", extensions: ["png", "jpg", "jpeg", "gif", "webp", "bmp"] }]);

// ------------------------------------------------------------------ colour fields (§14.3)
/** `#rgb` / `#rrggbb` (any case) → lowercase `#rrggbb`, else null. Invalid text is never sent. */
export const normHex = (s: string) => (isHex(s) ? toHex(...rgbOf(s)) : null);

export interface ColorOpts {
  label: string;
  tag?: string;
  /** Effective colour as currently shown by the app. */
  value: () => string;
  /** Patch that applies a new colour. */
  apply: (hex: string) => Json;
  /** Patch that drops the override; the ↺ button shows only while `overridden()`. */
  reset: () => Json;
  overridden: () => boolean;
}

export function colorRow(o: ColorOpts) {
  const sw = h("input", { type: "color", class: "s-swatch", title: "Pick a colour", "aria-label": `${o.label}: colour picker` });
  const hex = h("input", { class: "input s-hex", type: "text", spellcheck: false, "aria-label": `${o.label}: hex value`, placeholder: "#rrggbb" });
  const rst = h("button", { class: "s-reset", type: "button", title: "Reset to the preset's colour", "aria-label": `${o.label}: reset`, on: { click: () => send(o.reset()) } }, "↺");
  const apply = (c: string) => send(o.apply(c), true);
  sw.addEventListener("input", () => ((hex.value = sw.value), apply(sw.value)));
  hex.addEventListener("input", () => {
    const c = normHex(hex.value);
    hex.classList.toggle("bad", !c && hex.value.trim() !== "");
    if (c) ((sw.value = c), apply(c));
  });
  hex.addEventListener("change", () => ((hex.value = normHex(hex.value) ?? normHex(o.value()) ?? "#000000"), hex.classList.remove("bad")));
  hex.addEventListener("keydown", (e) => e.key === "Enter" && hex.blur());
  const el = h("div", { class: "s-color" }, h("span", { class: "s-lbl" }, o.label, o.tag ? h("em", { class: "s-tag" }, o.tag) : null), sw, hex, rst);
  return reg(el, () => {
    const v = normHex(o.value()) ?? "#000000";
    sw.value = v;
    if (!focused(hex)) hex.value = v;
    const on = o.overridden();
    el.classList.toggle("overridden", on);
    rst.style.visibility = on ? "visible" : "hidden";
  });
}
