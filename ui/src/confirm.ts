//! Confirmation dialog for capabilities that declare side effects.
//!
//! The hub can blank a display, reroute audio, take a device exclusively or
//! reset it. Those are legitimate operations, but not ones a stray slider drag
//! should trigger silently, so the panel asks once per capability kind and
//! remembers the answer in localStorage.

import { el } from "./dom";
import { t } from "./i18n";
import type { CapabilityDescriptor, SideEffect } from "./protocol";

const FOCUSABLE = 'button, input, select, textarea, [tabindex]:not([tabindex="-1"])';

/** The localStorage key remembering a "don't ask again" choice. */
export function confirmKey(capType: string): string {
  return `uph.confirm.${capType}`;
}

/** Whether the user already accepted this capability kind for good. */
export function alreadyConfirmed(capType: string): boolean {
  try {
    return window.localStorage.getItem(confirmKey(capType)) === "1";
  } catch {
    // Storage can be unavailable (privacy mode); ask every time then.
    return false;
  }
}

function remember(capType: string): void {
  try {
    window.localStorage.setItem(confirmKey(capType), "1");
  } catch {
    // Not fatal: the user simply gets asked again next time.
  }
}

function warningFor(effect: string): string {
  const key = `danger.side.${effect}`;
  const text = t(key);
  return text === key ? effect : text;
}

export interface DangerousConfirmOptions {
  capability: CapabilityDescriptor;
  /** Called only when the user chooses Apply (or confirms and applies). */
  onApply: () => void;
}

/**
 * Ask before writing a capability with side effects.
 *
 * Focus stays inside, Escape/backdrop cancel, and Apply remembers the
 * "don't ask again" choice before invoking the callback.
 */
export function confirmDangerous(options: DangerousConfirmOptions): void {
  const { capability, onApply } = options;
  const opener = document.activeElement instanceof HTMLElement ? document.activeElement : null;

  const backdrop = el("div", { class: "modal-backdrop" });
  const dialog = el("div", {
    class: "modal modal-danger",
    role: "alertdialog",
    "aria-modal": "true",
    "aria-labelledby": "danger-title",
    "aria-describedby": "danger-intro",
    tabindex: "-1",
  });
  backdrop.append(dialog);
  document.body.append(backdrop);
  document.body.classList.add("modal-open");

  let closed = false;

  function close(): void {
    if (closed) return;
    closed = true;
    document.removeEventListener("keydown", onKeyDown, true);
    backdrop.remove();
    if (!document.querySelector(".modal-backdrop")) {
      document.body.classList.remove("modal-open");
    }
    if (opener?.isConnected) opener.focus();
  }

  function focusable(): HTMLElement[] {
    return Array.from(dialog.querySelectorAll<HTMLElement>(FOCUSABLE)).filter(
      (node) => !node.hasAttribute("disabled"),
    );
  }

  function onKeyDown(event: KeyboardEvent): void {
    if (closed) return;
    if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      close();
      return;
    }
    if (event.key !== "Tab") return;
    const nodes = focusable();
    if (nodes.length === 0) {
      event.preventDefault();
      dialog.focus();
      return;
    }
    const first = nodes[0];
    const last = nodes[nodes.length - 1];
    const active = document.activeElement;
    const inside = active instanceof Node && dialog.contains(active);
    if (event.shiftKey) {
      if (!inside || active === first) {
        event.preventDefault();
        last.focus();
      }
    } else if (!inside || active === last) {
      event.preventDefault();
      first.focus();
    }
  }

  document.addEventListener("keydown", onKeyDown, true);
  backdrop.addEventListener("click", (event) => {
    if (event.target === backdrop) close();
  });

  const dontAsk = el("input", { type: "checkbox" });

  const cancel = el("button", {
    class: "button",
    type: "button",
    text: t("danger.cancel"),
    onclick: close,
  });

  const apply = el("button", {
    class: "button button-primary",
    type: "button",
    text: t("danger.apply"),
    onclick: () => {
      if ((dontAsk as HTMLInputElement).checked) remember(capability.cap_type);
      close();
      onApply();
    },
  });

  const list = el("ul", { class: "danger-list" });
  for (const effect of capability.cost.side_effects as SideEffect[]) {
    list.append(el("li", { text: warningFor(effect) }));
  }

  dialog.append(
    el("h2", { id: "danger-title", class: "modal-title", text: t("danger.title") }),
    el("p", {
      id: "danger-intro",
      class: "muted",
      text: t("danger.intro", { label: capability.label, type: capability.cap_type }),
    }),
    list,
    el("label", { class: "check-row" }, [dontAsk, el("span", { text: t("danger.dontAsk") })]),
    el("div", { class: "modal-actions" }, [cancel, apply]),
  );

  cancel.focus();
}
