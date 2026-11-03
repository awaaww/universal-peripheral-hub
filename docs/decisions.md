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

## D14 — A VCP code is negative-cached after two failures

Monitors advertise codes in their MCCS string that they then reject. Exo and
ddcci.js both learn this the hard way: without a negative cache, a single lying
code costs two round trips on every refresh and makes the whole panel feel
broken. `adapter-ddc` counts strikes per `(monitor, code)`, retries twice with
70 ms between attempts, and after two failed rounds refuses the code locally
until the adapter is reloaded. A successful read clears the strike count. A
rejected read is reported as `unsupported`, not `transient`, so callers do not
retry forever.

## D15 — DDC writes settle before the read-back

The hub reads every write back to report `matched`. On the KTC H27T22S a
read issued immediately after `SetVCPFeature` returns the *old* value; ~120 ms
is enough. `settle_ms` default is 120 and can be raised per model in
`data/monitors/<KEY>.json`. The sleep lives in the adapter, not the hub, because
it is a property of the device, and the hub stays generic.

## D16 — Integration tests opt out of the DDC adapter

`Service::bootstrap` registers the hardware-dependent DDC adapter, which makes
device counts machine-dependent. Tests use
`Service::bootstrap_with(..., ServiceOptions { ddc: false })`; only the mock
adapter is registered there. This keeps the pipe contract test hermetic without
a feature flag or environment variable.

## D17 — Blocking is adapter-scoped and applied by rebuilding the adapter

A monitor behind a one-way splitter can make every DDC request wait seconds,
and the GPU's I2C bus is shared, so one bad link stalls all of them. The block
list therefore has to be consulted *before* the first request, i.e. inside the
adapter, not filtered afterwards in the hub.

Two consequences:

* The persisted token is adapter-specific (`SKG2725`, an EDID key), so the
  setting is keyed by adapter id: `blocked: { "ddc": ["SKG2725"] }` on the wire,
  `ddc_blocked` in `settings.json`. Clients never interpret tokens, they echo
  them back with `{ adapter, token }`.
* Changing the list rebuilds that adapter (unload + load). Re-enumeration is
  cheap when the blocked monitor is skipped, and it avoids a second, live copy
  of the list drifting out of sync with the file.

`device.set_blocked` accepts either a device id (from a card) or the
`(adapter, token)` pair (from the restore list), because a blocked device is by
definition not enumerated and therefore has no id to address.

## D18 — The panel's pipe client reads while idle

The first client only read the pipe while waiting for a response to a request it
had just sent. `hubd` pushes `devices.changed` on its own schedule (a tray menu,
a second window, a block from another client), so those pushes sat in the pipe
until the next request — the panel showed a stale device list and the block
control looked broken. The client now runs a dedicated reader task and selects
between it and the command channel, so a push is delivered immediately.

The reader is a separate task rather than a `select!` on `read_line`, because
`AsyncBufReadExt::read_line` is not cancel safe: dropping it mid-line loses
bytes that were already consumed from the socket.

## D19 — The startup baseline is seeded explicitly

The WinEvent hook reports *changes*, so a hub that starts while Notepad is in
front sees nothing until the user switches away. The engine then treats that
first real switch as the startup baseline and applies nothing — the scene for
the app you were already using never runs.

`main` therefore asks for the current foreground once, right after installing
the hook, and feeds it to the engine as the baseline. The hook itself stays a
pure change feed: it does not call back during setup, so the ordering is visible
at the call site and the baseline cannot race the first change.

If there is no foreground window yet (boot, lock screen), the baseline is
deferred with a warning; the first change then becomes the baseline, which is
the old behaviour and still safe (nothing is written).

## D20 — Scenes address capabilities by `cap_type`, not flat keys

The plan sketched scene items as flat keys (`monitor.brightness`), which ties a
scene to one naming scheme and one device. The implementation uses the
capability model from D2: an item is `{ cap_type, device?, value }`, where
`cap_type` is the stable cross-device name.

Consequences:

* one scene covers every display that has `display.brightness`; an optional
  `device` pins an item when the user really means one device;
* items are expanded at *apply* time, so a scene saved before a monitor was
  connected still works after it appears;
* a `cap_type` no present device provides is reported as `missing` in the
  apply report, not as a failure — the rest of the scene still applies.

## D21 — An invalid scene document does not replace the live one

