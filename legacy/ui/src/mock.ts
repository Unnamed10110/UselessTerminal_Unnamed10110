// Browser-only mock backend (used when not running inside Tauri). It exists so the UI can be developed
// and inspected with `npm run dev` + a plain browser; it fakes a very small shell and static data.

import type { Settings, ShellProfile } from "./types";
import { mockSessions } from "./mock-sessions";
import { settingsMock } from "./settings/mock-backend";

type Handler = (p: never) => void;
const listeners = new Map<string, Set<Handler>>();
export function listen(ev: string, fn: Handler): Promise<() => void> {
  if (!listeners.has(ev)) listeners.set(ev, new Set());
  listeners.get(ev)!.add(fn);
  return Promise.resolve(() => listeners.get(ev)?.delete(fn));
}
function emit(ev: string, payload: unknown) {
  listeners.get(ev)?.forEach((fn) => (fn as (p: unknown) => void)(payload));
}

export class MockChannel<T> {
  onmessage: (m: T) => void = () => {};
}

const defaults: Settings = {
  schemaVersion: 3,
  terminal: {
    fontFamily: "'Cascadia Code', 'Cascadia Mono', Consolas, 'Courier New', monospace",
    fontSize: 14, fontWeight: 400, lineHeight: 1, letterSpacing: 0, cursorStyle: "bar", cursorBlink: true, scrollback: 10000,
    renderer: "webgl", rendererRepair: "auto", typedInputColor: "#ffffff", overridePsReadLineColors: true, copyOnSelect: false,
    clearSelectionOnCopy: true, trimTrailingWhitespaceOnCopy: true, rightClick: "menu", multiLinePasteWarning: "auto",
    pasteImages: "passThroughCtrlV", osc52: "write", zoomScope: "global", closeOnExit: "never", refreshEnvironment: true,
    conptyImplementation: "auto", bell: { visual: true, audible: false, flashTaskbar: true },
    backgroundImage: { path: "", opacity: 0.52 }, crt: false, minimap: false,
  },
  theme: { preset: "Default", overrides: {} },
  ui: { fontFamily: "Segoe UI", fontSize: 13, fontWeight: 400, scale: 1, backdrop: "none" },
  shells: { defaultProfile: "auto" },
  startup: { mode: "restoreLastSession", workspaceId: null },
  processes: { closeConfirm: "whenRunning", killConsoleTreeOnClose: true },
  notifications: { commandFinished: { enabled: true, minDurationSec: 10, toast: true } },
  ssh: { connectionReuse: "auto" },
  drop: { defaultAction: "copy" },
  links: { editorCommand: 'code --goto "{file}:{line}"' },
  logging: { format: "plain", directory: "" },
  recording: { captureInput: false },
  quake: { hotkey: "Win+Backquote", dropdown: false, heightPercent: 50, hideOnBlur: false },
};
let settings: Settings = structuredClone(defaults);

// settings / theme / keybinding commands live in settings/mock-backend.ts (RFC 7386 patch, real theme derivation)
const settingsCtx = { defaults, get settings() { return settings; }, set settings(v: Settings) { settings = v; }, emit };

const shells: ShellProfile[] = [
  { id: "pwsh", name: "PowerShell", path: "C:\\Program Files\\PowerShell\\7\\pwsh.exe", arguments: "", color: "#00e5ff", kind: "powerShell", isDefault: true, command: "pwsh.exe" },
  { id: "cmd", name: "Command Prompt", path: "C:\\Windows\\System32\\cmd.exe", arguments: "", color: "#ffff00", kind: "cmd", isDefault: false, command: "cmd.exe" },
  { id: "wsl", name: "WSL", path: "C:\\Windows\\System32\\wsl.exe", arguments: "", color: "#ff8800", kind: "wsl", isDefault: false, command: "wsl.exe" },
];

// ------------------------------------------------------------------ fake shell
const te = new TextEncoder();
const panes = new Map<string, { out: (b: Uint8Array) => void; line: string; cwd: string }>();
const PROMPT = (cwd: string) => `\x1b]133;A\x07\x1b]7;file:///${cwd.replace(/\\/g, "/")}\x07\x1b[32mPS\x1b[0m ${cwd}> \x1b]133;B\x07`;

