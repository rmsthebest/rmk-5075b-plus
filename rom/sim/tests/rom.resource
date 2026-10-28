*** Comments ***
Keywords for the ROM bootloader suites: start the real ROM
($ROM_WORK/dump/rom.bin, rom/README.md) on the Akko machine (rom/sim/rom.resc) and talk to it over its vendor bulk pipe
with the command set in notes.md ("DFU state machine"). All multi-byte fields
are little-endian.

*** Settings ***
Library             Collections

*** Variables ***
${ROMSIM}           ${CURDIR}/..
${DUMP}             %{ROM_WORK=${CURDIR}/../../../../wb32-rom-re}/dump
${HOST_CMD}         0xF00
${HOST_STATE}       0xF04
${HOST_OUT_BYTE}    0xF0C
${BULK_EP}          1
${APP}              0x08000000

*** Keywords ***
Start The ROM
    [Documentation]    Reset, then the ROM's SP and PC, as boot-guard's jump does.
    Create Log Tester    3
    Execute Command    $dump=@${DUMP}
    Execute Command    include @${ROMSIM}/rom.resc
    Execute Command    runMacro $enter_rom

Wait For Enumeration
    Wait For Log Entry    HOST: configured: if 0 class ff eps [81 01]    timeout=3    pauseEmulation=true

Le32
    [Arguments]    ${v}
    ${b}=    Evaluate    list((${v}).to_bytes(4, "little"))
    RETURN    ${b}

Bulk Send
    [Documentation]    One command on bulk OUT EP1: ${bytes}, split into 64-byte packets.
    [Arguments]    @{bytes}
    FOR    ${b}    IN    @{bytes}
        Execute Command    usb WriteDoubleWord ${HOST_OUT_BYTE} ${b}
    END
    Execute Command    usb WriteDoubleWord ${HOST_CMD} ${{ 0x0E | (${BULK_EP} << 8) }}

Reply Should Be
    [Documentation]    The next reply packet on bulk IN EP1.
    [Arguments]    ${hex}    ${timeout}=1
    Wait For Log Entry    HOST BULK 1: ${hex}    timeout=${timeout}    pauseEmulation=true

Command
    [Documentation]    <cmd> <sub> 00 00 <a LE> <b LE> [data]: the updater's layout.
    [Arguments]    ${cmd}    ${sub}=0    ${a}=0    ${b}=0    @{data}
    ${la}=    Le32    ${a}
    ${lb}=    Le32    ${b}
    ${all}=    Evaluate    [${cmd}, ${sub}, 0, 0] + $la + $lb + [int(str(x), 0) for x in $data]
    Bulk Send    @{all}

Hex
    [Arguments]    @{bytes}
    ${h}=    Evaluate    " ".join("%02x" % int(str(b), 0) for b in $bytes)
    RETURN    ${h}
