import { clear, el } from "./dom";
import { failureText } from "./failures";
import { t } from "./i18n";
import { keymapEditor } from "./keymap";
import type {
  AdapterState,
  CapabilityDescriptor,
  CapabilityValue,
  DeviceDescriptor,
  HistoryEntry,
  HubFailure,
  Scene,
  SceneItem,
  SceneOptions,
  SceneRuntime,
  ScenesResult,
  Snapshot,
} from "./protocol";

export interface StatusEntry {
  text: string;
  tone: "ok" | "error" | "saving";
  /// Present after a failed write: resend this value when Retry is clicked.
  retry?: { device: string; capability: string; value: CapabilityValue };
}

export interface UiState {
  snapshot: Snapshot | null;
  values: Map<string, CapabilityValue>;
  readErrors: Map<string, HubFailure>;
  status: Map<string, StatusEntry>;
  online: boolean;
  version: string | null;
  error: string | null;
  loading: boolean;
  /// Adapter ids whose ignored-device list is expanded in the adapters strip.
  blockedOpen: Set<string>;
  /// Last `scenes.get` / `scenes.set` / `scene.apply` / `scenes-changed` result.
  /// Null while the first read is still in flight.
  scenes: ScenesResult | null;
  /// A scenes RPC failed; shown in the same banner as a failed device action.
  scenesError: string | null;
  /// The running hubd answered `scenes.get` with "method not implemented".
  /// Only then is the section hidden instead of reporting an error.
  scenesUnsupported: boolean;
  /// One scenes RPC at a time, so two full-document writes cannot race.
  scenesBusy: boolean;
  /// Device whose detail page replaces the card grid; null on the grid.
  detailDeviceId: string | null;
  /// Newest-first undo history for the open detail device.
  history: HistoryEntry[];
  /// A `device.history` read is in flight.
  historyLoading: boolean;
  /// A `device.undo` call is in flight.
  undoBusy: boolean;
  /// Transient "Reverted" note shown next to the undo button.
  undoNote: string | null;
}

export interface Actions {
  write(device: DeviceDescriptor, capability: CapabilityDescriptor, value: CapabilityValue): void;
  toggleAdapter(adapter: AdapterState): void;
  rescan(): void;
  ignoreDevice(device: DeviceDescriptor): void;
  restoreDevice(adapter: AdapterState, token: string): void;
  toggleBlockedList(adapterId: string): void;
  applyScene(sceneId: string): void;
  setSceneOption(option: "allow_side_effects" | "startup_apply", checked: boolean): void;
  /** Open the settings overlay (language, close behaviour, scan, battery alert). */
  openSettings(): void;
  /** Replace the card grid with one device's detail page. */
  openDevice(deviceId: string): void;
  /** Return from the detail page to the card grid. */
  closeDevice(): void;
  /** Revert the newest change on the open device. */
  undo(deviceId: string): void;
  /** Resend the value of a failed write. */
  retry(deviceId: string, capabilityId: string, value: CapabilityValue): void;
  /** Re-render from the current state without touching the hub. */
  redraw(): void;
}

function key(device: string, capability: string): string {
  return `${device}/${capability}`;
}

export function render(root: HTMLElement, state: UiState, actions: Actions): void {
  clear(root);
  root.append(header(state, actions));
  if (!state.online) {
    root.append(offline(state, actions));
  } else {
    const error = state.error ?? state.scenesError;
    if (error) root.append(actionError(error));
  }
  const detailDevice =
    state.detailDeviceId && state.snapshot
      ? (state.snapshot.devices.find((device) => device.id === state.detailDeviceId) ?? null)
      : null;
  if (detailDevice) {
    root.append(detail(detailDevice, state, actions));
  } else {
    if (state.snapshot) root.append(adapters(state.snapshot.adapters, state, actions));
    if (state.scenes && !state.scenesUnsupported) root.append(scenes(state.scenes, state, actions));
    root.append(devices(state, actions));
  }
  root.append(footer(state));
}

