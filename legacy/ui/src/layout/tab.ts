// A tab: a binary split tree of panes. All pane elements are absolutely positioned children of one body
// element, so splitting/closing never re-parents a live xterm (which would risk losing its WebGL context).

import { h, uid, clamp } from "../dom";
import { ipc } from "../ipc";
import { store } from "../store";
import { TerminalPane, panes, type PaneHooks } from "../terminal/pane";
import type { LaunchSpec, ShellEventKind } from "../types";

export type Layout =
  | { type: "leaf"; paneId: string; cwd?: string | null }
  | { type: "split"; dir: "right" | "down"; ratio: number; a: Layout; b: Layout };

export const MAX_PANES = 16;
const SPLITTER = 2;
const MIN_PANE = 60;

export interface TabHooks {
  changed(t: Tab, what: "title" | "cwd" | "exit" | "focus" | "activity" | "shell" | "branch" | "layout" | "flags" | "spawned"): void;
  contextMenu(t: Tab, p: TerminalPane, e: MouseEvent): void;
  zoom(t: Tab, p: TerminalPane, dir: 1 | -1): void;
  input(t: Tab, p: TerminalPane, b: Uint8Array): void;
  bell(t: Tab, p: TerminalPane): void;
  shell(t: Tab, p: TerminalPane, kind: ShellEventKind, exitCode: number | null, durationMs: number | null): void;
  output(t: Tab, p: TerminalPane): void;
  empty(t: Tab): void;
  searchAll(q: string, o: { regex: boolean; caseSensitive: boolean; wholeWord: boolean }): void;
}

export interface TabInit {
  id?: string;
  title: string;
  titleLocked?: boolean;
  color?: string | null;
  pinned?: boolean;
  group?: string;
  readOnly?: boolean;
  /** Original command line of the tab (profile command, session command, quick-connect line…). */
  command: string;
  launch: LaunchSpec;
  cwd?: string;
  startingCommand?: string;
  sessionId?: string;
  env?: Record<string, string>;
  fontSize?: number;
  themeBackground?: string;
  integration?: "auto" | "off";
  /** Restored layout; leaf cwds are each pane's last live cwd. */
  layout?: Layout | null;
}

interface Rect { x: number; y: number; w: number; h: number }

export class Tab {
  readonly id: string;
  readonly el: HTMLElement;
  title: string;
  titleLocked: boolean;
  color: string | null;
  pinned: boolean;
  group: string;
  readOnly: boolean;
  broadcast = false;
  command: string;
  launch: LaunchSpec;
  cwd?: string;
  startingCommand?: string;
  sessionId?: string;
  env?: Record<string, string>;
  fontSize: number;
  themeBackground: string;
  integration?: "auto" | "off";
  activity = false;
  layout: Layout;
  focusedId = "";
  visible = false;
  logging = false;
  recording = false;
  readonly paneIds: string[] = [];

  private rects = new Map<string, Rect>();
  private splitters: HTMLElement[] = [];
  private ro: ResizeObserver;
  private pendingStart: Set<string> = new Set();

  constructor(init: TabInit, private hooks: TabHooks) {
    this.id = init.id ?? uid("t");
    this.title = init.title;
    this.titleLocked = !!init.titleLocked;
    this.color = init.color ?? null;
    this.pinned = !!init.pinned;
    this.group = init.group ?? "";
    this.readOnly = !!init.readOnly;
    this.command = init.command;
    this.launch = init.launch;
    this.cwd = init.cwd;
    this.startingCommand = init.startingCommand;
    this.sessionId = init.sessionId;
    this.env = init.env;
    this.fontSize = init.fontSize ?? 0;
    this.themeBackground = init.themeBackground ?? "";
    this.integration = init.integration;
    this.el = h("div", { class: "tab-body inactive" });
    this.ro = new ResizeObserver(() => this.relayout());
    this.ro.observe(this.el);

    const restored = init.layout && validLayout(init.layout) ? init.layout : null;
    if (restored) {
      this.layout = restored;
      for (const leaf of leaves(restored)) this.createPane(leaf.paneId, leaf.cwd ?? undefined);
    } else {
      const id = uid("p");
      this.layout = { type: "leaf", paneId: id };
      this.createPane(id, init.cwd);
    }
    this.focusedId = this.paneIds[0];
    this.relayout();
  }

