# The WB32FQ95 ROM bootloader: notes

Filled in as we read the dump. Every claim names its ROM address (or `out/` file), or says it is from the reference manual (RM2905025; `target/rm/rm.txt` is its `pdftotext -layout` text). `out/` means `$ROM_WORK/out/` (rom/README.md). Mark guesses as guesses.

## Dump

- rom.bin sha256: 3067688dbf8467244d217fee457146f48e390e67263c5297e14cc0e8de5ebe40
- info.bin sha256: 31eaf7d2181635756d8c4f2a1f43f31e63bd7e9bc48339166ab2b2f3f45e38ae
- Read on 2026-10-28 20:32 with rom/tools/rompeek.py (an akko-rmk image with the `rom-peek` feature). The two reads agreed. These hashes identify the dump; if yours match, it's the same ROM.
- The dump is real: the vector table points into the ROM, and the chip id 0x3A50E980 is at 0x1FFFF200.

## Known before the dump

- System memory is 0x1FFFE000–0x1FFFEFFF, 4 KB (RM 3.2.3). It is aliased at 0x0 when the chip boots from it (BOOT0=1, RM 2.4). Our board doesn't use that path: firmware jumps in.
- The information blocks are 0x1FFFF000–0x1FFFF7FF (RM 3.2.2). Blocks 0–3 are read-only in hardware. Block 4 is boot control, 5 user write protection, 6 secondary development protection, and 7 reboot configuration plus OTP from 0x1FFFF720. The unique ID is at 0x1FFFF7E8 (RM 26.3).
- SYS_BTCR (0x4001640C) holds SMEM_DIS (bit 24), SRAM_DIS (16) and SWD_DIS (8). With RD_PROT these make up the security level (RM table 3-1). SYS_BTSR (0x40016434) BOOT_MODE: 01 system memory, 10 user code, 11 SRAM.
- The RM says the boot loader "can be activated through the UART interface". On this board it enumerates over USB as 342d:dfa0, and wb32-dfu-updater_cli talks to it.
- How we get in: boot-guard writes 0xDEADBEEF at 0x20006FFC and resets, then jumps to 0x1FFFE000 (crates/akko-boot-guard). QMK uses the same magic.
- The tool reports chip id 0x3A50E980, 256 KB flash and 36 KB SRAM on download, so the ROM reads these from somewhere (SYS_ID 0x40016400? the information blocks?).
- Firmware "can't be read off the keyboard" (FLASHING.md). Why: (UPLOAD path, below).
- QMK's EFL driver (hal_efl_lld.c `pre_op_code`) runs a RAM blob that reads 0x1FFFF000 and sets up FMC timing. The ROM's flash routine may do the same.

## Information blocks (tools/info.py)

- 0x1FFFF200: 0x3A50E980, the chip id the DFU tool prints.
- 0x1FFFF410 (block 4, boot control): 0x0C0C0C0C, then 0xF3 at 0x1FFFF414 and 0x5AA5 at 0x1FFFF418. The ROM's read-protection check (0x1FFFE928) reads 0x1FFFF410: low nibble 0xC = unprotected. So **this chip is unprotected**. These are exactly the values the updater's `fn_unread_protection` writes (see "Toolbox mode" below), and so are block 5's 0xFFFFFFFF at +0x10 and block 6's zeros.
- 0x1FFFF24C / 0x1FFFF24E: 0x000B / 0xFFF4, checked at start-up in 0x1FFFE940 (see below).
- 0x1FFFF340: read by 0x1FFFEE54 (the flash routine, it seems): 0x05807304. Probably flash timing trim, like QMK's `pre_op_code`. Guess.
- Block 5 (write protection) at 0x1FFFF510: ffffffff, then zeros. Block 6: all zero.

## Vector table and start-up

