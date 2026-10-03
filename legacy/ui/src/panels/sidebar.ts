// Sessions sidebar (§8.2): header, search, tree, selection, keyboard, pointer drag-and-drop, context menus, snippets.
// Cards carry no closures: one set of delegated listeners on the tree reads `data-*` attributes, so a re-render
// is just `replaceChildren` (rebuilt only on sessions:changed, expand/collapse and the debounced query — §24 #41).

import "./sidebar.css";
import { app, sessionCommand } from "../app";
import { debounce, h } from "../dom";
import { ipc, type DropTarget } from "../ipc";
import { shortcutLabel } from "../keys";
import { store } from "../store";
import { ICONS, icon } from "../layout/tabstrip";
import { choiceDialog, confirmDialog, promptDialog } from "../dialogs/modal";
import { showBanner, showMenu, toast, type MenuItem } from "../dialogs/menu";
import { openSessionEditor } from "../dialogs/session-editor";
import { IC, lsGet, lsSet } from "./sb-util";
import { Snippets } from "./snippets";
import type { Folder, Session } from "../types";

const EXPANDED_KEY = "ut.sidebar.expanded";

const iconCache = new Map<string, Promise<string>>();
const iconUrl = (cmd: string) => {
  if (!iconCache.has(cmd)) iconCache.set(cmd, ipc.iconFor(cmd).catch(() => ""));
  return iconCache.get(cmd)!;
};

/** Split `text` around case-insensitive occurrences of the (already lower-cased) query and mark them. */
function hl(text: string, q: string): (Node | string)[] {
  if (!q) return [text];
  const out: (Node | string)[] = [];
  const lo = text.toLowerCase();
  let i = 0;
  for (let j = lo.indexOf(q); j >= 0; j = lo.indexOf(q, i)) {
    if (j > i) out.push(text.slice(i, j));
    out.push(h("mark", { class: "sb-hl" }, text.slice(j, j + q.length)));
    i = j + q.length;
  }
  out.push(text.slice(i));
  return out;
}

const matches = (s: Session, q: string) => [s.name, s.description, `${s.shellPath} ${s.arguments}`].some((t) => t.toLowerCase().includes(q));

interface Drop {
  target: DropTarget;
  half: "top" | "bottom";
  /** Element that gets the indicator class. */
  el: HTMLElement;
  mode: "before" | "after" | "into" | "root";
}

const actBtn = (a: string, title: string, path: string) =>
  h("button", { class: "sb-act", "data-act": a, tabindex: -1, title, "aria-label": title }, icon(path, title));

export class Sidebar {
  readonly el: HTMLElement;
  private search = h("input", { class: "sb-search", type: "text", placeholder: "Search sessions", spellcheck: false, "aria-label": "Search sessions" });
  private clearBtn = h("button", { class: "sb-clear", tabindex: -1, title: "Clear search", "aria-label": "Clear search", on: { click: () => this.setQuery("") } }, icon(ICONS.close, "clear"));
  private tree = h("div", { class: "sb-tree", tabindex: 0, role: "tree", "aria-label": "Sessions" });
  private snippets = new Snippets();

  /** Lower-cased trimmed query; empty = tree mode. */
  private q = "";
  private sel = new Set<string>();
  private anchor: string | null = null;
  private cursor: string | null = null;
  /** Visible item ids in DOM order (keyboard, ranges). */
  private rows: string[] = [];
  private sessionMap = new Map<string, Session>();
  private folderMap = new Map<string, Folder>();
  private expanded: Record<string, boolean> = lsGet(EXPANDED_KEY, {});
  private lastBanner: string | null = store.sessions.banner ?? null;
  private suppressClick = false;
  private reveal: string | null = null;