function header(state: UiState, actions: Actions): HTMLElement {
  const pill = el("span", {
    class: `pill ${state.online ? "pill-online" : "pill-offline"}`,
    text: state.online ? t("app.connected") : t("app.disconnected"),
  });
  return el("header", { class: "topbar" }, [
    el("div", { class: "brand" }, [
      el("div", { class: "brand-mark", text: "UP" }),
      el("div", {}, [
        el("h1", { text: t("app.title") }),
        el("p", { class: "subtitle", text: t("app.subtitle") }),
      ]),
    ]),
    el("div", { class: "topbar-actions" }, [
      pill,
      el("button", { class: "button", text: t("app.rescan"), onclick: () => actions.rescan() }),
      el("button", {
        class: "button button-icon",
        type: "button",
        text: "⚙",
        title: t("app.settings"),
        "aria-label": t("app.settings"),
        onclick: () => actions.openSettings(),
      }),
    ]),
  ]);
}

function offline(state: UiState, actions: Actions): HTMLElement {
  const hint = el("p", { class: "muted", text: t("offline.body") });
  return el("section", { class: "banner banner-error" }, [
    el("div", {}, [
      el("strong", { text: t("offline.title") }),
      hint,
      state.error ? el("pre", { class: "detail", text: state.error }) : el("span", {}),
    ]),
    el("button", { class: "button", text: t("offline.retry"), onclick: () => actions.rescan() }),
  ]);
}

/** The hub answered, but the last action was refused (e.g. a block-list RPC). */
function actionError(message: string): HTMLElement {
  return el("section", { class: "banner banner-error" }, [
    el("div", {}, [
      el("strong", { text: t("error.actionFailed") }),
      el("p", { class: "muted", text: message }),
    ]),
  ]);
}

function adapters(list: AdapterState[], state: UiState, actions: Actions): HTMLElement {
  const strip = el("section", { class: "adapters" }, [
    el("h2", { class: "section-title", text: t("adapters.title") }),
  ]);
  const row = el("div", { class: "adapter-row" });
  // A missing block list (older hub, or nothing ignored) is simply empty.
  const blocked = state.snapshot?.blocked ?? {};
  for (const adapter of list) {
    const tokens = blocked[adapter.id] ?? [];
    const button = el("button", {
      class: `button button-small ${adapter.enabled ? "" : "button-ghost"}`,
      text: adapter.enabled ? t("adapters.disable") : t("adapters.enable"),
      onclick: () => actions.toggleAdapter(adapter),
    });
    const meta = el("div", { class: "adapter-meta" });
    if (tokens.length > 0) meta.append(blockedChip(adapter, tokens.length, state, actions));
    meta.append(
      el("span", {
        class: "muted",
        text: adapter.enabled
          ? t("adapters.deviceCount", { count: adapter.device_count })
          : t("adapters.disabled"),
      }),
      button,
    );
    const node = el("div", { class: `adapter ${adapter.enabled ? "" : "adapter-off"}` }, [
      el("div", { class: "adapter-main" }, [
        el("span", { class: "adapter-name", text: adapter.name }),
        el("span", {
          class: "tag",
          text: adapter.kind === "in_process" ? t("adapters.builtIn") : t("adapters.subprocess"),
        }),
        el("span", { class: "muted", text: `v${adapter.version}` }),
      ]),
      meta,
    ]);
    if (tokens.length > 0 && state.blockedOpen.has(adapter.id)) {
      node.append(blockedList(adapter, tokens, actions));
    }
    row.append(node);
  }
  strip.append(row);
  return strip;
}

/** Small muted chip in the adapters strip; toggles the ignored-device list. */
function blockedChip(adapter: AdapterState, count: number, state: UiState, actions: Actions): HTMLElement {
  const open = state.blockedOpen.has(adapter.id);
  return el("button", {
    class: `blocked-chip ${open ? "blocked-chip-open" : ""}`,
    type: "button",
    "aria-expanded": open ? "true" : "false",
    title: t("adapters.ignoredTitle", { count, adapter: adapter.name }),
    text: t("adapters.ignoredCount", { count }),
    onclick: () => actions.toggleBlockedList(adapter.id),
  });
}

