// Context menus and transient banners.

import { h } from "../dom";

export interface MenuItem {
  label?: string;
  action?: () => void;
  /** Right-aligned hint, e.g. a keyboard shortcut. */
  hint?: string;
  checked?: boolean;
  disabled?: boolean;
  danger?: boolean;
  separator?: boolean;
  submenu?: MenuItem[];
  /** Small colored dot (tab colors). */
  swatch?: string;
}

let current: HTMLElement | null = null;

export function closeMenu() {
  current?.remove();
  current = null;
}

/** Show a menu at viewport coordinates. Items are evaluated by the caller each time (labels never go stale). */
export function showMenu(x: number, y: number, items: MenuItem[]) {
  closeMenu();
  const root = build(items);
  root.classList.add("menu-root");
  document.body.append(root);
  current = root;
  place(root, x, y);
  const off = (e: Event) => {
    if (e instanceof KeyboardEvent && e.key !== "Escape") return;
    if (e instanceof MouseEvent && (e.target as Node) && root.contains(e.target as Node)) return;
    cleanup();
  };
  const cleanup = () => {
    closeMenu();
    window.removeEventListener("mousedown", off, true);
    window.removeEventListener("keydown", off, true);
    window.removeEventListener("blur", cleanup);
    window.removeEventListener("wheel", cleanup, true);
  };
  window.addEventListener("mousedown", off, true);
  window.addEventListener("keydown", off, true);
  window.addEventListener("blur", cleanup);
  window.addEventListener("wheel", cleanup, true);
  root.addEventListener("click", () => queueMicrotask(cleanup));
}

function place(el: HTMLElement, x: number, y: number) {
  const r = el.getBoundingClientRect();
  el.style.left = `${Math.max(0, Math.min(x, window.innerWidth - r.width - 2))}px`;
  el.style.top = `${Math.max(0, Math.min(y, window.innerHeight - r.height - 2))}px`;
}

function build(items: MenuItem[]): HTMLElement {
  const el = h("div", { class: "menu", role: "menu" });
  for (const it of items) {
    if (it.separator) {
      el.append(h("div", { class: "menu-sep" }));
      continue;
    }
    const row = h("div", {
      class: `menu-item${it.disabled ? " disabled" : ""}${it.danger ? " danger" : ""}`,
      role: "menuitem",
    },
      h("span", { class: "menu-check" }, it.checked ? "✓" : ""),
      it.swatch !== undefined ? h("span", { class: "menu-swatch", style: { background: it.swatch || "transparent", borderColor: it.swatch ? it.swatch : "var(--ui-foreground-muted)" } }) : null,
      h("span", { class: "menu-label" }, it.label ?? ""),
      h("span", { class: "menu-hint" }, it.submenu ? "▸" : it.hint ?? ""),
    );
    if (it.submenu) {
      let sub: HTMLElement | null = null;
      const open = () => {
        if (sub) return;
        sub = build(it.submenu!);
        sub.classList.add("menu-sub");
        document.body.append(sub);
        const r = row.getBoundingClientRect();
        place(sub, r.right - 2, r.top - 4);
        sub.addEventListener("mouseleave", close);
        sub.addEventListener("click", () => queueMicrotask(closeMenu));
      };
      const close = () => {
        sub?.remove();
        sub = null;
      };
      row.addEventListener("mouseenter", open);
      row.addEventListener("mouseleave", (e) => {
        if (!(e.relatedTarget as HTMLElement | null)?.closest?.(".menu-sub")) close();
      });
      row.addEventListener("click", (e) => {
        e.stopPropagation();
        open();
      });
    } else if (!it.disabled) {
      row.addEventListener("click", () => it.action?.());
    }
    el.append(row);
  }
  return el;
}

// ---------------------------------------------------------------- banners
export function showBanner(text: string, kind: "info" | "warning" | "error" = "info", actions: { label: string; run: () => void }[] = []) {
  const host = document.getElementById("banner-root")!;
  const el = h("div", { class: `banner banner-${kind}` },
    h("span", { class: "banner-text" }, text),
    ...actions.map((a) => h("button", { class: "btn btn-small", on: { click: () => { a.run(); el.remove(); } } }, a.label)),
    h("button", { class: "banner-close", title: "Dismiss", on: { click: () => el.remove() } }, "✕"));
  host.append(el);
  return el;
}

export function toast(text: string, kind: "info" | "success" | "warning" | "error" = "info", ms = 3200) {
  const host = document.getElementById("toast-root")!;
  const el = h("div", { class: `toast toast-${kind}` }, text);
  host.append(el);
  setTimeout(() => el.remove(), ms);
}
