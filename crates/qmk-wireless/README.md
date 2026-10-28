# qmk-wireless

The UART protocol of the Bluetooth / 2.4G radio module used by QMK's `keyboards/linker/wireless`
boards (the Akko 5075B Plus is one of them). It's that one module's protocol, not a general
wireless stack, and it does no I/O itself (sans-I/O). The firmware feeds in the bytes it receives
and the time, and writes out the bytes `Link` hands back.

## What it does
- **`frame`:** encodes every message the keyboard sends:
  - keyboard, NKRO, consumer, system and mouse reports;
  - device control (select BT 1-5, 2.4G or USB, pairing, battery queries);
  - BT names, dongle strings, VID/PID;
  - raw HID (Vial over the dongle) and the RF test commands.
  
  Frames are `cmd, payload, 8-bit sum`.
- **`rx`:** a resyncing parser for what the module sends: ACKs (`61 0D 0A`), host LEDs, device and
  connection state, battery level, firmware version, host suspend/resume, raw HID.
- **`link`:**
  - a send queue that waits for each ACK, retries a bounded number of times, then drops and counts;
  - keeps the latest keyboard report so releases aren't lost while disconnected;
  - rate-limits reconnect attempts;
  - keeps statistics.
- Board strings (BT names, dongle strings, VID/PID) come in through `DeviceStrings`. Nothing
  board-specific is hardcoded.

It ports QMK's `module.c`, `smsg.c` and `wireless.c`, with the fixes from our QMK branch (see the
crate docs for the list).

## Tested how
`cargo test -p qmk-wireless --target x86_64-unknown-linux-gnu` checks the encoding byte for byte
against frames dumped from the C (`tests/wire_expected.txt`, `tests/wire_board.txt`). The firmware's
2.4G and BT links run on it on the board.
