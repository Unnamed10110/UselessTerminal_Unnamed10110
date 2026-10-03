// Command palette (§7.10): a modal overlay inside the main webview. Fuzzy matching, recents first,
// shortcut labels read from the LIVE keymap, theme live-preview on selection.

import { app } from "../app";
import { h } from "../dom";
import { ipc } from "../ipc";
import { shortcutLabel } from "../keys";
import { endPreview, previewTheme, store } from "../store";
import { openSettings } from "../settings/panel";
import { openQuickConnect } from "./quick-connect";
import { openDiagnostics } from "./diagnostics";
import { modal, promptDialog, confirmDialog } from "./modal";
import { toast } from "./menu";

interface Entry {
  id: string;
  label: string;
  action?: string; // keybinding action id → shortcut label
  run: () => void | Promise<void>;
  preview?: () => void;
}

const BASE: [string, string, string?][] = [
  ["newTab", "New Tab", "newTab"],
  ["closePane", "Close Tab / Pane", "closePane"],
  ["togglePanel", "Toggle Sessions Panel", "togglePanel"],
  ["toggleBrowser", "Toggle Browser Panel", "toggleBrowser"],
  ["settings", "Open Settings", "settings"],
  ["duplicateTab", "Duplicate Tab", "duplicateTab"],
  ["newSession", "New Saved Session", "newSession"],
  ["addPane", "Add Pane (Split)"],
  ["splitRight", "Split Pane Right", "splitRight"],
  ["splitDown", "Split Pane Down", "splitDown"],
  ["unsplitAll", "Unsplit All Panes"],
  ["renameTab", "Rename Tab"],
  ["pinTab", "Pin / Unpin Tab"],
  ["broadcastToggle", "Toggle Broadcast Input"],
  ["nextTab", "Next Tab", "nextTab"],
  ["prevTab", "Previous Tab", "prevTab"],
  ["closeOthers", "Close Other Tabs"],
  ["closeRight", "Close Tabs to the Right"],
  ["quake", "Toggle Window (Quake Mode)", "quake"],
  ["quickConnect", "Quick SSH Connect", "quickConnect"],
  ["toggleLog", "Toggle Session Logging"],
  ["toggleReadOnly", "Toggle Read-Only Mode"],
  ["toggleRecording", "Toggle Recording (asciicast)"],
  ["toggleCrt", "Toggle Retro CRT Mode"],
  ["findAllTabs", "Find in All Tabs"],
  ["toggleMinimap", "Toggle Minimap Scrollbar"],
  ["saveWorkspace", "Save Current Tabs as Workspace"],
];

/** Subsequence score: bonus for word starts and consecutive runs, penalty for gaps. `null` = no match. */
export function fuzzy(query: string, text: string): number | null {
  if (!query) return 0;
  const q = query.toLowerCase();
  const t = text.toLowerCase();
  let score = 0;
  let ti = 0;
  let prev = -2;
  for (const ch of q) {
    const i = t.indexOf(ch, ti);
    if (i < 0) return null;
    score += 10;
    if (i === prev + 1) score += 15; // consecutive
    if (i === 0 || /[\s\-_/:]/.test(t[i - 1])) score += 12; // word start
    score -= Math.min(8, i - ti); // gap
    prev = i;
    ti = i + 1;
  }
  return score - Math.floor(t.length / 8);
}

const RECENT_KEY = "ut.palette.recent";
const recent = (): string[] => { try { return JSON.parse(localStorage.getItem(RECENT_KEY) ?? "[]"); } catch { return []; } };
const remember = (id: string) => { try { localStorage.setItem(RECENT_KEY, JSON.stringify([id, ...recent().filter((x) => x !== id)].slice(0, 12))); } catch { /* no storage */ } };

