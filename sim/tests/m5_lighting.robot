*** Comments ***
M5: the RGB lighting of the RMK image (akko-rmk). The LED chain is
sim/peripherals/spim2.py, which decodes the WS2812 bit stream on SPIM2's
MOSI back into colours and logs any malformed frame as an error. Lighting
starts once RMK's storage is up: the first frame goes out about 2.8 s after
a reset with a blank flash.

The effects themselves are checked frame for frame against QMK's C on the
host (tools/rgbref/run.py); these tests check the firmware around them: the
driver, the frame timing, power, keys and the hit tracker.

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
${FLASH_ERASE}      0xF8
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
    # Let the start-up Win/Mac switch tap through.
    Run For    0.1

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
    ...    255, brightness 84 = the board's maximum): every LED at hue
    ...    x - t for one effect time t, and t moves on.
    Boot To Lighting
    ${colours}=    LED Colours
    ${t1}=    Cycle Left Right Time    ${colours}
    Run For    0.2
    ${colours}=    LED Colours
    ${t2}=    Cycle Left Right Time    ${colours}
    Should Not Be Equal    ${t1}    ${t2}
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

RGB Toggle Turns The LEDs And Their Supply Off And On
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
    LED Power Should Be    ${True}
    No LED Errors

Brightness Steps By 21 And The Supply Goes Off At 0
    Boot To Lighting
    Fn Tap    ${RGB_VAD}
    Run For    0.05
    ${colours}=    LED Colours
    Cycle Left Right Time    ${colours}    v=${63}
    Fn Tap    ${RGB_VAD}    3
    Run For    0.05
    ${colours}=    LED Colours
    ${on}=    Lit    ${colours}
    Should Be Empty    ${on}
    LED Power Should Be    ${False}
    Fn Tap    ${RGB_VAI}
    Run For    0.05
    ${colours}=    LED Colours
    Cycle Left Right Time    ${colours}    v=${21}
    LED Power Should Be    ${True}
    Fn Tap    ${RGB_VAI}    5
    Run For    0.05
    ${colours}=    LED Colours
    Cycle Left Right Time    ${colours}    v=${84}
    No LED Errors

A Reactive Effect Lights The Key Pressed
    [Documentation]    Twenty steps from cycle left-right (13) is solid
    ...    reactive simple (33): LEDs light where keys are hit and fade in
    ...    about 0.4 s at the default speed. Once the Fn taps have faded,
    ...    T (2, 5) lights LED 39 alone. The Win/Mac switch's taps aren't
    ...    hits (its (5, 3) is side light 105 on QMK's map).
    Boot To Lighting
    Fn Tap    ${RGB_MOD}    20
    Run For    0.6
    ${colours}=    LED Colours
    ${on}=    Lit    ${colours}
    Should Be Empty    ${on}
    Key    2    5    1
    Run For    0.05
    ${colours}=    LED Colours
    ${on}=    Lit    ${colours}
    Should Be Equal    ${on}    ${{ [39] }}
    Key    2    5    0
    Run For    0.6
    ${colours}=    LED Colours
    ${on}=    Lit    ${colours}
    Should Be Empty    ${on}
    No LED Errors