/** The expanded list of one adapter's ignored tokens, each restorable. */
function blockedList(adapter: AdapterState, tokens: string[], actions: Actions): HTMLElement {
  const list = el("ul", { class: "blocked-list" });
  for (const token of tokens) {
    list.append(
      el("li", { class: "blocked-row" }, [
        el("span", { class: "blocked-token mono", text: token }),
        el("button", {
          class: "link-button",
          type: "button",
          text: t("adapters.restore"),
          title: t("adapters.restoreTitle", { token }),
          onclick: () => actions.restoreDevice(adapter, token),
        }),
      ]),
    );
  }
  return list;
}

/**
 * The scenes document plus what the engine is doing right now. The backend
 * owns rules and startup behaviour; the panel only edits the two options and
 * asks for one apply, so there is no local rule editor here.
 */
function scenes(result: ScenesResult, state: UiState, actions: Actions): HTMLElement {
  const section = el("section", { class: "scenes" }, [
    el("h2", { class: "section-title", text: t("scenes.title") }),
  ]);
  const entries = Object.entries(result.document.scenes);
  if (entries.length === 0) {
    // A document with no scenes gets one honest line and no controls: an Apply
    // button without a scene, or toggles with nothing to apply, would lie.
    section.append(el("p", { class: "muted scene-empty", text: t("scenes.empty") }));
    return section;
  }
  const list = el("div", { class: "scene-list" });
  for (const [id, scene] of entries) {
    list.append(sceneRow(id, scene, result.runtime, state, actions));
  }
  section.append(
    list,
    sceneEngine(result),
    sceneOptions(result.document.options, state, actions),
  );
  return section;
}

function sceneRow(
  id: string,
  scene: Scene,
  runtime: SceneRuntime,
  state: UiState,
  actions: Actions,
): HTMLElement {
  const current = runtime.current_scene === id;
  const summary = sceneSummary(scene.items);

  const head = el("div", { class: "scene-head" });
  if (current) {
    head.append(
      el("span", {
        class: "scene-dot",
        "aria-hidden": "true",
        title: t("scenes.selectedByRule"),
      }),
    );
  }
  head.append(el("span", { class: "scene-name", text: scene.name }));
  if (current) head.append(el("span", { class: "scene-pill-current", text: t("scenes.current") }));
  head.append(
    el("span", {
      class: summary.mono ? "scene-summary mono" : "scene-summary",
      title: summary.title,
      text: summary.text,
    }),
  );

  return el("div", { class: current ? "scene-row scene-row-current" : "scene-row" }, [
    head,
    el("button", {
      class: "button button-small",
      type: "button",
      text: t("scenes.apply"),
      title: t("scenes.applyTitle", { name: scene.name }),
      disabled: state.scenesBusy,
      onclick: () => actions.applyScene(id),
    }),
  ]);
}

/** `3 values`, or the capability types themselves when there are at most two. */
function sceneSummary(items: SceneItem[]): { text: string; title: string; mono: boolean } {
  const types = items.map((item) => item.cap_type);
  if (types.length === 0) {
    return { text: t("scenes.valuesCount", { count: 0 }), title: t("scenes.noItems"), mono: false };
  }
  if (types.length <= 2) {
    return { text: types.join(", "), title: types.join("\n"), mono: true };
  }
  return {
    text: t("scenes.valuesCount", { count: types.length }),
    title: types.join("\n"),
    mono: false,
  };
}

