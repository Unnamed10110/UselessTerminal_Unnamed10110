// Chrome bar + tab strip (§7.3). Headers are rebuilt on structural changes and patched in place for
// title/activity updates; drag-reorder uses pointer events (HTML5 DnD is unavailable once file-drop is on).

import { h, exeStem } from "../dom";
import { ipc } from "../ipc";
import { shortcutLabel } from "../keys";
import type { Tab } from "./tab";

export const TAB_COLORS = ["#00ff44", "#ff003c", "#ffff00", "#00e5ff", "#ff00ff", "#ff8800", "#ffffff", "#888888"];

export interface StripHooks {
  select(id: string): void;
  close(id: string): void;
  reorder(id: string, toIndex: number): void;
  newTab(): void;
  shellMenu(anchor: HTMLElement): void;
  toggleSessions(): void;
  toggleBrowser(): void;
  settings(): void;
  contextMenu(tab: Tab, e: MouseEvent): void;
  uiZoom(dir: 1 | -1): void;
}

const iconCache = new Map<string, Promise<string>>();
const iconFor = (cmd: string) => {
  const stem = exeStem(cmd);
  if (!iconCache.has(stem)) iconCache.set(stem, ipc.iconFor(cmd).catch(() => ""));
  return iconCache.get(stem)!;
};

const icon = (d: string, label: string) =>
  h("span", { class: "svg-icon", "aria-label": label, innerHTML: `<svg viewBox="0 0 16 16" width="16" height="16" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round">${d}</svg>` });

export const ICONS = {
  sessions: '<rect x="2" y="3" width="12" height="10" rx="1.5"/><path d="M6 3v10"/>',
  browser: '<circle cx="8" cy="8" r="6"/><path d="M2 8h12M8 2c2 2 2 10 0 12M8 2c-2 2-2 10 0 12"/>',
  settings: '<circle cx="8" cy="8" r="2.2"/><path d="M8 1.5v2M8 12.5v2M1.5 8h2M12.5 8h2M3.4 3.4l1.4 1.4M11.2 11.2l1.4 1.4M12.6 3.4l-1.4 1.4M4.8 11.2l-1.4 1.4"/>',
  plus: '<path d="M8 3v10M3 8h10"/>',
  chevron: '<path d="M4 6l4 4 4-4"/>',
  close: '<path d="M4 4l8 8M12 4l-8 8"/>',
  pin: '<path d="M9.5 2.5l4 4-2 .5-2.5 2.5.5 3-1 1-3-3-3.5 3.5M6.5 6l-1 1 3 3"/>',
  lock: '<rect x="3.5" y="7" width="9" height="6.5" rx="1.2"/><path d="M5.5 7V5a2.5 2.5 0 015 0v2"/>',
  console: '<rect x="1.5" y="2.5" width="13" height="11" rx="1.5"/><path d="M4.5 6l2.5 2-2.5 2M8.5 10.5h3"/>',
  branch: '<circle cx="4.5" cy="3.5" r="1.4"/><circle cx="4.5" cy="12.5" r="1.4"/><circle cx="11.5" cy="6.5" r="1.4"/><path d="M4.5 5v6M11.5 8c0 2.5-7 1.5-7 3"/>',
  folder: '<path d="M1.5 4.5a1 1 0 011-1h3.2l1.6 1.8h6.2a1 1 0 011 1v5.7a1 1 0 01-1 1h-11a1 1 0 01-1-1z"/>',
  more: '<circle cx="3" cy="8" r="1"/><circle cx="8" cy="8" r="1"/><circle cx="13" cy="8" r="1"/>',
};
export { icon };

export class TabStrip {
  readonly el: HTMLElement;
  private scroller: HTMLElement;
  private headers = new Map<string, HTMLElement>();
  private tabs: Tab[] = [];
  private activeId = "";

  constructor(private hooks: StripHooks) {
    const btn = (ic: keyof typeof ICONS, title: string, act: string | null, fn: (e: MouseEvent) => void) =>
      h("button", { class: "chrome-btn", title: act ? `${title} (${shortcutLabel(act)})` : title, on: { click: fn } }, icon(ICONS[ic], title));
    this.scroller = h("div", { class: "tab-scroller", role: "tablist" });
    this.scroller.addEventListener("wheel", (e) => {
      if (e.ctrlKey) return;
      this.scroller.scrollLeft += e.deltaY || e.deltaX;
      e.preventDefault();
    }, { passive: false });
    this.el = h("div", { class: "chrome-bar" },
      btn("sessions", "Toggle Sessions", "togglePanel", () => hooks.toggleSessions()),
      this.scroller,
      h("div", { class: "chrome-right" },
        btn("browser", "Toggle Browser", "toggleBrowser", () => hooks.toggleBrowser()),
        btn("settings", "Settings", "settings", () => hooks.settings()),
        btn("plus", "New Tab", "newTab", () => hooks.newTab()),
        btn("chevron", "Shells", null, (e) => hooks.shellMenu(e.currentTarget as HTMLElement)),
      ),
    );
    // Ctrl+wheel over the chrome scales the UI (not over a terminal).
    this.el.addEventListener("wheel", (e) => {
      if (!e.ctrlKey) return;
      e.preventDefault();
      hooks.uiZoom(e.deltaY < 0 ? 1 : -1);
    }, { passive: false });
  }

