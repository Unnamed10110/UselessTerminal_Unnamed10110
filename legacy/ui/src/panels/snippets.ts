// Snippets section at the bottom of the sidebar (§8.2): collapsible list, add/edit dialog, run on the focused pane.

import { app } from "../app";
import { h } from "../dom";
import { ipc } from "../ipc";
import { store } from "../store";
import { ICONS, icon } from "../layout/tabstrip";
import { confirmDialog, modal } from "../dialogs/modal";
import { showMenu, toast } from "../dialogs/menu";
import { IC, lsGet, lsSet } from "./sb-util";
import type { Snippet } from "../types";

const OPEN_KEY = "ut.sidebar.snippetsOpen";

/** One dialog for add and edit: Name, Command (multi-line), "Press Enter after sending". */
export function openSnippetEditor(sn?: Snippet): Promise<Snippet | undefined> {
  return modal<Snippet>({
    title: sn ? "Edit Snippet" : "New Snippet",
    width: 480,
    build: ({ close }) => {
      const name = h("input", { class: "input", type: "text", value: sn?.name ?? "", spellcheck: false });
      const command = h("textarea", { class: "input ed-mono", rows: 5, value: sn?.command ?? "", spellcheck: false });
      const enter = h("input", { type: "checkbox", checked: sn?.appendEnter ?? true });
      const err = h("div", { class: "field-error" });
      const save = async () => {
        err.textContent = "";
        if (!name.value.trim()) return void ((err.textContent = "Name is required."), name.focus());
        if (!command.value.trim()) return void ((err.textContent = "Command is required."), command.focus());
        const v = { name: name.value.trim(), command: command.value, appendEnter: enter.checked };
        try {
          close(await (sn ? ipc.snippetUpdate({ ...sn, ...v }) : ipc.snippetAdd(v)));
        } catch (e) {
          err.textContent = String(e).replace(/^Error:\s*/, "");
        }
      };
      name.addEventListener("keydown", (e) => { if (e.key === "Enter") (e.preventDefault(), void save()); });
      command.addEventListener("keydown", (e) => { if (e.key === "Enter" && e.ctrlKey) (e.preventDefault(), void save()); });
      return {
        body: h("div", null,
          h("label", { class: "ed-field" }, h("span", { class: "field-label" }, "Name"), name),
          h("label", { class: "ed-field" }, h("span", { class: "field-label" }, "Command"), command),
          h("label", { class: "ed-check" }, enter, "Press Enter after sending"),
          err,
          h("div", { class: "modal-buttons" },
            h("button", { class: "btn", on: { click: () => close(undefined) } }, "Cancel"),
            h("button", { class: "btn btn-primary", on: { click: () => void save() } }, "Save"))),
        focus: name,
      };
    },
  });
}

export class Snippets {
  readonly el: HTMLElement;
  private list = h("div", { class: "sn-list", tabindex: 0, role: "listbox", "aria-label": "Snippets" });
  private count = h("span", { class: "sb-count" });
  private open = lsGet(OPEN_KEY, true);
  private sel: string | null = null;

