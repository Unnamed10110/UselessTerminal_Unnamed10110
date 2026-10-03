// One terminal pane: an xterm.js instance plus its PTY session. All hot paths (output writes, decorations,
// repair) stay out of any framework reactivity; this file only touches the DOM it owns.

import { Terminal, type IMarker, type IDecoration, type IDisposable } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";
import { FitAddon } from "@xterm/addon-fit";
import { WebglAddon } from "@xterm/addon-webgl";
import { Unicode11Addon } from "@xterm/addon-unicode11";
import { SearchAddon } from "@xterm/addon-search";
import { WebLinksAddon } from "@xterm/addon-web-links";
import { ImageAddon } from "@xterm/addon-image";
import { clamp, debounce, frameOrTimeout, h, isHex, sleep } from "../dom";
import { ipc, listen, type SpawnRequest } from "../ipc";
import { store } from "../store";
import { findPaths, openExternalUrl, openPath } from "../links";
import { confirmDialog } from "../dialogs/modal";
import { toast } from "../dialogs/menu";
import * as renderer from "./renderer";
import { SearchBar, type SearchOpts } from "./search";
import { Minimap } from "./minimap";
import { sanitizePaste, fgEscape, RESET_FG, rgba, snapFont } from "./util";
import type { LaunchSpec, ShellEventKind, ShellKind, SpawnInfo } from "../types";

export const panes = new Map<string, TerminalPane>();

export interface PaneHooks {
  onInput?(p: TerminalPane, bytes: Uint8Array): void;
  onOutput?(p: TerminalPane): void;
  onBell?(p: TerminalPane): void;
  onTitle?(p: TerminalPane, title: string): void;
  onCwd?(p: TerminalPane): void;
  onShell?(p: TerminalPane, kind: ShellEventKind, exitCode: number | null, durationMs: number | null): void;
  onExit?(p: TerminalPane, code: number | null): void;
  onFocus?(p: TerminalPane): void;
  onContextMenu?(p: TerminalPane, e: MouseEvent): void;
  onZoom?(p: TerminalPane, dir: 1 | -1): void;
  onSpawned?(p: TerminalPane, info: SpawnInfo): void;
  onBranch?(p: TerminalPane): void;
  onSearchAll?(q: string, o: SearchOpts): void;
  onStateChange?(p: TerminalPane): void;
}

export interface PaneInit {
  id: string;
  launch: LaunchSpec;
  cwd?: string;
  env?: Record<string, string>;
  startingCommand?: string;
  integration?: "auto" | "off";
  sessionId?: string;
  /** Per-session overrides (§8.1). */
  fontSize?: number;
  themeBackground?: string;
  readOnly?: boolean;
  hooks: PaneHooks;
}

type Phase = "none" | "prompt" | "input" | "running" | "done";
const enc = new TextEncoder();

export class TerminalPane {
  readonly id: string;
  readonly el: HTMLElement;
  readonly host: HTMLElement;
  readonly term: Terminal;
  private fit = new FitAddon();
  private searchAddon = new SearchAddon();
  private searchBar!: SearchBar;
  private webgl: WebglAddon | null = null;
  private bg: HTMLElement;
  private exitBanner: HTMLElement;
  private dropEl: HTMLElement;
  private badges: HTMLElement;
  private minimap: Minimap | null = null;

  hooks: PaneHooks;
  init: PaneInit;
  info: SpawnInfo | null = null;
  opened = false;
  started = false;
  exited = false;
  exitCode: number | null = null;
  readOnly: boolean;
  broadcast = false;
  logging = false;
  recording = false;
  cwd: string | null = null;
  cwdHost = "";
  cwdLocal = true;
  title = "";
  lastExit: number | null = null;
  lastDurationMs: number | null = null;
  branch: string | null = null;
  /** Foreground process name (status bar [P1]), refreshed by the app for the focused pane only. */
  foreground: string | null = null;
  fontOverride: number;
  themeBackground: string;
  phase: Phase = "none";
  bgUrl: string | null = store.bgUrl;
  /** Set by the owning tab: hidden panes never fit (§5.4); they refit when shown. */
  visible = false;

  private cols = 0;
  private rows = 0;
  rx = 0;
  frames = 0;
  private pendingAck = 0;
  private ackTimer: ReturnType<typeof setTimeout> | undefined;
  private exitPending: { code: number | null; total: number } | null = null;
  private markers: IMarker[] = [];
  private navAnchor: number | null = null;
  private navScroll = false;
  private decos: IDecoration[] = [];
  private localInputColor = false;
  private disposables: IDisposable[] = [];
  private ro: ResizeObserver;
  private dprQuery: MediaQueryList | null = null;
  private zoomAcc = 0;
  private lastReq: SpawnRequest | null = null;
  private lastForce = 0;
  private idleTimer: ReturnType<typeof setTimeout> | undefined;
  private redrawTimer: ReturnType<typeof setTimeout> | undefined;
  private startedAtMs = 0;