function run(p: { out: (b: Uint8Array) => void; cwd: string }, id: string, cmd: string) {
  const w = (s: string) => p.out(te.encode(s));
  let code = 0;
  w("\x1b]133;C\x07");
  const [name, ...args] = cmd.trim().split(/\s+/);
  if (!name) code = 0;
  else if (name === "help") w("mock shell: help, clear, seq N, cd X, fail, colors\r\n");
  else if (name === "clear" || name === "cls") w("\x1b[2J\x1b[H");
  else if (name === "seq") for (let i = 1; i <= Math.min(+args[0] || 10, 5000); i++) w(`${i}\r\n`);
  else if (name === "cd") { p.cwd = args[0] ?? p.cwd; emit("pane:cwd", { paneId: id, path: p.cwd, host: "", local: true }); }
  else if (name === "fail") { code = 1; w("\x1b[31merror: failed\x1b[0m\r\n"); }
  else if (name === "colors") for (let i = 30; i < 38; i++) w(`\x1b[${i}mcolor ${i}\x1b[0m \x1b[1;${i}mbold ${i}\x1b[0m\r\n`);
  else { code = 127; w(`mock: ${name}: command not found\r\n`); }
  w(`\x1b]133;D;${code}\x07`);
  emit("pane:shell", { paneId: id, kind: "commandEnd", exitCode: code, durationMs: 5 });
  w(PROMPT(p.cwd));
}

const total = new Map<string, number>();

export async function invoke<T>(cmd: string, args: Record<string, unknown> | Uint8Array = {}, opts?: { headers?: Record<string, string> }): Promise<T> {
  const a = args as Record<string, unknown>;
  // sessions/folders/snippets/import/export/icons/pickers live in mock-sessions.ts (stateful, emits sessions:changed)
  const owned = mockSessions(cmd, a, emit);
  if (owned !== undefined) return owned as T;
  const cfg = settingsMock(cmd, a, settingsCtx);
  if (cfg !== undefined) return cfg as T;
  switch (cmd) {
    case "pty_spawn": {
      const req = a.req as { paneId: string };
      const ch = a.output as MockChannel<ArrayBuffer>;
      const out = (b: Uint8Array) => { total.set(req.paneId, (total.get(req.paneId) ?? 0) + b.length); ch.onmessage(b.slice().buffer); };
      const p = { out, line: "", cwd: "C:\\Users\\mock" };
      panes.set(req.paneId, p);
      setTimeout(() => { out(te.encode("\x1b[36mUseless Terminal (mock backend)\x1b[0m — type `help`\r\n")); out(te.encode(PROMPT(p.cwd))); }, 30);
      return { pid: 4242, shellKind: "powerShell", commandLine: "pwsh.exe", cwd: p.cwd, notes: [] } as T;
    }
    case "pty_write": {
      const id = opts?.headers?.paneid ?? "";
      const p = panes.get(id);
      if (!p) return undefined as T;
      for (const ch of new TextDecoder().decode(args as Uint8Array)) {
        if (ch === "\r") { p.out(te.encode("\r\n")); const l = p.line; p.line = ""; run(p, id, l); }
        else if (ch === "\x7f") { if (p.line) { p.line = p.line.slice(0, -1); p.out(te.encode("\b \b")); } }
        else if (ch >= " ") { p.line += ch; p.out(te.encode(ch)); }
      }
      return undefined as T;
    }
    case "pty_close": panes.delete(a.paneId as string); return undefined as T;
    case "pane_info": return { pid: 4242, alive: true, exitCode: null, cwd: panes.get(a.paneId as string)?.cwd ?? null, cwdHost: "", title: "", lastExit: null, shellKind: "powerShell", commandLine: "pwsh.exe", unacked: 0 } as T;
    case "shells_detect": return shells as T;
    case "workspaces_list": return [] as T;
    case "window_state_load": return null as T;
    case "app_info": return { version: "0.1.0-mock", elevated: false, osBuild: 26200, computerName: "MOCK", homeDir: "C:\\Users\\mock", dataDir: "C:\\mock", args: {} } as T;
    case "clipboard_read": { let text: string | null = null; try { text = await navigator.clipboard.readText(); } catch { /* denied */ } return { text, files: [], hasImage: false } as T; }
    case "clipboard_write": try { await navigator.clipboard.writeText(a.text as string); } catch { /* denied */ } return undefined as T;
    case "diagnostics": return { workingSetBytes: 0, panes: [] } as T;
    // Browser: no native webview in the mock; calls are logged on `window.__browserCalls` and navigation echoes back.
    case "browser_toggle": case "browser_set_bounds": case "browser_nav": case "browser_hide": case "browser_paste": case "browser_navigate":
      ((window as unknown as { __browserCalls?: unknown[] }).__browserCalls ??= []).push([cmd, a]);
      if (cmd === "browser_navigate") emit("browser:navigated", { url: a.url });
      return undefined as T;
    default: return undefined as T;
  }
}

export function mockEmit(ev: string, payload: unknown) { emit(ev, payload); }
