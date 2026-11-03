# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## 0.6.0 — adapter trace record and replay

### Added

- `hub-trace`: a small, versioned format for HID exchanges (devices plus an
  ordered list of open/send/recv steps) with strict-order replay.
- `atk-adapter --probe --record <trace>` and `via-adapter --probe --record <trace>`
  capture everything the adapter sent and received.
- `--replay <trace>` boots the adapter against the recording instead of hardware;
  `via-adapter --replay <trace> --json` dumps the descriptor list for goldens.
- Checked-in recordings: `adapters/via/tests/traces/via-rs7-air.json` (real
  keyboard) and `adapters/via/tests/traces/via-rs7-air.descriptors.json`
  (descriptor golden), plus `adapters/atk/tests/traces/atk-mouse-8k.json`
  (a real capture of the mouse's wireless link asleep — replay must report no
  device instead of inventing one).
- Replay tests assert the decoded values, that the whole recording is consumed,
  and that the descriptor list still matches the golden.

### Notes

- Recording never runs in CI; the traces are ordinary files.
- The ATK recording is a *silent bus* capture. A success capture needs the
  wireless mouse to be physically moved during `--probe --record`.

## 0.5.1 — window fit and a banner that goes away

### Fixed

- The panel now clamps itself to the monitor and re-centres at startup, so a
  1024×760 window on a 1366×768 laptop (or any 125–175 % display) no longer
  leaves the right-hand controls off-screen.
- A slow core start could leave "cannot open \.\pipe\…" on screen after the
  pipe was already up. Any successful request now clears that banner, because a
  message about a failure must not outlive the failure.

## 0.5.0 — device pages, undo and diagnostics

### Added

- Device detail pages: clicking a card (or pressing Enter on it) opens the
  device, capabilities are grouped by their `cap_type` prefix, and Escape or the
  back button returns to the grid with focus restored to the card. Keymap
  editing keeps working inside the detail page.
- Per-device undo: `hubd` records the value each panel write replaced (20 per
  device, in memory) and `device.history` / `device.undo` expose it. The detail
  header shows what the undo would restore ("Undo Brightness 40%") and how many
  steps remain. Scene writes are not recorded (D34).
- Global `Ctrl+Alt+M` to open the panel. When another program already owns the
  combination, the core falls back to `Ctrl+Alt+Shift+M` and logs which one is
  live (D35).
- Diagnostics block in the settings page: hub version, protocol, pid, pipe,
  config directory, every adapter with its state, and the last 20 failed RPCs
  with time, method and typed kind. Refresh and Copy are provided (D36).
- Writes whose descriptor declares side effects (signal renegotiation, audio
  route change) now ask for confirmation, with an opt-out remembered per
  `cap_type` in `localStorage`.
- Failed writes explain the failure in a sentence plus its typed kind, and offer
  Retry with the same value.

### Changed

- Cards are keyboard reachable (`tabindex`, Enter/Space), the detail page moves
  focus to Back on open and back to the card on close.
- Read-only capabilities no longer render a disabled slider; booleans still show
  a disabled switch (documented limitation).

### Fixed

- Undoing a write left the previous "Synced · read back …" text on the row,
  describing the value that had just been undone; the undo now clears it.

## 0.4.0 — panel preferences

### Added

- Settings page in the panel: language (English, 简体中文, 繁體中文), close-button
  behaviour (hide to the tray or quit), optional auto scan with a 15–300 s
  interval, and low-battery alerts with a configurable threshold. The tray menu
  and battery alerts follow the language preference.
- `prefs.get` / `prefs.set` / `prefs.changed` over the pipe, backed by a `Prefs`
  struct in `settings.json`; `hubd` normalizes out-of-range values before it
  answers (D33).
- `hub.shutdown` so the panel's "quit" close behaviour stops the core as well.
- Auto scan runs inside `hubd` (5 s ticker) and reuses `adapters.reload`, so a
  scheduled scan and the Rescan button cannot diverge.
- Low-battery tray notifications with hysteresis: alert once below the threshold
  and re-arm five points above it.

### Changed

- A device's battery percentage is now a small ring with the number next to the
  device name; the battery row is gone. Other read-only integers render as plain
  text instead of a disabled slider.
- The tray icon is the application icon embedded from
  `ui/src-tauri/icons/icon.ico` instead of the stock Windows icon.
- Panel window no longer remembers being "always on top": it never was in the
  configuration; the screenshots that looked that way raised the window
  themselves.

### Fixed

