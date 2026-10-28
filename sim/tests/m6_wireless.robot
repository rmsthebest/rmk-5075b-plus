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
${GPIOA_ODR}        0x40000014
${GPIOB_ODR}        0x40000414
${GPIOC_MODER}      0x40000800
${GPIO_DRIVE}       0x40000FF4
${LED_POWER}        0x100
${LED_BOOST}        0x200
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
${HOST_SUSPEND}     0x0060
${HOST_RESUME}      0x0160
${LED_SELECT}       0xF8
${LED_COLOUR}       0xFC
${LED_2G4}          38
${RED}              0xFF0000
${GREEN}            0x00FF00
${SYST_CSR}         0xE000E010
${SCB_SCR}          0xE000ED10
${VERSION_ASK}      a6 70 16

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

Should Be Asleep
    [Documentation]    Asleep (src/power.rs): the LED supply and boost off,
    ...    the Caps LED off, and (unless ${lower}) the columns driven low:
    ...    C0-C3 are outputs (MODER 01).
    [Arguments]    ${asleep}    ${lower}=${False}
    ${a}=    Execute Command    sysbus ReadDoubleWord ${GPIOA_ODR}
    ${a}=    Evaluate    int($a.strip(), 0)
    Should Be Equal    ${{ not ($a & ${LED_BOOST}) }}    ${asleep}    boost
    IF    ${asleep}
        Should Be Equal    ${{ bool($a & ${LED_POWER}) }}    ${False}    LED power
        Caps LED Should Be    ${False}
    END
    ${moder}=    Execute Command    sysbus ReadDoubleWord ${GPIOC_MODER}
    ${driven}=    Evaluate    (int($moder.strip(), 0) & 0xFF) == 0x55
    Should Be Equal    ${driven}    ${{ ${asleep} and not ${lower} }}    columns driven

Set Sleep Timeout
    [Documentation]    Over Vial channel 0xD7: 0 never, 1 = 1 min, ..., 7 = 2 min.
    [Arguments]    ${index}
    Raw HID Send    0x08    0xD7    ${{ ${index} + 1 }}
    Wait For Log Entry    HOST HID 3: 08 d7 ${{ "%02x %02x" % (${index} + 1, ${index}) }}    timeout=1    pauseEmulation=true

Drive
    [Documentation]    Pin ${pin} of port ${port} (0 A, 1 B, 2 C): 0 low, 1 high, 2 released.
    [Arguments]    ${port}    ${pin}    ${level}
    Execute Command    sysbus WriteDoubleWord ${GPIO_DRIVE} ${{ (${level} << 16) | (${port} << 8) | ${pin} }}

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

Stop Mode
    [Documentation]    Vial channel 0xD9: 0x53 ('S') on, 1 off, 0 reads.
    [Arguments]    ${arg}
    Raw HID Send    0x08    0xD9    ${arg}

Should Be Stopped
    [Documentation]    In stop mode's WFI (src/stop.rs, wb32-hal stop.rs):
    ...    SysTick off and SLEEPDEEP set.
    [Arguments]    ${stopped}
    ${csr}=    Execute Command    sysbus ReadDoubleWord ${SYST_CSR}
    ${scr}=    Execute Command    sysbus ReadDoubleWord ${SCB_SCR}
    Should Be Equal    ${{ not (int($csr.strip(), 0) & 1) }}    ${stopped}    SysTick off
    Should Be Equal    ${{ bool(int($scr.strip(), 0) & 4) }}    ${stopped}    SLEEPDEEP

Stop Counts Should Be
    [Documentation]    0xD9's answer: on, the stops, those a key, the knob or
    ...    the cable ended.
    [Arguments]    ${stops}    ${wakes}
    Stop Mode    0
    ${hex}=    Evaluate    "08 d9 00 01 " + " ".join("%02x" % b for b in list(int(${stops}).to_bytes(4, "little")) + list(int(${wakes}).to_bytes(4, "little")))
    Wait For Log Entry    HOST HID 3: ${hex}    timeout=1    pauseEmulation=true