/** Engine state: baseline, current auto match, and what the last apply did. */
function sceneEngine(result: ScenesResult): HTMLElement {
  const { runtime, document } = result;
  const box = el("div", { class: "scene-engine" });
  if (!runtime.armed) {
    box.append(
      el("p", {
        class: "scene-engine-line",
        text: t("scenes.baselineMissing"),
      }),
    );
  } else if (runtime.current_exe) {
    if (runtime.current_scene) {
      const name = document.scenes[runtime.current_scene]?.name ?? runtime.current_scene;
      // "Auto" only when a rule actually picked it: a manual Apply leaves the
      // scene in place without the rule for this exe agreeing, and claiming
      // otherwise would be a small lie the user cannot debug.
      const text =
        runtime.last_source === "manual"
          ? t("scenes.appliedManually", { name })
          : t("scenes.autoMatch", { exe: runtime.current_exe, name });
      box.append(el("p", { class: "scene-engine-line", text }));
    } else {
      box.append(
        el("p", {
          class: "scene-engine-line",
          text: t("scenes.autoNoMatch", { exe: runtime.current_exe }),
        }),
      );
    }
  }

  const report = runtime.last_report;
  if (report) {
    box.append(
      el("p", {
        class: "scene-report",
        text: t("scenes.report", {
          written: report.written,
          unchanged: report.unchanged,
          skipped: report.skipped,
        }),
      }),
    );
    const issues = [...(report.failed ?? []), ...(report.missing ?? [])];
    if (issues.length > 0) {
      const [first, ...rest] = issues;
      box.append(
        el("p", {
          class: "scene-issues",
          text:
            rest.length === 0
              ? first
              : t("scenes.moreIssues", { first: first ?? "", count: rest.length }),
          title: issues.join("\n"),
        }),
      );
    }
  }
  return box;
}

function sceneOptions(options: SceneOptions, state: UiState, actions: Actions): HTMLElement {
  return el("div", { class: "scene-options" }, [
    optionToggle(
      t("scenes.allowSideEffects"),
      Boolean(options.allow_side_effects),
      state.scenesBusy,
      (checked) => actions.setSceneOption("allow_side_effects", checked),
    ),
    optionToggle(t("scenes.applyAtStartup"), Boolean(options.startup_apply), state.scenesBusy, (checked) =>
      actions.setSceneOption("startup_apply", checked),
    ),
  ]);
}

function optionToggle(
  label: string,
  checked: boolean,
  disabled: boolean,
  onchange: (checked: boolean) => void,
): HTMLElement {
  const input = el("input", { type: "checkbox", checked, disabled }) as HTMLInputElement;
  input.addEventListener("change", () => onchange(input.checked));
  return el("label", { class: "scene-option" }, [input, el("span", { text: label })]);
}

function devices(state: UiState, actions: Actions): HTMLElement {
  const section = el("section", { class: "devices" });
  const devices = state.snapshot?.devices ?? [];
  if (devices.length === 0) {
    section.append(
      el("div", { class: "empty" }, [
        el("strong", {
          text: state.loading ? t("devices.loadingTitle") : t("devices.emptyTitle"),
        }),
        el("p", {
          class: "muted",
          text: state.loading ? t("devices.loadingBody") : t("devices.emptyBody"),
        }),
      ]),
    );
    return section;
  }
  const grid = el("div", { class: "grid" });
  for (const device of devices) grid.append(deviceCard(device, state, actions));
  section.append(grid);
  return section;
}