- Closing the panel window did nothing visible: `core:window:allow-hide` and
  `core:window:allow-destroy` were missing from the capability file, so the
  close handler's `preventDefault()` left the window open.
- Battery alerts ignored capabilities such as `battery.level` whose `cap_type`
  does not end in `.battery`.

## 0.3.0 — M6 packaging and auto-start

### Added

- Per-user NSIS installer (`ui/src-tauri/tauri.conf.json` + `installer-hooks.nsh`):
  bundles the core and both adapter helpers, installs under
  `%LOCALAPPDATA%\Universal Peripheral Hub\`, registers auto-start for the current
  user and starts the tray process. Uninstalling removes the program, the
  processes and the auto-start value but keeps `%APPDATA%\UniversalPeripheralHub\`.
- `hubd --autostart on|off|status` and a "Start with Windows" tray item, backed
  by the per-user `Run` value (see D32).
- `hubd` hides its own console window when it starts with no arguments, so the
  installer and the Run key do not leave a black console on screen; the CLI
  modes keep it.

- Out-of-process adapters: `ProcAdapter` in `hubd` speaks the stdio JSON-RPC
  dialect from `docs/adapter-protocol.md` to a child process, with a 3 s
  handshake, per-call timeouts, stderr passthrough, and a latch after three
  consecutive timeouts (the panel/tray can reset it).
- `hub-core`: `Text` capability kind (`{ "type": "text", "value": … }`), used for
  firmware versions.
- `adapters/atk`: first real HID adapter (ATK wireless mouse, `373B:11FE`),
  battery %, charging, wireless link, firmware version, report rate and the
  active DPI slot. Report rate and the DPI slot are writable through the same
  read-modify-write EEPROM block the vendor app uses; numeric DPI is not
  advertised because the firmware does not expose the per-slot values. Ships
  `--probe` for a headless one-device dump.
- `hubd --selftest --adapter atk` enumerates the adapter and prints every
  capability value, exiting non-zero when the helper is missing or no device
  answers.
- `adapters/via`: second out-of-process adapter, speaking VIA/QMK raw HID to the
  ATK RS7 Air keyboard (`373B:1258`, usage page `0xFF60`). Read-only in this
  milestone: protocol version, keymap layers, key count (derived from the 768-byte
  dynamic keymap buffer: 4 layers x 96 keys) and macro count. Keymap and lighting
  writes are not implemented; `write()` returns `unsupported` and no frame that
  changes a setting is ever sent.
- `hubd --selftest --adapter via`, and `UPH_VIA_ADAPTER` for an explicit helper
  path (the ATK helper keeps `UPH_ATK_ADAPTER`).
- Keymap editor in the panel: layer tabs, a button per key, a keycode picker,
  and a whole-image write through `keyboard.keymap`. Only edited keycodes travel;
  the adapter reads the image back, restores it on a mismatch, and keeps the
  pre-edit image in `%APPDATA%\UniversalPeripheralHub\keymap-backups\`.
- `via-adapter --write-selftest`: writes one keycode on the last keymap layer,
  verifies the read-back, restores the original and exits non-zero on a
  mismatch. The panel does not expose key remapping yet (see D30).
- `docs/re/atk-hub.md`: the protocol research record (frame layout, EEPROM map,
  what the vendor binaries actually do, and what is still unknown).
- `device.set_blocked` RPC and a panel control to stop talking to a monitor
  (and to restore it), for KVM/splitter links that hang the DDC bus. The list
  is persisted per adapter and applied before the first request.
- Monitors are ordered left-to-right, top-to-bottom by their desktop geometry,
  so card order no longer follows the undocumented `EnumDisplayMonitors` order.
- Scene engine (M2): `scenes.json` holds named scenes, foreground rules and
  options; `scenes.get`, `scenes.set` and `scene.apply` expose it, and
  `scenes.changed` pushes updates to the panel.
- Foreground tracking uses `SetWinEventHook(EVENT_SYSTEM_FOREGROUND)` with a 2 s
  timer fallback, so scene switching does not poll the process list.
- Apply reports (`written` / `unchanged` / `skipped` / `failed` / `missing`) are
  returned to callers and shown in the panel.
- `data/scenes.example.json`, and a scenes section in `docs/architecture.md`.

### Fixed

- Dragging a slider while a write was in flight silently dropped the new value;
  writes are now queued per device and coalesced to the newest value.
- The startup baseline is taken from the foreground at launch instead of being
  consumed by the first app switch, so the rule for the app already in front
  runs when the user comes back to it.

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
