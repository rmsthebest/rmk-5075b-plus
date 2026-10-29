#!/usr/bin/env python3
"""Reads akko-rmk's battery level over Vial channel 0xD4 (src/radio.rs
fill_diag: byte 21 is the module's battery %), over USB or through the 2.4G
dongle. Asleep, the keyboard doesn't answer through the dongle; the query
doesn't wake it either, so it's only answered while the keyboard is in use.

  (none)              print the level once
  log FILE [MIN]      every MIN minutes (10 by default), append
                      "<time> <level>" to FILE when the keyboard answers

Usage: python3 -I tools/battery.py [log FILE [MIN]]
"""

import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import sleep  # noqa: E402  (rmk_raw_hid, the Vial request)

CHANNEL = 0xD4


def level():
    """The battery %, or None while the keyboard doesn't answer."""
    try:
        return sleep.ask(CHANNEL, 0)[21]
    except SystemExit:
        return None


def main(args):
    if not args:
        lvl = level()
        if lvl is None:
            sys.exit("no answer: the keyboard is asleep or not connected (press a key, try again)")
        print(f"battery {lvl}%")
    elif args[0] == "log" and len(args) in (2, 3):
        path, every = Path(args[1]), float(args[2]) if len(args) == 3 else 10
        while True:
            lvl = level()
            if lvl is not None:
                line = f"{time.strftime('%Y-%m-%d %H:%M')} {lvl}"
                with path.open("a") as f:
                    f.write(line + "\n")
                print(line, flush=True)
            time.sleep(every * 60)
    else:
        sys.exit(__doc__)


if __name__ == "__main__":
    main(sys.argv[1:])
