*** Comments ***
M5: the RGB lighting of the RMK image (akko-rmk). The LED chain is
sim/peripherals/spim2.py, which decodes the WS2812 bit stream on SPIM2's
MOSI back into colours and logs any malformed frame as an error. Lighting
starts once RMK's storage is up: the first frame goes out about 2.8 s after
a reset with a blank flash.

The effects and the board's own lighting (the vendor's RGB keys, side
lights, blinks and recorded colours) are checked frame for frame against
QMK's and the vendor's C on the host (tools/rgbref/run.py, board.py); these
tests check the firmware around them: the driver, the frame timing, power,
that keys reach the board, and what it keeps from the keyboard.

The side lights (LEDs 84-105) run their own effect, the vendor's rainbow
swirl by default. The USB key's LED (37) shows white for 2 s once USB is
configured. Most RGB keys confirm with a 0.5 s black blink of the whole
board.

*** Settings ***
Library             Process
Library             OperatingSystem
Library             ${CURDIR}/leds.py

*** Variables ***
${SIM}              ${CURDIR}/..
${RMK}              ${CURDIR}/../../target/thumbv7m-none-eabi/release/akko-rmk
${GOLDEN_SPIM2}     ${CURDIR}/../golden/qmk_spim2_init.txt
${GPIO_KEY}         0x40000FF0
${GPIOA_ODR}        0x40000014
${LED_POWER}        0x100
${FLASH_ADDR}       0xF0
${FLASH_WORD}       0xF4
${FLASH_ERASE}      0xF8
${AIRCR}            0xE000ED0C
${SYSRESETREQ}      0x05FA0004
${LED_ERRORS}       0xEC
${LED_FRAMES}       0xF0
${LED_COUNT}        0xF4
${LED_SELECT}       0xF8
${LED_COLOUR}       0xFC
${FN}               5 10
${RGB_MOD}          0 13
${RGB_TOG}          3 9
${RGB_VAI}          4 13
${RGB_VAD}          5 13
${RGB_SAI}          5 12
${RGB_SAD}          5 14
${RL_MOD}           5 0
${EE_CLR}           1 0
${HOST_CMD}         0xF00
${HOST_OUT_BYTE}    0xF0C
${VIAL_OUT_EP}      2

*** Keywords ***
Boot To Lighting
    Create Log Tester    3
    Execute Command    include @${SIM}/akko.resc
    Execute Command    logLevel 3 gpio
    Execute Command    qspi WriteDoubleWord ${FLASH_ERASE} 1
    Execute Command    sysbus LoadELF @${RMK}
    Execute Command    machine Reset
    Run For    2.5
    FOR    ${i}    IN RANGE    20
        ${frames}=    Spim2    ${LED_FRAMES}
        IF    ${frames} > 0    BREAK
        Run For    0.05
    END
    Should Be True    ${frames} > 0    no LED frame by 3.5 s
    # Past the USB key's 2 s of white after USB is configured.
    Wait For Log Entry    HOST: configured    timeout=4    pauseEmulation=true
    Run For    2.1

Reset And Boot
    [Documentation]    A system reset, the flash kept; on past the USB key's white.
    Execute Command    sysbus WriteDoubleWord ${AIRCR} ${SYSRESETREQ}
    Wait For Log Entry    HOST: configured    timeout=6    pauseEmulation=true
    Run For    2.1

Brightest Key LED
    [Documentation]    The largest channel of LEDs 1-83: the brightness, for
    ...    the default effect (saturation 255).
    ${colours}=    LED Colours
    ${v}=    Evaluate    max(max(c >> 16, (c >> 8) & 255, c & 255) for c in $colours[1:84])
    RETURN    ${v}

