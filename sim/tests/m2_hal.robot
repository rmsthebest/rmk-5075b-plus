*** Comments ***
M2: the HAL core (clocks, TIM2 time base, GPIO, EXTI) on embassy, through the
m2-blink image. sim/run.sh builds it three times: normally, and with the time
base started 20 ms before its 16-bit counter's half-wrap (sim-time-near-half)
or full wrap (sim-time-near-wrap), the two points where the driver's period
count advances.
"In DFU" means the CPU is parked in the ROM stub at 0x1FFFE008 (see sim/akko.resc).

*** Settings ***
Library             Process
Library             OperatingSystem

*** Variables ***
${SIM}              ${CURDIR}/..
${M2}               ${CURDIR}/../../target/thumbv7m-none-eabi/release/m2-blink
${M2_HALF}          ${CURDIR}/../../target/sim-near-half/thumbv7m-none-eabi/release/m2-blink
${M2_WRAP}          ${CURDIR}/../../target/sim-near-wrap/thumbv7m-none-eabi/release/m2-blink
${GOLDEN_CLOCK}     ${CURDIR}/../golden/qmk_clock_init.txt
${ROM_PARKED}       0x1FFFE008
${GPIOC_ODR}        0x40000814
${CAPS_LED}         0x20
${TIM2_CNT}         0x40002024
${GPIO_KEY}         0x40000FF0
${ESC_DOWN}         0x10000
${F1_DOWN}          0x10001
# boot-guard's Esc check comes before main and is covered by boot_guard.robot.
${BOOT_GUARD_WRITES}    6

*** Keywords ***
Boot
    [Arguments]    ${elf}
    Execute Command    include @${SIM}/akko.resc
    Execute Command    sysbus LoadELF @${elf}
    Execute Command    machine Reset

Run For
    [Arguments]    ${seconds}
    Execute Command    emulation RunFor "${seconds}"

Read Word
    [Arguments]    ${addr}
    ${v}=    Execute Command    sysbus ReadDoubleWord ${addr}
    ${v}=    Convert To Integer    ${v.strip()}
    RETURN    ${v}

Period Count
    [Documentation]    The time driver's half-wrap count (first word of DRIVER).
    [Arguments]    ${elf}
    ${nm}=    Run Process    nm    -C    ${elf}
    ${addr}=    Evaluate    [l.split()[0] for l in $nm.stdout.splitlines() if l.endswith(" wb32_hal::time_driver::DRIVER")][0]
    ${period}=    Read Word    0x${addr}
    RETURN    ${period}

Boundary Advances The Period
    [Documentation]    Boots ${elf}, whose counter starts 20 ms before ${boundary}.
    [Arguments]    ${elf}    ${boundary}    ${period_before}
    Boot    ${elf}
    Run For    0.01
    ${cnt}=    Read Word    ${TIM2_CNT}
    ${left}=    Evaluate    (${boundary} - ${cnt}) % (1 << 16)
    Should Be True    8000 <= ${left} <= 11000    TIM2 CNT ${cnt} is not 10 ms before ${boundary}
    ${p}=    Period Count    ${elf}
    Should Be Equal As Integers    ${p}    ${period_before}
    Run For    0.02
    ${cnt}=    Read Word    ${TIM2_CNT}
    ${past}=    Evaluate    (${cnt} - ${boundary}) % (1 << 16)
    Should Be True    8000 <= ${past} <= 11000    TIM2 CNT ${cnt} is not 10 ms past ${boundary}
    ${p}=    Period Count    ${elf}
    ${expected}=    Evaluate    int(${period_before}) + 1
    Should Be Equal As Integers    ${p}    ${expected}

Caps LED Is On
    ${odr}=    Read Word    ${GPIOC_ODR}
    # active low
    ${on}=    Evaluate    (${odr} & ${CAPS_LED}) == 0
    RETURN    ${on}

PC Should Be In ROM
    ${pc}=    Execute Command    cpu PC
    Should Be Equal As Integers    ${pc.strip()}    ${ROM_PARKED}