  constructor(init: PaneInit) {
    this.id = init.id;
    this.init = init;
    this.hooks = init.hooks;
    this.readOnly = !!init.readOnly;
    this.fontOverride = init.fontSize ?? 0;
    this.themeBackground = init.themeBackground ?? "";

    this.bg = h("div", { class: "pane-bg", style: store.bgUrl ? `background-image:url("${store.bgUrl}");opacity:${store.settings.terminal.backgroundImage.opacity}` : "display:none" });
    this.host = h("div", { class: "term-host" });
    this.exitBanner = h("div", { class: "pane-exit", style: "display:none" });
    this.dropEl = h("div", { class: "pane-drop", style: "display:none" });
    this.badges = h("div", { class: "pane-badges" });
    this.el = h("div", { class: "pane", "data-pane": this.id }, this.bg, this.host, h("div", { class: "pane-dim" }), this.badges, this.exitBanner, this.dropEl);

    this.term = new Terminal(this.buildOptions());
    this.ro = new ResizeObserver(() => this.scheduleFit());
    this.ro.observe(this.el);
    panes.set(this.id, this);
  }

  // ------------------------------------------------------------- options/theme
  private fontSize(): number {
    return this.fontOverride >= 8 ? this.fontOverride : store.settings.terminal.fontSize;
  }

  private buildOptions() {
    const t = store.settings.terminal;
    return {
      fontFamily: t.fontFamily,
      fontSize: snapFont(this.fontSize()),
      fontWeight: t.fontWeight as never,
      fontWeightBold: Math.min(900, t.fontWeight + 250) as never,
      lineHeight: t.lineHeight,
      letterSpacing: t.letterSpacing,
      cursorStyle: t.cursorStyle,
      cursorBlink: t.cursorBlink,
      scrollback: t.scrollback,
      allowProposedApi: true,
      customGlyphs: true,
      rescaleOverlappingGlyphs: true,
      allowTransparency: !!this.bgUrl,
      windowsPty: { backend: "conpty" as const, buildNumber: store.info?.osBuild || undefined },
      overviewRuler: { width: 10 },
      theme: this.xtermTheme(),
    };
  }

  private xtermTheme() {
    const t = { ...store.theme.terminal };
    if (this.themeBackground && isHex(this.themeBackground)) t.background = this.themeBackground;
    // An opaque background unless an image is active (§5.3: transparency + erase = ghost pixels).
    if (this.bgUrl) t.background = rgba(t.background ?? "#000000", 0.62);
    return t;
  }

  applyTheme() {
    this.term.options.theme = this.xtermTheme();
    this.el.style.setProperty("--pane-bg", store.theme.terminal.background ?? "#000");
    this.refreshAll();
  }

  /** Re-read the terminal settings (only the changed options are touched by xterm). */
  applySettings() {
    const t = store.settings.terminal;
    const o = this.term.options;
    o.fontFamily = t.fontFamily;
    o.fontSize = snapFont(this.fontSize());
    o.fontWeight = t.fontWeight as never;
    o.fontWeightBold = Math.min(900, t.fontWeight + 250) as never;
    o.lineHeight = t.lineHeight;
    o.letterSpacing = t.letterSpacing;
    o.cursorStyle = t.cursorStyle;
    o.cursorBlink = t.cursorBlink;
    o.scrollback = t.scrollback;
    this.el.classList.toggle("crt", t.crt);
    if (this.opened) this.setMinimap(t.minimap);
    if (t.renderer === "dom") this.detachWebgl();
    else if (this.opened) renderer.touch(this);
    this.scheduleFit();
  }

  /** §5.13: the class shrinks `.term-host` by the strip's width; the caller refits afterwards. */
  private setMinimap(on: boolean) {
    this.el.classList.toggle("has-minimap", on);
    if (on && !this.minimap) this.minimap = new Minimap(this.term, this.el);
    else if (!on) (this.minimap?.dispose(), (this.minimap = null));
  }

  /** Font size changed by zoom / settings: re-snap, refit, tell the PTY. */
  setFontSize(px: number) {
    this.term.options.fontSize = snapFont(px);
    this.scheduleFit();
  }

