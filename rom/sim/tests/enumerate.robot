*** Comments ***
Scenario 1: the ROM from cold, as the host sees it.

*** Settings ***
Resource            rom.resource
Test Setup          Start The ROM

*** Test Cases ***
The ROM Enumerates As The Vendor Bootloader
    [Documentation]    342d:dfa0, "Westberry Tech." / "WB Device in DFU Mode",
    ...    one vendor interface with bulk EP1 IN/OUT (notes.md "USB").
    # Not paused mid-enumeration: the host model can't resume inside a
    # control transfer.
    Wait For Log Entry    HOST: device 342d:dfa0, EP0 64 bytes    timeout=3
    Wait For Log Entry    HOST: string 2 = 'WB Device in DFU Mode'    timeout=1
    Wait For Log Entry    HOST: string 1 = 'Westberry Tech.'    timeout=1
    Wait For Enumeration
    Should Not Be In Log    HOST ERROR    timeout=0

GET_INFO Reports The Chip
    [Documentation]    01 00 -> 01 00 0d <version 04> <SYS_ID> <flash bytes> <sram bytes>.
    ...    SYS_ID and MEMSZ come from sim/peripherals/sys.py (0x3A50E980, and the
    ...    MEMSZ that gives 256 KB / 36 KB).
    Wait For Enumeration
    Bulk Send    0x01    0x00
    Reply Should Be    01 00 0d 04 80 e9 50 3a 00 00 04 00 00 90 00 00

An Unknown Command Is Invalid
    Wait For Enumeration
    Bulk Send    0x55    0x00
    Reply Should Be    55 ff
