// Browser panel (§17): ordinary DOM chrome (nav bar + quick links) around a placeholder whose on-screen
// rectangle is what the native child webview occupies. That webview has NO IPC (§18.1) and is created lazily
// by the backend on the first `browser_set_bounds`, so bounds are never pushed while the panel is closed (§24 #34).

import "./browser.css";
import { app } from "../app";
import { frameOrTimeout, h } from "../dom";
import { ipc, isTauri, listen } from "../ipc";
import { icon } from "../layout/tabstrip";
import { toast } from "../dialogs/menu";

const LINKS: [string, string][] = [
  ["ChatGPT", "https://chatgpt.com"],
  ["DeepSeek", "https://chat.deepseek.com"],
  ["Claude", "https://claude.ai"],
  ["Gemini", "https://gemini.google.com"],
  ["Copilot", "https://copilot.microsoft.com"],
  ["Perplexity", "https://www.perplexity.ai"],
  ["Grok", "https://grok.com"],
];

/** Address bar rules (§17.2); `null` = ignore the input. */
export function normalizeAddress(raw: string): string | null {
  const t = raw.trim();
  if (!t) return null;
  if (t.includes("://")) return t;
  if (t.includes(".") && !/\s/.test(t)) return `https://${t}`;
  return `https://www.google.com/search?q=${encodeURIComponent(t)}`;
}

const NAV = {
  back: '<path d="M13 8H3M7 4L3 8l4 4"/>',
  forward: '<path d="M3 8h10M9 4l4 4-4 4"/>',
  reload: '<path d="M13 8a5 5 0 11-1.6-3.7M13 2.5v3h-3"/>',
};

export class BrowserPanel {
  readonly el: HTMLElement;
  private view = h("div", { class: "br-view" });
  private addr = h("input", { class: "br-addr", type: "text", placeholder: "Search or enter address", "aria-label": "Address" });
  private open = false;
  private dirty = false; // the user is editing: backend navigations must not overwrite the text
  private shown = ""; // last URL known to the backend
  private last = "";
  private inflight: Promise<void> = Promise.resolve();

  constructor() {
    const nav = (ic: keyof typeof NAV, title: string, act: "back" | "forward" | "reload") =>
      h("button", { class: "br-btn", title, "aria-label": title, on: { click: () => void ipc.browserNav(act).catch(() => {}) } }, icon(NAV[ic], title));
    this.el = h("div", { class: "browser-inner" },
      h("div", { class: "br-nav" }, nav("back", "Back", "back"), nav("forward", "Forward", "forward"), nav("reload", "Refresh", "reload"), this.addr),
      h("div", { class: "br-links" }, LINKS.map(([name, url]) => h("button", { class: "br-link", title: url, on: { click: () => this.go(url) } }, name))),
      this.view,
    );
    if (!isTauri) this.view.append(h("div", { class: "br-note" }, "Browser view is available in the desktop app"));

    this.addr.spellcheck = false;
    this.addr.addEventListener("input", () => (this.dirty = true));
    this.addr.addEventListener("focus", () => this.addr.select());
    this.addr.addEventListener("blur", () => { this.dirty = false; this.addr.value = this.shown; });
    this.addr.addEventListener("keydown", (e) => {
      if (e.key === "Enter") {
        const url = normalizeAddress(this.addr.value);
        if (url) this.go(url);
        this.addr.blur();
      } else if (e.key === "Escape") this.addr.blur();
    });

    void listen("browser:navigated", ({ url }) => {
      this.shown = url;
      if (!this.dirty) this.addr.value = url;
    });
    new ResizeObserver(() => this.syncBounds()).observe(this.view);
    window.addEventListener("resize", () => this.syncBounds());
  }

  private go(url: string) {
    this.shown = url;
    this.dirty = false;
    this.addr.value = url;
    void ipc.browserNavigate(url).catch((e) => toast(String(e), "error"));
  }

  setOpen(open: boolean) {
    this.open = open;
    if (open) this.syncBounds();
  }

  /** Push the placeholder's rectangle (logical px) to the native webview; rAF-throttled, skipped while closed. */
  syncBounds() {
    if (this.open) frameOrTimeout(() => void this.push());
  }

  /** Immediate push; resolves once the backend has applied (and, the first time, created) the webview. */
  push(): Promise<void> {
    const r = this.view.getBoundingClientRect();
    if (!this.open || r.width < 1 || r.height < 1) return this.inflight; // closed or collapsed: never create the webview (§24 #34)
    const b = [r.x, r.y, r.width, r.height].map(Math.round) as [number, number, number, number];
    const key = b.join();
    if (key === this.last) return this.inflight;
    this.last = key;
    return (this.inflight = ipc.browserSetBounds(...b).catch(() => { this.last = ""; }));
  }
}

/** [P1] §17.2: terminal selection → clipboard → focused page input (clipboard only, no script injection). */
export async function sendSelectionToBrowser() {
  const text = app.active?.focused?.term.getSelection();
  if (!text) return void toast("Select some text in the terminal first.", "warning");
  if (!app.browserOpen) app.toggleBrowser(true);
  await app.browser.push(); // the webview must exist before it can receive the paste
  try {
    await ipc.browserPaste(text);
  } catch (e) {
    toast(String(e), "error");
  }
}
