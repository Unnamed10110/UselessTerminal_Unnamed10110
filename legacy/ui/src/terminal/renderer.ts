// WebGL context budget (§5.3): Chromium caps live WebGL contexts per page (~16) and drops the oldest, so WebGL
// stays attached only to an LRU of the most recently visible panes. Others run on the DOM renderer until shown.

import type { TerminalPane } from "./pane";

export const MAX_WEBGL = 8;
const lru: TerminalPane[] = [];

export function touch(p: TerminalPane) {
  const i = lru.indexOf(p);
  if (i >= 0) lru.splice(i, 1);
  lru.unshift(p);
  while (lru.length > MAX_WEBGL) lru.pop()!.detachWebgl();
  p.attachWebgl();
}

export function forget(p: TerminalPane) {
  const i = lru.indexOf(p);
  if (i >= 0) lru.splice(i, 1);
}

/** Detach WebGL everywhere (settings switched to the DOM renderer). */
export function releaseAll() {
  for (const p of lru.splice(0)) p.detachWebgl();
}

export function liveContexts(): number {
  return lru.filter((p) => p.hasWebgl).length;
}
