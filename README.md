# Universal Peripheral Hub

One Windows application for monitors, keyboards, mice, headsets and controllers,
instead of one vendor utility per device.

The hub is a resident core process (`hubd`) plus a web-technology UI. Devices are
described by adapters as a set of typed **capabilities** (brightness, polling
rate, RGB, HDR, ...); the UI and the scene engine are generated from those
descriptors rather than hard-coded per vendor.

> **Status: M2 (scenes).** The application shell, the adapter host, the RPC
> transport, the descriptor-driven UI, a real DDC/CI adapter and the scene engine
> are implemented. Brightness, contrast and input source are read and written on
> real hardware (verified on a KTC H27T22S); HDR is exposed only when the monitor
> advertises VCP `0xE9`. Scenes switch on the foreground executable, read before
> writing, and skip renegotiating capabilities unless you opt in. HID/audio
> adapters, auto-start and installers are **not implemented yet** — see the
> roadmap.

## Status / roadmap

| Milestone | Content | State |
| --- | --- | --- |
| M0 | Tauri shell, `hubd`, adapter host, mock adapter, RPC, CI | **done** |
| M1 | DDC/CI adapter (brightness, contrast, input, HDR) | **done** (HDR where advertised) |
| M2 | Scene engine (foreground trigger, read-before-write) | **done** (side-effect control untested on hardware) |
| M3 | First HID adapter (keyboard/mouse) | not implemented |
| M4 | Full panel UI | partial (descriptor-driven controls) |
| M5 | Session recording / replay | not implemented |
| M6 | Packaging, auto-start, signing | not implemented |
| M7 | Public release | not implemented |

## Architecture in one paragraph

`hubd` owns adapters, devices, settings, the tray icon and the single-instance
mutex. The UI is a Tauri window that talks to `hubd` over a per-user named pipe
using newline-delimited JSON-RPC 2.0; it holds no device state of its own. An
adapter enumerate call returns `DeviceDescriptor`s that carry
`CapabilityDescriptor`s (stable `cap_type` + adapter-local `id`, a value kind,
a cost model and traits such as `signal_renegotiation`). First-party adapters
run in-process; third-party or experimental adapters will run in a child process
over stdio (not implemented in M0). See [docs/architecture.md](docs/architecture.md).

## Repository layout

```
crates/hub-core     descriptor model, adapter trait, in-process host, settings
crates/hub-proto    JSON-RPC wire types shared by hubd and the UI shell
crates/hubd         resident core: pipe server, tray, single instance, --selftest
adapters/mock       in-process mock adapter with two fake devices
ui/                 Vite + TypeScript frontend
ui/src-tauri        Tauri 2 shell (window only; no tray, no device logic)
docs/               architecture, decisions, adapter protocol
data/monitors/      per-EDID monitor overrides (SKG2725.json = KTC H27T22S)
data/scenes.example.json  documented example scene document
```

## Build and run

Prerequisites: Rust stable (MSVC toolchain), Node.js 20+, WebView2 runtime
(preinstalled on Windows 10 1809+ / Windows 11).

Development (the shell loads the Vite dev server, so start it through Tauri):

```powershell
# terminal 1 — core process: tray icon appears, right-click for the menu
cargo run -p hubd

# terminal 2 — panel with hot reload
cd ui
npm install
npm run tauri dev
```

Standalone binaries:

```powershell
cd ui; npm run build; cd ..
cargo build --release -p hubd
# The shell needs `custom-protocol` to embed ui/dist; without it, `tauri`
# proxies to the Vite dev server at http://localhost:1420 instead.
cargo build --release -p universal-peripheral-hub --features custom-protocol
target\release\hubd.exe                        # resident core, tray icon
target\release\universal-peripheral-hub.exe    # panel, starts hubd if needed
```

`npm run tauri dev` builds the shell in dev mode (dev server, hot reload);
`npm run tauri build` enables `custom-protocol` and embeds `ui/dist`.

Headless smoke test, no window, no tray:

```powershell
cargo run -p hubd -- --selftest
```

It loads the mock adapter, prints the two device descriptors as JSON, unloads and
reloads the adapter, and exits with code 0.

Configuration lives in `%APPDATA%\UniversalPeripheralHub\settings.json`. Writes
are atomic; an unparsable file is preserved as `settings.json.corrupt-<timestamp>`
and replaced with defaults.

## Design principles

1. **Descriptors, not hard-coded controls.** The UI renders whatever the adapter
   describes; adding a capability must not require UI changes.
2. **Read before write.** Every write is followed by a read-back; a scene only
   writes values that differ from the current state.
3. **Renegotiation is opt-in.** Capabilities that can blank the screen or change
   the signal (HDR, image mode, display application, colour temperature) are
   marked `signal_renegotiation` and never applied implicitly.
4. **No shell code.** Every module in this repository either works or is marked
   as not implemented. There are no placeholder commands that silently do nothing.
5. **Offline by default.** The hub makes no network requests.

## 中文摘要

通用外设中心：用一个常驻核心进程 `hubd` 加一个 Web 技术界面，取代显示器、键盘、鼠标、
耳机、手柄各自的厂商驱动软件。适配器把设备描述成一组带类型的「能力」（亮度、回报率、
RGB、HDR 等），界面和场景引擎都从描述符生成，而不是为每个型号写死控件。

当前是 **M1**：外壳、适配器宿主、RPC、描述符驱动的界面、模拟适配器，以及真实 DDC/CI
显示器适配器（亮度/对比度/输入源，HDR 视机型是否上报 VCP `0xE9`）可用。HID、音频适配器、
场景引擎、开机自启和安装包均**未实现**，状态表见上。

进程边界：`hubd` 拥有托盘、单实例、设置与设备；UI 只是一个窗口，通过按用户命名的
命名管道用「按行 JSON-RPC 2.0」与 `hubd` 通信，自身不保存设备状态。

## License

MIT. See [LICENSE](LICENSE) and [NOTICE.md](NOTICE.md). Prior art and protocol
references are acknowledged there; no source code from those projects is included.
