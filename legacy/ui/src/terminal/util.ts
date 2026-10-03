import { rgbOf } from "../color";

export const RESET_FG = "\x1b[39m";

/** `ESC[38;2;r;g;bm` — written into xterm only, never to the PTY (§6.6). */
export function fgEscape(hex: string): string {
  const [r, g, b] = rgbOf(hex);
  return `\x1b[38;2;${r};${g};${b}m`;
}

export function rgba(hex: string, a: number): string {
  if (hex.startsWith("rgba")) return hex;
  const [r, g, b] = rgbOf(hex);
  return `rgba(${r},${g},${b},${a})`;
}

/** Snap to the device pixel grid: fractional CSS px × DPR is the usual source of fuzzy canvas text (§5.4). */
export function snapFont(px: number): number {
  if (!Number.isFinite(px) || px < 8) px = 14;
  const dpr = window.devicePixelRatio || 1;
  return Math.round(px * dpr) / dpr;
}

/**
 * Sanitize pasted text (§5.6): ESC becomes U+241B so a pasted `ESC[201~` cannot break out of bracketed paste;
 * C1 controls are stripped.
 */
export function sanitizePaste(text: string): string {
  return text.replace(/\x1b/g, "␛").replace(/[\u0080-\u009f]/g, "");
}
