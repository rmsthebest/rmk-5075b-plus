# WB32FQ95 QSPI controller (0x4000_3000) in plain SPI mode, with the board's
# external SPI NOR flash on it (chip select C12, a GPIO, active low).
# Renode Python peripheral (IronPython 2.7), mapped at 0x4000_3000 +0x100.
#
# Controller: a DesignWare SSI (wb32fq95xx.h SPI_TypeDef; driven by
# ChibiOS-contrib LLD/SPIv1/hal_spi_lld.c). What's modelled:
#   0x00 CR0, 0x04 CR1, 0x14 BAUDR: only writable while SPIENR = 0, as on
#        the DW SSI. A write that would change one while enabled is logged as
#        an error and ignored (ChibiOS rewrites CR0 unchanged while enabled).
#   0x10 SER, 0x18 TXFTLR, 0x1C RXFTLR: writable at any time. ChibiOS writes
#        the thresholds between two sends of one transaction with SPIENR = 1,
#        and QMK reads its settings fine on the board, so they must take
#        effect then.
#   0x08 SPIENR: 0 flushes both FIFOs.
#   0x20 TXFLR (always 0: a byte written to DR is shifted at once),
#   0x24 RXFLR, 0x28 SR (TFNF, TFE, RFNE, RFF; never BUSY),
#   0x2C IER, 0x30 ISR = RISR & IER, 0x34 RISR (TXE = TX level <= TXFTLR,
#        RXO, RXF = RX level > RXFTLR), 0x38-0x48 the clear registers (reading
#        ICR or RXOICR clears RXO), 0x60 DR.
#   TXFTLR and RXFTLR hold values below FIFO_DEPTH only, so ChibiOS's depth
#   probe finds FIFO_DEPTH. A received byte beyond FIFO_DEPTH is dropped and
#   sets RXO, as on the real block.
#   NVIC line 26 is high while ISR != 0.
# Not modelled: clock timing (a byte takes no time), the frame formats other
# than Motorola SPI with 8-bit frames, DMA, dual/quad mode. FIFO_DEPTH is an
# assumption: the chip's real depth isn't documented in the sources we have.
#
# Flash: a generic 25-series SPI NOR. Its part number isn't known (the case
# stays shut), so the JEDEC ID and size are those of QMK's default part, the
# MX25L4006E (C2 20 13, 512 KB); firmware must check the capacity byte rather
# than assume it. Commands: 9F RDID, 05 RDSR, 06 WREN, 04 WRDI, 03 READ,
# 0B FAST_READ, 02 PP (wraps inside its 256-byte page), 20 SE (4 KB),
# D8 BE (64 KB), 60/C7 CE, B9 DP, AB RDP/RES. PP/SE/BE/CE need WEL (set by
# WREN, cleared when the command completes) and set WIP for BUSY_POLLS RDSR
# reads. Programming ANDs bits in, as real NOR does. A command ends when chip
# select goes high (gpio.py counts its rising edges in the AppDomain).
# Every erase and program is logged ("flash: SE 0x010000" etc.), so a test can
# check which regions an image writes.
# The array is kept in the AppDomain, so it survives a machine reset (as the
# real chip keeps its data across a keyboard reset).
#
# Simulation control (offsets 0xF0..):
#   0xF0 write: address for 0xF4
#   0xF4 read : the flash word at that address (little endian), then the
#               address advances by 4; write: program that word as stored
#               (no erase needed, no log)
#   0xF8 write: any value erases the whole chip to 0xFF (no log)

