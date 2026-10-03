// Application orchestrator: tabs, panes, actions/shortcuts, window state, notifications. Feature panels
// (sidebar, settings, palette, browser) plug into the `app` singleton exported here.

import { Emitter, clamp, debounce, h, exeStem, sleep } from "./dom";
import { ipc, isTauri, listen, type AppInfo } from "./ipc";
import { applyBundle, defaultProfile, loadStore, patchSettings, persistFontSize, refreshShells, store } from "./store";
import { compileKeymap, matchAction } from "./keys";
import { Tab, type TabHooks, type TabInit, type Layout } from "./layout/tab";
import { TabStrip, TAB_COLORS } from "./layout/tabstrip";
import { StatusBar, fmt } from "./layout/statusbar";
import { TitleBar, appWindow } from "./layout/titlebar";
import { initPaneEvents, panes, type TerminalPane } from "./terminal/pane";
import * as renderer from "./terminal/renderer";
import { confirmDialog, choiceDialog, promptDialog, modalState } from "./dialogs/modal";
import { showMenu, showBanner, toast, type MenuItem } from "./dialogs/menu";
import { Sidebar } from "./panels/sidebar";
import { BrowserPanel, sendSelectionToBrowser } from "./panels/browser";
import { openPalette } from "./dialogs/palette";
import { openSettings } from "./settings/panel";
import { openQuickConnect } from "./dialogs/quick-connect";
import { initDropUi } from "./dialogs/drop";
import type { Session, ShellProfile, Snippet, Workspace } from "./types";

const FUNNY = ["Quantum Potato", "Void Chicken", "Turbo Waffle", "Cosmic Pickle", "Neon Walrus", "Spicy Nebula", "Gravity Noodle", "Laser Badger", "Pixel Llama", "Atomic Muffin", "Hyper Teapot", "Rocket Cabbage", "Sonic Biscuit", "Plasma Penguin", "Glitch Otter", "Orbital Taco", "Fuzzy Kernel", "Binary Banana", "Crispy Daemon", "Lunar Pancake", "Stealth Pretzel", "Mighty Mango", "Cyber Koala", "Zen Toaster"];
const funnyName = () => FUNNY[Math.floor(Math.random() * FUNNY.length)];

/** The command line of a saved session, for display / restore fallback (the backend builds the real one). */
export function sessionCommand(s: Session): string {
  const p = s.shellPath.trim();
  if (!s.arguments.trim()) return p.includes(" ") && !p.startsWith('"') && !/\.(exe|cmd|bat)\b.* /i.test(p) ? `"${p}"` : p;
  return `${p.startsWith('"') ? p : `"${p}"`} ${s.arguments}`;
}

export interface AppEvents extends Record<string, unknown> {
  tabs: null;
  active: Tab | null;
}

class App {
  readonly events = new Emitter<AppEvents>();
  tabs: Tab[] = [];
  active: Tab | null = null;
  windowFocused = true;
  info!: AppInfo;

  strip!: TabStrip;
  status!: StatusBar;
  titlebar!: TitleBar;
  sidebar!: Sidebar;
  browser!: BrowserPanel;
  sidebarOpen = true;
  sidebarWidth = 260;
  browserOpen = false;
  browserWidth = 500;

  private panesHost!: HTMLElement;
  private restored = false;
  private closing = false;
  private replacing = false;
  private lastSaved = "";
  private saveTimer: ReturnType<typeof setTimeout> | undefined;
  private fgTimer: ReturnType<typeof setTimeout> | undefined;
  private bursts = new Map<string, { n: number; timer?: ReturnType<typeof setTimeout> }>();

  // ================================================================== boot
  async init(root: HTMLElement) {
    await loadStore();
    this.info = store.info;
    compileKeymap();
    store.events.on("keymap", () => compileKeymap());

    this.titlebar = new TitleBar(() => void this.requestClose());
    this.strip = new TabStrip({
      select: (id) => this.activateId(id),
      close: (id) => void this.closeTab(this.tabs.find((t) => t.id === id)!),
      reorder: (id, i) => this.moveTab(id, i),
      newTab: () => void this.newTab(),
      shellMenu: (a) => this.shellMenu(a),
      toggleSessions: () => this.toggleSidebar(),
      toggleBrowser: () => this.toggleBrowser(),
      settings: () => openSettings(),
      contextMenu: (t, e) => this.tabMenu(t, e.clientX, e.clientY),
      uiZoom: (d) => this.uiZoom(d),
    });
    this.status = new StatusBar();
    this.sidebar = new Sidebar();
    this.browser = new BrowserPanel();
    this.panesHost = h("div", { class: "panes" });

    const sidebarEl = h("aside", { id: "sidebar", class: "sidebar" }, this.sidebar.el);
    const browserEl = h("aside", { id: "browser", class: "browser-panel" }, this.browser.el);
    const main = h("main", { class: "main" }, this.strip.el, this.panesHost, this.status.el);
    const sidebarSplit = h("div", { class: "panel-splitter left" });
    const browserSplit = h("div", { class: "panel-splitter right" });
    const body = h("div", { class: "body" }, sidebarEl, sidebarSplit, main, browserSplit, browserEl);
    root.append(
      this.titlebar.el, body,
      h("div", { id: "banner-root" }), h("div", { id: "toast-root" }), h("div", { id: "modal-root" }),
    );
    this.wireSplitter(sidebarSplit, sidebarEl, "left", 160, 900, () => this.sidebarWidth, (w) => (this.sidebarWidth = w), () => this.sidebarOpen);
    this.wireSplitter(browserSplit, browserEl, "right", 250, 1600, () => this.browserWidth, (w) => (this.browserWidth = w), () => this.browserOpen);

    await initPaneEvents();
    this.wireBackendEvents();
    this.wireSettings();
    this.wireKeyboard();
    initDropUi();
    // Airspace (§17.1): the browser webview paints above ours, so hide it while a modal is open.
    modalState.on("open", ({ open }) => void ipc.browserHide(open).catch(() => {}));

    for (const m of this.info.startup ?? []) showBanner(m.text, m.kind as never);
    if (store.loadError) this.settingsErrorBanner();
    if (store.sessions.banner) showBanner(store.sessions.banner, "warning", [{ label: "OK", run: () => void ipc.sessionsClearBanner() }]);

    await this.refreshBackground();
    await this.restoreState();
    this.applyPanels();
    this.restored = true;
    window.ut = {
      scrollLines: (n) => this.active?.focused?.scrollLines(n),
      scrollPages: (n) => this.active?.focused?.scrollPages(n),
      scrollToTop: () => this.active?.focused?.scrollToTop(),
      scrollToBottom: () => this.active?.focused?.scrollToBottom(),
    };
    window.addEventListener("beforeunload", () => void this.saveState(true));
    await ipc.appReady();
    // Lazy restore (§16.3 [P1]): only the active tab started; the rest start 2 s later in the background.
    setTimeout(() => void this.startBackgroundTabs(), 2000);
    await this.handleStartArgs(this.info.args);
  }

