//! The settings overlay.
//!
//! RPC access is injected (`SettingsDeps`) so this module never imports the
//! Tauri bridge: `main.ts` owns `call()`, the prefs cache and the language
//! switch. The modal is appended to `document.body`, not the panel root, so
//! the periodic `paint()` cannot tear it down while it is open.

import { clear, el } from "./dom";
import { isLanguage, getLanguage, LANGUAGE_OPTIONS, t } from "./i18n";
import type { Diagnostics, Prefs } from "./protocol";

export interface SettingsDeps {
  /** `prefs.get`; called on every open so the form shows the stored values. */
  load(): Promise<Prefs>;
  /** `prefs.set` plus applying the normalized result (language included). */
  save(prefs: Prefs): Promise<void>;
  /** `diagnostics.get`; shown below the preferences, refreshed on demand. */
  diagnostics(): Promise<Diagnostics>;
}

const INTERVALS = [15, 30, 60, 120, 300];
const DEFAULT_INTERVAL = 60;
const MIN_ALERT = 5;
const MAX_ALERT = 50;
const DEFAULT_ALERT = 20;

const FOCUSABLE = 'button, input, select, textarea, [tabindex]:not([tabindex="-1"])';

/** A readable message for an RPC rejection (string, Error or `{message}`). */
function messageOf(err: unknown): string {
  if (typeof err === "string") return err;
  if (err instanceof Error) return err.message;
  if (err && typeof err === "object" && "message" in err) {
    const message = (err as { message?: unknown }).message;
    if (typeof message === "string") return message;
  }
  return String(err);
}

function clampThreshold(value: number): number {
  if (!Number.isFinite(value)) return DEFAULT_ALERT;
  return Math.min(MAX_ALERT, Math.max(MIN_ALERT, Math.round(value)));
}

function radioOption(name: string, value: string, label: string, checked: boolean): HTMLElement {
  return el("label", { class: "radio-row" }, [
    el("input", { type: "radio", name, value, checked }),
    el("span", { text: label }),
  ]);
}

/**
 * Open the settings modal.
 *
 * Focus stays inside the dialog (Tab wraps), Escape and a backdrop click close
 * it, and a failed save leaves the form open with the reason inline.
 */
