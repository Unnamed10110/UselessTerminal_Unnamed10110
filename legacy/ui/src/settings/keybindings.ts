// Keybindings UI (§15.3): searchable action list, click to record a chord, conflicts highlighted,
// per-action Reset / Unbind, built-in shell-safe keymap. The backend owns validation and persistence.

import { debounce, h } from "../dom";
import { ipc } from "../ipc";
import { eventToChord } from "../keys";
import { store } from "../store";
import { confirmDialog } from "../dialogs/modal";
import { toast } from "../dialogs/menu";
import { btn, cleanup } from "./fields";

type Keymap = Record<string, string[]>;

const ORDER = ["newTab", "closePane", "togglePanel", "toggleBrowser", "settings", "nextTab", "prevTab", "newSession", "duplicateTab", "commandPalette", "quickConnect", "movePaneFocus", "prevCommand", "nextCommand", "search", "exportBuffer", "copy", "paste", "splitRight", "splitDown", "zoomIn", "zoomOut", "zoomReset", "scrollPageUp", "scrollPageDown"];
const LABEL: Record<string, string> = {
  newTab: "New tab", closePane: "Close pane / tab", togglePanel: "Toggle sessions panel", toggleBrowser: "Toggle browser panel", settings: "Open settings",
  nextTab: "Next tab", prevTab: "Previous tab", newSession: "New saved session", duplicateTab: "Duplicate tab", commandPalette: "Command palette",
  quickConnect: "Quick SSH connect", movePaneFocus: "Move pane focus", prevCommand: "Previous command", nextCommand: "Next command", search: "Find in terminal",
  exportBuffer: "Save output…", copy: "Copy", paste: "Paste", splitRight: "Split pane right", splitDown: "Split pane down", zoomIn: "Zoom in",
  zoomOut: "Zoom out", zoomReset: "Reset zoom", scrollPageUp: "Scroll page up", scrollPageDown: "Scroll page down",
};
export const actionLabel = (a: string) => {
  const m = /^selectTab(Numpad)?(\d)$/.exec(a);
  if (m) return m[1] ? `Go to tab (numpad ${m[2]})${m[2] === "0" ? " = tab 10" : ""}` : `Go to tab ${m[2]}`;
  return LABEL[a] ?? a.replace(/([a-z])([A-Z])/g, "$1 $2").replace(/^./, (c) => c.toUpperCase());
};
const rank = (a: string) => { const i = ORDER.indexOf(a); return i < 0 ? 1000 : i; };

const PUNCT: Record<string, string> = { Comma: ",", Period: ".", Equal: "=", Minus: "-", Backquote: "`", Slash: "/", Backslash: "\\", BracketLeft: "[", BracketRight: "]", Semicolon: ";", Quote: "'", Arrow: "Arrow keys" };
const pretty = (c: string) => c.replace(/[^+]+$/, (k) => PUNCT[k] ?? k);

const ARROWS = new Set(["Up", "Down", "Left", "Right", "Arrow"]);
const split = (c: string) => { const p = c.split("+"); const key = p.pop()!; return { mods: p.sort().join("+"), key }; };
/** Same modifiers and key; `Arrow` stands for any arrow key (mirrors Chord::overlaps in ut-core). */
const overlaps = (a: string, b: string) => { const x = split(a), y = split(b); return x.mods === y.mods && (x.key === y.key || (ARROWS.has(x.key) && ARROWS.has(y.key) && (x.key === "Arrow" || y.key === "Arrow"))); };

/** action → other actions sharing one of its chords. */
export function findConflicts(map: Keymap): Map<string, Set<string>> {
  const flat = Object.entries(map).filter(([a]) => a !== "quake").flatMap(([a, cs]) => cs.map((c) => [a, c] as const));
  const out = new Map<string, Set<string>>();
  for (const [a, c] of flat) for (const [b, d] of flat) if (a !== b && overlaps(c, d)) (out.get(a) ?? out.set(a, new Set()).get(a)!).add(b);
  return out;
}

const setKeymap = (m: Keymap) => { store.keymap = m; store.events.emit("keymap", m); };
export const applyKeymap = (p: Promise<Keymap>) => p.then(setKeymap).catch((e) => toast(`Keybindings: ${e}`, "error"));