  private async startBackgroundTabs() {
    for (const t of this.tabs) {
      if (t !== this.active) await t.startAll().catch(() => {});
      await sleep(50);
    }
  }

  private settingsErrorBanner() {
    showBanner(`settings.json could not be read (${store.loadError}). Defaults are in use and the file was not changed.`, "error", [
      { label: "Open file", run: () => void ipc.settingsOpenFile() },
      { label: "Reset (backup created)", run: () => void ipc.settingsResetWithBackup().then(applyBundle) },
    ]);
  }

  // ============================================================ DOM plumbing
  private wireSplitter(split: HTMLElement, panel: HTMLElement, side: "left" | "right", min: number, max: number, get: () => number, set: (w: number) => void, open: () => boolean) {
    split.addEventListener("pointerdown", (e) => {
      if (!open()) return;
      e.preventDefault();
      split.setPointerCapture(e.pointerId);
      const x0 = e.clientX;
      const w0 = get();
      const move = (ev: PointerEvent) => {
        const dx = side === "left" ? ev.clientX - x0 : x0 - ev.clientX;
        panel.style.width = `${clamp(w0 + dx, min, max)}px`;
        this.browser.syncBounds();
      };
      const up = (ev: PointerEvent) => {
        split.removeEventListener("pointermove", move);
        split.removeEventListener("pointerup", up);
        const dx = side === "left" ? ev.clientX - x0 : x0 - ev.clientX;
        const w = w0 + dx;
        if (w >= min && w <= max) set(w); // only widths inside the limits are saved (§7.2)
        panel.style.width = `${get()}px`;
        this.browser.syncBounds();
        this.markDirty();
      };
      split.addEventListener("pointermove", move);
      split.addEventListener("pointerup", up);
    });
  }

  applyPanels() {
    const sb = document.getElementById("sidebar")!;
    const br = document.getElementById("browser")!;
    sb.style.width = this.sidebarOpen ? `${this.sidebarWidth}px` : "0px";
    sb.classList.toggle("collapsed", !this.sidebarOpen);
    br.style.width = this.browserOpen ? `${this.browserWidth}px` : "0px";
    br.classList.toggle("collapsed", !this.browserOpen);
    document.querySelector(".panel-splitter.left")?.classList.toggle("hidden", !this.sidebarOpen);
    document.querySelector(".panel-splitter.right")?.classList.toggle("hidden", !this.browserOpen);
    this.browser.setOpen(this.browserOpen);
  }

  toggleSidebar(open = !this.sidebarOpen) {
    this.sidebarOpen = open;
    this.applyPanels();
    this.markDirty();
    if (open) this.sidebar.focusSearch?.();
    else this.active?.focused?.focus();
  }

  toggleBrowser(open = !this.browserOpen) {
    this.browserOpen = open;
    this.applyPanels();
    this.markDirty();
    void ipc.browserToggle(open).catch(() => {});
  }

  // ======================================================== backend events
  private wireBackendEvents() {
    void listen("app:close-requested", () => void this.requestClose());
    void listen("app:focus", ({ focused }) => {
      this.windowFocused = focused;
      if (focused) {
        this.bursts.clear();
        this.active?.focused?.doFit();
        this.active?.focused?.focus();
      }
    });
    void listen("app:scroll", ({ action }) => {
      const p = this.active?.focused;
      if (!p) return;
      ({ lineUp: () => p.scrollLines(-1), lineDown: () => p.scrollLines(1), pageUp: () => p.scrollPages(-1), pageDown: () => p.scrollPages(1), top: () => p.scrollToTop(), bottom: () => p.scrollToBottom() })[action]?.();
    });
    void listen("app:quake-toggle", () => this.active?.focused?.focus());
    void listen("app:second-instance", ({ args }) => void this.handleStartArgs(args as never));
    void listen("app:banner", (b) => showBanner(b.text, b.kind));
    void listen("app:action" as never, ((p: { action: string }) => this.runAction(p.action)) as never);
    void listen("sessions:changed", () => this.events.emit("tabs", null));
  }

