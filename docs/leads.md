# Leads

Open items from the 2026-10-30 retrospective review, and changes that could go upstream. Nothing
here is planned work: each needs evidence or a board check first, and nothing goes upstream until
it has run on the keyboard and we're confident in it. There is no rush.

## Board checks

Since b39480e (flashed 2026-10-30, bin 47f23bd3), these are in the tree but not on the keyboard:
the review's leftovers (d42b381, 84ec9c7), our runnables in StaticCells (02f0895) and RMK's
storage built in place (82ee88f, rmk b32708d4). The last one runs on every boot against the real
NOR, so it's the one to watch: the keymap and the lighting settings should survive a reset and a
power cycle, and a Vial keymap edit should still be saved.

Each item is proven in the sim; these confirm it on the board:

1. Caps Lock on over USB, then Fn+Y: the Caps LED goes off on the dongle (C-1/D-1).
2. On the dongle with a 1 min sleep timeout, hold a key past it: it sleeps once (lights off) and
   stays asleep; releasing the key wakes it (E-1).
3. On USB, Fn+Y: the lights don't blink off during the switch (B-6).
4. Optional: Vial 0xD2 after a deliberate fault reads "HardFault" (A-1). No Vial trigger for a
   fault exists yet.

The QSPI FIFO depth (review #25) is in sim-rs/BOARD_FACTS.md, "Open questions for the board".

## Leads that need evidence first

No change is proposed for any of these until something shows it matters.

| Lead | What would settle it |
|------|----------------------|
| A-6/C-4: a host that stays in report protocol and never reads the composite endpoint wedges the writer, then the watchdog, then DFU | A real host or KVM that does it. Upstream chose the boot-mode design over a timeout (#1376), so it stays as is unless one turns up |
| C-3: remote_wakeup always returns Ok, with no SOF confirmation as ChibiOS has | Board: whether SUSIS fires again after a resume the host ignored |
| D-3: an ACK read after the 10 ms deadline causes a resend (the vendor's order too) | A sim repro first (can be done at the desk); if it reproduces, drain RX before the poll |
| D-4: a consumer, system or mouse release lost while the link is down | Board: hold a media key through a link drop |
| B-3: wake edges inside the few-µs window as stop is entered | Needs one bounce-free edge, which switch contacts don't make; a sim hook could reproduce it |
| H1-4: SysTick re-enabled after a stop: does CVR keep its value or reload (ARM's docs disagree)? The stop path relies on it | Board: read CVR after the re-enable through a Vial diag |
| A-8: flip-link's "an overflow faults" is unconfirmed on the WB32 (the region below RAM is "Reserved") | An optional Vial diag that writes 0x1FFF_FFF0; safe now that a fault resets |
| C-2: the D+ pull-up is on about 1.6 s before USB exists (on a blank flash) | Works today; match QMK's order only if a host complains |
| E-6: the knob's start state is read with no 100 µs settle for the pull-ups (QMK waits) | Cheap parity (can be done at the desk), but no failure has been seen |
| F-6: a slow erase could trip the 2 s scan watchdog | Only with a degraded NOR; F-1's fix removed the DFU that would follow |
| embassy-usb: `HidReader::read` reads into chunks of N, so with N under the endpoint's max packet size (RMK's keyboard reader: 1 byte on an 8-byte endpoint) a longer OUT packet overflows at the driver and the endpoint stays stuck. musb, embassy-stm32 and synopsys-otg all keep the packet on overflow, so it's not a musb bug | A host that sends a multi-byte keyboard OUT report. None seen |

Kept as is, by choice: the sim's optional fidelity items (review #30: INVPC nesting, NOCP vs
UNDEFINSTR, narrow writes with side effects). No image depends on them.

## Upstream candidates (held)

Not to be offered until each has run on the keyboard for a while and we're confident in it.
Prepare locally; posting is the user's call. An upstream branch gets its own commits, without
the Co-Authored-By trailer the fork's akko-main commits carry.

- **rmk: `Storage::new_in`** (fork b32708d49, perf(storage)): builds the storage in a slot the
  caller gives, so the poll that opens it doesn't hold copies of it (3.7 KB to 0.75 KB on our
  build). It adds to the API without breaking anything; `Storage::new` wraps it. Expect
  questions about the one `unsafe` (rebuilding the map in place after an erase, because
  sequential-storage keeps its cache reset private); the safety comment gives the argument.
  An alternative to offer: a public cache reset in sequential-storage, which would make the
  `unsafe` unnecessary.
- **rmk: the QMK default-layer option** (7402abdb, with 52a7dd73: layers clear only when the
  default changes).
- **rmk: G-3**, a phantom 100% battery in every USB build: only if the battery hook is ever
  offered upstream, and behind a feature first.
- **embassy-usb: an issue** about `HidReader` with N under the max packet size (the lead above),
  if a host ever shows it.

The musb PR queue and the rmk fix-* branches have their own notes; this list is only what the
review added.