  constructor() {
    const newBtn = h("button", { class: "sb-new", title: `New Session (${shortcutLabel("newSession") || "Ctrl+Shift+N"})`, on: { click: () => this.newSession() } }, icon(ICONS.plus, "add"), "New");
    const moreBtn = h("button", { class: "icon-btn sb-more", title: "More", "aria-label": "More actions", on: { click: (e: MouseEvent) => this.headerMenu(e.currentTarget as HTMLElement) } }, icon(ICONS.more, "more"));
    this.el = h("div", { class: "sidebar-inner sb" },
      h("div", { class: "sidebar-header" }, h("span", { class: "sidebar-title" }, "Sessions"), h("div", { class: "sb-head-right" }, newBtn, moreBtn)),
      h("div", { class: "sb-searchbox" }, this.search, this.clearBtn),
      this.tree,
      this.snippets.el);

    const apply = debounce(() => this.setQuery(this.search.value, false), 100); // §24 #41
    this.search.addEventListener("input", () => { this.clearBtn.hidden = !this.search.value; apply(); });
    this.search.addEventListener("keydown", (e) => {
      if (e.key === "Escape" && this.search.value) (e.preventDefault(), this.setQuery(""));
      else if (e.key === "ArrowDown") {
        e.preventDefault();
        apply.flush();
        this.tree.focus();
        if (this.rows[0]) this.pick(this.rows[0], {});
      }
    });
    this.clearBtn.hidden = true;

    const t = this.tree;
    t.addEventListener("mousedown", (e) => { if ((e.target as HTMLElement).closest("button")) e.preventDefault(); }); // buttons never steal focus
    t.addEventListener("click", (e) => this.onClick(e));
    t.addEventListener("dblclick", (e) => this.onDblClick(e));
    t.addEventListener("contextmenu", (e) => this.onContext(e));
    t.addEventListener("pointerdown", (e) => this.onPointerDown(e));
    t.addEventListener("keydown", (e) => this.onKey(e));

    store.events.on("sessions", (s) => {
      this.render();
      // A banner arriving later (startup one is shown by app.ts); show each distinct text once until cleared.
      const b = s.banner ?? null;
      if (b && b !== this.lastBanner) showBanner(b, "warning", [{ label: "OK", run: () => void ipc.sessionsClearBanner() }]);
      this.lastBanner = b;
    });
    app.events.on("tabs", () => this.paintLive()); // live dots: event-driven, no polling (§8.2)
    this.render();
  }

  focusSearch() {
    this.search.focus();
    this.search.select();
  }

  newSession() {
    void this.createSession(null);
  }

  // =============================================================== render
  private setQuery(v: string, syncInput = true) {
    if (syncInput) this.search.value = v;
    this.clearBtn.hidden = !this.search.value;
    this.q = v.trim().toLowerCase();
    this.render();
  }

  private isOpen = (id: string) => this.expanded[id] !== false;

  private setOpen(id: string, open: boolean) {
    if (this.isOpen(id) === open) return;
    this.expanded[id] = open;
    lsSet(EXPANDED_KEY, this.expanded);
    this.render();
  }

