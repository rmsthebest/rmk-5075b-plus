# WB32FQ95 flash cache controller, 0x4001_5400 (CACHE_TypeDef in wb32fq95xx.h).
# Renode Python peripheral (IronPython 2.7). Only remembers writes, so the
# clock setup's CR write (cache on, prefetch, wait states) shows up in traces.
#
# Log lines: "MMIO <R|W> CACHE+0x<off> = 0x<val>" (Info).

if request.IsInit:
    regs = {}

if not request.IsInit:
    off = request.Offset
    if request.IsRead:
        request.Value = regs.get(off, 0)
        self.InfoLog("MMIO R CACHE+0x%03x = 0x%08x" % (off, request.Value))
    else:
        regs[off] = request.Value & 0xFFFFFFFF
        self.InfoLog("MMIO W CACHE+0x%03x = 0x%08x" % (off, regs[off]))
