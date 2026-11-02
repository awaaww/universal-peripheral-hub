# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## Unreleased

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
