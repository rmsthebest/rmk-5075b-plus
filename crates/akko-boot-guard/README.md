# akko-boot-guard

A way back into DFU for the Akko 5075B Plus. The case can't be opened (no SWD, no BOOT0), so the
WB32's ROM DFU bootloader at `0x1FFF_E000` can only be reached through code in the running image.
Every image must link this crate, and `tools/precheck.py` refuses an image without it.

## What it does
- **`__pre_init`** (plain assembly, before RAM init and clocks):
  - if the RAM magic word is set (`0xDEADBEEF` at `0x2000_6FFC`, QMK's protocol, so QMK and RMK
    images hand over to each other), clear it and enter the ROM bootloader;
  - if Esc is held (column C0 driven low, row A0 read), set the word and reset into DFU.
- `HardFault` and `DefaultHandler` go to DFU. `request_bootloader()` is for the bootloader keycode.
- **`panic-dfu` feature:**
  - a panic handler that keeps the message for the next boot (`take_panic_message`) and resets;
  - a second panic before `mark_stable()` goes to DFU, so a crash loop ends where it can be
    reflashed.
- **`always-dfu` feature:** enter DFU on every boot, for bring-up.

## Board-specific
The Esc pins, the magic address (QMK's 28K RAM layout), and the panic record at `0x2000_7000` /
counter at `0x2000_7100`, which match `firmware/memory.x`, are all this board's. The ROM entry
sequence itself is generic to the WB32FQ95.

## Tested how
- `sim/tests/boot_guard.robot` and the panic tests in `m3_usb`/`m4_rmk`.
- Every image flashed to the board has relied on it.