  // ----------------------------------------------------------------- panes
  private createPane(paneId: string, cwd?: string) {
    const hooks: PaneHooks = {
      onFocus: (p) => this.focusPane(p.id, false),
      onInput: (p, b) => this.hooks.input(this, p, b),
      onOutput: (p) => {
        if (!this.visible && !this.activity) {
          this.activity = true;
          this.hooks.changed(this, "activity");
        }
        this.hooks.output(this, p);
      },
      onBell: (p) => {
        if (!this.visible) this.activity = true;
        this.hooks.bell(this, p);
        this.hooks.changed(this, "activity");
      },
      onTitle: (p) => {
        if (!this.titleLocked && p.id === this.focusedId && p.title) {
          this.title = p.title;
          this.hooks.changed(this, "title");
        }
      },
      onCwd: (p) => this.hooks.changed(this, p.id === this.focusedId ? "cwd" : "layout"),
      onShell: (p, k, code, dur) => {
        this.hooks.shell(this, p, k, code, dur);
        this.hooks.changed(this, "shell");
      },
      onBranch: () => this.hooks.changed(this, "branch"),
      onSpawned: () => this.hooks.changed(this, "spawned"),
      onStateChange: () => this.hooks.changed(this, "exit"),
      onExit: (p, code) => this.paneExited(p, code),
      onContextMenu: (p, e) => this.hooks.contextMenu(this, p, e),
      onZoom: (p, d) => this.hooks.zoom(this, p, d),
      onSearchAll: (q, o) => this.hooks.searchAll(q, o),
    };
    const pane = new TerminalPane({
      id: paneId,
      launch: this.launch,
      cwd: cwd ?? this.cwd,
      env: this.env,
      startingCommand: this.startingCommand,
      integration: this.integration,
      sessionId: this.sessionId,
      fontSize: this.fontSize || undefined,
      themeBackground: this.themeBackground || undefined,
      readOnly: this.readOnly,
      hooks,
    });
    pane.setFlags({ readOnly: this.readOnly, broadcast: this.broadcast });
    this.paneIds.push(paneId);
    this.el.append(pane.el);
    // Start lazily, only once the tab is visible (hidden panes never fit, §5.4).
    this.pendingStart.add(paneId);
    return pane;
  }

  pane(id: string) {
    return panes.get(id);
  }

  get focused(): TerminalPane | undefined {
    return panes.get(this.focusedId) ?? panes.get(this.paneIds[0]);
  }

  allPanes(): TerminalPane[] {
    return this.paneIds.map((i) => panes.get(i)).filter((p): p is TerminalPane => !!p);
  }

  private paneExited(p: TerminalPane, code: number | null) {
    this.hooks.changed(this, "exit");
    const mode = store.settings.terminal.closeOnExit;
    if (mode === "always" || (mode === "graceful" && code === 0)) this.closePane(p.id);
  }

  focusPane(id: string, focusTerm = true) {
    if (!panes.has(id)) return;
    const changed = this.focusedId !== id;
    this.focusedId = id;
    for (const pid of this.paneIds) panes.get(pid)?.el.classList.toggle("focused", pid === id);
    this.el.classList.toggle("multi", this.paneIds.length > 1);
    if (focusTerm) panes.get(id)?.focus();
    if (changed) this.hooks.changed(this, "focus");
  }

  /** Split the pane `fromId` (default: focused). New panes inherit tab flags and the live LOCAL cwd (§7.4). */
  split(dir: "right" | "down", fromId = this.focusedId): TerminalPane | null {
    if (this.paneIds.length >= MAX_PANES) return null;
    const src = panes.get(fromId);
    const cwd = src && src.cwdLocal && src.cwd ? src.cwd : src?.init.cwd ?? this.cwd;
    const id = uid("p");
    this.layout = replaceLeaf(this.layout, fromId, { type: "split", dir, ratio: 0.5, a: { type: "leaf", paneId: fromId }, b: { type: "leaf", paneId: id } });
    const pane = this.createPane(id, cwd ?? undefined);
    this.relayout();
    this.focusPane(id);
    if (this.visible) void this.startPending();
    this.hooks.changed(this, "layout");
    return pane;
  }

  /** P0 parity layout: "Add Pane" fills 1→2→3→4 in the fixed arrangement, then keeps splitting right. */
  addPane(): TerminalPane | null {
    const n = this.paneIds.length;
    if (n === 1) return this.split("right", this.paneIds[0]);
    if (n === 2) return this.split("down", this.paneIds[0]);
    if (n === 3) return this.split("right", this.paneIds[2]);
    return this.split("right");
  }

  closePane(id: string) {
    const pane = panes.get(id);
    if (!pane) return;
    const next = removeLeaf(this.layout, id);
    const order = this.paneIds.indexOf(id);
    this.paneIds.splice(order, 1);
    pane.dispose();
    this.pendingStart.delete(id);
    if (!next) return this.hooks.empty(this);
    this.layout = next;
    this.rects.delete(id);
    this.relayout();
    if (this.focusedId === id) {
      // Focus moves to the sibling (§7.4): the pane now occupying the nearest slot in tree order.
      this.focusPane(this.paneIds[Math.min(order, this.paneIds.length - 1)]);
    } else this.focusPane(this.focusedId, false);
    this.hooks.changed(this, "layout");
  }

