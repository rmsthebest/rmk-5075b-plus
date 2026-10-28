#!/usr/bin/env python3
"""Reads or sets akko-rmk's sleep timeout over Vial channel 0xD7
(src/power.rs): how long the keyboard waits, with no key or knob input, before
it sleeps on Bluetooth or 2.4G (it never sleeps on USB).

  never              never sleep when idle (it still sleeps when the
                     computer does, or the battery is flat)
  1 3 5 10 20 30     minutes
  default            2 minutes, the vendor's default
  (none)             print the current setting

The keyboard saves the setting with its lighting record, so it survives a
replug.

  stop on|off        stop mode while asleep (channel 0xD9, M7c): the chip
                     stops its clocks; only on the battery (cable out).
                     Off at every reset, and not saved: turn it on, then
                     unplug the cable (the keyboard runs on).
  stop diag [N]      stop mode on, with the Caps and Win-lock LEDs showing
                     how far the last stop got: Caps alone, it never
                     woke; both, it woke but its clocks didn't come back;
                     Win-lock alone, it got out of the stop. N (0-63, 0 by
                     default) adds variants, summed: 1 rows and UART wake
                     on both edges, 2 pending interrupts cleared and it
                     stops anyway, 4 the knob doesn't wake it, 8 the lines
                     are EXTI events too, 16 100x the delay after waking,
                     32 the PLL turned off before the clocks restart
  stop               print whether it's on, and how many stops so far
                     (and how many a key, the knob or the cable ended)

Usage: python3 -I tools/sleep.py [never|1|3|5|10|20|30|default]
       python3 -I tools/sleep.py stop [on|off|diag [N]]
"""

import os
import select
import sys
import time
from pathlib import Path

CHANNEL = 0xD7
STOP_CHANNEL = 0xD9
STOP_ARG = {None: 0, "off": 1, "on": ord("S"), "diag": ord("D")}
# The firmware's index for each setting (lighting/src/record.rs SLEEP_*).
INDEX = {"never": 0, "1": 1, "3": 2, "5": 3, "10": 4, "20": 5, "30": 6, "default": 7}
NAMES = {0: "never", 1: "1 min", 2: "3 min", 3: "5 min", 4: "10 min", 5: "20 min", 6: "30 min",
         7: "2 min (default)"}


def rmk_raw_hid():
    """/dev path of akko-rmk's Vial raw HID node, or None (as tools/bench.py)."""
    for h in sorted(Path("/sys/class/hidraw").iterdir()):
        dev = h.resolve() / "device"
        uevent = (dev / "uevent").read_text()
        # The 2.4G dongle has the same ids (and a Vial node): only "(RMK)".
        if "0003:0000342D:0000E4F6" not in uevent.upper() or "(RMK)" not in uevent:
            continue
        if (dev / "report_descriptor").read_bytes().startswith(b"\x06\x60\xff"):
            return f"/dev/{h.name}"
    return None


def ask(channel, arg, more=0):
    """Sends 08 <channel> <arg> <more> on Vial's raw HID; returns the
    answer."""
    path = rmk_raw_hid()
    if not path:
        sys.exit("akko-rmk isn't connected (over USB)")
    fd = os.open(path, os.O_RDWR | os.O_NONBLOCK)
    try:
        os.write(fd, bytes([0x00, 0x08, channel, arg, more]) + bytes(28))
        deadline = time.monotonic() + 0.5
        while True:
            left = deadline - time.monotonic()
            if left <= 0 or not select.select([fd], [], [], left)[0]:
                sys.exit(f"no answer (an image without channel {channel:#04x}?)")
            data = os.read(fd, 64)
            if data[:2] == bytes([0x08, channel]):
                return data
    finally:
        os.close(fd)


def main():
    args = sys.argv[1:]
    if args[:1] == ["stop"] and (args[1:] or [None])[0] in STOP_ARG:
        variant = 0
        if args[1:2] == ["diag"] and len(args) == 3 and args[2].isdigit() and int(args[2]) < 64:
            variant = int(args[2])
        elif len(args) > 2:
            sys.exit(__doc__.strip())
        data = ask(STOP_CHANNEL, STOP_ARG[(args[1:] or [None])[0]], variant)
        stops, wakes, aborts = (int.from_bytes(data[i:i + 4], "little") for i in (4, 8, 12))
        diag = "" if data[16] == 0xFF else f" (diag, variant {data[16]})"
        print(f"stop mode {'on' if data[3] else 'off'}{diag}; {stops} stops, {wakes} ended by a key, the knob or"
              f" the cable; {aborts} tries put off (an interrupt pending)")
        return
    if len(args) > 1 or (args and args[0] not in INDEX):
        sys.exit(__doc__.strip())
    data = ask(CHANNEL, INDEX[args[0]] + 1 if args else 0)
    print("sleep after", NAMES.get(data[3], f"? ({data[3]})"))


if __name__ == "__main__":
    main()