- 31 entries. Initial SP 0x20000450, Reset 0x1FFFE091. Slot 7 (reserved) holds 0xFFFF0004, maybe a marker or version. Every IRQ goes to one default loop (0x1FFFE0AF) **except USB (IRQ 14, 0x1FFFE839)**. The ROM is USB-interrupt-driven and uses no other interrupt.
- Reset -> 0x1FFFE07C -> 0x1FFFE0E0 (copies/zeroes .data/.bss at 0x20000000, ldm of 3 words) -> main 0x1FFFE940.
- SRAM use: the start-up table at 0x1FFFEFE8 copies 0x18 bytes of .data to 0x20000000 and zeroes 0x438 bytes from 0x20000018 to 0x20000450, which includes the stack. That is all of it: 0x20000000-0x2000044F. **It never touches 0x20006FFC (the boot magic) or 0x20007000+ (boot-guard's panic record and crash counter)**, which is why the panic record survives DFU. Only the stack region is dynamic, so check the stack depth before saying more.
- No call or jump leaves the ROM (out/calls.txt has only SRAM and information-block references). It doesn't depend on any other ROM code.

## Call graph (out/functions.txt)

## Clocks and USB set-up

From main 0x1FFFE940, in order. The register names are pointer + offset resolved against out/labels.csv.
1. IRQs off. SCB_AIRCR priority grouping is set (VECTKEY 0x05FA, bits 8-10 = 3).
2. PWR_ANAKEY1 = 3, PWR_ANAKEY2 = 0xC (unlocks the analog registers, as in our stop.rs).
3. ANCTL_MHSIENR = 1 and a wait on MHSISR. MAINCLKSRC = 0 (MHSI) and MAINCLKUEN = 1. CACHE_CR = the constant at 0x1FFFED30.
4. **Optional IWDG**, only if RCC+0x104 (0x40010D04, not in the RM; RSTSTAT is at +0x100) is non-zero:
   - a 0x6E0-tick SysTick wait, then RCC_AHBENR0 |= 4;
   - the IWDG starts: KR 0xAAAA, KR 0x5555 + PR = 6 (/256), KR 0x5555 + RLR = 0xFFF, waiting on SR between steps. That's about 26 s at a 40 kHz LSI.

   The feed, 0x1FFFE904, **re-reads RCC+0x104 every time** and writes KR = 0xAAAA only while it's non-zero. It's called from the receive/send loops, every delay and every erased page. So that register works as an "IWDG is running" flag. Guess: hardware sets it, from a reset cause or an option bit in the information blocks. **This bears on our IWDG question.** If our firmware's IWDG sets it, DFU keeps the dog fed. If it doesn't, DFU would be reset about 26 s (or whatever we programmed) after entry. Unresolved; the sim can't tell us, but the board could, with a 0xDA-style read of 0x40010D04 after starting an IWDG.
5. ANCTL_PORCR = 0x7BE, ANCTL_FHSIENR = 1 (fast HSI on). VTOR = 0x1FFFE000, SysTick off, NVIC ICER0/1 = all.
6. IRQs on. **The trim check:** if 0x1FFFF24C != 0x000B *and* 0x1FFFF24E != 0xFFF4, it hangs forever. Ours are 0x000B/0xFFF4.
7. Buses: AHBPRE = 0, APB1PRE = 0x80, APB1ENR = 0x81E0, a pulse of APB1RSTR bits 5-8, then APB1ENR = 0x8120.
8. **HSE crystal:** GPIOD CFGMSK = 0xFFFC and MODER = 0xF (PD0/PD1 to analog, the oscillator pins). HSECR1 = 2, HSECR0 = 1, then a wait for HSESR != 0. **No crystal means it hangs here**, with no timeout. Our board has one, since QMK and RMK run on HSE.
9. CACHE_CR again. PLL: PLLSRC = 1 (HSE), PLLPRE = 0x20|4|1, PLLCR = 0x80, PLLENR = 1, then a wait for PLLSR == 3 (locked). MAINCLKSRC = 2 (PLL), MAINCLKUEN = 1. PWR_ANAKEY1 = 0 (locks again).
10. 0x1FFFE6B8: GPIOA clock on (APB1ENR |= 0x8020). Only PA11/PA12 unmasked (CFGMSK 0xFFFFE7FF/0xFFFFEFFF), driven as GPIO outputs (BSRR), i.e. **the USB D-/D+ pins held, a forced disconnect**.
11. A wait of 15 x 100 ms = **1.5 s** (0x1FFFE178 with 0x493E00 ticks at 48 MHz), so the host sees the old device go away.
12. 0x1FFFE754, the USB init:
    - clears its state (0x20000238, 0x14 bytes);
    - AHBENR1 |= 0x100|2, and a pulse of AHBRSTR1 bit 1;
    - USBFIFOCLKSRC = 1, USBFIFOCLKENR = 1, **SFM_USBPCON = 2** (the internal D+ pull-up, so no board pull-up is needed), USBCLKENR = 1, USBPRE = 8;
    - USB interrupt enables cleared, the USB IRQ's priority byte (NVIC_IPR byte 14, 0xE000E40E) = 0x10, then NVIC_ISER0 = 1<<14.
13. 0x1FFFE63C: PA11/PA12 to AF3 (AFRH 0x33000, MODER 0x2800000), so USB is on the pins. USB+0xB = 6. Then the command loop.

Leaving (RESET/GO), 0x1FFFE678 undoes the USB set-up: NVIC_ICER0 = 1<<14, USBPRE/USBCLKENR/USBFIFOCLKENR = 0, AHBENR1 bit 1 off. Then 0x1FFFE6B8 holds the pins again, so the host sees a disconnect before the reset or jump.

## USB: descriptors and EP0

- Device descriptor at 0x1FFFEF45: `12 01 00 02 00 00 00 40 2d 34 a0 df 00 01 01 02 00 01`, i.e. USB 2.0, class 0, EP0 64 bytes, **VID 0x342D, PID 0xDFA0**, bcdDevice 1.00, strings 1/2, one configuration.
- Configuration descriptor at 0x1FFFEF57: `09 02 20 00 01 01 00 80 32`, i.e. one interface, bus-powered, 100 mA. Interface `09 04 00 00 02 ff ff 00 00`: **vendor class 0xFF/0xFF**, 2 endpoints. **EP1 IN 0x81 bulk 64** and **EP1 OUT 0x01 bulk 64**. It's not USB DFU: no DFU functional descriptor and no class requests. "DFU" is only the name.
- Strings: 0 = 0x0409, 1 = "Westberry Tech.", 2 = "WB Device in DFU Mode" (pointer table at 0x1FFFEFF4: 0x1FFFEF77, 0x1FFFEF7B, 0x1FFFEF9B). No serial string.
- Packets: a command is one or more 64-byte bulk OUT transfers. A short packet ends it, and the total is at most 0x110 bytes (0x1FFFED60). The reply goes back on EP1 IN in 64-byte chunks (0x1FFFEDD8). Both loops feed the IWDG, and they give up when the USB state (0x2000024A) leaves 2 (configured).
- The USB IRQ handler 0x1FFFE838 calls 0x1FFFE1A0 (the EP0 state machine: setup/data/status via USB_CSR0, USB_INDEX) and helpers at 0x1FFFE6F0, 0x1FFFE6FC and 0x1FFFE7DC.
- Standard requests (0x1FFFE330, a table branch on bRequest 0-9; rust/src/ep0.rs). Answered: GET_STATUS, CLEAR_FEATURE and SET_FEATURE (DEVICE_REMOTE_WAKEUP, ENDPOINT_HALT), SET_ADDRESS, GET_DESCRIPTOR (device, configuration index 0, strings 0-2), GET_CONFIGURATION, SET_CONFIGURATION (only 1). Stalled: SET_DESCRIPTOR, GET_INTERFACE, SET_INTERFACE, SYNCH_FRAME, and every class or vendor request (bmRequestType & 0x60 isn't 0).
- Quirks, all harmless to a normal host:
  - SET_ADDRESS writes FADDR at once, before the status stage, and is refused once configured.
  - GET_STATUS(device) finds bmAttributes by looking up the configuration descriptor with the *current configuration* as the index. Once configured (1) the lookup fails, and the ROM reads byte 7 of the null pointer, address 0x00000007 (the boot alias).
  - CLEAR_FEATURE(ENDPOINT_HALT) resets the data toggle but doesn't clear SendStall, so a halted endpoint stays halted until SET_CONFIGURATION.
  - The string language ID isn't checked.
  - No request has an OUT data stage, so the ROM's OUT data path is never reached. Its last packet would write 0x68 to CSR0, which includes SendStall.

## DFU state machine

It isn't the DFU class, but its own command protocol over the bulk pipe. The names come from wb32-dfu-updater's `src_common/cmdinfo.h` and they match the ROM. The main loop (0x1FFFEAFE) receives a command into 0x20000018, switches on byte 0 and replies with `<cmd> <status> [data]`.

Commands. All fields are little-endian, `addr` is at +4 and `n` at +8:

| Byte | Name | Request | What the ROM does |
|---|---|---|---|
| 0x01 | GET_INFO | `01 00` | Replies 16 bytes: `01 00 0d <ver> <SYS_ID 0x40016400> <flash bytes> <sram bytes>`. ver = the byte at 0x1FFFE01C (vector slot 7: **0x04**, the bootloader version). The sizes come from SYS_MEMSZ (0x40016404) through the byte table at 0x1FFFEE44, x 4 KB. Sub-id other than 0: ARGERR. |
| 0x5E | ERASE | `5e 00 .. addr n` | Not if read-protected (0x1A), chip erase included. Page erase: addr rounded down to 256 bytes, n pages; every page must be in flash (0x1FFFE8CC = 1). FMC op 0x08 per page, feeding the IWDG. |
| | | `5e f0` | Chip erase: FMC op 0x1B on address 0. |
| 0x6A | WRITE_PAGE | `6a .. addr n data` | addr rounded down to 256 bytes, n bytes of data at +12. Allowed in flash (region 1), or in **information blocks 4-7, 0x1FFFF400-0x1FFFF7FF** (region 2: addr + 0xE0000C00 < 0x400). n <= 0x100; only the start address is checked. Read protection refuses region 1 (0x1A) but **not region 2**, which is what lets the updater lift protection by writing block 4 (0x1FFFEBEC). For region 2 the ROM erases the page first (op 0x08). Then op 0x04 (clear the page latch), the data words into the latch at 0x40017900, and op 0x0C (program). Failure: 0x12. |
| 0x72 | READ | `72 .. addr n` | n <= 0x100. Not if read-protected (0x1A). addr and addr+n-1 must be in flash or blocks 4-7. Replies `72 00` + n bytes. |
| 0x83 | RESET | `83 .. ms` | Acks, waits 10 ms, takes USB and GPIOA down (0x1FFFE678, 0x1FFFE6B8), waits `ms` (at +4) x 1 ms (48000 SysTick ticks each, so HCLK is 48 MHz by then; inferred), then AIRCR = 0x05FA0004 (SYSRESETREQ). The updater's `-R` sends this. |
| 0x9B | GO | `9b .. addr ms` | addr must be in flash. Acks, takes USB and GPIOA down, waits `ms` (at +8), MAINCLKSRC back to MHSI, **VTOR = 0** (0xE000ED08), MSP = [addr], then jumps to [addr+4]. Clocks/PLL otherwise stay as the ROM left them. |
| other | | | INVALID (0xFF) |

Status codes (cmdinfo.h): 0x00 success, 0x01 ARGERR, 0x11 ERASE_FAIL, 0x12 WRITE_FAIL, 0x1A READ_PROTECT, 0xFF INVALID.

Address check 0x1FFFE8CC: 1 if `addr - 0x08000000 < table[SYS_MEMSZ & 0xF] << 12`, 2 if `0x1FFFF400 <= addr < 0x1FFFF800`, else 0. **System memory itself (the ROM) and blocks 0-3 aren't reachable by any command**, so the ROM can't be told to overwrite itself or the factory trim.

## Flash erase and program (FMC)

0x1FFFE104(addr, op) runs one FMC operation:
1. It calls 0x1FFFEE54, an FHSI check, which reads 0x1FFFF340. **QMK's `pre_op_code` (hal_efl_lld.c) is byte-for-byte this function**: 228 bytes, identical. It restarts FHSI and enables a block at 0x4000B800, which the RM lists as reserved but which RCC_RNGCLKENR clocks (likely an undocumented RNG). It reads 0x4000B800 twenty times. If more than two of the ten even-numbered samples equal the one before, FHSI counts as stuck: its trim (ANCTL+0x34 bits 20-27, from 0x1FFFF340 bits 20-27) is lowered by 16 per try and FHSI restarted, up to five tries. It leaves the analog registers unlocked.
2. FMC_ADDR = addr, RCC_PCLKENR = 1, FMC_CON = 0x7F5F0D40 | op, and the keys 0x5188DA08, 0x12586590 go to FMC_KEY.
3. It copies a 12-byte routine from 0x1FFFEF38 to the stack and runs it there: `str r0,[r1]; nop; ldr r0,[r1]; lsls r0,#24; bmi` (CON = 0x800080 to start, then wait for bit 7 to clear). The ROM is itself in the flash macro (RM 26.1: "stored in the System memory area in the Flash memory module"), so it can't run from there while the flash is busy. **QMK's `FLASH_OP_RAM_CODE` is the same 12 bytes.**
4. RCC_PCLKENR = 0, FMC_CON = 0x005F0000, then the result is FMC_STAT bit 2 (error).

Op codes: 0x04 clears the page latch, 0x08 erases a page, 0x0C programs a page, 0x1B erases the chip. ChibiOS-contrib's WB32 EFL driver is a copy of this sequence.

## UPLOAD, and read protection

- `-U` is just READ (0x72) from 0x08000000, 256 bytes at a time (dfufw.c `dfufw_opt_upload`, `fn_read_data`).
- READ and ERASE are refused (0x1A) unless 0x1FFFF410's low nibble is 0xC. **Ours is 0xC**, so `-U` should work on this chip now. FLASHING.md's "can't be read off" was probably true of the stock firmware's protected state. The chip was unprotected later, perhaps by the updater's prompt the first time QMK was flashed (see below). That's a guess. Not tried, and no need: we have every image that has been on it.

## Toolbox mode (`-t`)

What it asks the ROM to do. Recorded so we know what never to send.

`fn_unread_protection` (dfufwdevice.c) runs when ERASE returns READ_PROTECT. With `-t` it runs without asking. Without `-t` it asks "[Y/n]", and when the countdown runs out **that counts as yes**. It sends WRITE_PAGE to:
- block 4 (0x1FFFF400): +0x10 = 0x0C0C0C0C, +0x14 = 0xF3 (its inverse), +0x18 = 0x5AA5;
- block 5 (0x1FFFF500): +0x10 = 0xFFFFFFFF, +0x1C = 0;
- block 6 (0x1FFFF600): all zeros.

Then it sends RESET and asks for a power cycle. Each of those is an erase and reprogram of an information page (the ROM erases region-2 pages first). Power lost between the erase and the program would leave block 4 blank, which reads as protected. The RM warns that erasing blocks 4-7 while they are write-protected damages the main array. **Our chip already holds exactly these values**, so `-t` would rewrite them for nothing, at a risk. Never send it.

## Leaving DFU: reset and jump to the app

There's no automatic exit and no timeout. It stays in DFU until RESET (0x83) or GO (0x9B), or until power is cut. With the IWDG running (RCC+0x104 set), the main loop keeps feeding it, so it doesn't time out either. Normal boot from flash never goes through the ROM (BOOT0 = 0). The ROM runs only when our firmware jumps to it (boot-guard) or when BOOT0 is high.

## Dependencies outside the ROM (out/calls.txt)

## Confirmed in the sim (rom/sim/README.md)

The real ROM in Renode enumerated as decoded. GET_INFO gave the predicted 16 bytes. ERASE, WRITE_PAGE and READ round-tripped a page, the address checks refused the ROM and blocks 0-3, and GO and RESET both ended in a downloaded app (VTOR = 0, SP from the app). The write traces are in sim/traces/.

## Open questions

- What sets RCC+0x104 (0x40010D04), the IWDG flag? It needs the board.
- The real SYS_MEMSZ value (the sim assumes 0x03).
- What 0x0D in GET_INFO's reply stands for.
- What the block at 0x4000B800 is (the FMC preparation's FHSI check).

## The Rust translation (rust/, step 4)

`rust/` rewrites the whole ROM in Rust, one function per ROM function, each naming its range. Built for 0x1FFFE000 (3.9 KB) with the ROM's SRAM layout, it gives **the same write traces as the ROM in all three scenarios** (boot 347 writes, download 605, leave 634: `diff` empty), and the ROM suites pass on it (`sim/run.sh --rust`, 9/9).

Matching the traces corrected two readings of the decompiled C:
- The FHSI retrim in 0x1FFFEE54 lowers the trim by 16 on *every* try, not by 16 from the original each time.
- The EP1 OUT byte count (0x1FFFE3F8) is two byte reads, OUTCOUNTL and OUTCOUNTH. Ghidra had shown one halfword read; the sim's USB model rejects that.

Where the translation differs from the ROM. None of these shows in a write trace:
- Reset: the ROM zeroes 0x438 bytes from 0x20000018, up to its stack top, stack included. Its init can do that because it never pushes. The translation zeroes .bss (0x20000018-0x2000024B) only.
- GO: if the app returns, the ROM goes back to its command loop on the app's stack. The translation's call doesn't return.
- Exceptions: the ROM has a separate `b .` per handler (all IRQs but USB share one). The translation uses one shared handler; HardFault still resets.
- Timing: the delay loops are the same SysTick periods. The `for (i = 0; i < 1000; i++)` spins in the FHSI check run at Rust's speed.