  private async handleStartArgs(a?: { cwd?: string | null; session?: string | null; workspace?: string | null; command?: string[] }) {
    if (!a) return;
    if (a.session) {
      const s = store.sessions.sessions.find((x) => x.id === a.session || x.name.toLowerCase() === a.session!.toLowerCase());
      if (s) return void this.openSession(s);
    }
    if (a.workspace) {
      const w = store.workspaces.find((x) => x.id === a.workspace || x.name.toLowerCase() === a.workspace!.toLowerCase());
      if (w) return void this.openWorkspace(w, "add");
    }
    if (a.command?.length) {
      const cmd = a.command.map((x) => (/\s/.test(x) ? `"${x}"` : x)).join(" ");
      return void this.openCommand(cmd, { title: a.command[0], cwd: a.cwd ?? undefined });
    }
    if (a.cwd) await this.newTab({ cwd: a.cwd });
  }

  private wireSettings() {
    store.events.on("theme", () => { for (const p of panes.values()) p.applyTheme(); });
    store.events.on("settings", ({ settings, prev }) => {
      if (prev) {
        const a = prev.terminal.backgroundImage;
        const b = settings.terminal.backgroundImage;
        if (a.path !== b.path) void this.refreshBackground();
        else if (a.opacity !== b.opacity) for (const p of panes.values()) p.setBackgroundOpacity(b.opacity);
        if (prev.terminal.renderer !== settings.terminal.renderer && settings.terminal.renderer === "dom") renderer.releaseAll();
        if (prev.ui.scale !== settings.ui.scale) requestAnimationFrame(() => this.refitAll());
      }
      for (const p of panes.values()) p.applySettings();
      if (store.loadError) this.settingsErrorBanner();
    });
  }

  async refreshBackground() {
    const url = store.settings.terminal.backgroundImage.path ? await ipc.backgroundUrl().catch(() => null) : null;
    store.bgUrl = url;
    for (const p of panes.values()) await p.setBackground(url);
  }

  private refitAll() {
    for (const p of panes.values()) p.doFit();
  }

  // ===================================================================== tabs
  private tabHooks: TabHooks = {
    changed: (t, what) => {
      this.strip.update(t);
      if (t === this.active) this.refreshChrome();
      if (what === "layout" || what === "title" || what === "cwd" || what === "flags" || what === "spawned") this.markDirty();
      if (what === "spawned" || what === "exit") this.events.emit("tabs", null);
    },
    contextMenu: (t, p, e) => this.paneMenu(t, p, e),
    zoom: (_t, p, d) => this.zoomFont(p, d),
    input: (t, p, bytes) => {
      // Broadcast: raw bytes verbatim to every OTHER pane of the tab, skipping read-only ones (§7.4).
      if (!t.broadcast) return;
      for (const o of t.allPanes()) if (o !== p && !o.readOnly && !o.exited && o.started) void ipc.ptyWrite(o.id, bytes).catch(() => {});
    },
    bell: (t, p) => this.bell(t, p),
    shell: (t, p, kind, code, dur) => {
      if (kind === "commandEnd") this.commandFinished(t, p, code, dur);
      if (p === this.active?.focused) {
        this.refreshForeground(p);
        this.status.update(this.active);
      }
    },
    output: (t, p) => this.outputBurst(t, p),
    empty: (t) => void this.removeTab(t),
    searchAll: (q, o) => { for (const t of this.tabs) for (const p of t.allPanes()) p.searchQuery(q, o); },
  };

  async newTab(init: Partial<TabInit> & { profile?: ShellProfile } = {}): Promise<Tab | null> {
    const prof = init.profile ?? defaultProfile();
    const command = init.command ?? prof?.command ?? "cmd.exe";
    return this.addTab({
      title: "Terminal",
      color: prof?.color ?? null,
      command,
      launch: init.launch ?? { command },
      cwd: store.info.homeDir,
      ...init,
    });
  }

  addTab(init: TabInit, activate = true): Tab {
    const tab = new Tab(init, this.tabHooks);
    // Pinned tabs live first; new tabs go after them.
    this.tabs.push(tab);
    this.panesHost.append(tab.el);
    if (activate) this.activate(tab);
    else this.strip.render(this.tabs, this.active?.id ?? "");
    this.events.emit("tabs", null);
    this.markDirty();
    return tab;
  }

  activate(tab: Tab) {
    if (this.active === tab) return this.active.focused?.focus();
    this.active?.hide();
    this.active = tab;
    tab.show();
    this.strip.render(this.tabs, tab.id);
    this.refreshChrome();
    this.events.emit("active", tab);
    this.markDirty();
  }

  activateId(id: string) {
    const t = this.tabs.find((x) => x.id === id);
    if (t) this.activate(t);
  }

  private refreshChrome() {
    const t = this.active;
    this.status.update(t);
    const p = t?.focused;
    this.titlebar.setTitle(p?.cwd ?? t?.cwd ?? null, !!this.info?.elevated);
  }

  /** Foreground process name for the status bar: event-driven, re-armed only while a command runs (§7.5). */
  private refreshForeground(p: TerminalPane) {
    clearTimeout(this.fgTimer);
    if (!p.info || p.exited) return;
    void ipc.paneInfo(p.id, true).then((i) => {
      if (!i) return;
      p.foreground = i.foreground ?? null;
      if (p === this.active?.focused) this.status.update(this.active);
      if (p.phase === "running" && p === this.active?.focused) this.fgTimer = setTimeout(() => this.refreshForeground(p), 2000);
    }).catch(() => {});
  }