Saved To Sector
    [Documentation]    A lighting save landing in sector ${n} (0x20000 + n * 4 KB),
    ...    and nothing written outside RMK's and the lighting's sectors.
    [Arguments]    ${n}
    Wait For Log Entry    flash: PP 0x02${n}200    timeout=4    pauseEmulation=true
    Should Not Be In Log    flash: (CE|BE|(SE|PP) 0x0[03-7])    timeout=0    treatAsRegex=true

Run For
    [Arguments]    ${seconds}
    Execute Command    emulation RunFor "${seconds}"

Key
    [Arguments]    ${row}    ${col}    ${down}
    ${v}=    Evaluate    (${down} << 16) | (${row} << 8) | ${col}
    Execute Command    sysbus WriteDoubleWord ${GPIO_KEY} ${v}

Fn Tap
    [Documentation]    Fn held, ${row} ${col} tapped ${times} times, Fn released.
    [Arguments]    ${pos}    ${times}=1
    ${row}    ${col}=    Evaluate    "${pos}".split()
    Key    5    10    1
    Run For    0.03
    FOR    ${i}    IN RANGE    ${times}
        Key    ${row}    ${col}    1
        Run For    0.03
        Key    ${row}    ${col}    0
        Run For    0.03
    END
    Key    5    10    0
    Run For    0.03

Fn Tap And Wait
    [Documentation]    Fn Tap, then past the black blink that confirms it.
    [Arguments]    ${pos}    ${times}=1
    FOR    ${i}    IN RANGE    ${times}
        Fn Tap    ${pos}
        Run For    0.6
    END

Raw HID Send
    [Documentation]    One 32-byte packet on the Vial OUT endpoint, ${bytes} then zeros.
    [Arguments]    @{bytes}
    ${padded}=    Evaluate    [int(str(b), 0) for b in $bytes] + [0] * (32 - len($bytes))
    FOR    ${b}    IN    @{padded}
        Execute Command    usb WriteDoubleWord ${HOST_OUT_BYTE} ${b}
    END
    Execute Command    usb WriteDoubleWord ${HOST_CMD} ${{ 0x09 | (${VIAL_OUT_EP} << 8) }}
    Run For    0.05

HID Should Show
    [Arguments]    ${ep}    ${hex}    ${timeout}=1
    Wait For Log Entry    HOST HID ${ep}: ${hex}    timeout=${timeout}    pauseEmulation=true

All LEDs Should Be
    [Arguments]    ${colour}
    ${colours}=    LED Colours
    ${others}=    Evaluate    [i for i, c in enumerate($colours) if c != ${colour}]
    Should Be Empty    ${others}

Spim2
    [Arguments]    ${offset}
    ${v}=    Execute Command    spim2 ReadDoubleWord ${offset}
    RETURN    ${{ int($v.strip(), 0) }}

LED Colours
    [Documentation]    The colours of the last frame, 0xRRGGBB per LED.
    ${n}=    Spim2    ${LED_COUNT}
    Should Be Equal As Integers    ${n}    106
    @{colours}=    Create List
    FOR    ${i}    IN RANGE    ${n}
        Execute Command    spim2 WriteDoubleWord ${LED_SELECT} ${i}
        ${c}=    Spim2    ${LED_COLOUR}
        Append To List    ${colours}    ${c}
    END
    RETURN    ${colours}

No LED Errors
    ${e}=    Spim2    ${LED_ERRORS}
    Should Be Equal As Integers    ${e}    0

LED Power Should Be
    [Arguments]    ${on}
    ${odr}=    Execute Command    sysbus ReadDoubleWord ${GPIOA_ODR}
    ${is_on}=    Evaluate    bool(int($odr.strip(), 0) & ${LED_POWER})
    Should Be Equal    ${is_on}    ${on}

Append To List
    [Arguments]    ${list}    ${item}
    Evaluate    $list.append($item)

