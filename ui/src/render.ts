import { clear, el } from "./dom";
import type {
  AdapterState,
  CapabilityDescriptor,
  CapabilityValue,
  DeviceDescriptor,
  HubFailure,
  Snapshot,
} from "./protocol";

export interface UiState {
  snapshot: Snapshot | null;
  values: Map<string, CapabilityValue>;
  readErrors: Map<string, HubFailure>;
  status: Map<string, { text: string; tone: "ok" | "error" | "saving" }>;
  busy: Set<string>;
  online: boolean;
  version: string | null;
  error: string | null;
  loading: boolean;
}

export interface Actions {
  write(device: DeviceDescriptor, capability: CapabilityDescriptor, value: CapabilityValue): void;
  toggleAdapter(adapter: AdapterState): void;
  rescan(): void;
}

function key(device: string, capability: string): string {
  return `${device}/${capability}`;
}

export function render(root: HTMLElement, state: UiState, actions: Actions): void {
  clear(root);
  root.append(header(state, actions));
  if (!state.online) root.append(offline(state, actions));
  if (state.snapshot) root.append(adapters(state.snapshot.adapters, state, actions));
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

function adapters(list: AdapterState[], state: UiState, actions: Actions): HTMLElement {
  const strip = el("section", { class: "adapters" }, [
    el("h2", { class: "section-title", text: "Adapters" }),
  ]);
  const row = el("div", { class: "adapter-row" });
  for (const adapter of list) {
    const button = el("button", {
      class: `button button-small ${adapter.enabled ? "" : "button-ghost"}`,
      text: adapter.enabled ? "Disable" : "Enable",
      disabled: state.busy.has(adapter.id),
      onclick: () => actions.toggleAdapter(adapter),
    });
    row.append(
      el("div", { class: `adapter ${adapter.enabled ? "" : "adapter-off"}` }, [
        el("div", { class: "adapter-main" }, [
          el("span", { class: "adapter-name", text: adapter.name }),
          el("span", { class: "tag", text: adapter.kind === "in_process" ? "built-in" : "subprocess" }),
          el("span", { class: "muted", text: `v${adapter.version}` }),
        ]),
        el("div", { class: "adapter-meta" }, [
          el("span", {
            class: "muted",
            text: adapter.enabled ? `${adapter.device_count} device(s)` : "disabled",
          }),
          button,
        ]),
      ]),
    );
  }
  strip.append(row);
  return strip;
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
      el("span", { class: "tag", text: device.adapter }),
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
  const busy = state.busy.has(id);
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

  const control = buildControl(device, capability, value, busy, actions);

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

  return el("div", { class: "row" }, [
    el("div", { class: "row-info" }, [title, el("div", { class: "cap-meta" }, meta)]),
    el("div", { class: "row-control" }, [control, feedback]),
  ]);
}

function buildControl(
  device: DeviceDescriptor,
  capability: CapabilityDescriptor,
  value: CapabilityValue | undefined,
  busy: boolean,
  actions: Actions,
): HTMLElement {
  const disabled = busy || capability.access === "read_only";
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