  unsplitAll() {
    for (const id of [...this.paneIds].slice(1)) this.closePane(id);
  }

  // ------------------------------------------------------------- visibility
  show() {
    this.visible = true;
    this.activity = false;
    this.el.classList.remove("inactive");
    this.relayout();
    for (const p of this.allPanes()) p.visible = true;
    this.focusPane(this.focusedId, false);
    void this.startPending();
    for (const p of this.allPanes()) void p.shown();
    this.focused?.focus();
    this.hooks.changed(this, "activity");
  }

  hide() {
    this.visible = false;
    this.el.classList.add("inactive");
    for (const p of this.allPanes()) p.visible = false;
  }

  /** Start every pane now without showing the tab (lazy restore, §16.3): bodies keep their size when inactive. */
  async startAll() {
    this.relayout();
    for (const p of this.allPanes()) if (!p.started) await p.start();
    this.pendingStart.clear();
  }

  private async startPending() {
    for (const id of [...this.pendingStart]) {
      this.pendingStart.delete(id);
      const p = panes.get(id);
      if (p) await p.shown();
    }
  }

  // ------------------------------------------------------------------ layout
  relayout() {
    const W = this.el.clientWidth;
    const H = this.el.clientHeight;
    if (!W || !H) return;
    this.rects.clear();
    const sp: { r: Rect; dir: "right" | "down"; node: Layout & { type: "split" }; parent: Rect }[] = [];
    const place = (n: Layout, r: Rect) => {
      if (n.type === "leaf") return void this.rects.set(n.paneId, r);
      if (n.dir === "right") {
        const aw = clamp(Math.round((r.w - SPLITTER) * n.ratio), Math.min(MIN_PANE, r.w / 2), r.w - Math.min(MIN_PANE, r.w / 2) - SPLITTER);
        place(n.a, { x: r.x, y: r.y, w: aw, h: r.h });
        place(n.b, { x: r.x + aw + SPLITTER, y: r.y, w: r.w - aw - SPLITTER, h: r.h });
        sp.push({ r: { x: r.x + aw, y: r.y, w: SPLITTER, h: r.h }, dir: "right", node: n, parent: r });
      } else {
        const ah = clamp(Math.round((r.h - SPLITTER) * n.ratio), Math.min(MIN_PANE, r.h / 2), r.h - Math.min(MIN_PANE, r.h / 2) - SPLITTER);
        place(n.a, { x: r.x, y: r.y, w: r.w, h: ah });
        place(n.b, { x: r.x, y: r.y + ah + SPLITTER, w: r.w, h: r.h - ah - SPLITTER });
        sp.push({ r: { x: r.x, y: r.y + ah, w: r.w, h: SPLITTER }, dir: "down", node: n, parent: r });
      }
    };
    place(this.layout, { x: 0, y: 0, w: W, h: H });
    for (const [id, r] of this.rects) {
      const el = panes.get(id)?.el;
      if (el) Object.assign(el.style, { left: `${r.x}px`, top: `${r.y}px`, width: `${r.w}px`, height: `${r.h}px` });
    }
    this.syncSplitters(sp);
    this.el.classList.toggle("multi", this.paneIds.length > 1);
  }

  private syncSplitters(sp: { r: Rect; dir: "right" | "down"; node: Layout & { type: "split" }; parent: Rect }[]) {
    while (this.splitters.length > sp.length) this.splitters.pop()!.remove();
    while (this.splitters.length < sp.length) {
      const el = h("div", { class: "splitter" });
      this.el.append(el);
      this.splitters.push(el);
    }
    sp.forEach((s, i) => {
      const el = this.splitters[i];
      el.className = `splitter ${s.dir === "right" ? "vertical" : "horizontal"}`;
      Object.assign(el.style, { left: `${s.r.x}px`, top: `${s.r.y}px`, width: `${s.r.w}px`, height: `${s.r.h}px` });
      el.onpointerdown = (ev) => this.dragSplitter(ev, el, s.node, s.dir, s.parent);
    });
  }

  private dragSplitter(ev: PointerEvent, el: HTMLElement, node: Layout & { type: "split" }, dir: "right" | "down", parent: Rect) {
    ev.preventDefault();
    el.setPointerCapture(ev.pointerId);
    const box = this.el.getBoundingClientRect();
    const move = (e: PointerEvent) => {
      const pos = dir === "right" ? e.clientX - box.left - parent.x : e.clientY - box.top - parent.y;
      const span = (dir === "right" ? parent.w : parent.h) - SPLITTER;
      node.ratio = clamp(pos / span, Math.min(0.45, MIN_PANE / span), Math.max(0.55, 1 - MIN_PANE / span));
      this.relayout();
    };
    const up = () => {
      el.removeEventListener("pointermove", move);
      el.removeEventListener("pointerup", up);
      this.hooks.changed(this, "layout");
    };
    el.addEventListener("pointermove", move);
    el.addEventListener("pointerup", up);
  }

