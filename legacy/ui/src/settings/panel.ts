// Settings panel (§14.3): a modal page inside the main webview (so the browser webview hides, §17.1).
// Edits are patched live (the backend persists each patch, debounced); Cancel patches the original values back,
// Save keeps them. Sections live in sections.ts / theme.ts / keybindings.ts.

import "./settings.css";
import { clamp, h } from "../dom";
import { ipc } from "../ipc";
import { applyBundle, patchSettings, store } from "../store";
import { modal } from "../dialogs/modal";
import { toast } from "../dialogs/menu";
import { btn, cleanup, diff, drop, flush, reg, resetRegistries, send, syncAll } from "./fields";
import { applyKeymap, keybindingsSection } from "./keybindings";
import { aboutSection, backgroundSection, behaviorSection, fontSection, interfaceSection, sshSection } from "./sections";
import { themeSection } from "./theme";

const SECTIONS: [string, () => HTMLElement][] = [
  ["Terminal font", fontSection],
  ["Interface", interfaceSection],
  ["Shell background", backgroundSection],
  ["Prompt & output", themeSection],
  ["Behavior", behaviorSection],
  ["Keybindings", keybindingsSection],
  ["SSH & drops", sshSection],
  ["About", aboutSection],
];
/** Every top-level group of settings.json: `null` resets a group to its defaults (RFC 7386 in the backend). */
const GROUPS = ["terminal", "theme", "ui", "shells", "startup", "processes", "notifications", "ssh", "drop", "links", "logging", "recording", "quake"];

let isOpen = false;
let lastSection = 0;

export function openSettings(): void {
  if (isOpen) return;
  isOpen = true;
  void modal<boolean>({ width: 960, className: "settings-modal", dismissible: false, build: ({ close, dialog }) => build(close, dialog) })
    .finally(() => ((isOpen = false), resetRegistries()));
}

function build(close: (saved: boolean) => void, dialog: HTMLElement) {
  let orig = structuredClone(store.settings); // what Cancel returns to
  const origKeys = structuredClone(store.keymap);
  let busy = false;

  const finish = async (save: boolean) => {
    if (busy) return;
    busy = true;
    try {
      if (save) await flush();
      else {
        await drop();
        const back = diff(store.settings, orig);
        if (back) await patchSettings(back);
        for (const [a, c] of Object.entries(origKeys)) if (JSON.stringify(store.keymap[a]) !== JSON.stringify(c)) await applyKeymap(ipc.keybindingsSet(a, c));
      }
    } catch (e) {
      toast(String(e), "error");
    }
    close(save);
  };

  // ---- body: nav + sections (built on first visit, then kept so their state survives switching)
  const sections = h("div", { class: "s-sections" });
  const built: (HTMLElement | undefined)[] = [];
  const nav = h("nav", { class: "s-nav", "aria-label": "Settings sections" });
  const navBtns = SECTIONS.map(([name], i) => h("button", { class: "s-nav-item", type: "button", on: { click: () => show(i) } }, name));
  nav.append(...navBtns);
  nav.addEventListener("keydown", (e) => {
    const d = e.key === "ArrowDown" ? 1 : e.key === "ArrowUp" ? -1 : 0;
    if (!d) return;
    e.preventDefault();
    const i = clamp(lastSection + d, 0, SECTIONS.length - 1);
    show(i);
    navBtns[i].focus();
  });
  function show(i: number) {
    lastSection = i;
    built[i] ??= sections.appendChild(h("div", { class: "s-sec", role: "region", "aria-label": SECTIONS[i][0] }, SECTIONS[i][1]()));
    built.forEach((el, j) => el && (el.hidden = j !== i));
    navBtns.forEach((b, j) => (j === i ? b.setAttribute("aria-current", "page") : b.removeAttribute("aria-current")));
    content.scrollTop = 0;
  }

  // settings.json unreadable (§14.1): the backend keeps defaults in memory and saves nothing, so editing is pointless until reset
  const notice = reg(h("div", { class: "s-notice", role: "alert" }), (n) => {
    n.hidden = !store.loadError;
    sections.classList.toggle("s-locked", !!store.loadError);
    n.replaceChildren(
      h("span", { class: "grow" }, `settings.json could not be read (${store.loadError}). Defaults are in use and nothing is saved until you reset.`),
      btn("Open file", () => void ipc.settingsOpenFile()),
      btn("Reset (backup created)", () => void ipc.settingsResetWithBackup().then((b) => {
        applyBundle(b);
        orig = structuredClone(store.settings);
        toast("settings.json was backed up and reset", "success");
      }).catch((e) => toast(String(e), "error"))));
  });
  const content = h("div", { class: "s-content" }, notice, sections);

  const resetDefaults = () => {
    send(Object.fromEntries(GROUPS.map((g) => [g, null])));
    toast("Defaults loaded. Save to keep them, Cancel to go back.", "info");
  };

  // ---- Esc = Cancel (a nested confirm dialog consumes its own Esc first)
  const onEsc = (e: KeyboardEvent) => {
    const all = document.querySelectorAll("#modal-root > .modal-backdrop");
    if (e.key === "Escape" && !e.defaultPrevented && all[all.length - 1] === dialog.parentElement) {
      e.preventDefault();
      void finish(false);
    }
  };
  document.addEventListener("keydown", onEsc);
  cleanup.add(() => document.removeEventListener("keydown", onEsc));
  cleanup.add(store.events.on("settings", syncAll));
  cleanup.add(store.events.on("theme", syncAll));

  show(lastSection);
  return {
    focus: navBtns[lastSection],
    body: h("div", { class: "s-root" },
      h("div", { class: "s-head" },
        h("div", { class: "s-title" }, "Settings"),
        h("button", { class: "s-x", type: "button", title: "Cancel and close (Esc)", "aria-label": "Close settings and discard changes", on: { click: () => void finish(false) } }, "✕")),
      h("div", { class: "s-body" }, nav, content),
      h("div", { class: "s-foot" },
        btn("Reset defaults", resetDefaults, "Load the default values into the preview; nothing is kept until you Save"),
        h("span", { class: "s-hint grow" }, "Changes preview live. Cancel reverts them."),
        h("button", { class: "btn", type: "button", on: { click: () => void finish(false) } }, "Cancel"),
        h("button", { class: "btn btn-primary", type: "button", on: { click: () => void finish(true) } }, "Save"))),
  };
}
