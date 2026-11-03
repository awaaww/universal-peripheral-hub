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
  ScenesResult,
  Snapshot,
  WriteOutcome,
} from "./protocol";

const root = document.getElementById("app")!;

const state: UiState = {
  snapshot: null,
  values: new Map(),
  readErrors: new Map(),
  status: new Map(),
  online: false,
  version: null,
  error: null,
  loading: true,
  blockedOpen: new Set(),
  scenes: null,
  scenesError: null,
  scenesUnsupported: false,
  scenesBusy: false,
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

/** Adopt a snapshot from any RPC or push and repaint the panel. */
function applySnapshot(snapshot: Snapshot): void {
  state.snapshot = snapshot;
  state.online = true;
  state.error = null;
  state.version = null;
  paint();
}

/** Re-enumerate adapters, then read every capability again.
 *
 * `devices.list` only re-reads what the hub cached at adapter load, so a
 * wireless mouse that was asleep then stays missing until the adapter is
 * rebuilt. Rescan exists to make that rebuild reachable without a restart; an
 * older hubd that does not know the method still gets a plain refresh.
 */
async function rescan(): Promise<void> {
  state.loading = true;
  paint();
  void loadScenes();
  try {
    let snapshot: Snapshot;
    try {
      snapshot = await call<Snapshot>("adapters.reload", {});
    } catch {
      snapshot = await call<Snapshot>("devices.list", null);
    }
    applySnapshot(snapshot);
    await loadValues(snapshot.devices);
  } catch (err) {
    state.online = false;
    state.error = describe(err).message;
  } finally {
    state.loading = false;
    paint();
  }
}

async function refresh(): Promise<void> {
  state.loading = true;
  paint();
  // The scenes section is independent of the device snapshot; start its read
  // alongside `devices.list` so it does not wait behind every capability read.
  void loadScenes();
  try {
    const snapshot = await call<Snapshot>("devices.list", null);
    applySnapshot(snapshot);
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

/**
 * `RpcError::method_not_found` carries no `data`, so the Tauri bridge reports
 * it with `kind: null` and only this text. That is the one failure that should
 * hide the scenes section instead of showing the error banner.
 */
function isUnknownScenesMethod(failure: HubFailure): boolean {
  const kind = failure.kind ?? "";
  return (
    (kind === "" || kind === "method_not_found") &&
    /not implemented by this hub|method not found/i.test(failure.message)
  );
}

async function loadScenes(): Promise<void> {
  try {
    const result = await call<ScenesResult>("scenes.get", null);
    state.scenes = result;
    state.scenesUnsupported = false;
    state.scenesError = null;
  } catch (err) {
    const failure = describe(err);
    if (isUnknownScenesMethod(failure)) {
      state.scenes = null;
      state.scenesUnsupported = true;
    } else {
      state.scenesError = failure.message;
    }
  }
  paint();
}

/**
 * Persist one engine option. `allow_side_effects` changes what the next rule
 * evaluation may do, so it is sent as `map`; `startup_apply` is only a stored
 * preference, so it is a plain `save`.
 */
async function setSceneOption(
  option: "allow_side_effects" | "startup_apply",
  checked: boolean,
): Promise<void> {
  const current = state.scenes;
  if (!current || state.scenesBusy) return;

  const options = {
    ...current.document.options,
    allow_side_effects:
      option === "allow_side_effects" ? checked : Boolean(current.document.options.allow_side_effects),
    startup_apply:
      option === "startup_apply" ? checked : Boolean(current.document.options.startup_apply),
  };
  const document = { ...current.document, options };

  // Optimistic: the checkbox stays where the user put it while the document is
  // written. On failure it snaps back and the banner explains why.
  state.scenes = { ...current, document };
  state.scenesBusy = true;
  paint();
  try {
    const result = await call<ScenesResult>("scenes.set", {
      document,
      mode: option === "allow_side_effects" ? "map" : "save",
    });
    state.scenes = result;
    state.scenesError = null;
  } catch (err) {
    state.scenes = current;
    state.scenesError = describe(err).message;
  } finally {
    state.scenesBusy = false;
    paint();
  }
}

async function applyScene(sceneId: string): Promise<void> {
  if (!state.scenes || state.scenesBusy) return;
  state.scenesBusy = true;
  paint();
  try {
    state.scenes = await call<ScenesResult>("scene.apply", { scene: sceneId });
    state.scenesError = null;
  } catch (err) {
    state.scenesError = describe(err).message;
  } finally {
    state.scenesBusy = false;
    paint();
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
    case "text":
      return value.value.length > 0 ? value.value : "—";
  }
}

function setStatus(id: string, text: string, tone: "ok" | "error" | "saving") {
  state.status.set(id, { text, tone });
  paint();
}

interface PendingWrite {
  device: DeviceDescriptor;
  capability: CapabilityDescriptor;
  value: CapabilityValue;
}

/**
 * Pending writes, one FIFO per device (`deviceId -> capabilityId -> write`).
 *
 * The DDC bus is serial and slow: the hub sends one write to a device at a
 * time and the order matters, so a newer value waits in the queue instead of
 * being dropped while a write is in flight. Inside a device the queue is keyed
 * by capability, so a newer value for the same capability replaces the queued
 * one: a fast drag coalesces into the last value instead of replaying every
 * intermediate step.
 */
const pending = new Map<string, Map<string, PendingWrite>>();
const draining = new Set<string>();

function queueFor(deviceId: string): Map<string, PendingWrite> {
  let queue = pending.get(deviceId);
  if (!queue) {
    queue = new Map();
    pending.set(deviceId, queue);
  }
  return queue;
}

function firstQueued(queue: Map<string, PendingWrite> | undefined): PendingWrite | undefined {
  if (!queue) return undefined;
  for (const item of queue.values()) return item;
  return undefined;
}

function isQueued(deviceId: string, capabilityId: string): boolean {
  return pending.get(deviceId)?.has(capabilityId) ?? false;
}

function write(device: DeviceDescriptor, capability: CapabilityDescriptor, value: CapabilityValue): void {
  const id = key(device.id, capability.id);
  // Optimistic value: the slider keeps the position the user dragged to while
  // the write is on the wire.
  state.values.set(id, value);
  setStatus(id, "Saving…", "saving");
  queueFor(device.id).set(capability.id, { device, capability, value });
  void drain(device.id);
}

async function drain(deviceId: string): Promise<void> {
  if (draining.has(deviceId)) return;
  draining.add(deviceId);
  try {
    for (;;) {
      const queue = pending.get(deviceId);
      const next = firstQueued(queue);
      if (!queue || !next) break;
      // Take the write out before sending it: anything still in the queue is
      // by definition newer than the value currently on the bus.
      queue.delete(next.capability.id);
      await send(next);
    }
  } finally {
    draining.delete(deviceId);
    const queue = pending.get(deviceId);
    if (queue && queue.size === 0) pending.delete(deviceId);
  }
}

async function send(item: PendingWrite): Promise<void> {
  const id = key(item.device.id, item.capability.id);
  setStatus(id, "Saving…", "saving");

  let attempt = 0;
  for (;;) {
    try {
      const outcome = await call<WriteOutcome>("capability.write", {
        device: item.device.id,
        capability: item.capability.id,
        value: item.value,
      });
      // A newer value for this capability may have been queued while this one
      // was on the bus. Do not snap the slider back to the stale read-back.
      const superseded = isQueued(item.device.id, item.capability.id);
      if (!superseded) {
        state.values.set(id, outcome.value);
        if (outcome.matched) {
          setStatus(id, `Synced · read back ${valueOf(item.capability, outcome.value)}`, "ok");
        } else if (outcome.readback_error) {
          setStatus(id, `Applied, read-back failed: ${outcome.readback_error}`, "error");
        } else {
          setStatus(id, `Device kept ${valueOf(item.capability, outcome.value)} instead`, "error");
        }
      }
      return;
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
      // Drop the optimistic value unless a newer write is already queued for
      // the same capability; that queued value is what the user last asked for.
      if (!isQueued(item.device.id, item.capability.id)) {
        state.values.delete(id);
        setStatus(id, `${failure.kind ?? "error"}: ${failure.message}`, "error");
      }
      return;
    }
  }
}

async function toggleAdapter(adapter: AdapterState): Promise<void> {
  try {
    const snapshot = await call<Snapshot>("adapter.set_enabled", {
      adapter: adapter.id,
      enabled: !adapter.enabled,
    });
    applySnapshot(snapshot);
    await loadValues(snapshot.devices);
  } catch (err) {
    state.error = describe(err).message;
    paint();
  }
}

/**
 * A block-list target: a live device is addressed by id, an already ignored one
 * by the persisted (adapter, token) pair — blocked devices are no longer
 * enumerated, so they have no device id to address.
 */
type BlockTarget = { device: string } | { adapter: string; token: string };

async function setBlocked(target: BlockTarget, blocked: boolean): Promise<void> {
  try {
    const snapshot = await call<Snapshot>("device.set_blocked", { ...target, blocked });
    applySnapshot(snapshot);
    await loadValues(snapshot.devices);
    paint();
  } catch (err) {
    state.error = describe(err).message;
    paint();
  }
}

async function ignoreDevice(device: DeviceDescriptor): Promise<void> {
  const question =
    `Ignore “${device.name}”?\n\n` +
    "Stop talking to this monitor. Use this for a KVM or splitter that hangs the DDC bus.\n\n" +
    "You can restore it later from the adapters strip.";
  if (!window.confirm(question)) return;
  await setBlocked({ device: device.id }, true);
}

async function restoreDevice(adapter: AdapterState, token: string): Promise<void> {
  await setBlocked({ adapter: adapter.id, token }, false);
}

function toggleBlockedList(adapterId: string): void {
  if (state.blockedOpen.has(adapterId)) state.blockedOpen.delete(adapterId);
  else state.blockedOpen.add(adapterId);
  paint();
}

const actions: Actions = {
  write: (device, capability, value) => write(device, capability, value),
  toggleAdapter: (adapter) => void toggleAdapter(adapter),
  rescan: () => void rescan(),
  ignoreDevice: (device) => void ignoreDevice(device),
  restoreDevice: (adapter, token) => void restoreDevice(adapter, token),
  toggleBlockedList: (adapterId) => toggleBlockedList(adapterId),
  applyScene: (sceneId) => void applyScene(sceneId),
  setSceneOption: (option, checked) => void setSceneOption(option, checked),
  redraw: () => paint(),
};

function paint(): void {
  render(root, state, actions);
}

void listen<Snapshot>("hub://devices-changed", (event) => {
  // The push carries the same payload as devices.list, block list included.
  applySnapshot(event.payload);
  void loadValues(event.payload.devices);
}).catch((err) => {
  // Losing the push channel is not fatal: every action still talks to hubd.
  console.error("cannot subscribe to device changes", err);
});

void listen<ScenesResult>("hub://scenes-changed", (event) => {
  // Same payload as scenes.get/scenes.set: document plus engine runtime.
  state.scenes = event.payload;
  paint();
}).catch((err) => {
  console.error("cannot subscribe to scene changes", err);
});

window.addEventListener("unhandledrejection", (event) => {
  console.error("unhandled rejection", event.reason);
});

void refresh();