  /**
   * Geometric focus move (§7.4 [P1]). Returns false when there is no pane that way, so the key can pass
   * through to the shell (PSReadLine uses Ctrl+Shift+Arrow to select words).
   */
  moveFocus(dir: "left" | "right" | "up" | "down"): boolean {
    const cur = this.rects.get(this.focusedId);
    if (!cur || this.rects.size < 2) return false;
    let best: { id: string; d: number } | null = null;
    for (const [id, r] of this.rects) {
      if (id === this.focusedId) continue;
      const overlapX = Math.min(cur.x + cur.w, r.x + r.w) - Math.max(cur.x, r.x);
      const overlapY = Math.min(cur.y + cur.h, r.y + r.h) - Math.max(cur.y, r.y);
      let d: number | null = null;
      if (dir === "right" && r.x >= cur.x + cur.w - 1 && overlapY > 0) d = r.x - (cur.x + cur.w);
      if (dir === "left" && r.x + r.w <= cur.x + 1 && overlapY > 0) d = cur.x - (r.x + r.w);
      if (dir === "down" && r.y >= cur.y + cur.h - 1 && overlapX > 0) d = r.y - (cur.y + cur.h);
      if (dir === "up" && r.y + r.h <= cur.y + 1 && overlapX > 0) d = cur.y - (r.y + r.h);
      if (d !== null && (!best || d < best.d)) best = { id, d };
    }
    if (!best) return false;
    this.focusPane(best.id);
    return true;
  }

  // ----------------------------------------------------------------- flags
  setReadOnly(v: boolean) {
    this.readOnly = v;
    for (const p of this.allPanes()) p.setFlags({ readOnly: v });
    this.hooks.changed(this, "flags");
  }

  setBroadcast(v: boolean) {
    this.broadcast = v;
    for (const p of this.allPanes()) p.setFlags({ broadcast: v });
    this.hooks.changed(this, "flags");
  }

  // --------------------------------------------------------------- snapshot
  /** Local live cwd of the focused pane, only if it exists locally (checked by the caller / backend). */
  liveCwd(): string | undefined {
    const p = this.focused;
    return p && p.cwdLocal && p.cwd ? p.cwd : undefined;
  }

  serializeLayout(n: Layout = this.layout): Layout {
    if (n.type === "leaf") {
      const p = panes.get(n.paneId);
      return { type: "leaf", paneId: n.paneId, cwd: p && p.cwdLocal && p.cwd ? p.cwd : p?.init.cwd ?? null };
    }
    return { type: "split", dir: n.dir, ratio: n.ratio, a: this.serializeLayout(n.a), b: this.serializeLayout(n.b) };
  }

  /** Any pane with something running (OSC 133 `running`, or shell children for shells without integration). */
  async runningCount(): Promise<number> {
    let n = 0;
    for (const p of this.allPanes()) {
      if (p.exited || !p.started) continue;
      if (p.phase === "running") n++;
      else if (p.info && !p.info.injected && (await ipc.hasBusyChildren(p.id).catch(() => false))) n++;
      else if (p.info?.shellKind === "ssh") n++; // an ssh session whose primary process is alive
    }
    return n;
  }

  dispose() {
    this.ro.disconnect();
    for (const p of this.allPanes()) p.dispose();
    this.el.remove();
  }
}

// -------------------------------------------------------------- tree helpers
export function leaves(n: Layout): { paneId: string; cwd?: string | null }[] {
  return n.type === "leaf" ? [n] : [...leaves(n.a), ...leaves(n.b)];
}

function validLayout(n: unknown, depth = 0): n is Layout {
  const l = n as Layout;
  if (!l || depth > 8) return false;
  if (l.type === "leaf") return typeof l.paneId === "string" && l.paneId.length > 0;
  return l.type === "split" && (l.dir === "right" || l.dir === "down") && validLayout(l.a, depth + 1) && validLayout(l.b, depth + 1) && leaves(l).length <= MAX_PANES;
}

function replaceLeaf(n: Layout, id: string, by: Layout): Layout {
  if (n.type === "leaf") return n.paneId === id ? by : n;
  return { ...n, a: replaceLeaf(n.a, id, by), b: replaceLeaf(n.b, id, by) };
}

/** Remove a leaf; its parent split collapses into the sibling. `null` when nothing is left. */
function removeLeaf(n: Layout, id: string): Layout | null {
  if (n.type === "leaf") return n.paneId === id ? null : n;
  const a = removeLeaf(n.a, id);
  const b = removeLeaf(n.b, id);
  if (!a) return b;
  if (!b) return a;
  return { ...n, a, b };
}
