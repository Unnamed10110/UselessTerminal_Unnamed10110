// Session edit dialog (§8.3). The backend validates again on save; its error string is shown inline.

import { rgbOf, toHex } from "../color";
import { h, isHex } from "../dom";
import { ipc } from "../ipc";
import { defaultProfile, store } from "../store";
import { modal } from "./modal";
import type { Session } from "../types";

const SWATCHES = ["#00ff44", "#ff003c", "#ffff00", "#00e5ff", "#ff00ff", "#ff8800"];
const DEFAULTS: Session = {
  id: "", name: "New Session", description: "", shellPath: "", arguments: "", workingDirectory: "", startingCommand: "",
  colorTag: SWATCHES[0], iconOverride: "", folderId: null, sortOrder: 0, themeBackground: "", themePreset: "", fontSize: 0,
  environment: "", integration: "auto", runAsAdmin: false,
};

/** `<input type=color>` only takes `#rrggbb`; the stored colour itself is never rewritten unless the user picks one. */
const pickerValue = (c: string) => (/^#[0-9a-f]{6}$/i.test(c) ? c.toLowerCase() : toHex(...rgbOf(c)));

/** Mirrors the backend's `validate_env` (§8.3 [P1]); parsing itself never fails, bad lines are just ignored. */
function envProblems(text: string): string[] {
  const out: string[] = [];
  const seen = new Map<string, number>();
  text.split("\n").forEach((raw, i) => {
    const line = raw.replace(/\r$/, "");
    if (!line.trim()) return;
    const eq = line.indexOf("=");
    if (eq < 0) return void out.push(`Line ${i + 1}: expected KEY=VALUE (no "=" found); ignored.`);
    const key = line.slice(0, eq).trim().toUpperCase();
    if (!key) return void out.push(`Line ${i + 1}: missing variable name before "="; ignored.`);
    const first = seen.get(key);
    if (first) out.push(`Line ${i + 1}: duplicate of line ${first}; the last value wins.`);
    else seen.set(key, i + 1);
  });
  return out;
}

export function openSessionEditor(session: Partial<Session> = {}, opts: { folderId?: string | null } = {}): Promise<Session | undefined> {
  const isNew = !session.id;
  return modal<Session>({
    title: isNew ? "New Session" : "Edit Session",
    width: 600,
    className: "ed-modal",
    build: ({ close }) => {
      const first = isNew && !session.shellPath ? defaultProfile() : undefined; // new sessions start on the default shell
      const s0: Session = { ...DEFAULTS, shellPath: first?.path ?? "", arguments: first?.arguments ?? "", ...session, folderId: session.folderId ?? opts.folderId ?? null };
      let color = s0.colorTag || SWATCHES[0];
      let nameTouched = !isNew;

      const err = (cls = "") => h("div", { class: `field-error ${cls}` });
      const field = (label: string, control: HTMLElement, extra: (Node | null)[] = []) =>
        h("label", { class: "ed-field" }, h("span", { class: "field-label" }, label), control, ...extra);
      const input = (value: string, placeholder = "") => h("input", { class: "input", type: "text", value, placeholder, spellcheck: false });
      const hint = (t: string) => h("div", { class: "ed-hint" }, t);

      const name = input(s0.name);
      const nameErr = err();
      const desc = h("textarea", { class: "input", rows: 2, value: s0.description, spellcheck: false });
      const preset = h("select", { class: "input" }, h("option", { value: "" }, "— Custom —"), store.shells.map((p) => h("option", { value: p.id }, p.name)));
      const path = input(s0.shellPath, "e.g. C:\\Program Files\\PowerShell\\7\\pwsh.exe");
      const pathErr = err();
      const args = input(s0.arguments);
      const cwd = input(s0.workingDirectory, "blank = your home folder");
      const startCmd = input(s0.startingCommand);
      const bg = input(s0.themeBackground, "none");
      const bgPick = h("input", { class: "ed-pick", type: "color", value: pickerValue(s0.themeBackground || "#000000"), title: "Pick a background colour" });
      const bgErr = err();
      const font = h("input", { class: "input", type: "number", min: 0, max: 32, step: 1, value: String(s0.fontSize) });
      const themePreset = h("select", { class: "input" }, h("option", { value: "" }, "— None —"),
        [...new Set([...store.presets, ...(s0.themePreset ? [s0.themePreset] : [])])].map((p) => h("option", { value: p }, p)));
      const env = h("textarea", { class: "input ed-mono", rows: 4, value: s0.environment, spellcheck: false, placeholder: "KEY=VALUE, one per line" });
      const envProb = h("div", { class: "ed-env-problems" });
      const integ = h("select", { class: "input" }, h("option", { value: "auto" }, "Auto"), h("option", { value: "off" }, "Off"));
      const saveErr = err("ed-save-error");
      themePreset.value = s0.themePreset;
      integ.value = s0.integration;

      // ---- preset ⇄ path/arguments (re-synced on every edit, §8.3 [P1])
      const profileFor = () => store.shells.find((p) => p.path.toLowerCase() === path.value.trim().toLowerCase() && p.arguments.trim() === args.value.trim());
      const syncPreset = () => (preset.value = profileFor()?.id ?? "");
      syncPreset();
      path.addEventListener("input", syncPreset);
      args.addEventListener("input", syncPreset);
      preset.addEventListener("change", () => {
        const p = store.shells.find((x) => x.id === preset.value);
        if (!p) return;
        path.value = p.path;
        args.value = p.arguments;
        setColor(p.color);
        if (!nameTouched) name.value = p.name;
      });
      name.addEventListener("input", () => (nameTouched = true));

      const browse = (target: HTMLInputElement, pick: () => Promise<string | null>) => h("button", {
        class: "btn", type: "button",
        on: { click: async () => {
          const p = await pick().catch(() => null);
          if (!p) return;
          target.value = p;
          target.dispatchEvent(new Event("input"));
        } },
      }, "Browse…");

      // ---- background colour: hex text is the source of truth, the picker mirrors it
      bg.addEventListener("input", () => { if (isHex(bg.value)) bgPick.value = pickerValue(bg.value.trim()); });
      bgPick.addEventListener("input", () => (bg.value = bgPick.value));

      // ---- colour tag: six swatches + a custom colour; an existing non-swatch colour is kept as-is (§24 #17)
      const swatchBtns = SWATCHES.map((c) => h("button", {
        class: "ed-swatch", type: "button", role: "radio", title: c, "aria-label": c,
        style: { background: c }, on: { click: () => setColor(c) },
      }));
      const custom = h("input", { class: "ed-swatch ed-custom", type: "color", title: "Custom colour", "aria-label": "Custom colour", value: pickerValue(color) });
      const hex = h("span", { class: "ed-hex" });
      custom.addEventListener("input", () => setColor(custom.value));
      function setColor(c: string) {
        color = c;
        swatchBtns.forEach((b, i) => {
          const on = SWATCHES[i] === c.toLowerCase();
          b.classList.toggle("on", on);
          b.setAttribute("aria-checked", String(on));
        });
        const isCustom = !SWATCHES.includes(c.toLowerCase());
        custom.classList.toggle("on", isCustom);
        custom.value = pickerValue(c);
        custom.style.setProperty("--c", c);
        hex.textContent = c;
      }
      setColor(color);

      const showEnv = () => envProb.replaceChildren(...envProblems(env.value).map((p) => h("div", null, p)));
      env.addEventListener("input", showEnv);
      showEnv();

      // ---- save
      const saveBtn = h("button", { class: "btn btn-primary", type: "button" }, "Save");
      const save = async () => {
        for (const e of [nameErr, pathErr, bgErr, saveErr]) e.textContent = "";
        let bad: HTMLElement | null = null;
        if (!name.value.trim()) (nameErr.textContent = "Name is required."), (bad ??= name);
        if (!path.value.trim()) (pathErr.textContent = "Shell path is required."), (bad ??= path);
        const bgv = bg.value.trim();
        if (bgv && !isHex(bgv)) (bgErr.textContent = "Use a colour like #1e1e2e."), (bad ??= bg);
        if (bad) return void bad.focus();
        let size = Math.trunc(Number(font.value)) || 0;
        if (size !== 0 && (size < 8 || size > 32)) size = 0; // §8.3: outside 8–32 resets to "global"
        const out: Session = {
          ...s0, name: name.value.trim(), description: desc.value, shellPath: path.value.trim(), arguments: args.value.trim(),
          workingDirectory: cwd.value.trim(), startingCommand: startCmd.value.trim(), colorTag: color, themeBackground: bgv,
          themePreset: themePreset.value, fontSize: size, environment: env.value, integration: integ.value === "off" ? "off" : "auto",
        };
        saveBtn.disabled = true;
        try {
          close(await (isNew ? ipc.sessionAdd(out) : ipc.sessionUpdate(out)));
        } catch (e) {
          saveErr.textContent = String(e).replace(/^Error:\s*/, "");
          saveBtn.disabled = false;
        }
      };
      saveBtn.addEventListener("click", () => void save());

      const body = h("div", { class: "ed" },
        h("div", { class: "ed-scroll" },
          field("Name *", name, [nameErr]),
          field("Description", desc),
          field("Preset shell", preset),
          field("Shell path *", h("div", { class: "ed-row" }, path, browse(path, () => ipc.pickFile("Select shell", [{ name: "Executables", extensions: ["exe"] }]))), [pathErr]),
          field("Arguments", args),
          field("Working directory", h("div", { class: "ed-row" }, cwd, browse(cwd, () => ipc.pickFolder("Select working directory")))),
          field("Starting command", startCmd, [hint("Runs after shell starts (e.g. ssh user@host, cd /project)")]),
          h("div", { class: "ed-section" }, "Theme override"),
          h("div", { class: "ed-grid" },
            field("Background colour", h("div", { class: "ed-row" }, bg, bgPick), [bgErr]),
            field("Font size", font, [hint("0 = global (otherwise 8–32)")]),
            field("Theme preset", themePreset)),
          h("div", { class: "ed-field", role: "radiogroup", "aria-label": "Colour tag" },
            h("span", { class: "field-label" }, "Colour tag"),
            h("div", { class: "ed-swatches" }, swatchBtns, custom, hex)),
          field("Environment variables", env, [envProb]),
          field("Shell integration", integ),
        ),
        h("div", { class: "ed-footer" }, saveErr,
          h("div", { class: "modal-buttons" }, h("button", { class: "btn", type: "button", on: { click: () => close(undefined) } }, "Cancel"), saveBtn)),
      );
      // Enter saves from single-line fields, Ctrl+Enter from anywhere.
      body.addEventListener("keydown", (e) => {
        const t = e.target as HTMLElement;
        if (e.key !== "Enter" || e.isComposing) return;
        if (e.ctrlKey || (t instanceof HTMLInputElement && t.type !== "color")) {
          e.preventDefault();
          void save();
        }
      });
      queueMicrotask(() => name.select());
      return { body, focus: name };
    },
  });
}
