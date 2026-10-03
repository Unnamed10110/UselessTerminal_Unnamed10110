// Modal dialogs rendered inside the main webview (never native windows — §7.12, airspace).

import { Emitter, h } from "../dom";

/** Fires when the first modal opens / the last closes so the browser-panel webview can hide (§17.1). */
export const modalState = new Emitter<{ open: { open: boolean } }>();
let depth = 0;

export interface ModalHandle<T> {
  close: (v?: T) => void;
}

export interface ModalOpts<T> {
  title?: string;
  width?: number;
  /** Builds the body; call `ctx.close(value)` to resolve. Return the element to focus (optional). */
  build: (ctx: ModalHandle<T> & { dialog: HTMLElement }) => { body: HTMLElement; focus?: HTMLElement | null };
  /** Resolve `undefined` on Esc / backdrop click (default true). */
  dismissible?: boolean;
  className?: string;
}

export function modal<T>(opts: ModalOpts<T>): Promise<T | undefined> {
  return new Promise((resolve) => {
    const prevFocus = document.activeElement as HTMLElement | null;
    const backdrop = h("div", { class: "modal-backdrop" });
    const dialog = h("div", { class: `modal ${opts.className ?? ""}`, role: "dialog", "aria-modal": "true", style: { width: `${opts.width ?? 420}px` } });
    let closed = false;
    const close = (v?: T) => {
      if (closed) return;
      closed = true;
      document.removeEventListener("keydown", onKey, true);
      backdrop.remove();
      if (--depth === 0) modalState.emit("open", { open: false });
      prevFocus?.focus?.();
      resolve(v);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape" && opts.dismissible !== false && backdrop.parentElement && lastModal() === backdrop) {
        e.preventDefault();
        e.stopPropagation();
        close(undefined);
      }
    };
    const { body, focus } = opts.build({ close, dialog });
    if (opts.title) dialog.append(h("div", { class: "modal-title" }, opts.title));
    dialog.append(body);
    backdrop.append(dialog);
    backdrop.addEventListener("mousedown", (e) => {
      if (e.target === backdrop && opts.dismissible !== false) close(undefined);
    });
    document.addEventListener("keydown", onKey, true);
    document.getElementById("modal-root")!.append(backdrop);
    if (depth++ === 0) modalState.emit("open", { open: true });
    queueMicrotask(() => (focus ?? dialog.querySelector<HTMLElement>("input,textarea,select,button"))?.focus());
  });
}

function lastModal() {
  const all = document.querySelectorAll("#modal-root > .modal-backdrop");
  return all[all.length - 1];
}

export interface ConfirmOpts {
  title?: string;
  ok?: string;
  cancel?: string;
  /** Style the OK button as a warning/danger action. */
  danger?: boolean;
  /** Put the default (Enter) on Cancel — used for "close while running". */
  defaultCancel?: boolean;
  extra?: HTMLElement;
  /** Extra button between Cancel and OK. */
  third?: { label: string; value: string };
}

export async function confirmDialog(message: string, o: ConfirmOpts = {}): Promise<boolean> {
  const r = await modal<boolean | string>({
    title: o.title,
    width: 440,
    build: ({ close }) => {
      const ok = h("button", { class: `btn ${o.danger ? "btn-danger" : "btn-primary"}`, on: { click: () => close(true) } }, o.ok ?? "OK");
      const cancel = h("button", { class: "btn", on: { click: () => close(false) } }, o.cancel ?? "Cancel");
      const third = o.third ? h("button", { class: "btn", on: { click: () => close(o.third!.value) } }, o.third.label) : null;
      return {
        body: h("div", null, h("div", { class: "modal-text" }, message), o.extra ?? null, h("div", { class: "modal-buttons" }, cancel, third, ok)),
        focus: o.defaultCancel ? cancel : ok,
      };
    },
  });
  return r === true;
}

/** Three-way choice returning the clicked button's value (or undefined on dismiss). */
export function choiceDialog(title: string, message: string, buttons: { label: string; value: string; kind?: "primary" | "danger" }[], defaultValue?: string) {
  return modal<string>({
    title,
    width: 460,
    build: ({ close }) => {
      const els = buttons.map((b) =>
        h("button", { class: `btn ${b.kind === "primary" ? "btn-primary" : b.kind === "danger" ? "btn-danger" : ""}`, on: { click: () => close(b.value) } }, b.label),
      );
      return {
        body: h("div", null, h("div", { class: "modal-text" }, message), h("div", { class: "modal-buttons" }, els)),
        focus: els[Math.max(0, buttons.findIndex((b) => b.value === defaultValue))],
      };
    },
  });
}

export interface PromptOpts {
  title: string;
  label?: string;
  value?: string;
  placeholder?: string;
  /** Whether an empty value is accepted (tab group / title yes, folder name no). */
  allowEmpty?: boolean;
  width?: number;
  multiline?: boolean;
  validate?: (v: string) => string | null;
  okLabel?: string;
}

/** Rename-style dialog (§7.12): all text selected, Enter = OK, Esc = cancel, result trimmed. */
export function promptDialog(o: PromptOpts): Promise<string | undefined> {
  return modal<string>({
    title: o.title,
    width: o.width ?? 360,
    build: ({ close }) => {
      const input = o.multiline
        ? h("textarea", { class: "input", rows: 4, value: o.value ?? "", placeholder: o.placeholder ?? "" })
        : h("input", { class: "input", type: "text", value: o.value ?? "", placeholder: o.placeholder ?? "", spellcheck: false });
      const err = h("div", { class: "field-error" });
      const submit = () => {
        const v = input.value.trim();
        if (!v && !o.allowEmpty) return;
        const bad = o.validate?.(v);
        if (bad) return void (err.textContent = bad);
        close(v);
      };
      input.addEventListener("keydown", ((e: KeyboardEvent) => {
        if (e.key === "Enter" && !(o.multiline && !e.ctrlKey)) {
          e.preventDefault();
          submit();
        }
      }) as EventListener);
      queueMicrotask(() => input.select());
      return {
        body: h("div", null, o.label ? h("label", { class: "field-label" }, o.label) : null, input, err,
          h("div", { class: "modal-buttons" },
            h("button", { class: "btn", on: { click: () => close(undefined) } }, "Cancel"),
            h("button", { class: "btn btn-primary", on: { click: submit } }, o.okLabel ?? "OK"))),
        focus: input,
      };
    },
  });
}

export function messageDialog(title: string, message: string, ok = "OK") {
  return modal<void>({
    title,
    build: ({ close }) => {
      const b = h("button", { class: "btn btn-primary", on: { click: () => close() } }, ok);
      return { body: h("div", null, h("div", { class: "modal-text" }, message), h("div", { class: "modal-buttons" }, b)), focus: b };
    },
  });
}
