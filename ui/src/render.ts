import { clear, el } from "./dom";
import { keymapEditor } from "./keymap";
import type {
  AdapterState,
  CapabilityDescriptor,
  CapabilityValue,
  DeviceDescriptor,
  HubFailure,
  Scene,
  SceneItem,
  SceneOptions,
  SceneRuntime,
  ScenesResult,
  Snapshot,
} from "./protocol";

export interface UiState {
  snapshot: Snapshot | null;
  values: Map<string, CapabilityValue>;
  readErrors: Map<string, HubFailure>;
  status: Map<string, { text: string; tone: "ok" | "error" | "saving" }>;
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
  if (state.snapshot) root.append(adapters(state.snapshot.adapters, state, actions));
  if (state.scenes && !state.scenesUnsupported) root.append(scenes(state.scenes, state, actions));
  root.append(devices(state, actions));
  root.append(footer());
}

function header(state: UiState, actions: Actions): HTMLElement {
  const pill = el("span", {
    class: `pill ${state.online ? "pill-online" : "pill-offline"}`,
    text: state.online ? "hubd connected" : "hubd not reachable",
  });
  return el("header", { class: "topbar" }, [
    el("div", { class: "brand" }, [
      el("div", { class: "brand-mark", text: "UP" }),
      el("div", {}, [
        el("h1", { text: "Universal Peripheral Hub" }),
        el("p", { class: "subtitle", text: "One process for displays, input, audio and more." }),
      ]),
    ]),
    el("div", { class: "topbar-actions" }, [
      pill,
      el("button", { class: "button", text: "Rescan", onclick: () => actions.rescan() }),
    ]),
  ]);
}

function offline(state: UiState, actions: Actions): HTMLElement {
  const hint = el("p", {
    class: "muted",
    text: "The resident core (hubd) is not answering. It is started automatically by the panel and by the tray icon.",
  });
  return el("section", { class: "banner banner-error" }, [
    el("div", {}, [
      el("strong", { text: "Cannot reach the hub" }),
      hint,
      state.error ? el("pre", { class: "detail", text: state.error }) : el("span", {}),
    ]),
    el("button", { class: "button", text: "Try again", onclick: () => actions.rescan() }),
  ]);
}

/** The hub answered, but the last action was refused (e.g. a block-list RPC). */
function actionError(message: string): HTMLElement {
  return el("section", { class: "banner banner-error" }, [
    el("div", {}, [
      el("strong", { text: "That action failed" }),
      el("p", { class: "muted", text: message }),
    ]),
  ]);
}

function adapters(list: AdapterState[], state: UiState, actions: Actions): HTMLElement {
  const strip = el("section", { class: "adapters" }, [
    el("h2", { class: "section-title", text: "Adapters" }),
  ]);
  const row = el("div", { class: "adapter-row" });
  // A missing block list (older hub, or nothing ignored) is simply empty.
  const blocked = state.snapshot?.blocked ?? {};
  for (const adapter of list) {
    const tokens = blocked[adapter.id] ?? [];
    const button = el("button", {
      class: `button button-small ${adapter.enabled ? "" : "button-ghost"}`,
      text: adapter.enabled ? "Disable" : "Enable",
      onclick: () => actions.toggleAdapter(adapter),
    });
    const meta = el("div", { class: "adapter-meta" });
    if (tokens.length > 0) meta.append(blockedChip(adapter, tokens.length, state, actions));
    meta.append(
      el("span", {
        class: "muted",
        text: adapter.enabled ? `${adapter.device_count} device(s)` : "disabled",
      }),
      button,
    );
    const node = el("div", { class: `adapter ${adapter.enabled ? "" : "adapter-off"}` }, [
      el("div", { class: "adapter-main" }, [
        el("span", { class: "adapter-name", text: adapter.name }),
        el("span", { class: "tag", text: adapter.kind === "in_process" ? "built-in" : "subprocess" }),
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
    title: `${count} ignored device token(s) on ${adapter.name}`,
    text: `${count} ignored`,
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
          text: "Restore",
          title: `Talk to ${token} again.`,
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
    el("h2", { class: "section-title", text: "Scenes" }),
  ]);
  const entries = Object.entries(result.document.scenes);
  if (entries.length === 0) {
    // A document with no scenes gets one honest line and no controls: an Apply
    // button without a scene, or toggles with nothing to apply, would lie.
    section.append(
      el("p", {
        class: "muted scene-empty",
        text: "No scenes yet. They live in %APPDATA%\\UniversalPeripheralHub\\scenes.json.",
      }),
    );
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
        title: "Selected by the current rule.",
      }),
    );
  }
  head.append(el("span", { class: "scene-name", text: scene.name }));
  if (current) head.append(el("span", { class: "scene-pill-current", text: "current" }));
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
      text: "Apply",
      title: `Apply “${scene.name}” now.`,
      disabled: state.scenesBusy,
      onclick: () => actions.applyScene(id),
    }),
  ]);
}

