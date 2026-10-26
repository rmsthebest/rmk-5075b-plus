*** Comments ***
Boot-guard escape hatch on the simulated WB32FQ95 (sim/run.sh builds the images).
"In DFU" means the CPU is parked in the ROM stub at 0x1FFFE008 (see sim/akko.resc).

*** Variables ***
${SIM}              ${CURDIR}/..
${IMAGES}           ${CURDIR}/../../target/thumbv7m-none-eabi/release
${M1A}              ${IMAGES}/m1a-bounce
${M1B}              ${IMAGES}/m1b-stub
${ROM_PARKED}       0x1FFFE008
${MAGIC_ADDR}       0x20006FFC
${MAGIC}            0xDEADBEEF
${GPIO_KEY}         0x40000FF0
${ESC_DOWN}         0x10000

*** Keywords ***
Boot
    [Arguments]    ${elf}
    Execute Command    include @${SIM}/akko.resc
    Execute Command    sysbus LoadELF @${elf}
    Execute Command    machine Reset

Run For
    [Arguments]    ${seconds}
    Execute Command    emulation RunFor "${seconds}"

PC Should Be In ROM
    ${pc}=    Execute Command    cpu PC
    Should Be Equal As Integers    ${pc.strip()}    ${ROM_PARKED}

PC Should Be In Flash
    ${pc}=    Execute Command    cpu PC
    ${pc}=    Convert To Integer    ${pc.strip()}
    Should Be True    0x08000000 <= ${pc} < 0x08020000    PC 0x${pc} is not in flash

Word Should Be
    [Arguments]    ${addr}    ${expected}    ${mask}=0xFFFFFFFF
    ${v}=    Execute Command    sysbus ReadDoubleWord ${addr}
    ${v}=    Evaluate    int("${v.strip()}", 0) & ${mask}
    Should Be Equal As Integers    ${v}    ${expected}

*** Test Cases ***
Normal Boot Stays In Firmware
    Create Log Tester    1
    Boot    ${M1B}
    Run For    0.2
    PC Should Be In Flash
    Word Should Be    ${MAGIC_ADDR}    0
    Should Not Be In Log    LOCKED

Esc Held At Cold Start Enters DFU
    Create Log Tester    1
    Boot    ${M1B}
    Execute Command    sysbus WriteDoubleWord ${GPIO_KEY} ${ESC_DOWN}
    Run For    0.2
    Wait For Log Entry    Resetting platform with SYSRESETREQ
    PC Should Be In ROM
    # boot-guard clears the magic word before jumping, so QMK/Rust start clean next time
    Word Should Be    ${MAGIC_ADDR}    0

Magic Word Enters DFU Without Touching GPIO
    Create Log Tester    1
    Boot    ${M1B}
    Execute Command    sysbus WriteDoubleWord ${MAGIC_ADDR} ${MAGIC}
    Run For    0.1
    PC Should Be In ROM
    Word Should Be    ${MAGIC_ADDR}    0
    Should Not Be In Log    MMIO W GPIO

Esc Check Restores Pin State
    Boot    ${M1B}
    Run For    0.01
    # A0: input, no pull; C0: input; GPIOA's CFGMSK back to reset. (m1b's main has
    # already made C5 an output for the LED, so only C0's field is checked on GPIOC.)
    Word Should Be    0x40000000    0    mask=0x3    # GPIOA MODER, A0
    Word Should Be    0x4000000C    0    mask=0x3    # GPIOA PUPDR, A0
    Word Should Be    0x40000030    0                # GPIOA CFGMSK
    Word Should Be    0x40000800    0    mask=0x3    # GPIOC MODER, C0

Other Keys Do Not Enter DFU
    Boot    ${M1B}
    # F1 (row 0, col 1) and LCtrl (row 5, col 0): same row or same column as Esc
    Execute Command    sysbus WriteDoubleWord ${GPIO_KEY} 0x10001
    Execute Command    sysbus WriteDoubleWord ${GPIO_KEY} 0x10500
    Run For    0.2
    PC Should Be In Flash

Caps Lock LED Blinks
    Create Log Tester    1
    Boot    ${M1B}
    Run For    1
    # BSRR on C5: reset half (LED on, active low), then set half (off)
    Wait For Log Entry    MMIO W GPIOC+0x018 = 0x00200000
    Wait For Log Entry    MMIO W GPIOC+0x018 = 0x00000020

M1B Returns To DFU On Its Own
    Boot    ${M1B}
    Run For    12
    PC Should Be In ROM

HardFault Lands In DFU
    Boot    ${M1B}
    Run For    0.05
    # UDF #0 in RAM; with no UsageFault handler enabled it escalates to HardFault
    Execute Command    sysbus WriteWord 0x20005000 0xDE00
    Execute Command    cpu PC 0x20005000
    Run For    0.1
    PC Should Be In ROM

M1A Bounces Straight Into DFU
    Create Log Tester    1
    Boot    ${M1A}
    Run For    0.05
    PC Should Be In ROM
    Should Not Be In Log    MMIO W