  async setBackground(url: string | null) {
    if (url === this.bgUrl) return;
    this.bgUrl = url;
    const o = store.settings.terminal.backgroundImage.opacity;
    this.bg.style.backgroundImage = url ? `url("${url}")` : "";
    this.bg.style.opacity = String(o);
    this.bg.style.display = url ? "" : "none";
    this.el.classList.toggle("has-bg", !!url);
    this.term.options.allowTransparency = !!url;
    this.term.options.theme = this.xtermTheme();
    this.detachWebgl(); // the transparency mode is baked into the WebGL renderer
    if (this.opened) renderer.touch(this);
  }

  setBackgroundOpacity(o: number) {
    this.bg.style.opacity = String(o);
  }

  // ------------------------------------------------------------------ lifecycle
  /** Open xterm and spawn the PTY. Must run while the pane is visible (hidden panes never fit — §5.4). */
  async start() {
    if (this.started) return;
    this.started = true;
    const t = store.settings.terminal;
    const family = t.fontFamily;
    try {
      // Cell metrics are wrong if the font loads late, and the shell caches a bad width (§5.4).
      await document.fonts.load(`${snapFont(this.fontSize())}px ${family}`);
      await document.fonts.ready;
    } catch { /* system fonts resolve anyway */ }
    this.openTerminal();
    const size = await this.stableFit();
    await this.spawn(size.cols, size.rows);
    // Late re-fits: WebView2 sometimes reports the final size a few frames after the first paint.
    for (const ms of [60, 180, 400]) setTimeout(() => frameOrTimeout(() => this.doFit()), ms);
  }

  private openTerminal() {
    const term = this.term;
    term.open(this.host);
    this.opened = true;
    term.loadAddon(this.fit);
    term.loadAddon(this.searchAddon);
    term.loadAddon(new Unicode11Addon());
    term.unicode.activeVersion = "11";
    term.loadAddon(new ImageAddon());
    const mouseOff = () => term.modes.mouseTrackingMode === "none";
    term.loadAddon(new WebLinksAddon((e, uri) => { if (e.ctrlKey || mouseOff()) void openExternalUrl(uri); }));
    term.options.linkHandler = {
      allowNonHttpProtocols: true,
      activate: (e, text) => { if (e.ctrlKey || mouseOff()) void openExternalUrl(text); },
      hover: (_e, text) => { this.host.title = text; },
      leave: () => { this.host.title = ""; },
    };
    this.registerFileLinks();
    this.searchBar = new SearchBar(term, this.searchAddon, {
      allTabs: (q, o) => this.hooks.onSearchAll?.(q, o),
    });
    this.el.append(this.searchBar.el);

    term.attachCustomKeyEventHandler((ev) => this.keyHandler(ev));
    this.disposables.push(
      term.onData((d) => this.sendInput(enc.encode(d))),
      term.onBinary((d) => this.sendInput(Uint8Array.from(d, (c) => c.charCodeAt(0) & 0xff))),
      term.onSelectionChange(() => {
        if (store.settings.terminal.copyOnSelect && term.hasSelection()) void this.copySelection(false);
      }),
      term.onScroll(() => { if (!this.navScroll) this.navAnchor = null; }),
      term.onBell(() => this.hooks.onBell?.(this)),
      term.onTitleChange(() => {}),
    );
    // Shell integration markers (§6.2): the backend owns state; the frontend only keeps xterm markers.
    const osc = (data: string) => (this.onShellMark(data), true);
    this.disposables.push(term.parser.registerOscHandler(133, osc), term.parser.registerOscHandler(633, osc));
    this.setupRepair();
    this.setupHostEvents();
    this.el.classList.toggle("crt", store.settings.terminal.crt);
    this.setMinimap(store.settings.terminal.minimap);
    this.watchDpr();
    this.applyTheme();
    if (store.settings.terminal.renderer === "webgl") renderer.touch(this);
  }

  private async spawn(cols: number, rows: number) {
    const req: SpawnRequest = {
      paneId: this.id,
      launch: this.init.launch,
      cwd: this.init.cwd,
      env: this.init.env,
      cols,
      rows,
      startingCommand: this.init.startingCommand,
      integration: this.init.integration,
    };
    this.lastReq = req;
    this.cols = cols;
    this.rows = rows;
    this.exited = false;
    this.exitCode = null;
    this.exitPending = null;
    this.rx = 0;
    this.phase = "none";
    this.startedAtMs = performance.now();
    this.exitBanner.style.display = "none";
    try {
      const info = await ipc.ptySpawn(req, (b) => this.onData(b));
      this.info = info;
      this.localInputColor = info.localInputColor ?? false;
      for (const n of info.notes ?? []) this.term.write(`\x1b[2m[${n}]\x1b[0m\r\n`);
      this.hooks.onSpawned?.(this, info);
      this.hooks.onStateChange?.(this);
    } catch (e) {
      // §4.1: a failed spawn shows inside the pane — red text plus the command line that failed.
      this.exited = true;
      this.term.write(`\r\n\x1b[31m${String(e).replace(/\n/g, "\r\n")}\x1b[0m\r\n`);
      this.hooks.onExit?.(this, null);
    }
  }