  private render() {
    const { folders, sessions } = store.sessions;
    this.sessionMap = new Map(sessions.map((s) => [s.id, s]));
    this.folderMap = new Map(folders.map((f) => [f.id, f]));
    const bySort = (a: { sortOrder: number }, b: { sortOrder: number }) => a.sortOrder - b.sortOrder;
    const groups = new Map<string | null, Session[]>();
    for (const s of sessions) {
      const k = s.folderId && this.folderMap.has(s.folderId) ? s.folderId : null;
      groups.set(k, [...(groups.get(k) ?? []), s]);
    }
    for (const g of groups.values()) g.sort(bySort);
    const sortedFolders = [...folders].sort(bySort);

    const out: Node[] = [];
    this.rows = [];
    if (this.q) {
      // Flat list ordered by sortOrder (stable over tree order); DnD is off while searching.
      const ordered = [...sortedFolders.flatMap((f) => groups.get(f.id) ?? []), ...(groups.get(null) ?? [])];
      const hits = ordered.filter((s) => matches(s, this.q)).sort(bySort);
      for (const s of hits) (out.push(this.sessionEl(s)), this.rows.push(s.id));
      if (!hits.length) out.push(h("div", { class: "sb-empty" }, `No sessions match "${this.search.value.trim()}".`));
    } else {
      for (const f of sortedFolders) {
        const kids = groups.get(f.id) ?? [];
        out.push(this.folderEl(f, kids));
        this.rows.push(f.id);
        if (this.isOpen(f.id)) for (const s of kids) this.rows.push(s.id);
      }
      for (const s of groups.get(null) ?? []) (out.push(this.sessionEl(s)), this.rows.push(s.id));
      if (!out.length) out.push(h("div", { class: "sb-empty" }, "No sessions yet. Use + New to create one."));
    }
    this.tree.replaceChildren(...out);

    const visible = new Set(this.rows);
    this.sel = new Set([...this.sel].filter((id) => visible.has(id)));
    if (this.anchor && !visible.has(this.anchor)) this.anchor = null;
    if (this.cursor && !visible.has(this.cursor)) this.cursor = null;
    this.paint();
    this.flushReveal();
  }

  private sessionEl(s: Session): HTMLElement {
    const cmd = sessionCommand(s);
    const q = this.q;
    const img = h("img", { class: "sb-icon", width: 20, height: 20, alt: "" });
    img.draggable = false;
    void iconUrl(s.iconOverride || s.shellPath).then((u) => (u ? (img.src = u) : img.replaceWith(h("span", { class: "sb-icon-fallback" }, icon(ICONS.console, "shell")))));
    const el = h("div", { class: "sb-card sb-session", "data-kind": "session", "data-id": s.id, role: "treeitem", title: s.description || cmd },
      h("span", { class: "sb-dot" }), img,
      h("div", { class: "sb-text" },
        h("div", { class: "sb-name" }, hl(s.name, q)),
        h("div", { class: "sb-cmd" }, hl(cmd, q)),
        // A hit that is only in the description would otherwise show no highlight at all.
        q && s.description.toLowerCase().includes(q) && !s.name.toLowerCase().includes(q) && !cmd.toLowerCase().includes(q)
          ? h("div", { class: "sb-cmd" }, hl(s.description, q)) : null),
      h("span", { class: "sb-live", title: "Open in a tab" }),
      h("span", { class: "sb-acts" }, actBtn("edit", "Edit session", IC.edit), actBtn("del", "Delete session", IC.trash)));
    el.style.setProperty("--tag", s.colorTag);
    return el;
  }

  private folderEl(f: Folder, kids: Session[]): HTMLElement {
    const open = this.isOpen(f.id);
    const head = h("div", { class: "sb-card sb-folder-head", "data-kind": "folder", "data-id": f.id, role: "treeitem", "aria-expanded": String(open) },
      h("button", { class: "sb-chev", "data-act": "toggle", tabindex: -1, title: open ? "Collapse" : "Expand", "aria-label": open ? "Collapse folder" : "Expand folder" }, icon(ICONS.chevron, "")),
      h("span", { class: "sb-folder-glyph" }, icon(ICONS.folder, "folder")),
      h("div", { class: "sb-text" }, h("div", { class: "sb-name" }, f.name)),
      h("span", { class: "sb-count" }, String(kids.length)),
      h("span", { class: "sb-acts" },
        actBtn("add", "New session in folder", ICONS.plus), actBtn("rename", "Rename folder", IC.edit),
        actBtn("del", "Delete folder — sessions in this folder will be moved to the root list", IC.trash)));
    const box = h("div", { class: `sb-folder${open ? "" : " closed"}`, "data-fid": f.id }, head);
    if (open && kids.length) box.append(h("div", { class: "sb-children" }, kids.map((s) => this.sessionEl(s))));
    return box;
  }

