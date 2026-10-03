// Shapes shared with the Rust backend (serde camelCase). Keep in sync with crates/ut-core + ut-data.

export interface Settings {
  schemaVersion: number;
  terminal: {
    fontFamily: string;
    fontSize: number;
    fontWeight: number;
    lineHeight: number;
    letterSpacing: number;
    cursorStyle: "bar" | "block" | "underline";
    cursorBlink: boolean;
    scrollback: number;
    renderer: "webgl" | "dom";
    rendererRepair: "auto" | "on" | "off";
    typedInputColor: string;
    overridePsReadLineColors: boolean;
    copyOnSelect: boolean;
    clearSelectionOnCopy: boolean;
    trimTrailingWhitespaceOnCopy: boolean;
    rightClick: "menu" | "paste" | "copyPaste";
    multiLinePasteWarning: "auto" | "always" | "never";
    pasteImages: "passThroughCtrlV" | "inlinePreview";
    osc52: "write" | "readwrite" | "off";
    zoomScope: "global" | "pane";
    closeOnExit: "never" | "graceful" | "always";
    refreshEnvironment: boolean;
    conptyImplementation: "auto" | "bundled" | "system";
    bell: { visual: boolean; audible: boolean; flashTaskbar: boolean };
    backgroundImage: { path: string; opacity: number };
    crt: boolean;
    minimap: boolean;
  };
  theme: { preset: string; overrides: Record<string, unknown> };
  ui: { fontFamily: string; fontSize: number; fontWeight: number; scale: number; backdrop: "none" | "mica" | "acrylic" };
  shells: { defaultProfile: string };
  startup: { mode: "restoreLastSession" | "workspace" | "defaultTab"; workspaceId: string | null; lazyRestore?: boolean };
  processes: { closeConfirm: "whenRunning" | "always" | "never"; killConsoleTreeOnClose: boolean };
  notifications: { commandFinished: { enabled: boolean; minDurationSec: number; toast: boolean } };
  ssh: { connectionReuse: "auto" | "always" | "never" };
  drop: { defaultAction: "copy" | "paste" };
  links: { editorCommand: string };
  logging: { format: "plain" | "raw"; directory: string };
  recording: { captureInput: boolean };
  quake: { hotkey: string; dropdown: boolean; heightPercent: number; hideOnBlur: boolean };
}

/** xterm `ITheme`-shaped colours (all `#rrggbb`). */
export type TerminalTheme = Record<string, string>;

export interface EffectiveTheme {
  terminal: TerminalTheme;
  /** UI tokens keyed by camelCase name; also exported as `--ui-*` CSS variables in `cssVars`. */
  ui: Record<string, string>;
}

/** What `settings_get` / `settings:changed` carry. `theme`/`cssVars` are present only when they changed. */
export interface SettingsBundle {
  settings: Settings;
  theme?: EffectiveTheme;
  cssVars?: [string, string][];
  presets?: string[];
  /** Set when settings.json could not be parsed (§14.1): the file is untouched and defaults are in use. */
  loadError?: string | null;
}

export type ShellKind = "powerShell" | "ssh" | "wsl" | "cmd" | "posix" | "unknown";

export interface ShellProfile {
  id: string;
  name: string;
  path: string;
  arguments: string;
  color: string;
  kind: ShellKind;
  isDefault: boolean;
  command: string;
}

export interface Session {
  id: string;
  name: string;
  description: string;
  shellPath: string;
  arguments: string;
  workingDirectory: string;
  startingCommand: string;
  colorTag: string;
  iconOverride: string;
  folderId: string | null;
  sortOrder: number;
  themeBackground: string;
  themePreset: string;
  fontSize: number;
  environment: string;
  integration: "auto" | "off";
  runAsAdmin: boolean;
}

export interface Folder {
  id: string;
  name: string;
  sortOrder: number;
}

export interface Snippet {
  id: string;
  name: string;
  command: string;
  appendEnter: boolean;
  sortOrder: number;
}

export interface SessionsSnapshot {
  folders: Folder[];
  sessions: Session[];
  snippets: Snippet[];
  banner?: string | null;
  version: number;
}

export interface WorkspaceTab {
  sessionId?: string | null;
  title: string;
  command: string;
  cwd?: string | null;
  startingCommand?: string | null;
  color?: string | null;
  layout?: unknown;
}

export interface Workspace {
  id: string;
  name: string;
  tabs: WorkspaceTab[];
}

export interface LaunchSpec {
  command?: string;
  profileId?: string;
  sessionId?: string;
}

export interface SpawnInfo {
  pid: number;
  shellKind: ShellKind;
  injected: boolean;
  localInputColor: boolean;
  sessionName: string | null;
  commandLine: string;
  cwd: string;
  notes: string[];
}

export interface PaneInfo {
  pid: number;
  alive: boolean;
  exitCode: number | null;
  cwd: string | null;
  cwdHost: string | null;
  title: string;
  lastExit: number | null;
  shellKind: ShellKind;
  commandLine: string;
  unacked: number;
  foreground?: string | null;
  branch?: string | null;
  ssh?: unknown;
}

export interface ClipboardInfo {
  text: string | null;
  files: string[];
  hasImage: boolean;
}

export type ShellEventKind = "promptStart" | "inputStart" | "commandStart" | "commandEnd";

// ---------------------------------------------------------------- events
export interface PaneEvents {
  "pane:exited": { paneId: string; exitCode: number | null; totalBytes: number };
  "pane:title": { paneId: string; title: string };
  "pane:cwd": { paneId: string; path: string; host: string; local: boolean };
  "pane:shell": { paneId: string; kind: ShellEventKind; exitCode: number | null; durationMs: number | null };
  "pane:bell": { paneId: string };
  "pane:progress": { paneId: string; state: number; value: number };
  "pane:notify": { paneId: string; title: string; body: string };
  "pane:branch": { paneId: string; branch: string | null };
  "drop:status": { paneId: string; state: string; title: string; detail: string; severity: "info" | "success" | "warning" | "error" };
  "settings:changed": SettingsBundle & { patch?: unknown };
  "sessions:changed": SessionsSnapshot;
  "workspaces:changed": Workspace[];
  "app:quake-toggle": null;
  "app:second-instance": { args: string[]; cwd: string };
  "app:scroll": { action: "lineUp" | "lineDown" | "pageUp" | "pageDown" | "top" | "bottom" };
  "app:flush": null;
  "app:close-requested": null;
  "app:focus": { focused: boolean };
  "keybindings:changed": Record<string, string[]>;
  "app:banner": { kind: "info" | "warning" | "error"; text: string };
  "browser:navigated": { url: string };
}

// ------------------------------------------------------------- window state
export type LayoutNode =
  | { type: "pane"; paneId: string }
  | { type: "split"; dir: "row" | "col"; ratio: number; a: LayoutNode; b: LayoutNode };
