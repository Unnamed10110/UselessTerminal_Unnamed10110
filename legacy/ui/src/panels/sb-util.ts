// Small bits shared by the sessions tree and the snippets section.

/** Extra 16×16 stroke icons (the common set lives in layout/tabstrip.ts `ICONS`). */
export const IC = {
  edit: '<path d="M2.5 13.5l.7-3.2 7.6-7.6 2.5 2.5-7.6 7.6z"/><path d="M9.5 4l2.5 2.5"/>',
  trash: '<path d="M2.5 4.5h11M6 4.5V3h4v1.5M4 4.5l.7 8.5h6.6l.7-8.5M6.8 7v4M9.2 7v4"/>',
};

export function lsGet<T>(key: string, fallback: T): T {
  try {
    const v = localStorage.getItem(key);
    return v == null ? fallback : (JSON.parse(v) as T);
  } catch {
    return fallback;
  }
}

export function lsSet(key: string, value: unknown) {
  try {
    localStorage.setItem(key, JSON.stringify(value));
  } catch { /* storage unavailable: the state just is not remembered */ }
}
