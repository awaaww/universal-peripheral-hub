//! Wire types mirrored from `crates/hub-core` and `crates/hub-proto`.
//!
//! These are produced by the backend, never hand-authored by the UI: the whole
//! point of the descriptor model is that new devices need no UI changes.

export type CapabilityKind =
  | { type: "int"; min: number; max: number; step: number; unit?: string | null }
  | { type: "bool" }
  | { type: "enum"; options: { value: string; label: string }[] }
  | { type: "rgb" }
  | { type: "bytes"; max_len: number }
  | { type: "text"; max_len: number };

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
  | { type: "bytes"; value: number[] }
  | { type: "text"; value: string };

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

/// Persisted preferences, mirrored from the `prefs.*` RPCs and the
/// `prefs.changed` push.
export type Language = "en" | "zh-CN" | "zh-TW";

export interface Prefs {
  language: Language;
  /// `true` hides the window to the tray instead of quitting.
  close_to_tray: boolean;
  /// `0` disables the periodic rescan; otherwise the backend answers 15..=3600.
  auto_scan_secs: number;
  battery_alert: boolean;
  battery_alert_percent: number;
}

/// Backend defaults, used until `prefs.get` answers (or if that call fails).
export const DEFAULT_PREFS: Prefs = {
  language: "en",
  close_to_tray: true,
  auto_scan_secs: 0,
  battery_alert: false,
  battery_alert_percent: 20,
};

/// One undoable change, from `device.history` (newest first, at most 20).
export interface HistoryEntry {
  capability: string;
  cap_type: string;
  label: string;
  value: CapabilityValue;
  at_ms: number;
}

/// `device.history` response.
export interface HistoryResult {
  device: string;
  entries: HistoryEntry[];
}

/// `device.undo` response: the value the device holds after the revert.
export interface UndoResult {
  device: string;
  capability: string;
  cap_type: string;
  label: string;
  value: CapabilityValue;
  matched: boolean;
}

/// One entry of `diagnostics.get`'s recent-error ring.
export interface DiagnosticsError {
  at_ms: number;
  method: string;
  kind?: string | null;
  message: string;
}

/// `diagnostics.get` response.
export interface Diagnostics {
  hub_version: string;
  protocol: string;
  pid: number;
  config_dir: string;
  pipe: string;
  adapters: AdapterState[];
  recent_errors: DiagnosticsError[];
}