*** Test Cases ***
The LEDs Show QMK's Default Effect
    [Documentation]    Cycle left-right at QMK's defaults (hue 0, saturation
    ...    255, brightness 84 = the board's maximum): every key LED at hue
    ...    x - t for one effect time t, and t moves on. The side lights swirl
    ...    (hue 11 i + h for light i, mirrored), 3 hue steps every 20 ms.
    Boot To Lighting
    ${colours}=    LED Colours
    ${t1}=    Cycle Left Right Time    ${colours}
    ${h1}=    Swirl Hue    ${colours}
    Run For    0.2
    ${colours}=    LED Colours
    ${t2}=    Cycle Left Right Time    ${colours}
    ${h2}=    Swirl Hue    ${colours}
    Should Not Be Equal    ${t1}    ${t2}
    Should Not Be Equal    ${h1}    ${h2}
    No LED Errors
    LED Power Should Be    ${True}

A Frame Goes Out Every 16 ms While The Colours Change
    Boot To Lighting
    ${a}=    Spim2    ${LED_FRAMES}
    Run For    1
    ${b}=    Spim2    ${LED_FRAMES}
    ${n}=    Evaluate    ${b} - ${a}
    Should Be True    55 <= ${n} <= 63    ${n} frames in 1 s
    No LED Errors

SPIM2 Is Set Up And Driven As QMK Does
    [Documentation]    The first register writes, through the first frame,
    ...    against QMK's (sim/golden/qmk_spim2_init.txt).
    ${trace}=    Run Process    ${SIM}/trace.sh    ${RMK}    3.5    ^SPIM2
    ${golden}=    Get File    ${GOLDEN_SPIM2}
    ${golden}=    Evaluate    $golden.strip().splitlines()
    ${n}=    Get Length    ${golden}
    ${ours}=    Evaluate    $trace.stdout.strip().splitlines()[:${n}]
    Should Be Equal    ${ours}    ${golden}

The USB Key Shows White Once USB Is Configured
    [Documentation]    rgb_matrix_hs_devs_indicator: 2 s of white on LED 37.
    Create Log Tester    3
    Execute Command    include @${SIM}/akko.resc
    Execute Command    logLevel 3 gpio
    Execute Command    qspi WriteDoubleWord ${FLASH_ERASE} 1
    Execute Command    sysbus LoadELF @${RMK}
    Execute Command    machine Reset
    Wait For Log Entry    HOST: configured    timeout=4    pauseEmulation=true
    FOR    ${i}    IN RANGE    40
        ${frames}=    Spim2    ${LED_FRAMES}
        IF    ${frames} > 0    BREAK
        Run For    0.05
    END
    Run For    0.1
    ${colours}=    LED Colours
    Should Be Equal As Integers    ${colours}[37]    0xFFFFFF
    Run For    2.1
    ${colours}=    LED Colours
    Cycle Left Right Time    ${colours}
    No LED Errors

RGB Toggle Turns The LEDs And Their Supply Off And On
    [Documentation]    The side lights go off and on with the rest.
    Boot To Lighting
    Fn Tap    ${RGB_TOG}
    Run For    0.05
    ${colours}=    LED Colours
    ${on}=    Lit    ${colours}
    Should Be Empty    ${on}
    LED Power Should Be    ${False}
    Fn Tap    ${RGB_TOG}
    Run For    0.05
    ${colours}=    LED Colours
    Cycle Left Right Time    ${colours}
    Swirl Hue    ${colours}
    LED Power Should Be    ${True}
    No LED Errors

The RGB Keys Confirm With A Black Blink
    [Documentation]    rgb_blink_dir: every LED black for 0.5 s.
    Boot To Lighting
    Fn Tap    ${RGB_MOD}
    Run For    0.1
    All LEDs Should Be    0
    Run For    0.5
    ${colours}=    LED Colours
    ${on}=    Lit Keys    ${colours}
    Should Not Be Empty    ${on}
    Swirl Hue    ${colours}