Asleep And Stopped
    [Documentation]    Stop mode on, on the dongle; the host suspends: asleep at
    ...    once, stopped a second later (STOP_AFTER_MS).
    Boot
    Stop Mode    0x53
    Wait For Log Entry    HOST HID 3: 08 d9 53 01    timeout=1    pauseEmulation=true
    On The Dongle
    Module Sends    ${HOST_SUSPEND}
    Run For    0.5
    Should Be Asleep    ${True}
    Should Be Stopped    ${False}
    Run For    1.2
    Should Be Stopped    ${True}
    # The vendor's PWR mode, STOP LP4 S32KON (from reset: CR0 0).
    Wait For Log Entry    sysctl: MMIO W PWR+0x000 = 0x0003b004    timeout=0
    Wait For Log Entry    sysctl: MMIO W PWR+0x020 = 0x000003b3    timeout=0

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
    ...    crates/qmk-wireless/tests/wire_board.txt. A shorter hold doesn't pair.
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

Pressing The Current Device's Key Again Selects It Again
    [Documentation]    As the vendor's WLS_KEYCODE_WAKEUP: Fn+Y on the dongle
    ...    selects it again, so the module reconnects (a kick for a link
    ...    that misbehaves).
    Boot
    On The Dongle
    Run For    0.5
    Fn Hold    ${DONGLE}
    Radio Should Get    ${SEL_DONGLE}

