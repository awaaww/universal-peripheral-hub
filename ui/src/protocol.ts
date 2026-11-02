//! Wire types mirrored from `crates/hub-core` and `crates/hub-proto`.
//!
//! These are produced by the backend, never hand-authored by the UI: the whole
//! point of the descriptor model is that new devices need no UI changes.

export type CapabilityKind =
  | { type: "int"; min: number; max: number; step: number; unit?: string | null }
  | { type: "bool" }
  | { type: "enum"; options: { value: string; label: string }[] }
  | { type: "rgb" }
  | { type: "bytes"; max_len: number };

export type SideEffect =
  | "signal_renegotiation"
  | "audio_route_change"
  | "exclusive_access"
  | "device_reset";

export interface CapabilityDescriptor {
  id: string;
  cap_type: string;
  label: string;
  kind: CapabilityKind;
  access: "read_write" | "read_only" | "write_only";
  cost: { latency_ms: number; side_effects: SideEffect[] };
  traits: { live: boolean; sceneable: boolean; bindable: boolean };
}

export interface DeviceDescriptor {
  id: string;
  adapter: string;
  name: string;
  vendor?: string | null;
  model?: string | null;
  serial?: string | null;
  capabilities: CapabilityDescriptor[];
}

export interface AdapterState {
  id: string;
  name: string;
  version: string;
  kind: "in_process" | "out_of_process";
  enabled: boolean;
  loaded: boolean;
  device_count: number;
}

export interface Snapshot {
  devices: DeviceDescriptor[];
  adapters: AdapterState[];
  revision: number;
  /// Devices the user asked the hub not to talk to, keyed by adapter id.
  /// Tokens are opaque to the UI: it renders and echoes them back unchanged.
  blocked?: Record<string, string[]>;
}

export type CapabilityValue =
  | { type: "int"; value: number }
  | { type: "bool"; value: boolean }
  | { type: "enum"; value: string }
  | { type: "rgb"; value: { r: number; g: number; b: number } }
  | { type: "bytes"; value: number[] };

export interface WriteOutcome {
  value: CapabilityValue;
  matched: boolean;
  readback_error?: string | null;
}

export interface HubFailure {
  message: string;
  kind?: string | null;
  retryable: boolean;
}

/// One change a scene asks for, addressed by stable `cap_type` (not by an
/// adapter-local capability id), so a scene survives a device being replaced.
export interface SceneItem {
  cap_type: string;
  device?: string | null;
  value: CapabilityValue;
}

export interface Scene {
  name: string;
  items: SceneItem[];
}

export interface SceneRule {
  exe?: string | null;
  default?: boolean;
  scene: string;
  priority?: number;
}

export interface SceneOptions {
  allow_side_effects?: boolean;
  startup_apply?: boolean;
}

/// The whole `scenes.json`, mirrored from `hub-core`.
export interface ScenesFile {
  version: 1;
  scenes: Record<string, Scene>;
  rules: SceneRule[];
  options: SceneOptions;
}

export interface ApplyReport {
  scene: string;
  written: number;
  unchanged: number;
  skipped: number;
  failed?: string[] | null;
  missing?: string[] | null;
}

export interface SceneRuntime {
  armed: boolean;
  current_scene?: string | null;
  current_exe?: string | null;
  last_applied_ms?: number | null;
  last_report?: ApplyReport | null;
  /** `startup` (baseline), `rule` (foreground change) or `manual` (Apply). */
  last_source?: "startup" | "rule" | "manual" | null;
}

export interface ScenesResult {
  document: ScenesFile;
  runtime: SceneRuntime;
}

/// `map` re-evaluates the current foreground executable (rules/options changed);
/// `save` only persists the document (value edits).
export type SceneMode = "map" | "save";
