// Scrollback overview strip on the right edge of a pane (§5.13): one 48 px DPR-aware canvas that
// overlays the terminal (the pane shrinks `.term-host` by the same width via `.has-minimap`).

import "./minimap.css";
import type { IDisposable, Terminal } from "@xterm/xterm";
import { clamp, h } from "../dom";

export const MINIMAP_WIDTH = 48;
const LINE_H = 2; // css px per sampled line

export class Minimap {
  private canvas = h("canvas", { class: "minimap" });
  private ro = new ResizeObserver(() => this.schedule());
  private subs: IDisposable[];
  private raf = 0;
  private dragging = false;

  constructor(private term: Terminal, parent: HTMLElement) {
    parent.append(this.canvas);
    this.subs = [term.onScroll(() => this.schedule()), term.onRender(() => this.schedule())];
    this.ro.observe(this.canvas);
    const c = this.canvas;
    c.addEventListener("pointerdown", (e) => {
      e.preventDefault();
      e.stopPropagation();
      c.setPointerCapture(e.pointerId);
      this.dragging = true;
      this.jump(e);
    });
    c.addEventListener("pointermove", (e) => this.dragging && this.jump(e));
    c.addEventListener("pointerup", () => (this.dragging = false));
    c.addEventListener("pointercancel", () => (this.dragging = false));
    this.schedule();
  }

  dispose() {
    cancelAnimationFrame(this.raf);
    this.ro.disconnect();
    for (const s of this.subs) s.dispose();
    this.canvas.remove();
  }

  private schedule() {
    this.raf ||= requestAnimationFrame(() => {
      this.raf = 0;
      this.render();
    });
  }

  /** CSS px per buffer line: 1:1 (top-anchored) while the buffer is short, compressed to fit once it is long. */
  private scale(total: number, height: number) {
    return Math.min(LINE_H, height / total);
  }

  private jump(e: PointerEvent) {
    const total = this.term.buffer.active.length;
    const { top, height } = this.canvas.getBoundingClientRect();
    const ratio = clamp((e.clientY - top) / (total * this.scale(total, height)), 0, 1);
    this.term.scrollToLine(Math.round(ratio * total));
  }

  private render() {
    const w = this.canvas.clientWidth;
    const hgt = this.canvas.clientHeight;
    if (!w || !hgt) return;
    const dpr = window.devicePixelRatio || 1;
    const pw = Math.round(w * dpr);
    const ph = Math.round(hgt * dpr);
    if (this.canvas.width !== pw || this.canvas.height !== ph) {
      this.canvas.width = pw;
      this.canvas.height = ph;
    }
    const ctx = this.canvas.getContext("2d");
    if (!ctx) return;
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, w, hgt);

    const buf = this.term.buffer.active;
    const total = buf.length;
    const scale = this.scale(total, hgt);
    const step = Math.floor(total / Math.min(total, Math.ceil(hgt / LINE_H)));
    for (let y = 0; y < total; y += step) {
      const len = buf.getLine(y)?.translateToString(true).length ?? 0;
      if (!len) continue;
      const g = Math.min(255, 60 + 3 * len);
      ctx.fillStyle = `rgba(${g},${g},${g},.5)`;
      ctx.fillRect(1, y * scale, Math.min(1, len / 120) * (w - 2), Math.max(1, scale * step - 1));
    }
    ctx.strokeStyle = "rgba(255,255,255,.3)";
    ctx.strokeRect(0.5, buf.viewportY * scale + 0.5, w - 1, Math.max(4, this.term.rows * scale) - 1);
  }
}