  /** Enter after the process exited (§4.7 [P1]). */
  async restart() {
    if (!this.exited || !this.lastReq) return;
    this.term.write("\r\n");
    const { cols, rows } = this.term;
    await this.spawn(cols, rows);
  }

  dispose() {
    this.ro.disconnect();
    this.dprQuery?.removeEventListener("change", this.onDpr);
    clearTimeout(this.ackTimer);
    clearTimeout(this.idleTimer);
    clearTimeout(this.redrawTimer);
    for (const d of this.disposables) d.dispose();
    this.minimap?.dispose();
    renderer.forget(this);
    this.webgl?.dispose();
    this.webgl = null;
    if (this.started) void ipc.ptyClose(this.id, "graceful").catch(() => {});
    try { this.term.dispose(); } catch { /* already disposed */ }
    panes.delete(this.id);
    this.el.remove();
  }

  // ------------------------------------------------------------------- output
  private onData(b: Uint8Array) {
    this.rx += b.length;
    this.frames++;
    const n = b.length;
    // Acks count only bytes xterm has PARSED (§3.5), which is what the write callback signals.
    this.term.write(b, () => this.parsed(n));
    this.hooks.onOutput?.(this);
    if (this.exitPending && this.rx >= this.exitPending.total) this.showExit();
  }

  private parsed(n: number) {
    this.pendingAck += n;
    if (this.pendingAck >= 256 * 1024) this.flushAck();
    else if (!this.ackTimer) this.ackTimer = setTimeout(() => this.flushAck(), 16); // not rAF: it stalls while hidden
  }

  private flushAck() {
    clearTimeout(this.ackTimer);
    this.ackTimer = undefined;
    const n = this.pendingAck;
    this.pendingAck = 0;
    if (n > 0) void ipc.ptyAck(this.id, n).catch(() => {});
  }

  handleExit(code: number | null, total: number) {
    this.exited = true;
    this.exitCode = code;
    this.exitPending = { code, total };
    if (this.rx >= total) this.showExit(); // else onData() shows it once the last frames arrived
  }

  private showExit() {
    const p = this.exitPending;
    if (!p) return;
    this.exitPending = null;
    this.term.write(`\r\n\x1b[2m[process exited with code ${p.code ?? "?"}]\x1b[0m\r\n`);
    this.exitBanner.textContent = "Process exited — Enter to restart, Ctrl+W to close";
    this.exitBanner.style.display = "";
    this.hooks.onStateChange?.(this);
    this.hooks.onExit?.(this, p.code); // the tab applies `terminal.closeOnExit`
  }

  // -------------------------------------------------------------------- input
  sendInput(bytes: Uint8Array) {
    if (this.readOnly) return;
    if (this.exited) {
      if (bytes.length === 1 && bytes[0] === 13) void this.restart();
      return;
    }
    if (this.localInputColor && bytes.includes(13)) this.term.write(RESET_FG);
    void ipc.ptyWrite(this.id, bytes).catch(() => {});
    this.hooks.onInput?.(this, bytes);
  }

  /** Write text as typed input (snippets, drops): bracketed when the program asked for it. */
  paste(text: string, enter = false) {
    if (!text) return;
    const clean = sanitizePaste(text);
    this.term.paste(clean);
    if (enter) this.sendInput(enc.encode("\r"));
  }

  private keyHandler(ev: KeyboardEvent): boolean {
    if (ev.type !== "keydown") return true;
    return true; // shortcuts (copy/paste/...) are matched app-wide in the capture phase before xterm (§5.5)
  }

  hasSelection() {
    return this.term.hasSelection();
  }

  async copySelection(clear = true) {
    const sel = this.term.getSelection();
    if (!sel) return;
    const text = store.settings.terminal.trimTrailingWhitespaceOnCopy ? sel.replace(/[ \t]+$/gm, "") : sel;
    await ipc.clipboardWrite(text);
    if (clear && store.settings.terminal.clearSelectionOnCopy) this.term.clearSelection();
  }

