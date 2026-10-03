// Status bar (§7.5): event-driven — updated on tab/pane focus changes and pane events, never polled.

import { h, exeStem } from "../dom";
import { store } from "../store";
import { ICONS, icon } from "./tabstrip";
import type { Tab } from "./tab";

export class StatusBar {
  readonly el: HTMLElement;
  private shell = h("span", { class: "sb-shell" });
  private pid = h("span", { class: "sb-pid" });
  private cwd = h("span", { class: "sb-cwd" });
  private branch = h("span", { class: "sb-branch" });
  private exit = h("span", { class: "sb-exit" });
  private state = h("span", { class: "sb-state" });
  private fg = h("span", { class: "sb-fg" });

  constructor() {
    const sep = () => h("span", { class: "sb-sep" });
    this.el = h("div", { class: "statusbar" }, this.shell, sep(), this.pid, sep(), this.cwd, this.branch, sep(), this.exit, h("span", { class: "sb-spacer" }), this.fg, this.state);
  }

  update(tab: Tab | null) {
    const p = tab?.focused;
    if (!tab || !p) {
      for (const e of [this.shell, this.pid, this.cwd, this.branch, this.exit, this.state, this.fg]) e.textContent = "";
      return;
    }
    const cmd = p.info?.commandLine ?? tab.command;
    this.shell.replaceChildren(icon(ICONS.console, "shell"), exeStem(cmd));
    this.pid.textContent = p.info ? `PID ${p.info.pid}` : "";
    const where = p.cwd ?? tab.cwd ?? store.info?.homeDir ?? "";
    this.cwd.textContent = !p.cwdLocal && p.cwdHost ? `${p.cwdHost}:${where}` : where;
    this.cwd.title = this.cwd.textContent ?? "";
    this.branch.replaceChildren(...(p.branch ? [icon(ICONS.branch, "branch"), p.branch] : []));
    this.branch.style.display = p.branch ? "" : "none";

    // A bare OSC 133;D keeps whatever was shown before (§7.5) — `lastExit` only changes on a real code.
    const code = p.lastExit;
    this.exit.className = "sb-exit";
    if (code === null || code === undefined) this.exit.textContent = "";
    else {
      this.exit.classList.add(code === 0 ? "ok" : "bad");
      const d = p.lastDurationMs;
      this.exit.textContent = `exit: ${code}${d !== null && d >= 1000 ? ` · ${fmt(d)}` : ""}`;
    }
    this.state.className = `sb-state ${p.exited ? "exited" : "running"}`;
    this.state.textContent = p.exited ? "○ exited" : p.started ? "● running" : "";
    this.fg.textContent = p.foreground && !p.exited ? p.foreground : "";
  }
}

export function fmt(ms: number): string {
  const s = Math.round(ms / 1000);
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  return m < 60 ? `${m}m ${s % 60}s` : `${Math.floor(m / 60)}h ${m % 60}m`;
}
