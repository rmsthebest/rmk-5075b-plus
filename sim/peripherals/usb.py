# WB32FQ95 USB device controller (Mentor MUSB, full speed) at 0x4001_4000,
# with a scripted USB host on its bus. Mapped at 0x4001_4000 +0x1000, which
# also covers CRC (0x800) and SFM (0xC00) as plain registers; SFM USBPCON
# (0xC40) is the PHY enable the host checks.
# Renode Python peripheral (IronPython 2.7).
#
# Device registers (USB_TypeDef in wb32fq95xx.h), all 8 bits:
#   0x00 FADDR  0x01 POWER  0x02 INTRIN  0x04 INTROUT  0x06 INTRUSB
#   0x07 INTRINE  0x09 INTROUTE  0x0B INTRUSBE  0x0C FRAMEL  0x0D FRAMEH
#   0x0E INDEX, then indexed by INDEX: 0x10 INMAXP  0x11 CSR0/INCSR1
#   0x12 INCSR2  0x13 OUTMAXP  0x14 OUTCSR1  0x15 OUTCSR2
#   0x16 COUNT0/OUTCOUNTL  0x17 OUTCOUNTH;  0x20 + 4n FIFO n (byte access)
# Interrupt flags clear on read. The NVIC line (USB, 14) is high while
# INTRIN & INTRINE | INTROUT & INTROUTE | INTRUSB & INTRUSBE is non-zero.
#
# Semantics follow the Mentor MUSBFSFC programming model and what
# ChibiOS-contrib's WB32 driver (known to work on the chip) relies on:
# - A bus reset clears FADDR, INDEX, POWER.SUSEN, the endpoint state and
#   FIFOs, and sets the enables to their reset values (INTRINE 0x0F,
#   INTROUTE 0x0E, INTRUSBE 0x06: no suspend interrupt). ChibiOS writes
#   POWER = SUSEN and sets INTRUSBE.SUSIE again after every reset.
# - Suspend is only detected while POWER.SUSEN is set.
# - EP0 status stage: the host's status token is NAKed while OutPktRdy is
#   set. For a request without a data stage it completes once OutPktRdy is
#   serviced; if DataEnd wasn't set, SetupEnd is raised (the musb crate's
#   SET_ADDRESS handling depends on this).
# - A FADDR write during a control transfer takes effect when its status
#   stage ends: ChibiOS sets the address on receiving SET_ADDRESS
#   (USB_EARLY_SET_ADDRESS), before the host's status IN at address 0.
# - MAXP registers count 8-byte units.
# Anything the host can't accept is logged as "HOST ERROR", which tests treat
# as a failure.
#
# The host: attaches once D+/D- (PA11/PA12) are in their USB alternate
# function, the USB and FIFO clocks are on, USBPRE gives 48 MHz and the PHY
# is enabled. Then, like Linux: 100 ms debounce, reset, GET_DESCRIPTOR(device,
# 64), reset, SET_ADDRESS, the device/config/string descriptors,
# SET_CONFIGURATION, then HID SET_IDLE and report descriptors. After that it
# polls every IN endpoint (interrupt ones at bInterval) and logs:
#   "HOST CDC: <line>"           text lines from a CDC-ACM data interface
#   "HOST HID <ep>: <hex>"        reports from HID IN endpoints
# The host runs in 100 us steps of virtual time and logs every request.
#
# Test commands, written to 0xF00 (offset in this window):
#   0x01                 open the CDC port (SET_LINE_CODING, DTR|RTS)
#   0x02 | byte << 8     send one byte to the CDC data OUT endpoint
#   0x03 | leds << 8     HID LED output report (interrupt OUT, else SET_REPORT)
#   0x04                 suspend the bus;  0x05  resume it
#   0x06                 bus reset and enumerate again
#   0x07 / 0x08          unplug the cable / plug it back in (immediate)
#   0x09 | ep << 8       send the bytes queued at 0xF0C as one packet on
#                        interrupt OUT endpoint ep (raw HID, e.g. Vial)
#   0x0C | id << 8 | iface << 16
#                        GET_REPORT(input, id) on interface iface
# 0xF0C write: queue one byte (value & 0xFF) for command 0x09.
# 0xF04 reads back the host state: 0 detached, 1 enumerating, 2 configured,
# 3 suspended, 4 failed. 0xF08 reads the host's step count (100 us each).
# 0xF14 write: select IN endpoint n; read: the step count when the device
# last set its INPKTRDY (0xFFFFFFFF: never), for press-to-report latency.

