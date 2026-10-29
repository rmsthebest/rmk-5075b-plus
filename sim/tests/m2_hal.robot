*** Comments ***
M2: the HAL core (clocks, SysTick time base, GPIO, EXTI) on embassy, through
the m2-blink image.
"In DFU" means the CPU is parked in the ROM stub at 0x1FFFE008 (see sim/akko.resc).

*** Settings ***
Library             Process
Library             OperatingSystem

*** Variables ***
${SIM}              ${CURDIR}/..
${M2}               ${CURDIR}/../../target/thumbv7m-none-eabi/release/m2-blink
${GOLDEN_CLOCK}     ${CURDIR}/../golden/qmk_clock_init.txt
${ROM_PARKED}       0x1FFFE008
${GPIOC_ODR}        0x40000814
${CAPS_LED}         0x20
${SYST_CSR}         0xE000E010
${SYST_RVR}         0xE000E014
${SYST_CVR}         0xE000E018
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

Milliseconds
    [Documentation]    The time driver's time in ms: DRIVER's first fields
    ...    (the ticks, us, up to the current period, u64, and the cycles past
    ...    that tick) plus how far SysTick is into that period (its reload
    ...    minus its count).
    [Arguments]    ${elf}
    ${nm}=    Run Process    nm    -C    ${elf}
    ${addr}=    Evaluate    int([l.split()[0] for l in $nm.stdout.splitlines() if l.endswith(" wb32_hal::time_driver::DRIVER")][0], 16)
    ${lo}=    Read Word    ${addr}
    ${hi}=    Read Word    ${{ ${addr} + 4 }}
    ${frac}=    Read Word    ${{ ${addr} + 8 }}
    ${rvr}=    Read Word    ${SYST_RVR}
    ${cvr}=    Read Word    ${SYST_CVR}
    RETURN    ${{ ((${hi} << 32 | ${lo}) + (${frac} + ${rvr} - ${cvr}) // 96) // 1000 }}

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
    Should Be True    0x08000000 <= ${pc} < 0x08040000    PC 0x${pc} is not in flash

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

SysTick Runs Tickless
    # Core clock, interrupt on; the reload is set to the next alarm (the
    # blink's 500 ms is past the counter's reach: its longest, 2^24 cycles).
    Boot    ${M2}
    Run For    0.1
    ${csr}=    Read Word    ${SYST_CSR}
    Should Be Equal As Integers    ${{ ${csr} & 0x7 }}    7
    ${rvr}=    Read Word    ${SYST_RVR}
    Should Be Equal As Integers    ${rvr}    ${{ (1 << 24) - 1 }}

Time Base Counts Milliseconds
    Boot    ${M2}
    Run For    1
    ${ms}=    Milliseconds    ${M2}
    Should Be True    999 <= ${ms} <= 1001    time base at ${ms} ms after 1 s

Caps LED Blinks For 10 s Then Returns To DFU
    Boot    ${M2}
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