function deviceCard(device: DeviceDescriptor, state: UiState, actions: Actions): HTMLElement {
  const subtitle = [device.vendor, device.model].filter(Boolean).join(" · ");
  // A read-only integer percentage (or an adapter-declared battery) is a
  // headline gauge, not a row: it moves into the card header.
  const battery = device.capabilities.find(isBatteryCapability);
  const titleRow = el("div", { class: "card-title-row" }, [el("h3", { text: device.name })]);
  if (battery) {
    titleRow.append(batteryRing(battery, state.values.get(key(device.id, battery.id))));
  }

  // The identity block is the "open details" hit area; the Ignore button and
  // the controls stay outside it so clicking them never navigates.
  const identity = el(
    "div",
    {
      class: "card-identity",
      title: t("detail.openLabel", { name: device.name }),
      onclick: () => actions.openDevice(device.id),
    },
    [
      titleRow,
      subtitle ? el("p", { class: "muted", text: subtitle }) : el("span", {}),
      device.serial ? el("p", { class: "mono muted", text: device.serial }) : el("span", {}),
    ],
  );

  const card = el(
    "article",
    {
      class: "card card-openable",
      tabindex: "0",
      role: "group",
      "aria-label": t("detail.openLabel", { name: device.name }),
      "data-device-id": device.id,
    },
    [
      el("div", { class: "card-head" }, [
        identity,
        el("div", { class: "card-head-side" }, [
          el("span", { class: "tag", text: device.adapter }),
          el("button", {
            class: "link-button",
            type: "button",
            text: t("devices.ignore"),
            title: t("devices.ignoreTitle"),
            onclick: () => actions.ignoreDevice(device),
          }),
        ]),
      ]),
    ],
  );

  // Enter/Space on the focused card opens it. Key events from the controls
  // inside the card bubble here, so only events targeting the card itself count.
  card.addEventListener("keydown", (event) => {
    if (event.target !== card) return;
    if (event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      actions.openDevice(device.id);
    }
  });

  const rows = el("div", { class: "rows" });
  for (const capability of device.capabilities) {
    if (capability === battery) continue;
    rows.append(capabilityRow(device, capability, state, actions));
  }
  card.append(rows);
  return card;
}

/** The detail page: identity, undo, then every capability grouped by prefix. */
function detail(device: DeviceDescriptor, state: UiState, actions: Actions): HTMLElement {
  const back = el("button", {
    class: "button detail-back",
    type: "button",
    text: t("detail.back"),
    onclick: () => actions.closeDevice(),
  });

  const subtitle = [device.vendor, device.model].filter(Boolean).join(" · ");
  const meta = el("div", { class: "detail-meta" }, [
    subtitle ? el("p", { class: "muted", text: subtitle }) : el("span", {}),
    device.serial ? el("p", { class: "mono muted", text: device.serial }) : el("span", {}),
    el("span", {
      class: "tag",
      text: device.adapter,
      title: t("detail.adapterTag", { adapter: device.adapter }),
    }),
  ]);

  const newest = state.history[0];
  const undoText = newest
    ? t("detail.undo", { label: newest.label, value: historyValue(device, newest) })
    : t("detail.undoEmpty");
  const undoButton = el("button", {
    class: "button button-small",
    type: "button",
    text: state.historyLoading && !newest ? t("card.reading") : undoText,
    title: newest ? t("detail.undo", { label: newest.label, value: historyValue(device, newest) }) : t("detail.undoEmpty"),
    disabled: state.undoBusy || state.historyLoading || !newest,
    onclick: () => actions.undo(device.id),
  });

  const undoRow = el("div", { class: "detail-undo" }, [undoButton]);
  if (state.history.length > 1) {
    undoRow.append(
      el("span", {
        class: "chip",
        text: String(state.history.length),
        title: t("detail.undoCount", { count: state.history.length }),
      }),
    );
  }
  if (state.undoNote) undoRow.append(el("span", { class: "note-ok", text: state.undoNote }));

  const section = el("section", { class: "device-detail" }, [
    el("div", { class: "detail-head" }, [
      back,
      el("div", { class: "detail-title" }, [el("h2", { text: device.name }), meta]),
      el("div", { class: "detail-actions" }, [undoRow]),
    ]),
  ]);

  for (const [prefix, capabilities] of groupCapabilities(device.capabilities)) {
    const rows = el("div", { class: "rows" });
    for (const capability of capabilities) rows.append(capabilityRow(device, capability, state, actions));
    section.append(
      el("div", { class: "detail-group" }, [
        el("h3", { class: "detail-group-title", text: groupTitle(prefix) }),
        rows,
      ]),
    );
  }
  return section;
}

/** Capabilities bucketed by the part of `cap_type` before the first dot. */
function groupCapabilities(
  capabilities: CapabilityDescriptor[],
): [string, CapabilityDescriptor[]][] {
  const groups = new Map<string, CapabilityDescriptor[]>();
  for (const capability of capabilities) {
    const prefix = capability.cap_type.split(".")[0] || capability.cap_type;
    const list = groups.get(prefix);
    if (list) list.push(capability);
    else groups.set(prefix, [capability]);
  }
  return [...groups.entries()];
}