Brightness Steps By 21 And The Supply Goes Off At 0
    [Documentation]    The side lights keep their own brightness (120); at 0
    ...    the vendor's code cuts the LEDs' supply anyway.
    Boot To Lighting
    Fn Tap And Wait    ${RGB_VAD}
    ${colours}=    LED Colours
    Cycle Left Right Time    ${colours}    v=${63}
    Fn Tap And Wait    ${RGB_VAD}    3
    ${colours}=    LED Colours
    ${on}=    Lit Keys    ${colours}
    Should Be Empty    ${on}
    LED Power Should Be    ${False}
    Fn Tap And Wait    ${RGB_VAI}
    ${colours}=    LED Colours
    Cycle Left Right Time    ${colours}    v=${21}
    LED Power Should Be    ${True}
    Fn Tap And Wait    ${RGB_VAI}    5
    ${colours}=    LED Colours
    Cycle Left Right Time    ${colours}    v=${84}
    No LED Errors

A Reactive Effect Lights The Key Pressed
    [Documentation]    RGB_MOD steps through the vendor's list (13, 15, 16,
    ...    24, 25, 26, 29, 37, 33, ...): eight steps from cycle left-right
    ...    is solid reactive simple (33). LEDs light where keys are hit and
    ...    fade in about 0.4 s. T (2, 5) then lights LED 39 alone of the
    ...    keys. The Win/Mac switch's taps aren't hits.
    Boot To Lighting
    Fn Tap And Wait    ${RGB_MOD}    8
    ${colours}=    LED Colours
    ${on}=    Lit Keys    ${colours}
    Should Be Empty    ${on}
    Key    2    5    1
    Run For    0.05
    ${colours}=    LED Colours
    ${on}=    Lit Keys    ${colours}
    Should Be Equal    ${on}    ${{ [39] }}
    Key    2    5    0
    Run For    0.6
    ${colours}=    LED Colours
    ${on}=    Lit Keys    ${colours}
    Should Be Empty    ${on}
    No LED Errors

Saturation Keys Step The Effect's Colour
    [Documentation]    RGB_SAI / RGB_SAD step through the vendor's five
    ...    colours, the last white (saturation 0), and blink only when they
    ...    move.
    Boot To Lighting
    Fn Tap And Wait    ${RGB_SAI}    4
    ${colours}=    LED Colours
    Cycle Left Right Time    ${colours}    s=${0}
    Fn Tap    ${RGB_SAI}
    Run For    0.1
    ${colours}=    LED Colours
    Cycle Left Right Time    ${colours}    s=${0}
    Fn Tap And Wait    ${RGB_SAD}
    ${colours}=    LED Colours
    Cycle Left Right Time    ${colours}

Fn LCtrl Steps The Side Lights
    [Documentation]    RL_MOD: swirl, rainbow mood, breathing, static red,
    ...    the static hues 40 to 200, white, off, and round to the swirl.
    Boot To Lighting
    Fn Tap    ${RL_MOD}
    Run For    0.3
    ${colours}=    LED Colours
    ${c}=    Side Colours    ${colours}
    Should Not Be Equal As Integers    ${c}    0
    Fn Tap    ${RL_MOD}    2
    Run For    0.1
    ${colours}=    LED Colours
    ${c}=    Side Colours    ${colours}
    ${expected}=    Hsv To Rgb    0    255    120
    Should Be Equal As Integers    ${c}    ${expected}
    FOR    ${h}    IN    40    80    120    160    200
        Fn Tap    ${RL_MOD}
        Run For    0.1
        ${colours}=    LED Colours
        ${c}=    Side Colours    ${colours}
        ${expected}=    Hsv To Rgb    ${h}    255    120
        Should Be Equal As Integers    ${c}    ${expected}
    END
    Fn Tap    ${RL_MOD}
    Run For    0.1
    ${colours}=    LED Colours
    ${c}=    Side Colours    ${colours}
    ${expected}=    Hsv To Rgb    0    0    120
    Should Be Equal As Integers    ${c}    ${expected}
    Fn Tap    ${RL_MOD}
    Run For    0.1
    ${colours}=    LED Colours
    ${c}=    Side Colours    ${colours}
    Should Be Equal As Integers    ${c}    0
    Cycle Left Right Time    ${colours}
    LED Power Should Be    ${True}
    Fn Tap    ${RL_MOD}
    Run For    0.1
    ${colours}=    LED Colours
    Swirl Hue    ${colours}
    No LED Errors

