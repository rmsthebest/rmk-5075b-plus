# WB32FQ95 SPIM2 (0x4000_9000), driving the board's WS2812 LED chain from
# MOSI (B15). Renode Python peripheral (IronPython 2.7), mapped at
# 0x4000_9000 +0x100.
#
# Controller: the same DesignWare SSI as QSPI (see qspi.py, whose register
# model this copies): CR0/CR1/BAUDR writable only while SPIENR = 0;
# SER/TXFTLR/RXFTLR any time; SPIENR = 0 flushes; TXFLR always 0 (a byte is
# shifted at once); RX bytes (always 0: nothing drives MISO) fill an 8-deep
# FIFO, overflow sets RXO; ISR = RISR & IER; NVIC line 27 while ISR != 0.
# Not modelled: clock timing, so a FIFO underrun can't show (on the chip the
# line would idle low mid-frame), and frame formats other than 8-bit SPI.
#
# LED chain: QMK's ws2812_spi encoding, 4 SPI bits per WS2812 bit
# (0 = 1000, 1 = 1110), so every byte of a frame is two bits and never 0x00;
# a 0x00 byte is the line held low. A frame starts at the first non-zero byte
# after at least RESET_BYTES zero bytes (QMK sends 4 before and 112 after,
# 116 between frames; WS2812 latch needs 280 us = 105 bytes at 3 MHz) and
# ends at the next zero byte. 12 bytes make one LED, in G, R, B order. A
# byte with a nibble other than 1000/1110, a frame that isn't a whole number
# of LEDs, a frame after too short a reset, or sending with the wrong mode
# (CR0 not 8-bit Motorola mode 0, or BAUDR not 32 = 3 MHz) is logged as an error.
# When a frame's colours differ from the previous frame, logs
# "leds: frame <n>, <count> LEDs, first changed <i>".
#
# Simulation control (offsets 0xEC..):
#   0xEC read : errors logged so far (any of the above)
#   0xF0 read : frames received
#   0xF4 read : LED count of the last frame
#   0xF8 write: LED index for 0xFC
#   0xFC read : that LED's colour in the last frame, 0x00RRGGBB

if request.IsInit:
    from Antmicro.Renode.Core import EmulationManager

    FIFO_DEPTH = 8
    IRQ = 27
    RESET_BYTES = 105
    BAUDR = 32

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
    # LED decoder
    s.zeros = RESET_BYTES     # the line has been low since reset
    s.cur = None              # bytes of the frame in progress, None between frames
    s.leds = []               # last frame, 0xRRGGBB per LED
    s.frames = 0
    s.sel = 0
    s.mode_checked = False
    s.errors = 0

    LOCKED = (0x00, 0x04, 0x14)
    NIBBLE = {0x8: 0, 0xE: 1}

    def error(msg):
        s.errors += 1
        self.ErrorLog(msg)

    def risr():
        r = 1                          # TX level 0 <= TXFTLR
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

    def end_frame():
        data = s.cur
        s.cur = None
        if len(data) % 12:
            error("leds: frame of %d bytes, not a whole number of LEDs" % len(data))
        leds = []
        for i in range(len(data) // 12):
            c = []
            for k in range(3):
                v = 0
                for b in data[i * 12 + k * 4:i * 12 + k * 4 + 4]:
                    hi, lo = NIBBLE.get(b >> 4), NIBBLE.get(b & 0xF)
                    if hi is None or lo is None:
                        error("leds: byte 0x%02x is not two WS2812 bits" % b)
                        hi, lo = hi or 0, lo or 0
                    v = (v << 2) | (hi << 1) | lo
                c.append(v)
            g, r, b = c
            leds.append((r << 16) | (g << 8) | b)
        s.frames += 1
        if leds != s.leds:
            first = 0
            while first < min(len(leds), len(s.leds)) and leds[first] == s.leds[first]:
                first += 1
            self.InfoLog("leds: frame %d, %d LEDs, first changed %d" % (s.frames, len(leds), first))
        s.leds = leds

    def shift(b):
        if not s.mode_checked:
            s.mode_checked = True
            if (s.regs[0x00] & 0x3FF) != 0x07 or s.regs[0x14] != BAUDR:
                error("leds: CR0 0x%x BAUDR %d, not 8-bit mode 0 at 3 MHz" % (s.regs[0x00], s.regs[0x14]))
        if b == 0:
            if s.cur is not None:
                end_frame()
            s.zeros += 1
            return
        if s.cur is None:
            if s.zeros < RESET_BYTES:
                error("leds: frame after only %d zero bytes (latch needs %d)" % (s.zeros, RESET_BYTES))
            s.cur = []
        s.zeros = 0
        s.cur.append(b)

if request.IsInit:
    pass
elif request.Offset >= 0xEC:
    o = request.Offset
    if request.IsRead:
        if o == 0xEC:
            request.Value = s.errors
        elif o == 0xF0:
            request.Value = s.frames
        elif o == 0xF4:
            request.Value = len(s.leds)
        elif o == 0xFC:
            request.Value = s.leds[s.sel] if s.sel < len(s.leds) else 0xFFFFFFFF
        else:
            request.Value = 0
    elif o == 0xF8:
        s.sel = request.Value
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
        if o == 0x60:
            if s.regs[0x08] & 1:
                shift(v & 0xFF)
                if len(s.rx) < FIFO_DEPTH:
                    s.rx.append(0)
                else:
                    s.rxo = True
            else:
                error("spim2: DR write while SPIENR = 0, ignored")
        else:
            self.InfoLog("MMIO W SPIM2+0x%03x = 0x%08x" % (o, v))
            if o == 0x08:
                s.regs[0x08] = v & 1
                s.rx = []
            elif o in LOCKED:
                if not s.regs[0x08] & 1:
                    s.regs[o] = v
                    s.mode_checked = False
                elif v != s.regs[o]:
                    error("spim2: write to 0x%02x while SPIENR = 1, ignored" % o)
            elif o in (0x18, 0x1C):
                if v < FIFO_DEPTH:
                    s.regs[o] = v
            elif o == 0x10:
                s.regs[o] = v
            elif o == 0x2C:
                s.regs[0x2C] = v & 0x3F
    update_irq()
