// Settings sections that are plain lists of path-bound controls (§14.3 #1, #2, #3, #5, #7).

import { h } from "../dom";
import { ipc } from "../ipc";
import { eventToChord } from "../keys";
import { patchSettings, store } from "../store";
import { toast } from "../dialogs/menu";
import { btn, check, cleanup, colorRow, get, group, num, pathField, pickImage, reg, row, select, set, slider, syncAll, text, type Opt } from "./fields";
import { overrides } from "./theme";

// ------------------------------------------------------------------ 1. Terminal font
const firstFamily = (stack: string) => stack.split(",")[0].trim().replace(/^['"]|['"]$/g, "");

export function fontSection() {
  let fonts: string[] = [];
  const picker = h("select", { class: "input", on: { change: () => picker.value && set("terminal.fontFamily", `'${picker.value}', Consolas, 'Courier New', monospace`) } });
  const build = () => {
    picker.replaceChildren(h("option", { value: "" }, "Custom font stack (type below)"), ...fonts.map((f) => h("option", { value: f }, f)));
    const cur = firstFamily(get("terminal.fontFamily") ?? "");
    picker.value = fonts.includes(cur) ? cur : "";
  };
  reg(picker, build);
  void ipc.monospaceFonts().then((f) => ((fonts = f), build())).catch(() => {});

  return h("div", null,
    group("Terminal font",
      row("Monospace font", h("div", { class: "s-stack" }, picker, text("terminal.fontFamily", "CSS font stack, e.g. 'Cascadia Code', Consolas, monospace")), "Only monospace fonts installed on this PC are listed; the field below accepts any CSS font stack."),
      row("Size", slider("terminal.fontSize", { min: 8, max: 32, step: 1, unit: " px" })),
      row("Weight", slider("terminal.fontWeight", { min: 300, max: 700, step: 50 })),
      row("Line height", slider("terminal.lineHeight", { min: 0.8, max: 2, step: 0.05 })),
      row("Letter spacing", slider("terminal.letterSpacing", { min: -2, max: 8, step: 1, unit: " px" }))),
    group("Interface scale",
      row("UI scale", slider("ui.scale", { min: 75, max: 200, step: 5, k: 100, unit: "%" }), "Ctrl+scroll over tabs, session panel, or status bar")));
}

// ------------------------------------------------------------------ 2. Interface
const UI_TOKENS: [string, string][] = [
  ["foreground", "Text"], ["foregroundMuted", "Muted text"], ["accent", "Accent"], ["highlight", "Highlight"], ["success", "Success"],
  ["warning", "Warning"], ["error", "Error"], ["chromeBg", "Window background"], ["cardBg", "Card background"], ["cardBorder", "Card border"],
  ["folderSelected", "Selected folder"], ["tabFg", "Tab text"], ["tabSelectedBg", "Selected tab"], ["tabSelectedFg", "Selected tab text"],
  ["statusBg", "Status bar"], ["icon", "Icons"], ["inputBg", "Input background"], ["inputFg", "Input text"], ["hoverBg", "Hover background"], ["splitter", "Splitter"],
];

export function interfaceSection() {
  const tokens = UI_TOKENS.map(([k, label]) => colorRow({
    label,
    value: () => store.theme.ui[k] ?? "#000000",
    apply: (hex) => ({ theme: { overrides: { ui: { [k]: hex } } } }),
    reset: () => ({ theme: { overrides: { ui: { [k]: null } } } }),
    overridden: () => !!overrides().ui?.[k],
  }));
  return h("div", null,
    group("Interface font",
      row("Font", text("ui.fontFamily", "Segoe UI")),
      row("Size", slider("ui.fontSize", { min: 10, max: 22, step: 1, unit: " px" })),
      row("Weight", slider("ui.fontWeight", { min: 300, max: 700, step: 50 }))),
    group("Interface colours", h("div", { class: "s-hint" }, "Derived from the theme; edit one to override it. ↺ returns to the derived value."), h("div", { class: "s-colors" }, tokens)),
    group("Terminal",
      row("Cursor", h("div", { class: "s-inline" }, select("terminal.cursorStyle", [["bar", "Bar"], ["block", "Block"], ["underline", "Underline"]]), check("terminal.cursorBlink", "Blink"))),
      row("Scrollback", num("terminal.scrollback", 0, 200000, 1000), "Lines kept per pane (0–200000)"),
      row("Window backdrop", select("ui.backdrop", [["none", "None"], ["mica", "Mica"], ["acrylic", "Acrylic"]]), "Mica needs Windows 11; it falls back to none elsewhere")));
}

// ------------------------------------------------------------------ 3. Shell background
export function backgroundSection() {
  return h("div", null, group("Shell background",
    row("Image", pathField("terminal.backgroundImage.path", "No image", pickImage, true), "png, jpg, gif, webp or bmp, up to 15 MiB"),
    row("Opacity", slider("terminal.backgroundImage.opacity", { min: 0, max: 100, step: 1, k: 100, unit: "%" }))));
}

// ------------------------------------------------------------------ 5. Behavior
const label = (hk: string) => hk.split("+");

/** Global hotkey recorder (§7.8): shows the key by its label on the current layout, e.g. Ñ on es-ES. */
function quakeHotkey() {
  const win = h("input", { type: "checkbox", title: "Windows key modifier" });
  const shown = h("button", { class: "btn s-hotkey", type: "button", title: "Click, then press the key (add Win with the box)" });
  let stop = () => {};
  const apply = async (hk: string) => {
    try {
      await patchSettings({ quake: { hotkey: await ipc.setQuakeHotkey(hk) } }); // the backend registered it and saved it; this refreshes the store
    } catch (e) {
      toast(String(e), "error");
      syncAll();
    }
  };
  win.addEventListener("change", () => {
    const parts = label(get("quake.hotkey")).filter((p) => p !== "Win");
    void apply((win.checked ? ["Win", ...parts] : parts).join("+"));
  });
  shown.addEventListener("click", () => {
    stop();
    shown.textContent = "Press the key…  (Esc cancels)";
    document.body.classList.add("recording-key");
    const onKey = (e: KeyboardEvent) => {
      e.preventDefault();
      e.stopImmediatePropagation();
      if (e.repeat || e.getModifierState("AltGraph")) return;
      if (e.key === "Escape") return stop();
      const c = eventToChord(e);
      if (!c) return;
      if (!(win.checked || e.ctrlKey || e.altKey)) return void toast("A global hotkey needs Ctrl, Alt or Win", "warning");
      stop();
      void apply((win.checked ? "Win+" : "") + c);
    };
    window.addEventListener("keydown", onKey, true);
    stop = () => {
      window.removeEventListener("keydown", onKey, true);
      document.body.classList.remove("recording-key");
      stop = () => {};
      syncAll();
    };
  });
  cleanup.add(() => stop());
  reg(shown, async () => {
    const hk: string = get("quake.hotkey");
    win.checked = label(hk).includes("Win");
    if (document.body.classList.contains("recording-key")) return;
    const parts = label(hk);
    const key = parts.pop()!;
    shown.textContent = [...parts, await ipc.keyLabel(key).catch(() => key)].join(" + ");
  });
  return h("div", { class: "s-inline" }, shown, h("label", { class: "s-check" }, win, "Win"));
}

const sel = (path: string, ...o: Opt[]) => select(path, o);

export function behaviorSection() {
  const profiles = (): Opt[] => [["auto", "Automatic (first detected)"], ...store.shells.map((s) => [s.id, s.name] as Opt)];
  const workspaces = (): Opt[] => store.workspaces.map((w) => [w.id, w.name] as Opt);
  const wsRow = reg(row("Workspace", select("startup.workspaceId", workspaces, { conv: { to: (s) => s || null, from: (v) => String(v ?? "") } })), (r) => (r.hidden = get("startup.mode") !== "workspace"));

  return h("div", null,
    group("Copy & paste",
      check("terminal.copyOnSelect", "Copy on select"),
      check("terminal.clearSelectionOnCopy", "Clear the selection after copying"),
      check("terminal.trimTrailingWhitespaceOnCopy", "Trim trailing whitespace when copying"),
      row("Right click", sel("terminal.rightClick", ["menu", "Context menu"], ["paste", "Paste"], ["copyPaste", "Copy when selected, otherwise paste"])),
      row("Multi-line paste warning", sel("terminal.multiLinePasteWarning", ["auto", "Auto (only without bracketed paste)"], ["always", "Always"], ["never", "Never"])),
      row("Pasting an image", sel("terminal.pasteImages", ["passThroughCtrlV", "Pass Ctrl+V to the program"], ["inlinePreview", "Show an inline preview"])),
      row("OSC 52 clipboard", sel("terminal.osc52", ["write", "Programs may write"], ["readwrite", "Programs may read and write"], ["off", "Off"])),
      row("Zoom applies to", sel("terminal.zoomScope", ["global", "All panes"], ["pane", "The focused pane"]))),
    group("Tabs & processes",
      row("When a process exits", sel("terminal.closeOnExit", ["never", "Keep the tab open"], ["graceful", "Close if the exit code is 0"], ["always", "Always close the tab"])),
      row("Confirm before closing", sel("processes.closeConfirm", ["whenRunning", "Only when something is running"], ["always", "Always"], ["never", "Never"])),
      check("processes.killConsoleTreeOnClose", "Kill the console process tree when closing a tab", true)),
    group("Bell & notifications",
      check("terminal.bell.visual", "Visual bell (pane flash)"),
      check("terminal.bell.audible", "Audible bell"),
      check("terminal.bell.flashTaskbar", "Flash the taskbar when the window is unfocused"),
      check("notifications.commandFinished.enabled", "Notify when a long command finishes"),
      row("…longer than", h("div", { class: "s-inline" }, num("notifications.commandFinished.minDurationSec", 0, 86400), h("span", { class: "s-unit" }, "seconds"))),
      check("notifications.commandFinished.toast", "Also show a toast")),
    group("Startup",
      row("On start", sel("startup.mode", ["restoreLastSession", "Restore the last session"], ["workspace", "Open a workspace"], ["defaultTab", "One tab with the default shell"])),
      wsRow,
      check("startup.lazyRestore", "Start restored tabs lazily (only the active one immediately)", true),
      row("Default shell", select("shells.defaultProfile", profiles))),
    group("Rendering & environment",
      row("Renderer", sel("terminal.renderer", ["webgl", "WebGL (fast)"], ["dom", "DOM (compatible)"])),
      row("Ghost-cell repair", sel("terminal.rendererRepair", ["auto", "Auto"], ["on", "On"], ["off", "Off"])),
      row("ConPTY", sel("terminal.conptyImplementation", ["auto", "Auto"], ["bundled", "Bundled (newer conhost)"], ["system", "System"]), "Applies to new tabs"),
      check("terminal.refreshEnvironment", "Refresh PATH and environment variables from the registry for new tabs"),
      check("terminal.crt", "Retro CRT effect"),
      check("terminal.minimap", "Minimap scrollbar")),
    group("Links, logs & recording",
      row("Editor command", text("links.editorCommand", 'code --goto "{file}:{line}"'), "{file} and {line} are replaced when a path:line link is opened"),
      row("Log format", sel("logging.format", ["plain", "Plain text (escape codes removed)"], ["raw", "Raw (keep escape codes)"])),
      row("Log folder", pathField("logging.directory", "Default (app data folder)", () => ipc.pickFolder("Choose the log folder"))),
      check("recording.captureInput", "Capture typed input in asciicast recordings")),
    group("Quake window",
      row("Global hotkey", quakeHotkey(), "Shows or hides the window from anywhere"),
      check("quake.dropdown", "Drop-down from the top of the screen"),
      row("Height", slider("quake.heightPercent", { min: 10, max: 100, step: 5, unit: "%" })),
      check("quake.hideOnBlur", "Hide when it loses focus")));
}

// ------------------------------------------------------------------ 7. SSH & drops
export function sshSection() {
  return h("div", null,
    group("SSH", row("Connection reuse", sel("ssh.connectionReuse", ["auto", "Auto (when ssh supports it)"], ["always", "Always"], ["never", "Never"]), "OpenSSH ControlMaster: new panes to the same host skip the login")),
    group("File drops", row("Dropping files", sel("drop.defaultAction", ["copy", "Copy them into the shell's folder"], ["paste", "Paste their paths"]), "Hold Shift while dropping to do the other one")));
}

// ------------------------------------------------------------------ 8. About
export function aboutSection() {
  const i = store.info;
  const kv = (k: string, v: string) => row(k, h("div", { class: "s-value" }, v));
  return h("div", null, group("Useless Terminal",
    kv("Version", i.version),
    kv("Developer", "Unnamed10110"),
    kv("WebView2", i.webview2Version || "unknown"),
    kv("Windows build", String(i.osBuild)),
    kv("Elevated", i.elevated ? "Yes: running as administrator" : "No"),
    kv("Data folder", i.dataDir),
    row("Settings file", btn("Open settings.json", () => void ipc.settingsOpenFile().catch((e) => toast(String(e), "error"))))));
}
