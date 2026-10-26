# WB32FQ95 GPIO ports A-D (0x4000_0000, 0x400 each), AFIO (0x4000_1400) and
# EXTI (0x4000_1800), plus the Akko 5075B Plus key matrix and other board-level
# inputs. Renode Python peripheral (IronPython 2.7), mapped at 0x4000_0000 +0x2000.
#
# Register layout per port (wb32fq95xx.h GPIO_TypeDef):
#   0x00 MODER  0x04 OTYPER  0x08 OSPEEDR  0x0C PUPDR  0x10 IDR  0x14 ODR
#   0x18 BSRR   0x1C LCKR    0x20 AFRL     0x24 AFRH   0x28 SMIT 0x2C CURRENT
#   0x30 CFGMSK
# WB32 quirk: writes to the configuration registers only change pins whose
# CFGMSK bit is 0 (ChibiOS sets CFGMSK = ~mask before writing MODER etc.).
#
# Simulation control, offsets inside GPIOD's unused space (0x4000_0FF0..):
#   0xFF0 write: key   = (pressed << 16) | (row << 8) | col
#   0xFF4 write: drive = (level << 16) | (port << 8) | pin, level 0 = low,
#                1 = high, 2 = released (back to pull/floating)
#   0xFF8 write: any value releases every key
#
# C12 is the SPI flash chip select: after every write its level and its count
# of rising edges go to the AppDomain ("akko-gpio-c12"), where qspi.py reads
# them to tell where one flash command ends.
#
# EXTI lines 0-15 follow the pin AFIO EXTICR selects. After every write here
# (firmware or test control) each line's level is recomputed; an edge enabled
# in RTSR/FTSR sets PR, and each NVIC line (EXTI0-4, EXTI9_5, EXTI15_10) is
# held high while PR & IMR has a bit in its group. PR clears on writing 1.