/** Localized heading for a known prefix; adapter-specific prefixes stay as-is. */
function groupTitle(prefix: string): string {
  const translationKey = `group.${prefix}`;
  const text = t(translationKey);
  return text === translationKey ? prefix : text;
}

/** A history entry's value, using its capability descriptor when still present. */
function historyValue(device: DeviceDescriptor, entry: HistoryEntry): string {
  const capability =
    device.capabilities.find((item) => item.id === entry.capability) ??
    device.capabilities.find((item) => item.cap_type === entry.cap_type);
  if (!capability) return formatValue(entry.value);
  const value = entry.value;
  if (value.type === "int" && capability.kind.type === "int") {
    return `${value.value}${capability.kind.unit ?? ""}`;
  }
  if (value.type === "enum" && capability.kind.type === "enum") {
    const option = capability.kind.options.find((item) => item.value === value.value);
    return option?.label ?? value.value;
  }
  return formatValue(value);
}

/** The value alone, without a capability's unit (history labels). */
function formatValue(value: CapabilityValue): string {
  switch (value.type) {
    case "int":
      return String(value.value);
    case "bool":
      return value.value ? t("control.on") : t("control.off");
    case "enum":
      return value.value;
    case "rgb":
      return `#${[value.value.r, value.value.g, value.value.b]
        .map((n) => n.toString(16).padStart(2, "0"))
        .join("")}`;
    case "bytes":
      return t("card.bytes", { count: value.value.length });
    case "text":
      return value.value.length > 0 ? value.value : "—";
  }
}

/** A battery-like capability: read-only integer percentage, or `*.battery`. */
function isBatteryCapability(capability: CapabilityDescriptor): boolean {
  return (
    capability.access === "read_only" &&
    capability.kind.type === "int" &&
    (capability.kind.unit === "%" || capability.cap_type.endsWith(".battery"))
  );
}

const SVG_NS = "http://www.w3.org/2000/svg";
const BATTERY_SIZE = 34;
const BATTERY_STROKE = 3.5;

function svgEl(tag: string, attrs: Record<string, string | number>): SVGElement {
  const node = document.createElementNS(SVG_NS, tag) as SVGElement;
  for (const [name, value] of Object.entries(attrs)) node.setAttribute(name, String(value));
  return node;
}

function batteryColour(percent: number): string {
  if (percent >= 50) return "#3ddc97";
  if (percent >= 20) return "#e0a33e";
  return "#e05d5d";
}

/**
 * Small SVG donut for a battery reading. The arc is proportional to the
 * percentage and the number sits inside it; the wrapper carries the tooltip.
 */
function batteryRing(
  capability: CapabilityDescriptor,
  value: CapabilityValue | undefined,
): HTMLElement {
  const reading = value?.type === "int" ? value.value : null;
  const percent = Math.min(100, Math.max(0, reading ?? 0));
  const colour = batteryColour(percent);
  const radius = (BATTERY_SIZE - BATTERY_STROKE) / 2;
  const circumference = 2 * Math.PI * radius;
  const filled = (percent / 100) * circumference;
  const centre = BATTERY_SIZE / 2;

  const svg = svgEl("svg", {
    class: "battery-ring",
    width: BATTERY_SIZE,
    height: BATTERY_SIZE,
    viewBox: `0 0 ${BATTERY_SIZE} ${BATTERY_SIZE}`,
    role: "img",
    "aria-label": `${capability.label}: ${reading === null ? t("card.reading") : `${percent}%`}`,
  });
  svg.append(
    svgEl("circle", {
      class: "battery-track",
      cx: centre,
      cy: centre,
      r: radius,
      fill: "none",
      "stroke-width": BATTERY_STROKE,
    }),
    svgEl("circle", {
      class: "battery-arc",
      cx: centre,
      cy: centre,
      r: radius,
      fill: "none",
      stroke: colour,
      "stroke-width": BATTERY_STROKE,
      "stroke-linecap": "round",
      "stroke-dasharray": `${filled} ${circumference - filled}`,
      transform: `rotate(-90 ${centre} ${centre})`,
    }),
  );
  const number = svgEl("text", {
    class: "battery-number",
    x: centre,
    y: centre,
    "text-anchor": "middle",
    "dominant-baseline": "central",
    fill: colour,
  });
  number.textContent = reading === null ? "—" : String(Math.round(percent));
  svg.append(number);

  return el(
    "span",
    {
      class: "battery",
      title:
        reading === null
          ? capability.label
          : t("battery.tooltip", { label: capability.label, percent: Math.round(percent) }),
    },
    [svg],
  );
}

