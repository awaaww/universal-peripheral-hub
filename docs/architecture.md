# Architecture

Universal Peripheral Hub replaces a pile of vendor utilities with one resident
process and one panel. This document is the contributor-facing summary of the
project plan; the full plan (Chinese) lives outside the repository.

## Why a resident process

Vendor tools each ship their own background service, their own tray icon, their
own updater and their own copy of the same HID/DDC plumbing. Doing the work once
gives us:

- **one owner of device handles** — DDC/CI, HID and audio devices are opened
  once, serialised per device, and closed when the adapter unloads;
- **state that survives the window** — closing the panel must not renegotiate a
  display signal or drop a keyboard's lighting;
- **a stable place for scene logic** — "apply this preset when this program is
  in front" needs a process that is always running.

## Processes

```
┌──────────────────────────────┐        named pipe         ┌───────────────────────────┐
│ hubd.exe (resident)          │  JSON-RPC 2.0, one line   │ UniversalPeripheralHub.exe│
│  · adapters + device handles │ <───────────────────────> │  Tauri 2 + WebView2       │
│  · named-pipe server         │        per object         │  · window, cards, drawer  │
│  · tray icon, single instance│                           │  · no device access       │
│  · settings, scene engine    │                           │  · can be closed anytime  │
└──────────────────────────────┘                           └───────────────────────────┘
```

`hubd` owns the tray icon and the single-instance mutex. The Tauri shell is only
a window: it never opens a device, and `invoke` cannot cross a process boundary,
so every action travels over the pipe.

Third-party or experimental adapters run as child processes speaking the same
JSON-RPC dialect over stdio (see [`adapter-protocol.md`](adapter-protocol.md)).
First-party adapters (`ddc`, `hid`, `audio`) are compiled into `hubd`, because
their call paths are latency-sensitive and share OS handles.

The split is a trust and blast-radius decision, not a performance one: a crashing
out-of-process adapter must not take the tray icon with it.

## Data model

```
Adapter 1 ──< Device 1 ──< Capability
             Device 2 ──< Capability
```

- **Adapter** — a provider of devices, identified by a stable `id` (`ddc`,
  `hid`, `mock`).
- **Device** — a physical thing, with a globally unique `id` (`mock:display-1`),
  a human name, and optional vendor/model/serial from the device itself.
- **Capability** — one controllable or observable property. Every capability has
  two names, and the distinction matters:
  - `cap_type` is **stable across devices** (`display.brightness`). Scenes,
    profiles and hotkeys refer to capabilities by `cap_type`, never by index.
  - `id` is **adapter-local** (`brightness`, `hdr`, VCP code names). It is what
    the adapter needs on the wire.

Values are typed (`int`, `bool`, `enum`, `rgb`, `bytes`) and carry:

- `access` — `read_write`, `read_only` (e.g. battery level) or `write_only`;
- `cost.latency_ms` — what a round trip realistically costs, used by the UI and
  by the scene engine to avoid queueing work that cannot finish in time;
- `cost.side_effects` — `signal_renegotiation` (HDR, input source, colour
  temperature), `audio_route_change`, `exclusive_access`, `device_reset`;
- `traits` — `live` (may change without us), `sceneable` (safe to apply as part
  of a preset), `bindable` (sensible to bind to a key or gesture).

A write is not considered successful until it has been read back. `hubd` returns
the value the device reported, whether it matched, and — when the write itself
succeeded but the read-back failed — the read-back error. Values that differ are
surfaced to the user, not silently retried into place.

The rules that follow from this, and that scene code must respect:

1. **A scene applied at startup changes nothing.** It records the current state
   as the baseline; the first write happens when a rule actually fires.
2. **Read before write.** Writing a value the device already has is skipped.
3. **Renegotiation is opt-in.** Capabilities with `signal_renegotiation` are
   never applied implicitly; the user enables them per scene.

## Scenes

A scene is a named set of capability values; a rule maps a foreground executable
to a scene. Both live in one JSON document (`%APPDATA%\UniversalPeripheralHub\scenes.json`):