if request.IsInit:
    from System import AppDomain, Array, Byte
    from Antmicro.Renode.Core import EmulationManager

    FIFO_DEPTH = 8
    SIZE = 512 * 1024
    JEDEC_ID = (0xC2, 0x20, 0x13)
    BUSY_POLLS = 2
    IRQ = 26
    CS_KEY = "akko-gpio-c12"          # (level, rising edge count), set by gpio.py
    MEM_KEY = "akko-spi-flash"

    mem = AppDomain.CurrentDomain.GetData(MEM_KEY)
    if mem is None:
        mem = Array.CreateInstance(Byte, SIZE)
        for i in range(SIZE):
            mem[i] = 0xFF
        AppDomain.CurrentDomain.SetData(MEM_KEY, mem)

    nvic = None
    for m in EmulationManager.Instance.CurrentEmulation.Machines:
        nvic = m.SystemBus.WhatPeripheralIsAt(0xE000E000)

    class State(object):
        pass
    s = State()
    s.regs = {0x00: 0x07, 0x04: 0, 0x08: 0, 0x10: 0, 0x14: 0, 0x18: 0, 0x1C: 0, 0x2C: 0}
    s.rx = []
    s.rxo = False
    s.irq = False
    s.dbg_addr = 0
    # flash
    s.cs_edges = None
    s.cmd = None          # current command byte, None = waiting for one
    s.args = []           # bytes after the command
    s.addr = 0
    s.wel = False
    s.busy = 0
    s.deep = False
    s.pp_count = 0
    s.pp_addr = 0

    LOCKED = (0x00, 0x04, 0x14)

    def cs():
        v = AppDomain.CurrentDomain.GetData(CS_KEY)
        return (1, 0) if v is None else (v.Item1, v.Item2)

    def risr():
        r = 0
        if 0 <= s.regs[0x18]:          # TX level is always 0 <= TXFTLR
            r |= 1 << 0
        if s.rxo:
            r |= 1 << 3
        if len(s.rx) > s.regs[0x1C]:
            r |= 1 << 4
        return r

    def update_irq():
        level = (risr() & s.regs[0x2C]) != 0
        if level != s.irq:
            s.irq = level
            nvic.OnGPIO(IRQ, level)

    def status():
        return (1 if s.busy else 0) | (2 if s.wel else 0)

    NAMES = {0x9F: "RDID", 0x05: "RDSR", 0x06: "WREN", 0x04: "WRDI", 0x03: "READ",
             0x0B: "FAST_READ", 0x02: "PP", 0x20: "SE", 0xD8: "BE", 0x60: "CE",
             0xC7: "CE", 0xB9: "DP", 0xAB: "RDP"}

    def finish():
        """The chip select went high: complete the command in progress."""
        if s.cmd in (0x03, 0x0B) and len(s.args) >= 3:
            n = len(s.args) - 3 - (1 if s.cmd == 0x0B else 0)
            a = (s.args[0] << 16) | (s.args[1] << 8) | s.args[2]
            self.InfoLog("flash: %s 0x%06x n=%d" % (NAMES[s.cmd], a, n))
        elif s.cmd in (0x9F, 0x06, 0x04, 0xB9, 0xAB):
            self.InfoLog("flash: %s" % NAMES[s.cmd])
        if s.cmd == 0x02 and s.pp_count:
            self.InfoLog("flash: PP 0x%06x n=%d" % (s.pp_addr, s.pp_count))
            s.wel = False
            s.busy = BUSY_POLLS
        s.cmd = None
        s.args = []
        s.pp_count = 0

    def erase(start, n, name):
        self.InfoLog("flash: %s 0x%06x" % (name, start))
        for i in range(start, start + n):
            mem[i] = 0xFF
        s.wel = False
        s.busy = BUSY_POLLS

    def need_wel(name):
        if s.busy:
            self.ErrorLog("flash: %s while busy (WIP), ignored" % name)
            return False
        if not s.wel:
            self.ErrorLog("flash: %s without WREN, ignored" % name)
            return False
        return True

    def flash_byte(b):
        """One byte from the controller; returns the byte the flash drives."""
        if s.cmd is None:
            s.cmd = b
            s.args = []
            if s.deep and b != 0xAB:
                return 0xFF
            if b == 0x06:
                if not s.busy:
                    s.wel = True
            elif b == 0x04:
                if not s.busy:
                    s.wel = False
            elif b in (0x60, 0xC7):
                if need_wel("CE"):
                    erase(0, SIZE, "CE")
            elif b == 0xB9:
                s.deep = True
            elif b == 0xAB:
                s.deep = False
            elif b not in (0x9F, 0x05, 0x03, 0x0B, 0x02, 0x20, 0xD8):
                self.WarningLog("flash: unknown command 0x%02x" % b)
            return 0xFF
        c = s.cmd
        s.args.append(b)
        n = len(s.args)
        if c == 0x9F:
            return JEDEC_ID[n - 1] if n <= 3 else 0xFF
        if c == 0x05:
            v = status()
            if s.busy:
                s.busy -= 1
            return v
        if c == 0xAB:
            return JEDEC_ID[2] - 1 if n >= 4 else 0xFF   # electronic ID after 3 dummies
        if c in (0x03, 0x0B, 0x02, 0x20, 0xD8):
            if n <= 3:
                if n == 3:
                    s.addr = ((s.args[0] << 16) | (s.args[1] << 8) | s.args[2]) % SIZE
                    if c == 0x20 and need_wel("SE"):
                        erase(s.addr & ~0xFFF, 0x1000, "SE")
                    elif c == 0xD8 and need_wel("BE"):
                        erase(s.addr & ~0xFFFF, 0x10000, "BE")
                    elif c == 0x02:
                        if need_wel("PP"):
                            s.pp_addr = s.addr
                        else:
                            s.cmd = 0xFE    # ignore the data bytes
                return 0xFF
            if c == 0x0B and n == 4:
                return 0xFF                  # dummy byte
            if c in (0x03, 0x0B):
                if s.busy:
                    self.ErrorLog("flash: READ while busy (WIP)")
                v = mem[s.addr]
                s.addr = (s.addr + 1) % SIZE
                return v
            if c == 0x02:
                page = s.addr & ~0xFF
                mem[s.addr] = mem[s.addr] & b
                s.addr = page | ((s.addr + 1) & 0xFF)
                s.pp_count += 1
                return 0xFF
        return 0xFF

    def shift(b):
        level, edges = cs()
        if edges != s.cs_edges:
            s.cs_edges = edges
            finish()
        if level:
            self.WarningLog("qspi: byte 0x%02x sent with flash CS high" % b)
            return 0xFF
        return flash_byte(b)