EE_CLR Keeps The Selected Device
    [Documentation]    As the vendor's hs_reset_settings (confinfo.devs kept):
    ...    Fn+` held 3 s on the dongle resets the settings, and after RMK's
    ...    reboot the keyboard is back on the dongle, not on USB.
    Boot
    On The Dongle
    Wait For Log Entry    flash: PP 0x02    timeout=4    pauseEmulation=true
    Key    5 10    1
    Run For    0.03
    Key    1 0    1
    Run For    3.05
    Key    1 0    0
    Run For    0.03
    Key    5 10    0
    Radio Should Get    ${VERSION_ASK}    timeout=6
    Radio Should Get    ${SEL_DONGLE}
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

Idle On The Dongle It Sleeps, And A Key Wakes It And Is Typed
    [Documentation]    Sleep timeout 1 min (src/power.rs): a minute without
    ...    input on the dongle, it sleeps: the lights' supply, the boost and
    ...    the Caps LED off, the columns driven low. A key wakes it; the key
    ...    reaches the host, the device is selected again, the lights come
    ...    back.
    Boot
    On The Dongle
    Module Sends    ${CAPS_ON}
    Set Sleep Timeout    1
    Run For    58
    Should Be Asleep    ${False}
    Caps LED Should Be    ${True}
    Run For    3
    Should Be Asleep    ${True}
    Key    ${KEY_A}    1
    Radio Should Get    ${A_DOWN}
    Radio Should Get    ${SEL_DONGLE}
    Key    ${KEY_A}    0
    Radio Should Get    ${ALL_UP}
    Run For    0.3
    Should Be Asleep    ${False}
    Caps LED Should Be    ${True}
    ${a}=    Execute Command    sysbus ReadDoubleWord ${GPIOA_ODR}
    Should Be True    int($a.strip(), 0) & ${LED_POWER}    the lights are back
    No Radio Errors

Never Means Never; Otherwise A Link Down For 30 s Sleeps
    [Documentation]    With the setting "never" the keyboard stays awake with the
    ...    link down; with the default (2 min) it sleeps 30 s after the link
    ...    went down, telling the module first (devctrl USB, as the vendor).
    Boot
    On The Dongle
    Set Sleep Timeout    0
    Module Sends    ${DISCONNECTED}
    Run For    35
    Should Be Asleep    ${False}
    Set Sleep Timeout    7
    Run For    0.5
    # Input (the Vial request isn't) restarted nothing: 35 s down already.
    Radio Should Get    ${SEL_USB}
    Run For    0.5
    Should Be Asleep    ${True}

Typing Doesn't Hold Off The Link-Down Sleep; The Host Resuming Wakes It
    [Documentation]    As the vendor's: with the link down, keys don't restart
    ...    the 30 s (only a device key would), so it sleeps 30 s after the
    ...    link went down. The host's resume frame wakes it from any sleep.
    Boot
    On The Dongle
    Module Sends    ${DISCONNECTED}
    Run For    10
    Tap    ${KEY_A}
    Run For    10
    Tap    ${KEY_A}
    Run For    10.5
    Radio Should Get    ${SEL_USB}
    Run For    0.5
    Should Be Asleep    ${True}
    Module Sends    ${HOST_RESUME}
    Radio Should Get    ${SEL_DONGLE}
    Run For    0.3
    Should Be Asleep    ${False}

The Host Suspending Sleeps It At Once, Its Resume Wakes It
    [Documentation]    The module's host state frame (0x60): 0 the host
    ...    suspended, 1 it resumed. Also with the setting "never". A long
    ...    sleep doesn't trip the watchdog.
    Boot
    On The Dongle
    Set Sleep Timeout    0
    Module Sends    ${HOST_SUSPEND}
    Run For    0.5
    Should Be Asleep    ${True}
    # Asleep, the scan runs at 10 ms: the watchdog (2 s) stays quiet. A
    # reset would have woken it.
    Run For    10
    Should Be Asleep    ${True}
    Module Sends    ${HOST_RESUME}
    Radio Should Get    ${SEL_DONGLE}
    Run For    0.3
    Should Be Asleep    ${False}

The Knob Wakes It, And The Turn Is Sent
    Boot
    On The Dongle
    Module Sends    ${HOST_SUSPEND}
    Run For    0.5
    Should Be Asleep    ${True}
    Drive    1    7    0
    Run For    0.002
    Drive    1    6    0
    Run For    0.002
    Drive    1    7    2
    Run For    0.002
    Drive    1    6    2
    # A consumer report through the module: a3, volume up (e9).
    Radio Should Get    a3 e9 00
    Run For    0.3
    Should Be Asleep    ${False}

A Flat Battery Sleeps It Until The Cable Goes In
    [Documentation]    At 0% for a minute (M7a's timer) the module is told and
    ...    the keyboard sleeps; keys don't wake it (the columns stay
    ...    released); the cable going in (A7 high) does.
    Boot
    Execute Command    uart3 WriteDoubleWord ${RADIO_FLAGS} 0
    On The Dongle
    Run For    62
    Radio Should Get    ${SEL_USB}    timeout=5
    Run For    0.5
    Should Be Asleep    ${True}    lower=${True}
    Key    ${KEY_A}    1
    Run For    0.3
    Key    ${KEY_A}    0
    Should Be Asleep    ${True}    lower=${True}
    Drive    0    7    1
    Run For    0.5
    Should Be Asleep    ${False}
    Drive    0    7    2

In Stop Mode A Flat Battery Stops It Until The Cable Goes In
    [Documentation]    Lower sleep stopped: only the cable and the UART are
    ...    armed. A key doesn't wake it; a frame from the module does, but
    ...    it's read and the keyboard stops again (the host resuming doesn't
    ...    end a flat battery's sleep); the cable going in ends it.
    Boot
    Stop Mode    0x53
    Wait For Log Entry    HOST HID 3: 08 d9 53 01    timeout=1    pauseEmulation=true
    Execute Command    uart3 WriteDoubleWord ${RADIO_FLAGS} 0
    On The Dongle
    Run For    62
    Radio Should Get    ${SEL_USB}    timeout=5
    Run For    1.5
    Should Be Asleep    ${True}    lower=${True}
    Should Be Stopped    ${True}
    Key    ${KEY_A}    1
    Run For    0.3
    Should Be Stopped    ${True}
    Key    ${KEY_A}    0
    Module Sends    ${HOST_RESUME}
    Run For    1.5
    Should Be Asleep    ${True}    lower=${True}
    Should Be Stopped    ${True}
    Drive    0    7    1
    Run For    0.5
    Should Be Stopped    ${False}
    Should Be Asleep    ${False}
    Drive    0    7    2
    Stop Counts Should Be    2    1

Stop Mode Is Off Unless Turned On
    [Documentation]    M7c's stop mode is off at reset: asleep, the clocks run.
    Boot
    On The Dongle
    Module Sends    ${HOST_SUSPEND}
    Run For    2
    Should Be Asleep    ${True}
    Should Be Stopped    ${False}

In Stop Mode A Key Wakes It And Is Typed
    [Documentation]    Stopped (SysTick off, SLEEPDEEP), the rows' EXTI lines
    ...    wake it: the key is read and sent, the device selected again. Ten
    ...    seconds stopped don't trip the watchdog (no reset: the module isn't
    ...    asked its version again).
    Asleep And Stopped
    Run For    10
    Should Be Stopped    ${True}
    Key    ${KEY_A}    1
    # Out of the stop the device is selected again before the scan reads.
    Radio Should Get    ${SEL_DONGLE}
    Radio Should Get    ${A_DOWN}
    Key    ${KEY_A}    0
    Radio Should Get    ${ALL_UP}
    Run For    0.3
    Should Be Stopped    ${False}
    Should Be Asleep    ${False}
    Radio Should Not Get    ${VERSION_ASK}
    Stop Counts Should Be    1    1
    No Radio Errors
    # PRE_LP, POST_LP and the clocks' set-up again open ANCTL first.
    Should Not Be In Log    LOCKED write dropped    timeout=0

With Stop Diag On, Caps Shows A Stop In Progress
    [Documentation]    0xD9 'D': stop mode with the LEDs marking how far a stop
    ...    got (src/stop.rs). Stopped: Caps on (C5 low), Win-lock off (B0
    ...    high); a key still wakes it, and the device is selected again.
    Boot
    Stop Mode    0x44
    Wait For Log Entry    HOST HID 3: 08 d9 44 01    timeout=1    pauseEmulation=true
    On The Dongle
    Module Sends    ${HOST_SUSPEND}
    Run For    1.7
    Should Be Stopped    ${True}
    ${c}=    Execute Command    sysbus ReadDoubleWord ${GPIOC_ODR}
    ${b}=    Execute Command    sysbus ReadDoubleWord ${GPIOB_ODR}
    Should Be Equal    ${{ int($c.strip(), 0) & ${CAPS_LED} }}    ${0}    Caps on
    Should Be Equal    ${{ int($b.strip(), 0) & 1 }}    ${1}    Win-lock off
    Key    ${KEY_A}    1
    Radio Should Get    ${SEL_DONGLE}
    Key    ${KEY_A}    0
    Run For    0.3
    Should Be Stopped    ${False}

Every Stop Diag Variant At Once Still Stops And Wakes On A Key
    [Documentation]    0xD9 'D' 63: both edges, pending cleared, no knob, EXTI
    ...    events, the long settle, the PLL restarted (src/stop.rs
    ...    VARIANT_*). Byte 16 of the answer echoes the bits.
    Boot
    Raw HID Send    0x08    0xD9    0x44    63
    Wait For Log Entry    HOST HID 3: 08 d9 44 01    timeout=1    pauseEmulation=true
    On The Dongle
    Module Sends    ${HOST_SUSPEND}
    Run For    1.7
    Should Be Stopped    ${True}
    Key    ${KEY_A}    1
    Radio Should Get    ${SEL_DONGLE}
    Radio Should Get    ${A_DOWN}
    Key    ${KEY_A}    0
    Run For    0.3
    Should Be Stopped    ${False}
    Raw HID Send    0x08    0xD9    0x00
    Wait For Log Entry    HOST HID 3: 08 d9 00 01 01 00 00 00 01 00 00 00 00 00 00 00 3f    timeout=1    pauseEmulation=true

In Stop Mode The Module's UART Wakes It
    [Documentation]    A frame from the module ends the stop (EXTI 11 on the
    ...    chip; the UART's interrupt in the sim); the radio reads it: the
    ...    host resuming wakes the keyboard.
    Asleep And Stopped
    Module Sends    ${HOST_RESUME}
    Radio Should Get    ${SEL_DONGLE}
    Run For    0.3
    Should Be Stopped    ${False}
    Should Be Asleep    ${False}
    Stop Counts Should Be    1    0

In Stop Mode The Cable Wakes It, And It Doesn't Stop With The Cable In
    Asleep And Stopped
    Drive    0    7    1
    Run For    0.5
    Should Be Asleep    ${False}
    Module Sends    ${HOST_SUSPEND}
    Run For    2
    Should Be Asleep    ${True}
    Should Be Stopped    ${False}
    Drive    0    7    2

In Stop Mode The Knob Wakes It, And The Next Turn Is Sent
    [Documentation]    B6's EXTI line wakes it. Line 7 is the cable's (A7)
    ...    while stopped, so the first detent's B7 edge is missed and that
    ...    turn isn't read; line 7 is the knob's again after, and the next
    ...    turn is sent.
    Asleep And Stopped
    Drive    1    7    0
    Run For    0.002
    Drive    1    6    0
    Run For    0.002
    Drive    1    7    2
    Run For    0.002
    Drive    1    6    2
    Radio Should Get    ${SEL_DONGLE}
    Run For    0.3
    Should Be Asleep    ${False}
    Should Be Stopped    ${False}
    Drive    1    7    0
    Run For    0.002
    Drive    1    6    0
    Run For    0.002
    Drive    1    7    2
    Run For    0.002
    Drive    1    6    2
    Radio Should Get    a3 e9 00
