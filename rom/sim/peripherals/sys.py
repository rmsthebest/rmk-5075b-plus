# WB32FQ95 SYS block at 0x4001_6400 (RM 3.3), read-only identification for
# the ROM's GET_INFO. Renode Python peripheral (IronPython 2.7).
#   0x00 SYS_ID    0x3A50E980, what wb32-dfu-updater_cli prints for this chip
#   0x04 MEMSZ     0x03: an assumption, picked because the ROM's tables
#                  (0x1FFFEE44 flash, 0x1FFFEE40 SRAM) then give the 256 KB /
#                  36 KB the tool reports; the board's real value is unread
#   0x34 BTSR      2: booted from user flash (the ROM is entered by a jump)
# Writes are logged and dropped.

if request.IsInit:
    REGS = {0x00: 0x3A50E980, 0x04: 0x03, 0x34: 0x02}

if not request.IsInit:
    off = request.Offset
    if request.IsRead:
        request.Value = REGS.get(off, 0)
    else:
        self.WarningLog("SYS write dropped: +0x%02x = 0x%08x" % (off, request.Value & 0xFFFFFFFF))