function entries(): Entry[] {
  const out: Entry[] = BASE.map(([id, label, action]) => ({ id, label, action: action ?? id, run: () => void app.runAction(id) }));
  for (const w of store.workspaces) out.push({ id: `ws:${w.id}`, label: `Open Workspace: ${w.name}`, run: () => void app.openWorkspace(w) });
  out.push({ id: "manageWorkspaces", label: "Manage Workspaces…", run: () => void manageWorkspaces() });
  for (const s of store.sessions.sessions) out.push({ id: `session:${s.id}`, label: `Open Session: ${s.name}`, run: () => void app.openSession(s) });
  for (const s of store.sessions.snippets) out.push({ id: `snippet:${s.id}`, label: `Run Snippet: ${s.name}`, run: () => app.runSnippet(s) });
  app.tabs.forEach((t, i) => out.push({ id: `tab:${t.id}`, label: `Go to Tab: ${i + 1} ${t.title}`, run: () => app.activate(t) }));
  for (const name of store.presets) {
    out.push({
      id: `theme:${name}`, label: `Theme: ${name}`,
      preview: () => void ipc.presetTheme(name).then((r) => r.theme && previewTheme(r.theme, r.cssVars)),
      run: async () => { await import("../store").then((m) => m.patchSettings({ theme: { preset: name, overrides: {} } })); },
    });
  }
  out.push({ id: "diagnostics", label: "Developer: Diagnostics", run: () => openDiagnostics() });
  out.push({ id: "settingsFile", label: "Open settings.json", run: () => void ipc.settingsOpenFile() });
  return out;
}

async function manageWorkspaces() {
  const ws = store.workspaces;
  if (!ws.length) return void toast("No workspaces saved yet.", "info");
  const name = await promptDialog({ title: "Manage workspaces", label: `Workspace to rename or delete (${ws.map((w) => w.name).join(", ")})`, value: ws[0].name });
  const w = ws.find((x) => x.name === name);
  if (!w) return;
  const newName = await promptDialog({ title: `Rename "${w.name}"`, label: "New name (empty = delete)", value: w.name, allowEmpty: true });
  if (newName === undefined) return;
  if (newName === "") {
    if (await confirmDialog(`Delete workspace "${w.name}"?`, { danger: true, ok: "Delete" })) await ipc.workspaceDelete(w.id);
  } else await ipc.workspaceRename(w.id, newName);
}

let opened = false;

export function openPalette() {
  if (opened) return;
  opened = true;
  const all = entries();
  const rec = recent();
  let shown: Entry[] = [];
  let sel = 0;
  void modal<Entry | undefined>({
    className: "palette",
    width: 480,
    build: ({ close }) => {
      const input = h("input", { class: "input palette-input", type: "text", placeholder: "Type a command…", spellcheck: false });
      const list = h("div", { class: "palette-list", role: "listbox" });
      const render = () => {
        const q = input.value.trim();
        shown = all
          .map((e) => ({ e, s: fuzzy(q, e.label) }))
          .filter((x): x is { e: Entry; s: number } => x.s !== null)
          .sort((a, b) => {
            const ra = rec.indexOf(a.e.id), rb = rec.indexOf(b.e.id);
            const rbonus = (r: number) => (r < 0 ? 0 : 40 - r * 3);
            return b.s + rbonus(rb) - (a.s + rbonus(ra)) || a.e.label.localeCompare(b.e.label);
          })
          .map((x) => x.e)
          .slice(0, 60);
        sel = Math.min(sel, Math.max(0, shown.length - 1));
        list.replaceChildren(
          ...shown.map((e, i) =>
            h("div", { class: `palette-item${i === sel ? " sel" : ""}`, role: "option", on: { click: () => close(e), mousemove: () => { if (sel !== i) { sel = i; mark(); } } } },
              h("span", { class: "pi-label" }, e.label),
              h("span", { class: "pi-hint" }, e.action ? shortcutLabel(e.action) : "")),
          ),
        );
        mark();
      };
      const mark = () => {
        [...list.children].forEach((c, i) => c.classList.toggle("sel", i === sel));
        list.children[sel]?.scrollIntoView({ block: "nearest" });
        const e = shown[sel];
        if (e?.preview) e.preview(); else endPreview();
      };
      input.addEventListener("input", () => { sel = 0; render(); });
      input.addEventListener("keydown", (ev) => {
        if (ev.key === "ArrowDown" || ev.key === "ArrowUp") {
          ev.preventDefault();
          if (shown.length) { sel = (sel + (ev.key === "ArrowDown" ? 1 : -1) + shown.length) % shown.length; mark(); }
        } else if (ev.key === "Enter") { ev.preventDefault(); if (shown[sel]) close(shown[sel]); }
      });
      window.addEventListener("blur", () => close(undefined), { once: true }); // closes when the window loses focus
      render();
      return { body: h("div", null, input, list), focus: input };
    },
  }).then(async (e) => {
    opened = false;
    endPreview();
    if (!e) return;
    remember(e.id);
    await e.run();
  });
}

export { openSettings, openQuickConnect };
