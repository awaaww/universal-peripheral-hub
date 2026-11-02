# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## Unreleased

### Added

- `device.set_blocked` RPC and a panel control to stop talking to a monitor
  (and to restore it), for KVM/splitter links that hang the DDC bus. The list
  is persisted per adapter and applied before the first request.
- Monitors are ordered left-to-right, top-to-bottom by their desktop geometry,
  so card order no longer follows the undocumented `EnumDisplayMonitors` order.

### Fixed

- Dragging a slider while a write was in flight silently dropped the new value;
  writes are now queued per device and coalesced to the newest value.

## 0.2.0 — M1 DDC/CI

### Added

- `adapters/ddc`: real DDC/CI monitor adapter.
  - Stable device ids from EDID (`ddc:skg2725`), with a hashed device-path
    fallback marked weak when the EDID blob is unreadable.
  - Capabilities parsed from the MCCS string: brightness, contrast, input source
    (values taken from the monitor), and HDR only where VCP `0xE9` is advertised.
  - Reads retry twice; after that the VCP code is cached as unsupported, so a
    capability string that lies cannot cost two round trips per refresh.
  - Writes settle for `settle_ms` (120 ms default, per-model override) before
    the hub reads back, because the panel latches the value late.
  - Per-model overrides in `data/monitors/<KEY>.json`; user copies in
    `%APPDATA%\UniversalPeripheralHub\monitors\`.
  - `ddc_blocked` in `settings.json` skips a monitor behind a splitter or KVM
    that would otherwise stall the shared I2C bus.
- Examples: `probe` (read-only enumeration with timings) and `smoke` (write,
  read back, restore on the real monitor).

### Changed

- `hubd` registers the DDC adapter by default. `ServiceOptions { ddc: false }`
  keeps integration tests hermetic.

### Known limitations

- No UI control yet for blocking a misbehaving monitor; edit `settings.json`.
- HID and audio adapters are not implemented (M3).
- Scene engine is not implemented (M2).

## 0.1.0 — M0 skeleton

### Added

- `hubd` resident core: per-user named-pipe JSON-RPC 2.0 server, single-instance
  mutex, tray icon with "open panel / disable mock adapter / quit", `--selftest`
  headless mode.
- `hub-core`: capability descriptor model (int/bool/enum/rgb/bytes, cost,
  traits), `Adapter` trait, in-process adapter host with load/unload, atomic
  settings with corrupt-file quarantine.
- `adapters/mock`: two fake devices (display with brightness/contrast/HDR/input
  source, keyboard with polling rate/RGB/battery), 30 ms simulated latency and
  read-back on every write.
- `ui`: Tauri 2 window, descriptor-driven device cards with inline capability
  controls (slider, switch, select, colour, hex), read-back feedback and typed
  error surfacing.
- Repository scaffolding: MIT license, CI (fmt, clippy, test, frontend build),
  release workflow skeleton, issue templates, dependabot.

### Known limitations

- DDC/CI, HID and audio adapters are not implemented (M1/M3).
- Scene engine is not implemented (M2).
- No installer, auto-start task or code signing (M6).
