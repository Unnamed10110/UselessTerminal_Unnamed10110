// Tiny DevTools-protocol client for scripted checks against the running app (UT_DEBUG_PORT, default 9222).
//   node scripts/cdp.mjs eval "<js expression>"      → prints the (JSON) result; promises are awaited
//   node scripts/cdp.mjs shot <file.png>             → screenshot of the main webview
//   node scripts/cdp.mjs type "<text>"               → Input.insertText into the focused element
//   node scripts/cdp.mjs key <key> [ctrl|shift|alt…] → keydown/keyup (e.g. key Enter, key c ctrl)
//   node scripts/cdp.mjs targets                     → list pages
import { writeFileSync } from "node:fs";

const port = process.env.UT_DEBUG_PORT || 9222;
const [, , cmd, ...args] = process.argv;
const targets = await (await fetch(`http://127.0.0.1:${port}/json`)).json();
if (cmd === "targets") {
  console.log(JSON.stringify(targets.map((t) => ({ type: t.type, title: t.title, url: t.url })), null, 1));
  process.exit(0);
}
const page = targets.find((t) => t.type === "page" && /localhost:1420|tauri\.localhost|index\.html/.test(t.url)) ?? targets.find((t) => t.type === "page");
if (!page) throw new Error("no page target");
const ws = new WebSocket(page.webSocketDebuggerUrl);
await new Promise((r, j) => ((ws.onopen = r), (ws.onerror = j)));
let id = 0;
const pending = new Map();
ws.onmessage = (m) => {
  const d = JSON.parse(m.data);
  if (d.id && pending.has(d.id)) {
    const { res, rej } = pending.get(d.id);
    pending.delete(d.id);
    d.error ? rej(new Error(JSON.stringify(d.error))) : res(d.result);
  }
};
const send = (method, params = {}) => new Promise((res, rej) => (pending.set(++id, { res, rej }), ws.send(JSON.stringify({ id, method, params }))));

try {
  if (cmd === "eval") {
    const r = await send("Runtime.evaluate", { expression: args.join(" "), awaitPromise: true, returnByValue: true });
    if (r.exceptionDetails) console.log("EXCEPTION", r.exceptionDetails.exception?.description ?? JSON.stringify(r.exceptionDetails));
    else console.log(typeof r.result.value === "string" ? r.result.value : JSON.stringify(r.result.value));
  } else if (cmd === "shot") {
    const r = await send("Page.captureScreenshot", { format: "png" });
    writeFileSync(args[0], Buffer.from(r.data, "base64"));
    console.log("saved", args[0]);
  } else if (cmd === "type") {
    await send("Input.insertText", { text: args.join(" ") });
  } else if (cmd === "key") {
    const [key, ...mods] = args;
    const modifiers = (mods.includes("alt") ? 1 : 0) | (mods.includes("ctrl") ? 2 : 0) | (mods.includes("shift") ? 8 : 0);
    const code = key.length === 1 ? `Key${key.toUpperCase()}` : key;
    const vk = key.length === 1 ? key.toUpperCase().charCodeAt(0) : { Enter: 13, Tab: 9, Escape: 27, ArrowUp: 38, ArrowDown: 40, ArrowLeft: 37, ArrowRight: 39, Backspace: 8 }[key] ?? 0;
    const text = key === "Enter" ? "\r" : key.length === 1 && !modifiers ? key : undefined;
    await send("Input.dispatchKeyEvent", { type: text ? "keyDown" : "rawKeyDown", key, code, modifiers, windowsVirtualKeyCode: vk, text });
    await send("Input.dispatchKeyEvent", { type: "keyUp", key, code, modifiers, windowsVirtualKeyCode: vk });
  } else throw new Error(`unknown command ${cmd}`);
} finally {
  ws.close();
}