  /** The single place pastes are handled (§5.6): text → files → image. */
  async pasteFromClipboard() {
    if (this.readOnly || this.exited) return;
    let c;
    try {
      c = await ipc.clipboardRead();
    } catch (e) {
      return void toast(`Clipboard unavailable: ${e}`, "error");
    }
    if (c.text) {
      const t = c.text;
      const multi = /\r|\n/.test(t.replace(/(\r?\n)$/, ""));
      const warn = store.settings.terminal.multiLinePasteWarning;
      const bracketed = this.term.modes.bracketedPasteMode;
      if (multi && (warn === "always" || (warn === "auto" && !bracketed))) {
        const lines = t.split(/\r?\n/);
        const preview = h("pre", { class: "paste-preview" }, lines.slice(0, 5).join("\n") + (lines.length > 5 ? `\n… (${lines.length - 5} more lines)` : ""));
        const ok = await confirmDialog(`Paste ${lines.length} lines? Each line may run as a command.`, { title: "Paste multiple lines", ok: "Paste", extra: preview, defaultCancel: true });
        if (!ok) return;
      }
      this.paste(t);
    } else if (c.files.length) {
      const q = await ipc.quotePaths(this.id, c.files);
      this.term.paste(sanitizePaste(q + " "));
    } else if (c.hasImage) {
      // Clipboard-aware CLIs (Claude Code, Codex) read the image from the OS clipboard on Ctrl+V (§5.6).
      this.sendInput(Uint8Array.of(0x16));
    }
    this.term.focus();
  }

  // ------------------------------------------------------------ geometry / DPR
  private onDpr = () => {
    this.term.options.fontSize = snapFont(this.fontSize());
    this.watchDpr();
    this.scheduleFit();
  };

  private watchDpr() {
    this.dprQuery?.removeEventListener("change", this.onDpr);
    this.dprQuery = matchMedia(`(resolution: ${window.devicePixelRatio}dppx)`);
    this.dprQuery.addEventListener("change", this.onDpr);
  }

  private scheduleFit = debounce(() => frameOrTimeout(() => this.doFit()), 50);

  doFit() {
    if (!this.opened || (this.started && !this.visible && this.info) || !this.el.offsetWidth || !this.el.offsetHeight || !this.host.offsetWidth) return;
    const d = this.fit.proposeDimensions();
    if (!d || !Number.isFinite(d.cols) || !Number.isFinite(d.rows) || d.cols < 2 || d.rows < 1) return;
    if (d.cols !== this.term.cols || d.rows !== this.term.rows) this.fit.fit();
    this.pushSize();
  }

  private pushSize() {
    const { cols, rows } = this.term;
    if (!this.info || cols === this.cols && rows === this.rows) return;
    this.cols = cols;
    this.rows = rows;
    void ipc.ptyResize(this.id, cols, rows).catch(() => {});
  }

  /** fit → frame → fit → frame → fit until the size is non-zero and unchanged for 2 frames (§5.4). */
  private async stableFit(): Promise<{ cols: number; rows: number }> {
    let last = "";
    let stable = 0;
    // ~16 s of frames at most: a hidden/zero-sized webview must not make us spawn a 2x1 console.
    for (let i = 0; i < 400 && stable < 2; i++) {
      if (this.el.offsetWidth && this.host.offsetWidth) this.fit.fit();
      await new Promise<void>((r) => frameOrTimeout(r, 40));
      const k = `${this.term.cols}x${this.term.rows}`;
      if (this.term.cols > 3 && this.term.rows > 1 && this.host.offsetWidth > 0 && k === last) stable++;
      else stable = 0;
      last = k;
    }
    if (stable < 2) return { cols: 80, rows: 24 };
    return { cols: this.term.cols, rows: this.term.rows };
  }

  /** The tab holding this pane became visible. */
  async shown() {
    if (!this.started) {
      await this.start();
    } else {
      frameOrTimeout(() => this.doFit());
      if (this.opened && store.settings.terminal.renderer === "webgl") renderer.touch(this);
    }
  }

  // ----------------------------------------------------------------- renderer
  get hasWebgl() {
    return !!this.webgl;
  }

  attachWebgl() {
    if (this.webgl || !this.opened || store.settings.terminal.renderer !== "webgl") return;
    try {
      const gl = new WebglAddon();
      gl.onContextLoss(() => {
        gl.dispose();
        if (this.webgl === gl) this.webgl = null; // keep running on the DOM renderer
      });
      this.term.loadAddon(gl);
      this.webgl = gl;
    } catch {
      this.webgl = null; // WebGL unavailable → DOM renderer
    }
  }