if request.IsInit:
    pass
elif request.Offset >= 0xF0:
    o = request.Offset
    if o == 0xF0 and request.IsWrite:
        s.dbg_addr = request.Value % SIZE
    elif o == 0xF4:
        a = s.dbg_addr
        if request.IsRead:
            request.Value = mem[a] | (mem[(a + 1) % SIZE] << 8) | (mem[(a + 2) % SIZE] << 16) | (mem[(a + 3) % SIZE] << 24)
        else:
            for i in range(4):
                mem[(a + i) % SIZE] = (request.Value >> (8 * i)) & 0xFF
        s.dbg_addr = (a + 4) % SIZE
    elif o == 0xF8 and request.IsWrite:
        for i in range(SIZE):
            mem[i] = 0xFF
    elif request.IsRead:
        request.Value = 0
else:
    o = request.Offset
    if request.IsRead:
        if o == 0x60:
            request.Value = s.rx.pop(0) if s.rx else 0
        elif o == 0x20:
            request.Value = 0
        elif o == 0x24:
            request.Value = len(s.rx)
        elif o == 0x28:
            request.Value = 0x06 | (0x08 if s.rx else 0) | (0x10 if len(s.rx) >= FIFO_DEPTH else 0)
        elif o == 0x30:
            request.Value = risr() & s.regs[0x2C]
        elif o == 0x34:
            request.Value = risr()
        elif o in (0x3C, 0x48):
            request.Value = 1 if s.rxo else 0
            s.rxo = False
        elif o in (0x38, 0x40, 0x44):
            request.Value = 0
        else:
            request.Value = s.regs.get(o, 0)
    else:
        v = request.Value & 0xFFFFFFFF
        if o != 0x60:
            self.InfoLog("MMIO W QSPI+0x%03x = 0x%08x" % (o, v))
        if o == 0x60:
            if s.regs[0x08] & 1:
                r = shift(v & 0xFF)
                if len(s.rx) < FIFO_DEPTH:
                    s.rx.append(r)
                else:
                    s.rxo = True
            else:
                self.ErrorLog("qspi: DR write while SPIENR = 0, ignored")
        elif o == 0x08:
            s.regs[0x08] = v & 1
            if not v & 1:
                s.rx = []
        elif o in LOCKED:
            if not s.regs[0x08] & 1:
                s.regs[o] = v
            elif v != s.regs[o]:
                # ChibiOS rewrites CR0 with the same value while enabled.
                self.ErrorLog("qspi: write to 0x%02x while SPIENR = 1, ignored" % o)
        elif o in (0x18, 0x1C):
            if v < FIFO_DEPTH:
                s.regs[o] = v
        elif o == 0x10:
            s.regs[o] = v
        elif o == 0x2C:
            s.regs[0x2C] = v & 0x3F
    update_irq()
