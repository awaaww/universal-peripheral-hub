# Out-of-process adapter protocol

> **Status: not implemented.** `hubd` only loads in-process adapters today. This
> document fixes the wire format for M3 so that the host side and the first
> third-party adapter cannot drift apart. Nothing in this repository speaks this
> protocol yet; the mock adapter is compiled in.

An out-of-process adapter is an executable that `hubd` spawns and talks to over
stdin/stdout. It must not open a window, must not require elevation, and must not
write to stdout outside the protocol (use stderr for logs; the host forwards
them to its own log with the adapter id as prefix).

## Transport

- One JSON object per line, UTF-8, `\n` terminated. Maximum 1 MiB per line.
- stdin is host → adapter. stdout is adapter → host. stderr is free-form logs.
- The process exits when stdin closes. The host kills it after 5 s if it does
  not.

## Handshake

The host writes a request, the adapter answers:

```jsonc
// host → adapter
{"jsonrpc":"2.0","id":1,"method":"adapter.hello","params":{"protocol":1,"hub_version":"0.1.0"}}
// adapter → host
{"jsonrpc":"2.0","id":1,"result":{"id":"vendor-x","name":"Vendor X Devices","version":"0.1.0","protocol":1}}
```

`id` must match the `AdapterInfo.id` the adapter would return in-process. The
host refuses the adapter (and logs why) if `protocol` differs or the process does
not answer within 3 s.

## Methods

| Method | Params | Result |
| --- | --- | --- |
| `adapter.enumerate` | `{}` | `{"devices":[DeviceDescriptor…]}` |
| `capability.read` | `{"device":"…","cap":"…"}` | `{"value":CapabilityValue}` |
| `capability.write` | `{"device":"…","cap":"…","value":CapabilityValue}` | `{}` |

`DeviceDescriptor` and `CapabilityValue` are byte-for-byte the JSON produced by
`hub-core`'s serde derives (`crates/hub-core/src/device.rs`,
`crates/hub-core/src/value.rs`). Do not invent a parallel schema; generate your
types from the Rust definitions or copy them exactly.

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

An adapter that returns `{}` for a write but leaves the device unchanged is
lying: the host reads back and reports the mismatch, and repeated mismatches
disable the adapter for the session.

## Timeouts and restarts

- The host applies 10 s to `enumerate`, 5 s to reads, 5 s to writes. A timeout is
  reported as `Timeout`; the host does not kill the adapter on the first one.
- Three consecutive timeouts, or a process exit, disable the adapter until the
  panel or tray re-enables it. The device handles are released by the process
  dying, which is one reason this mode exists.
- Restart on enable, not automatically: a crash loop must be visible, not
  silent.

## Security

- The host passes no credentials, no tokens, no network configuration. The
  default build never contacts the network; an adapter that does is out of
  scope for the project's zero-network guarantee and must say so in its
  `AdapterInfo.name` or documentation.
- The adapter runs with the user's privileges. There is no sandbox: an adapter is
  trusted code, chosen by the user. The process boundary is about crash and hang
  isolation, not security.
