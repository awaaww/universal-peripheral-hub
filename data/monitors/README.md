# Per-monitor overrides

This directory ships **no monitor definitions yet**. The DDC/CI adapter (M1) is
the first consumer; the format is fixed here so that M1 does not invent a
second one.

## Why an override file exists at all

DDC/CI is a standard with generous room for interpretation. Monitors differ in:

- which VCP codes they actually implement, and which ones report success while
  doing nothing;
- whether a code needs a settle delay longer than the 120 ms the hub uses by
  default before the value can be read back;
- whether a write requires the display to be on the same input;
- whether the capability string can be trusted at all.

The hub adapts per model, never by guessing at runtime, and never by shipping
vendor software.

## File naming and lookup

One JSON file per model, named by the EDID identity `<mfg><product>.json`, all
uppercase, e.g. `GSM5B7C.json` for an LG panel whose EDID reports manufacturer
`GSM` and product `0x5B7C`. The adapter reads the EDID from the monitor, looks
up this directory, and falls back to generic behaviour when no file matches.

Files in `%APPDATA%\UniversalPeripheralHub\monitors\` override the files shipped
here, per file, so a user can fix a model without forking.

## Schema

Strict JSON (no comments, no trailing commas), so it can be validated in CI and
parsed by any JSON library. Unknown keys are rejected — a typo must fail loudly
rather than silently do nothing.

```json
{
  "schema": 1,
  "edid": "GSM5B7C",
  "name": "LG 27GP850",
  "settle_ms": 200,
  "quirks": {
    "requires_open_handle_per_write": false,
    "unsupported_vcp": ["0xE0"],
    "ignored_capabilities": ["0x14"]
  },
  "capabilities": {
    "brightness": { "vcp": "0x10", "min": 0, "max": 100, "settle_ms": 150 },
    "contrast": { "vcp": "0x12", "min": 0, "max": 100 },
    "input": {
      "vcp": "0x60",
      "values": { "0x0f": "DisplayPort", "0x11": "HDMI-1", "0x12": "HDMI-2" }
    }
  }
}
```

- `settle_ms` — how long to wait after a write before reading back. The default
  is 120 ms; KTC panels needed 100–400 ms in our own testing.
- `unsupported_vcp` — codes the monitor rejects; the adapter marks the matching
  capability as unsupported without probing it on every start.
- `ignored_capabilities` — codes reported by the capability string but known to
  be broken on this model.
- `capabilities.*.vcp` — the code the adapter should use for a stable
  `cap_type`, when it is not the standard one.
- `capabilities.input.values` — display values are model-specific; a monitor
  may use `0x0f` for DisplayPort while another uses `0x10`.

## Adding a definition

1. Run the adapter's probe against the monitor (`cargo run -p hubd -- --selftest
   --probe-display`, M1) and read the capability string.
2. Verify each capability by writing the value and reading it back; record the
   settle time that actually worked, not the one that "should" work.
3. Add the file, run `cargo test -p hub-ddc`, and mention the model in the PR.

Do not copy definitions from other projects without checking them: Exo's files
are a useful cross-reference, but they use `//` comments, Exo resource GUIDs for
labels, and a different `ignoredCapabilitiesVcpCodes` semantic. We keep our own
format and our own labels.
