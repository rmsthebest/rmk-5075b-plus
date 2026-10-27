# rmk 0.9.0, vendored

Copied from crates.io (rmk-0.9.0, without its tests) and patched for the
Akko 5075B Plus port. Changes, all marked `akko-5075b-plus`:

- `src/host/via/mod.rs`: `set_custom_get_value_hook` lets the firmware
  answer VIA's id_custom_get_value (0x08). The keyboard uses it for live
  scan diagnostics (see akko-5075b-plus/src/scan.rs).
