# Security policy

## Supported versions

Only the latest release receives fixes. This is a young project; assume earlier
builds have unfixed issues.

## How the hub touches devices

- `hubd` runs as the signed-in user. It does not install a kernel driver, does
  not request elevation, and listens on a per-user named pipe
  (`\.\pipe\universal-peripheral-hub-<user>`) rather than a network port.
- Adapters may open HID devices and DDC/CI handles. Out-of-process adapters
  speak JSON lines over stdio and are supervised by `hubd`; a crash in one does
  not take the core down.
- Nothing phones home. There is no telemetry, update check or analytics. The
  only network use in the build is fetching Rust crates and npm packages.

## Reporting a vulnerability

Open a private security advisory on GitHub (Security → Report a vulnerability)
rather than a public issue. Please include the version, the adapter involved and
what an attacker would gain. Expect an initial reply within a week.

## Known limitations

- The installer is not code-signed yet, so Windows SmartScreen will warn.
- Capability writes are not authenticated beyond the per-user pipe: any process
  running as the same user can talk to `hubd`. That matches the trust boundary
  of a desktop utility; do not run untrusted code as the same user.