if request.IsInit:
    PORTS = 4
    CFG_REGS = (0x00, 0x04, 0x08, 0x0C, 0x20, 0x24, 0x28, 0x2C)
    # 2-bit fields per pin in these registers; others are 1 bit (OTYPER) or 4 bits (AFR*)
    FIELD_BITS = {0x00: 2, 0x04: 1, 0x08: 2, 0x0C: 2, 0x20: 4, 0x24: 4, 0x28: 1, 0x2C: 2}

    regs = [dict((off, 0) for off in (0x00, 0x04, 0x08, 0x0C, 0x14, 0x1C, 0x20, 0x24, 0x28, 0x2C, 0x30)) for _ in range(PORTS)]

    A, B, C, D = 0, 1, 2, 3
    ROWS = [(A, 0), (A, 1), (A, 2), (A, 3), (A, 4), (C, 13)]
    COLS = [(C, 0), (C, 1), (C, 2), (C, 3), (A, 6), (B, 10), (B, 11), (B, 12), (B, 13), (B, 14), (A, 10), (C, 6), (C, 7), (C, 8), (C, 9)]
    pressed = set()   # (row, col)
    driven = {}       # (port, pin) -> 0/1, an external level overriding pulls

    def field_mask(off, pin):
        bits = FIELD_BITS[off]
        if off == 0x24:
            pin -= 8
        if off in (0x20, 0x24) and not (0 <= pin < 8):
            return 0
        return ((1 << bits) - 1) << (pin * bits)

    def mode(port, pin):
        return (regs[port][0x00] >> (pin * 2)) & 3          # 0 in, 1 out, 2 af, 3 analog

    def pull(port, pin):
        return (regs[port][0x0C] >> (pin * 2)) & 3          # 0 none, 1 up, 2 down

    def out_level(port, pin):
        return (regs[port][0x14] >> pin) & 1

    def drives_low(port, pin):
        return mode(port, pin) == 1 and out_level(port, pin) == 0

    def pin_level(port, pin):
        if (port, pin) in driven:
            return driven[(port, pin)]
        if mode(port, pin) == 1:
            return out_level(port, pin)
        # ROW2COL: a held key pulls its row low when its column is driven low.
        if (port, pin) in ROWS:
            row = ROWS.index((port, pin))
            for (r, c) in pressed:
                if r == row and drives_low(*COLS[c]):
                    return 0
        p = pull(port, pin)
        if p == 1:
            return 1
        return 0  # pull-down or floating: read as low

    AFIO, EXTI = 0x1400, 0x1800
    exticr = [0, 0, 0, 0]                 # AFIO+0x08..0x14
    exti = dict((off, 0) for off in (0x00, 0x04, 0x08, 0x0C, 0x10, 0x14))
    IMR, RTSR, FTSR, SWIER, PR = 0x00, 0x08, 0x0C, 0x10, 0x14
    EXTI_IRQS = [(6, 1 << 0), (7, 1 << 1), (8, 1 << 2), (9, 1 << 3), (10, 1 << 4),
                 (16, 0x03E0), (33, 0xFC00)]
    irq_level = {}
    line_last = [None] * 16

    from Antmicro.Renode.Core import EmulationManager
    nvic = None
    for m in EmulationManager.Instance.CurrentEmulation.Machines:
        nvic = m.SystemBus.WhatPeripheralIsAt(0xE000E000)

    def line_level(line):
        port = (exticr[line // 4] >> ((line % 4) * 4)) & 0xF
        return pin_level(port, line) if port < PORTS else 0

    def exti_update():
        for line in range(16):
            lv = line_level(line)
            old = line_last[line]
            line_last[line] = lv
            if old is None or lv == old:
                continue
            bit = 1 << line
            if (lv and exti[RTSR] & bit) or (not lv and exti[FTSR] & bit):
                exti[PR] |= bit
        active = exti[PR] & exti[IMR]
        for irq, mask in EXTI_IRQS:
            level = (active & mask) != 0
            if irq_level.get(irq, False) != level:
                irq_level[irq] = level
                nvic.OnGPIO(irq, level)

    from System import AppDomain, Tuple
    CS_KEY = "akko-gpio-c12"
    cs_state = [1, 0]     # level, rising edges

    def cs_update():
        lv = pin_level(C, 12)
        if lv and not cs_state[0]:
            cs_state[1] += 1
        cs_state[0] = lv
        AppDomain.CurrentDomain.SetData(CS_KEY, Tuple.Create(lv, cs_state[1]))

    def idr(port):
        v = 0
        for pin in range(16):
            v |= pin_level(port, pin) << pin
        return v

if request.IsInit:
    pass
elif request.Offset >= EXTI:
    off = request.Offset - EXTI
    if request.IsRead:
        request.Value = exti.get(off, 0)
    else:
        v = request.Value & 0xFFFFFFFF
        self.InfoLog("MMIO W EXTI+0x%03x = 0x%08x" % (off, v))
        if off == PR:
            exti[PR] &= ~v
        elif off == SWIER:
            exti[PR] |= v & ~exti[SWIER]
            exti[SWIER] = v
        elif off in exti:
            exti[off] = v
elif request.Offset >= AFIO:
    off = request.Offset - AFIO
    idx = (off - 0x08) // 4
    if request.IsRead:
        request.Value = exticr[idx] if 0 <= idx < 4 else 0
    else:
        v = request.Value & 0xFFFF
        self.InfoLog("MMIO W AFIO+0x%03x = 0x%08x" % (off, v))
        if 0 <= idx < 4:
            exticr[idx] = v
elif request.Offset >= 0x1000:
    if request.IsRead:
        request.Value = 0
elif request.Offset >= 0xFF0:
    if request.IsWrite:
        v = request.Value
        if request.Offset == 0xFF0:
            key = ((v >> 8) & 0xFF, v & 0xFF)
            if (v >> 16) & 1:
                pressed.add(key)
            else:
                pressed.discard(key)
        elif request.Offset == 0xFF4:
            pin_id = ((v >> 8) & 0xFF, v & 0xFF)
            level = (v >> 16) & 0xFF
            if level == 2:
                driven.pop(pin_id, None)
            else:
                driven[pin_id] = level
        elif request.Offset == 0xFF8:
            pressed.clear()
    else:
        request.Value = 0
else:
    port = request.Offset // 0x400
    off = request.Offset % 0x400
    r = regs[port] if port < PORTS else None
    if request.IsWrite:
        self.InfoLog("MMIO W GPIO%s+0x%03x = 0x%08x" % ("ABCD"[port], off, request.Value & 0xFFFFFFFF))
    if r is None or off > 0x30:
        if request.IsRead:
            request.Value = 0
    elif request.IsRead:
        if off == 0x10:
            request.Value = idr(port)
            self.NoisyLog("MMIO R GPIO%s+0x010 = 0x%08x" % ("ABCD"[port], request.Value))
        elif off == 0x18:
            request.Value = 0
        else:
            request.Value = r[off]
    else:
        v = request.Value & 0xFFFFFFFF
        if off in CFG_REGS:
            m = 0
            for pin in range(16):
                if not (r[0x30] >> pin) & 1:
                    m |= field_mask(off, pin)
            r[off] = (r[off] & ~m) | (v & m)
        elif off == 0x18:  # BSRR: low half sets, high half resets (set wins)
            r[0x14] = (r[0x14] & ~((v >> 16) & 0xFFFF)) | (v & 0xFFFF)
        elif off == 0x10:
            pass
        else:
            r[off] = v

if not request.IsInit and request.IsWrite:
    exti_update()
    cs_update()
