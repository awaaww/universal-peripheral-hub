# ATK peripherals — is there a public protocol?

Status: **research record**. Stages 1–3a are implemented in
`adapters/atk` (mouse) and `adapters/via` (keyboard); the headset is not.

The question this note answers: before reverse engineering anything, does ATK
publish a HID protocol specification, and is there a reusable open-source
implementation we could build on (the way HID++ has libratbag/Solaar)?

## Verdict

**C for our devices and for the capabilities we need (DPI, report rate, RGB),
with a narrow B for the transport frame and the battery query.**

In the A/B/C vocabulary from the brief:

- **A — no.** ATK publishes no protocol specification, SDK, or developer
  documentation. The web HUB is a client, not a spec.
- **B — only partially.** Two community projects exist. One is MIT but documents
  only the frame format and a battery query, and lists two product IDs, neither
  of which is ours. The other knows far more but is GPL-3.0, so its code cannot
  be reused in an MIT project; only the facts it states can be checked against
  our own hardware.
- **C — required for the rest.** Our three devices are not in either project's
  registry, and DPI / report-rate / lighting are not documented anywhere public.
  Those need stage 2 (read-only static analysis) before anything is written.

So the honest plan is: adopt the *transport* framing as a hypothesis, verify it
on our hardware with read-only queries, and treat every command and EEPROM
address beyond the battery query as unverified until stage 2 confirms it.

