#!/usr/bin/env python3
"""The sim suites a change can affect, for `SIM_CHANGED=1 sim/run.sh`.

    affected.py [REV]

Looks at the files changed since REV (default HEAD: the uncommitted work,
untracked files included) and prints one line per suite to run:
"<suite path>\\t<test name regex, empty for all>". Prints nothing when no
change reaches the sim (host tools, docs).

Each file maps to suites by the first rule in RULES that matches it; a
file no rule knows runs everything. A change to the akko-rmk image also
runs SMOKE from m4_rmk: boot, a key, the DFU paths and the stack headroom.
The full suite still runs before an image is frozen for flashing
(tools/bench.py).
"""

import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
TESTS = ROOT / "sim/tests"
ALL = "*"
# m4_rmk tests that any change to akko-rmk runs.
SMOKE = ("Enumerates As The QMK Keyboard|A Key Press Sends A Keyboard Report|Enters DFU"
         "|No Panic|A Panic Resets|The Stack Has Headroom")
RMK = ["m4_rmk", "m5_lighting", "m6_wireless", "m7_power"]

# (path regex, suites). First match wins.
RULES = [
    (r"sim/tests/(\w+)\.robot$", None),  # the suite itself
    (r"sim/tests/leds\.py$", ["m5_lighting"]),
    (r"sim/tests/powercount\.py$", ["m7_power"]),
    (r"sim/(affected\.py|shard\.py|run\.sh)$", []),  # the runner
    (r"rom/", []),  # the ROM study: its own suites (rom/sim/run.sh)
    (r"(tools/(?!precheck)|crates/akko-lighting/examples/|.*\.md$|.*README)", []),  # host-only
    (r"crates/akko-lighting/", ["m5_lighting", "m7_power"]),
    (r"crates/qmk-wireless/", ["m6_wireless"]),
    (r"firmware/src/lighting\.rs$", ["m5_lighting", "m7_power"]),
    (r"firmware/src/radio\.rs$", ["m6_wireless", "m7_power"]),
    (r"firmware/src/bin/m1", ["boot_guard"]),
    (r"firmware/src/bin/m2", ["m2_hal"]),
    (r"firmware/src/bin/m3", ["m3_usb"]),
    (r"firmware/(src/|rmk\.toml|keyboard\.toml)", RMK),
    (r"third_party/rmk", RMK),
    (r"third_party/musb", ["m3_usb", "m4_rmk"]),
    (r"crates/wb32-hal/src/ws2812", ["m5_lighting"]),
    (r"crates/wb32-hal/src/(uart|stop)", ["m6_wireless"]),
    (r"crates/wb32-hal/src/usb", ["m3_usb", "m4_rmk"]),
    (r"crates/wb32-hal/src/(spi_nor|qspi)", ["m4_rmk", "m5_lighting"]),
]


def changed(rev):
    def git(*args):
        return subprocess.run(["git", *args], cwd=ROOT, capture_output=True, text=True, check=True).stdout.split()
    return sorted(set(git("diff", "--name-only", rev)) | set(git("ls-files", "--others", "--exclude-standard")))


def suites_for(path):
    for pattern, suites in RULES:
        m = re.match(pattern, path)
        if m:
            return [m.group(1)] if suites is None else suites
    return [ALL]


def main():
    rev = sys.argv[1] if len(sys.argv) > 1 else "HEAD"
    want = set()
    for path in changed(rev):
        s = suites_for(path)
        want |= set(s)
    if ALL in want:
        want = {p.stem for p in TESTS.glob("*.robot")}
    for stem in sorted(want):
        print(f"{TESTS / (stem + '.robot')}\t")
    if want & set(RMK) and "m4_rmk" not in want:
        print(f"{TESTS / 'm4_rmk.robot'}\t{SMOKE}")


if __name__ == "__main__":
    main()
