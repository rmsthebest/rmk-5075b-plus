# WB32FQ95 UART3 (0x4000_8400) with the board's radio module on the other
# end. Renode Python peripheral (IronPython 2.7), mapped at 0x4000_8400 +0x100.
#
# Controller: the DesignWare 16550 of wb32fq95xx.h's UART_TypeDef: RBR/THR
# and DLL at 0x00, IER and DLH at 0x04 (DLL/DLH while LCR.DLAB), IIR/FCR
# 0x08, LCR 0x0C, MCR 0x10, LSR 0x14, USR 0x7C, SFE 0x98, SRT 0x9C, DLF 0xC0.
# Bytes go out the moment THR is written, so THR is always empty (LSR.THRE,
# TEMT); received bytes queue without limit. Interrupt (NVIC line 32) while
# IER.RDAIE and a byte is waiting, or while IER.THREIE (THR is empty).
# Not modelled: timing (a frame is instant), line errors, FIFO depths, the
# modem lines. The first byte sent checks the set-up QMK uses (115200 8N1
# from a 96 MHz PCLK2: DLL 52, DLH 0, DLF 1; LCR 3; FIFOs on) and logs an
# error otherwise.
#
# Radio module: splits what the keyboard sends into frames (lengths by
# command, as module.c sends them), checks each sum, and logs
#   "radio: <- a6 30 d6"          a frame from the keyboard
#   "radio: <- ack"               the keyboard ACKed a module frame
#   "radio: -> 5b 32 8d"          a frame the module sends
# It ACKs every good frame (unless told not to), answers a battery inquiry
# with the battery level, and, with auto-connect on, answers a wireless
# device select (devctrl 0x30-0x35) with CONNECTED.
#
# Simulation control (offsets 0xE0..):
#   0xE0 write: one byte from the module, as is
#   0xE4 write: a module frame "cmd arg sum" from cmd | arg << 8
#   0xE8 write: bit 0 no ACKs, bit 1 auto-connect, bits 8-15 battery %
#               (reset: ACKs on, auto-connect off, battery 100)
#   0xEC read : errors logged so far (a bad sum, a stray byte, wrong set-up)
#   0xF0 read : frames received from the keyboard
#   0xF4 read : ACKs received from the keyboard
#   0xF8 read : the last frame's first 4 bytes, little-endian (first byte low)