Recording Takes Key Presses And Plays Them Back
    [Documentation]    RP_P0 and RP_END aren't on the default keymap: Vial
    ...    puts them on Fn+Y and Fn+T. RP_P0 plays channel 0 (green keys);
    ...    RP_END starts recording, which clears it; A then turns red and
    ...    doesn't reach the host. RP_END saves it; RGB_MOD leaves playback;
    ...    RP_P0 shows the recording, and A types again.
    Boot To Lighting
    Raw HID Send    0x05    1    2    6    0x7E    0x05
    Raw HID Send    0x05    1    2    5    0x7E    0x08
    Fn Tap    2 6
    Run For    0.1
    ${colours}=    LED Colours
    ${on}=    Lit Keys    ${colours}
    Should Be Equal    ${on}    ${{ [14, 42, 45, 46, 47, 60, 81, 82, 83] }}
    ${expected}=    Hsv To Rgb    85    255    84
    Should Be Equal As Integers    ${colours}[46]    ${expected}
    Fn Tap    2 5
    Run For    0.1
    ${colours}=    LED Colours
    ${on}=    Lit Keys    ${colours}
    Should Be Empty    ${on}
    Key    3    1    1
    Run For    0.05
    Key    3    1    0
    Run For    0.1
    Should Not Be In Log    HOST HID 1: 00 00 04    timeout=0
    ${colours}=    LED Colours
    ${on}=    Lit Keys    ${colours}
    Should Be Equal    ${on}    ${{ [46] }}
    ${expected}=    Hsv To Rgb    0    255    84
    Should Be Equal As Integers    ${colours}[46]    ${expected}
    Fn Tap    2 5
    Fn Tap And Wait    ${RGB_MOD}
    ${colours}=    LED Colours
    Cycle Left Right Time    ${colours}
    Fn Tap    2 6
    Run For    0.1
    ${colours}=    LED Colours
    ${on}=    Lit Keys    ${colours}
    Should Be Equal    ${on}    ${{ [46] }}
    Key    3    1    1
    HID Should Show    1    00 00 04 00 00 00 00 00
    Key    3    1    0
    No LED Errors

