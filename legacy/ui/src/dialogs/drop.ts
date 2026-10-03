// File drag-and-drop (§11): native drop events arrive from the backend in PHYSICAL pixels; the pane under the
// cursor is found with elementFromPoint. Copy into the pane's cwd by default, Shift = paste quoted paths.

import { h } from "../dom";
import { ipc, listen } from "../ipc";
import { panes } from "../terminal/pane";
import { store } from "../store";
import { modal } from "./modal";

interface DragEv {
  type: "enter" | "over" | "drop" | "leave";
  paths?: string[];
  x?: number;
  y?: number;
  shift?: boolean;
}

export function initDropUi() {
  let hover: string | null = null;
  let paths: string[] = [];

  const paneAt = (x: number, y: number) => {
    const el = document.elementFromPoint(x / window.devicePixelRatio, y / window.devicePixelRatio)?.closest<HTMLElement>(".pane");
    return el?.dataset.pane ?? null;
  };
  const setHover = (id: string | null) => {
    if (id === hover) return;
    if (hover) panes.get(hover)?.dropOverlay(null);
    hover = id;
    const p = id ? panes.get(id) : null;
    if (!p) return;
    p.dropOverlay({
      title: store.settings.drop.defaultAction === "paste" ? "Drop to paste paths" : "Drop to copy",
      detail: p.cwd ?? "Release to copy into this folder",
      severity: "info",
    });
  };

  void (listen as (e: string, f: (p: DragEv) => void) => Promise<unknown>)("app:drag", (e) => {
    if (e.type === "leave") return setHover(null);
    if (e.paths) paths = e.paths;
    const id = e.x !== undefined && e.y !== undefined ? paneAt(e.x, e.y) : null;
    if (e.type === "drop") {
      setHover(null);
      if (id && paths.length) {
        const mode = e.shift ? (store.settings.drop.defaultAction === "paste" ? "copy" : "paste") : store.settings.drop.defaultAction;
        void ipc.filesDrop(id, paths, mode);
      }
      paths = [];
    } else setHover(id);
  });

  // "File already exists" (§11.4): Rename (default) / Replace / Cancel.
  void (listen as (e: string, f: (p: { paneId: string; names: string[]; dest: string }) => void) => Promise<unknown>)("drop:conflict", async (c) => {
    const n = c.names.length;
    const shown = n > 6 ? [...c.names.slice(0, 5), `… and ${n - 5} more`] : c.names;
    const r = await modal<string>({
      title: "File already exists",
      width: 480,
      dismissible: false,
      build: ({ close }) => {
        const rename = h("button", { class: "btn btn-primary", on: { click: () => close("rename") } }, "Rename");
        const first = c.names[0] ?? "name.ext";
        const stem = first.replace(/(\.[^.]*)?$/, ""), ext = /(\.[^.]*)$/.exec(first)?.[1] ?? "";
        return {
          body: h("div", null,
            h("div", { class: "modal-text" }, n > 1 ? `${n} items already exist in the destination.` : "A file with this name already exists."),
            h("div", { class: "field-label" }, `Destination: ${c.dest}`),
            h("ul", { class: "conflict-names" }, shown.map((s) => h("li", null, s))),
            h("div", { class: "modal-hint" }, `Rename keeps both copies as "${stem} (1)${ext}". Replace overwrites the existing item(s).`),
            h("div", { class: "modal-buttons" },
              h("button", { class: "btn", on: { click: () => close("cancel") } }, "Cancel"),
              rename,
              h("button", { class: "btn btn-danger", on: { click: () => close("replace") } }, "Replace"))),
          focus: rename,
        };
      },
    });
    await ipc.dropResolve(c.paneId, (r ?? "cancel") as "rename" | "replace" | "cancel");
  });
}
