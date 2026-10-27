# rmk 0.9.0, vendored

Copied from crates.io (rmk-0.9.0, without its tests) and patched for the
Akko 5075B Plus port. Changes, all marked `akko-5075b-plus`:

- `src/host/via/mod.rs`: `set_custom_value_hook` lets the firmware answer
  VIA's id_custom_set_value, get_value and save (0x07-0x09). The keyboard
  uses them for live scan diagnostics (see akko-5075b-plus/src/scan.rs) and
  Vial's lighting tab (VialRGB, akko-5075b-plus/src/lighting.rs).
- `src/keyboard.rs`: `set_action_hook` lets the firmware see each action
  before the keyboard processes it, replace it (Right Ctrl as App), or keep
  it (the board's lighting recording mode takes key presses; EE_CLR waits
  for its 3 s hold).
- `src/host/via/keycode_convert.rs`: QMK's magic keycodes (0x7000-0x701F,
  e.g. GU_TOGG) map to and from `Action::User(0x80 | n)` instead of No, so
  the firmware can handle them and Vial shows them as they are.
- `src/boot.rs`: `set_bootloader_hook` gives `jump_to_bootloader` (VIA's
  bootloader jump) the board's way into DFU (boot-guard).
- `build.rs`: `RMK_BUILD_HASH` in the environment (set in .cargo/config.toml)
  replaces the storage build hash, which was the commit and the build time.
  Storage reinitialises (the keymap back to the default) when the hash
  changes; fixed, a new image keeps the keymap edited in Vial, as QMK's
  EEPROM does.
