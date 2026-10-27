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
${GPIOB_ODR}        0x40000414
${GPIO_DRIVE}       0x40000FF4
${WIN_LOCK_LED}     0x1
${AUX_LED}          0x200
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
${HS_BATQ}          5 6
${RADIO_FLAGS}      0xE8
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

Pin B Should Be
    [Arguments]    ${mask}    ${high}
    ${v}=    Execute Command    sysbus ReadDoubleWord ${GPIOB_ODR}
    ${is_high}=    Evaluate    bool(int($v.strip(), 0) & ${mask})
    Should Be Equal    ${is_high}    ${high}

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

Set Battery
    [Documentation]    The radio module's battery level, then past its next
    ...    inquiry (every 3 s) and a frame.
    [Arguments]    ${percent}
    Execute Command    uart3 WriteDoubleWord ${RADIO_FLAGS} ${{ ${percent} << 8 }}
    Run For    3.1

Drive Pin A
    [Documentation]    ${level}: 0 low, 1 high, 2 released.
    [Arguments]    ${pin}    ${level}
    Execute Command    sysbus WriteDoubleWord ${GPIO_DRIVE} ${{ (${level} << 16) | ${pin} }}

LED 0 Over
    [Documentation]    LED 0's colours and the LED power, every 20 ms for
    ...    ${seconds}: a list of (colour, power on, other LEDs lit).
    [Arguments]    ${seconds}
    @{seen}=    Create List
    FOR    ${i}    IN RANGE    ${{ int(${seconds} * 50) }}
        Run For    0.02
        ${colours}=    LED Colours
        ${odr}=    Execute Command    sysbus ReadDoubleWord ${GPIOA_ODR}
        ${on}=    Evaluate    bool(int($odr.strip(), 0) & ${LED_POWER})
        ${others}=    Evaluate    any($colours[1:84])
        Append To List    ${seen}    ${{ ($colours[0], $on, $others) }}
    END
    RETURN    ${seen}

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


GU_TOGG Turns Off The Windows Keys And Lights Win-Lock
    [Documentation]    Fn+LWin (GU_TOGG) in Windows mode: the Win-lock LED
    ...    (B0, active low) comes on and LWin types nothing. It's saved: after
    ...    a reset it's still on; GU_TOGG again gives LWin back. Vial reads
    ...    the key as QMK's GU_TOGG (0x700B).
    Boot To Lighting
    Raw HID Send    0x04    1    5    1
    HID Should Show    3    04 01 05 01 70 0b
    Pin B Should Be    ${WIN_LOCK_LED}    ${True}
    Fn Tap    5 1
    Pin B Should Be    ${WIN_LOCK_LED}    ${False}
    Key    5    1    1
    Run For    0.05
    Key    5    1    0
    Run For    0.05
    Should Not Be In Log    HOST HID 1: 08    timeout=0
    Saved To Sector    0
    Reset And Boot
    Pin B Should Be    ${WIN_LOCK_LED}    ${False}
    Fn Tap    5 1
    Pin B Should Be    ${WIN_LOCK_LED}    ${True}
    Key    5    1    1
    HID Should Show    1    08 00 00 00 00 00 00 00
    Key    5    1    0
    No LED Errors

HS_CT_A Held 3 s Makes Right Ctrl Send App
    [Documentation]    Right Ctrl is Right Ctrl until Fn+RCtrl (HS_CT_A) is
    ...    held 3 s; a black blink confirms, and then it sends App (0x65).
    Boot To Lighting
    Key    5    11    1
    HID Should Show    1    10 00 00 00 00 00 00 00
    Key    5    11    0
    Run For    0.05
    Key    5    10    1
    Run For    0.03
    Key    5    11    1
    Run For    2.9
    ${colours}=    LED Colours
    ${lit}=    Evaluate    sum(1 for c in $colours[1:84] if c)
    Should Be True    ${lit} > 0    dark before the 3 s
    ${black}=    Set Variable    ${False}
    FOR    ${i}    IN RANGE    6
        Run For    0.05
        ${colours}=    LED Colours
        ${lit}=    Evaluate    sum(1 for c in $colours[1:84] if c)
        IF    ${lit} == 0
            ${black}=    Set Variable    ${True}
            BREAK
        END
    END
    Should Be True    ${black}    no black blink after the 3 s hold
    Key    5    11    0
    Run For    0.03
    Key    5    10    0
    Run For    0.05
    Key    5    11    1
    HID Should Show    1    00 00 65 00 00 00 00 00
    Key    5    11    0
    No LED Errors

The Aux LED Shows Lighting On Without A Cable
    [Documentation]    B9 is high while the lighting is on and the cable
    ...    sense (A7) is low; a cable (A7 high) or Fn+L turns it off.
    Boot To Lighting
    Pin B Should Be    ${AUX_LED}    ${True}
    Execute Command    sysbus WriteDoubleWord ${GPIO_DRIVE} ${{ (1 << 16) | 7 }}
    Run For    0.05
    Pin B Should Be    ${AUX_LED}    ${False}
    Execute Command    sysbus WriteDoubleWord ${GPIO_DRIVE} ${{ (2 << 16) | 7 }}
    Run For    0.05
    Pin B Should Be    ${AUX_LED}    ${True}
    Fn Tap And Wait    ${RGB_TOG}
    Pin B Should Be    ${AUX_LED}    ${False}
    No LED Errors

