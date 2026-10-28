# Step 4: `wb32-rom-dfu`, the ROM rewritten in Rust

Not started; it waits for the disassembly (step 2) and the sim traces (step 3).

- A standalone `no_std` crate, not a member of the repo's workspace, depending on `../../crates/wb32-pac` by path.
- One module per ROM area: `reset` (clocks), `usb` (EP0, descriptors), `dfu` (state machine), `flash` (FMC), `boot` (the app jump).
- Every function names the ROM range it reconstructs (`// ROM 0x1FFFE1A0-0x1FFFE1F4`). Keep the ROM's order of register writes, odd parts included, and comment on them instead of tidying them up.
- Its linker script puts it at 0x1FFFE000, which DFU can't write. No bench or flash script refers to it. **It never goes on the board.**
- Equivalence check: run it in the step 3 scenarios and diff its peripheral access traces against the ROM's. Note expected differences (stack addresses, delay loops) in ../notes.md.
- Build with its own target dir (this crate's `target/`), never the repo's `target/`, which the board sim loads.
