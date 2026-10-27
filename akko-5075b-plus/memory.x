/* The chip is a WB32FQ95xC (the DFU tool reports 256 KB flash, 36 KB SRAM),
   although QMK links it as an xB (128K/28K). */
MEMORY
{
  FLASH : ORIGIN = 0x08000000, LENGTH = 256K
  /* 28K minus the last 8 bytes: 0x20006FFC holds the bootloader magic word
     (boot-guard), shared with QMK. Keep it out of .data/.bss/stack. The SRAM
     above it holds boot-guard's panic record (0x20007000). */
  RAM   : ORIGIN = 0x20000000, LENGTH = 28K - 8
  /* The SRAM past boot-guard's records (panic record 0x20007000, crash
     counter 0x20007100), to the end of the 36K: buffers that need no
     start-up initialisation, in section .ram2 (src/main.rs `ram2`).
     flip-link puts the stack at the bottom of RAM, so every byte of .bss is
     a byte of stack; big buffers go here instead. */
  RAM2  : ORIGIN = 0x20007400, LENGTH = 36K - 0x7400
}

SECTIONS
{
  .ram2 (NOLOAD) : ALIGN(4)
  {
    *(.ram2 .ram2.*);
    . = ALIGN(4);
  } > RAM2
} INSERT AFTER .got;
