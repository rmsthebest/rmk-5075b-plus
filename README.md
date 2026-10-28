# RMK firmware for the Akko 5075B Plus

Rust firmware ([RMK](https://github.com/HaoboGu/rmk) on [Embassy](https://embassy.dev)) for the
Akko 5075B Plus, a 75% wireless keyboard on a WestberryTech WB32FQ95 (Cortex-M3, 96 MHz). It
replaces the vendor's QMK build with feature parity:
- USB, Bluetooth 1-3 and the 2.4G dongle;
- Vial;
- the RGB matrix and side lights;
- the knob;
- battery reporting;
- sleep and stop mode.

There was no Rust support for the WB32, so this repo also carries a PAC and a HAL for the chip.

## What's here

| Path | Crate | What it is | Reusable? |
|---|---|---|---|
| `crates/wb32-pac` | `wb32-pac` | Register access for the WB32FQ95, generated from the vendor header | Any WB32FQ95 project |
| `crates/wb32-hal` | `wb32-hal` | Embassy drivers for the WB32FQ95 | Any WB32FQ95 project, with the limits in its README |
| `crates/qmk-wireless` | `qmk-wireless` | Sans-I/O protocol for the BT/2.4G module in QMK's `keyboards/linker/wireless` boards | Keyboards with that module |
| `crates/akko-boot-guard` | `akko-boot-guard` | Reset-time escape into ROM DFU, crash record and crash policy | This board |
| `crates/akko-lighting` | `akko-lighting` | QMK rgb_matrix port plus this board's lights and indicators | This board (the rgb_matrix part is generic in nature, but tied to this layout) |
| `firmware` | `firmware` | The `akko-rmk` image and the bring-up images `m1a-bounce`, `m1b-stub`, `m2-blink`, `m3-usb` | This board |
| `third_party/rmk` | `rmk` | Submodule: [our fork](https://github.com/rmsthebest/rmk), branch `akko-0.9`: RMK 0.9.0 with our hooks and fixes (`rmk/README-akko.md`) | |
| `third_party/musb` | `musb` | Submodule: [our fork](https://github.com/rmsthebest/musb), branch `wb32`: musb 0.3.1 with WB32 support and fixes (`README-wb32.md`) | |
| `sim/` | | Renode simulation of the board and the test suites (`sim/README.md`) | |
| `rom/` | | The chip's ROM bootloader, studied: read-out (`rom-peek` feature), disassembly tools, the ROM in Renode, write-up (`rom/README.md`). The ROM itself isn't in the repo | |
| `tools/` | | Generators (PAC, keymap, LED layout, Vial JSON), checks (precheck, rgbref, PAC vs manual), the workbench | |

Naming: a chip or protocol name means the crate is meant to be reusable. An `akko-` prefix means it's
written for this board.

## Build, test, flash

```fish
git submodule update --init              # third_party/rmk and third_party/musb
cargo build --release --bin akko-rmk     # target thumbv7m-none-eabi (needs flip-link)
sim/run.sh                               # every sim suite (about 20 min); SIM_CHANGED=1 for the affected ones
cargo test -p qmk-wireless --target x86_64-unknown-linux-gnu
cargo test -p akko-lighting --target x86_64-unknown-linux-gnu
python3 -I tools/rgbref/run.py           # akko-lighting against QMK's C, frame by frame
python3 -I tools/bench.py                # workbench on :8475: sim runs, freeze, flash RMK or QMK
```

## Flashing safety

The case can't be opened: there's no SWD and no BOOT0. The ROM DFU bootloader can only be reached
through code in the running image. That's either the RAM magic word, which QMK and RMK both honour,
or Esc held at a cold start.

So, before an image goes to the board:
- every image must link `akko-boot-guard`;
- `tools/precheck.py` must pass on it;
- it must be frozen and tested in the sim first.

The workbench refuses to flash anything else.

## License

GPL-2.0-or-later (`LICENSE`). Much of this code is a port of QMK and of the vendor's QMK keyboard
code. `third_party/` keeps its upstream licenses: rmk is MIT OR Apache-2.0, musb is Apache-2.0. `rom/rust/` (the ROM bootloader rewritten in Rust) is MIT OR Apache-2.0.
