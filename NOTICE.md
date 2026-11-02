# NOTICE

Universal Peripheral Hub is licensed under the MIT license (see `LICENSE`).

## Prior art and protocol references

This project reimplements behaviour that was learned by reading public
documentation and public source code. **No source code from the projects below
is included in this repository.** Where an implementation follows the semantics
of another project (for example the classification of DDC/CI error codes), this
is stated in the source file and in `docs/decisions.md`.

- **Exo** — https://github.com/hexawyz/Exo (MIT). Reference for the DDC/CI error
  taxonomy, capabilities-string parsing and per-EDID monitor overrides. The
  `DeviceTools.DisplayDevices` behaviour is reimplemented in Rust for M1; no
  assemblies are referenced.
- **ddcutil** — https://www.ddcutil.com/ (GPL-2.0). Used as documentation for
  DDC/CI command semantics only. No code is linked or copied.
- **ddcci.js** — the 376-line reference implementation shipped with this
  project's design package. It is an internal reference, not a dependency; its
  known defects (broken `reopen`, duplicated monitor handles, busy-wait sleep)
  are fixed rather than ported.
- **OpenRGB** — https://openrgb.org/ (GPL-2.0). Prior art for RGB capability
  modelling. No code is copied.
- **libratbag / Piper** — https://github.com/libratbag/libratbag (MIT).
  Prior art for polling-rate and button/profile modelling. No code is copied.

## Runtime dependencies

Third-party Rust and npm dependencies keep their own licenses; `cargo deny` /
`cargo about` integration is planned for M6. Tauri, WebView2 and the Microsoft
Visual C++ runtime are distributed under their own terms.

## Trademarks

Product and vendor names (monitor, keyboard, mouse, headset and controller
brands) are trademarks of their respective owners and are used only to describe
compatibility.
