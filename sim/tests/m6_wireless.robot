*** Comments ***
M6: the radio module on UART3 (src/radio.rs, the wireless crate). The
module is sim/peripherals/uart3.py: it logs every frame the keyboard sends
("radio: <- ..."), ACKs it, answers battery inquiries, and sends what a
test tells it to ("radio: -> ..."). The frames' bytes are the fixed QMK
tree's (the wireless crate's tests check them against module.c); these
tests check the firmware around them: when frames go out, the pairing
keys, where the reports go, and what's kept across a reset.

*** Settings ***
Library             Process
Library             OperatingSystem

*** Variables ***
${SIM}              ${CURDIR}/..
${RMK}              ${CURDIR}/../../target/thumbv7m-none-eabi/release/akko-rmk
${HOST_CMD}         0xF00
${HOST_OUT_BYTE}    0xF0C
${VIAL_OUT_EP}      2
${GPIO_KEY}         0x40000FF0
${GPIOB_ODR}        0x40000414
${GPIOC_ODR}        0x40000814
${USB_POWER}        0x02
${CAPS_LED}         0x20
${FLASH_ERASE}      0xF8
${AIRCR}            0xE000ED0C
${SYSRESETREQ}      0x05FA0004
${RADIO_FRAME}      0xE4
${RADIO_FLAGS}      0xE8
${RADIO_ERRORS}     0xEC
${RADIO_FRAMES}     0xF0
${NO_ACKS}          0x6401
${BT1}              2 3
${BT2}              2 4
${DONGLE}           2 6
${USB}              2 7
${KEY_A}            3 1
# Frames, as uart3.py logs them.
${SEL_USB}          a6 11 b7
${SEL_DONGLE}       a6 30 d6
${SEL_BT2}          a6 32 d8
${ALL_UP}           a1 00 00 00 00 00 00 00 00 a1
${A_DOWN}           a1 00 00 04 00 00 00 00 00 a5
${CONNECTED}        0x325B
${DISCONNECTED}     0x335B
${CAPS_ON}          0x025A
${LED_SELECT}       0xF8
${LED_COLOUR}       0xFC
${LED_2G4}          38
${RED}              0xFF0000
${GREEN}            0x00FF00

*** Keywords ***
Start
    [Documentation]    From a blank SPI flash; not waiting for anything.
    Create Log Tester    3
    Execute Command    include @${SIM}/akko.resc
    Execute Command    logLevel 3 gpio
    Execute Command    qspi WriteDoubleWord ${FLASH_ERASE} 1
    Execute Command    sysbus LoadELF @${RMK}
    Execute Command    machine Reset

Boot
    [Documentation]    Start, on until USB is configured (past the radio's
    ...    start-up frames, which go out 100 ms in).
    Start
    Wait For Log Entry    HOST: configured    timeout=4    pauseEmulation=true
    Run For    0.05

Run For
    [Arguments]    ${seconds}
    Execute Command    emulation RunFor "${seconds}"

Key
    [Arguments]    ${pos}    ${down}
    ${row}    ${col}=    Evaluate    "${pos}".split()
    ${v}=    Evaluate    (${down} << 16) | (${row} << 8) | ${col}
    Execute Command    sysbus WriteDoubleWord ${GPIO_KEY} ${v}

Tap
    [Arguments]    ${pos}
    Key    ${pos}    1
    Run For    0.03
    Key    ${pos}    0
    Run For    0.03

Fn Hold
    [Documentation]    Fn held, ${pos} held ${seconds}, both released.
    [Arguments]    ${pos}    ${seconds}=0.03
    Key    5 10    1
    Run For    0.03
    Key    ${pos}    1
    Run For    ${seconds}
    Key    ${pos}    0
    Run For    0.03
    Key    5 10    0
    Run For    0.03

Radio Should Get
    [Arguments]    ${hex}    ${timeout}=1
    Wait For Log Entry    radio: <- ${hex}    timeout=${timeout}    pauseEmulation=true

Radio Should Not Get
    [Arguments]    ${hex}
    Should Not Be In Log    radio: <- ${hex}    timeout=0

Module Sends
    [Documentation]    A module frame "cmd arg sum", ${frame} = cmd | arg << 8.
    [Arguments]    ${frame}
    Execute Command    uart3 WriteDoubleWord ${RADIO_FRAME} ${frame}

Radio
    [Arguments]    ${offset}
    ${v}=    Execute Command    uart3 ReadDoubleWord ${offset}
    RETURN    ${{ int($v.strip(), 0) }}

No Radio Errors
    ${e}=    Radio    ${RADIO_ERRORS}
    Should Be Equal As Integers    ${e}    0

USB Pull-up Should Be
    [Arguments]    ${on}
    ${v}=    Execute Command    sysbus ReadDoubleWord ${GPIOB_ODR}
    # B1 low: the pull-up is on.
    Should Be Equal    ${{ not (int($v.strip(), 0) & ${USB_POWER}) }}    ${on}