```json
{
  "version": 1,
  "scenes": { "focus": { "name": "Focus", "items": [
    { "cap_type": "display.brightness", "value": { "type": "int", "value": 25 } } ] } },
  "rules": [ { "exe": "notepad.exe", "scene": "focus", "priority": 10 },
             { "default": true, "scene": "daily" } ],
  "options": { "allow_side_effects": false, "startup_apply": false }
}
```

Items address capabilities by `cap_type`, so one scene works on any display that
has `display.brightness`; an optional `device` pins an item to one device. A
scene is expanded at apply time against whatever devices are currently present,
and capabilities no device provides are reported as `missing` instead of failing
the whole scene.

The engine is told about foreground changes by a `SetWinEventHook` watcher
(`EVENT_SYSTEM_FOREGROUND`), not by polling the process list; if the hook cannot
be installed it falls back to a 2 s timer, and the log says so.

Applying a scene:

1. **Startup records a baseline and writes nothing.** `hubd` records the
   foreground it started under; that scene is not applied unless `startup_apply`
   is set.
2. **Read before write.** A value the device already reports is skipped and
   counted as `unchanged` rather than `written`.
3. **Renegotiation is opt-in.** Items whose capability has a non-empty
   `cost.side_effects` (HDR, input source, picture mode) are applied first when
   allowed, and reported as `skipped` when `allow_side_effects` is false.
4. **Failures do not abort the scene.** Each failure is collected, retryable
   ones are retried once, and the rest of the scene still runs.

Every apply returns an `ApplyReport` (`written`, `unchanged`, `skipped`,
`failed`, `missing`), which is what the panel shows; "nothing happened" is
visible, not silent.

Re-entering the same executable is a no-op, but re-entering the same *scene*
from a different executable is not: values may have been changed by hand while
the other app was in front, so the read-before-write check runs every time.

## Pipe protocol

Line-delimited JSON-RPC 2.0 over a per-user named pipe:

```
\\.\pipe\universal-peripheral-hub-<lowercased user name>
```

Methods: `hub.hello`, `devices.list`, `capability.read`, `capability.write`,
`adapter.set_enabled`, `device.set_blocked`, `scenes.get`, `scenes.set`,
`scene.apply`. Notifications: `hub.activate` (client → hub, "show the panel"),
and the pushes `devices.changed` and `scenes.changed` (hub → client). `error.code` is always an
integer, as the spec requires; the machine-readable class is in
`error.data.kind` and `error.data.retryable`.

Handshake: the first call on a connection is `hub.hello`; both sides refuse a
`protocol` they do not implement. One client is one connection; the server keeps
reading until the client hangs up or sends a line over 1 MiB.

## Failure model

- Adapter errors are classified (`Unsupported`, `Transient`, `Timeout`,
  `Protocol`, `PermissionDenied`, …) so retry decisions are made once, in the
  adapter, instead of being guessed by the UI.
- Device operations run on blocking workers; a slow DDC read must not stall the
  pipe for other clients.
- A missing UI is not an error: `hubd` starts without it, and the tray can
  enable/disable adapters.
- Settings and scenes are written atomically (`.tmp` + rename). An unparsable
  `settings.json` or `scenes.json` is preserved as `.corrupt-<timestamp>` and the
  defaults are used, because losing a user's configuration silently is worse
  than leaving one stray file behind. A scene document that parses but does not
  validate (for example a rule pointing at a scene that does not exist) is
  quarantined the same way, and the previous in-memory document stays active.

## Repository map

| Path | Contents |
| --- | --- |
| `crates/hub-core` | descriptors, capability values, hub registry, settings |
| `crates/hub-proto` | wire types and constants shared by hubd and the UI |
| `crates/hubd` | resident process: pipe server, tray, selftest |
| `adapters/mock` | two fake devices used by tests and the UI |
| `ui` | Vite + TypeScript panel |
| `ui/src-tauri` | Tauri shell; a window onto the pipe and nothing else |
| `data/monitors` | per-EDID monitor overrides (populated in M1) |
| `docs` | this document, decisions, adapter authoring |

## What M0 does not contain

Real DDC/CI (M1), scenes (M2), out-of-process adapters (M3), the full panel
design (M4), input capture/replay (M5), installers and auto-start (M6). The
modules above either work or are absent; none of them is a placeholder that
pretends to work.
