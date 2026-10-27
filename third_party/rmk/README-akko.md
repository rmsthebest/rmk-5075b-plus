# rmk 0.9.0, vendored

Copied from crates.io (rmk-0.9.0, without its tests) and patched for the
Akko 5075B Plus port. Changes, all marked `akko-5075b-plus`:

- `src/host/via/mod.rs`: `set_custom_value_hook` lets the firmware answer
  VIA's id_custom_set_value, get_value and save (0x07-0x09). The keyboard
  uses them for live scan diagnostics (see akko-5075b-plus/src/scan.rs) and
  Vial's lighting tab (VialRGB, akko-5075b-plus/src/lighting.rs).
- `src/keyboard.rs`: `set_action_hook` lets the firmware see each action
  before the keyboard processes it, and keep it (the board's lighting
  recording mode takes key presses; EE_CLR waits for its 3 s hold).