function capabilityRow(
  device: DeviceDescriptor,
  capability: CapabilityDescriptor,
  state: UiState,
  actions: Actions,
): HTMLElement {
  const id = key(device.id, capability.id);
  const value = state.values.get(id);
  const readError = state.readErrors.get(id);
  const status = state.status.get(id);

  const title = el("div", { class: "cap-title" }, [
    el("span", { class: "cap-label", text: capability.label }),
    el("span", { class: "cap-type mono", text: capability.cap_type }),
  ]);

  const meta: HTMLElement[] = [
    el("span", { class: "muted", text: t("card.latency", { ms: capability.cost.latency_ms }) }),
  ];
  if (capability.access === "read_only") {
    meta.push(el("span", { class: "tag", text: t("card.readOnly") }));
  }
  if (capability.cost.side_effects.includes("signal_renegotiation")) {
    meta.push(el("span", { class: "tag tag-warn", text: t("card.renegotiate") }));
  }

  const control = buildControl(device, capability, value, actions);

  const feedback = el("div", { class: "feedback" });
  if (status) {
    feedback.append(el("span", { class: `status status-${status.tone}`, text: status.text }));
    if (status.retry) {
      const retry = status.retry;
      feedback.append(
        el("button", {
          class: "button button-small retry-button",
          type: "button",
          text: t("status.retry"),
          onclick: () => actions.retry(retry.device, retry.capability, retry.value),
        }),
      );
    }
  } else if (readError) {
    feedback.append(el("span", { class: "status status-error", text: failureText(readError) }));
  } else if (capability.access === "write_only") {
    feedback.append(el("span", { class: "muted", text: t("card.writeOnly") }));
  } else {
    feedback.append(
      el("span", {
        class: "muted",
        text: value === undefined ? t("card.reading") : t("card.readBack"),
      }),
    );
  }

  if (capability.cap_type === "keyboard.keymap" && value?.type === "bytes") {
    return el("div", { class: "row row-wide" }, [
      el("div", { class: "row-info" }, [title, el("div", { class: "cap-meta" }, meta)]),
      el("div", { class: "row-control" }, [
        keymapEditor(device, capability, value, state, actions),
        feedback,
      ]),
    ]);
  }

  return el("div", { class: "row" }, [
    el("div", { class: "row-info" }, [title, el("div", { class: "cap-meta" }, meta)]),
    el("div", { class: "row-control" }, [control, feedback]),
  ]);
}