if request.IsInit:
    from Antmicro.Renode.Core import EmulationManager
    from Antmicro.Renode.Time import TimeInterval
    from System import Action, AppDomain

    mach = list(EmulationManager.Instance.CurrentEmulation.Machines)[0]
    bus = mach.SystemBus
    nvic = bus.WhatPeripheralIsAt(0xE000E000)
    USB_IRQ = 14
    TICK_US = 100
    TICKS_PER_MS = 1000 // TICK_US
    NEP = 4
    NAK_LIMIT_TICKS = 5000          # 500 ms of NAKs before a transfer fails

    GPIOA_MODER = 0x40000000
    RCC = 0x40010C00
    RCC_USBPRE, RCC_AHBENR1, RCC_USBCLKENR, RCC_USBFIFOCLKENR = 0x14, 0x40, 0x6C, 0x7C
    SFM_USBPCON = 0xC40

    # CSR0 bits
    C0_OUTPKTRDY, C0_INPKTRDY, C0_SENTSTALL, C0_DATAEND = 0x01, 0x02, 0x04, 0x08
    C0_SETUPEND, C0_SENDSTALL, C0_SVDOUTPKTRDY, C0_SVDSETUPEND = 0x10, 0x20, 0x40, 0x80
    # INCSR1 bits
    I_INPKTRDY, I_FIFONE, I_UNDERRUN, I_FLUSH, I_SENDSTALL, I_SENTSTALL, I_CLRTOG = 0x01, 0x02, 0x04, 0x08, 0x10, 0x20, 0x40
    # OUTCSR1 bits
    O_OUTPKTRDY, O_FIFOFULL, O_OVERRUN, O_DATAERR, O_FLUSH, O_SENDSTALL, O_SENTSTALL, O_CLRTOG = 0x01, 0x02, 0x04, 0x08, 0x10, 0x20, 0x40, 0x80
    # INTRUSB bits
    U_SUSPEND, U_RESUME, U_RESET, U_SOF = 0x01, 0x02, 0x04, 0x08
    # POWER bits
    P_SUSEN, P_SUSMD, P_RESUME, P_RSTFLG = 0x01, 0x02, 0x04, 0x08

    other = {}                       # CRC/SFM and unmodelled USB offsets

    class Dev(object):
        pass
    d = Dev()

    def dev_reset(power_on):
        d.faddr = 0
        d.faddr_pending = None
        d.power = 0 if power_on else (d.power & ~(P_SUSEN | P_SUSMD | P_RESUME))
        d.intrin = 0
        d.introut = 0
        d.intrusb = 0
        d.intrine = 0x0F
        d.introute = 0x0E
        d.intrusbe = 0x06
        d.index = 0
        d.frame = 0
        # EP0
        d.c0 = 0                     # OUTPKTRDY/INPKTRDY/SENTSTALL/DATAEND/SETUPEND/SENDSTALL
        d.ep0_rx = []
        d.ep0_tx = []
        # EP1..3, index 0 unused
        d.incsr1 = [0] * NEP
        d.incsr2 = [0] * NEP
        d.inmaxp = [0] * NEP
        d.outcsr1 = [0] * NEP
        d.outcsr2 = [0] * NEP
        d.outmaxp = [0] * NEP
        d.in_fifo = [[] for _ in range(NEP)]
        d.out_fifo = [[] for _ in range(NEP)]
        d.tog_in = [0] * NEP
        d.tog_out = [0] * NEP

    dev_reset(True)
    irq_state = [False]

    def update_irq():
        level = ((d.intrin & d.intrine) | (d.introut & d.introute) | (d.intrusb & d.intrusbe)) != 0
        if level != irq_state[0]:
            irq_state[0] = level
            nvic.OnGPIO(USB_IRQ, level)

    def hexs(b):
        return " ".join("%02x" % x for x in b)

    # ---- register access ----------------------------------------------

    def reg_read(off):
        i = d.index
        if off == 0x00: return d.faddr if d.faddr_pending is None else d.faddr_pending
        if off == 0x01: return d.power
        if off == 0x02:
            v = d.intrin; d.intrin = 0; return v
        if off == 0x04:
            v = d.introut; d.introut = 0; return v
        if off == 0x06:
            v = d.intrusb; d.intrusb = 0; return v
        if off == 0x07: return d.intrine
        if off == 0x09: return d.introute
        if off == 0x0B: return d.intrusbe
        if off == 0x0C: return d.frame & 0xFF
        if off == 0x0D: return (d.frame >> 8) & 0x7
        if off == 0x0E: return d.index
        if i >= NEP:
            return 0
        if off == 0x10: return d.inmaxp[i]
        if off == 0x11:
            if i == 0: return d.c0
            v = d.incsr1[i] & ~I_FIFONE
            if d.in_fifo[i]: v |= I_FIFONE
            return v
        if off == 0x12: return d.incsr2[i]
        if off == 0x13: return d.outmaxp[i]
        if off == 0x14: return d.outcsr1[i]
        if off == 0x15: return d.outcsr2[i]
        if off == 0x16: return (len(d.ep0_rx) if i == 0 else len(d.out_fifo[i])) & 0xFF
        if off == 0x17: return 0 if i == 0 else (len(d.out_fifo[i]) >> 8)
        if 0x20 <= off < 0x20 + 4 * NEP and off % 4 == 0:
            n = (off - 0x20) // 4
            fifo = d.ep0_rx if n == 0 else d.out_fifo[n]
            if not fifo:
                self.WarningLog("HOST ERROR: firmware read empty FIFO %d" % n)
                return 0
            return fifo.pop(0)
        return other.get(off, 0)

    def reg_write(off, v):
        i = d.index
        if off == 0x00:
            if ctl[0] in ("setup", "data", "status"):
                d.faddr_pending = v & 0x7F
            else:
                d.faddr = v & 0x7F
        elif off == 0x01: d.power = (d.power & (P_SUSMD | P_RSTFLG)) | (v & ~(P_SUSMD | P_RSTFLG) & 0xFF)
        elif off == 0x07: d.intrine = v & 0x0F
        elif off == 0x09: d.introute = v & 0x0E
        elif off == 0x0B: d.intrusbe = v & 0x0F
        elif off == 0x0E: d.index = v & 0x0F
        elif off in (0x02, 0x04, 0x06, 0x0C, 0x0D):
            pass                     # read-only
        elif 0x10 <= off <= 0x17:
            if i >= NEP:
                return
            if off == 0x10: d.inmaxp[i] = v
            elif off == 0x11:
                if i == 0: csr0_write(v)
                else: incsr1_write(i, v)
            elif off == 0x12: d.incsr2[i] = v
            elif off == 0x13: d.outmaxp[i] = v
            elif off == 0x14: outcsr1_write(i, v)
            elif off == 0x15: d.outcsr2[i] = v
        elif 0x20 <= off < 0x20 + 4 * NEP and off % 4 == 0:
            n = (off - 0x20) // 4
            (d.ep0_tx if n == 0 else d.in_fifo[n]).append(v & 0xFF)
        else:
            other[off] = v

    def csr0_write(v):
        if v & C0_SVDOUTPKTRDY:
            d.c0 &= ~C0_OUTPKTRDY
            d.ep0_rx = []
        if v & C0_SVDSETUPEND:
            d.c0 &= ~C0_SETUPEND
        if not (v & C0_SENTSTALL):
            d.c0 &= ~C0_SENTSTALL
        if v & C0_SENDSTALL:
            d.c0 |= C0_SENDSTALL
        if v & C0_DATAEND:
            d.c0 |= C0_DATAEND
        if v & C0_INPKTRDY:
            if len(d.ep0_tx) > 64:
                self.WarningLog("HOST ERROR: EP0 IN packet of %d bytes" % len(d.ep0_tx))
            d.c0 |= C0_INPKTRDY

    def incsr1_write(i, v):
        r = d.incsr1[i]
        if v & I_FLUSH:
            d.in_fifo[i] = []
            r &= ~I_INPKTRDY
        if v & I_CLRTOG:
            d.tog_in[i] = 0
        if not (v & I_UNDERRUN): r &= ~I_UNDERRUN
        if not (v & I_SENTSTALL): r &= ~I_SENTSTALL
        r = (r & ~I_SENDSTALL) | (v & I_SENDSTALL)
        if v & I_INPKTRDY:
            maxp = d.inmaxp[i] * 8
            if len(d.in_fifo[i]) > maxp:
                self.WarningLog("HOST ERROR: EP%d IN packet of %d bytes, INMAXP allows %d" % (i, len(d.in_fifo[i]), maxp))
            r |= I_INPKTRDY
            h.ready_tick[i] = h.tick
        d.incsr1[i] = r

    def outcsr1_write(i, v):
        r = d.outcsr1[i]
        if v & O_FLUSH:
            d.out_fifo[i] = []
            r &= ~O_OUTPKTRDY
        if v & O_CLRTOG:
            d.tog_out[i] = 0
        if not (v & O_OUTPKTRDY):
            r &= ~O_OUTPKTRDY
            d.out_fifo[i] = []
        if not (v & O_OVERRUN): r &= ~O_OVERRUN
        if not (v & O_SENTSTALL): r &= ~O_SENTSTALL
        r = (r & ~O_SENDSTALL) | (v & O_SENDSTALL)
        d.outcsr1[i] = r

    # ---- bus side: what the controller does for each host token ---------
    # Each returns "NAK", "STALL", "ACK", "TIMEOUT" or a list of bytes.

    def responds(addr):
        return d.faddr == addr and not (d.power & P_SUSMD)

    def tok_setup(addr, data):
        if not responds(addr): return "TIMEOUT"
        d.c0 &= ~(C0_INPKTRDY | C0_DATAEND | C0_SENDSTALL)
        d.ep0_tx = []
        d.ep0_rx = list(data)
        d.c0 |= C0_OUTPKTRDY
        d.intrin |= 1
        return "ACK"

    def ep0_stall_check():
        if d.c0 & C0_SENDSTALL:
            d.c0 = (d.c0 & ~C0_SENDSTALL) | C0_SENTSTALL
            d.intrin |= 1
            return True
        return False

    def tok_in0(addr):
        if not responds(addr): return "TIMEOUT"
        if ep0_stall_check(): return "STALL"
        if d.c0 & C0_OUTPKTRDY or not (d.c0 & C0_INPKTRDY): return "NAK"
        data = d.ep0_tx
        d.ep0_tx = []
        d.c0 &= ~C0_INPKTRDY
        d.intrin |= 1
        return data

    def tok_out0(addr, data):
        if not responds(addr): return "TIMEOUT"
        if ep0_stall_check(): return "STALL"
        if d.c0 & C0_OUTPKTRDY: return "NAK"
        d.ep0_rx = list(data)
        d.c0 |= C0_OUTPKTRDY
        d.intrin |= 1
        return "ACK"

    def status_done():
        d.intrin |= 1
        if d.faddr_pending is not None:
            d.faddr = d.faddr_pending
            d.faddr_pending = None

    def tok_status_in0(addr, no_data):
        """Status stage of a request with no data stage or an OUT data stage."""
        if not responds(addr): return "TIMEOUT"
        if ep0_stall_check(): return "STALL"
        if d.c0 & C0_OUTPKTRDY: return "NAK"
        if d.c0 & C0_DATAEND:
            d.c0 &= ~C0_DATAEND
        elif no_data:
            d.c0 |= C0_SETUPEND
            self.InfoLog("HOST: status stage ended without DataEnd (SetupEnd raised)")
        else:
            return "NAK"
        status_done()
        return []

    def tok_status_out0(addr):
        """Status stage of a control read."""
        if not responds(addr): return "TIMEOUT"
        if ep0_stall_check(): return "STALL"
        if d.c0 & C0_DATAEND:
            d.c0 &= ~C0_DATAEND
        else:
            d.c0 |= C0_SETUPEND
        d.c0 &= ~C0_INPKTRDY
        d.ep0_tx = []
        status_done()
        return "ACK"

    host_tog_in = [0] * NEP
    host_tog_out = [0] * NEP

    def tok_in(addr, n):
        if not responds(addr): return "TIMEOUT"
        r = d.incsr1[n]
        if r & I_SENDSTALL:
            d.incsr1[n] |= I_SENTSTALL
            d.intrin |= 1 << n
            return "STALL"
        if not (r & I_INPKTRDY): return "NAK"
        data = d.in_fifo[n]
        d.in_fifo[n] = []
        d.incsr1[n] &= ~I_INPKTRDY
        if d.tog_in[n] != host_tog_in[n]:
            self.WarningLog("HOST ERROR: EP%d IN sent DATA%d, host expected DATA%d" % (n, d.tog_in[n], host_tog_in[n]))
        d.tog_in[n] ^= 1
        host_tog_in[n] ^= 1
        d.intrin |= 1 << n
        return data

    def tok_out(addr, n, data):
        if not responds(addr): return "TIMEOUT"
        r = d.outcsr1[n]
        if r & O_SENDSTALL:
            d.outcsr1[n] |= O_SENTSTALL
            d.introut |= 1 << n
            return "STALL"
        if r & O_OUTPKTRDY: return "NAK"
        if len(data) > d.outmaxp[n] * 8:
            self.WarningLog("HOST ERROR: EP%d OUT packet of %d bytes, OUTMAXP allows %d" % (n, len(data), d.outmaxp[n] * 8))
        if d.tog_out[n] != host_tog_out[n]:
            self.WarningLog("HOST ERROR: EP%d OUT expects DATA%d, host sent DATA%d" % (n, d.tog_out[n], host_tog_out[n]))
        d.tog_out[n] ^= 1
        host_tog_out[n] ^= 1
        d.out_fifo[n] = list(data)
        d.outcsr1[n] |= O_OUTPKTRDY
        d.introut |= 1 << n
        return "ACK"

    # ---- the host ----------------------------------------------------------

    h = Dev()
    h.state = 0                      # see 0xF04
    h.addr = 0
    h.cmds = []
    h.out_bytes = []                 # queued by 0xF0C, sent by command 0x09
    h.trace = False                  # 0xF10: log every register access (USBT lines)
    h.cdc_text = []
    h.ifaces = []                    # [number, class, subclass, protocol, [(ep_addr, attrs, maxp, interval)]]
    h.task = None                    # running control-transfer generator
    h.next = None
    h.tick = 0
    h.ready_tick = {}                # 0xF14: IN endpoint -> step of its last INPKTRDY
    h.ready_ep = 0
    h.suspended = False
    h.plugged = True
    ctl = [None]                     # current control stage, for SETUPEND
    result = {}

    def log(msg):
        self.InfoLog("HOST: %s (host t=%d.%d ms, gen %d)" % (msg, h.tick // TICKS_PER_MS, h.tick % TICKS_PER_MS, gen))

    def err(msg):
        self.WarningLog("HOST ERROR: " + msg)
        h.state = 4

    def attached():
        moder = bus.ReadDoubleWord(GPIOA_MODER)
        pins_af = ((moder >> 22) & 3) == 2 and ((moder >> 24) & 3) == 2
        clocks = (bus.ReadDoubleWord(RCC + RCC_AHBENR1) & 0x2) and \
                 (bus.ReadDoubleWord(RCC + RCC_USBCLKENR) & 1) and \
                 (bus.ReadDoubleWord(RCC + RCC_USBFIFOCLKENR) & 1)
        return pins_af and clocks and other.get(SFM_USBPCON, 0) == 2

    def usb_clock_ok():
        # MAINCLK is 96 MHz after rcc::init; USBPRE must divide it to 48.
        pre = bus.ReadDoubleWord(RCC + RCC_USBPRE)
        return pre == 0x09           # SRCEN | DIVEN, ratio /2

    def wait_ms(ms):
        for _ in range(ms * TICKS_PER_MS):
            yield

    def bus_reset():
        log("bus reset")
        dev_reset(False)
        d.power |= P_RSTFLG
        d.intrusb |= U_RESET
        h.addr = 0
        for _ in wait_ms(10):
            yield
        d.power &= ~P_RSTFLG
        for _ in wait_ms(10):
            yield

    def retry(fn, what):
        """Repeats token fn() while it NAKs; result in result['r']."""
        for _ in range(NAK_LIMIT_TICKS):
            r = fn()
            if r != "NAK":
                if r == "TIMEOUT":
                    err("%s: no response" % what)
                result["r"] = r
                return
            yield
        err("%s: NAKed for %d ms" % (what, NAK_LIMIT_TICKS // TICKS_PER_MS))
        result["r"] = "TIMEOUT"

    def control(what, bm, req, value, index, length, out=None):
        """One control transfer. result['data'] = bytes, 'STALL' or None."""
        setup = [bm, req, value & 0xFF, value >> 8, index & 0xFF, index >> 8, length & 0xFF, length >> 8]
        result["data"] = None
        ctl[0] = "setup"
        r = tok_setup(h.addr, setup)
        if r != "ACK":
            err("%s: SETUP %s" % (what, r)); ctl[0] = None; return
        yield
        data = []
        if bm & 0x80:
            ctl[0] = "data"
            while True:
                for x in retry(lambda: tok_in0(h.addr), what + " data IN"):
                    yield
                r = result["r"]
                if r == "STALL":
                    log("%s -> STALL" % what); result["data"] = "STALL"; ctl[0] = None; return
                if r == "TIMEOUT":
                    ctl[0] = None; return
                data += r
                if len(r) < 64 or len(data) >= length:
                    break
                yield
            yield
            ctl[0] = "status"
            for x in retry(lambda: tok_status_out0(h.addr), what + " status OUT"):
                yield
        else:
            if out:
                ctl[0] = "data"
                for k in range(0, len(out), 64):
                    pkt = out[k:k + 64]
                    for x in retry(lambda: tok_out0(h.addr, pkt), what + " data OUT"):
                        yield
                    if result["r"] == "STALL":
                        log("%s -> STALL" % what); result["data"] = "STALL"; ctl[0] = None; return
                    yield
            ctl[0] = "status"
            for x in retry(lambda: tok_status_in0(h.addr, not out), what + " status IN"):
                yield
        ctl[0] = "idle"
        if result["r"] == "STALL":
            log("%s -> STALL" % what); result["data"] = "STALL"; return
        if result["r"] == "TIMEOUT":
            return
        result["data"] = data
        log("%s -> %s" % (what, ("%d bytes: %s" % (len(data), hexs(data))) if bm & 0x80 else "ok"))

    def get_descriptor(what, dtype, idx, length, lang=0, bm=0x80, index=None):
        for x in control("GET_DESCRIPTOR(%s)" % what, bm, 6, (dtype << 8) | idx,
                         lang if index is None else index, length):
            yield

    def parse_config(cfg):
        h.ifaces = []
        k = 0
        while k + 1 < len(cfg):
            ln, ty = cfg[k], cfg[k + 1]
            if ln == 0:
                err("zero-length descriptor in configuration"); return
            if ty == 4:
                h.ifaces.append([cfg[k + 2], cfg[k + 5], cfg[k + 6], cfg[k + 7], []])
            elif ty == 5 and h.ifaces:
                h.ifaces[-1][4].append((cfg[k + 2], cfg[k + 3], cfg[k + 4] | (cfg[k + 5] << 8), cfg[k + 6]))
            k += ln

    def string(idx):
        if idx == 0:
            return
        for x in get_descriptor("string %d" % idx, 3, idx, 255, lang=0x0409):
            yield
        s = result["data"]
        if s and s != "STALL" and len(s) >= 2:
            txt = "".join(unichr(s[k] | (s[k + 1] << 8)) for k in range(2, s[0] - 1, 2))
            log("string %d = '%s'" % (idx, txt))

    def enumerate_device():
        h.state = 1
        for x in bus_reset(): yield
        for x in get_descriptor("device", 1, 0, 64): yield
        dd = result["data"]
        if not dd or dd == "STALL" or len(dd) < 8:
            err("first device descriptor read failed"); return
        for x in bus_reset(): yield
        new_addr = 5
        for x in control("SET_ADDRESS(%d)" % new_addr, 0x00, 5, new_addr, 0, 0): yield
        if result["data"] is None:
            return
        for x in wait_ms(2): yield
        h.addr = new_addr
        for x in get_descriptor("device", 1, 0, 18): yield
        dd = result["data"]
        if not dd or dd == "STALL" or len(dd) != 18:
            err("device descriptor read failed at the new address"); return
        log("device %04x:%04x, EP0 %d bytes" % (dd[8] | dd[9] << 8, dd[10] | dd[11] << 8, dd[7]))
        for x in get_descriptor("config", 2, 0, 9): yield
        c9 = result["data"]
        if not c9 or c9 == "STALL" or len(c9) != 9:
            err("config descriptor read failed"); return
        total = c9[2] | c9[3] << 8
        for x in get_descriptor("config", 2, 0, total): yield
        cfg = result["data"]
        if not cfg or cfg == "STALL" or len(cfg) != total:
            err("full config descriptor read failed"); return
        parse_config(cfg)
        for x in get_descriptor("string 0", 3, 0, 255): yield
        for idx in (dd[15], dd[14], dd[16]):
            for x in string(idx): yield
        for x in control("SET_CONFIGURATION(1)", 0x00, 9, 1, 0, 0): yield
        if result["data"] is None:
            return
        for n in range(NEP):
            host_tog_in[n] = 0
            host_tog_out[n] = 0
        for num, cls, sub, proto, eps in h.ifaces:
            if cls == 3:
                for x in control("SET_IDLE(if %d)" % num, 0x21, 0x0A, 0, num, 0): yield
                for x in get_descriptor("HID report, if %d" % num, 0x22, 0, 255, bm=0x81, index=num): yield
        log("configured: " + ", ".join("if %d class %02x eps [%s]" % (num, cls, " ".join("%02x" % e[0] for e in eps))
                                        for num, cls, sub, proto, eps in h.ifaces))
        h.state = 2

    def find_ep(cls, direction, attrs=None, sub_proto=None):
        for num, c, sub, proto, eps in h.ifaces:
            if c == cls and (sub_proto is None or (sub, proto) == sub_proto):
                for e in eps:
                    if (e[0] & 0x80) == direction and (attrs is None or (e[1] & 3) == attrs):
                        return num, e
        return None, None

    def cdc_comm_iface():
        for num, c, sub, proto, eps in h.ifaces:
            if c == 2:
                return num
        return None

    def run_command(cmd):
        cmd, data = cmd if isinstance(cmd, tuple) else (cmd, None)
        op, arg = cmd & 0xFF, (cmd >> 8) & 0xFF
        if op == 0x01:
            num = cdc_comm_iface()
            if num is None:
                err("no CDC interface"); return
            for x in control("SET_LINE_CODING", 0x21, 0x20, 0, num, 7, out=[0x00, 0xC2, 0x01, 0x00, 0, 0, 8]): yield
            for x in control("SET_CONTROL_LINE_STATE(DTR|RTS)", 0x21, 0x22, 3, num, 0): yield
        elif op == 0x02:
            num, ep = find_ep(0x0A, 0x00)
            if ep is None:
                err("no CDC data OUT endpoint"); return
            for x in retry(lambda: tok_out(h.addr, ep[0] & 0xF, [arg]), "CDC OUT"): yield
        elif op == 0x03:
            # The boot keyboard interface (subclass 1, protocol 1).
            num, ep = find_ep(0x03, 0x00, 3, (1, 1))
            if ep is not None:
                for x in retry(lambda: tok_out(h.addr, ep[0] & 0xF, [arg]), "HID OUT"): yield
                log("HID LEDs %02x sent on EP%d OUT" % (arg, ep[0] & 0xF))
            else:
                num, _ = find_ep(0x03, 0x80, None, (1, 1))
                if num is None:
                    err("no boot keyboard interface"); return
                for x in control("SET_REPORT(output)", 0x21, 0x09, 0x0200, num, 1, out=[arg]): yield
        elif op == 0x04:
            h.suspended = True
            log("bus idle (suspend)")
            for x in wait_ms(3): yield
            if d.power & P_SUSEN:
                d.power |= P_SUSMD
                d.intrusb |= U_SUSPEND
                h.state = 3
            else:
                log("device did not detect suspend: POWER.SUSEN is clear")
        elif op == 0x05:
            log("resume")
            if d.power & P_SUSMD:
                d.power &= ~P_SUSMD
                d.intrusb |= U_RESUME
            h.suspended = False
            for x in wait_ms(20): yield
            h.state = 2
        elif op == 0x06:
            for x in enumerate_device(): yield
        elif op == 0x0A:
            # CLEAR_FEATURE(ENDPOINT_HALT) with wIndex = arg, which may name
            # an endpoint the device doesn't have (a host may send anything).
            for x in control("CLEAR_FEATURE(ENDPOINT_HALT, 0x%02x)" % arg, 0x02, 0x01, 0, arg, 0): yield
        elif op == 0x0B:
            # A control read the host gives up on after its SETUP: the next
            # SETUP comes before the data stage (the device sees SetupEnd).
            setup = [0x80, 6, 0, 2, 0, 0, 0xFF, 0]
            if tok_setup(h.addr, setup) != "ACK":
                err("aborted GET_DESCRIPTOR: SETUP not ACKed"); return
            log("GET_DESCRIPTOR(config) SETUP, then abandoned")
            for x in wait_ms(2): yield
            d.c0 |= C0_SETUPEND
            d.intrin |= 1
            for x in get_descriptor("device, after an aborted transfer", 1, 0, 18): yield
            got = result["data"]
            if not isinstance(got, list) or got[:2] != [18, 1]:
                err("no device descriptor after an aborted transfer: %r" % (got,))
        elif op == 0x0D:
            # A SETUP of 5 bytes (no real host sends one; the device must
            # drop it, not spin on it), then an ordinary request.
            if tok_setup(h.addr, [0x80, 6, 0, 1, 0]) != "ACK":
                err("short SETUP not ACKed"); return
            log("short SETUP (5 bytes)")
            # A host gives up on a control transfer after seconds, so wait
            # past the 2 s software watchdog before the next SETUP (which
            # would end a spin by refilling the FIFO).
            for x in wait_ms(2500): yield
            for x in get_descriptor("device, after a short SETUP", 1, 0, 18): yield
            got = result["data"]
            if not isinstance(got, list) or got[:2] != [18, 1]:
                err("no device descriptor after a short SETUP: %r" % (got,))
        elif op == 0x0C:
            # GET_REPORT(input, id = arg) on interface bits 16-23, 4 bytes
            # asked, as Linux's battery query (hidinput_query_battery_capacity).
            num = (cmd >> 16) & 0xFF
            for x in control("GET_REPORT(input %d)" % arg, 0xA1, 0x01, 0x0100 | arg, num, 4): yield
        elif op == 0x09:
            if not any(e[0] == arg for i in h.ifaces for e in i[4]):
                err("no EP%d OUT" % arg); return
            for x in retry(lambda: tok_out(h.addr, arg, data), "EP%d OUT" % arg): yield
            log("EP%d OUT %s" % (arg, hexs(data)))

    def poll_endpoints():
        frame_start = h.tick % TICKS_PER_MS == 0
        for num, cls, sub, proto, eps in h.ifaces:
            for (addr, attrs, maxp, interval) in eps:
                if not (addr & 0x80):
                    continue
                n = addr & 0xF
                kind = attrs & 3
                if kind == 3 and not (frame_start and (h.tick // TICKS_PER_MS) % max(interval, 1) == 0):
                    continue
                r = tok_in(h.addr, n)
                if r in ("NAK", "TIMEOUT"):
                    continue
                if r == "STALL":
                    err("EP%d IN stalled" % n); continue
                if cls == 0x0A:
                    for b in r:
                        if b == 0x0A:
                            log_cdc()
                        elif b != 0x0D:
                            h.cdc_text.append(b)
                elif cls == 0x03:
                    self.InfoLog("HOST HID %d: %s" % (n, hexs(r)))
                else:
                    log("EP%d IN %s" % (n, hexs(r)))

    def log_cdc():
        self.InfoLog("HOST CDC: " + "".join(chr(b) for b in h.cdc_text))
        h.cdc_text = []

    # A machine reset runs this init again (in the same scope), but the tick
    # scheduled by the previous init would keep going, on the new state.
    # Each init takes a new generation number; a tick from an older one
    # (bound as a default argument when it was defined) stops.
    GEN_KEY = "akko-usb-model-generation"
    gen = (AppDomain.CurrentDomain.GetData(GEN_KEY) or 0) + 1
    AppDomain.CurrentDomain.SetData(GEN_KEY, gen)

    def host_tick(_t, my_gen=gen):
        if AppDomain.CurrentDomain.GetData(GEN_KEY) != my_gen:
            return
        try:
            host_step()
        except Exception as e:
            self.ErrorLog("HOST ERROR: model exception %s" % e)
        update_irq()
        mach.ScheduleAction(TimeInterval.FromMicroseconds(TICK_US), Action[TimeInterval](host_tick))

    def detach(why):
        log(why)
        dev_reset(True)
        h.state, h.task, h.next, h.ifaces, h.suspended, h.cdc_text = 0, None, None, [], False, []
        ctl[0] = None

    def host_step():
        h.tick += 1
        if not h.plugged:
            if h.state != 0:
                detach("cable unplugged")
            return
        if h.state == 0:
            if h.tick % TICKS_PER_MS == 0 and attached():
                if not usb_clock_ok():
                    err("USB clock is not 48 MHz (USBPRE = 0x%x)" % bus.ReadDoubleWord(RCC + RCC_USBPRE))
                    return
                log("device attached")
                h.state = 1
                h.task = iter(wait_ms(100))
                h.next = enumerate_device
            return
        if h.state in (1, 2, 3, 4) and h.tick % TICKS_PER_MS == 0 and not attached():
            detach("device detached")
            return
        if h.tick % TICKS_PER_MS == 0 and not h.suspended:
            d.frame = (d.frame + 1) & 0x7FF
            d.intrusb |= U_SOF
        if h.suspended and (d.power & P_RESUME) and (d.power & P_SUSMD):
            log("remote wakeup")
            d.power &= ~P_SUSMD
            h.suspended = False
            h.state = 2
        if h.task is not None:
            try:
                next(h.task)
            except StopIteration:
                h.task = None
                if getattr(h, "next", None):
                    h.task = iter(h.next())
                    h.next = None
        elif h.cmds and h.state in (2, 3):
            h.task = iter(run_command(h.cmds.pop(0)))
        if h.state == 2 and not h.suspended and ctl[0] in (None, "idle"):
            poll_endpoints()

    mach.ScheduleAction(TimeInterval.FromMicroseconds(TICK_US), Action[TimeInterval](host_tick))

if not request.IsInit:
    off = request.Offset
    if off == 0xF00:
        if request.IsWrite:
            v = int(request.Value)
            if v in (0x07, 0x08):
                h.plugged = v == 0x08
            elif v & 0xFF == 0x09:
                # The packet is the bytes queued so far, not at send time.
                h.cmds.append((v, h.out_bytes))
                h.out_bytes = []
            else:
                h.cmds.append(v)
        else:
            request.Value = 0
    elif off == 0xF04:
        if request.IsRead:
            request.Value = h.state
    elif off == 0xF08:
        if request.IsRead:
            request.Value = h.tick
    elif off == 0xF0C:
        if request.IsWrite:
            h.out_bytes.append(int(request.Value) & 0xFF)
    elif off == 0xF14:
        if request.IsWrite:
            h.ready_ep = int(request.Value)
        else:
            request.Value = h.ready_tick.get(h.ready_ep, 0xFFFFFFFF)
    elif off == 0xF10:
        if request.IsWrite:
            h.trace = int(request.Value) != 0
        else:
            request.Value = int(h.trace)
    elif off < 0x400:
        idx = d.index
        if request.IsRead:
            request.Value = reg_read(off)
        else:
            reg_write(off, int(request.Value) & 0xFF)
        if h.trace:
            self.InfoLog("USBT %s i%d +0x%02x %s 0x%02x" % ("R" if request.IsRead else "W", idx, off,
                         "->" if request.IsRead else "<-", int(request.Value) & 0xFF))
            if request.Length > 1:
                self.WarningLog("HOST ERROR: %d-byte write to USB+0x%03x (registers are 8 bits)" % (request.Length, off))
        if request.IsRead and request.Length > 1:
            self.WarningLog("HOST ERROR: %d-byte read of USB+0x%03x (registers are 8 bits)" % (request.Length, off))
        update_irq()
    else:
        if request.IsRead:
            request.Value = other.get(off, 0)
        else:
            other[off] = int(request.Value)
            self.InfoLog("MMIO W %s+0x%03x = 0x%08x" % ("CRC" if off < 0xC00 else "SFM", off % 0x400, request.Value & 0xFFFFFFFF))