  moveTab(id: string, toIndex: number) {
    const i = this.tabs.findIndex((t) => t.id === id);
    if (i < 0) return;
    const [t] = this.tabs.splice(i, 1);
    this.tabs.splice(clamp(toIndex, 0, this.tabs.length), 0, t);
    this.strip.render(this.tabs, this.active?.id ?? "");
    this.events.emit("tabs", null);
    this.markDirty();
  }

  /** Confirm only when something is running (§7.9). */
  private async confirmClose(running: number, what: string): Promise<boolean> {
    const mode = store.settings.processes.closeConfirm;
    if (mode === "never" || (mode === "whenRunning" && running === 0)) return true;
    const msg = running === 0 ? `Close ${what}?` : `${running} terminal session${running === 1 ? " is" : "s are"} still running. Close anyway?`;
    return confirmDialog(msg, { title: "Close", ok: "Close", danger: true, defaultCancel: true });
  }

  async closeTab(tab: Tab | undefined, force = false) {
    if (!tab) return;
    if (!force && tab.paneIds.length > 1 && !(await this.confirmClose(await tab.runningCount(), "this tab"))) return;
    await this.removeTab(tab);
  }

  private async removeTab(tab: Tab) {
    const i = this.tabs.indexOf(tab);
    if (i < 0) return;
    this.tabs.splice(i, 1);
    tab.dispose();
    if (this.active === tab) {
      this.active = null;
      const next = this.tabs[Math.min(i, this.tabs.length - 1)];
      if (next) this.activate(next);
      else this.events.emit("active", null);
    }
    this.strip.render(this.tabs, this.active?.id ?? "");
    this.refreshChrome();
    this.events.emit("tabs", null);
    this.markDirty();
    // Closing the last tab closes the window; the saved state has `tabs: []` so the next launch opens a default tab.
    if (this.tabs.length === 0 && !this.closing && !this.replacing) await this.quit();
  }

  async closeOthers(keep: Tab) {
    const others = this.tabs.filter((t) => t !== keep && !t.pinned);
    let running = 0;
    for (const t of others) running += await t.runningCount();
    if (!(await this.confirmClose(running, `${others.length} tab${others.length === 1 ? "" : "s"}`))) return;
    for (const t of others) await this.removeTab(t);
  }

  async closeRight(of: Tab) {
    const others = this.tabs.slice(this.tabs.indexOf(of) + 1).filter((t) => !t.pinned);
    let running = 0;
    for (const t of others) running += await t.runningCount();
    if (!(await this.confirmClose(running, `${others.length} tab${others.length === 1 ? "" : "s"}`))) return;
    for (const t of others) await this.removeTab(t);
  }

  private liveCwdOrOriginal(t: Tab) {
    return t.liveCwd() ?? t.cwd;
  }

  duplicate(t: Tab) {
    // Copies EVERYTHING about the source: environment, theme and font overrides, starting command, session id.
    return this.addTab({
      title: t.title, titleLocked: t.titleLocked, color: t.color, pinned: false, group: t.group, readOnly: t.readOnly,
      command: t.command, launch: t.launch, cwd: this.liveCwdOrOriginal(t), startingCommand: t.startingCommand,
      sessionId: t.sessionId, env: t.env, fontSize: t.fontSize, themeBackground: t.themeBackground, integration: t.integration,
    });
  }

  togglePin(t: Tab) {
    t.pinned = !t.pinned;
    // Pinning moves the tab to just after the existing pinned tabs.
    const rest = this.tabs.filter((x) => x !== t);
    const idx = rest.filter((x) => x.pinned).length;
    rest.splice(t.pinned ? idx : idx, 0, t);
    this.tabs = rest;
    this.strip.render(this.tabs, this.active?.id ?? "");
    this.markDirty();
  }

  async rename(t: Tab) {
    const v = await promptDialog({ title: "Rename tab", value: t.title, allowEmpty: true });
    if (v === undefined) return;
    if (v === "") {
      t.titleLocked = false; // empty unlocks the title again [P1]
      t.title = t.focused?.title || "Terminal";
    } else {
      t.title = v;
      t.titleLocked = true;
    }
    this.strip.update(t);
    this.refreshChrome();
    this.markDirty();
  }

  async setGroup(t: Tab) {
    const v = await promptDialog({ title: "Set tab group", label: "Group label (leave empty to clear)", value: t.group, allowEmpty: true });
    if (v === undefined) return;
    t.group = v;
    this.strip.update(t);
    this.markDirty();
  }

  setColor(t: Tab, c: string | null) {
    t.color = c;
    this.strip.update(t);
    this.markDirty();
  }

  // ----------------------------------------------------------- launching
  openCommand(command: string, o: Partial<TabInit> = {}) {
    return this.addTab({ title: o.title ?? exeStem(command), color: "#6be5ff", command, launch: { command }, cwd: store.info.homeDir, ...o });
  }

  async openProfile(p: ShellProfile, askName = true) {
    let title = "Terminal";
    let locked = false;
    if (askName) {
      const n = await promptDialog({ title: "New tab", label: "Tab name", value: funnyName(), allowEmpty: true });
      if (n === undefined) return;
      if (n) (title = n), (locked = true);
    }
    return this.addTab({ title, titleLocked: locked, color: p.color, command: p.command, launch: { command: p.command }, cwd: store.info.homeDir });
  }