  detachWebgl() {
    if (!this.webgl) return;
    try { this.webgl.dispose(); } catch { /* ignore */ }
    this.webgl = null;
    this.refreshAll();
  }

  refreshAll() {
    if (this.opened) this.term.refresh(0, this.term.rows - 1);
  }

  private repairEnabled() {
    const m = store.settings.terminal.rendererRepair;
    return m === "on" || (m === "auto" && !!this.webgl);
  }

  /** Clear the WebGL glyph atlas and repaint (ghost cells after in-place redraws, §5.3). */
  forceFullRedraw() {
    if (!this.opened) return;
    this.lastForce = performance.now();
    this.webgl?.clearTextureAtlas();
    this.term.refresh(0, this.term.rows - 1);
  }

  private coalescedRedraw(ms = 16) {
    if (this.redrawTimer) return;
    this.redrawTimer = setTimeout(() => {
      this.redrawTimer = undefined;
      this.forceFullRedraw();
    }, ms);
  }

  private setupRepair() {
    const t = this.term;
    this.disposables.push(
      t.onWriteParsed(() => {
        if (!this.repairEnabled()) return;
        clearTimeout(this.idleTimer);
        // 40 ms of quiet, and at most one redraw per 100 ms during sustained output.
        this.idleTimer = setTimeout(() => {
          const since = performance.now() - this.lastForce;
          if (since >= 100) this.forceFullRedraw();
          else this.idleTimer = setTimeout(() => this.forceFullRedraw(), 100 - since);
        }, 40);
      }),
      // CSI J (erase in display): the idle redraw races PSReadLine, so cancel it and redraw explicitly.
      t.parser.registerCsiHandler({ final: "J" }, () => {
        if (this.repairEnabled()) {
          clearTimeout(this.idleTimer);
          const go = () => { this.term.scrollToBottom(); this.forceFullRedraw(); };
          queueMicrotask(go);
          requestAnimationFrame(go);
          setTimeout(go, 40);
          setTimeout(go, 120);
        }
        return false; // xterm still processes it
      }),
      t.parser.registerCsiHandler({ final: "K" }, () => (this.repairEnabled() && this.coalescedRedraw(), false)),
      // Leaving the alternate screen does not emit CSI J: TUI ghosts would stay over the prompt.
      t.buffer.onBufferChange(() => this.repairEnabled() && this.coalescedRedraw()),
    );
  }

  // ------------------------------------------------------------ host events
  private setupHostEvents() {
    const host = this.host;
    host.addEventListener("focusin", () => this.hooks.onFocus?.(this));
    host.addEventListener("mousedown", () => this.hooks.onFocus?.(this), true);
    // Native paste events are swallowed: paste is handled only by the keybinding action (§5.5, §23.12).
    host.addEventListener("paste", (e) => { e.preventDefault(); e.stopPropagation(); }, true);
    // Ctrl + wheel = font zoom.
    host.addEventListener("wheel", (e) => {
      if (!e.ctrlKey) return;
      e.preventDefault();
      e.stopPropagation();
      this.zoomAcc += e.deltaY;
      if (Math.abs(this.zoomAcc) > 40) {
        this.hooks.onZoom?.(this, this.zoomAcc < 0 ? 1 : -1);
        this.zoomAcc = 0;
      }
    }, { passive: false, capture: true });
    host.addEventListener("contextmenu", (e) => {
      e.preventDefault();
      const mode = store.settings.terminal.rightClick;
      if (mode === "paste") void this.pasteFromClipboard();
      else if (mode === "copyPaste") this.hasSelection() ? void this.copySelection() : void this.pasteFromClipboard();
      else this.hooks.onContextMenu?.(this, e);
    });
  }

  // ------------------------------------------------------------------- links
  private registerFileLinks() {
    const term = this.term;
    this.disposables.push(
      term.registerLinkProvider({
        provideLinks: (y, cb) => {
          const buf = term.buffer.active;
          const line = buf.getLine(y - 1);
          if (!line) return cb(undefined);
          // Join wrapped rows so a path that wraps is still one link (§5.10 [P1]).
          let s = y - 1;
          while (s > 0 && buf.getLine(s)?.isWrapped) s--;
          const rows: string[] = [];
          let e = s;
          for (;;) {
            const l = buf.getLine(e);
            if (!l) break;
            rows.push(l.translateToString(false));
            if (!buf.getLine(e + 1)?.isWrapped) break;
            e++;
          }
          const text = rows.join("");
          const cols = term.cols;
          const links = findPaths(text)
            .map((m) => {
              const sr = s + Math.floor(m.start / cols);
              const er = s + Math.floor((m.end - 1) / cols);
              if (y - 1 < sr || y - 1 > er) return null;
              return {
                text: text.slice(m.start, m.end),
                range: { start: { x: (m.start % cols) + 1, y: sr + 1 }, end: { x: ((m.end - 1) % cols) + 1, y: er + 1 } },
                activate: (ev: MouseEvent) => {
                  if (ev.ctrlKey || term.modes.mouseTrackingMode === "none") void openPath(m.path, m.line, this.cwdLocal ? this.cwd ?? undefined : undefined);
                },
              };
            })
            .filter((x): x is NonNullable<typeof x> => !!x);
          cb(links.length ? links : undefined);
        },
      }),
    );
  }