Caps LED Should Be
    [Arguments]    ${on}
    ${v}=    Execute Command    sysbus ReadDoubleWord ${GPIOC_ODR}
    Should Be Equal    ${{ not (int($v.strip(), 0) & ${CAPS_LED}) }}    ${on}

On The Dongle
    [Documentation]    Fn+Y, then the module reports the link up.
    Fn Hold    ${DONGLE}
    Radio Should Get    ${SEL_DONGLE}
    Module Sends    ${CONNECTED}
    Radio Should Get    ${ALL_UP}

LED
    [Documentation]    LED ${i}'s colour in the last frame, 0xRRGGBB.
    [Arguments]    ${i}
    Execute Command    spim2 WriteDoubleWord ${LED_SELECT} ${i}
    ${v}=    Execute Command    spim2 ReadDoubleWord ${LED_COLOUR}
    RETURN    ${{ int($v.strip(), 0) }}

LED Colours Over
    [Documentation]    LED ${i}'s colours, every 50 ms for ${seconds}.
    [Arguments]    ${i}    ${seconds}
    @{seen}=    Create List
    FOR    ${_}    IN RANGE    ${{ int(${seconds} / 0.05) }}
        Run For    0.05
        ${c}=    LED    ${i}
        Evaluate    $seen.append($c)
    END
    RETURN    ${seen}

Raw HID Send
    [Documentation]    One 32-byte packet on the Vial OUT endpoint, ${bytes} then zeros.
    [Arguments]    @{bytes}
    ${padded}=    Evaluate    [int(str(b), 0) for b in $bytes] + [0] * (32 - len($bytes))
    FOR    ${b}    IN    @{padded}
        Execute Command    usb WriteDoubleWord ${HOST_OUT_BYTE} ${b}
    END
    Execute Command    usb WriteDoubleWord ${HOST_CMD} ${{ 0x09 | (${VIAL_OUT_EP} << 8) }}
    Run For    0.05

*** Test Cases ***
Start-up Talks To The Module As QMK Does
    [Documentation]    100 ms in (wireless_post_task): the module's version, its
    ...    30 min sleep in BT and 2.4G mode, then the saved device (USB on a
    ...    blank flash). The charge state (no cable: stopped) and, every 3 s,
    ...    a battery inquiry, which the module answers. UART3 is set up as
    ...    QMK's (uart3.py checks the divider and line settings).
    Start
    # On a blank flash RMK writes the default keymap first (~2.8 s).
    Radio Should Get    a6 70 16    timeout=5
    Radio Should Get    a6 55 fb
    Radio Should Get    a6 57 fd
    Radio Should Get    ${SEL_USB}
    Radio Should Get    a6 65 0b
    Radio Should Get    a6 53 f9    timeout=4
    Wait For Log Entry    radio: -> 5c 64 c0    timeout=0.1    pauseEmulation=true
    USB Pull-up Should Be    ${True}
    No Radio Errors

Fn Y Moves The Keys To The Dongle
    [Documentation]    Fn+Y selects 2.4G and cuts USB's pull-up; nothing is
    ...    typed until the module says it's connected, then the keyboard's
    ...    current state goes first, and keys go to the module, not USB.
    Boot
    Run For    0.2
    Fn Hold    ${DONGLE}
    Radio Should Get    ${SEL_DONGLE}
    USB Pull-up Should Be    ${False}
    Key    ${KEY_A}    1
    Run For    0.05
    Radio Should Not Get    a1
    Module Sends    ${CONNECTED}
    Radio Should Get    ${A_DOWN}
    Key    ${KEY_A}    0
    Radio Should Get    ${ALL_UP}
    Should Not Be In Log    HOST HID 1: 00 00 04    timeout=0
    No Radio Errors

A Release While The Link Is Down Reaches The Host On Reconnect
    [Documentation]    Fixed-tree bug 1: the module drops out with A held, A is
    ...    released, the link comes back: the host gets the release. The
    ...    device is re-selected while down, at most every 500 ms.
    Boot
    On The Dongle
    Key    ${KEY_A}    1
    Radio Should Get    ${A_DOWN}
    Module Sends    ${DISCONNECTED}
    Run For    0.05
    Key    ${KEY_A}    0
    Radio Should Get    ${SEL_DONGLE}
    Run For    0.3
    Module Sends    ${CONNECTED}
    Radio Should Get    ${ALL_UP}

