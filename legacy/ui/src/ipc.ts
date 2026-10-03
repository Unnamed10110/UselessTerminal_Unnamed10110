// Typed wrappers over Tauri `invoke` / events. Outside Tauri (plain browser, `npm run dev`) a mock
// backend is used so the layout can be developed and inspected without the native shell.

import type {
  ClipboardInfo, LaunchSpec, PaneEvents, PaneInfo, SessionsSnapshot, Settings, SettingsBundle, ShellProfile,
  SpawnInfo, Workspace, Session, Folder, Snippet,
} from "./types";

export const isTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

type Invoke = <T>(cmd: string, args?: Record<string, unknown>, opts?: { headers?: Record<string, string> }) => Promise<T>;

let rawInvoke: Invoke;
let ChannelCtor: new <T>() => { onmessage: (m: T) => void };
let rawListen: <K extends keyof PaneEvents>(ev: K, fn: (p: PaneEvents[K]) => void) => Promise<() => void>;

export async function initIpc() {
  if (isTauri) {
    const core = await import("@tauri-apps/api/core");
    const event = await import("@tauri-apps/api/event");
    rawInvoke = core.invoke as unknown as Invoke;
    ChannelCtor = core.Channel as never;
    rawListen = (ev, fn) => event.listen(ev as string, (e) => fn(e.payload as never));
  } else {
    const mock = await import("./mock");
    rawInvoke = mock.invoke as Invoke;
    ChannelCtor = mock.MockChannel as never;
    rawListen = mock.listen as never;
  }
}

const call = <T>(cmd: string, args?: Record<string, unknown>) => rawInvoke<T>(cmd, args);

export const listen = <K extends keyof PaneEvents>(ev: K, fn: (p: PaneEvents[K]) => void) => rawListen(ev, fn);

// ------------------------------------------------------------------ PTY
export interface SpawnRequest {
  paneId: string;
  launch: LaunchSpec;
  cwd?: string;
  env?: Record<string, string>;
  cols: number;
  rows: number;
  startingCommand?: string;
  integration?: "auto" | "off";
}

const enc = new TextEncoder();

