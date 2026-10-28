#!/usr/bin/env python3
"""Checks wb32-pac against the reference manual: what tools/gen_pac.py
generates from the CMSIS header (with tools/pac-overrides.toml) against
what tools/rm_regs.py read out of the manual.

    tools/pac_vs_rm.py <wb32fq95xx.h> [rm-regs.json] [OUT.txt]

Registers are matched by block and offset (the two sources name some of them
differently, e.g. the manual's EXTI_INTEN is the header's IMR). Per register
it reports what's in one and not the other, fields that sit at different
bits, PAC fields over bits the manual has reserved, and access (a PAC
register writable where the manual has every field read-only, and the
reverse). Then the interrupt vectors. Blocks the firmware uses come first.
The default OUT is target/pac-vs-rm.txt.
"""

import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT / "tools"))
import gen_pac  # noqa: E402

# The manual's block names for each PAC type (the PAC's SPI is the manual's
# QSPI/SPIM master and its SPI slave in one).
RM_BLOCKS = {"SPI": ["QSPI", "SPI"], "DBGMCU": ["DBGMCU", "DBG"]}
USED = ["RCC", "PWR", "ANCTL", "GPIO", "AFIO", "EXTI", "UART", "SPI", "TIM", "CACHE", "SFM", "SYS", "DBGMCU"]


def pac_regs(blocks, ty):
    """offset -> (name, access, size), arrays expanded."""
    out = {}
    for r in gen_pac.all_regs(blocks[ty]):
        for i in range(max(r.count, 1)):
            name = r.name if r.count <= 1 else f"{r.name}[{i}]"
            out[r.offset + i * r.size] = (name, r.access, r.size, r.name)
    return out


def pac_fields(fields, reg):
    """[(name, lo, width)] with indexed fields expanded."""
    out = []
    for name, (lo, width, arr) in fields.get(reg, {}).items():
        if arr:
            n, stride = arr
            out += [(f"{name}{i}", lo + i * stride, width) for i in range(n)]
        else:
            out.append((name, lo, width))
    return out


def rm_access(fields):
    acc = {f["access"] for f in fields if f["name"]}
    if not acc:
        return None
    if acc <= {"R", "RO"}:
        return "R"
    if acc <= {"W", "WO"}:
        return "W"
    return "RW"


def bits(lo, width):
    return set(range(lo, lo + width))


def group(f):
    """A manual field for a run of per-pin or per-line bits ('OTy', 'MRx',
    'CFGMSKy'), which the PAC has as an indexed field."""
    n = f["name"].rstrip("[]0-9:")
    return f["width"] > 1 and n[-1:] in ("x", "y")


def check_reg(rm, pname, pacc, pf, out):
    notes = []
    named = [f for f in rm["fields"] if f["name"]]
    reserved = set()
    for f in rm["fields"]:
        if not f["name"]:
            reserved |= bits(f["lo"], f["width"])
    acc = rm_access(rm["fields"])
    if acc == "R" and pacc in ("RW", "W"):
        notes.append(f"access: PAC {pacc}, manual read-only")
    if acc in ("RW", "W") and pacc == "R":
        notes.append(f"access: PAC R, manual {acc}")
    if pf:
        for name, lo, width in pf:
            b = bits(lo, width)
            same = [f for f in named if f["lo"] == lo and f["width"] == width]
            if same:
                if not any(f["name"].lower().rstrip("xy") in name.lower() or name.lower() in f["name"].lower()
                           for f in same):
                    notes.append(f"name: PAC {name}[{lo}+{width}] is the manual's {same[0]['name']}")
                continue
            over = [f for f in named if bits(f["lo"], f["width"]) & b]
            if over and all(group(f) and b <= bits(f["lo"], f["width"]) for f in over):
                continue
            if over:
                o = ", ".join(f"{f['name']}[{f['lo']}+{f['width']}]" for f in over)
                notes.append(f"bits: PAC {name}[{lo}+{width}], manual {o}")
            elif b & reserved:
                notes.append(f"reserved: PAC {name}[{lo}+{width}] over bits the manual reserves")
            elif named:
                notes.append(f"extra: PAC {name}[{lo}+{width}] not in the manual's table")
        for f in named:
            b = bits(f["lo"], f["width"])
            if not any(bits(lo, w) & b for _, lo, w in pf):
                notes.append(f"missing: manual {f['name']}[{f['lo']}+{f['width']}] {f['access']} not in the PAC")
    elif named:
        notes.append(f"no PAC fields (raw); manual has {len(named)}")
    if notes:
        out.append(f"  0x{rm['offset']:03X} {pname:<14} (manual {rm['block']}_{rm['reg']}, §{rm['section']})")
        out += [f"      {n}" for n in notes]
    return len(notes)


def main():
    if len(sys.argv) < 2:
        sys.exit(__doc__.strip())
    header = sys.argv[1]
    rmj = Path(sys.argv[2]) if len(sys.argv) > 2 else ROOT / "target" / "rm-regs.json"
    outp = Path(sys.argv[3]) if len(sys.argv) > 3 else ROOT / "target" / "pac-vs-rm.txt"
    rm = json.loads(rmj.read_text())
    blocks, instances, irqs, fields_by_ty, _ = gen_pac.build_model(header)

    by_block = {}
    for r in rm["regs"]:
        by_block.setdefault(r["block"], []).append(r)

    out, total = [], 0
    order = USED + sorted(t for t in fields_by_ty if t not in USED)
    for ty in order:
        if ty not in fields_by_ty:
            continue
        names = RM_BLOCKS.get(ty, [ty])
        rms = [r for n in names for r in by_block.get(n, [])]
        pac = pac_regs(blocks, ty)
        out.append(f"== {ty}{' (used)' if ty in USED else ''}: {len(pac)} PAC registers, "
                   f"{len(rms)} in the manual ({', '.join(names)})")
        if not rms:
            out.append("  not in the parsed manual: check by eye if used")
            continue
        matched = set()
        for r in sorted(rms, key=lambda r: (r["block"], r["offset"])):
            p = pac.get(r["offset"])
            if not p:
                out.append(f"  0x{r['offset']:03X} missing in PAC: manual {r['block']}_{r['reg']} (§{r['section']})")
                total += 1
                continue
            matched.add(r["offset"])
            pname, pacc, _, base = p
            if base.upper() != r["reg"] and pname.upper() != r["reg"]:
                pass  # names differ; reported only with the fields
            total += check_reg(r, pname, pacc, pac_fields(fields_by_ty[ty], base), out)
        for off, (pname, *_rest) in sorted(pac.items()):
            if off not in matched:
                out.append(f"  0x{off:03X} {pname}: in the PAC, not in the manual")
                total += 1

    out.append("== interrupt vectors")
    pac_irq = {v: n for n, v in irqs}
    rm_irq = dict((p, n) for p, n in rm["vectors"])
    for pos in sorted(set(pac_irq) | set(rm_irq)):
        a, b = pac_irq.get(pos), rm_irq.get(pos)
        if (a or "").upper() != (b or "").upper():
            out.append(f"  {pos:2}: PAC {a}, manual {b}")
            total += 1
    out.append("== manual sections not parsed (no register name or offset)")
    out += [f"  §{s['section']} {s['title']} (line {s['line']})" for s in rm["skipped"]]

    outp.parent.mkdir(parents=True, exist_ok=True)
    outp.write_text("\n".join(out) + "\n")
    print(f"pac_vs_rm: {total} findings -> {outp}", file=sys.stderr)


if __name__ == "__main__":
    main()