  private cardOf(id: string) {
    return this.tree.querySelector<HTMLElement>(`[data-kind][data-id="${id}"]`);
  }

  private paint() {
    for (const el of this.tree.querySelectorAll<HTMLElement>("[data-kind]")) {
      const id = el.dataset.id!;
      const on = this.sel.has(id);
      el.classList.toggle("selected", on);
      el.classList.toggle("cursor", id === this.cursor);
      el.setAttribute("aria-selected", String(on));
    }
    this.paintLive();
  }

  private paintLive() {
    const live = app.liveSessionIds();
    for (const el of this.tree.querySelectorAll<HTMLElement>(".sb-session")) el.classList.toggle("live", live.has(el.dataset.id!));
  }

  private flushReveal() {
    const el = this.reveal && this.cardOf(this.reveal);
    if (!el) return;
    this.reveal = null;
    el.scrollIntoView({ block: "nearest" });
  }

  // ============================================================ selection
  private setSel(ids: string[]) {
    this.sel = new Set(ids);
    this.anchor = this.cursor = ids[0] ?? null;
    this.paint();
  }

  private selSessions() {
    return this.rows.filter((id) => this.sel.has(id) && this.sessionMap.has(id));
  }

  /** Click / arrow semantics: plain = single, Ctrl = toggle, Shift = range from the anchor (sessions only). */
  private pick(id: string, m: { ctrlKey?: boolean; shiftKey?: boolean }) {
    const session = this.sessionMap.has(id);
    const anchorOk = !!this.anchor && this.sessionMap.has(this.anchor);
    if (m.shiftKey && session && anchorOk) {
      const a = this.rows.indexOf(this.anchor!);
      const b = this.rows.indexOf(id);
      const range = this.rows.slice(Math.min(a, b), Math.max(a, b) + 1).filter((x) => this.sessionMap.has(x));
      this.sel = new Set(m.ctrlKey ? [...this.sel, ...range] : range);
    } else if (m.ctrlKey && session) {
      const next = new Set([...this.sel].filter((x) => this.sessionMap.has(x))); // a folder never mixes into a multi-selection
      if (!next.delete(id)) next.add(id);
      this.sel = next;
      this.anchor = id;
    } else {
      this.sel = new Set([id]);
      this.anchor = id;
    }
    this.cursor = id;
    this.paint();
    this.cardOf(id)?.scrollIntoView({ block: "nearest" });
  }

  // ================================================================ mouse
  private onClick(e: MouseEvent) {
    if (this.suppressClick) return;
    const t = e.target as HTMLElement;
    const card = t.closest<HTMLElement>("[data-kind]");
    if (!card) return this.setSel([]);
    const id = card.dataset.id!;
    const act = t.closest<HTMLElement>("[data-act]")?.dataset.act;
    if (!act) return this.pick(id, e);
    const s = this.sessionMap.get(id);
    const f = this.folderMap.get(id);
    if (act === "toggle" && f) this.setOpen(id, !this.isOpen(id));
    else if (act === "edit" && s) void this.edit(s);
    else if (act === "add" && f) void this.createSession(id);
    else if (act === "rename" && f) void this.renameFolder(f);
    else if (act === "del") void (s ? this.delSessions([id]) : f && this.delFolder(f));
  }

  private onDblClick(e: MouseEvent) {
    const t = e.target as HTMLElement;
    const card = t.closest<HTMLElement>("[data-kind]");
    if (!card || t.closest("button")) return; // double-clicks on buttons are ignored (§8.2)
    const id = card.dataset.id!;
    if (this.folderMap.has(id)) return this.setOpen(id, !this.isOpen(id));
    this.setSel([id]);
    this.open([id], e.ctrlKey); // Ctrl+double-click = run as administrator (§4.9)
  }