  /** Every launch path that comes from a session sets `sessionId` on the tab (live dots, workspaces, restore). */
  async openSession(s: Session, admin = false) {
    if (admin && !this.info.elevated) {
      try {
        await ipc.runElevated(s.id);
        toast("Opened in a separate elevated console. The starting command, environment, theme and shell integration are not applied.", "info", 6000);
      } catch (e) {
        toast(String(e), "error");
      }
      return null;
    }
    return this.addTab({
      title: s.name, titleLocked: true, color: s.colorTag || null, command: sessionCommand(s),
      launch: { sessionId: s.id }, sessionId: s.id, startingCommand: undefined,
      fontSize: s.fontSize || undefined, themeBackground: s.themeBackground || undefined, integration: s.integration,
    });
  }

  liveSessionIds(): Set<string> {
    return new Set(this.tabs.map((t) => t.sessionId).filter((x): x is string => !!x));
  }

  async openWorkspace(w: Workspace, how?: "replace" | "add") {
    if (!how) {
      how = this.tabs.length === 0 ? "add" : ((await choiceDialog("Open workspace", `Open "${w.name}" by replacing the current tabs or adding to this window?`, [
        { label: "Cancel", value: "cancel" }, { label: "Add to window", value: "add" }, { label: "Replace current tabs", value: "replace", kind: "primary" },
      ], "add")) as "replace" | "add" | "cancel" | undefined) as never;
      if (!how || (how as string) === "cancel") return;
    }
    if (how === "replace") {
      let running = 0;
      for (const t of this.tabs) running += await t.runningCount();
      if (!(await this.confirmClose(running, "the current tabs"))) return;
      this.replacing = true; // do not quit when the last tab goes away
      for (const t of [...this.tabs]) await this.removeTab(t);
      this.replacing = false;
    }
    for (const wt of w.tabs) {
      const s = wt.sessionId ? store.sessions.sessions.find((x) => x.id === wt.sessionId) : undefined;
      const init: TabInit = s
        ? { title: wt.title || s.name, titleLocked: true, color: wt.color ?? s.colorTag, command: wt.command || sessionCommand(s), launch: { sessionId: s.id }, sessionId: s.id, cwd: wt.cwd ?? undefined, fontSize: s.fontSize || undefined, themeBackground: s.themeBackground || undefined, integration: s.integration, layout: wt.layout as Layout | undefined }
        : { title: wt.title, titleLocked: true, color: wt.color ?? null, command: wt.command, launch: { command: wt.command }, cwd: wt.cwd ?? undefined, startingCommand: wt.startingCommand ?? undefined, layout: wt.layout as Layout | undefined };
      this.addTab(init, false);
    }
    const first = this.tabs[this.tabs.length - w.tabs.length];
    if (first) this.activate(first);
  }

  async saveWorkspace() {
    const name = await promptDialog({ title: "Save current tabs as workspace", label: "Workspace name", value: "My Workspace" });
    if (!name) return;
    await ipc.workspaceSave({
      name,
      tabs: this.tabs.map((t) => ({
        sessionId: t.sessionId ?? null, title: t.title, command: t.command, cwd: this.liveCwdOrOriginal(t) ?? null,
        startingCommand: t.startingCommand ?? null, color: t.color, layout: t.serializeLayout(),
      })),
    });
    toast(`Workspace "${name}" saved.`, "success");
  }

  runSnippet(s: Snippet) {
    const p = this.active?.focused;
    if (!p) return toast("No terminal is focused.", "warning");
    // With broadcast on, the onInput hook fans the same bytes out to the other targets.
    p.paste(s.command, s.appendEnter);
    p.focus();
  }

  // ============================================================== menus
  async shellMenu(anchor: HTMLElement) {
    const r = anchor.getBoundingClientRect();
    const shells = store.shells;
    showMenu(r.left, r.bottom + 2, [
      ...shells.map<MenuItem>((s) => ({ label: s.name, swatch: s.color, action: () => void this.openProfile(s) })),
      { separator: true },
      { label: "Refresh shells", action: () => void refreshShells(true) },
    ]);
    void refreshShells(false); // refresh in the background; cached for 60 s
  }

  private tabMenu(t: Tab, x: number, y: number) {
    // Rebuilt on every open so labels never go stale (§7.3, §24 #30).
    const multi = t.paneIds.length > 1;
    const colorItems: MenuItem[] = [
      { label: "None", swatch: "", checked: !t.color, action: () => this.setColor(t, null) },
      ...TAB_COLORS.map((c) => ({ label: c, swatch: c, checked: t.color === c, action: () => this.setColor(t, c) })),
      { label: "Custom…", action: () => {
        const inp = h("input", { type: "color", value: t.color ?? "#00e5ff", style: "position:fixed;opacity:0;pointer-events:none" });
        document.body.append(inp);
        inp.addEventListener("change", () => { this.setColor(t, inp.value); inp.remove(); });
        inp.addEventListener("blur", () => inp.remove());
        inp.click();
      } },
    ];
    showMenu(x, y, [
      { label: t.pinned ? "Unpin" : "Pin", action: () => this.togglePin(t) },
      { label: "Rename", action: () => void this.rename(t) },
      { label: "Tab Color", submenu: colorItems },
      { separator: true },
      { label: "Split Right", action: () => t.split("right") },
      { label: "Split Down", action: () => t.split("down") },
      { label: "Unsplit All", disabled: !multi, action: () => t.unsplitAll() },
      { label: t.broadcast ? "Broadcast Input: on" : "Broadcast Input: off", checked: t.broadcast, action: () => t.setBroadcast(!t.broadcast) },
      { label: t.logging ? "Stop Logging" : "Start Logging", action: () => void this.toggleLogging(t) },
      { label: t.recording ? "Stop Recording (.cast)" : "Start Recording (.cast)", action: () => void this.toggleRecording(t) },
      { label: t.readOnly ? "Read-Only: on" : "Read-Only: off", checked: t.readOnly, action: () => t.setReadOnly(!t.readOnly) },
      { label: "Set Tab Group…", action: () => void this.setGroup(t) },
      { separator: true },
      { label: "Duplicate Tab", action: () => this.duplicate(t) },
      { label: "Save as Session…", action: () => void this.saveAsSession(t) },
      { separator: true },
      { label: "Close Tab", action: () => void this.closeTab(t, true) },
      { label: "Close Other Tabs", disabled: this.tabs.length < 2, action: () => void this.closeOthers(t) },
      { label: "Close Tabs to the Right", disabled: this.tabs.indexOf(t) >= this.tabs.length - 1, action: () => void this.closeRight(t) },
    ]);
  }