/** `3 values`, or the capability types themselves when there are at most two. */
function sceneSummary(items: SceneItem[]): { text: string; title: string; mono: boolean } {
  const types = items.map((item) => item.cap_type);
  if (types.length === 0) {
    return { text: "0 values", title: "This scene has no items.", mono: false };
  }
  if (types.length <= 2) {
    return { text: types.join(", "), title: types.join("\n"), mono: true };
  }
  return { text: `${types.length} values`, title: types.join("\n"), mono: false };
}

/** Engine state: baseline, current auto match, and what the last apply did. */
function sceneEngine(result: ScenesResult): HTMLElement {
  const { runtime, document } = result;
  const box = el("div", { class: "scene-engine" });
  if (!runtime.armed) {
    box.append(
      el("p", {
        class: "scene-engine-line",
        text: "Baseline not recorded yet (nothing will be written).",
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
          ? `Applied manually: ${name}`
          : `Auto: ${runtime.current_exe} → ${name}`;
      box.append(el("p", { class: "scene-engine-line", text }));
    } else {
      box.append(
        el("p", {
          class: "scene-engine-line",
          text: `Auto: no rule matches ${runtime.current_exe}`,
        }),
      );
    }
  }

  const report = runtime.last_report;
  if (report) {
    box.append(
      el("p", {
        class: "scene-report",
        text: `Wrote ${report.written} · unchanged ${report.unchanged} · skipped ${report.skipped}`,
      }),
    );
    const issues = [...(report.failed ?? []), ...(report.missing ?? [])];
    if (issues.length > 0) {
      const [first, ...rest] = issues;
      box.append(
        el("p", {
          class: "scene-issues",
          text: rest.length === 0 ? first : `${first} +${rest.length} more`,
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
      "Allow changes that briefly blank the display (HDR, picture mode)",
      Boolean(options.allow_side_effects),
      state.scenesBusy,
      (checked) => actions.setSceneOption("allow_side_effects", checked),
    ),
    optionToggle("Apply at startup", Boolean(options.startup_apply), state.scenesBusy, (checked) =>
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
        el("strong", { text: state.loading ? "Looking for devices…" : "No devices" }),
        el("p", {
          class: "muted",
          text: state.loading
            ? "Asking every enabled adapter for its device list."
            : "All adapters are disabled, or no adapter reported a device.",
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
  const card = el("article", { class: "card" }, [
    el("div", { class: "card-head" }, [
      el("div", {}, [
        el("h3", { text: device.name }),
        subtitle ? el("p", { class: "muted", text: subtitle }) : el("span", {}),
        device.serial ? el("p", { class: "mono muted", text: device.serial }) : el("span", {}),
      ]),
      el("div", { class: "card-head-side" }, [
        el("span", { class: "tag", text: device.adapter }),
        el("button", {
          class: "link-button",
          type: "button",
          text: "Ignore",
          title: "Stop talking to this monitor. Use this for a KVM or splitter that hangs the DDC bus.",
          onclick: () => actions.ignoreDevice(device),
        }),
      ]),
    ]),
  ]);

  const rows = el("div", { class: "rows" });
  for (const capability of device.capabilities) {
    rows.append(capabilityRow(device, capability, state, actions));
  }
  card.append(rows);
  return card;
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
    el("span", { class: "muted", text: `${capability.cost.latency_ms} ms` }),
  ];
  if (capability.access === "read_only") meta.push(el("span", { class: "tag", text: "read-only" }));
  if (capability.cost.side_effects.includes("signal_renegotiation")) {
    meta.push(el("span", { class: "tag tag-warn", text: "may renegotiate the signal" }));
  }

  const control = buildControl(device, capability, value, actions);

  const feedback = el("div", { class: "feedback" });
  if (status) {
    feedback.append(el("span", { class: `status status-${status.tone}`, text: status.text }));
  } else if (readError) {
    feedback.append(
      el("span", { class: "status status-error", text: `${readError.kind ?? "error"}: ${readError.message}` }),
    );
  } else if (capability.access === "write_only") {
    feedback.append(el("span", { class: "muted", text: "write-only" }));
  } else {
    feedback.append(el("span", { class: "muted", text: value === undefined ? "reading…" : "read back from device" }));
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
      return el("div", { class: "control" }, [toggle, el("span", { class: "muted", text: current ? "On" : "Off" })]);
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
        el("span", { class: "tag", text: "raw bytes" }),
      ]);
    }
  }
}

function footer(): HTMLElement {
  return el("footer", { class: "footer" }, [
    el("span", { text: "Device cards are generated from adapter capability descriptors." }),
    el("span", { class: "mono", text: "M0 · mock adapter" }),
  ]);
}
