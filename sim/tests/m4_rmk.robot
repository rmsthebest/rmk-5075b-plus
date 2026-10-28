*** Comments ***
M4: the RMK keyboard image (akko-rmk) on the USB host model, with the key
matrix, knob and Win/Mac switch driven through gpio.py and RMK's storage on
the SPI flash model (qspi.py). Every test starts from a blank flash unless it
says otherwise; the first boot then writes the default keymap, which takes
about 1.6 s before USB comes up.
"In DFU" means the CPU is parked in the ROM stub at 0x1FFFE008 (see sim/akko.resc).

*** Settings ***
Library             Process
Library             OperatingSystem

*** Variables ***
${SIM}              ${CURDIR}/..
${RMK}              ${CURDIR}/../../target/thumbv7m-none-eabi/release/akko-rmk
${ROM_PARKED}       0x1FFFE008
${HOST_CMD}         0xF00
${HOST_STATE}       0xF04
${HOST_OUT_BYTE}    0xF0C
${GPIO_KEY}         0x40000FF0
${GPIO_DRIVE}       0x40000FF4
${GPIO_RELEASE}     0x40000FF8
${GPIOC_ODR}        0x40000814
${CAPS_LED}         0x20
${GPIOB_ODR}        0x40000414
${WIN_LOCK_LED}     0x01
${FLASH_ADDR}       0xF0
${FLASH_WORD}       0xF4
${FLASH_ERASE}      0xF8
${AIRCR}            0xE000ED0C
${SYSRESETREQ}      0x05FA0004
${VIAL_OUT_EP}      2
${USB_TRACE}        0xF10

*** Keywords ***
Load Machine
    Create Log Tester    3
    Execute Command    include @${SIM}/akko.resc
    # The scan reconfigures 15 pins every millisecond; logging each write
    # slows the simulation twentyfold.
    Execute Command    logLevel 3 gpio

Start
    Execute Command    sysbus LoadELF @${RMK}
    Execute Command    machine Reset

Boot
    [Documentation]    From a blank SPI flash unless ${blank} is false.
    [Arguments]    ${blank}=${True}
    Load Machine
    IF    ${blank}    Execute Command    qspi WriteDoubleWord ${FLASH_ERASE} 1
    Start

Run For
    [Arguments]    ${seconds}
    Execute Command    emulation RunFor "${seconds}"

Host
    [Arguments]    ${cmd}
    Execute Command    usb WriteDoubleWord ${HOST_CMD} ${cmd}

Host State Should Be
    [Arguments]    ${expected}
    ${v}=    Execute Command    usb ReadDoubleWord ${HOST_STATE}
    Should Be Equal As Integers    ${v.strip()}    ${expected}

Wait For Enumeration
    Wait For Log Entry    HOST: configured    timeout=4    pauseEmulation=true
    # Let the start-up switch tap and its release go through.
    Run For    0.05

Boot And Enumerate
    Boot
    Wait For Enumeration

Key
    [Arguments]    ${row}    ${col}    ${down}
    ${v}=    Evaluate    (${down} << 16) | (${row} << 8) | ${col}
    Execute Command    sysbus WriteDoubleWord ${GPIO_KEY} ${v}

Tap
    [Arguments]    ${row}    ${col}
    Key    ${row}    ${col}    1
    Run For    0.03
    Key    ${row}    ${col}    0
    Run For    0.03

Drive
    [Documentation]    ${level}: 0 low, 1 high, 2 released.
    [Arguments]    ${port}    ${pin}    ${level}
    ${v}=    Evaluate    (${level} << 16) | (${port} << 8) | ${pin}
    Execute Command    sysbus WriteDoubleWord ${GPIO_DRIVE} ${v}

Knob Step
    [Documentation]    One detent through the quadrature states, A = B7, B = B6,
    ...    ${edge} seconds apart. ${a_first}: A leads (clockwise, as QMK
    ...    decodes this knob).
    [Arguments]    ${a_first}    ${edge}=0.002
    ${first}    ${second}=    Set Variable If    ${a_first}    ${{(7, 6)}}    ${{(6, 7)}}
    Drive    1    ${first}    0
    Run For    ${edge}
    Drive    1    ${second}    0
    Run For    ${edge}
    Drive    1    ${first}    2
    Run For    ${edge}
    Drive    1    ${second}    2
    Run For    ${edge}

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
    Host    ${{ 0x09 | (${VIAL_OUT_EP} << 8) }}