  private paneMenu(t: Tab, p: TerminalPane, e: MouseEvent) {
    t.focusPane(p.id, false);
    showMenu(e.clientX, e.clientY, [
      { label: "Copy", disabled: !p.hasSelection(), hint: "Ctrl+Shift+C", action: () => void p.copySelection() },
      { label: "Paste", hint: "Ctrl+V", action: () => void p.pasteFromClipboard() },
      { label: "Select All", action: () => p.selectAll() },
      { label: "Clear", action: () => p.clear() },
      { label: "Search", hint: "Ctrl+Shift+F", action: () => p.openSearch() },
      { label: "Save Output…", hint: "Ctrl+Shift+S", action: () => void this.exportBuffer(p) },
      { separator: true },
      { label: "Split Right", action: () => t.split("right", p.id) },
      { label: "Split Down", action: () => t.split("down", p.id) },
    ]);
  }

  async saveAsSession(t: Tab) {
    const name = await promptDialog({ title: "Save as session", label: "Session name", value: t.title });
    if (!name) return;
    const s = await ipc.sessionAdd({
      name, shellPath: t.command, arguments: "", workingDirectory: this.liveCwdOrOriginal(t) ?? "",
      startingCommand: t.startingCommand ?? "", colorTag: t.color ?? "#00ff44",
    });
    toast(`Saved session "${s.name}".`, "success");
  }

  // ==================================================== tab-level features
  async toggleLogging(t: Tab) {
    const want = !t.logging;
    const ps = t.allPanes().filter((p) => p.started && !p.exited);
    for (const [i, p] of ps.entries()) {
      const st = await ipc.logToggle(p.id, t.title, ps.length > 1 ? `[${i + 1}] ` : undefined).catch(() => null);
      if (st && st.active !== want) await ipc.logToggle(p.id, t.title); // keep every pane consistent
      p.setFlags({ logging: want });
      if (st?.path && want && i === 0) toast(`Logging to ${st.path}`, "info", 4000);
    }
    t.logging = want;
    this.strip.update(t);
  }

  async toggleRecording(t: Tab) {
    const p = t.focused;
    if (!p || !p.started) return;
    const st = await ipc.recordToggle(p.id, t.title).catch((e) => (toast(String(e), "error"), null));
    if (!st) return;
    t.recording = st.active;
    p.setFlags({ recording: st.active });
    this.strip.update(t);
    toast(st.active ? `Recording to ${st.path}` : `Saved ${st.path}`, "info", 4000);
  }

  async exportBuffer(p = this.active?.focused) {
    if (!p) return;
    const path = await ipc.exportBuffer(p.bufferText());
    if (path) toast(`Saved ${path}`, "success");
  }

  // ============================================================ zoom
  zoomFont(p: TerminalPane, dir: 1 | -1 | 0) {
    const cur = p.fontOverride >= 8 ? p.fontOverride : store.settings.terminal.fontSize;
    const next = dir === 0 ? 14 : clamp(cur + dir, 8, 32);
    if (next === cur) return;
    if (store.settings.terminal.zoomScope === "pane" || p.fontOverride >= 8) {
      p.fontOverride = next;
      p.setFontSize(next);
      return;
    }
    // Global: every pane without a session font override follows; only `{fontSize}` is ever sent (§5.9).
    store.settings.terminal.fontSize = next;
    for (const q of panes.values()) if (q.fontOverride < 8) q.setFontSize(next);
    persistFontSize(next);
  }

  uiZoom(dir: 1 | -1) {
    const s = clamp(Math.round((store.settings.ui.scale + dir * 0.05) * 20) / 20, 0.75, 2);
    if (s !== store.settings.ui.scale) void patchSettings({ ui: { scale: s } });
  }

  // =========================================================== notifications
  private bell(t: Tab, p: TerminalPane) {
    const b = store.settings.terminal.bell;
    if (!(t === this.active)) t.activity = true;
    this.strip.update(t);
    if (b.visual) {
      p.el.classList.add("bell");
      setTimeout(() => p.el.classList.remove("bell"), 150);
    }
    if (b.audible) beep();
    if (b.flashTaskbar && !this.windowFocused) void ipc.notifyAttention(p.id, "flash").catch(() => {});
  }

  private commandFinished(t: Tab, p: TerminalPane, code: number | null, dur: number | null) {
    const cfg = store.settings.notifications.commandFinished;
    if (!cfg.enabled || dur === null || dur < cfg.minDurationSec * 1000) return;
    if (this.windowFocused && t === this.active) return;
    void ipc.notifyAttention(p.id, "flash").catch(() => {});
    if (cfg.toast) {
      const body = code === 0 || code === null ? `✔ Command finished in '${t.title}' after ${fmt(dur)}` : `✖ Command in '${t.title}' exited with ${code} after ${fmt(dur)}`;
      void ipc.notifyAttention(p.id, "toast", "Useless Terminal", body).catch(() => {});
    }
    if (t !== this.active) {
      t.activity = true;
      this.strip.update(t);
    }
  }

