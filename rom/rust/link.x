/* The ROM's memory map: code at 0x1FFFE000 (4 KB, the information blocks
 * start at 0x1FFFF000), and SRAM 0x20000000-0x2000044F only, as the ROM.
 * The sim loads this over the ROM image; DFU can't write here, and nothing
 * here goes on the board. */
MEMORY
{
  ROM (rx)  : ORIGIN = 0x1FFFE000, LENGTH = 4K
  RAM (rwx) : ORIGIN = 0x20000000, LENGTH = 0x450
}

ENTRY(reset);

SECTIONS
{
  .vector_table ORIGIN(ROM) : { KEEP(*(.vector_table)) } > ROM
  .text : { *(.text .text.*) } > ROM
  .rodata : { *(.rodata .rodata.*) } > ROM

  /* ram.rs: DATA is the ROM's .data at 0x20000000, BSS its zeroed rest. */
  .data : ALIGN(4)
  {
    _sdata = .;
    KEEP(*(.data.ram_data))
    *(.data .data.*)
    . = ALIGN(4);
    _edata = .;
  } > RAM AT > ROM
  _sidata = LOADADDR(.data);

  .bss (NOLOAD) : ALIGN(4)
  {
    _sbss = .;
    KEEP(*(.bss.ram_bss))
    *(.bss .bss.*)
    . = ALIGN(4);
    _ebss = .;
  } > RAM

  /DISCARD/ : { *(.ARM.exidx .ARM.exidx.* .ARM.extab.*) }
}

ASSERT(_sdata == 0x20000000 && _edata == 0x20000018, "DATA isn't the ROM's .data (0x20000000-0x20000017)");
ASSERT(_ebss == 0x2000024C, "BSS isn't the ROM's (0x20000018-0x2000024B)");
