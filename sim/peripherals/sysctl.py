# WB32FQ95 system control block, 0x4001_0000..0x4001_0FFF:
#   PWR 0x000, ANCTL 0x400, IWDG 0x800, RCC 0xC00 (offsets from wb32fq95xx.h).
# Renode Python peripheral (IronPython 2.7). Registers remember what was
# written; the status bits hardware would set are derived from the enables:
#   ANCTL MHSISR/FHSISR/LSISR/HSESR bit 0, and PLLSR = 3 once PLLON is set.
# ANCTL is only writable while PWR ANAKEY1/2 hold 0x03/0x0C (as ChibiOS's
# hal_lld.c brackets every ANCTL access). Locked writes are dropped and logged
# as "LOCKED", which tests treat as a firmware bug.
#
# Log lines: "MMIO <R|W> <BLOCK>+0x<off> = 0x<val>" (Info; status polls Noisy).

if request.IsInit:
    regs = {}
    BLOCKS = ("PWR", "ANCTL", "IWDG", "RCC")
    PWR, ANCTL, IWDG, RCC = 0x000, 0x400, 0x800, 0xC00
    ANAKEY1, ANAKEY2 = PWR + 0x28, PWR + 0x2C

    def reg(off):
        return regs.get(off, 0)

    def anctl_unlocked():
        return reg(ANAKEY1) == 0x03 and reg(ANAKEY2) == 0x0C

    def status(off):
        # (status register, enable register, enable bit) -> ready flag in bit 0
        ready = {
            ANCTL + 0x30: (reg(ANCTL + 0x2C) & 1) or 1,  # MHSI: on from reset
            ANCTL + 0x3C: reg(ANCTL + 0x38) & 1,           # FHSI
            ANCTL + 0x48: reg(ANCTL + 0x44) & 1,           # LSI
            ANCTL + 0x58: 1 if (reg(ANCTL + 0x4C) & 3) else 0,  # HSE: HSEON or BYPASS
            ANCTL + 0x7C: 3 if (reg(ANCTL + 0x78) & 1) else 0,  # PLL
        }
        return ready.get(off)

    def name(off):
        return "%s+0x%03x" % (BLOCKS[off // 0x400], off % 0x400)

if not request.IsInit:
    off = request.Offset
    if request.IsRead:
        s = status(off)
        v = reg(off) if s is None else s
        request.Value = v
        if s is None:
            self.InfoLog("MMIO R %s = 0x%08x" % (name(off), v))
        else:
            self.NoisyLog("MMIO R %s = 0x%08x" % (name(off), v))
    else:
        v = request.Value & 0xFFFFFFFF
        if ANCTL <= off < IWDG and not anctl_unlocked():
            self.WarningLog("LOCKED write dropped: %s = 0x%08x (ANAKEY not set)" % (name(off), v))
        else:
            regs[off] = v
            self.InfoLog("MMIO W %s = 0x%08x" % (name(off), v))