  /** Fallback without integration (§12.3): ≥3 bursts then 2.5 s of quiet while unfocused → flash once. */
  private outputBurst(_t: Tab, p: TerminalPane) {
    if (this.windowFocused || p.info?.injected || !store.settings.notifications.commandFinished.enabled) return;
    let b = this.bursts.get(p.id);
    if (!b) this.bursts.set(p.id, (b = { n: 0 }));
    b.n++;
    clearTimeout(b.timer);
    b.timer = setTimeout(() => {
      if (b!.n >= 3 && !this.windowFocused) void ipc.notifyAttention(p.id, "flash").catch(() => {});
      b!.n = 0;
    }, 2500);
  }

  // ============================================================= keyboard
  private wireKeyboard() {
    window.addEventListener("keydown", (e) => {
      const el = e.target as HTMLElement | null;
      const inField = el && /^(INPUT|TEXTAREA|SELECT)$/.test(el.tagName) && !el.closest(".xterm");
      if (inField || document.querySelector("#modal-root > .modal-backdrop") || document.body.classList.contains("recording-key")) return;
      const action = matchAction(e);
      if (!action) return;
      if (this.runAction(action, e)) {
        e.preventDefault();
        e.stopPropagation();
      }
    }, true);
  }

  /** Returns whether the key was consumed (else it passes through to the shell). */
  runAction(action: string, e?: KeyboardEvent): boolean {
    const t = this.active;
    const p = t?.focused;
    const n = /^selectTab(\d)$/.exec(action);
    if (n) { const tab = this.tabs[+n[1] - 1]; if (tab) this.activate(tab); return !!tab; }
    const np = /^selectTabNumpad(\d)$/.exec(action);
    if (np) { const tab = this.tabs[np[1] === "0" ? 9 : +np[1] - 1]; if (tab) this.activate(tab); return !!tab; }
    switch (action) {
      case "newTab": void this.newTab(); return true;
      case "closePane": {
        if (!t || !p) return true;
        if (t.pinned && t.paneIds.length === 1) return true; // Ctrl+W does nothing on a pinned single-pane tab
        if (t.paneIds.length === 1) void this.closeTab(t, true);
        else t.closePane(p.id);
        return true;
      }
      case "togglePanel": this.toggleSidebar(); return true;
      case "toggleBrowser": this.toggleBrowser(); return true;
      case "sendToBrowser": void sendSelectionToBrowser(); return true;
      case "settings": openSettings(); return true;
      case "nextTab": case "prevTab": {
        if (!t || this.tabs.length < 2) return true;
        const i = this.tabs.indexOf(t) + (action === "nextTab" ? 1 : -1);
        this.activate(this.tabs[(i + this.tabs.length) % this.tabs.length]);
        return true;
      }
      case "newSession": this.sidebar.newSession(); return true;
      case "duplicateTab": if (t) this.duplicate(t); return true;
      case "commandPalette": openPalette(); return true;
      case "quickConnect": openQuickConnect(); return true;
      case "movePaneFocus": {
        if (!t || !e) return false;
        const dir = ({ ArrowLeft: "left", ArrowRight: "right", ArrowUp: "up", ArrowDown: "down" } as const)[e.key as "ArrowLeft"];
        return !!dir && t.moveFocus(dir); // passes through when no pane that way (PSReadLine word selection)
      }
      case "prevCommand": p?.navigate(-1); return true;
      case "nextCommand": p?.navigate(1); return true;
      case "search": p?.openSearch(); return !!p;
      case "exportBuffer": void this.exportBuffer(); return true;
      case "copy": {
        if (!p) return false;
        const plainCtrlC = e && e.ctrlKey && !e.shiftKey && e.key.toLowerCase() === "c";
        if (p.hasSelection()) { void p.copySelection(); return true; }
        return !plainCtrlC; // Ctrl+Shift+C is consumed either way; plain Ctrl+C reaches the shell as ^C
      }
      case "paste": if (!p) return false; void p.pasteFromClipboard(); return true;
      case "splitRight": t?.split("right"); return true;
      case "splitDown": t?.split("down"); return true;
      case "zoomIn": if (p) this.zoomFont(p, 1); return true;
      case "zoomOut": if (p) this.zoomFont(p, -1); return true;
      case "zoomReset": if (p) this.zoomFont(p, 0); return true;
      case "scrollPageUp": p?.scrollPages(-1); return !!p;
      case "scrollPageDown": p?.scrollPages(1); return !!p;
      // palette-only commands
      case "unsplitAll": t?.unsplitAll(); return true;
      case "addPane": t?.addPane(); return true;
      case "renameTab": if (t) void this.rename(t); return true;
      case "pinTab": if (t) this.togglePin(t); return true;
      case "broadcastToggle": if (t) t.setBroadcast(!t.broadcast); return true;
      case "closeOthers": if (t) void this.closeOthers(t); return true;
      case "closeRight": if (t) void this.closeRight(t); return true;
      case "quake": void appWindow().then((w) => w?.close()); return true;
      case "toggleLog": if (t) void this.toggleLogging(t); return true;
      case "toggleReadOnly": if (t) t.setReadOnly(!t.readOnly); return true;
      case "toggleRecording": if (t) void this.toggleRecording(t); return true;
      case "toggleCrt": void patchSettings({ terminal: { crt: !store.settings.terminal.crt } }); return true;
      case "toggleMinimap": void patchSettings({ terminal: { minimap: !store.settings.terminal.minimap } }); return true;
      case "findAllTabs": p?.openSearch(); return true;
      case "saveWorkspace": void this.saveWorkspace(); return true;
      default: return false;
    }
  }