export function keybindingsSection() {
  let query = "";
  let rec: { action: string; add: boolean } | null = null;
  let stopRec = () => {};
  const list = h("div", { class: "kb-list", role: "list" });
  const summary = h("div", { class: "s-hint kb-summary" });

  const render = () => {
    const map = store.keymap;
    const conf = findConflicts(map);
    const q = query.trim().toLowerCase();
    const actions = Object.keys(map)
      .filter((a) => a !== "quake") // the global hotkey lives in Behavior → Quake
      .filter((a) => !q || a.toLowerCase().includes(q) || actionLabel(a).toLowerCase().includes(q))
      .sort((a, b) => rank(a) - rank(b) || a.localeCompare(b, undefined, { numeric: true }));
    summary.textContent = conf.size ? `⚠ ${conf.size} action(s) share a chord with another action (highlighted).` : "";
    list.replaceChildren(...actions.map((a) => {
      const chords = map[a] ?? [];
      const clash = conf.get(a);
      const recording = rec?.action === a;
      const el = h("div", { class: `kb-row${recording ? " recording" : ""}${clash ? " conflict" : ""}`, role: "listitem", tabIndex: 0, title: "Click, then press the new chord",
        on: { click: () => start(a, false), keydown: (e: KeyboardEvent) => e.target === el && e.key === "Enter" && start(a, false) } },
        h("div", { class: "kb-name" }, actionLabel(a), h("span", { class: "kb-id" }, a)),
        h("div", { class: "kb-chords" },
          recording ? h("span", { class: "kb-prompt" }, rec!.add ? "Press a chord to add…  (Esc cancels)" : "Press the new chord…  (Esc cancels)")
            : chords.length ? chords.map((c) => h("kbd", { class: "kb-chip", title: clash ? `Also bound to: ${[...clash].map(actionLabel).join(", ")}` : c }, (clash ? "⚠ " : "") + pretty(c)))
            : h("span", { class: "kb-none" }, "unbound")),
        h("div", { class: "kb-act", on: { click: (e: Event) => e.stopPropagation() } },
          btn("+", () => start(a, true), "Add another chord"),
          btn("Reset", () => void applyKeymap(ipc.keybindingsReset(a)), "Back to the default chord(s)"),
          btn("Unbind", () => void applyKeymap(ipc.keybindingsSet(a, null)), "Free the key for the shell")));
      return el;
    }));
  };

  function start(action: string, add: boolean) {
    stopRec();
    rec = { action, add };
    document.body.classList.add("recording-key"); // keeps the app's global shortcuts out of the way
    const onKey = (e: KeyboardEvent) => {
      e.preventDefault();
      e.stopImmediatePropagation();
      if (e.repeat || e.getModifierState("AltGraph")) return; // AltGr is text input, never a chord (§15.1)
      if (e.key === "Escape") return stopRec();
      const chord = eventToChord(e);
      if (!chord) return; // a lone modifier: keep waiting
      if (!(e.ctrlKey || e.altKey || /(^|\+)(F\d+|Insert|Delete|Home|End|PageUp|PageDown)$/.test(chord))) return void toast("Use Ctrl or Alt with the key: plain keys must reach the shell", "warning");
      const cur = store.keymap[action] ?? [];
      stopRec();
      void applyKeymap(ipc.keybindingsSet(action, add ? [...cur.filter((c) => c !== chord), chord] : [chord]));
    };
    window.addEventListener("keydown", onKey, true); // window capture runs before the modal's Esc handler
    stopRec = () => {
      window.removeEventListener("keydown", onKey, true);
      document.body.classList.remove("recording-key");
      rec = null;
      stopRec = () => {};
      render();
    };
    render();
  }

  const onKeymap = store.events.on("keymap", () => render());
  cleanup.add(() => (stopRec(), onKeymap()));

  const search = h("input", { class: "input", type: "search", placeholder: "Search actions or chords…", "aria-label": "Search actions" });
  search.addEventListener("input", debounce(() => ((query = search.value), render()), 100));
  const confirmed = (msg: string, ok: string, run: () => Promise<Keymap>) => confirmDialog(msg, { title: "Keybindings", ok }).then((y) => void (y && applyKeymap(run())));

  render();
  return h("div", null,
    h("div", { class: "kb-tools" }, search,
      btn("Use shell-safe keymap (Windows Terminal style)", () => void confirmed("Reset all bindings, then use Ctrl+Shift+T / Ctrl+Shift+W / Ctrl+Shift+E for new tab / close pane / sessions panel, so Ctrl+T, Ctrl+W and Ctrl+B reach tmux, readline and zsh.", "Apply", () => ipc.keybindingsKeymap("shellSafe"))),
      btn("Reset all", () => void confirmed("Reset every keybinding to its default?", "Reset all", () => ipc.keybindingsReset()))),
    summary, list,
    h("div", { class: "s-hint" }, "Changes apply immediately. The global Quake hotkey is under Behavior."));
}