  private onContext(e: MouseEvent) {
    e.preventDefault();
    const t = e.target as HTMLElement;
    const card = t.closest<HTMLElement>("[data-kind]") ?? t.closest<HTMLElement>(".sb-folder")?.querySelector<HTMLElement>("[data-kind]") ?? null;
    if (!card) {
      this.setSel([]);
      return this.backgroundMenu(e.clientX, e.clientY);
    }
    const id = card.dataset.id!;
    if (!this.sel.has(id)) this.setSel([id]);
    this.cursor = id;
    this.paint();
    const s = this.sessionMap.get(id);
    const f = this.folderMap.get(id);
    if (s) this.sessionMenu(s, e.clientX, e.clientY);
    else if (f) this.folderMenu(f, e.clientX, e.clientY);
  }

  // =============================================================== keyboard
  private onKey(e: KeyboardEvent) {
    const t = e.target as HTMLElement;
    if (t.closest("button,input,textarea") || e.altKey || e.isComposing) return;
    const id = this.cursor;
    const at = id ? this.rows.indexOf(id) : -1;
    const go = (i: number) => {
      const to = this.rows[Math.max(0, Math.min(this.rows.length - 1, i))];
      if (to) this.pick(to, { shiftKey: e.shiftKey });
    };
    const f = id ? this.folderMap.get(id) : undefined;
    const s = id ? this.sessionMap.get(id) : undefined;
    switch (e.key) {
      case "ArrowDown": go(at + 1); break;
      case "ArrowUp": go(at < 0 ? 0 : at - 1); break;
      case "Home": go(0); break;
      case "End": go(this.rows.length - 1); break;
      case "ArrowLeft":
        if (f && this.isOpen(f.id)) this.setOpen(f.id, false);
        else if (s?.folderId && !this.q && this.folderMap.has(s.folderId)) this.pick(s.folderId, {});
        break;
      case "ArrowRight":
        if (f && !this.isOpen(f.id)) this.setOpen(f.id, true);
        else if (f && this.rows[at + 1] && this.sessionMap.get(this.rows[at + 1])?.folderId === f.id) this.pick(this.rows[at + 1], {});
        break;
      case "Enter":
        if (f) this.setOpen(f.id, !this.isOpen(f.id));
        else this.open(this.selSessions(), e.ctrlKey); // Ctrl+Enter = run as administrator
        break;
      case "F2":
        if (f) void this.renameFolder(f);
        else if (s) void this.renameSession(s);
        break;
      case "Delete":
        if (f) void this.delFolder(f);
        else void this.delSessions(this.selSessions());
        break;
      case "a":
      case "A":
        if (!e.ctrlKey) return;
        this.sel = new Set(this.rows.filter((x) => this.sessionMap.has(x)));
        this.paint();
        break;
      default: return;
    }
    e.preventDefault();
  }