No Panic Since Start-Up
    [Documentation]    Vial channel 0xD2 (the previous run's panic message) is empty.
    ...    A panic resets into the image at once, which takes boot-guard's
    ...    record, so the record itself can't be checked after the fact.
    Raw HID Send    0x08    0xD2    0x00
    HID Should Show    3    08 d2 00 00

Win Lock LED Should Be
    [Documentation]    PB0, active low.
    [Arguments]    ${state}
    ${v}=    Execute Command    sysbus ReadDoubleWord ${GPIOB_ODR}
    ${on}=    Evaluate    (${v.strip()} & ${WIN_LOCK_LED}) == 0
    Should Be Equal    ${on}    ${{ "${state}" == "on" }}

PC Should Be In Flash
    ${pc}=    Execute Command    cpu PC
    ${pc}=    Convert To Integer    ${pc.strip()}
    Should Be True    0x08000000 <= ${pc} < 0x08040000    PC ${pc} is not in flash

PC Should Be In ROM
    ${pc}=    Execute Command    cpu PC
    Should Be Equal As Integers    ${pc.strip()}    ${ROM_PARKED}

Paint Stack
    [Documentation]    Fills the stack (RAM below _stack_start, cortex-m-rt puts it at
    ...    the bottom) with 0xAA before the CPU runs, for Stack Depth.
    ${top}=    Symbol    _stack_start
    ${len}=    Evaluate    ${top} - 0x20000000
    ${file}=    Set Variable    ${TEMPDIR}/akko-stack-paint.bin
    Evaluate    open($file, "wb").write(b"\\xAA" * ${len})
    Execute Command    sysbus LoadBinary @${file} 0x20000000

Stack Depth
    [Documentation]    Bytes of stack used so far: the lowest word that isn't paint.
    ${top}=    Symbol    _stack_start
    ${a}=    Set Variable    ${0x20000000}
    WHILE    ${a} < ${top}
        ${v}=    Execute Command    sysbus ReadDoubleWord ${a}
        IF    int("${v.strip()}", 0) != 0xAAAAAAAA    BREAK
        ${a}=    Evaluate    ${a} + 16
    END
    RETURN    ${{ ${top} - ${a} }}

Symbol
    [Documentation]    Its address in the RMK image. (Run Process hangs on nm's output here.)
    [Arguments]    ${name}
    ${addr}=    Evaluate    [int(l.split()[0], 16) for l in subprocess.run(["arm-none-eabi-nm", $RMK], capture_output=True, text=True).stdout.splitlines() if l.endswith(" " + $name)][0]
    ...    modules=subprocess
    RETURN    ${addr}

Flash Byte
    [Arguments]    ${addr}
    Execute Command    qspi WriteDoubleWord ${FLASH_ADDR} ${addr}
    ${v}=    Execute Command    qspi ReadDoubleWord ${FLASH_WORD}
    ${b}=    Evaluate    int("${v.strip()}", 0) & 0xFF
    RETURN    ${b}

*** Test Cases ***
Enumerates As The QMK Keyboard With Vial
    Boot
    # Not paused here: pausing in the middle of enumeration trips the USB
    # model (an index error in its next tick after the resume).
    Wait For Log Entry    HOST: string 2 = 'AKKO 5075B Plus (RMK)'    timeout=4
    Wait For Log Entry    HOST: configured: if 0 class 03 eps [81 01], if 1 class 03 eps [82], if 2 class 03 eps [83 02]
    ...    timeout=4    pauseEmulation=true
    Host State Should Be    2
    Should Not Be In Log    HOST ERROR    treatAsRegex=false

A Key Press Sends A Keyboard Report
    Boot And Enumerate
    Key    3    1    1
    HID Should Show    1    00 00 04 00 00 00 00 00
    # No longer the scan's diagnostic: off, as in QMK without Win lock.
    Win Lock LED Should Be    off
    Key    3    1    0
    HID Should Show    1    00 00 00 00 00 00 00 00

Endpoints Are Driven As ChibiOS Drives Them
    [Documentation]    QMK's ChibiOS driver, which works on the board, clears
    ...    INCSR2/OUTCSR2 when it opens an endpoint and starts each IN packet with
    ...    a plain write of InPktRdy. musb left both CSR2s as they were and set
    ...    InPktRdy with a read-modify-write; keyboard reports on EP1 never
    ...    reached the host on the board (the knob on EP2 did).
    Load Machine
    Execute Command    qspi WriteDoubleWord ${FLASH_ERASE} 1
    Start
    # After the reset, which re-initialises the USB model.
    Execute Command    usb WriteDoubleWord ${USB_TRACE} 1
    # In the order embassy-usb opens them: keyboard EP1 IN and OUT, composite
    # EP2 IN, Vial EP3 IN and EP2 OUT.
    FOR    ${write}    IN    i1 +0x12    i1 +0x15    i2 +0x12    i3 +0x12    i2 +0x15
        # Not paused: these come during SET_CONFIGURATION (see sim/README.md).
        Wait For Log Entry    USBT W ${write} <- 0x00    timeout=4
    END
    Wait For Enumeration
    Key    3    1    1
    Wait For Log Entry    USBT W i1 +0x11 <- 0x01    timeout=1    pauseEmulation=true
    HID Should Show    1    00 00 04 00 00 00 00 00
    Key    3    1    0
    HID Should Show    1    00 00 00 00 00 00 00 00
    Should Not Be In Log    USBT W i1 +0x11 <- 0x03

Fn Layer Sends Consumer Keys
    Boot And Enumerate
    Key    5    10    1
    Run For    0.02
    Tap    0    6
    Wait For Log Entry    HOST HID 2: .*cd 00    timeout=1    treatAsRegex=true    pauseEmulation=true
    Key    5    10    0

The Knob Turns The Volume
    Boot And Enumerate
    Knob Step    ${True}
    Wait For Log Entry    HOST HID 2: .*e9 00    timeout=1    treatAsRegex=true    pauseEmulation=true
    Knob Step    ${False}
    Wait For Log Entry    HOST HID 2: .*ea 00    timeout=1    treatAsRegex=true    pauseEmulation=true

A Fast Turn Keeps Every Step
    [Documentation]    Three detents in 3 ms, faster than a step's 5 ms tap:
    ...    the steps queue and each one is sent.
    Boot And Enumerate
    FOR    ${i}    IN RANGE    3
        Knob Step    ${False}    0.00025
    END
    FOR    ${i}    IN RANGE    3
        Wait For Log Entry    HOST HID 2: 03 ea 00    timeout=2    pauseEmulation=true
        Wait For Log Entry    HOST HID 2: 03 00 00    timeout=2    pauseEmulation=true
    END

The Mac Switch Swaps Gui And Alt
    Boot And Enumerate
    Key    5    1    1
    HID Should Show    1    08 00 00 00 00 00 00 00
    Key    5    1    0
    Run For    0.03
    Drive    2    14    0
    Run For    0.05
    Key    5    1    1
    HID Should Show    1    04 00 00 00 00 00 00 00
    Key    5    1    0
    Run For    0.03
    # Back to Win.
    Drive    2    14    2
    Run For    0.05
    Key    5    1    1
    HID Should Show    1    08 00 00 00 00 00 00 00

The Caps Lock LED Follows The Host
    Boot And Enumerate
    # RMK's LED reader fails its first read (before configuration) and then
    # sleeps a second before it reads again.
    Run For    1.1
    Host    0x0203
    Run For    0.05
    ${v}=    Execute Command    sysbus ReadDoubleWord ${GPIOC_ODR}
    Should Be Equal As Integers    ${{ int("${v.strip()}", 0) & ${CAPS_LED} }}    0
    Host    0x0003
    Run For    0.05
    ${v}=    Execute Command    sysbus ReadDoubleWord ${GPIOC_ODR}
    Should Be Equal As Integers    ${{ int("${v.strip()}", 0) & ${CAPS_LED} }}    ${CAPS_LED}

Fn Right Shift Esc Enters DFU
    Boot And Enumerate
    Key    5    10    1
    Run For    0.02
    Key    4    12    1
    Run For    0.02
    Key    0    0    1
    Run For    0.03
    PC Should Be In ROM

Vial's Bootloader Button Enters DFU
    [Documentation]    VIA's id_bootloader_jump (0x0B), through RMK's
    ...    bootloader hook and boot-guard.
    Boot And Enumerate
    Raw HID Send    0x0B
    Run For    0.05
    PC Should Be In ROM

VIA Bulk Keymap Write Sets And Keeps Keys
    [Documentation]    VIA's DynamicKeymapSetBuffer (0x13), as its "load layout"
    ...    sends it: 28 bytes, 14 big-endian keycodes. RMK read 28 keycodes from
    ...    the 32-byte report (a panic) and little-endian. GetBuffer (0x12)
    ...    reads them back, the keys type them, and they survive a reset.
    Boot And Enumerate
    # Layer 0 row 0, from column 1: 14 keys of KC_B (0x0005).
    Raw HID Send    0x13    0x00    0x02    28    @{{ [0x00, 0x05] * 14 }}
    Run For    0.05
    No Panic Since Start-Up
    Raw HID Send    0x12    0x00    0x02    28
    HID Should Show    3    12 00 02 1c${{ " 00 05" * 14 }}
    Tap    0    1
    HID Should Show    1    00 00 05 00 00 00 00 00
    # Saved: give storage time, then reset without erasing the flash.
    Run For    1
    Execute Command    sysbus WriteDoubleWord ${AIRCR} ${SYSRESETREQ}
    Wait For Log Entry    HOST: configured    timeout=1    pauseEmulation=true
    Run For    0.05
    Tap    0    14
    HID Should Show    1    00 00 05 00 00 00 00 00

Odd Host Requests Don't Crash USB
    [Documentation]    CLEAR_FEATURE(ENDPOINT_HALT) for endpoints the chip doesn't
    ...    have (musb indexed its waker arrays with the host's number and
    ...    panicked), and a control read abandoned after its SETUP (SetupEnd).
    ...    USB keeps working without a reset: no panic was recorded.
    Boot And Enumerate
    Host    ${{ 0x0A | (0x85 << 8) }}
    Wait For Log Entry    HOST: CLEAR_FEATURE(ENDPOINT_HALT, 0x85) -> ok    timeout=1    pauseEmulation=true
    Host    ${{ 0x0A | (0x0F << 8) }}
    Wait For Log Entry    HOST: CLEAR_FEATURE(ENDPOINT_HALT, 0x0f) -> ok    timeout=1    pauseEmulation=true
    # The status stage completes before the request is handled, so "ok"
    # doesn't rule out a panic after it.
    Run For    0.05
    No Panic Since Start-Up
    Host    0x0B
    Wait For Log Entry    HOST: GET_DESCRIPTOR(device, after an aborted transfer) -> 18 bytes    timeout=1    pauseEmulation=true
    Host State Should Be    2
    Tap    3    1
    HID Should Show    1    00 00 04 00 00 00 00 00
    No Panic Since Start-Up

A Short SETUP Is Dropped
    [Documentation]    A SETUP that isn't 8 bytes: musb's setup() looped on it
    ...    without acking RxPktRdy, so the CPU spun until the watchdog reset.
    ...    Now it is acked and stalled, and the next request is answered.
    ...    The host waits 2.5 s before that request, past the watchdog.
    Boot And Enumerate
    Host    0x0D
    Wait For Log Entry    HOST: GET_DESCRIPTOR(device, after a short SETUP) -> 18 bytes    timeout=4    pauseEmulation=true
    Host State Should Be    2
    Tap    3    1
    HID Should Show    1    00 00 04 00 00 00 00 00
    No Panic Since Start-Up

Esc At A Cold Start Enters DFU
    Boot
    Key    0    0    1
    Run For    0.01
    PC Should Be In ROM

Storage Stays Out Of QMK's Block
    Load Machine
    Execute Command    qspi WriteDoubleWord ${FLASH_ERASE} 1
    # A marker in each 4 KB sector of the QMK block.
    FOR    ${a}    IN RANGE    0    0x10000    0x1000
        Execute Command    qspi WriteDoubleWord ${FLASH_ADDR} ${a}
        Execute Command    qspi WriteDoubleWord ${FLASH_WORD} ${{ 0xA5000000 | ${a} }}
    END
    Start
    Wait For Enumeration
    FOR    ${a}    IN RANGE    0    0x10000    0x1000
        Execute Command    qspi WriteDoubleWord ${FLASH_ADDR} ${a}
        ${v}=    Execute Command    qspi ReadDoubleWord ${FLASH_WORD}
        Should Be Equal As Integers    ${v.strip()}    ${{ 0xA5000000 | ${a} }}
    END
    Should Not Be In Log    flash: (CE|(SE|BE|PP) 0x00)    treatAsRegex=true
    # RMK's sectors are its whole window, 0x10000-0x1FFFF.
    ${b}=    Flash Byte    0x10000
    Should Not Be Equal As Integers    ${b}    0xFF

A Keymap Change Through Vial Survives A Reset
    Boot And Enumerate
    # VIA protocol version.
    Raw HID Send    0x01
    HID Should Show    3    01 00 09
    # dynamic_keymap_set_keycode(layer 0, row 3, col 1) = B, then read it back.
    Raw HID Send    0x05    0    3    1    0x00    0x05
    Run For    0.05
    Raw HID Send    0x04    0    3    1
    HID Should Show    3    04 00 03 01 00 05
    Run For    0.5
    Execute Command    sysbus WriteDoubleWord ${AIRCR} ${SYSRESETREQ}
    Wait For Log Entry    HOST: configured    timeout=1    pauseEmulation=true
    Run For    0.05
    Key    3    1    1
    HID Should Show    1    00 00 05 00 00 00 00 00

The Scan Reports Its Counters Over Vial
    [Documentation]    VIA id_custom_get_value on channel 0xD1 answers with the
    ...    scan's diagnostics (src/scan.rs fill_diag): a pass every 1 ms (on
    ...    the board, a TIM2 time base made it one per 65.5 ms), one debounced
    ...    event, and the held key in the readings since the last query.
    Boot And Enumerate
    Raw HID Send    0x08    0xD1
    ${first}=    Wait For Log Entry    HOST HID 3: 08 d1 .*    timeout=1    treatAsRegex=true    pauseEmulation=true
    Run For    0.3
    Key    3    1    1
    Run For    0.02
    Raw HID Send    0x08    0xD1
    ${second}=    Wait For Log Entry    HOST HID 3: 08 d1( [0-9a-f]{2}){12} 01 00 00 00( [0-9a-f]{2}){2} 00 00 00 02 00 00 00 00 00 00 00 00    timeout=1    treatAsRegex=true    pauseEmulation=true
    ${a}=    Evaluate    bytes.fromhex($first.split("HOST HID 3: ")[1].strip())
    ${b}=    Evaluate    bytes.fromhex($second.split("HOST HID 3: ")[1].strip())
    ${passes}=    Evaluate    int.from_bytes($b[2:6], "little") - int.from_bytes($a[2:6], "little")
    Should Be True    ${passes} >= 290    only ${passes} scan passes in 0.32 s
    ${gap}=    Evaluate    int.from_bytes($b[18:20], "little")
    Should Be True    ${gap} <= 25    longest gap between passes ${gap / 10} ms
    # Other channels stay unanswered.
    Raw HID Send    0x08    0x01
    HID Should Show    3    08 01 00 00 00 00

Vial Edits Don't Hold Up The Scan
    [Documentation]    300 key edits through Vial, each saved to the SPI flash
    ...    by RMK's storage (which fills a sector and moves to the next),
    ...    while the scan's longest gap between passes stays short. Storage
    ...    reads are polled at 3 MHz; uncached, it walked whole sectors per
    ...    edit without letting the scan run.
    Boot And Enumerate
    Run For    1
    Raw HID Send    0x08    0xD1
    Wait For Log Entry    HOST HID 3: 08 d1 .*    timeout=1    treatAsRegex=true    pauseEmulation=true
    FOR    ${i}    IN RANGE    300
        ${kc}=    Evaluate    0x04 + ${i} % 26
        Raw HID Send    0x05    ${{ ${i} % 2 }}    ${{ (${i} // 15) % 6 }}    ${{ ${i} % 15 }}    0x00    ${kc}
        Run For    0.01
    END
    Run For    1
    Raw HID Send    0x08    0xD1
    ${reply}=    Wait For Log Entry    HOST HID 3: 08 d1 .*    timeout=1    treatAsRegex=true    pauseEmulation=true
    ${b}=    Evaluate    bytes.fromhex($reply.split("HOST HID 3: ")[1].strip())
    ${gap}=    Evaluate    int.from_bytes($b[18:20], "little")
    Log To Console    longest scan gap during Vial edits: ${gap / 10} ms
    # 34.8 ms before the page caches and the read yields.
    Should Be True    ${gap} <= 50    longest gap between passes ${gap / 10} ms
    No Panic Since Start-Up

A Press Counts At Once, A Release After 5 ms
    [Documentation]    Eager press, deferred release (src/scan.rs): the report for
    ...    a press goes out within 3 ms (it took 5-6 ms with presses debounced),
    ...    and a 2 ms bounce while releasing doesn't release the key.
    Boot And Enumerate
    Key    3    1    1
    HID Should Show    1    00 00 04 00 00 00 00 00    timeout=0.003
    Run For    0.02
    Key    3    1    0
    Run For    0.002
    Key    3    1    1
    Run For    0.02
    Should Not Be In Log    HOST HID 1: 00 00 00 00 00 00 00 00
    Key    3    1    0
    HID Should Show    1    00 00 00 00 00 00 00 00    timeout=0.01

Presses Can Wait Out The Debounce, Set Over Vial
    [Documentation]    Channel 0xD6: 2 makes presses wait the 5 ms too (for a
    ...    chattering switch), 1 goes back; the setting is saved with the
    ...    lighting record and survives a reset.
    Boot And Enumerate
    Raw HID Send    0x08    0xD6    0x00
    HID Should Show    3    08 d6 00 00
    Raw HID Send    0x08    0xD6    0x02
    HID Should Show    3    08 d6 02 01
    Key    3    1    1
    Run For    0.003
    # timeout=0: look at what's logged so far, without running on.
    Should Not Be In Log    HOST HID 1: 00 00 04    timeout=0
    HID Should Show    1    00 00 04 00 00 00 00 00    timeout=0.01
    Key    3    1    0
    # Saved at the next lighting frame; then a reset keeps it.
    Run For    1
    Execute Command    sysbus WriteDoubleWord ${AIRCR} ${SYSRESETREQ}
    Wait For Log Entry    HOST: configured    timeout=2    pauseEmulation=true
    Run For    0.05
    Raw HID Send    0x08    0xD6    0x00
    HID Should Show    3    08 d6 00 01
    Raw HID Send    0x08    0xD6    0x01
    HID Should Show    3    08 d6 01 00

The Sleep Timeout Is Set Over Vial And Kept
    [Documentation]    Channel 0xD7 (tools/sleep.py): 0 reads the setting (7, 2
    ...    min, by default), 1 + n sets it to n; out of range is ignored. It's
    ...    saved with the lighting record and survives a reset.
    Boot And Enumerate
    Raw HID Send    0x08    0xD7    0x00
    HID Should Show    3    08 d7 00 07
    Raw HID Send    0x08    0xD7    0x03
    HID Should Show    3    08 d7 03 02
    Raw HID Send    0x08    0xD7    0x09
    HID Should Show    3    08 d7 09 02
    Run For    1
    Execute Command    sysbus WriteDoubleWord ${AIRCR} ${SYSRESETREQ}
    Wait For Log Entry    HOST: configured    timeout=2    pauseEmulation=true
    Run For    0.05
    Raw HID Send    0x08    0xD7    0x00
    HID Should Show    3    08 d7 00 02
    Raw HID Send    0x08    0xD7    0x01
    HID Should Show    3    08 d7 01 00

The Last Run's Panic Message Is Readable Over Vial
    [Documentation]    Channel 0xD2 answers with the message boot-guard kept from
    ...    the previous run's panic (record at 0x20007000), 28 bytes per request.
    Load Machine
    Execute Command    qspi WriteDoubleWord ${FLASH_ERASE} 1
    ${msg}=    Set Variable    firmware/src/scan.rs:1:1: a simulated panic
    ${record}=    Evaluate    (lambda m: (lambda h: [0x43, 0x49, 0x4E, 0x50] + list(len(m).to_bytes(4, "little")) + list(h.to_bytes(4, "little")) + list(m))(__import__("functools").reduce(lambda h, b: ((h ^ b) * 0x01000193) & 0xFFFFFFFF, len(m).to_bytes(4, "little") + m, 0x811C9DC5)))($msg.encode())
    FOR    ${i}    ${b}    IN ENUMERATE    @{record}
        Execute Command    sysbus WriteByte ${{ 0x20007000 + ${i} }} ${b}
    END
    Start
    Wait For Enumeration
    ${n}=    Get Length    ${msg}
    FOR    ${off}    IN RANGE    0    ${n}    28
        Raw HID Send    0x08    0xD2    ${off}
        ${part}=    Evaluate    $msg.encode()[${off}:${off} + 28].hex(" ")
        HID Should Show    3    08 d2 ${{ "%02x" % ${off} }} ${{ "%02x" % ${n} }} ${part}
    END

A Panic Resets Into The Keyboard, A Second One Enters DFU
    [Documentation]    boot-guard's crash policy, through the deliberate panic
    ...    on Vial channel 0xD3 (argument 'P'). The second panic comes well
    ...    within STABLE_AFTER (60 s), so it goes to DFU.
    Boot And Enumerate
    # Without the confirmation byte, nothing happens.
    Raw HID Send    0x08    0xD3    0x00
    HID Should Show    3    08 d3 00 00
    Raw HID Send    0x08    0xD3    0x50
    Run For    0.01
    PC Should Be In Flash
    Wait For Log Entry    HOST: configured    timeout=4    pauseEmulation=true
    Run For    0.05
    Raw HID Send    0x08    0xD2    0
    # The message starts "firmware/src/main.rs:".
    Wait For Log Entry    HOST HID 3: 08 d2 00 [0-9a-f]{2} ${{ b"firmware/src/main.rs:".hex(" ") }}    timeout=1    treatAsRegex=true    pauseEmulation=true
    Key    3    1    1
    HID Should Show    1    00 00 04 00 00 00 00 00
    Key    3    1    0
    Raw HID Send    0x08    0xD3    0x50
    Run For    0.01
    PC Should Be In ROM

A Hang Trips The Watchdog: A Reset, Then DFU
    [Documentation]    The software watchdog (src/watchdog.rs): Vial channel
    ...    0xD8 (argument 'H') hangs the executor; within 2-2.25 s TIM2's
    ...    check panics, with the watchdog's message, and boot-guard resets.
    ...    A second hang before the image is stable goes to DFU.
    Boot And Enumerate
    Raw HID Send    0x08    0xD8    0x00
    HID Should Show    3    08 d8 00 00
    Raw HID Send    0x08    0xD8    0x48
    Run For    1.9
    # Still hung: no reset yet (a reset would re-enumerate).
    Should Not Be In Log    HOST: configured    timeout=0
    Run For    0.5
    Wait For Log Entry    HOST: configured    timeout=4    pauseEmulation=true
    Run For    0.05
    Raw HID Send    0x08    0xD2    0
    # The message starts "firmware/src/watchdog.rs:LL:" (28 bytes with a
    # two-digit line), and the second part is "CC: watchdog: ...".
    Wait For Log Entry    HOST HID 3: 08 d2 00 [0-9a-f]{2} ${{ b"firmware/src/watchdog.rs:".hex(" ") }}    timeout=1    treatAsRegex=true    pauseEmulation=true
    Raw HID Send    0x08    0xD2    28
    Wait For Log Entry    HOST HID 3: 08 d2 1c [0-9a-f]{2} (3[0-9] )+${{ b": watchdog: ".hex(" ") }}    timeout=1    treatAsRegex=true    pauseEmulation=true
    Key    3    1    1
    HID Should Show    1    00 00 04 00 00 00 00 00
    Key    3    1    0
    Raw HID Send    0x08    0xD8    0x48
    Run For    2.5
    PC Should Be In ROM

The Stack Has Headroom
    [Documentation]    Stack high-water mark over first boot (default keymap
    ...    written to flash), typing, the knob and Vial; fails under 12 KB spare
    ...    (16.9 KB after the 2026-10-27 headroom pass; 4.9 KB before).
    Load Machine
    Execute Command    qspi WriteDoubleWord ${FLASH_ERASE} 1
    Start
    Paint Stack
    Wait For Enumeration
    Tap    3    1
    Key    5    10    1
    Tap    0    6
    Key    5    10    0
    Knob Step    ${True}
    Knob Step    ${False}
    Raw HID Send    0x01
    HID Should Show    3    01 00 09
    Raw HID Send    0x05    0    3    1    0x00    0x05
    Run For    0.05
    Raw HID Send    0x04    0    3    1
    HID Should Show    3    04 00 03 01 00 05
    Run For    0.5
    ${used}=    Stack Depth
    ${top}=    Symbol    _stack_start
    ${size}=    Evaluate    ${top} - 0x20000000
    Log To Console    stack: ${used} of ${size} bytes used
    Should Be True    ${size} - ${used} >= 12288    only ${{ ${size} - ${used} }} bytes of stack spare
