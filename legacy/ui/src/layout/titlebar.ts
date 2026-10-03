// Custom title bar for the frameless window (§7.1).

import { h } from "../dom";
import { isTauri } from "../ipc";
import { icon } from "./tabstrip";

type Win = {
  minimize(): Promise<void>;
  toggleMaximize(): Promise<void>;
  close(): Promise<void>;
  setTitle(t: string): Promise<void>;
  isMaximized(): Promise<boolean>;
  onResized(cb: () => void): Promise<() => void>;
};

let win: Win | null = null;
export async function appWindow(): Promise<Win | null> {
  if (!isTauri) return null;
  win ??= (await import("@tauri-apps/api/window")).getCurrentWindow() as unknown as Win;
  return win;
}

export class TitleBar {
  readonly el: HTMLElement;
  private center = h("div", { class: "tb-title", "data-tauri-drag-region": "" });
  private maxBtn: HTMLButtonElement;
  private base = "Useless Terminal";

  constructor(onClose: () => void) {
    const ring = h("span", { class: "tb-ring", "data-tauri-drag-region": "" });
    const btn = (cls: string, title: string, path: string, fn: () => void) =>
      h("button", { class: `tb-btn ${cls}`, title, tabindex: -1, on: { click: fn } }, icon(path, title));
    this.maxBtn = btn("tb-max", "Maximize", '<rect x="3.5" y="3.5" width="9" height="9" rx="0.5"/>', async () => (await appWindow())?.toggleMaximize()) as HTMLButtonElement;
    this.el = h("div", { class: "titlebar", "data-tauri-drag-region": "" },
      ring, h("span", { class: "tb-name", "data-tauri-drag-region": "" }, "Useless Terminal"),
      this.center,
      h("div", { class: "tb-controls" },
        btn("tb-min", "Minimize", '<path d="M3 8.5h10"/>', async () => (await appWindow())?.minimize()),
        this.maxBtn,
        btn("tb-close", "Close", '<path d="M4 4l8 8M12 4l-8 8"/>', onClose),
      ),
    );
    void appWindow().then(async (w) => {
      if (!w) return;
      const sync = async () => this.maxBtn.classList.toggle("is-max", await w.isMaximized());
      await w.onResized(sync);
      void sync();
    });
  }

  /** `<base>  —  <cwd>` (two spaces around the em dash); `<base>` carries "— Administrator" when elevated. */
  setTitle(cwd: string | null, elevated: boolean) {
    this.base = elevated ? "Useless Terminal — Administrator" : "Useless Terminal";
    const t = cwd ? `${this.base}  —  ${cwd}` : this.base;
    this.center.textContent = cwd ?? "";
    document.title = t;
    void appWindow().then((w) => w?.setTitle(t));
  }
}