  // ============================================================ drag & drop
  private onPointerDown(e: PointerEvent) {
    const t = e.target as HTMLElement;
    if (e.button !== 0 || this.q || t.closest("button")) return;
    const card = t.closest<HTMLElement>("[data-kind]");
    if (!card) return;
    const id = card.dataset.id!;
    const isFolder = card.dataset.kind === "folder";
    const x0 = e.clientX;
    const y0 = e.clientY;
    let ghost: HTMLElement | null = null;
    let drop: Drop | null = null;
    let marked: HTMLElement | null = null;
    let dragged: string[] = [];
    const unmark = () => {
      marked?.classList.remove("drop-before", "drop-after", "drop-into", "drop-root");
      marked = null;
    };
    const move = (ev: PointerEvent) => {
      if (!ghost) {
        if (Math.hypot(ev.clientX - x0, ev.clientY - y0) < 6) return; // 6 px threshold
        // Dragging a selected session drags the whole selection, in sidebar order (§8.2).
        dragged = isFolder ? [] : this.sel.has(id) ? this.selSessions() : [id];
        const label = isFolder ? this.folderMap.get(id)!.name : dragged.length > 1 ? `${dragged.length} sessions` : this.sessionMap.get(id)!.name;
        ghost = h("div", { class: "sb-ghost" }, label);
        document.body.append(ghost);
        this.tree.classList.add("is-dragging");
        for (const d of isFolder ? [id] : dragged) this.cardOf(d)?.classList.add("dragging");
        try { this.tree.setPointerCapture(e.pointerId); } catch { /* pointer already gone */ }
      }
      ghost.style.transform = `translate(${ev.clientX + 14}px, ${ev.clientY + 10}px)`;
      const r = this.tree.getBoundingClientRect();
      if (ev.clientY < r.top + 24) this.tree.scrollTop -= 14;
      else if (ev.clientY > r.bottom - 24) this.tree.scrollTop += 14;
      drop = this.dropAt(ev.clientX, ev.clientY, id, isFolder, dragged);
      unmark();
      if (drop) {
        marked = drop.el;
        marked.classList.add(`drop-${drop.mode}`);
      }
    };
    const end = (commit: boolean) => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
      window.removeEventListener("pointercancel", cancel);
      window.removeEventListener("keydown", key, true);
      if (!ghost) return;
      unmark();
      ghost.remove();
      this.tree.classList.remove("is-dragging");
      for (const el of this.tree.querySelectorAll(".dragging")) el.classList.remove("dragging");
      try { this.tree.releasePointerCapture(e.pointerId); } catch { /* already released */ }
      this.suppressClick = true; // the click that follows pointerup must not change the selection
      setTimeout(() => (this.suppressClick = false), 0);
      if (commit && drop) void this.commitDrop(drop, id, isFolder, dragged);
    };
    const up = () => end(true);
    const cancel = () => end(false);
    const key = (ev: KeyboardEvent) => {
      if (ev.key !== "Escape") return;
      ev.stopPropagation();
      end(false);
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
    window.addEventListener("pointercancel", cancel);
    window.addEventListener("keydown", key, true);
  }

  /** Drop target under the pointer. Top/bottom halves use the HEADER ROW of a folder, not its whole box (§8.2). */
  private dropAt(x: number, y: number, id: string, isFolder: boolean, dragged: string[]): Drop | null {
    const el = document.elementFromPoint(x, y) as HTMLElement | null;
    if (!el || !this.tree.contains(el)) return null;
    const card = el.closest<HTMLElement>("[data-kind]");
    const half = (c: HTMLElement): "top" | "bottom" => {
      const r = c.getBoundingClientRect();
      return y < r.top + r.height / 2 ? "top" : "bottom";
    };
    const root: Drop = { target: { kind: "root" }, half: "bottom", el: this.tree, mode: "root" };
    if (isFolder) {
      if (card?.dataset.kind !== "folder") return root; // "anything else": moved to the end
      if (card.dataset.id === id) return null;
      const hf = half(card);
      return { target: { kind: "folder", id: card.dataset.id! }, half: hf, el: card.parentElement!, mode: hf === "top" ? "before" : "after" };
    }
    if (card?.dataset.kind === "session") {
      if (dragged.includes(card.dataset.id!)) return null; // no-op on one of the dragged items
      const hf = half(card);
      return { target: { kind: "session", id: card.dataset.id! }, half: hf, el: card, mode: hf === "top" ? "before" : "after" };
    }
    if (card) return { target: { kind: "folder", id: card.dataset.id! }, half: "top", el: card, mode: "into" };
    const box = el.closest<HTMLElement>(".sb-folder");
    if (box) return { target: { kind: "folderEdge", id: box.dataset.fid! }, half: "top", el: box.firstElementChild as HTMLElement, mode: "into" };
    return root;
  }

