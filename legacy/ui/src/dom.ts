// Tiny DOM helpers: no framework, hot paths (terminal output) never touch them.

type Child = Node | string | null | undefined | false;
type Attrs = Record<string, unknown> & { class?: string; style?: string | Partial<CSSStyleDeclaration>; on?: Record<string, (e: any) => void> };

export function h<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  attrs: Attrs | null = null,
  ...children: (Child | Child[])[]
): HTMLElementTagNameMap[K] {
  const el = document.createElement(tag);
  if (attrs) {
    for (const [k, v] of Object.entries(attrs)) {
      if (v == null || v === false) continue;
      if (k === "class") el.className = String(v);
      else if (k === "style") {
        if (typeof v === "string") el.setAttribute("style", v);
        else Object.assign(el.style, v);
      } else if (k === "on") {
        for (const [ev, fn] of Object.entries(v as Record<string, EventListener>)) el.addEventListener(ev, fn);
      } else if (k in el && k !== "list" && typeof v !== "object") (el as unknown as Record<string, unknown>)[k] = v;
      else el.setAttribute(k, v === true ? "" : String(v));
    }
  }
  append(el, children);
  return el;
}

function append(el: Element, children: (Child | Child[])[]) {
  for (const c of children) {
    if (Array.isArray(c)) append(el, c);
    else if (c != null && c !== false) el.append(c);
  }
}

export function clear(el: Element) {
  while (el.firstChild) el.removeChild(el.firstChild);
}

export function debounce<A extends unknown[]>(fn: (...a: A) => void, ms: number) {
  let t: ReturnType<typeof setTimeout> | undefined;
  const d = (...a: A) => {
    clearTimeout(t);
    t = setTimeout(() => fn(...a), ms);
  };
  d.cancel = () => clearTimeout(t);
  d.flush = (...a: A) => {
    clearTimeout(t);
    fn(...a);
  };
  return d;
}

export const raf = () => new Promise<void>((r) => requestAnimationFrame(() => r()));
export const sleep = (ms: number) => new Promise<void>((r) => setTimeout(r, ms));

/** Run `fn` on the next animation frame, but also after `fallbackMs` — rAF stops while the window is hidden. */
export function frameOrTimeout(fn: () => void, fallbackMs = 50) {
  let done = false;
  const run = () => {
    if (!done) {
      done = true;
      fn();
    }
  };
  requestAnimationFrame(run);
  setTimeout(run, fallbackMs);
}

export const esc = (s: string) => s.replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]!);

export function uid(prefix = ""): string {
  return prefix + crypto.randomUUID().replace(/-/g, "");
}

/** Typed tiny emitter. */
export class Emitter<E extends Record<string, unknown>> {
  private l = new Map<keyof E, Set<(v: never) => void>>();
  on<K extends keyof E>(k: K, fn: (v: E[K]) => void): () => void {
    let s = this.l.get(k);
    if (!s) this.l.set(k, (s = new Set()));
    s.add(fn as (v: never) => void);
    return () => s!.delete(fn as (v: never) => void);
  }
  emit<K extends keyof E>(k: K, v: E[K]) {
    this.l.get(k)?.forEach((fn) => (fn as (v: E[K]) => void)(v));
  }
}

export function basename(p: string): string {
  const s = p.replace(/[\\/]+$/, "");
  return s.slice(Math.max(s.lastIndexOf("\\"), s.lastIndexOf("/")) + 1) || s;
}

/** First token's file name without extension: `"C:\\Program Files\\x\\pwsh.exe" -NoLogo` → `pwsh`. */
export function exeStem(cmd: string): string {
  const m = cmd.trim().match(/^"([^"]+)"|^(\S+)/);
  const exe = m ? (m[1] ?? m[2]) : cmd;
  return basename(exe).replace(/\.[A-Za-z0-9]+$/, "");
}

export function clamp(n: number, lo: number, hi: number) {
  return Math.min(hi, Math.max(lo, n));
}

export function isHex(c: string) {
  return /^#([0-9a-f]{3}|[0-9a-f]{6})$/i.test(c.trim());
}