  render(tabs: Tab[], activeId: string) {
    this.tabs = tabs;
    this.activeId = activeId;
    const seen = new Set<string>();
    tabs.forEach((t, i) => {
      seen.add(t.id);
      let el = this.headers.get(t.id);
      if (!el) {
        el = this.makeHeader(t);
        this.headers.set(t.id, el);
      }
      this.fill(el, t, i);
      if (this.scroller.children[i] !== el) this.scroller.insertBefore(el, this.scroller.children[i] ?? null);
    });
    for (const [id, el] of this.headers) if (!seen.has(id)) (el.remove(), this.headers.delete(id));
    this.scrollIntoView();
  }

  update(t: Tab) {
    const el = this.headers.get(t.id);
    if (el) this.fill(el, t, this.tabs.indexOf(t));
  }

  private scrollIntoView() {
    const el = this.headers.get(this.activeId);
    if (!el) return;
    const s = this.scroller;
    const pad = 8;
    if (el.offsetLeft - pad < s.scrollLeft) s.scrollLeft = el.offsetLeft - pad;
    else if (el.offsetLeft + el.offsetWidth + pad > s.scrollLeft + s.clientWidth) s.scrollLeft = el.offsetLeft + el.offsetWidth + pad - s.clientWidth;
  }

  private makeHeader(t: Tab): HTMLElement {
    const el = h("div", { class: "tab", role: "tab", "data-tab": t.id });
    el.addEventListener("mousedown", (e) => {
      if (e.button === 1) { e.preventDefault(); this.hooks.close(t.id); return; }
      if (e.button !== 0 || (e.target as HTMLElement).closest(".tab-close")) return;
      this.hooks.select(t.id);
      this.startDrag(e, t, el);
    });
    el.addEventListener("contextmenu", (e) => { e.preventDefault(); this.hooks.select(t.id); this.hooks.contextMenu(t, e); });
    return el;
  }

  private fill(el: HTMLElement, t: Tab, index: number) {
    const active = t.id === this.activeId;
    el.classList.toggle("selected", active);
    el.classList.toggle("pinned", t.pinned);
    el.setAttribute("aria-selected", String(active));
    const color = t.color;
    el.style.setProperty("--tab-color", color ?? "transparent");
    el.style.borderColor = color ? (active ? color : color + "80") : "";
    const img = h("img", { class: "tab-icon", width: 16, height: 16, alt: "" });
    iconFor(t.command).then((u) => (u ? (img.src = u) : img.replaceWith(icon(ICONS.console, "shell"))));
    const tip = [t.title, t.group && `Group: ${t.group}`].filter(Boolean).join("\n");
    el.title = tip;
    const kids: (Node | null)[] = [
      color ? h("span", { class: "tab-dot", style: { background: color } }) : null,
      t.pinned ? h("span", { class: "tab-pin" }, icon(ICONS.pin, "pinned")) : null,
      img,
      t.group ? h("span", { class: "tab-group" }, t.group) : null,
      t.pinned ? null : h("span", { class: "tab-title" }, `${index + 1}  ${t.title}`),
      t.readOnly ? h("span", { class: "tab-ro", title: "Read-only" }, icon(ICONS.lock, "read-only")) : null,
      t.broadcast ? h("span", { class: "tab-bcast", title: "Broadcast input" }, "⇉") : null,
      t.logging ? h("span", { class: "tab-log", title: "Logging" }) : null,
      t.recording ? h("span", { class: "tab-rec", title: "Recording" }, "●") : null,
      t.activity && !active ? h("span", { class: "tab-activity", title: "New output" }) : null,
      h("button", { class: "tab-close", title: "Close", on: { click: (e: Event) => { e.stopPropagation(); this.hooks.close(t.id); } } }, icon(ICONS.close, "close")),
    ];
    el.replaceChildren(...(kids.filter(Boolean) as Node[]));
  }

  // ------------------------------------------------------------ drag reorder
  private startDrag(e: MouseEvent, t: Tab, el: HTMLElement) {
    const startX = e.clientX;
    let dragging = false;
    let dropIndex = this.tabs.indexOf(t);
    const move = (ev: MouseEvent) => {
      if (!dragging && Math.abs(ev.clientX - startX) < 6) return;
      dragging = true;
      el.classList.add("dragging");
      el.style.transform = `translateX(${ev.clientX - startX}px)`;
      // Drop index = first tab whose midpoint lies to the right of the pointer.
      dropIndex = this.tabs.length - 1;
      for (let i = 0; i < this.tabs.length; i++) {
        const o = this.headers.get(this.tabs[i].id)!;
        const r = o.getBoundingClientRect();
        if (o !== el && r.left + r.width / 2 > ev.clientX) { dropIndex = i > this.tabs.indexOf(t) ? i - 1 : i; break; }
      }
    };
    const up = () => {
      window.removeEventListener("mousemove", move);
      window.removeEventListener("mouseup", up);
      el.classList.remove("dragging");
      el.style.transform = "";
      if (dragging && dropIndex !== this.tabs.indexOf(t)) this.hooks.reorder(t.id, dropIndex);
    };
    window.addEventListener("mousemove", move);
    window.addEventListener("mouseup", up);
  }
}
