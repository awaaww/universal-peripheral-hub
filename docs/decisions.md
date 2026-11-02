# Decisions

Short records of decisions that are hard to reverse or that contradict an
earlier version of the project plan. Newest last.

## D1 — `hubd` owns the tray icon, the single-instance mutex and auto-start

The plan originally put the tray icon in the Tauri shell. That fails the basic
requirement that closing the panel must not stop device handling: the panel can
be closed at any time, including by the user, and the icon has to remain.

`hubd` therefore owns the tray icon, the per-session single-instance mutex and
(logically) the scheduled task. The Tauri shell is a window: closing it exits the
panel process only. A second `hubd` launch is treated as "show the panel": it
sends `hub.activate` to the running instance and exits.

Consequence: the panel is not required to be running for adapter enable/disable,
and the tray menu works while the panel has never been started.

## D2 — A capability has both a stable type and an adapter-local id

The plan used one identifier for a capability, which breaks the moment two
monitors expose the same setting under different VCP codes, or a scene is
recorded on one device and replayed on another.

`cap_type` (`display.brightness`) is the stable, cross-device name used by
scenes, profiles and bindings. `id` (`brightness`, `0x10`) is what the adapter
needs. The hub enforces uniqueness of `cap_type` within one device and of
`id` within one device.

## D3 — DDC/CI error semantics are ported, not guessed

`crates/ddc` (M1) follows the semantics proven by `DeviceTools.DisplayDevices`
and by the standalone `ddcci.js` implementation, without linking either:
per-error-code retry policy, negative caching with two-strike confirmation, a
100–400 ms settle delay before reading back a freshly written value, and a
reopen path for invalid handles. See `data/monitors/README.md` for the
per-EDID override format.

## D4 — `0x2589`-class errors degrade a capability, they do not blacklist it

The plan (§2.5) treats DDC/CI error `0x2589` and its siblings as "stop using
this VCP code". The shipped JavaScript implementation retries them as bus
collisions instead. Collisions are transient: a monitor that returns `0x2589`
today answers normally after the next mode change.

Decision: retry twice with backoff; if the error persists, mark the capability
as degraded for the session and surface it in the UI. Never write it off
permanently, and never retry forever.

## D5 — JSON-RPC error codes stay integers

JSON-RPC 2.0 requires `error.code` to be an integer. Domain detail travels in
`error.data: { kind, retryable }`. Clients branch on `kind`, not on the number.

## D6 — Repository layout follows plan §14.1, not §8.1

The two sections disagreed. §14.1 (a `crates/` + `adapters/` + `ui/` workspace)
won, because it maps one directory to one reviewable concern and lets adapter
crates be added without touching the root manifest.

## D7 — One configuration directory, one memory budget

Everything user-visible lives under `%APPDATA%\UniversalPeripheralHub\`
(`settings.json`, later `scenes/`, `monitors/` overrides written by the user).
The resident process targets 120 MB of working set; the panel is separate and
exits on close.

## D8 — The panel speaks to `hubd`, never to devices

Tauri's `invoke` cannot cross a process boundary, and a UI process holding DDC or
HID handles would reintroduce the "vendor app per device" problem. All device
access goes over the named pipe. The panel is allowed to be absent.

## D9 — Corrections to the original KTC/ATK reverse-engineering notes

The internal notes said adapters must always live out of process. That is now
`superseded`: first-party, latency-sensitive adapters are in-process (`ddc`,
`hid`, `audio`); third-party and experimental ones are subprocesses. The notes
also used per-EDID JSON with `//` comments and Exo resource GUIDs; our monitor
overrides are strict JSON with inline labels, so they can be validated and
localised without Exo.

## D10 — M0 ships a mock adapter, not a stubbed DDC adapter

A fake adapter that takes 30 ms per call, enforces read-only capabilities and
requires read-back is enough to prove the descriptor → UI → write → read-back
path. A DDC adapter that returns placeholder values would hide exactly the bugs
M1 needs to find (timing, read-back, error classes).

## D11 — The Tauri shell is built with `custom-protocol`

`tauri` proxies the window to `devUrl` unless its `custom-protocol` feature is
enabled, so `cargo build --release` alone produces a binary that shows
`ERR_CONNECTION_REFUSED localhost` on machines without the Vite dev server.
CI and release builds pass `--features custom-protocol`, which embeds
`ui/dist`. `npm run tauri dev` remains the development path.

## D12 — The panel uses an asynchronous pipe client

`hubd` creates its pipe instances in overlapped (asynchronous) mode. A
synchronous `WriteFile` from a `std::fs::File` against such an instance never
completes, which shows up as `hub_request` hanging forever while the server only
sees the first message. The panel therefore runs one tokio runtime thread that
owns the connection and forwards requests over a channel.

## D13 — DDC/CI adapter follows Twinkle Tray's operation model

Twinkle Tray (MIT) is the reference for *how* to talk to monitors, not for what
to show. Adopted: stable identity (never enumeration index), handle reuse across
enumerations, fast/accurate validation split, capabilities cache, a user-managed
blocked list for DDC-hostile links, transient-vs-permanent error classes, and a
single serialized queue for all DDC traffic because the bus is shared. Not
adopted: their device-path identity model, WMI brightness, a second worker
process. Reasoning and file map: `docs/reference-twinkle-tray.md`.
