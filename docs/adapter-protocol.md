# Out-of-process adapter protocol

> **Status: implemented in M3.** The normative types live in
> `crates/hub-proto/src/proc.rs`; host side in `crates/hubd/src/proc.rs`, first
> implementation in `adapters/atk`. This document explains the contract; when
> the two disagree, the Rust types win and this file is the bug.

An out-of-process adapter is an executable that `hubd` spawns and talks to over
stdin/stdout. It must not open a window, must not require elevation, and must not
write to stdout outside the protocol (use stderr for logs; the host forwards
them to its own log with the adapter id as prefix).

## Why a process boundary

Private protocols need reverse engineering, vendor binaries and sometimes
third-party runtimes. A crash, a deadlock in a vendor DLL or an infinite loop
must not take down the tray icon or the monitor control that was working fine.
The host therefore kills and restarts this category of adapter, and it can be
re-enabled after a crash loop instead of failing silently.

## Transport

- One JSON object per line, UTF-8, `\n` terminated. Maximum 1 MiB per line; a
  longer line is a protocol error and the connection is dropped.
- stdin is host → adapter. stdout is adapter → host. stderr is free-form logs.
- The process exits when stdin closes. The host sends `adapter.shutdown` first
  (best effort) and kills the process 5 s after closing stdin.

## Handshake

The host writes a request, the adapter answers:

```jsonc
// host → adapter
{"jsonrpc":"2.0","id":1,"method":"adapter.hello","params":{"protocol":1,"hub_version":"0.1.0"}}
// adapter → host
{"jsonrpc":"2.0","id":1,"result":{"id":"vendor-x","name":"Vendor X Devices","version":"0.1.0","protocol":1}}
```

`result.id` becomes the device-id prefix (`vendor-x:…`). The host refuses the
adapter (and logs why) if `protocol` differs or the process does not answer
within 3 s.

## Methods

| Method | Params | Result |
| --- | --- | --- |
| `adapter.enumerate` | `{}` | `{"devices":[DeviceDescriptor…]}` |
| `capability.read` | `{"device":"…","capability":"<adapter-local id>"}` | `{"value":CapabilityValue}` |
| `capability.write` | `{"device":"…","capability":"…","value":CapabilityValue}` | `{"accepted":true}` |

`DeviceDescriptor` carries its `capabilities` array — the host builds the panel
from it and never hard-codes a control. Both types are byte-for-byte the JSON
produced by `hub-core`'s serde derives (`crates/hub-core/src/device.rs`,
`crates/hub-core/src/capability.rs`, `crates/hub-core/src/value.rs`). Do not
invent a parallel schema; depend on `hub-core` and serialise its types.

`capability` is the adapter-local `id` (for example `battery`), **not** the
stable `cap_type` (`mouse.battery`). The host resolves `cap_type` → `id` before
calling, so an adapter can rename its internals without breaking scenes.

Notifications from adapter to host (no `id`):

| Method | Params | Meaning |
| --- | --- | --- |
| `devices.changed` | `{}` | Re-enumerate; the device set is stale |

## Errors

```jsonc
{"jsonrpc":"2.0","id":7,"error":{"code":-32000,"message":"I2C read failed",
 "data":{"kind":"transient","retryable":true}}}
```

- `code` is an integer; use `-32000` for adapter-originated failures.
- `data.kind` is one of the `ErrorKind` names in
  `crates/hub-core/src/error.rs`, lowercased (`unsupported`, `transient`,
  `timeout`, `protocol`, `permission_denied`, …).
- `data.retryable` is the adapter's own opinion. The host may still cap retries;
  a `false` here is binding.

An adapter that returns `{"accepted":true}` but leaves the device unchanged is
lying: the host reads back and reports the mismatch, and repeated mismatches
disable the adapter for the session.

## Timeouts and restarts

Constants are in `hub-proto::proc`:

- handshake 3 s, `enumerate` 10 s, read/write 5 s;
- a timeout is reported as `Timeout` and the adapter is not killed for the first
  one;
- three consecutive timeouts, or a process exit, disable the adapter until the
  panel or tray re-enables it. Device handles die with the process, which is one
  reason this mode exists;
- restart on enable, never automatically: a crash loop must be visible.

## Visibility rules

An adapter must not fail silently. When it cannot do its job, the reason must
reach a capability the user can see or a log line the user can find:

- missing runtime/dependency → fail the handshake with a message naming it;
- device busy or claimed by the vendor app → expose a read-only boolean
  capability (for example `conflict`) whose label says what to close, instead of
  returning empty devices;
- partially supported firmware → omit the capability, do not advertise a
  control that cannot work.

## Security

- The host passes no credentials, no tokens, no network configuration. The
  default build never contacts the network; an adapter that does is out of
  scope for the project's zero-network guarantee and must say so in its
  `AdapterInfo.name` or documentation.
- The adapter runs with the user's privileges. There is no sandbox: an adapter is
  trusted code, chosen by the user. The process boundary is about crash and hang
  isolation, not security.