  // -------------------------------------------------------- shell integration
  private onShellMark(data: string) {
    const k = data[0];
    const t = this.term;
    switch (k) {
      case "A": {
        if (t.buffer.active.type === "alternate") return;
        this.phase = "prompt";
        const m = t.registerMarker(0);
        const last = this.markers[this.markers.length - 1];
        if (last && !last.isDisposed && last.line === m.line) {
          m.dispose();
          break;
        }
        this.markers.push(m);
        m.onDispose(() => { const i = this.markers.indexOf(m); if (i >= 0) this.markers.splice(i, 1); });
        while (this.markers.length > 512) this.markers.shift()!.dispose();
        break;
      }
      case "B":
        if (this.phase !== "prompt") return;
        this.phase = "input";
        // Typed-input colour for every shell but PowerShell (whose script colours PSReadLine, §6.6).
        if (this.localInputColor) t.write(fgEscape(store.settings.terminal.typedInputColor));
        break;
      case "C":
        if (this.phase !== "input") return;
        this.phase = "running";
        this.startedAtMs = performance.now();
        if (this.localInputColor) t.write(RESET_FG);
        break;
      case "D": {
        if (this.phase === "done") return;
        const had = this.phase;
        this.phase = "done";
        if (this.localInputColor) t.write(RESET_FG);
        const parts = data.split(";");
        const code = parts.length > 1 && /^-?\d+$/.test(parts[1]) ? parseInt(parts[1], 10) : null;
        const dur = had === "running" ? Math.round(performance.now() - this.startedAtMs) : null;
        this.decorate(code, dur);
        if (this.repairEnabled()) { this.term.scrollToBottom(); this.coalescedRedraw(40); }
        break;
      }
    }
  }

  /** 3 px gutter mark on the prompt line, coloured by exit code (§6.4 [P1]). */
  private decorate(code: number | null, dur: number | null) {
    const marker = this.markers[this.markers.length - 1];
    if (!marker || marker.isDisposed) return;
    const deco = this.term.registerDecoration({ marker, x: 0, width: 1, layer: "top" });
    if (!deco) return;
    const color = code === null ? "var(--ui-foreground-muted)" : code === 0 ? "var(--ui-success)" : "var(--ui-error)";
    const tip = `${code === null ? "exit code unknown" : `exit ${code}`}${dur !== null ? ` · ${dur >= 1000 ? (dur / 1000).toFixed(1) + " s" : dur + " ms"}` : ""}`;
    deco.onRender((el) => {
      el.style.cssText = `width:3px;background:${color};pointer-events:auto;left:0`;
      el.title = tip;
    });
    this.decos.push(deco);
    if (this.decos.length > 512) this.decos.shift()?.dispose();
  }

  /** Ctrl+Alt+Up/Down (§6.4): moves the viewport only, never the PTY cursor. */
  navigate(dir: -1 | 1) {
    const lines = this.markers.filter((m) => !m.isDisposed).map((m) => m.line).sort((a, b) => a - b);
    const buf = this.term.buffer.active;
    const anchor = this.navAnchor ?? buf.viewportY + 1;
    const target = dir < 0 ? [...lines].reverse().find((l) => l < anchor) : lines.find((l) => l > anchor);
    if (target === undefined) return;
    this.navAnchor = target;
    this.navScroll = true;
    this.term.scrollToLine(clamp(target - 1, 0, Math.max(0, buf.length - this.term.rows)));
    setTimeout(() => (this.navScroll = false), 0);
  }

  /** "Clear" resets the markers too (§6.4). */
  clear() {
    this.term.clear();
    for (const m of this.markers.splice(0)) m.dispose();
    for (const d of this.decos.splice(0)) d.dispose();
    this.navAnchor = null;
  }