  // ========================================================== window state
  markDirty() {
    if (!this.restored || this.closing || this.saveTimer) return;
    // Autosave every 15 s, only when something changed (so a force-kill still leaves a usable session).
    this.saveTimer = setTimeout(() => {
      this.saveTimer = undefined;
      void this.saveState(false);
    }, 15000);
  }

  serialize() {
    return {
      schemaVersion: 3,
      sidebarOpen: this.sidebarOpen, sidebarWidth: Math.round(this.sidebarWidth),
      browserOpen: this.browserOpen, browserWidth: Math.round(this.browserWidth),
      activeTabIndex: Math.max(0, this.tabs.findIndex((t) => t === this.active)),
      tabs: this.tabs.map((t) => ({
        title: t.title, titleLocked: t.titleLocked, command: t.command, sessionId: t.sessionId ?? null,
        cwd: this.liveCwdOrOriginal(t) ?? null, startingCommand: t.startingCommand ?? null, color: t.color,
        pinned: t.pinned, group: t.group || null, readOnly: t.readOnly, layout: t.serializeLayout(),
      })),
    };
  }

  async saveState(force: boolean) {
    // Never before restore finished, never after close started (§16.3).
    if (!this.restored || (this.closing && !force)) return;
    const s = this.serialize();
    const json = JSON.stringify(s);
    if (!force && json === this.lastSaved) return;
    this.lastSaved = json;
    await ipc.windowStateSave(s).catch(() => {});
  }

  private async restoreState() {
    type WS = { sidebarOpen?: boolean; sidebarWidth?: number; browserOpen?: boolean; browserWidth?: number; activeTabIndex?: number; tabs?: Record<string, unknown>[] };
    let ws = null as WS | null;
    try { ws = (await ipc.windowStateLoad()) as WS | null; } catch { /* start fresh */ }
    if (ws) {
      this.sidebarOpen = ws.sidebarOpen ?? true;
      this.sidebarWidth = ws.sidebarWidth ?? 260;
      this.browserOpen = ws.browserOpen ?? false;
      this.browserWidth = ws.browserWidth ?? 500;
    }
    const startup = store.settings.startup;
    let made = 0;
    if (startup.mode === "workspace" && startup.workspaceId) {
      const w = store.workspaces.find((x) => x.id === startup.workspaceId);
      if (w) { await this.openWorkspace(w, "add"); made = this.tabs.length; }
    } else if (startup.mode !== "defaultTab") {
      // Restore each tab inside its own try/catch: one bad tab must not drop the others (§16.3, §23.24).
      for (const ts of ws?.tabs ?? []) {
        try {
          const sid = ts.sessionId as string | null;
          const s = sid ? store.sessions.sessions.find((x) => x.id === sid) : undefined;
          const command = (ts.command as string) || (s ? sessionCommand(s) : "");
          if (!command && !s) continue;
          this.addTab({
            title: (ts.title as string) || "Terminal", titleLocked: !!ts.titleLocked, color: (ts.color as string | null) ?? null,
            pinned: !!ts.pinned, group: (ts.group as string) ?? "", readOnly: !!ts.readOnly, command,
            launch: s ? { sessionId: s.id } : { command }, sessionId: s?.id, cwd: (ts.cwd as string) ?? undefined,
            startingCommand: s ? undefined : ((ts.startingCommand as string) ?? undefined),
            fontSize: s?.fontSize || undefined, themeBackground: s?.themeBackground || undefined, integration: s?.integration,
            layout: (ts.layout as Layout) ?? null,
          }, false);
          made++;
        } catch (e) {
          console.warn("could not restore a tab", e);
        }
      }
      const idx = clamp(ws?.activeTabIndex ?? 0, 0, Math.max(0, this.tabs.length - 1));
      if (this.tabs[idx]) this.activate(this.tabs[idx]);
    }
    if (made === 0 || this.tabs.length === 0) await this.newTab();
    else if (!this.active) this.activate(this.tabs[0]);
  }

  // ================================================================ closing
  async requestClose() {
    if (this.closing) return;
    let running = 0;
    for (const t of this.tabs) running += await t.runningCount();
    if (!(await this.confirmClose(running, "the window"))) return;
    await this.quit();
  }

  private async quit() {
    clearTimeout(this.saveTimer);
    // Save BEFORE disposing panes, otherwise the tab list would be empty (§16.3). A last tab closed by the
    // user already left `tabs: []`, so the next launch opens one default tab.
    await this.saveState(true);
    this.closing = true;
    await ipc.windowQuit().catch(() => {});
  }
}

let audio: AudioContext | null = null;
function beep() {
  try {
    audio ??= new AudioContext();
    const o = audio.createOscillator();
    const g = audio.createGain();
    o.frequency.value = 880;
    g.gain.value = 0.05;
    o.connect(g).connect(audio.destination);
    o.start();
    o.stop(audio.currentTime + 0.08);
  } catch { /* no audio */ }
}

export const app = new App();
export { isTauri, debounce };

declare global {
  interface Window {
    ut: { scrollLines(n: number): void; scrollPages(n: number): void; scrollToTop(): void; scrollToBottom(): void };
  }
}
