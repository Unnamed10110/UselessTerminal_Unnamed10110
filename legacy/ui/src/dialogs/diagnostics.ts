// Hidden diagnostics page (§19.3): per-pane throughput and flow-control state, renderer in use, live WebGL
// contexts, memory. Sampling runs only while the dialog is open.

import { app } from "../app";
import { h } from "../dom";
import { ipc } from "../ipc";
import { panes } from "../terminal/pane";
import * as renderer from "../terminal/renderer";
import { modal } from "./modal";

export function openDiagnostics() {
  void modal<void>({
    title: "Diagnostics",
    width: 720,
    build: ({ close }) => {
      const body = h("pre", { class: "diag" });
      const last = new Map<string, { rx: number; frames: number; t: number }>();
      const tick = async () => {
        const d = await ipc.diagnostics().catch(() => null);
        const now = performance.now();
        const mem = (performance as unknown as { memory?: { usedJSHeapSize: number } }).memory;
        const lines = [
          `renderer: ${[...panes.values()].some((p) => p.hasWebgl) ? "WebGL" : "DOM"}   live WebGL contexts: ${renderer.liveContexts()} / ${renderer.MAX_WEBGL}`,
          `JS heap: ${mem ? (mem.usedJSHeapSize / 1048576).toFixed(1) + " MiB" : "n/a"}   backend working set: ${d ? (d.workingSetBytes / 1048576).toFixed(1) + " MiB" : "n/a"}`,
          `tabs: ${app.tabs.length}   panes: ${panes.size}`,
          "",
          "pane            pid     bytes/s     frames/s   avg frame   unacked",
        ];
        for (const [id, p] of panes) {
          const l = last.get(id);
          let bps = 0, fps = 0;
          if (l) {
            const dt = (now - l.t) / 1000;
            bps = (p.rx - l.rx) / dt;
            fps = (p.frames - l.frames) / dt;
          }
          last.set(id, { rx: p.rx, frames: p.frames, t: now });
          const un = d?.panes.find((x) => x.paneId === id)?.unacked ?? 0;
          lines.push(`${id.slice(0, 12).padEnd(14)}  ${String(p.info?.pid ?? "-").padEnd(6)}  ${bps.toFixed(0).padStart(9)}  ${fps.toFixed(0).padStart(9)}  ${(fps ? bps / fps : 0).toFixed(0).padStart(9)}  ${un.toString().padStart(8)}`);
        }
        body.textContent = lines.join("\n");
      };
      void tick();
      const timer = setInterval(tick, 1000);
      const done = h("button", { class: "btn btn-primary", on: { click: () => { clearInterval(timer); close(); } } }, "Close");
      return { body: h("div", null, body, h("div", { class: "modal-buttons" }, done)), focus: done };
    },
  });
}
