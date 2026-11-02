# Adding a device

An adapter turns one family of hardware into `Device` + `Capability` values. The
hub does not know what a monitor, a keyboard or a headset is; it knows devices
that expose typed capabilities.

## 1. Where the adapter lives

| Situation | Location | Process |
| --- | --- | --- |
| First-party, latency-sensitive, shares OS handles (`ddc`, `hid`, `audio`) | `adapters/<name>` crate, linked into `hubd` | in-process |
| Third-party, experimental, or crash-prone | its own executable | subprocess ([protocol](adapter-protocol.md)) |

Start in-process. Move out of process when the adapter needs to spawn its own
helpers, take a dependency you do not want in the hub binary, or has proven it
can hang.

## 2. Implement the trait

```rust
pub trait Adapter: Send {
    fn info(&self) -> AdapterInfo;
    fn enumerate(&self) -> Result<Vec<DeviceDescriptor>, HubError>;
    fn read(&self, device: &str, cap: &str) -> Result<CapabilityValue, HubError>;
    fn write(&mut self, device: &str, cap: &str, value: &CapabilityValue)
        -> Result<(), HubError>;
}
```

- `info().id` is permanent: it appears in device ids and in `settings.json`.
- Device ids must be globally unique. Use `<adapter>:<stable-local-name>`
  (`mock:display-1`, `ddc:GSM5B7C:0`), never an array index — enumeration order
  changes when a cable is replugged.
- `enumerate` is called on load and on demand. It must be safe to call
  repeatedly and must not have side effects on the hardware.
- `read` and `write` take the adapter-local capability `id`, not `cap_type`.
- Return `HubError` with a classified `ErrorKind`. Do not return a stringly
  error: retry policy and UI treatment depend on the kind.

Writes are verified by the hub: whatever `write` leaves on the device is read
back and compared, so `write` must not fake success. If the device needs time to
settle, sleep inside the adapter, not in the caller.

## 3. Describe capabilities

```rust
CapabilityDescriptor {
    id: "brightness".into(),
    cap_type: "display.brightness".into(),
    label: "Brightness".into(),
    kind: CapabilityKind::Int { min: 0, max: 100, step: 1, unit: Some("%".into()) },
    access: Access::ReadWrite,
    cost: Cost { latency_ms: 120, side_effects: vec![SideEffect::SignalRenegotiation] },
    traits: Traits::default().sceneable(),
}
```

- `cap_type` names the *concept*, and is what scenes and bindings store. Use the
  existing vocabulary (`display.brightness`, `display.contrast`,
  `display.input`, `keyboard.polling_rate`, `keyboard.lighting`,
  `keyboard.battery`) before inventing one.
- Only mark `signal_renegotiation` if writing it can blank the display or drop
  the link. The scene engine will refuse to apply it implicitly; over-marking
  makes presets feel broken, under-marking makes them dangerous.
- Add `latency_ms` honestly: the UI shows it, and the scene engine uses it to
  decide whether a rule can finish before the user notices.

## 4. Register it

`crates/hubd/src/service.rs` builds the adapter list:

```rust
hub.register(adapter_mock::adapter_info(), adapter_mock::factory())?;
```

Register before `Service::bootstrap` loads adapters, so `--selftest` and the
panel see the same set.

## 5. Test without hardware

- Unit-test `read`/`write` against the mock harness (`adapters/mock` shows the
  shape: a value map behind a mutex, a 30 ms delay, an `enumerate` that returns
  descriptors).
- Add a case to `crates/hubd/tests/pipe_roundtrip.rs` when the adapter changes a
  wire-visible behaviour (method, error kind, notification).
- `cargo run -p hubd -- --selftest` must pass with the adapter registered: it
  loads, enumerates, reads and writes, and unloads cleanly. If an adapter panics
  on unload, the selftest is the first place it shows up.

## 6. Document what could break

Add a row to `docs/architecture.md` if the adapter introduces a new side effect,
a new process, or a new failure mode the UI has to explain. "The monitor goes
black for two seconds" is exactly the kind of thing a future contributor needs
to find before shipping it.

## Not implemented yet

Out-of-process adapters (M3), device hot-plug notifications, and per-device
settings overrides. Until then, an adapter is in-process, enumerates on load,
and its settings live in `settings.json` only as an enable/disable flag.

## Device ids are `adapter:token`

`device.set_blocked` addresses a blocked device by `(adapter, token)` because a
blocked device is not enumerated. That only works if the part of the device id
after the colon *is* the token your adapter persists for the block list — for
the DDC adapter, the lower-cased EDID key (`ddc:skg2725`). Keep that shape if
your adapter can hang the machine it runs on.

## Worked example: the DDC adapter

`adapters/ddc` is the first real adapter and the one to copy from:

| Concern | Where |
| --- | --- |
| Win32 calls, raw error codes | `adapters/ddc/src/win.rs` |
| Hand-written `dxva2.dll` FFI | `adapters/ddc/src/dxva2.rs` |
| Stable identity from EDID | `adapters/ddc/src/edid.rs` |
| Parsing the MCCS capabilities string | `adapters/ddc/src/caps.rs` |
| Hardware seam for tests | `adapters/ddc/src/transport.rs` |
| Policy and descriptors | `adapters/ddc/src/adapter.rs` |

Two lessons worth keeping:

* Policy must be testable without hardware. `Core<T: Transport>` is generic over
  the transport, so retries, the negative cache, clamping and descriptor
  building run against `FakeTransport` in unit tests.
* Anything expensive (the capabilities string costs ~1.1 s) is read once per
  device and cached for the adapter's lifetime; enumeration must stay fast.