**Stage 2 changed the transport verdict but not the overall one.** The frame
(16-byte payload, checksum summing to `0x4D`) is real, but it travels as an
**output report**, not a feature report: all three vendor collections report
`FeatureReportByteLength = 0` and refuse `HidD_SetFeature`. With output reports
the mouse answers battery / wireless-online / version, so those three become
confirmed facts; DPI, report rate and lighting remain **C** — the EEPROM read is
acknowledged but never answered on this firmware. Details in
[Stage 2 result](#stage-2-result-read-only-probes-2026-11-02).

## Devices on this machine

Enumerated read-only with `Get-PnpDevice` / `DEVPKEY_Device_HardwareIds`
(`UP:xxxx_U:yyyy` on each HID child):

| Device | VID:PID | Instance | Relevant HID collections |
| --- | --- | --- | --- |
| Wireless mouse 8k NANO dongle-L | `373B:11FE` | `541505796617` | MI_01 vendor pages `FF02:0002`, `FF03:0000`, `FF04:0002`, `FF05:0000`, `FF06:0002` |
| ATK RS7 Air (Hall-effect keyboard) | `373B:1258` | `0000001A0795F68E` | MI_01 vendor page `FF60:0061` |
| ATK N9 Ultra (headset) dongle | `373B:1204` | `4150353336333806` | MI_03 vendor page `FF02:0002`; MI_05 is a `COM3` serial port |

Other sibling vendor IDs seen on the machine (VXE/VGN family) are `3554` and
`25A7`; they are listed by community tooling but are not what we are targeting.

Two things already worth noting:

- The keyboard is a *different* vendor page (`FF60`) from the mice (`FF02`), so
  "ATK peripherals" is not one protocol surface.
- The headset exposes a vendor HID collection **and** a serial port. Neither is
  obviously the audio control path; the audio device itself is plain USB audio.

## What exists, with links

### Official

- No developer/SDK/开放平台 page on `atk.store`; the site's only HID-adjacent
  article is instructions for the web HUB:
  <https://www.atk.store/blogs/news/atk-v-hub-web-version-instructions>
  (page is unreachable from this machine — `curl` gets `Recv failure: Connection
  was reset`, `web_fetch` fails too — so this is cited from search results).
- No ATK organisation on GitHub with a protocol repository.
- The installed application contains no protocol documentation and no
  third-party notice file: `C:\Program Files\ATK HUB\` has only
  `LICENSE.electron.txt` and `LICENSES.chromium.html`. The vendored modules
  (`atk-mouse-system-api`, `key-linker-lib`, `atk-game-helper`, `loudness`,
  `music-rhythm-node`, `sound-tools`) are all closed source.
- Public vendor *client*: <https://hub.atk.pro/> responds 200 and loads a WebHID
  bundle from `bpcdn.atkgear.com/hub-v3/production/3.2.29/`. It proves the
  protocol is reachable from a browser, which is useful context, but minified
  client code is an implementation, not a specification, and carries no license
  to reuse.

### Community

- **`noosxe/atk-tool` — MIT** — <https://github.com/noosxe/atk-tool>
  Go CLI + library with `docs/protocol.md` and `docs/design.md`.
  - Transport: **Feature Report**, report id `0x08`, payload 16 bytes, write
    packet 17 bytes (id + payload), read buffer 64 bytes.
  - Checksum: the 8-bit wrapping sum of the 16 payload bytes must equal `0x4D`,
    i.e. `payload[15] = 0x4D - sum(payload[0..=14])`.
  - Battery query: opcode `0x04`; response `buf[6]` = percent, `buf[7]` =
    charging (0/1), `buf[8..10]` = millivolts, big-endian.
  - Registry: only `373b:1115` (A9 Plus wired) and `373b:10c9` (Nearlink
    dongle). **Neither is one of our devices**, and battery is the only command
    implemented.
  - License: MIT, so code reuse is possible with attribution; we would rather
    reimplement from the documented frame and cite the project.
- **`cyberphantom52/libatk-rs` — GPL-3.0** — <https://github.com/cyberphantom52/libatk-rs>
  Rust library, "abstraction over the reverse engineered ATK Mouse
  communication protocol". It encodes a much larger command and EEPROM map:
  command ids `0x01` download data, `0x02` download status, `0x03` wireless
  online, `0x04` battery, `0x07` set EEPROM, `0x08` get EEPROM, `0x12` mouse
  version, `0x14` 4K RGB mode, `0x18` dongle light mode; EEPROM addresses
  `0x00` report rate, `0x02` max DPI, `0x04` current DPI, `0x0c`+ DPI pairs,
  `0x4c`+ lighting. Frame: 16 bytes, `command | status | address(2) |
  length | data(10) | checksum`.
  - **Cannot be reused.** GPL-3.0 is incompatible with this project's MIT
    licence, and copying structures from it would taint the codebase. Its facts
    are treated as *hypotheses to verify*, not as a source.
- **`yszyszysz233/2.4G-battery-viewer`** — 237 stars, **no licence** —
  <https://github.com/yszyszysz233/2.4G-battery-viewer>
  Windows ctypes tool that reads dongle battery from 2.4G receivers. Same
  family of behaviour, no licence means no reuse; useful only as evidence that
  the battery query generalises beyond one product.
- **`Speyll` udev gist** —
  <https://gist.github.com/Speyll/b7803161b1ee43f258d484fe9e92c4b4>
  Permission rules for `373b`, `3554`, `25a7`, written to let the web HUB reach
  the devices on Linux. Confirms the vendor IDs and the WebUSB/WebHID client,
  nothing about the protocol.

### Projects we checked that do *not* cover ATK

- **OpenRGB**: `Smart-Solution-LLC/OpenRGB` `Controllers/` contains 153
  controllers and none match `atk`, `vxe`, or `vgn` (checked via the GitHub
  contents API). If ATK lighting is ever wanted, it would have to be a new
  OpenRGB controller, not an existing one we can ride on.
- **Linux kernel**: no `hid-atk` driver is upstream; the only recent vendor
  driver of this shape is Pulsar's, which is unrelated hardware.
- **libratbag / Solaar**: Logitech-only (HID++), irrelevant here.

## Licence position for this project

- We keep the repository MIT.
- `atk-tool` (MIT) may be reused with attribution; at present we only plan to
  adopt its *frame description* and cite it in `NOTICE.md`.
- `libatk-rs` (GPL-3.0) must not contribute code or structure. Any overlap in
  constant values is a fact about the hardware, not a copied expression, but the
  implementation has to be driven by our own observations.
- The vendor's web client is proprietary; reading it is fine for
  interoperability, copying it is not.

## What stage 1 does *not* establish

- Whether `373B:11FE`, `373B:1258`, or `373B:1204` actually answer the `0x08`
  feature-report frame at all, or on which collection.
- Whether report framing differs between mouse and keyboard (`FF02` vs `FF60`).
- The command/EEPROM map for report rate, DPI, and lighting on *our* firmware.
- Anything about the headset: its control channel may be the `COM3` serial port
  rather than HID, and its audio is standard USB audio.

## Proposed stage 2 (read-only, no writes to devices)

1. Static analysis of `resources/app.asar` with REA: every `node-hid` call site,
   feature-report payload construction, and `atk-mouse-system-api` call site.
2. Export tables of `atk-mouse-system-api.win32-x64-msvc.node` and
   `KeyLinkerlib.dll`, mapped to DPI / report-rate / battery functions.
3. Read-only probes against our three devices: which collection answers, what
   report lengths are accepted, and what a battery query returns — using the
   `0x08` frame only if it does not write state (a query opcode is read-only).
4. Update this document with confirmed vs. unverified facts; no Rust code until
   that report is reviewed.

## Stage 2 result (read-only probes, 2026-11-02)

Everything below was measured on the three devices in this machine, with the
vendor's own software running. No write command was sent: only query opcodes
(`0x03` wireless-online, `0x04` battery, `0x12` version) and one `GetEEPROM`
read. Probes live outside the repository (`atk-query.py`, `atk-eeprom.py` in the
scratch dir); the findings are recorded here.

### The transport is *not* feature reports

`HidP_GetCaps` on every `373B` vendor collection reports `FeatureReportByteLength
= 0`. `HidD_SetFeature` and `HidD_GetFeature` fail with
`ERROR_INVALID_FUNCTION (1)` at every candidate length, and
`IOCTL_HID_GET_REPORT_DESCRIPTOR` is refused for the same reason. So the public
MIT notes (`noosxe/atk-tool`) describe the *frame*, not the transport we have:

| Device | Vendor collection | Usage page:usage | Input | Output | Feature |
| --- | --- | --- | --- | --- | --- |
| `373B:11FE` 8K NANO dongle-L (mouse) | `mi_01&col05` | `FF02:0002` | 17 | 17 | **0** |
| `373B:1204` N9 Ultra dongle (headset) | `mi_03` | `FF02:0002` | 65 | 65 | **0** |
| `373B:1258` RS7 Air (keyboard) | `mi_01` | `FF60:0061` | 385 | 385 | **0** |

The frame goes out with `WriteFile` on an overlapped handle, padded to the
collection's output length; the reply comes back as an input report with
`ReadFile`. A 17-byte write to the 65/385-byte collections fails with
`ERROR_INVALID_PARAMETER (87)`, which is the cheapest way to find the right
collection per device.

### Confirmed frames — mouse (`373B:11FE`), report length 17

Request is report id `0x08` + 16-byte payload, checksum chosen so the 8-bit sum
is `0x4D` (this part of the public notes holds):

```
04 00 00 00 00 00 00 00 00 00 00 00 00 00 00 49   battery query
03 00 ... 4A                                       wireless online
12 00 ... 3B                                       version
```

Observed replies (hex, first 16 bytes), all with report id `0x08`:

| Query | Reply | Decoded |
| --- | --- | --- |
| `0x04` battery | `08 04 00 00 00 02 64 00 10 E6 …` | 100 %, discharging, 4326 mV |
| `0x03` online | `08 03 00 00 00 01 01 39 3F 58 …` | 1 (wireless link up) |
| `0x12` version | `08 12 00 00 00 02 03 03 …` | 3.3 |

Layout matches the public notes exactly: byte 1 echoes the opcode, byte 5 is the
data length, bytes 6/7/8-9 are percentage / charging flag / big-endian mV.
**This is enough for read-only battery and version capabilities.**

### Not confirmed

- **DPI and report rate.** `GetEEPROM` (`0x08` with the address in payload
  bytes 2-3) is answered with `08 08 01 <addr> 00 …` — the `0x01` looks like a
  status and there is no follow-up report within 800 ms, at any of the addresses
  from `libatk-rs` (`0x0000` report rate, `0x0002` max DPI, `0x0004` current
  DPI). Either the dongle proxies these through a different opcode, or a prior
  handshake (`down-load driver status`, `0x02`) is required. Writing DPI is a
  stage 3 question and needs this resolved first.
- **Keyboard (`373B:1258`, `FF60:0061`)**: same opcodes are echoed but the
  reply is `00 <opcode> 00 00 00 00 …` — report id 0, length 0, no payload.
  This is a different command set (likely the SOC behind `KeyLinkerlib.dll`),
  and treating it as the mouse protocol would be wrong.
- **Headset (`373B:1204`)**: the 17-byte frame is rejected outright and a
  65-byte frame is accepted but never answered; its control channel is probably
  the `COM3` interface or a different report id.
- **RGB / lighting**: nothing measured; the frame map in `libatk-rs` is GPL
  code we may read as facts but must not copy as structure.

### Consequence for stage 3

A process-isolated `adapters/atk` can start with the **mouse** and
`battery` / `firmware` as read-only capabilities, using the frame above. DPI,
report rate, lighting, keyboard and headset each need their own evidence before
they become code; the adapter must therefore advertise only what it can read on
the device in front of it, and say so in the log when a device answers nothing.

## Sources

- <https://github.com/noosxe/atk-tool>
- <https://github.com/cyberphantom52/libatk-rs>
- <https://github.com/yszyszysz233/2.4G-battery-viewer>
- <https://gist.github.com/Speyll/b7803161b1ee43f258d484fe9e92c4b4>
- <https://www.atk.store/blogs/news/atk-v-hub-web-version-instructions>
- <https://hub.atk.pro/>
- <https://github.com/Smart-Solution-LLC/OpenRGB>

### Native modules: what they are not

Static analysis of the two native modules shipped with the app (read-only, no
execution) ruled both out as the HID transport for these devices:

- `atk-mouse-system-api.win32-x64-msvc.node` exports exactly two N-API classes,
  `MouseSystemSetting` and `AudioSystemSetting`. The main-process bundle calls
  them from `getPointerSpeed` / `setPointerSpeed` / `getRollerSpeed` /
  `setRollerSpeed`, i.e. it wraps Windows pointer/scroll settings. No HID
  imports, no `hid.dll`, no usage-page strings, and zero occurrences of `dpi`.
- `key-linker-lib` (C++/CLI `KeyLinkerlib.dll` + a NAPI wrapper) speaks the
  *KeyLinker* controller protocol: opcode `0x1D` battery/heartbeat, capability
  pages `B0 07 …` / `B1`–`B7` reads, `0x31`–`0x37` writes, CRC-8 polynomial
  `0xEB`, replies in 20-byte chunks marked `DF AA`. That is a gamepad protocol
  and does not match what the mouse answers on report `0x08`.

So the mouse/keyboard protocol lives in the renderer bundle
(`resources/main/app.asar`, 48 MB, obfuscated) and/or the signed web client
(`hub.atk.pro`), not in the native modules. That is why the desktop app only
needs `node-hid` for transport and puts the packet builders in JavaScript.

### What is still unknown, and the two ways to close it

DPI, report rate and lighting remain unconfirmed: the EEPROM read
(`0x08` + address) is acknowledged (`08 08 01 <addr> 00 …`) and never answered
on this firmware, so either a handshake precedes it or the dongle uses a
different opcode family. Two honest options:

1. **Deobfuscate the renderer bundle** and read the packet builders
   (`bykb-advanced-keys.util-*.js`, `event-*.js` carry the command/EEPROM
   tables). Static, no devices touched, but a few hours and not guaranteed.
2. **Capture what the vendor app sends** while changing DPI in its UI
   (USBPcap/Wireshark, or a HID proxy). Definitive, but installing a capture
   driver on the user's machine is a bigger ask.

The read-only stage below is already enough to ship a first out-of-process
adapter (battery, charging state, wireless link, firmware version for the
mouse); DPI and rate should wait for one of the two steps above.

## The mouse protocol, from the shipped renderer (stage 3a)

The real implementation is not in a `.node` module. It is plain JavaScript in the
renderer bundle `main/app.asar` → `static_filter-3fyXcGGg.js` (145 799 bytes),
unobfuscated. Everything below is read from that file and cross-checked against
the read-only probes above.

Frame (`Command` base class): 16 bytes, `payload[0]` command id, `payload[1]`
status, `payload[2..4]` EEPROM address (little endian u16), `payload[4]` data
length, `payload[5..15]` data, `payload[15]` checksum with

```
checksum = (0x4D - sum(payload[0..15])) & 0xFF
```

which is the same rule the public `atk-tool` documents, and the same one our
probe uses.

Commands: `DownLoadData=1`, `DownLoadDriverStatus=2`, `GetWirelessMouseOnline=3`,
`GetBatteryLevel=4`, `SetEEPROM=7`, `GetEEPROM=8`, `GetMouseVersion=0x12`,
`Set4KRGBMode=20`, `Get4KRGBMode=21`, `SetFarDistanceMode=22`,
`GetFarDistanceMode=23`, `SetDongleLightMode=24`, `GetDongleLightMode=25`,
`GetDongleVersion=29`, upgrade status 90/91.

EEPROM addresses: `reportRate=0` (+1 complement), `maxDpi=2` (+3),
`currentDpi=4` (+5), `silentHeight=10` (+11), gear blocks `dpi1=12`, `dpi3=20`,
`dpi5=28`, `dpi7=36`, gear colours `dpi1Color=44` … `dpi7Color=68`, RGB
`dpiRGBLightingEffects=76`, `dpiRGBLongBrightBrightness=78`,
`dpiRGBLongBrightSpeed=80`, `dpiRGBEnable=82`, article lamp 84.., every value
followed by `85 - value` (the firmware's own complement byte).

Report rate is a bitmask, not an index: 1000 Hz = 1, 500 = 2, 250 = 4, 125 = 8,
2000 = 16, 4000 = 32, 8000 = 64.

Gear value encoding (`MousePaw3955DpiGear`, 6-byte block at the gear address):

```
xDpi = ((b0 | b1 << 8) + 1) * (b4 & 0x01 ? 2 : 1)
yDpi = ((b2 | b3 << 8) + 1) * (b4 & 0x10 ? 2 : 1)
crc  = 85 - (b0 + b1 + b2 + b3 + b4)   // must equal b5
```

A second model variant stores two gears per 8-byte block as 4-byte entries; the
renderer picks by firmware. Our mouse reports `maxDpi = 4`, so the 6-byte gear
form at 12/20/28/36 is the one to read.

`currentDpi` is a **1-based slot index**, not a DPI value; the real DPI is
`xDpi` of that gear block. A single-byte EEPROM read returns `0xFF` — reads must
cover the whole block (10 bytes at address 0 for the rate/max/current block,
6 bytes per gear).

### Observed conflict and idle behaviour (important for the adapter)

Opening the vendor collection and `WriteFile` always succeed; whether a reply
comes back is what varies. It was observed both ways *with ATK HUB running*:
silence on some runs, and on a later run all seven values came back while the
process was still up. An idle wireless mouse produces the same silence, and
replies resume as soon as it is moved. So the process being present is a hint,
not a verdict:

* "open ok + write ok + no reply" is **not** proof the write reached anything;
* the adapter must retry once, then surface a `conflict` capability (ATK HUB
  running) or a transient error, never a silent success, and the panel must be
  able to re-enumerate once the device answers again;
* we do not yet know whether ATK HUB and this adapter can coexist for *writes*;
  that question is deferred to stage 3b, where a write is attempted with and
  without ATK HUB running and the observed values are compared.

## Stage 3 write path (2026-11-03)

EEPROM writes are the same 16-byte frame with opcode `0x07` (`SetEEPROM`) and
the whole 10-byte block in the data field. The block stores every value as a
`value, 85 - value` pair, so the only safe write is read-modify-write:

1. `GetEEPROM(0x08)` at address 0, length 10, and check all five complements.
2. Patch the pair that belongs to the setting (`report_rate` = bytes 0/1,
   `current_dpi_slot` = bytes 4/5); leave every other byte alone.
3. Send `SetEEPROM(0x07)` with address 0 and the patched block.
4. Sleep 150 ms, read the block again, and fail with `transient` if it is not
   byte-identical. The hub performs its own read-back on top of that.

`report_rate` is the firmware bitmask (`1,2,4,8,16,32,64` -> 1000..8000 Hz), not
an index. The DPI slot is a 1-based index bounded by byte 2 (`max_dpi_slot`).

Numeric DPI stays unavailable: the per-slot blocks at `0x0C/0x14/0x1C/0x24`
answer `0xFF` (length 6/8) or ten zero bytes plus the echoed address (length
12) on this firmware, so there is no honest number to show. The adapter exposes
the slot only.

### Hardware round-trip (2026-11-03, 8K NANO dongle, ATK HUB running)

| what | before | write | read back | restore | read back |
| --- | --- | --- | --- | --- | --- |
| report rate | 250 Hz | 1000 Hz | 1000 Hz, `matched=true` | 250 Hz | 250 Hz |
| DPI slot | 1 | 2 | 2, `matched=true` | 1 | 1 |

Both writes went through `hubd` (not the probe binary) and the panel showed
`Synced · read back 1000 Hz` / `Synced · read back 250 Hz`. ATK HUB was running
throughout, which confirms the write path does not require closing it; a silent
collection still surfaces as `mouse.conflict` rather than a fake value.

## The keyboard: VIA/QMK raw HID (2026-11-03)

The RS7 Air exposes a vendor collection at usage page `0xFF60` / usage `0x61`
with `InputReportByteLength = OutputReportByteLength = 385` and no feature
report. The protocol is plain VIA/QMK raw HID, so no vendor-specific reversing
was needed: request byte 0 is the report id `0x00`, byte 1 is the command, the
arguments follow, and the reply echoes the command at byte 1.

Commands that answer on this firmware:

| Command | Meaning | Reply |
| --- | --- | --- |
| `0x01` | protocol version | `00 01` → 1 |
| `0x11` | keymap layer count | 4 |
| `0x12` | dynamic keymap buffer (`off_hi`, `off_lo`, `size`) | echoed offset and size, then big-endian keycodes |
| `0x0C` | macro count | 8 |

Commands that do **not** work here: `0x04` (get keycode) returns the request's
row/column echoed back, and `0x08` (lighting get) returns `0xFF`. The keymap is
768 bytes (4 layers × 96 keys, 6×16); offsets at and above `0x0300` read as
zero. `adapters/via` therefore advertises only protocol, layers, keys and
macros, and is read-only until the keymap and lighting write paths are verified.

ATK HUB polls the same collection while it runs; replies that do not echo the
expected command are discarded and the read waits for the real one.

### Writing the keymap

`0x13` (set keymap buffer) takes the same `offset_hi, offset_lo, size` prefix
followed by the keycode bytes, and it writes: a spare slot on layer 4 changed
from `0x0001` (`KC_TRNS`) to `0x0068` (`KC_F13`) and read back as `0x0068`, then
restored to `0x0001`. Two quirks matter:

- the command has **no reply frame**, so a successful write is not proof;
- the first `0x12` read after a write can return the pre-write page, so the
  verification must wait (~300 ms) and retry rather than fail immediately.

`adapters/via` implements this as `--write-selftest` and does not advertise a
writable capability yet: remapping needs a key-matrix editor, not a single test
key.
