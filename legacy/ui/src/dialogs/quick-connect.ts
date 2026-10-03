// Quick SSH connect (§10.2): `[ssh://][user@]host[:port]`, IPv6 aware; the backend parses the grammar.

import { app } from "../app";
import { h } from "../dom";
import { ipc } from "../ipc";
import { modal } from "./modal";
import { toast } from "./menu";

export async function openQuickConnect() {
  const history = await ipc.sshHistory().catch(() => [] as string[]);
  const r = await modal<{ input: string; save: boolean }>({
    title: "Quick SSH Connect",
    width: 460,
    build: ({ close }) => {
      const input = h("input", { class: "input", type: "text", list: "ssh-history", placeholder: "user@host or user@host:port", spellcheck: false });
      const dl = h("datalist", { id: "ssh-history" }, history.map((x) => h("option", { value: x })));
      const err = h("div", { class: "field-error" });
      const go = (save: boolean) => {
        const v = input.value.trim();
        if (!v) return;
        close({ input: v, save });
      };
      input.addEventListener("keydown", (e) => { if (e.key === "Enter") { e.preventDefault(); go(false); } });
      return {
        body: h("div", null,
          h("label", { class: "field-label" }, "Quick SSH Connect (user@host or user@host:port)"), input, dl, err,
          h("div", { class: "modal-buttons" },
            h("button", { class: "btn", on: { click: () => close(undefined) } }, "Cancel"),
            h("button", { class: "btn", on: { click: () => go(true) } }, "Save as session"),
            h("button", { class: "btn btn-primary", on: { click: () => go(false) } }, "Connect"))),
        focus: input,
      };
    },
  });
  if (!r) return;
  try {
    const q = await ipc.quickConnect(r.input);
    if (!q) return void toast("That is not a valid SSH target.", "error");
    if (r.save) {
      const s = await ipc.sessionAdd({ name: q.title, shellPath: q.commandLine, arguments: "", colorTag: "#6be5ff" });
      toast(`Saved session "${s.name}".`, "success");
    }
    await app.openCommand(q.commandLine, { title: q.title, color: "#6be5ff" });
  } catch (e) {
    toast(String(e), "error");
  }
}