function buildControl(
  device: DeviceDescriptor,
  capability: CapabilityDescriptor,
  value: CapabilityValue | undefined,
  actions: Actions,
): HTMLElement {
  // Writes are queued per device (see main.ts), so the control stays live while
  // an earlier value is still on the bus: the newest value simply wins.
  const disabled = capability.access === "read_only";
  switch (capability.kind.type) {
    case "int": {
      // A disabled slider still looks draggable, so a read-only integer is
      // plain value text. (Battery-like ones live in the card header instead.)
      if (disabled) {
        const shown =
          value?.type === "int" ? `${value.value}${capability.kind.unit ?? ""}` : "—";
        return el("div", { class: "control" }, [el("span", { class: "mono muted", text: shown })]);
      }
      const min = capability.kind.min;
      const max = capability.kind.max;
      const step = capability.kind.step || 1;
      const current = value?.type === "int" ? value.value : min;
      const readout = el("span", { class: "readout mono", text: `${current}${capability.kind.unit ?? ""}` });
      const slider = el("input", {
        type: "range",
        min: String(min),
        max: String(max),
        step: String(step),
        value: String(current),
        disabled,
        class: "slider",
      }) as HTMLInputElement;
      slider.addEventListener("input", () => {
        readout.textContent = `${slider.value}${capability.kind.type === "int" ? (capability.kind.unit ?? "") : ""}`;
      });
      slider.addEventListener("change", () => {
        actions.write(device, capability, { type: "int", value: Number(slider.value) });
      });
      return el("div", { class: "control control-int" }, [slider, readout]);
    }
    case "bool": {
      const current = value?.type === "bool" ? value.value : false;
      const toggle = el("button", {
        class: `switch ${current ? "switch-on" : ""}`,
        role: "switch",
        "aria-checked": current ? "true" : "false",
        disabled,
        onclick: () => actions.write(device, capability, { type: "bool", value: !current }),
      }, [el("span", { class: "switch-knob" })]);
      return el("div", { class: "control" }, [toggle, el("span", { class: "muted", text: current ? t("control.on") : t("control.off") })]);
    }
    case "enum": {
      // A disabled dropdown still renders every option, which reads like the
      // device is offering a choice it will not accept. Read-only enums show
      // the current label instead.
      if (disabled) {
        const current = value?.type === "enum" ? value.value : undefined;
        const label =
          capability.kind.options.find((option) => option.value === current)?.label ?? current ?? "—";
        return el("div", { class: "control" }, [el("span", { class: "mono muted", text: label })]);
      }
      const select = el("select", { class: "select", disabled }) as HTMLSelectElement;
      for (const option of capability.kind.options) {
        const opt = el("option", { value: option.value, text: option.label }) as HTMLOptionElement;
        if (value?.type === "enum" && value.value === option.value) opt.selected = true;
        select.append(opt);
      }
      select.addEventListener("change", () => {
        actions.write(device, capability, { type: "enum", value: select.value });
      });
      return el("div", { class: "control" }, [select]);
    }
    case "rgb": {
      const current =
        value?.type === "rgb" ? `#${[value.value.r, value.value.g, value.value.b].map((n) => n.toString(16).padStart(2, "0")).join("")}` : "#ff7a00";
      const picker = el("input", { type: "color", value: current, disabled, class: "color" }) as HTMLInputElement;
      picker.addEventListener("change", () => {
        const hex = picker.value;
        actions.write(device, capability, {
          type: "rgb",
          value: {
            r: parseInt(hex.slice(1, 3), 16),
            g: parseInt(hex.slice(3, 5), 16),
            b: parseInt(hex.slice(5, 7), 16),
          },
        });
      });
      return el("div", { class: "control" }, [picker, el("span", { class: "mono muted", text: current })]);
    }
    case "text": {
      const shown = value?.type === "text" && value.value.length > 0 ? value.value : "—";
      return el("div", { class: "control" }, [el("span", { class: "mono muted", text: shown })]);
    }
    case "bytes": {
      const hex =
        value?.type === "bytes"
          ? value.value.slice(0, 16).map((byte) => byte.toString(16).padStart(2, "0")).join(" ") +
            (value.value.length > 16 ? " …" : "")
          : "—";
      return el("div", { class: "control" }, [
        el("span", { class: "mono muted", text: hex }),
        el("span", { class: "tag", text: t("card.rawBytes") }),
      ]);
    }
  }
}

function footer(state: UiState): HTMLElement {
  // The version comes from the running core, not from a hand-edited string:
  // the footer said "M0 · mock adapter" long after the mock stopped being the
  // only adapter in the build.
  return el("footer", { class: "footer" }, [
    el("span", { text: t("footer.note") }),
    el("span", {
      class: "mono",
      "data-hub-version": state.version ?? "",
      text: state.version ? `hubd ${state.version}` : "hubd",
    }),
  ]);
}