if request.IsInit:
    from Antmicro.Renode.Core import EmulationManager

    IRQ = 32
    IER_RDAIE, IER_THREIE = 0x1, 0x2
    LCR_DLAB = 0x80
    # Frame length by command (keyboard to module); None: length byte at [1].
    LENGTHS = {0xA1: 10, 0xA2: 16, 0xA3: 4, 0xA4: 3, 0xA5: 3, 0xA6: 3, 0xA8: 7,
               0xA9: 21, 0xAB: None, 0xAC: None, 0xAD: 6, 0xAF: 36, 0xB3: 5, 0xB4: 3}
    ACK = [0x61, 0x0D, 0x0A]

    nvic = None
    for m in EmulationManager.Instance.CurrentEmulation.Machines:
        nvic = m.SystemBus.WhatPeripheralIsAt(0xE000E000)

    class State(object):
        pass
    s = State()
    s.regs = {0x04: 0, 0x0C: 0, 0x10: 0, 0x98: 0, 0x9C: 0, 0xC0: 0}
    s.dll = 0
    s.dlh = 0
    s.rx = []
    s.irq = False
    s.cur = []
    s.flags = 100 << 8
    s.errors = 0
    s.frames = 0
    s.acks = 0
    s.last = 0
    s.setup_checked = False

    def error(msg):
        s.errors += 1
        self.ErrorLog(msg)

    def hexs(bs):
        return " ".join("%02x" % b for b in bs)

    def module_send(bs):
        self.InfoLog("radio: -> " + hexs(bs))
        s.rx.extend(bs)

    def module_frame(cmd, arg):
        module_send([cmd, arg, (cmd + arg) & 0xFF])

    def got_frame(f):
        s.frames += 1
        s.last = 0
        for i, b in enumerate(f[:4]):
            s.last |= b << (8 * i)
        self.InfoLog("radio: <- " + hexs(f))
        if f[0] == 0xB3:
            ok = f[4] == (0xB3 + f[1] - f[3]) & 0xFF
        elif f[0] == 0xB4:
            ok = f[2] == 0xB4
        elif f[0] == 0xA9:
            ok = f[2 + f[1]] == sum(f[:2 + f[1]]) & 0xFF
        else:
            ok = f[-1] == sum(f[:-1]) & 0xFF
        if not ok:
            error("radio: bad sum in " + hexs(f))
            return
        if not s.flags & 1:
            module_send(ACK)
        if f[0] == 0xA6:
            if f[1] == 0x53:
                module_frame(0x5C, (s.flags >> 8) & 0xFF)
            elif 0x30 <= f[1] <= 0x35 and s.flags & 2:
                module_frame(0x5B, 0x32)

    def check_setup():
        s.setup_checked = True
        got = (s.dll, s.dlh, s.regs[0xC0], s.regs[0x0C] & 0x7F, s.regs[0x98] & 1)
        if got != (52, 0, 1, 3, 1):
            error("uart3: DLL %d DLH %d DLF %d LCR 0x%x SFE %d, not 115200 8N1 with FIFOs" % got)

    def shift(b):
        if not s.setup_checked:
            check_setup()
        s.cur.append(b)
        f = s.cur
        if f[0] == 0x61:
            if len(f) == 3:
                s.cur = []
                if f == ACK:
                    s.acks += 1
                    self.InfoLog("radio: <- ack")
                else:
                    error("radio: bad ACK " + hexs(f))
            return
        if f[0] not in LENGTHS:
            error("radio: stray byte 0x%02x" % b)
            s.cur = []
            return
        n = LENGTHS[f[0]]
        if n is None:
            if len(f) < 2:
                return
            n = f[1] + 3
        if len(f) == n:
            s.cur = []
            got_frame(f)

    def update_irq():
        ier = s.regs[0x04]
        level = bool((ier & IER_RDAIE and s.rx) or ier & IER_THREIE)
        if level != s.irq:
            s.irq = level
            nvic.OnGPIO(IRQ, level)

if request.IsInit:
    pass
elif request.Offset >= 0xE0:
    o = request.Offset
    if request.IsRead:
        if o == 0xEC:
            request.Value = s.errors
        elif o == 0xF0:
            request.Value = s.frames
        elif o == 0xF4:
            request.Value = s.acks
        elif o == 0xF8:
            request.Value = s.last
        else:
            request.Value = 0
    else:
        v = request.Value & 0xFFFFFFFF
        if o == 0xE0:
            module_send([v & 0xFF])
        elif o == 0xE4:
            module_frame(v & 0xFF, (v >> 8) & 0xFF)
        elif o == 0xE8:
            s.flags = v
    update_irq()
else:
    o = request.Offset
    dlab = s.regs[0x0C] & LCR_DLAB
    if request.IsRead:
        if o == 0x00:
            request.Value = s.dll if dlab else (s.rx.pop(0) if s.rx else 0)
        elif o == 0x04:
            request.Value = s.dlh if dlab else s.regs[0x04]
        elif o == 0x08:
            request.Value = 0x04 if s.rx else (0x02 if s.regs[0x04] & IER_THREIE else 0x01)
        elif o == 0x14:
            request.Value = 0x60 | (0x01 if s.rx else 0)
        elif o == 0x7C:
            request.Value = 0x06 | (0x08 if s.rx else 0)
        elif o == 0x80:
            request.Value = 0
        elif o == 0x84:
            request.Value = len(s.rx)
        else:
            request.Value = s.regs.get(o, 0)
    else:
        v = request.Value & 0xFFFFFFFF
        if o == 0x00:
            if dlab:
                s.dll = v & 0xFF
            else:
                shift(v & 0xFF)
        else:
            self.InfoLog("MMIO W UART3+0x%03x = 0x%08x" % (o, v))
            if o == 0x04:
                if dlab:
                    s.dlh = v & 0xFF
                else:
                    s.regs[0x04] = v & 0x8F
            elif o in s.regs:
                s.regs[o] = v
    update_irq()
