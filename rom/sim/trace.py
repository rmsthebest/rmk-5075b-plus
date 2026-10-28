#!/usr/bin/env python3
"""Runs a scenario on the ROM (or a rebuilt image, step 4) in Renode and
writes its peripheral write trace: one line per write, in order,
"<peripheral>+0x<off> = 0x<value>", with no PCs or timestamps, so two
images can be diffed. Reads are left out (polling loops make their count
timing-dependent).

  boot      reset into the image, run 1 s (the host enumerates it)
  download  boot, then ERASE 0x08000000, WRITE_PAGE (a 16-byte app), READ
  leave     download, then GO 0x08000000

Usage: python3 -I rom/sim/trace.py SCENARIO [--image FILE] [--out FILE]
  --image  $ROM_WORK/dump/rom.bin (default) or an ELF/bin linked at 0x1FFFE000;
           its SP and PC are taken from its vector table, as boot-guard's
           jump does
  --out    default rom/sim/traces/<scenario>.txt (for the ROM), or
           rom/sim/traces/<scenario>-<image name>.txt
"""

import argparse
import os
import re
import subprocess
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
RENODE = HERE.parents[2] / "tools/renode/renode"
# $ROM_WORK, by default ../wb32-rom-re beside the repo (rom/README.md).
DUMP = Path(os.environ.get("ROM_WORK") or HERE.parents[2] / "wb32-rom-re") / "dump"
ROM = DUMP / "rom.bin"
APP = [0x00, 0x10, 0x00, 0x20, 0x09, 0x00, 0x00, 0x08, 0xFE, 0xE7] + [0] * 6


def le32(v):
    return list(v.to_bytes(4, "little"))


def command(cmd, a=0, b=0, data=()):
    """Monitor commands that send one command on bulk OUT EP1."""
    out = [f"usb WriteDoubleWord 0xF0C {x}" for x in [cmd, 0, 0, 0] + le32(a) + le32(b) + list(data)]
    return out + [f"usb WriteDoubleWord 0xF00 {0x0E | 1 << 8}", 'emulation RunFor "0.05"']


def scenario(name):
    steps = ['emulation RunFor "1.0"']
    if name in ("download", "leave"):
        steps += command(0x5E, 0x08000000, 1)
        steps += command(0x6A, 0x08000000, len(APP), APP)
        steps += command(0x72, 0x08000000, len(APP))
    if name == "leave":
        steps += command(0x9B, 0x08000000, 0) + ['emulation RunFor "0.1"']
    return steps


def vectors(image):
    """(SP, PC) from the image's vector table."""
    if image.suffix == ".bin":
        b = image.read_bytes()
    else:
        with tempfile.NamedTemporaryFile(suffix=".bin") as f:
            subprocess.run(["arm-none-eabi-objcopy", "-O", "binary", "-j", ".vector_table", str(image), f.name],
                           check=True)
            b = Path(f.name).read_bytes()
    return int.from_bytes(b[0:4], "little"), int.from_bytes(b[4:8], "little")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("scenario", choices=["boot", "download", "leave"])
    ap.add_argument("--image", type=Path, default=ROM)
    ap.add_argument("--out", type=Path)
    args = ap.parse_args()
    image = args.image.resolve()
    sp, pc = vectors(image)
    load = [] if image == ROM.resolve() else (
        [f"sysbus LoadBinary @{image} 0x1FFFE000"] if image.suffix == ".bin" else [f"sysbus LoadELF @{image}"])
    cmds = [f"$dump=@{DUMP}", f"include @{HERE}/rom.resc", "logLevel 0 gpio", "sysbus LogAllPeripheralsAccess true", *load,
            "machine Reset", f"cpu SP {sp:#x}", f"cpu PC {pc:#x}", *scenario(args.scenario), "quit"]
    out = args.out or HERE / "traces" / (f"{args.scenario}.txt" if image == ROM.resolve()
                                         else f"{args.scenario}-{image.stem}.txt")
    out.parent.mkdir(exist_ok=True)
    pat = re.compile(r"\] (\w+): \[cpu: 0x[0-9A-F]+\] Write\w+ to (0x[0-9A-F]+)[^,]*, value (0x[0-9A-F]+)")
    n = 0
    with open(out, "w") as f:
        p = subprocess.Popen([str(RENODE), "--console", "--disable-gui", "--plain", "-e", "; ".join(cmds)],
                             stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, errors="replace")
        for line in p.stdout:
            if m := pat.search(line):
                f.write(f"{m.group(1)}+{int(m.group(2), 16):#x} = {int(m.group(3), 16):#x}\n")
                n += 1
            elif "HOST ERROR" in line or "FMC ERROR" in line or "LOCKED" in line:
                f.write("# " + line.split("] ", 1)[-1])
        p.wait()
    print(f"{out}: {n} writes")


if __name__ == "__main__":
    sys.exit(main())
