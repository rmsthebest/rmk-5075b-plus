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
- `src/channel.rs`: `set_report_hook` lets the firmware take reports before
  any transport (the radio module on UART3, akko-5075b-plus/src/radio.rs,
  which RMK doesn't know), and `release_usb_reports` leaves USB an all-up
  report when the reports move to the radio.
- `src/hid.rs`: `set_host_leds` sets the host's lock LEDs from outside a
  transport's LED reader (the radio module reports the host's LEDs).
- `src/hid.rs`, `src/usb/mod.rs`: a battery report on USB, report id 0x05
  of the composite interface (`CompositeReport`'s descriptor gains an
  application collection: Generic Device Controls' Battery Strength 0-100
  and Battery System's Charging; Linux shows it as a power supply).
  `set_battery` hands USB's writer the latest `BatteryReport`; the writer
  sends it only while configured (it must not wake a suspended host) and
  drops it after 20 ms if the host isn't reading, as steno's reports, so it
  can't hold up the keys. The BLE report map is unchanged.
- `build.rs`: `RMK_BUILD_HASH` in the environment (set in .cargo/config.toml)
  replaces the storage build hash, which was the commit and the build time.
  Storage reinitialises (the keymap back to the default) when the hash
  changes; fixed, a new image keeps the keymap edited in Vial, as QMK's
  EEPROM does.

Fixes:

- `src/host/via/mod.rs`, `src/host/context.rs`: VIA's
  `DynamicKeymapSetBuffer` (0x13) reads `size / 2` big-endian keycodes from
  key index `offset / 2`, as `GetBuffer` writes them. It read `size`
  little-endian keycodes from key index `offset`, past the end of the
  32-byte report at size 28 (a panic). Each key's save now waits for room in
  the flash channel instead of being dropped when it's full (sim:
  `m4_rmk.robot` "VIA Bulk Keymap Write Sets And Keeps Keys").
- `src/storage/mod.rs`: the map storage caches page states and pointers
  (`CACHE_PAGES` = 16 sectors at most). Uncached, every operation re-read
  each page's header and walked to its end over the 3 MHz SPI flash.
  `Storage::erase_all` starts a fresh cache after erasing:
  `MapStorage::erase_all` keeps the old one, and page states cached from a
  previous layout sent writes to the wrong place (found in the sim, moving
  the board's 4-sector storage to 16 sectors: the next boot wiped it).

RAM and stack (2026-10-27 headroom pass):

- `src/storage/mod.rs`: `update_storage_field!` binds the fetched item
  before storing, so its `StorageData` isn't held across `store_item` (it
  was, once per use: ~0.6 KB of the main future here). The read/write
  buffer is 128 bytes when macros are under 120 (was 256).
- `src/keyboard/held_buffer.rs`: `sort_held_by_key`, an insertion sort, in
  place of `sort_unstable_by_key` at every held-buffer sort (at most 16
  keys; core's sort had a 1.6 KB stack frame and 8 KB of code).
- `src/keyboard.rs`: the combo path is skipped when `COMBO_MAX_NUM` is 0.