  private async commitDrop(d: Drop, id: string, isFolder: boolean, dragged: string[]) {
    try {
      const moved = await ipc.itemsMove(isFolder ? [] : dragged, isFolder ? id : null, d.target, d.half);
      if (moved && !isFolder && (d.target.kind === "folder" || d.target.kind === "folderEdge")) this.setOpen(d.target.id, true);
    } catch (e) {
      toast(String(e), "error");
    }
  }

  // ================================================================ actions
  private open(ids: string[], admin: boolean) {
    for (const id of ids) {
      const s = this.sessionMap.get(id);
      if (s) void app.openSession(s, admin);
    }
  }

  private async createSession(folderId: string | null) {
    const s = await openSessionEditor(undefined, { folderId });
    if (!s) return;
    if (folderId) this.setOpen(folderId, true);
    this.setSel([s.id]);
    this.reveal = s.id;
    this.flushReveal();
  }

  private async edit(s: Session) {
    const saved = await openSessionEditor(s);
    if (saved) this.setSel([saved.id]);
  }

  private async duplicate(s: Session) {
    try {
      const copy = await ipc.sessionDuplicate(s.id);
      const named = await ipc.sessionUpdate({ ...copy, name: `${copy.name} (copy)` }).catch(() => copy);
      this.setSel([named.id]);
      this.reveal = named.id;
      this.flushReveal();
    } catch (e) {
      toast(String(e), "error");
    }
  }

  private copyCommand(s: Session) {
    void ipc.clipboardWrite(sessionCommand(s)).then(() => toast("Command line copied.", "success"), (e) => toast(String(e), "error"));
  }

  private async renameSession(s: Session) {
    const name = await promptDialog({ title: "Rename session", label: "Session name", value: s.name });
    if (name === undefined || name === s.name) return;
    try {
      await ipc.sessionUpdate({ ...s, name });
    } catch (e) {
      toast(String(e), "error");
    }
  }

  private async renameFolder(f: Folder) {
    const name = await promptDialog({ title: "Rename folder", label: "Folder name", value: f.name }); // empty is not allowed
    if (name === undefined || name === f.name) return;
    try {
      await ipc.folderRename(f.id, name);
    } catch (e) {
      toast(String(e), "error");
    }
  }

  private async newFolder() {
    const name = await promptDialog({ title: "New folder", label: "Folder name", value: "New Folder" });
    if (name === undefined) return;
    try {
      const f = await ipc.folderAdd(name);
      this.setSel([f.id]);
      this.reveal = f.id;
      this.flushReveal();
    } catch (e) {
      toast(String(e), "error");
    }
  }

  private async delSessions(ids: string[]) {
    const ss = ids.map((i) => this.sessionMap.get(i)).filter((s): s is Session => !!s);
    if (!ss.length) return;
    const msg = ss.length === 1 ? `Delete session "${ss[0].name}"?` : `Delete ${ss.length} sessions?`;
    if (!(await confirmDialog(msg, { title: ss.length === 1 ? "Delete session" : "Delete sessions", ok: "Delete", danger: true }))) return;
    try {
      for (const s of ss) await ipc.sessionDelete(s.id);
    } catch (e) {
      toast(String(e), "error");
    }
  }

  private async delFolder(f: Folder) {
    const msg = `Delete folder "${f.name}"?\nSessions in this folder will be moved to the root list.`;
    if (!(await confirmDialog(msg, { title: "Delete folder", ok: "Delete folder", danger: true }))) return;
    try {
      await ipc.folderDelete(f.id);
    } catch (e) {
      toast(String(e), "error");
    }
  }

  private async exportSessions() {
    try {
      const p = await ipc.sessionsExport();
      if (p) toast(`Exported sessions to ${p}`, "success");
    } catch (e) {
      toast(String(e), "error");
    }
  }