`scenes.set` validates before it writes. A document that parses but is invalid
(a rule pointing at a scene that does not exist, two rules both marked default,
an empty `exe`) is rejected with `invalid_value`, and the in-memory document is
left untouched.

On load, an invalid or unparsable file is quarantined next to the original as
`scenes.json.corrupt-<timestamp>` and the engine starts empty, rather than
loading half a document or deleting the user's file.

## D22 — Renegotiating capabilities are applied first

When a scene contains an HDR, input-source or picture-mode item, those items are
stably sorted to the front of the apply order. A renegotiation can make the
device re-enumerate or briefly drop its handle; doing it after brightness writes
would mean the writes were sent to a device that is about to change mode. The
report keeps one line per item regardless of order.

## D23 — The foreground watcher is one per process and owns a thread

`ForegroundWatcher::spawn` installs `SetWinEventHook(EVENT_SYSTEM_FOREGROUND,
…, WINEVENT_OUTOFCONTEXT)` and runs a message pump on a thread it owns. Two
guards follow from that: a second watcher in the same process is refused
(`AlreadyExists`), because Win32 events are delivered per thread and the second
pump would silently receive nothing; and `Drop` unhooks, kills the fallback
timer, posts `WM_QUIT`, and joins, so a scene engine cannot outlive its hook.

If the hook cannot be installed, the same thread falls back to a 2 s `SetTimer`
poll. That is slower than the hook but still bounds the scene latency, and the
log records the degradation instead of silently polling.

## D24 — The panel is told *why* a scene is current

`runtime.current_scene` is "the scene in effect", not "the scene the rule
picked". After a manual Apply those differ: the rule for the front app may say
`daily` while the user just applied `focus` by hand. Labelling that line
"Auto: … → Focus" would be a small lie that is hard to debug from the UI.

The engine therefore records an `AppliedBy` (`startup`, `rule`, `manual`) and
exposes it as `runtime.last_source`; the panel prints "Auto: <exe> → <scene>"
only for `rule`, and "Applied manually: <scene>" otherwise. The distinction is
also what the scene tests assert, so it cannot rot into a UI-only convention.

## D25 — Out-of-process adapters are supervised, not trusted

The ATK adapter is the first one that runs as a child process. `hubd` gives it
the stdio dialect, a 3 s handshake and per-call deadlines, and keeps stderr in
the hub log. Three consecutive timeouts, a protocol violation, or the child
exiting latches the adapter as disabled — every later call fails fast with
`Unsupported` naming the adapter — and the child is killed and reaped. The panel
or tray re-enables it, which spawns a fresh process and re-handshakes.

The alternative (auto-restart on every failure) was rejected: a vendor helper
that crashes on startup would otherwise spin forever and hide the reason from
the user.

## D26 — The ATK collections are three different protocols

`373B:11FE` (the 8K mouse dongle) exposes a 17-byte vendor collection that
answers the 16-byte frame described in `docs/re/atk-hub.md`. With the same
frame, `373B:1258` (RS7 Air keyboard) answers with report id 0 and empty
payloads — it is a VIA/QMK device (`usage_page 0xFF60`, confirmed in the
renderer bundle), and `373B:1204` (N9 Ultra headset) never answers at all; it
also exposes a COM port. Treating them as one family would be a guess dressed
up as support, so the ATK adapter only exposes what it has actually decoded:
the mouse. The keyboard and headset are documented as not implemented.

## D27 — An idle wireless mouse looks exactly like a conflict

Two different conditions produce "write ok, no reply": ATK HUB holding the
collection, and the mouse being asleep after inactivity. Both were observed.
The adapter cannot tell them apart from the pipe alone, so it:

* retries the probe three times with a 150 ms gap during enumeration;
* treats a missing reply as `Transient` (never as success);
* exposes `conflict` as a read-only bool derived from `ATK HUB.exe` being
  present, so the UI can say "ATK HUB is running" without claiming the reads are
  broken — they often are not, and later measurements read every value while it
  ran; closing it is the workaround, not a requirement;
* documents "move the mouse once" in the README, because that is the real fix
  for the sleep case.

This is deliberately conservative: a false "conflict" is visible and harmless,
a false "success" is not.