Fn Y Held 3 s Pairs The Dongle Anew
    [Documentation]    The dongle's strings, VID/PID, clear, pair: the bytes of
    ...    wireless/tests/wire_board.txt. A shorter hold doesn't pair.
    Boot
    Run For    0.2
    Fn Hold    ${DONGLE}    2.5
    Radio Should Not Get    a6 51 f7
    Fn Hold    ${DONGLE}    3.1
    Radio Should Get    ab 0a 41 00 4b 00 4b 00 4f 00 00 00 db
    Radio Should Get    ac 22 41 00 6b 00 6b 00 6f 00 20 00 32 00 2e 00 34 00 47 00 20 00 56 00 49 00 41 00 20 00 4b 00 42 00 00 00 fc
    Radio Should Get    ad 2d 34 f6 e4 e8
    Radio Should Get    a6 52 f8
    Radio Should Get    a6 51 f7
    No Radio Errors

Fn U Goes Back To USB
    [Documentation]    The module gets an all-up report, then the USB select;
    ...    the pull-up is back and keys reach USB.
    Boot
    On The Dongle
    Fn Hold    ${USB}
    Radio Should Get    ${SEL_USB}
    USB Pull-up Should Be    ${True}
    Tap    ${KEY_A}
    Wait For Log Entry    HOST HID 1: 00 00 04 00 00 00 00 00    timeout=1    pauseEmulation=true

The Host's Caps Lock Comes From The Module
    Boot
    On The Dongle
    Module Sends    ${CAPS_ON}
    Run For    0.1
    Caps LED Should Be    ${True}
    Module Sends    ${DISCONNECTED}
    Run For    0.1
    Caps LED Should Be    ${False}

The Selected Device Survives A Reset
    [Documentation]    Fn+R (BT2), saved with the lighting record; after a reset
    ...    start-up selects BT2 and keeps USB's pull-up off.
    Boot
    Run For    0.2
    Fn Hold    ${BT2}
    Radio Should Get    ${SEL_BT2}
    Wait For Log Entry    flash: PP 0x02    timeout=4    pauseEmulation=true
    Run For    0.2
    Execute Command    sysbus WriteDoubleWord ${AIRCR} ${SYSRESETREQ}
    Radio Should Get    a6 70 16    timeout=6
    Radio Should Get    ${SEL_BT2}
    Run For    0.1
    USB Pull-up Should Be    ${False}

An Unanswered Frame Is Dropped After 41 Tries
    [Documentation]    Fixed-tree bug 2: with the module silent, a frame is sent
    ...    41 times (10 ms apart), dropped, and counted; Vial channel 0xD4
    ...    reads the counters (tx, ACK timeouts, retry drops, ...).
    Boot
    Run For    0.2
    Execute Command    uart3 WriteDoubleWord ${RADIO_FLAGS} ${NO_ACKS}
    ${before}=    Radio    ${RADIO_FRAMES}
    Fn Hold    ${BT1}
    Run For    0.6
    ${after}=    Radio    ${RADIO_FRAMES}
    Should Be True    ${after} - ${before} >= 41
    Raw HID Send    0x08    0xD4
    ${line}=    Wait For Log Entry    HOST HID 3: 08 d4 .*    timeout=1    treatAsRegex=true    pauseEmulation=true
    ${b}=    Evaluate    bytes.fromhex($line.split("HOST HID 3: ")[1].strip())
    ${drops}=    Evaluate    int.from_bytes($b[6:8], "little")
    ${timeouts}=    Evaluate    int.from_bytes($b[4:6], "little")
    Should Be True    ${drops} >= 1    no retry drop counted
    Should Be True    ${timeouts} >= 41    only ${timeouts} ACK timeouts

The Dongle's Key Blinks Until Connected, Then Shows Green For 2 s
    [Documentation]    The connection indicator (LEDs 37-41) follows the radio:
    ...    after Fn+Y, Y's LED blinks (500 ms, red: the vendor's PAIR colour
    ...    in QMK's g r b order) for as long as the link is down; CONNECTED
    ...    shows it green (HS_PAIR_COLOR_2G4) for 2 s, then the effect is back.
    Boot
    Run For    0.2
    Fn Hold    ${DONGLE}
    Radio Should Get    ${SEL_DONGLE}
    ${seen}=    LED Colours Over    ${LED_2G4}    2.5
    Should Contain    ${seen}    ${{ ${RED} }}
    Should Contain    ${seen}    ${0}
    Should Not Contain    ${seen}    ${{ ${GREEN} }}
    Module Sends    ${CONNECTED}
    Run For    0.1
    ${c}=    LED    ${LED_2G4}
    Should Be Equal As Integers    ${c}    ${GREEN}
    ${seen}=    LED Colours Over    ${LED_2G4}    1.6
    Should Not Contain    ${seen}    ${{ ${RED} }}
    Run For    0.5
    ${seen}=    LED Colours Over    ${LED_2G4}    0.5
    Should Not Contain    ${seen}    ${{ ${GREEN} }}
    Should Not Contain    ${seen}    ${{ ${RED} }}