  constructor() {
    const head = h("div", { class: "sn-head" },
      h("button", { class: "sn-toggle", title: "Show or hide snippets", "aria-label": "Toggle snippets", on: { click: () => this.setOpen(!this.open) } },
        icon(ICONS.chevron, ""), h("span", { class: "sn-title" }, "Snippets"), this.count),
      h("button", { class: "sb-act sn-add", title: "New snippet", "aria-label": "New snippet", on: { click: () => void this.edit() } }, icon(ICONS.plus, "add")));
    this.el = h("div", { class: "sn" }, head, this.list);

    this.list.addEventListener("mousedown", (e) => { if ((e.target as HTMLElement).closest("button")) e.preventDefault(); });
    this.list.addEventListener("click", (e) => {
      const t = e.target as HTMLElement;
      const id = t.closest<HTMLElement>("[data-id]")?.dataset.id;
      if (!id) return;
      this.select(id);
      const a = t.closest<HTMLElement>("[data-act]")?.dataset.act;
      if (a === "edit") void this.edit(this.find(id));
      else if (a === "del") void this.remove(id);
    });
    this.list.addEventListener("dblclick", (e) => {
      const t = e.target as HTMLElement;
      const id = t.closest<HTMLElement>("[data-id]")?.dataset.id;
      if (id && !t.closest("button")) this.run(id);
    });
    this.list.addEventListener("contextmenu", (e) => {
      e.preventDefault();
      const id = (e.target as HTMLElement).closest<HTMLElement>("[data-id]")?.dataset.id;
      if (!id) return void showMenu(e.clientX, e.clientY, [{ label: "New snippet", action: () => void this.edit() }]);
      this.select(id);
      showMenu(e.clientX, e.clientY, [
        { label: "Run", hint: "Enter", action: () => this.run(id) },
        { label: "Edit", hint: "F2", action: () => void this.edit(this.find(id)) },
        { separator: true },
        { label: "Delete", hint: "Del", danger: true, action: () => void this.remove(id) },
      ]);
    });
    this.list.addEventListener("keydown", (e) => {
      const all = store.sessions.snippets;
      const i = all.findIndex((s) => s.id === this.sel);
      if (e.key === "ArrowDown" || e.key === "ArrowUp") this.select(all[Math.max(0, Math.min(all.length - 1, i + (e.key === "ArrowDown" ? 1 : -1)))]?.id ?? null);
      else if (e.key === "Enter" && this.sel) this.run(this.sel);
      else if (e.key === "F2" && this.sel) void this.edit(this.find(this.sel));
      else if (e.key === "Delete" && this.sel) void this.remove(this.sel);
      else return;
      e.preventDefault();
    });

    store.events.on("sessions", () => this.render());
    this.render();
  }

  private find(id: string) {
    return store.sessions.snippets.find((s) => s.id === id);
  }

  private setOpen(open: boolean) {
    this.open = open;
    lsSet(OPEN_KEY, open);
    this.render();
  }

  private select(id: string | null) {
    this.sel = id;
    for (const el of this.list.querySelectorAll<HTMLElement>("[data-id]")) {
      const on = el.dataset.id === id;
      el.classList.toggle("selected", on);
      if (on) el.scrollIntoView({ block: "nearest" });
    }
  }

  private run(id: string) {
    const s = this.find(id);
    if (s) app.runSnippet(s);
  }

  private async edit(sn?: Snippet) {
    const saved = await openSnippetEditor(sn);
    if (saved) this.select(saved.id);
  }

  private async remove(id: string) {
    const s = this.find(id);
    if (!s || !(await confirmDialog(`Delete snippet "${s.name}"?`, { title: "Delete snippet", ok: "Delete", danger: true }))) return;
    try {
      await ipc.snippetDelete(id);
    } catch (e) {
      toast(String(e), "error");
    }
  }

  private render() {
    const all = [...store.sessions.snippets].sort((a, b) => a.sortOrder - b.sortOrder);
    this.el.classList.toggle("collapsed", !this.open);
    this.count.textContent = String(all.length);
    if (this.sel && !all.some((s) => s.id === this.sel)) this.sel = null;
    const btn = (a: string, title: string, path: string) =>
      h("button", { class: "sb-act", "data-act": a, tabindex: -1, title, "aria-label": title }, icon(path, title));
    this.list.replaceChildren(...(all.length ? all.map((s) => {
      const lines = s.command.split("\n");
      return h("div", { class: `sn-item${s.id === this.sel ? " selected" : ""}`, "data-id": s.id, role: "option", title: `${s.command}\n\nDouble-click to run` },
        h("div", { class: "sb-text" },
          h("div", { class: "sb-name" }, s.name),
          h("div", { class: "sb-cmd" }, lines[0] + (lines.length > 1 ? "  ↵…" : ""))),
        h("span", { class: "sb-acts" }, btn("edit", "Edit snippet", IC.edit), btn("del", "Delete snippet", IC.trash)));
    }) : [h("div", { class: "sb-empty" }, "No snippets yet. Use + to add one.")]));
  }
}
