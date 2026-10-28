# WB32FQ95 flash controller (FMC) at 0x4001_7800, +0x200, as the ROM drives
# it (notes.md "Flash erase and program"). Renode Python peripheral
# (IronPython 2.7).
#   0x00 CON   an op is <op bits 4:0, written with 0x7F5F0D40> then, after
#              both keys, CON = 0x800080 (start); bit 7 reads as busy, which
#              this model never is. 0x5F0000 clears the op and locks again.
#   0x08 STAT  bit 2: the last op failed (locked, or an address no op may
#              touch)
#   0x0C KEY   0x5188DA08 then 0x12586590 unlocks one op
#   0x10 ADDR  the page (256 bytes; low bits ignored)
#   0x100-0x1FF the page latch the ROM fills before a program (0x40017900)
# Ops: 0x04 clear the latch, 0x08 erase the page, 0x0C program the latch into
# it, 0x1B erase the whole flash. Pages: main flash 0x0800_0000-0x0803_FFFF,
# and information blocks 4-7 (0x1FFF_F400-0x1FFF_F7FF), the only ones the
# ROM's address check allows. Erased bytes are 0x00: this chip's blank flash
# reads 0x00, not 0xFF.
# Log lines: "FMC op 0x<op> at 0x<addr>" (Info), "FMC ERROR ..." (Warning).

if request.IsInit:
    from Antmicro.Renode.Core import EmulationManager

    bus = list(EmulationManager.Instance.CurrentEmulation.Machines)[0].SystemBus
    FLASH, FLASH_SIZE = 0x08000000, 0x40000
    INFO_LO, INFO_HI = 0x1FFFF400, 0x1FFFF800
    KEY1, KEY2 = 0x5188DA08, 0x12586590
    st = {"con": 0, "stat": 0, "addr": 0, "keys": [], "op": 0}
    latch = [0] * 64

    def page_ok(a):
        return FLASH <= a < FLASH + FLASH_SIZE or INFO_LO <= a < INFO_HI

    def fill(a, words):
        for i, w in enumerate(words):
            bus.WriteDoubleWord(a + 4 * i, w)

    def run(op):
        a = st["addr"] & ~0xFF
        unlocked = st["keys"][-2:] == [KEY1, KEY2]
        st["keys"] = []
        if not unlocked:
            st["stat"] |= 4
            self.WarningLog("FMC ERROR: op 0x%02x without the keys" % op)
            return
        if op == 0x04:
            for i in range(64):
                latch[i] = 0
        elif op == 0x1B:
            fill(FLASH, [0] * (FLASH_SIZE // 4))
        elif op in (0x08, 0x0C) and page_ok(a):
            fill(a, [0] * 64 if op == 0x08 else latch)
        else:
            st["stat"] |= 4
            self.WarningLog("FMC ERROR: op 0x%02x at 0x%08x" % (op, a))
            return
        st["stat"] &= ~4
        self.InfoLog("FMC op 0x%02x at 0x%08x" % (op, a))

if not request.IsInit:
    off = request.Offset
    if request.IsRead:
        if off == 0x00:
            request.Value = st["con"] & ~0x80
        elif off == 0x08:
            request.Value = st["stat"]
        elif off == 0x10:
            request.Value = st["addr"]
        elif 0x100 <= off < 0x200:
            request.Value = latch[(off - 0x100) // 4]
        else:
            request.Value = 0
    else:
        v = request.Value & 0xFFFFFFFF
        if off == 0x00:
            if v & 0x800000 and v & 0x80:
                run(st["op"])
            elif v == 0x5F0000:
                st["op"] = 0
            else:
                st["op"] = v & 0x1F
            st["con"] = v
        elif off == 0x0C:
            st["keys"].append(v)
        elif off == 0x10:
            st["addr"] = v
        elif 0x100 <= off < 0x200:
            latch[(off - 0x100) // 4] = v
        else:
            self.WarningLog("FMC write to +0x%02x = 0x%08x not modelled" % (off, v))