export const ipc = {
  ptySpawn(req: SpawnRequest, onData: (b: Uint8Array) => void): Promise<SpawnInfo> {
    const output = new ChannelCtor<ArrayBuffer | number[]>();
    output.onmessage = (m) => onData(m instanceof ArrayBuffer ? new Uint8Array(m) : Uint8Array.from(m));
    return call("pty_spawn", { req, output });
  },
  /** Raw binary body; pane id in a header (§3.4.2). */
  ptyWrite(paneId: string, data: Uint8Array | string): Promise<void> {
    const bytes = typeof data === "string" ? enc.encode(data) : data;
    return (rawInvoke as unknown as (c: string, a: Uint8Array, o: unknown) => Promise<void>)("pty_write", bytes, {
      headers: { paneid: paneId },
    });
  },
  ptyResize: (paneId: string, cols: number, rows: number) => call<void>("pty_resize", { paneId, cols, rows }),
  ptyAck: (paneId: string, bytes: number) => call<void>("pty_ack", { paneId, bytes }),
  ptyClose: (paneId: string, mode: "graceful" | "force" = "graceful") => call<void>("pty_close", { paneId, mode }),
  paneInfo: (paneId: string, foreground = false) => call<PaneInfo | null>("pane_info", { paneId, foreground }),
  logToggle: (paneId: string, title?: string, linePrefix?: string) =>
    call<{ active: boolean; path: string | null }>("log_toggle", { paneId, title, linePrefix }),
  recordToggle: (paneId: string, title?: string) =>
    call<{ active: boolean; path: string | null }>("record_toggle", { paneId, title }),

  // ----------------------------------------------------------- clipboard / system
  clipboardRead: () => call<ClipboardInfo>("clipboard_read"),
  clipboardWrite: (text: string) => call<void>("clipboard_write", { text }),
  appReady: () => call<void>("app_ready"),
  appInfo: () => call<AppInfo>("app_info"),
  openExternal: (url: string, confirmed = false) => call<"opened" | "needsConfirmation">("open_external", { url, confirmed }),
  openPath: (path: string, line?: number, cwd?: string) => call<void>("open_path", { path, line, cwd }),
  exportBuffer: (text: string, suggestedName = "terminal-output.txt") => call<string | null>("export_buffer", { text, suggestedName }),
  notifyAttention: (paneId: string, kind: "flash" | "toast", title?: string, body?: string) =>
    call<void>("notify_attention", { paneId, kind, title, body }),
  runElevated: (sessionId: string) => call<void>("run_elevated", { sessionId }),
  hasBusyChildren: (paneId: string) => call<boolean>("pane_busy", { paneId }),

  // ------------------------------------------------------------ settings / keys
  settingsGet: () => call<SettingsBundle>("settings_get"),
  settingsPatch: (patch: unknown) => call<SettingsBundle>("settings_patch", { patch }),
  settingsResetWithBackup: () => call<SettingsBundle>("settings_reset_with_backup"),
  settingsOpenFile: () => call<void>("settings_open_file"),
  presetTheme: (preset: string, overrides?: unknown) => call<{ theme: SettingsBundle["theme"]; cssVars: [string, string][] }>("theme_preview", { preset, overrides }),
  monospaceFonts: () => call<string[]>("fonts_monospace"),
  keybindingsGet: () => call<Record<string, string[]>>("keybindings_get"),
  keybindingsSet: (action: string, chords: string[] | null) => call<Record<string, string[]>>("keybindings_set", { action, chords }),
  keybindingsReset: (action?: string) => call<Record<string, string[]>>("keybindings_reset", { action }),
  keybindingsKeymap: (name: "default" | "shellSafe") => call<Record<string, string[]>>("keybindings_keymap", { name }),

  // ---------------------------------------------------------------- shells
  shellsDetect: (force = false) => call<ShellProfile[]>("shells_detect", { force }),
  iconFor: (command: string) => call<string>("icon_for", { command }),

  // -------------------------------------------------------------- sessions
  sessionsSnapshot: () => call<SessionsSnapshot>("sessions_snapshot"),
  sessionsSearch: (query: string) => call<Session[]>("sessions_search", { query }),
  sessionAdd: (session: Partial<Session>) => call<Session>("session_add", { session }),
  sessionUpdate: (session: Session) => call<Session>("session_update", { session }),
  sessionDelete: (id: string) => call<void>("session_delete", { id }),
  sessionDuplicate: (id: string) => call<Session>("session_duplicate", { id }),
  folderAdd: (name: string) => call<Folder>("folder_add", { name }),
  folderRename: (id: string, name: string) => call<void>("folder_rename", { id, name }),
  folderDelete: (id: string) => call<void>("folder_delete", { id }),
  folderMove: (id: string, dir: "up" | "down") => call<void>("folder_move", { id, dir }),
  itemsMove: (sessionIds: string[], folderId: string | null, target: DropTarget, half: "top" | "bottom") =>
    call<boolean>("items_move", { sessionIds, folderId, target, half }),
  snippetAdd: (s: Partial<Snippet>) => call<Snippet>("snippet_add", { snippet: s }),
  snippetUpdate: (s: Snippet) => call<Snippet>("snippet_update", { snippet: s }),
  snippetDelete: (id: string) => call<void>("snippet_delete", { id }),
  sessionsExport: () => call<string | null>("sessions_export"),
  sessionsImport: (mode: "merge" | "replace") => call<{ added: number; skipped: number } | null>("sessions_import", { mode }),
  importWindowsTerminal: () => call<{ added: number; found: number; message: string }>("import_wt"),
  importSshConfig: () => call<{ added: number; message: string }>("import_ssh_config"),
  sessionsClearBanner: () => call<void>("sessions_clear_banner"),

  // ------------------------------------------------------------ workspaces
  workspacesList: () => call<Workspace[]>("workspaces_list"),
  workspaceSave: (ws: Omit<Workspace, "id"> & { id?: string }) => call<Workspace>("workspace_save", { workspace: ws }),
  workspaceRename: (id: string, name: string) => call<void>("workspace_rename", { id, name }),
  workspaceDelete: (id: string) => call<void>("workspace_delete", { id }),

  // ---------------------------------------------------------- window state
  windowStateLoad: () => call<unknown>("window_state_load"),
  windowStateSave: (state: unknown) => call<void>("window_state_save", { stateJson: state }),
  windowQuit: () => call<void>("app_quit"),
  backgroundUrl: () => call<string | null>("background_url"),

  // ------------------------------------------------------------ ssh / drops
  paneSsh: (paneId: string) => call<unknown>("pane_ssh", { paneId }),
  quickConnect: (input: string) => call<{ commandLine: string; title: string } | null>("quick_connect", { input }),
  sshHistory: () => call<string[]>("ssh_history"),
  filesDrop: (paneId: string, paths: string[], mode: "copy" | "paste") => call<void>("files_drop", { paneId, paths, mode }),
  dropResolve: (paneId: string, resolution: "skip" | "rename" | "replace" | "cancel") =>
    call<void>("drop_resolve", { paneId, resolution }),
  dropCancel: (paneId: string) => call<void>("drop_cancel", { paneId }),
  quotePaths: (paneId: string, paths: string[]) => call<string>("quote_paths", { paneId, paths }),

  // --------------------------------------------------------------- browser
  browserToggle: (open: boolean) => call<void>("browser_toggle", { open }),
  browserSetBounds: (x: number, y: number, width: number, height: number) =>
    call<void>("browser_set_bounds", { x, y, width, height }),
  browserNavigate: (url: string) => call<void>("browser_navigate", { url }),
  browserNav: (action: "back" | "forward" | "reload") => call<void>("browser_nav", { action }),
  browserHide: (hidden: boolean) => call<void>("browser_hide", { hidden }),
  browserPaste: (text: string) => call<void>("browser_paste", { text }),

  // ----------------------------------------------------------- diagnostics
  diagnostics: () => call<Diagnostics>("diagnostics"),
  setQuakeHotkey: (hotkey: string) => call<string>("quake_set_hotkey", { hotkey }),
  keyLabel: (code: string) => call<string>("key_label", { code }),
  pickFile: (title?: string, filters?: { name: string; extensions: string[] }[]) => call<string | null>("pick_file", { title, filters }),
  pickFolder: (title?: string) => call<string | null>("pick_folder", { title }),
};

export interface AppInfo {
  version: string;
  elevated: boolean;
  osBuild: number;
  computerName: string;
  webview2Version?: string | null;
  homeDir: string;
  dataDir: string;
  args: { cwd?: string; session?: string; workspace?: string; command?: string[] };
  startup?: { kind: string; text: string }[];
}

export interface Diagnostics {
  workingSetBytes: number;
  panes: { paneId: string; unacked: number; pid: number; alive: boolean }[];
}

export type DropTarget =
  | { kind: "folder"; id: string }
  | { kind: "session"; id: string }
  | { kind: "folderEdge"; id: string }
  | { kind: "root" };

export type { Settings };