When the mouse's control collection stays silent without the process being the
explanation, the adapter can only report what it knows: one device carrying only
the read-only `mouse.conflict` bool when ATK HUB is present, an empty list
otherwise. The panel therefore shows "ATK HUB is running" instead of an empty
adapter row, and `Rescan` re-enumerates after the device wakes up.
It never invents battery or DPI values for a device it could not talk to.

## D28 — EEPROM writes are read-modify-write of the whole block

The ATK mouse stores settings as `value, 85 - value` pairs inside one 10-byte
EEPROM block. Writing a single field would desynchronise the complements, so
every write reads the block, patches only the pair belonging to the capability,
writes the block back with opcode `0x07`, waits 150 ms for the dongle to commit,
then reads it back and fails `transient` on any mismatch. This is the same
sequence the vendor app performs; the hub also re-reads after every write, so a
silent failure surfaces in the panel instead of looking applied.

## D29 — VIA keymaps are read through the raw HID buffer, not per-key

The RS7 Air answers `0x12` (dynamic keymap buffer) with big-endian 16-bit
keycodes, but the per-key `0x04` command returns junk and the lighting `0x08`
command returns `0xFF`. Enumeration therefore derives the keyboard size from the
buffer itself: 32-byte pages are scanned until an empty page that still has the
verified 768-byte window after it (the board's last page is `KC_NO`, so a strict
"first empty page stops the scan" rule reports 92 keys), then
`keys = bytes / 2 / layers`. The adapter advertises only values it can actually
decode — protocol version, layers, keys, macros — and `write()` returns
`unsupported` instead of offering a control the firmware cannot honour.

## D30 — The VIA write path is proven headlessly before it gets a UI

`0x13` (set keymap buffer) does write on the RS7 Air, but it has no reply frame
and the first read after a write can return the pre-write page. The adapter
therefore waits 300 ms and re-reads up to three times, and `via-adapter
--write-selftest` performs the whole round-trip on the last keymap layer: read a
spare slot, write `KC_F13`, read back, restore the original keycode, read back,
and exit non-zero on any mismatch. The restore is issued even when the write is
not visible, so a failed experiment cannot leave a key remapped.

No writable capability is advertised yet. Remapping is a per-key editor (96 keys
times 4 layers), and the panel has no key matrix UI; exposing one arbitrary
"test key" control would be a fake feature. The transport, settle and restore
rules are documented and tested, so the editor can be added without reopening
the protocol question.

## D31 — The keymap is edited as one `bytes` capability, not a private RPC

The editor writes the complete keymap image (`4 layers x 96 keys x 2 bytes =
768 bytes`) through `capability.write` on `keyboard.keymap`, a `Bytes` capability
advertised as read/write. The adapter diffs that image against the board, sends
only the changed keycodes in frames of at most 32 bytes, waits, reads the image
back, and restores the original if the read-back disagrees. The pre-edit image is
copied to `%APPDATA%\UniversalPeripheralHub\keymap-backups\` once per helper process,
before the first frame.

Rejected alternatives: one RPC per key (a second protocol next to the capability
model, and the hub's read-back guarantee would be bypassed); a per-key capability
(96 capabilities on one device, and scene code would have to know key names); a
"test key" control (a fake feature). The panel keymap view is selected by
`cap_type == "keyboard.keymap"`, never by device id, so another keyboard adapter
that advertises the same capability gets the same editor.

## D32 — Auto-start is a per-user `Run` value, not a scheduled task

`hubd --autostart on` writes
`HKCU\Software\Microsoft\Windows\CurrentVersion\Run\UniversalPeripheralHub` with
the quoted path of `hubd.exe`; `off` deletes it; `status` reads it back. The tray
menu carries the same toggle ("Start with Windows") and shows a check mark when
the value points at the running executable.

Rejected alternative: a Task Scheduler entry at logon. It buys a delay and a
"run with highest privileges" flag we do not need — the core talks to HID and
DDC devices as the logged-in user — while costing a UAC prompt on install and
an extra artifact (a `schtasks` XML file) that users cannot inspect easily. A
per-user `Run` value is visible in Task Manager's Startup tab, needs no
elevation, and is removed by the uninstaller. If a future adapter needs
elevation it gets its own opt-in prompt; the core never starts elevated.

The installer registers auto-start for the current user only (`installMode:
currentUser`), and the uninstaller removes the value before it kills the
processes. `hubd` starts with no arguments from both the installer and the Run
key, so it hides its own console window at startup; the CLI modes keep the
console.