  // ------------------------------------------------------------- scroll / misc
  scrollLines(n: number) { this.term.scrollLines(n); }
  scrollPages(n: number) { this.term.scrollPages(n); }
  scrollToTop() { this.term.scrollToTop(); }
  scrollToBottom() { this.term.scrollToBottom(); }

  focus() { this.term.focus(); }

  selectAll() { this.term.selectAll(); }

  openSearch(query?: string) {
    this.searchBar?.open(query);
  }

  searchQuery(q: string, o: SearchOpts) {
    this.searchBar?.setQuery(q, o);
  }

  /** Every buffer line joined with `\n` (export, §12.5). */
  bufferText(): string {
    const b = this.term.buffer.active;
    const out: string[] = [];
    for (let i = 0; i < b.length; i++) out.push(b.getLine(i)?.translateToString(true) ?? "");
    while (out.length && !out[out.length - 1]) out.pop();
    return out.join("\n");
  }

  // ------------------------------------------------------------------ badges
  setFlags(f: { readOnly?: boolean; broadcast?: boolean; recording?: boolean; logging?: boolean }) {
    if (f.readOnly !== undefined) this.readOnly = f.readOnly;
    if (f.broadcast !== undefined) this.broadcast = f.broadcast;
    if (f.recording !== undefined) this.recording = f.recording;
    if (f.logging !== undefined) this.logging = f.logging;
    this.badges.replaceChildren(
      ...(this.broadcast ? [h("span", { class: "badge badge-bcast" }, "BROADCAST")] : []),
      ...(this.readOnly ? [h("span", { class: "badge badge-ro" }, "READ-ONLY")] : []),
      ...(this.recording ? [h("span", { class: "badge badge-rec" }, "● REC")] : []),
    );
  }

  // -------------------------------------------------------------- drop overlay
  dropOverlay(o: { title: string; detail: string; severity: "info" | "success" | "warning" | "error"; hideMs?: number } | null) {
    clearTimeout(this.dropTimer);
    if (!o) {
      this.dropEl.style.display = "none";
      return;
    }
    this.dropEl.className = `pane-drop sev-${o.severity}`;
    this.dropEl.replaceChildren(h("div", { class: "drop-title" }, o.title), h("div", { class: "drop-detail" }, o.detail));
    this.dropEl.style.display = "";
    if (o.hideMs) this.dropTimer = setTimeout(() => (this.dropEl.style.display = "none"), o.hideMs);
  }
  private dropTimer: ReturnType<typeof setTimeout> | undefined;

  // ----------------------------------------------------------- event routing
  applyEvent(ev: string, p: Record<string, unknown>) {
    switch (ev) {
      case "pane:title": this.title = p.title as string; this.hooks.onTitle?.(this, this.title); break;
      case "pane:cwd": this.cwd = p.path as string; this.cwdHost = p.host as string; this.cwdLocal = !!p.local; this.hooks.onCwd?.(this); break;
      case "pane:shell":
        if (p.kind === "commandEnd") {
          if (p.exitCode !== null && p.exitCode !== undefined) this.lastExit = p.exitCode as number;
          this.lastDurationMs = (p.durationMs as number | null) ?? null;
        }
        this.hooks.onShell?.(this, p.kind as ShellEventKind, (p.exitCode as number | null) ?? null, (p.durationMs as number | null) ?? null);
        break;
      case "pane:bell": this.hooks.onBell?.(this); break;
      case "pane:branch": this.branch = (p.branch as string | null) ?? null; this.hooks.onBranch?.(this); break;
      case "pane:exited": this.handleExit((p.exitCode as number | null) ?? null, p.totalBytes as number); break;
    }
  }

  get shellKind(): ShellKind | undefined {
    return this.info?.shellKind;
  }
}

/** Route backend pane events to panes. Called once at startup. */
export async function initPaneEvents() {
  const route = <K extends "pane:exited" | "pane:title" | "pane:cwd" | "pane:shell" | "pane:bell" | "pane:branch">(ev: K) =>
    listen(ev, (p) => panes.get((p as { paneId: string }).paneId)?.applyEvent(ev, p as unknown as Record<string, unknown>));
  await Promise.all(["pane:exited", "pane:title", "pane:cwd", "pane:shell", "pane:bell", "pane:branch"].map((e) => route(e as never)));
  await listen("drop:status", (p) => {
    const pane = panes.get(p.paneId);
    const hideMs = (p as unknown as { hideMs?: number }).hideMs;
    pane?.dropOverlay({ title: p.title, detail: p.detail, severity: p.severity, hideMs });
  });
}

export { sleep };