LED 0 Shows The Battery: Blinking When Low, Red While Charging
    [Documentation]    LED 0 (no key over it) is dark at a good level; at 15%
    ...    or less it blinks red, 250 ms on and off; with the cable in (A7
    ...    high) and the charger pulling A15 low it's red, and dark again
    ...    once A15 goes high (full). Fn+G hides it.
    Boot To Lighting
    ${seen}=    LED 0 Over    0.6
    Should Be True    all(c == 0 for c, _, _ in $seen)
    Set Battery    12
    ${seen}=    LED 0 Over    1.2
    ${colours}=    Evaluate    sorted(set(c for c, _, _ in $seen))
    Should Be Equal    ${colours}    ${{ [0, 0xFF0000] }}
    Drive Pin A    15    0
    Drive Pin A    7    1
    Run For    0.1
    ${seen}=    LED 0 Over    0.6
    Should Be True    all(c == 0xFF0000 for c, _, _ in $seen)
    Fn Tap    3 5
    ${seen}=    LED 0 Over    0.6
    Should Be True    all(c == 0 for c, _, _ in $seen)
    Fn Tap    3 5
    Drive Pin A    15    2
    Run For    0.1
    ${seen}=    LED 0 Over    0.6
    Should Be True    all(c == 0 for c, _, _ in $seen)
    Drive Pin A    7    2
    No LED Errors

Fn Space Shows The Battery Level
    [Documentation]    While Fn+Space is held the keys go dark but for a bar on
    ...    LEDs 16-25 (the number row), a LED per 10%: green from 50%,
    ...    yellow from 30%, red below. On USB too, and with the lighting off,
    ...    which powers the LEDs just for it.
    Boot To Lighting
    FOR    ${percent}    ${lit}    ${colour}    IN    42    4    0x969600    80    8    0x009600    7    1    0x960000
        Set Battery    ${percent}
        Key    5    10    1
        Run For    0.03
        Key    5    6    1
        Run For    0.1
        ${colours}=    LED Colours
        Key    5    6    0
        Run For    0.03
        Key    5    10    0
        Run For    0.1
        ${want}=    Evaluate    [${colour}] * ${lit} + [0] * (10 - ${lit})
        Should Be Equal    ${colours[16:26]}    ${want}
        Should Be True    not any($colours[1:16]) and not any($colours[26:84])
    END
    # (Above 15%, or the low-battery flash would power the LEDs too.)
    Set Battery    35
    Fn Tap And Wait    ${RGB_TOG}
    LED Power Should Be    ${False}
    Key    5    10    1
    Run For    0.03
    Key    5    6    1
    Run For    0.1
    LED Power Should Be    ${True}
    ${colours}=    LED Colours
    Should Be Equal    ${colours[16:26]}    ${{ [0x969600] * 3 + [0] * 7 }}
    Key    5    6    0
    Run For    0.03
    Key    5    10    0
    Run For    0.1
    LED Power Should Be    ${False}
    No LED Errors

With The Lights Off A Low Battery Flashes LED 0
    [Documentation]    The lighting off (Fn+L), LED power is off; at 15% or
    ...    less, every 5 s the LEDs are powered for about 190 ms, LED 0
    ...    red for about 150 ms of it, nothing else lit.
    Boot To Lighting
    Fn Tap And Wait    ${RGB_TOG}
    Set Battery    10
    ${seen}=    LED 0 Over    10.5
    ${red}=    Evaluate    [i for i, (c, on, o) in enumerate($seen) if c == 0xFF0000]
    ${powered}=    Evaluate    sum(1 for c, on, o in $seen if on)
    ${bad}=    Evaluate    [i for i in $red if not $seen[i][1] or $seen[i][2]]
    Should Be Empty    ${bad}    red without power, or with other LEDs lit
    # Two flashes of ~150 ms, 5 s apart, sampled every 20 ms.
    Should Be True    12 <= len($red) <= 18    ${red}
    Should Be True    len($red) < ${powered} <= 24    ${powered} powered samples
    Should Be True    $red[-1] - $red[0] > 200    one flash only
    No LED Errors

On USB The Lights Go Off 7 s After The Host Suspends
    [Documentation]    QMK's suspend_power_down after USB_POWER_DOWN_DELAY:
    ...    the host suspended for 7 s turns the LED power off (the MCU
    ...    doesn't sleep on USB); its resume turns it back on. (A key's
    ...    remote wakeup needs the host to enable it, which the model doesn't.)
    Boot To Lighting
    Execute Command    usb WriteDoubleWord ${HOST_CMD} 0x04
    Run For    6.5
    Should Not Be In Log    device did not detect suspend    timeout=0
    LED Power Should Be    ${True}
    Run For    0.8
    LED Power Should Be    ${False}
    Execute Command    usb WriteDoubleWord ${HOST_CMD} 0x05
    Run For    0.3
    LED Power Should Be    ${True}
    No LED Errors