EE_CLR Resets Only After A 3 s Hold
    [Documentation]    Fn+` held 1 s does nothing. Held 3 s the lighting
    ...    resets and blinks grey (0x10) three times; on release RMK erases
    ...    its storage and reboots, so a keymap change made through Vial is
    ...    gone.
    Boot To Lighting
    # dynamic_keymap_set_keycode(layer 0, row 3, col 1) = B.
    Raw HID Send    0x05    0    3    1    0x00    0x05
    Run For    0.5
    Key    5    10    1
    Run For    0.03
    Key    1    0    1
    Run For    1
    Key    1    0    0
    Run For    0.03
    Key    5    10    0
    Run For    0.5
    Should Not Be In Log    HOST HID 1: 00 00 35
    Key    3    1    1
    HID Should Show    1    00 00 05 00 00 00 00 00
    Key    3    1    0
    Run For    0.1
    Key    5    10    1
    Run For    0.03
    Key    1    0    1
    Run For    3.05
    ${grey}=    Set Variable    ${False}
    FOR    ${i}    IN RANGE    12
        Run For    0.1
        ${colours}=    LED Colours
        ${n}=    Evaluate    sum(1 for c in $colours if c == 0x101010)
        IF    ${n} == 106
            ${grey}=    Set Variable    ${True}
            BREAK
        END
    END
    Should Be True    ${grey}    no grey blink after the 3 s hold
    Key    1    0    0
    Run For    0.03
    Key    5    10    0
    Wait For Log Entry    HOST: configured    timeout=6    pauseEmulation=true
    Run For    0.1
    Key    3    1    1
    HID Should Show    1    00 00 04 00 00 00 00 00

Lighting Settings Survive A Reset
    [Documentation]    Fn+Down dims the default effect from 84 to 63. The
    ...    save goes to the first lighting sector 3 s after the last key,
    ...    and a reset brings it back.
    Boot To Lighting
    ${v}=    Brightest Key LED
    Should Be Equal As Integers    ${v}    84
    Fn Tap And Wait    ${RGB_VAD}
    ${v}=    Brightest Key LED
    Should Be Equal As Integers    ${v}    63
    Saved To Sector    0
    Reset And Boot
    ${v}=    Brightest Key LED
    Should Be Equal As Integers    ${v}    63
    No LED Errors

A Torn Lighting Save Keeps The One Before
    [Documentation]    Two saves alternate sectors; the second one damaged
    ...    (as by a power cut while writing), the first is what loads.
    Boot To Lighting
    Fn Tap And Wait    ${RGB_VAD}
    Saved To Sector    0
    Fn Tap And Wait    ${RGB_VAD}
    ${v}=    Brightest Key LED
    Should Be Equal As Integers    ${v}    42
    Saved To Sector    1
    Execute Command    qspi WriteDoubleWord ${FLASH_ADDR} 0x21100
    Execute Command    qspi WriteDoubleWord ${FLASH_WORD} 0x12345678
    Reset And Boot
    ${v}=    Brightest Key LED
    Should Be Equal As Integers    ${v}    63
    No LED Errors

EE_CLR Erases The Saved Lighting
    [Documentation]    A saved dimmer setting is gone after EE_CLR's reboot.
    Boot To Lighting
    Fn Tap And Wait    ${RGB_VAD}
    Saved To Sector    0
    Key    5    10    1
    Run For    0.03
    Key    1    0    1
    Run For    3.05
    Wait For Log Entry    flash: SE 0x021000    timeout=1    pauseEmulation=true
    Key    1    0    0
    Run For    0.03
    Key    5    10    0
    Wait For Log Entry    HOST: configured    timeout=6    pauseEmulation=true
    Run For    2.1
    ${v}=    Brightest Key LED
    Should Be Equal As Integers    ${v}    84
    No LED Errors

Vial's Lighting Tab Reads, Sets And Saves The Effect
    [Documentation]    VialRGB over VIA's custom values: the protocol and
    ...    the maximum brightness (84); the default mode, cycle left-right
    ...    (Vial's 14) at speed 153; the supported ids after 40. Setting
    ...    solid colour (2), green at 42, shows on the keys and is saved.
    Boot To Lighting
    Raw HID Send    0x08    0x40
    HID Should Show    3    08 40 01 00 54
    Raw HID Send    0x08    0x41
    HID Should Show    3    08 41 0e 00 99 00 ff 54
    Raw HID Send    0x08    0x42    40    0
    HID Should Show    3    08 42 29 00 2a 00 2b 00 2c 00 ff ff
    Raw HID Send    0x07    0x41    0x02    0x00    0x80    85    255    42
    Run For    0.1
    ${colours}=    LED Colours
    ${expected}=    Hsv To Rgb    85    255    42
    ${others}=    Evaluate    [i for i in range(1, 84) if $colours[i] != ${expected}]
    Should Be Empty    ${others}
    Raw HID Send    0x08    0x41
    HID Should Show    3    08 41 02 00 80 55 ff 2a
    Saved To Sector    0
    Reset And Boot
    Raw HID Send    0x08    0x41
    HID Should Show    3    08 41 02 00 80 55 ff 2a
    No LED Errors
