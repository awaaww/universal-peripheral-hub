# Contributing

Thanks for taking the time to contribute.

## Ground rules

- Be civil. See [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md).
- Every module you add must either work or be explicitly marked as not
  implemented. Placeholder functions that silently do nothing are rejected.
- No new network access at runtime. The hub is offline by default; build-time
  dependency downloads are fine.
- New device support goes through an adapter, not through special cases in the
  UI. If a capability cannot be expressed with the descriptor model, open an
  issue about the model first.

## Development setup

Prerequisites: Rust stable with the MSVC toolchain, Node.js 20+, WebView2.

```powershell
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test
cd ui; npm ci; npm run build
```

`cargo build` at the repository root builds the core crates only. The Tauri
shell (`ui/src-tauri`) is a workspace member but is excluded from
`default-members`, because it needs the frontend bundle and the full desktop
toolchain.

## Adding an adapter

Read [docs/adapter-protocol.md](docs/adapter-protocol.md). An in-process adapter
implements `hub_core::Adapter`; it is registered through an `AdapterFactory`.
Out-of-process adapters are specified but not implemented yet (M3).

## Windows: build outside low-integrity folders

Windows lowers a process token to the integrity level of the executable's file
when that file carries a mandatory label (for example a sandboxed or
"untrusted" build tree). A low-integrity `hubd` cannot send `WM_COPYDATA` to
Explorer, so `Shell_NotifyIconW` fails with `ERROR_ACCESS_DENIED` and no tray
icon appears; writing to `%APPDATA%` may fail for the same reason.

If you see `Shell_NotifyIconW(NIM_ADD) failed error=5`, check the label:

```powershell
Get-Item .\target\debug\hubd.exe | Get-Acl | Format-List
icacls .\target\debug\hubd.exe
```

Either build into a folder that is not labelled (`CARGO_TARGET_DIR=C:\dev\uph-target`)
or run the binaries from an unlabelled copy. This is a property of the checkout
location, not a defect in the hub.

## Commit messages

Conventional Commits (`feat:`, `fix:`, `docs:`, `refactor:`, `test:`, `chore:`).
Keep the subject line under 72 characters, write the body in the imperative mood
and explain the *why*, not the diff.

## Pull requests

- One logical change per pull request.
- CI must be green: `fmt`, `clippy -D warnings`, tests, frontend build.
- Behaviour changes need a test. Device-specific quirks need a comment with the
  model and firmware where they were observed.
- Update `CHANGELOG.md` under `## Unreleased` for user-visible changes.

## License of contributions

By submitting a pull request you agree that your contribution is licensed under
the MIT license of this repository.