PC Should Be In Flash
    ${pc}=    Execute Command    cpu PC
    ${pc}=    Convert To Integer    ${pc.strip()}
    Should Be True    0x08000000 <= ${pc} < 0x08020000    PC 0x${pc} is not in flash

Caps LED Toggles Every 500 ms Then DFU
    # Toggles at t = 0, 0.5, ..., 9.5 s; DFU at 10 s. Each edge is checked
    # within +-10 ms, and the LED must hold still in between.
    Run For    0.49
    FOR    ${i}    IN RANGE    1    20
        ${before}=    Caps LED Is On
        Run For    0.02
        ${after}=    Caps LED Is On
        Should Not Be Equal    ${before}    ${after}    no toggle at ${i * 0.5} s
        Run For    0.24
        ${mid}=    Caps LED Is On
        Should Be Equal    ${mid}    ${after}    extra toggle before ${i * 0.5 + 0.25} s
        Run For    0.24
    END
    PC Should Be In Flash
    Run For    0.02
    PC Should Be In ROM

*** Test Cases ***
Clock Setup Matches QMK
    ${trace}=    Run Process    ${SIM}/trace.sh    ${M2}    0.05    ^(PWR|ANCTL|RCC|CACHE)
    ${golden}=    Get File    ${GOLDEN_CLOCK}
    ${golden}=    Evaluate    $golden.strip().splitlines()
    ${n}=    Get Length    ${golden}
    ${ours}=    Evaluate    $trace.stdout.strip().splitlines()[${BOOT_GUARD_WRITES}:${BOOT_GUARD_WRITES} + ${n}]
    Should Be Equal    ${ours}    ${golden}

Only Modelled Peripherals Are Touched
    Create Log Tester    1
    Boot    ${M2}
    Run For    1
    Should Not Be In Log    LOCKED
    Should Not Be In Log    non existing peripheral

Time Base Runs At 1 MHz
    # now = period << 15 + (CNT with bit 15 corrected by period's parity)
    Boot    ${M2}
    Run For    1
    ${cnt}=    Read Word    ${TIM2_CNT}
    ${p}=    Period Count    ${M2}
    ${now}=    Evaluate    (${p} << 15) + (${cnt} ^ ((${p} & 1) << 15))
    Should Be True    999000 <= ${now} <= 1001000    time base at ${now} us after 1 s (CNT ${cnt}, period ${p})

The Counter Is 16-Bit
    # The chip keeps only the low bits of ARR, as akko.resc models: a 32-bit
    # time driver passes everything else here and fails on the board.
    Boot    ${M2}
    Run For    0.2
    ${arr}=    Read Word    0x4000202C
    Should Be Equal As Integers    ${arr}    0xFFFF
    ${cnt}=    Read Word    ${TIM2_CNT}
    Should Be True    ${cnt} <= 0xFFFF

Caps LED Blinks For 10 s Then Returns To DFU
    Boot    ${M2}
    Caps LED Toggles Every 500 ms Then DFU

Half-Wrap Advances The Period
    # CC1 at 0x8000
    Boundary Advances The Period    ${M2_HALF}    0x8000    0

Wrap Advances The Period
    # update event at the wrap to 0
    Boundary Advances The Period    ${M2_WRAP}    0x10000    1

Time Base Survives The Half-Wrap
    # The blink timing and the 10 s deadline must not notice the boundary at
    # 20 ms (or the ~300 after it).
    Boot    ${M2_HALF}
    Caps LED Toggles Every 500 ms Then DFU

Time Base Survives The Counter Wrap
    Boot    ${M2_WRAP}
    Caps LED Toggles Every 500 ms Then DFU

Esc Returns To DFU At Once
    Boot    ${M2}
    Run For    2.2
    PC Should Be In Flash
    Execute Command    sysbus WriteDoubleWord ${GPIO_KEY} ${ESC_DOWN}
    Run For    0.002
    PC Should Be In ROM

Other Keys On The Esc Row Do Not
    Boot    ${M2}
    Run For    1
    # F1 shares row A0, but its column (C1) isn't driven, so A0 stays high.
    Execute Command    sysbus WriteDoubleWord ${GPIO_KEY} ${F1_DOWN}
    Run For    1
    PC Should Be In Flash
