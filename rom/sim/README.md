# Step 3: the ROM in Renode

The real ROM (`$ROM_WORK/dump/rom.bin`, ../README.md) runs on the Akko machine. `rom.resc` takes the board sim's platform and models (`../../sim`), so they stay in step, and adds:
- `peripherals/usb_rom.py`: a copy of the USB host model with the vendor bulk pipe. It also accepts USBPRE 0x08, since the ROM's PLL runs at 48 MHz. The changes are marked `ROM:`.
- `peripherals/sys.py`: SYS_ID 0x3A50E980 and MEMSZ 0x03. The MEMSZ value is an **assumption**: it's the one that gives the 256 KB / 36 KB the tool reports.
- `peripherals/fmc.py`: the flash controller, as the ROM drives it.

```fish
rom/sim/run.sh                                  # the suites, about 2.5 min in parallel
python3 -I rom/sim/trace.py boot|download|leave # write traces → rom/sim/traces/
```

Suites (10 tests):
- `enumerate.robot`: 342d:dfa0 with its strings and the vendor interface; GET_INFO answers `01 00 0d 04 80 e9 50 3a 00 00 04 00 00 90 00 00`; an unknown command gets INVALID.
- `download.robot`: ERASE, WRITE_PAGE and READ round-trip a page; the ROM and blocks 0–3 can't be read; an erase outside flash is refused.
- `leave.robot`: GO ends in the app with VTOR = 0 and SP from the app; GO outside flash is refused; RESET boots the app.
- `peek.robot`: the firmware's `rom-peek` channel on the board sim (run.sh builds that image into `target/rom-peek`).

Traces, in `traces/`: `boot` 347 writes, `download` 605, `leave` 634. Writes only, in order, with no PCs. A rerun gives the same trace (checked on `download`). These are step 4's reference.

Differences from the board:
- SysTick runs at 96 MHz in this platform, so the ROM's delays are half as long. The host sees the device at 0.75 s, not 1.5 s.
- The FMC model is never busy, and erased bytes read 0x00.
- SYS MEMSZ is assumed, as above.

Gotchas:
- Don't pause the emulation mid-enumeration (`pauseEmulation=true` on a log entry before "configured"). The host model can't resume inside a control transfer.
- Renode's log tester only matches after its previous match, so wait for a peripheral's log line before the bulk reply that follows it.
- `machine Reset` resets VectorTableOffset, so the ROM is entered by setting SP and PC (`$enter_rom`, trace.py).
