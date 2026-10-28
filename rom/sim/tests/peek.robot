*** Comments ***
The rom-peek feature (firmware/src/main.rs `peek`, rom/tools/rompeek.py) on
the board sim: akko-rmk built with --features rom-peek into target/rom-peek
(rom/sim/run.sh builds it). The keywords are the few this needs from
sim/tests/m4_rmk.robot.

*** Settings ***
Library             Process

*** Variables ***
${SIM}              ${CURDIR}/../../../sim
${RMK}              ${CURDIR}/../../../target/rom-peek/thumbv7m-none-eabi/release/akko-rmk
${HOST_CMD}         0xF00
${HOST_OUT_BYTE}    0xF0C
${FLASH_ERASE}      0xF8
${VIAL_OUT_EP}      2

*** Keywords ***
Boot And Enumerate
    [Documentation]    From a blank SPI flash, as m4_rmk.robot.
    Create Log Tester    3
    Execute Command    include @${SIM}/akko.resc
    Execute Command    logLevel 3 gpio
    Execute Command    qspi WriteDoubleWord ${FLASH_ERASE} 1
    Execute Command    sysbus LoadELF @${RMK}
    Execute Command    machine Reset
    Wait For Log Entry    HOST: configured    timeout=4    pauseEmulation=true
    Execute Command    emulation RunFor "0.05"

HID Should Show
    [Arguments]    ${ep}    ${hex}    ${timeout}=1
    Wait For Log Entry    HOST HID ${ep}: ${hex}    timeout=${timeout}    pauseEmulation=true

Raw HID Send
    [Documentation]    One 32-byte packet on the Vial OUT endpoint, ${bytes} then zeros.
    [Arguments]    @{bytes}
    ${padded}=    Evaluate    [int(str(b), 0) for b in $bytes] + [0] * (32 - len($bytes))
    FOR    ${b}    IN    @{padded}
        Execute Command    usb WriteDoubleWord ${HOST_OUT_BYTE} ${b}
    END
    Execute Command    usb WriteDoubleWord ${HOST_CMD} ${{ 0x09 | (${VIAL_OUT_EP} << 8) }}

No Panic Since Start-Up
    Raw HID Send    0x08    0xD2    0x00
    HID Should Show    3    08 d2 00 00

Peek Should Show
    [Documentation]    Vial channel 0xDA (main.rs `peek`) at ${addr}: the answer
    ...    echoes the address, then ${status} and ${count}, then ${count}
    ...    bytes from sim memory, then zeros.
    [Arguments]    ${addr}    ${status}    ${count}
    ${a}=    Evaluate    (${addr}).to_bytes(4, "little")
    Raw HID Send    0x08    0xDA    ${a[0]}    ${a[1]}    ${a[2]}    ${a[3]}
    ${words}=    Evaluate    [${addr} + i for i in range(0, ${count}, 4)]
    ${body}=    Set Variable    ${EMPTY}
    FOR    ${w}    IN    @{words}
        ${v}=    Execute Command    sysbus ReadDoubleWord ${w}
        ${body}=    Evaluate    $body + int($v.strip(), 0).to_bytes(4, "little").hex(" ") + " "
    END
    ${expected}=    Evaluate    ($a.hex(" ") + " %02x %02x " % (${status}, ${count}) + $body + "00 " * (24 - ${count})).strip()
    HID Should Show    3    08 da ${expected}

*** Test Cases ***
The ROM Peek Reads Only The ROM And The Information Blocks
    [Documentation]    Vial channel 0xDA (rom/tools/rompeek.py) reads the ROM
    ...    bootloader (0x1FFFE000-0x1FFFEFFF) and the information blocks
    ...    (0x1FFFF000-0x1FFFF7FF), 24 bytes at a time and no further than
    ...    the end of a range. An unaligned address or one outside both is
    ...    refused and nothing is read. The sim maps 0x1FFFE000-0x1FFFFFFF as
    ...    memory, so a pattern written there shows whether it was read.
    Boot And Enumerate
    FOR    ${addr}    IN    0x1FFFE100    0x1FFFE104    0x1FFFE108    0x1FFFE10C    0x1FFFE110    0x1FFFE114
    ...    0x1FFFEFF8    0x1FFFEFFC    0x1FFFF000    0x1FFFF7F0    0x1FFFF7F4    0x1FFFF7F8    0x1FFFF7FC    0x1FFFF800
        Execute Command    sysbus WriteDoubleWord ${addr} ${{ ${addr} ^ 0xA5C3E10F }}
    END
    Peek Should Show    0x1FFFE100    0    24
    # The last 8 bytes of the ROM; the information blocks are the next read.
    Peek Should Show    0x1FFFEFF8    0    8
    Peek Should Show    0x1FFFF7F0    0    16
    # Past the information blocks, unaligned, and SRAM: refused, nothing read.
    Peek Should Show    0x1FFFF800    1    0
    Peek Should Show    0x1FFFE101    1    0
    Peek Should Show    0x20000000    1    0
    No Panic Since Start-Up
