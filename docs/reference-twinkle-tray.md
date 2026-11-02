# Reference notes: Twinkle Tray

Read before starting the DDC/CI adapter (M1). Twinkle Tray
(<https://github.com/xanderfrangos/twinkle-tray>, MIT, v1.18.0-beta2) is the most
complete open-source Windows implementation of monitor brightness over DDC/CI.
We port **semantics, not code**: no source is copied, and this file records what
we deliberately took and what we deliberately did not.

## Where the interesting parts are

| Path | What it contains |
| --- | --- |
| `src/modules/node-ddcci/ddcci.cc` (1665 lines) | the native addon: enumeration, handle cache, capability probe, VCP get/set, error classes |
| `src/modules/node-ddcci/index.js` | JS facade, capabilities-string parser, per-monitor `getInputs` |
| `src/Monitors.js` (2298 lines) | identity, hotplug, WMI fallback, blocked list, worker queue |
| `src/monitor-rules.json` | per-model quirks (non-standard brightness VCP code, skip-reapply list) |

## What we adopt

1. **Stable identity from `QueryDisplayConfig`.** Physical-monitor handles are
   matched to display targets so settings are keyed by `deviceKey`/device path
   instead of enumeration index. Their fallback order is: unique friendly-name
   match → unique GDI device name → enumeration order (documented as
   order-dependent). We use EDID instead, but the lesson holds: never key
   settings by index.

2. **Handle reuse across enumerations.** Before acquiring new physical-monitor
   handles, probe the previously working handle with a *fast* check; if it still
   answers, destroy the freshly acquired handle and keep the old one. This is
   exactly the class of bug found in `04_参考实现_ddcci.js` (handles leak and get
   double-destroyed on every `reopen()`).

3. **Two validation strengths.** `fast` = probe VCP `0x02` (new control value),
   `0xDF` (VCP version), `0x10` (brightness) and accept the first that answers;
   `accurate` = read the capabilities string (`0x00`); `no-validation` = trust
   the handle. Enumerating at startup must not cost one capabilities read per
   monitor, so "fast first, enrich later" is the default.

4. **Capabilities cache keyed by device key.** The capabilities string is read
   once and reused across refreshes; a monitor that already answered is not
   re-probed on every enumerate.

5. **A blocked list for DDC-hostile links.** Displays behind a one-way optical
   HDMI link, a KVM, or a DDC-stripping adapter can stall *every* request for
   seconds inside the graphics driver and poison the shared I2C/AUX bus. Twinkle
   Tray lets the user exclude a display by device key and skips all DDC and
   high-level probing for it. We need the same escape hatch, or one bad monitor
   makes the whole panel feel broken.

6. **Transient-vs-permanent error classes.** `isTransientDdcError` treats
   `ERROR_GRAPHICS_I2C_ERROR_TRANSMITTING_DATA`, `..._RECEIVING_DATA`,
   `ERROR_GRAPHICS_DDCCI_MONITOR_RETURNED_INVALID_TIMING_STATUS_BYTE`,
   `INVALID_MESSAGE_COMMAND`, `INVALID_MESSAGE_LENGTH`,
   `INVALID_MESSAGE_CHECKSUM` as retryable (3 attempts, 50 ms apart) and
   everything else as final. Errors carry the raw Win32 code so callers never
   parse localized strings.

7. **One serialized queue for all DDC traffic.** Their monitor code runs in a
   worker process and funnels every operation through a promise chain
   (`monitorMessageQueue`). The bus is shared, so serialization is not
   per-device, it is global to the adapter.

8. **High-level brightness as a first-class transport.** `GetMonitorBrightness`
   / `SetMonitorBrightness` (dxva2) works on some panels without DDC, and Twinkle
   Tray prefers it when available unless the user pinned a custom VCP code. We
   should expose it as an alternative implementation, not as a hidden fallback.

## What we do not adopt

- **Their identity model.** They key settings by device path/instance id and
  keep a separate `monitor-rules.json`; we key by EDID (`<mfg><product>.json`)
  and put quirks in the same file. Simpler to review, and our notes already
  carry the `TLH0025`-style keys.
- **WMI brightness for internal panels.** That is a different capability
  (`display.internal_brightness`, M3+ at the earliest), not a DDC concern.
- **A second worker process for the adapter.** Our in-process adapter host plus
  a serialized queue is enough for M1; the subprocess split stays for
  third-party adapters (M3).
- **Their Electron UI and store packaging.** Out of scope by design.

## Consequences for M1

- The adapter owns one async mutex serializing every VCP operation, and a small
  per-handle cache (capabilities string, last known value, last error class).
- Enumeration has two modes; `--selftest` and startup use fast, an explicit
  "refresh capabilities" path uses accurate.
- Settings gain `ddc_blocked: [<edid-key or device id>]` so a hostile link can be
  excluded without editing JSON by hand.
- Every VCP call gets a timeout (their IPC layer uses 8 s; we use 2 s per
  attempt, 6 s total) so a stalled driver call cannot freeze `hubd`'s pipe
  thread.
