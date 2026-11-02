import "./style.css";

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

import { render, type Actions, type UiState } from "./render";
import type {
  AdapterState,
  CapabilityDescriptor,
  CapabilityValue,
  DeviceDescriptor,
  HubFailure,
  Snapshot,
  WriteOutcome,
} from "./protocol";

const root = document.getElementById("app")!;

const state: UiState = {
  snapshot: null,
  values: new Map(),
  readErrors: new Map(),
  status: new Map(),
  busy: new Set(),
  online: false,
  version: null,
  error: null,
  loading: true,
};

function call<T>(method: string, params: unknown): Promise<T> {
  return invoke<T>("hub_request", { method, params });
}

function describe(err: unknown): HubFailure {
  if (typeof err === "string") return { message: err, retryable: false };
  if (err && typeof err === "object") {
    const failure = err as Partial<HubFailure>;
    return {
      message: failure.message ?? "unknown error",
      kind: failure.kind ?? null,
      retryable: Boolean(failure.retryable),
    };
  }
  return { message: String(err), retryable: false };
}

function key(device: string, capability: string): string {
  return `${device}/${capability}`;
}

async function refresh(): Promise<void> {
  state.loading = true;
  paint();
  try {
    const snapshot = await call<Snapshot>("devices.list", null);
    state.snapshot = snapshot;
    state.online = true;
    state.error = null;
    state.version = null;
    paint();
    await loadValues(snapshot.devices);
  } catch (err) {
    state.online = false;
    state.error = describe(err).message;
  } finally {
    state.loading = false;
    paint();
  }
}

async function loadValues(devices: DeviceDescriptor[]): Promise<void> {
  state.values.clear();
  state.readErrors.clear();
  for (const device of devices) {
    for (const capability of device.capabilities) {
      if (capability.access === "write_only") continue;
      const id = key(device.id, capability.id);
      try {
        const result = await call<{ value: CapabilityValue }>("capability.read", {
          device: device.id,
          capability: capability.id,
        });
        state.values.set(id, result.value);
      } catch (err) {
        state.readErrors.set(id, describe(err));
      }
      paint();
    }
  }
}

function valueOf(capability: CapabilityDescriptor, value: CapabilityValue | undefined): string {
  if (!value) return "—";
  switch (value.type) {
    case "int":
      return `${value.value}${capability.kind.type === "int" ? (capability.kind.unit ?? "") : ""}`;
    case "bool":
      return value.value ? "On" : "Off";
    case "enum": {
      const option = capability.kind.type === "enum" ? capability.kind.options.find((o) => o.value === value.value) : undefined;
      return option?.label ?? value.value;
    }
    case "rgb":
      return `#${[value.value.r, value.value.g, value.value.b].map((n) => n.toString(16).padStart(2, "0")).join("")}`;
    case "bytes":
      return `${value.value.length} bytes`;
  }
}

function setStatus(id: string, text: string, tone: "ok" | "error" | "saving") {
  state.status.set(id, { text, tone });
  paint();
}

async function write(
  device: DeviceDescriptor,
  capability: CapabilityDescriptor,
  value: CapabilityValue,
): Promise<void> {
  const id = key(device.id, capability.id);
  if (state.busy.has(id)) return;
  state.busy.add(id);
  state.values.set(id, value);
  setStatus(id, "Saving…", "saving");

  let attempt = 0;
  for (;;) {
    try {
      const outcome = await call<WriteOutcome>("capability.write", {
        device: device.id,
        capability: capability.id,
        value,
      });
      state.values.set(id, outcome.value);
      if (outcome.matched) {
        setStatus(id, `Synced · read back ${valueOf(capability, outcome.value)}`, "ok");
      } else if (outcome.readback_error) {
        setStatus(id, `Applied, read-back failed: ${outcome.readback_error}`, "error");
      } else {
        setStatus(id, `Device kept ${valueOf(capability, outcome.value)} instead`, "error");
      }
      break;
    } catch (err) {
      const failure = describe(err);
      // One automatic retry for errors the hub classifies as transient. Real
      // back-off policy belongs to the adapter; this keeps the UI honest about
      // what happened instead of hiding the second failure.
      if (failure.retryable && attempt === 0) {
        attempt += 1;
        setStatus(id, `Retrying after ${failure.kind ?? "error"}…`, "saving");
        await new Promise((resolve) => setTimeout(resolve, 300));
        continue;
      }
      state.values.delete(id);
      state.busy.delete(id);
      setStatus(id, `${failure.kind ?? "error"}: ${failure.message}`, "error");
      paint();
      return;
    }
  }
  state.busy.delete(id);
  paint();
}

async function toggleAdapter(adapter: AdapterState): Promise<void> {
  try {
    const snapshot = await call<Snapshot>("adapter.set_enabled", {
      adapter: adapter.id,
      enabled: !adapter.enabled,
    });
    state.snapshot = snapshot;
    paint();
    await loadValues(snapshot.devices);
  } catch (err) {
    state.error = describe(err).message;
    paint();
  }
}

const actions: Actions = {
  write: (device, capability, value) => void write(device, capability, value),
  toggleAdapter: (adapter) => void toggleAdapter(adapter),
  rescan: () => void refresh(),
};

function paint(): void {
  render(root, state, actions);
}

void listen<Snapshot>("hub://devices-changed", (event) => {
  state.snapshot = event.payload;
  paint();
  void loadValues(event.payload.devices);
}).catch((err) => {
  // Losing the push channel is not fatal: every action still talks to hubd.
  console.error("cannot subscribe to device changes", err);
});

window.addEventListener("unhandledrejection", (event) => {
  console.error("unhandled rejection", event.reason);
});

void refresh();
