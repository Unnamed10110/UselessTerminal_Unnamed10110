import "./style.css";
import { app } from "./app";
import { initIpc } from "./ipc";
import { panes } from "./terminal/pane";
import { store } from "./store";

async function boot() {
  const root = document.getElementById("app")!;
  try {
    await initIpc();
    await app.init(root);
    // Dev-only inspection handle (used by the CDP end-to-end checks).
    if (import.meta.env.DEV) (window as unknown as Record<string, unknown>).__ut = { app, panes, store };
  } catch (e) {
    root.textContent = `Useless Terminal failed to start: ${e}`;
    root.style.cssText = "color:#ff6b6b;padding:24px;font:14px Segoe UI,sans-serif;white-space:pre-wrap";
    console.error(e);
  }
}

void boot();