  private async importSessions() {
    const mode = await choiceDialog("Import sessions",
      "Merge adds the imported sessions to your current ones (duplicates are skipped). Replace discards all current sessions and folders first.",
      [{ label: "Cancel", value: "cancel" }, { label: "Replace", value: "replace", kind: "danger" }, { label: "Merge", value: "merge", kind: "primary" }], "merge");
    if (mode !== "merge" && mode !== "replace") return;
    try {
      const r = await ipc.sessionsImport(mode);
      if (r) toast(`Imported ${r.added} session(s)${r.skipped ? `, ${r.skipped} skipped` : ""}.`, r.added ? "success" : "info");
    } catch (e) {
      toast(String(e), "error");
    }
  }

  private async importWt() {
    try {
      const r = await ipc.importWindowsTerminal();
      toast(r.message, r.added ? "success" : "info");
    } catch (e) {
      toast(String(e), "error");
    }
  }

  private async importSsh() {
    try {
      const r = await ipc.importSshConfig();
      toast(r.message, r.added ? "success" : "info");
    } catch (e) {
      toast(String(e), "error");
    }
  }

  // ================================================================== menus
  private headerMenu(anchor: HTMLElement) {
    const r = anchor.getBoundingClientRect();
    showMenu(r.left, r.bottom + 2, [
      { label: "Export sessions…", action: () => void this.exportSessions() },
      { label: "Import sessions…", action: () => void this.importSessions() },
      { label: "New folder", action: () => void this.newFolder() },
      { separator: true },
      { label: "Import from Windows Terminal", action: () => void this.importWt() },
      { label: "Import from SSH Config", action: () => void this.importSsh() },
    ]);
  }

  private backgroundMenu(x: number, y: number) {
    showMenu(x, y, [
      { label: "New session", action: () => void this.createSession(null) },
      { label: "New folder", action: () => void this.newFolder() },
      { separator: true },
      { label: "Import from Windows Terminal", action: () => void this.importWt() },
      { label: "Import from SSH Config", action: () => void this.importSsh() },
      { separator: true },
      { label: "Export sessions…", action: () => void this.exportSessions() },
      { label: "Import sessions…", action: () => void this.importSessions() },
    ]);
  }

  private sessionMenu(s: Session, x: number, y: number) {
    const ids = this.selSessions();
    const many = ids.length > 1;
    const items: MenuItem[] = [
      { label: many ? `Open ${ids.length} sessions` : "Open", hint: "Enter", action: () => this.open(ids, false) },
      { label: "Run as administrator", hint: "Ctrl+Enter", action: () => this.open(ids, true) },
      { label: "Edit", disabled: many, action: () => void this.edit(s) },
      { label: "Duplicate", disabled: many, action: () => void this.duplicate(s) },
      { label: "Copy command line", disabled: many, action: () => this.copyCommand(s) },
      { separator: true },
      { label: many ? `Delete ${ids.length} sessions` : "Delete", hint: "Del", danger: true, action: () => void this.delSessions(ids) },
    ];
    showMenu(x, y, items);
  }

  private folderMenu(f: Folder, x: number, y: number) {
    const order = [...store.sessions.folders].sort((a, b) => a.sortOrder - b.sortOrder);
    const i = order.findIndex((o) => o.id === f.id);
    const open = this.isOpen(f.id);
    const move = (dir: "up" | "down") => void ipc.folderMove(f.id, dir).catch((e) => toast(String(e), "error"));
    showMenu(x, y, [
      { label: "New session in folder", action: () => void this.createSession(f.id) },
      { label: "Rename", hint: "F2", action: () => void this.renameFolder(f) },
      { label: "Move up", disabled: i <= 0, action: () => move("up") },
      { label: "Move down", disabled: i < 0 || i >= order.length - 1, action: () => move("down") },
      { separator: true },
      { label: "Expand", disabled: open, action: () => this.setOpen(f.id, true) },
      { label: "Collapse", disabled: !open, action: () => this.setOpen(f.id, false) },
      { separator: true },
      { label: "Delete folder", hint: "Del", danger: true, action: () => void this.delFolder(f) },
    ]);
  }
}
