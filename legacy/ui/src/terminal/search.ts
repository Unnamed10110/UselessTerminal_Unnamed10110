// Per-pane search overlay (§5.11): incremental, counter "3 of 17", regex / case / whole-word toggles, "All tabs".

import type { SearchAddon, ISearchOptions } from "@xterm/addon-search";
import type { Terminal } from "@xterm/xterm";
import { debounce, h } from "../dom";

const DECO: ISearchOptions["decorations"] = {
  matchBackground: "#5a4a00",
  matchBorder: "#ffd400",
  matchOverviewRuler: "#ffd400",
  activeMatchBackground: "#ff7a00",
  activeMatchBorder: "#ffffff",
  activeMatchColorOverviewRuler: "#ff7a00",
};

export interface SearchHooks {
  /** "All tabs": run the same query in every pane of every tab (the app does the fan-out). */
  allTabs?: (query: string, opts: SearchOpts) => void;
  onClose?: () => void;
}

export interface SearchOpts {
  regex: boolean;
  caseSensitive: boolean;
  wholeWord: boolean;
}

export class SearchBar {
  readonly el: HTMLElement;
  private input: HTMLInputElement;
  private counter: HTMLElement;
  private opts: SearchOpts = { regex: false, caseSensitive: false, wholeWord: false };
  private all: HTMLInputElement;

  constructor(private term: Terminal, private addon: SearchAddon, private hooks: SearchHooks = {}) {
    this.input = h("input", { class: "input search-input", type: "text", placeholder: "Find", spellcheck: false });
    this.counter = h("span", { class: "search-count" });
    this.all = h("input", { type: "checkbox" });
    const toggle = (label: string, key: keyof SearchOpts, title: string) => {
      const b = h("button", { class: "icon-btn search-toggle", title, "aria-pressed": "false" }, label);
      b.addEventListener("click", () => {
        this.opts[key] = !this.opts[key];
        b.classList.toggle("on", this.opts[key]);
        b.setAttribute("aria-pressed", String(this.opts[key]));
        this.run(true);
      });
      return b;
    };
    this.el = h("div", { class: "search-bar", style: "display:none" },
      this.input,
      toggle(".*", "regex", "Regular expression"),
      toggle("Aa", "caseSensitive", "Match case"),
      toggle("ab", "wholeWord", "Whole word"),
      this.counter,
      h("button", { class: "icon-btn", title: "Previous (Shift+Enter)", on: { click: () => this.step(-1) } }, "▲"),
      h("button", { class: "icon-btn", title: "Next (Enter)", on: { click: () => this.step(1) } }, "▼"),
      h("label", { class: "search-all" }, this.all, "All tabs"),
      h("button", { class: "icon-btn", title: "Close (Esc)", on: { click: () => this.close() } }, "✕"),
    );
    const inc = debounce(() => this.run(true), 50);
    this.input.addEventListener("input", inc);
    this.all.addEventListener("change", () => this.run(true));
    this.input.addEventListener("keydown", (e) => {
      if (e.key === "Enter") {
        e.preventDefault();
        this.step(e.shiftKey ? -1 : 1);
      } else if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        this.close();
      }
    });
    addon.onDidChangeResults(({ resultIndex, resultCount }) => {
      this.counter.textContent = !this.input.value ? "" : resultCount === 0 ? "No results" : resultIndex < 0 ? `${resultCount}+` : `${resultIndex + 1} of ${resultCount}`;
    });
  }

  get isOpen() {
    return this.el.style.display !== "none";
  }

  open(query?: string) {
    this.el.style.display = "";
    if (query !== undefined) this.input.value = query;
    else {
      const sel = this.term.getSelection();
      if (sel && !sel.includes("\n")) this.input.value = sel;
    }
    this.input.focus();
    this.input.select();
    if (this.input.value) this.run(true);
  }

  close() {
    this.el.style.display = "none";
    this.addon.clearDecorations();
    this.counter.textContent = "";
    this.hooks.onClose?.();
    this.term.focus();
  }

  /** Programmatic search (used by "All tabs" fan-out). */
  setQuery(q: string, opts?: SearchOpts) {
    if (opts) this.opts = { ...opts };
    this.el.style.display = "";
    this.input.value = q;
    this.run(true);
  }

  private options(incremental: boolean): ISearchOptions {
    return { ...this.opts, incremental, decorations: DECO };
  }

  private run(incremental: boolean) {
    const q = this.input.value;
    if (!q) {
      this.addon.clearDecorations();
      this.counter.textContent = "";
      return;
    }
    this.addon.findNext(q, this.options(incremental));
    if (this.all.checked) this.hooks.allTabs?.(q, this.opts);
  }

  private step(dir: 1 | -1) {
    const q = this.input.value;
    if (!q) return;
    if (dir === 1) this.addon.findNext(q, this.options(false));
    else this.addon.findPrevious(q, this.options(false));
  }
}