export function openSettings(deps: SettingsDeps): void {
  const opener = document.activeElement instanceof HTMLElement ? document.activeElement : null;
  const backdrop = el("div", { class: "modal-backdrop" });
  const dialog = el("div", {
    class: "modal",
    role: "dialog",
    "aria-modal": "true",
    "aria-labelledby": "settings-title",
    tabindex: "-1",
  });
  backdrop.append(dialog);
  document.body.append(backdrop);
  document.body.classList.add("modal-open");

  let closed = false;
  let saving = false;
  /** Set once the form exists; `persist` refuses to read a half-built form. */
  let ready = false;

  function close(): void {
    if (closed) return;
    closed = true;
    document.removeEventListener("keydown", onKeyDown, true);
    backdrop.remove();
    if (!document.querySelector(".modal-backdrop")) {
      document.body.classList.remove("modal-open");
    }
    // The opener can be a header button that a preferences repaint replaced;
    // fall back to the current settings button so focus is never lost.
    const returnTo = opener?.isConnected
      ? opener
      : document.querySelector<HTMLElement>(".button-icon");
    returnTo?.focus();
  }

  // --- Diagnostics ---------------------------------------------------------
  // A read-only block below the preferences. It loads independently, so an
  // older hub without `diagnostics.get` still leaves the prefs form usable.

  let diagnostics: Diagnostics | null = null;
  let diagnosticsError: string | null = null;
  let diagnosticsNode: HTMLElement | null = null;
  let diagnosticsBody: HTMLElement | null = null;
  let diagnosticsRefresh: HTMLButtonElement | null = null;
  let diagnosticsCopy: HTMLButtonElement | null = null;
  let diagnosticsBusy = false;

  function clock(at: number): string {
    const date = new Date(at);
    const pad = (value: number) => String(value).padStart(2, "0");
    return `${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}`;
  }

  function adapterKind(adapter: Diagnostics["adapters"][number]): string {
    return adapter.kind === "in_process"
      ? t("settings.adapterKind.in_process")
      : t("settings.adapterKind.out_of_process");
  }

  function adapterStatus(adapter: Diagnostics["adapters"][number]): string {
    const state = adapter.enabled ? t("settings.adapterOn") : t("settings.adapterOff");
    const loaded = adapter.loaded ? t("settings.adapterLoaded") : t("settings.adapterNotLoaded");
    const devices =
      adapter.device_count === 1
        ? t("settings.adapterDevicesOne")
        : t("settings.adapterDevices", { count: adapter.device_count });
    return `${state} · ${loaded} · ${devices}`;
  }

  function diagnosticsText(data: Diagnostics): string {
    const lines: string[] = [
      `${t("app.title")} — ${t("settings.diagnostics")}`,
      `${t("settings.hubVersion")}: ${data.hub_version}`,
      `${t("settings.protocol")}: ${data.protocol}`,
      `${t("settings.pid")}: ${data.pid}`,
      `${t("settings.pipe")}: ${data.pipe}`,
      `${t("settings.configDir")}: ${data.config_dir}`,
      "",
      `${t("adapters.title")}:`,
    ];
    if (data.adapters.length === 0) lines.push("  —");
    for (const adapter of data.adapters) {
      lines.push(`  ${adapter.name} (${adapter.id}) [${adapterKind(adapter)}] ${adapterStatus(adapter)}`);
    }
    lines.push("", `${t("settings.recentErrors")}:`);
    if (data.recent_errors.length === 0) {
      lines.push(`  ${t("settings.noErrors")}`);
    } else {
      for (const entry of data.recent_errors) {
        const kind = entry.kind ? ` [${entry.kind}]` : "";
        lines.push(`  ${clock(entry.at_ms)} ${entry.method}${kind}: ${entry.message}`);
      }
    }
    return lines.join("\n");
  }

  async function loadDiagnostics(): Promise<void> {
    if (!diagnosticsBody || diagnosticsBusy) return;
    diagnosticsBusy = true;
    if (diagnosticsRefresh) diagnosticsRefresh.disabled = true;
    clear(diagnosticsBody);
    diagnosticsBody.append(
      el("p", { class: "muted", text: t("settings.diagnosticsLoading") }),
    );
    try {
      diagnostics = await deps.diagnostics();
      diagnosticsError = null;
    } catch (err) {
      diagnostics = null;
      diagnosticsError = messageOf(err);
    } finally {
      diagnosticsBusy = false;
      if (diagnosticsRefresh) diagnosticsRefresh.disabled = false;
      renderDiagnostics();
    }
  }

  function renderDiagnostics(): void {
    if (!diagnosticsBody) return;
    clear(diagnosticsBody);
    if (diagnosticsCopy) diagnosticsCopy.disabled = diagnostics === null;
    if (!diagnostics) {
      diagnosticsBody.append(
        el("p", {
          class: "modal-error",
          text: t("settings.diagnosticsFailed", {
            error: diagnosticsError ?? t("error.unknown"),
          }),
        }),
      );
      return;
    }
    const data = diagnostics;

    const facts = el("dl", { class: "diag-facts" });
    const rows: [string, string][] = [
      [t("settings.hubVersion"), data.hub_version],
      [t("settings.protocol"), data.protocol],
      [t("settings.pid"), String(data.pid)],
      [t("settings.pipe"), data.pipe],
      [t("settings.configDir"), data.config_dir],
    ];
    for (const [label, value] of rows) {
      facts.append(
        el("div", { class: "diag-fact" }, [
          el("dt", { text: label }),
          el("dd", { class: "mono", text: value }),
        ]),
      );
    }

    const adapterBox = el("div", { class: "diag-adapters" }, [
      el("h4", { class: "diag-subtitle", text: t("adapters.title") }),
    ]);
    if (data.adapters.length === 0) {
      adapterBox.append(el("p", { class: "muted", text: "—" }));
    }
    for (const adapter of data.adapters) {
      adapterBox.append(
        el("div", { class: "diag-adapter" }, [
          el("span", { class: "diag-adapter-name", text: adapter.name }),
          el("span", { class: "tag", text: adapterKind(adapter) }),
          el("span", { class: "mono muted", text: adapter.id }),
          el("span", { class: "muted", text: adapterStatus(adapter) }),
        ]),
      );
    }

    const errorBox = el("div", { class: "diag-errors" }, [
      el("h4", { class: "diag-subtitle", text: t("settings.recentErrors") }),
    ]);
    if (data.recent_errors.length === 0) {
      errorBox.append(el("p", { class: "muted", text: t("settings.noErrors") }));
    } else {
      const list = el("ul", { class: "diag-error-list" });
      for (const entry of data.recent_errors) {
        const head = el("div", { class: "diag-error-head" }, [
          el("span", { class: "mono", text: clock(entry.at_ms) }),
          el("span", { class: "mono muted", text: entry.method }),
          entry.kind ? el("span", { class: "tag tag-warn", text: entry.kind }) : el("span", {}),
        ]);
        list.append(
          el("li", { class: "diag-error" }, [head, el("p", { class: "diag-message", text: entry.message })]),
        );
      }
      errorBox.append(list);
    }

    diagnosticsBody.append(facts, adapterBox, errorBox);
  }

  async function copyDiagnostics(): Promise<void> {
    if (!diagnostics || !diagnosticsCopy) return;
    const button = diagnosticsCopy;
    try {
      await navigator.clipboard.writeText(diagnosticsText(diagnostics));
      button.textContent = t("settings.copied");
    } catch {
      button.textContent = t("settings.copyFailed");
    }
    window.setTimeout(() => {
      if (button.isConnected) button.textContent = t("settings.copy");
    }, 1600);
  }

  function diagnosticsSection(): HTMLElement {
    if (diagnosticsNode) return diagnosticsNode;
    diagnosticsBody = el("div", { class: "diag-body" });
    diagnosticsRefresh = el("button", {
      class: "button button-small",
      type: "button",
      text: t("settings.refresh"),
      onclick: () => void loadDiagnostics(),
    });
    diagnosticsCopy = el("button", {
      class: "button button-small",
      type: "button",
      text: t("settings.copy"),
      disabled: true,
      onclick: () => void copyDiagnostics(),
    });
    diagnosticsNode = el("section", { class: "diagnostics" }, [
      el("hr", { class: "modal-divider" }),
      el("div", { class: "diag-head" }, [
        el("h3", { class: "diag-title", text: t("settings.diagnostics") }),
        el("div", { class: "diag-actions" }, [diagnosticsRefresh, diagnosticsCopy]),
      ]),
      diagnosticsBody,
    ]);
    void loadDiagnostics();
    return diagnosticsNode;
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

  function title(): HTMLElement {
    return el("h2", { id: "settings-title", class: "modal-title", text: t("settings.title") });
  }

  function actions(save: HTMLButtonElement | null, cancel: HTMLButtonElement): HTMLElement {
    return el("div", { class: "modal-actions" }, [cancel, save ?? el("span", {})]);
  }

  function showLoadFailure(message: string): void {
    clear(dialog);
    const cancel = el("button", {
      class: "button",
      type: "button",
      text: t("settings.cancel"),
      onclick: close,
    });
    dialog.append(
      title(),
      el("p", { class: "modal-error", text: t("settings.loadFailed", { error: message }) }),
      actions(null, cancel),
      diagnosticsSection(),
    );
    cancel.focus();
  }

  async function persist(
    language: HTMLSelectElement,
    autoScan: HTMLInputElement,
    interval: HTMLSelectElement,
    batteryAlert: HTMLInputElement,
    threshold: HTMLInputElement,
    saveButton: HTMLButtonElement,
    cancelButton: HTMLButtonElement,
    errorNode: HTMLElement,
  ): Promise<void> {
    if (!ready || saving) return;
    const closeChoice = dialog.querySelector<HTMLInputElement>(
      'input[name="close_behaviour"]:checked',
    );
    const selected = language.value;
    const seconds = Number(interval.value);
    const next: Prefs = {
      language: isLanguage(selected) ? selected : getLanguage(),
      close_to_tray: closeChoice?.value !== "quit",
      auto_scan_secs: autoScan.checked
        ? Number.isFinite(seconds) && seconds >= 15
          ? Math.round(seconds)
          : DEFAULT_INTERVAL
        : 0,
      battery_alert: batteryAlert.checked,
      battery_alert_percent: clampThreshold(Number(threshold.value)),
    };

    saving = true;
    errorNode.hidden = true;
    saveButton.disabled = true;
    cancelButton.disabled = true;
    saveButton.textContent = t("status.saving");
    try {
      await deps.save(next);
      close();
    } catch (err) {
      saving = false;
      saveButton.disabled = false;
      cancelButton.disabled = false;
      saveButton.textContent = t("settings.save");
      errorNode.textContent = t("settings.saveFailed", { error: messageOf(err) });
      errorNode.hidden = false;
    }
  }

  function buildForm(current: Prefs): void {
    clear(dialog);
    ready = true;

    const language = el("select", { class: "select modal-select" });
    for (const option of LANGUAGE_OPTIONS) {
      const node = el("option", { value: option.id, text: option.label });
      if (option.id === current.language) node.selected = true;
      language.append(node);
    }

    const closeChoice = current.close_to_tray ? "tray" : "quit";

    const autoScan = el("input", { type: "checkbox", checked: current.auto_scan_secs > 0 });
    const chosenInterval =
      current.auto_scan_secs > 0 && Number.isFinite(current.auto_scan_secs)
        ? Math.round(current.auto_scan_secs)
        : DEFAULT_INTERVAL;
    const intervals = [...INTERVALS];
    if (!intervals.includes(chosenInterval)) intervals.push(chosenInterval);
    intervals.sort((a, b) => a - b);
    const interval = el("select", { class: "select modal-select" });
    for (const seconds of intervals) {
      const node = el("option", {
        value: String(seconds),
        text: t("settings.interval", { seconds }),
      });
      if (seconds === chosenInterval) node.selected = true;
      interval.append(node);
    }
    interval.disabled = !autoScan.checked;
    autoScan.addEventListener("change", () => {
      interval.disabled = !autoScan.checked;
    });

    const batteryAlert = el("input", { type: "checkbox", checked: current.battery_alert });
    const threshold = el("input", {
      type: "number",
      class: "input",
      min: String(MIN_ALERT),
      max: String(MAX_ALERT),
      step: "1",
      value: String(clampThreshold(current.battery_alert_percent)),
      "aria-label": t("settings.batteryThreshold"),
    });
    threshold.disabled = !batteryAlert.checked;
    batteryAlert.addEventListener("change", () => {
      threshold.disabled = !batteryAlert.checked;
    });

    const errorNode = el("p", { class: "modal-error", hidden: true });

    const saveButton = el("button", {
      class: "button button-primary",
      type: "button",
      text: t("settings.save"),
    });
    const cancelButton = el("button", {
      class: "button",
      type: "button",
      text: t("settings.cancel"),
      onclick: close,
    });
    saveButton.addEventListener("click", () => {
      void persist(
        language,
        autoScan,
        interval,
        batteryAlert,
        threshold,
        saveButton,
        cancelButton,
        errorNode,
      );
    });

    dialog.append(
      title(),
      el("label", { class: "field" }, [
        el("span", { class: "field-label", text: t("settings.language") }),
        language,
      ]),
      el("div", { class: "field" }, [
        el("span", { class: "field-label", id: "settings-close-label", text: t("settings.closeBehaviour") }),
        el("div", { class: "radio-group", role: "radiogroup", "aria-labelledby": "settings-close-label" }, [
          radioOption("close_behaviour", "tray", t("settings.hideToTray"), closeChoice === "tray"),
          radioOption("close_behaviour", "quit", t("settings.quitApp"), closeChoice === "quit"),
        ]),
      ]),
      el("div", { class: "field" }, [
        el("label", { class: "check-row" }, [
          autoScan,
          el("span", { text: t("settings.autoScan") }),
        ]),
        el("label", { class: "field-inline" }, [
          el("span", { class: "muted", text: t("settings.autoScanInterval") }),
          interval,
        ]),
      ]),
      el("div", { class: "field" }, [
        el("label", { class: "check-row" }, [
          batteryAlert,
          el("span", { text: t("settings.batteryAlert") }),
        ]),
        el("label", { class: "field-inline" }, [
          el("span", { class: "muted", text: t("settings.batteryThreshold") }),
          threshold,
        ]),
      ]),
      errorNode,
      actions(saveButton, cancelButton),
      diagnosticsSection(),
    );
    language.focus();
  }

  clear(dialog);
  dialog.append(
    title(),
    el("p", { class: "muted", text: t("settings.loading") }),
  );
  dialog.focus();
  ready = false;

  deps.load().then(buildForm, (err: unknown) => {
    showLoadFailure(messageOf(err));
  });
}
