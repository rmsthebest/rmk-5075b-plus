#!/usr/bin/env python3
"""Writes the labels the disassembly tools put on addresses, into
$ROM_WORK/out/ (rom/README.md):

  out/labels.csv   address,name   every register of every wb32-pac instance
                   (from crates/wb32-pac/src/lib.rs), the Cortex-M3 system
                   registers, and the addresses we know from our own firmware
  out/vectors.csv  index,name     the vector table: the 16 core entries, then
                   the IRQs from target/rm-regs.json

Usage: python3 -I rom/tools/gen_labels.py
"""

import json
import os
import re
from pathlib import Path

HERE = Path(__file__).resolve().parent.parent
# The dump, the Ghidra project and everything generated from the dump live
# outside the repo (rom/README.md): $ROM_WORK, by default ../wb32-rom-re
# beside the repo.
WORK = Path(os.environ.get("ROM_WORK") or HERE.parent.parent / "wb32-rom-re")
REPO = HERE.parent

# Cortex-M3 system control space (ARMv7-M ARM B3.2) and what we know of the
# memory map from the firmware and the reference manual.
EXTRA = [
    (0xE000E004, "SCS_ICTR"), (0xE000E008, "SCS_ACTLR"),
    (0xE000E010, "SYST_CSR"), (0xE000E014, "SYST_RVR"), (0xE000E018, "SYST_CVR"), (0xE000E01C, "SYST_CALIB"),
    *[(0xE000E100 + 4 * i, f"NVIC_ISER{i}") for i in range(8)],
    *[(0xE000E180 + 4 * i, f"NVIC_ICER{i}") for i in range(8)],
    *[(0xE000E200 + 4 * i, f"NVIC_ISPR{i}") for i in range(8)],
    *[(0xE000E280 + 4 * i, f"NVIC_ICPR{i}") for i in range(8)],
    *[(0xE000E300 + 4 * i, f"NVIC_IABR{i}") for i in range(8)],
    *[(0xE000E400 + 4 * i, f"NVIC_IPR{i}") for i in range(60)],
    (0xE000ED00, "SCB_CPUID"), (0xE000ED04, "SCB_ICSR"), (0xE000ED08, "SCB_VTOR"), (0xE000ED0C, "SCB_AIRCR"),
    (0xE000ED10, "SCB_SCR"), (0xE000ED14, "SCB_CCR"), (0xE000ED18, "SCB_SHPR1"), (0xE000ED1C, "SCB_SHPR2"),
    (0xE000ED20, "SCB_SHPR3"), (0xE000ED24, "SCB_SHCSR"), (0xE000ED28, "SCB_CFSR"), (0xE000ED2C, "SCB_HFSR"),
    (0xE000ED34, "SCB_MMFAR"), (0xE000ED38, "SCB_BFAR"), (0xE000EF00, "NVIC_STIR"),
    (0x08000000, "FLASH_APP"), (0x20000000, "SRAM"), (0x20006FFC, "BOOT_MAGIC_QMK"),
    (0x1FFFE000, "ROM"), (0x1FFFF000, "INFO_BLOCK0"), (0x1FFFF100, "INFO_BLOCK1"), (0x1FFFF200, "INFO_BLOCK2"),
    (0x1FFFF300, "INFO_BLOCK3"), (0x1FFFF400, "INFO_BOOT_CONTROL"), (0x1FFFF500, "INFO_WRITE_PROTECT"),
    (0x1FFFF600, "INFO_SECONDARY_DEV"), (0x1FFFF700, "INFO_REBOOT_CFG"), (0x1FFFF720, "INFO_OTP"),
    (0x1FFFF7E8, "UNIQUE_ID"),
]
CORE_VECTORS = ["initial_sp", "Reset", "NMI", "HardFault", "MemManage", "BusFault", "UsageFault", "reserved7",
                "reserved8", "reserved9", "reserved10", "SVCall", "DebugMonitor", "reserved13", "PendSV", "SysTick"]


def pac_labels(lib):
    """(address, name) for every register of every instance in the PAC."""
    text = lib.read_text()
    # Instances: `pub const RCC: rcc::Rcc = ... from_ptr(0x40010c00usize ...`.
    instances = {}
    for name, typ, base in re.findall(r"^pub const (\w+): (\w+::\w+) = unsafe \{ \w+::\w+::from_ptr\((0x[0-9a-f]+)usize",
                                      text, re.M):
        instances.setdefault(typ, []).append((name, int(base, 16)))
    # Register blocks: `pub mod rcc {` ... `    impl Rcc {` with `#[doc = "REG @ 0x.."]`
    # and `wrapping_add(0x8usize + n * 4usize)` (arrays assert `n < K`).
    regs = {}
    mod = typ = None
    lines = text.splitlines()
    for i, line in enumerate(lines):
        if m := re.match(r"pub mod (\w+) \{", line):
            mod, typ = m.group(1), None
        elif m := re.match(r"    impl (\w+) \{", line):
            typ = f"{mod}::{m.group(1)}"
        elif line.startswith("    }"):
            typ = None
        elif typ and (m := re.match(r'        #\[doc = "(\w+) @ 0x[0-9a-fA-F]+"\]', line)):
            body = " ".join(lines[i + 1:i + 6])
            add = re.search(r"wrapping_add\((0x[0-9a-f]+)usize(?: \+ n \* (\d+)usize)?\)", body)
            if not add:
                continue
            off, stride = int(add.group(1), 16), add.group(2)
            if stride:
                count = int(re.search(r"assert!\(n < (\d+)usize\)", body).group(1))
                regs.setdefault(typ, []).extend((f"{m.group(1)}{n}", off + n * int(stride)) for n in range(count))
            else:
                regs.setdefault(typ, []).append((m.group(1), off))
    out = []
    for typ, insts in instances.items():
        for name, base in insts:
            # The register at offset 0 names the base; the peripheral's name
            # alone only when it has none there.
            named = [(base + off, f"{name}_{reg}") for reg, off in regs.get(typ, [])]
            out.extend(named if any(a == base for a, _ in named) else [(base, name)] + named)
    return out


def main():
    (WORK / "out").mkdir(parents=True, exist_ok=True)
    labels = {}
    for addr, name in pac_labels(REPO / "crates/wb32-pac/src/lib.rs") + EXTRA:
        labels.setdefault(addr, name)
    rows = sorted(labels.items())
    (WORK / "out/labels.csv").write_text("".join(f"{a:#010x},{n}\n" for a, n in rows))
    vectors = list(enumerate(CORE_VECTORS))
    rm = REPO / "target/rm-regs.json"
    if rm.exists():
        vectors += [(16 + n, f"IRQ_{name}") for n, name in json.loads(rm.read_text())["vectors"]]
    (WORK / "out/vectors.csv").write_text("".join(f"{i},{n}\n" for i, n in vectors))
    print(f"{WORK}/out/labels.csv: {len(rows)} labels; vectors.csv: {len(vectors)} vectors")


if __name__ == "__main__":
    main()
