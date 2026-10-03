// Opening links and files (§5.10, §18.2).

import { ipc } from "./ipc";
import { confirmDialog } from "./dialogs/modal";
import { toast } from "./dialogs/menu";

/** http/https/mailto open directly; any other scheme asks for confirmation showing the full URL. */
export async function openExternalUrl(url: string) {
  try {
    const r = await ipc.openExternal(url);
    if (r === "needsConfirmation") {
      const ok = await confirmDialog(`This link uses a non-web protocol. Open it anyway?\n\n${url}`, { title: "Open link", ok: "Open", defaultCancel: true });
      if (ok) await ipc.openExternal(url, true);
    }
  } catch (e) {
    toast(String(e), "error");
  }
}

export async function openPath(path: string, line?: number, cwd?: string) {
  try {
    await ipc.openPath(path, line, cwd);
  } catch (e) {
    toast(String(e), "error");
  }
}

export interface PathMatch {
  start: number;
  end: number; // exclusive
  path: string;
  line?: number;
  col?: number;
}

// Order matters: quoted paths first (they may contain spaces), then drive / UNC / relative / POSIX.
const PATTERNS: RegExp[] = [
  /"((?:[A-Za-z]:[\\/]|\\\\|\.{1,2}[\\/]|~[\\/])[^"\r\n]*)"/g,
  /[A-Za-z]:[\\/][^\s"'<>|*?]*/g,
  /\\\\[\w.$-]+\\[^\s"'<>|*?]*/g,
  /(?<![\w.:\\/])(?:\.{1,2}[\\/]|~\/)[^\s"'<>|*?]+/g,
  /(?<![\w.:\\/])\/[\w.@+\-]+(?:\/[\w.@+\-]*)+/g,
];

const TRAIL = /[.,;)\]}>]+$/;

/** Find file-path-looking tokens in one logical line (wrapped rows already joined). */
export function findPaths(text: string): PathMatch[] {
  const out: PathMatch[] = [];
  const taken: [number, number][] = [];
  const overlaps = (s: number, e: number) => taken.some(([a, b]) => s < b && e > a);
  for (const rx of PATTERNS) {
    rx.lastIndex = 0;
    let m: RegExpExecArray | null;
    while ((m = rx.exec(text))) {
      const quoted = m[1] !== undefined;
      let raw = quoted ? m[1] : m[0];
      let start = m.index + (quoted ? 1 : 0);
      if (!quoted) raw = raw.replace(TRAIL, "");
      if (!raw || raw.length < 3) continue;
      let end = start + raw.length;
      let line: number | undefined;
      let col: number | undefined;
      const suffix = /^:(\d+)(?::(\d+))?/.exec(text.slice(end));
      if (suffix) {
        line = +suffix[1];
        col = suffix[2] ? +suffix[2] : undefined;
        end += suffix[0].length;
      }
      if (quoted) end += 0;
      if (overlaps(start, end)) continue;
      taken.push([start, end]);
      out.push({ start, end, path: raw, line, col });
      start = end;
    }
  }
  return out.sort((a, b) => a.start - b.start);
}
