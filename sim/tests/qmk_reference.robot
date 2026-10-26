*** Comments ***
The known-good QMK build (flashed and in daily use) run in the simulator. These
tests check the simulator's models against real firmware: if one fails after a
model change, the model is wrong, not QMK.

*** Variables ***
${SIM}          ${CURDIR}/..
${QMK_BIN}      ${CURDIR}/../../../akko_5075b_plus_fixes.bin
${GOLDEN}       ${CURDIR}/../golden/qmk_sysctl_writes.txt
${GOLDEN_CLOCK}    ${CURDIR}/../golden/qmk_clock_init.txt
${GOLDEN_USB}      ${CURDIR}/../golden/qmk_usb_init.txt
${USB_INIT_REGS}    ^(RCC\\+0x(014|034|040|06c|07c|088)|SFM)
${GOLDEN_QSPI}    ${CURDIR}/../golden/qmk_qspi_init.txt

*** Keywords ***
Boot QMK
    Execute Command    include @${SIM}/akko.resc
    Execute Command    sysbus LoadBinary @${QMK_BIN} 0x08000000
    Execute Command    machine Reset

Boot QMK With Hooks
    [Documentation]    QMK past the unmodelled SPI flash (sim/qmk-hooks.resc), so it reaches USB.
    Boot QMK
    Execute Command    include @${SIM}/qmk-hooks.resc

*** Test Cases ***
QMK Clock Setup Respects The ANCTL Key
    Create Log Tester    1
    Boot QMK
    Execute Command    emulation RunFor "0.5"
    Should Not Be In Log    LOCKED

QMK Clock Setup Matches The Golden Trace
    ${trace}=    Run Process    ${SIM}/trace.sh    ${QMK_BIN}    0.5    ^(PWR|ANCTL|RCC)
    ${golden}=    Get File    ${GOLDEN}
    Should Be Equal    ${trace.stdout.strip()}    ${golden.strip()}

QMK Clock Init Matches Its Golden Trace
    # The prefix our rcc::init reproduces (tests/m2_hal.robot), cache write included.
    ${trace}=    Run Process    ${SIM}/trace.sh    ${QMK_BIN}    0.5    ^(PWR|ANCTL|RCC|CACHE)
    ${golden}=    Get File    ${GOLDEN_CLOCK}
    ${golden}=    Evaluate    $golden.strip().splitlines()
    ${n}=    Get Length    ${golden}
    ${qmk}=    Evaluate    $trace.stdout.strip().splitlines()[:${n}]
    Should Be Equal    ${qmk}    ${golden}

QMK Reaches The ChibiOS Scheduler
    # Stops at the first external-flash (QSPI) transfer until QSPI/DMA are modelled.
    Boot QMK
    Execute Command    emulation RunFor "0.5"
    ${pc}=    Execute Command    cpu PC
    ${pc}=    Convert To Integer    ${pc.strip()}
    Should Be True    0x08000000 <= ${pc} < 0x08020000

QMK SPI Flash Bring-Up Matches Its Golden Trace
    # Without the hooks: QSPI setup (BAUDR 0x20 = PCLK1/32, SER, CR0 8-bit
    # mode 0, the FIFO depth probe), chip select on C12 (GPIOC BSRR) and the
    # first flash commands on a blank chip (sim/peripherals/qspi.py).
    ${trace}=    Run Process    ${SIM}/trace.sh    ${QMK_BIN}    1.5    ^(QSPI|FLASH|GPIOC\\+0x018)
    ${golden}=    Get File    ${GOLDEN_QSPI}
    Should Be Equal    ${trace.stdout.strip()}    ${golden.strip()}

QMK USB Bring-Up Matches Its Golden Trace
    # tests/m3_usb.robot checks our image against the same file.
    ${trace}=    Run Process    ${SIM}/trace.sh    ${QMK_BIN}    1.0    ${USB_INIT_REGS}
    ...    env:SIM_SETUP=include @${SIM}/qmk-hooks.resc
    ${golden}=    Get File    ${GOLDEN_USB}
    Should Be Equal    ${trace.stdout.strip()}    ${golden.strip()}

QMK Enumerates On The USB Model
    # The model's control-transfer and FADDR timing against ChibiOS's driver,
    # which sets the address early (before the status stage).
    Create Log Tester    1
    Boot QMK With Hooks
    Execute Command    emulation RunFor "1.0"
    Wait For Log Entry    HOST: SET_ADDRESS(5) -> ok
    Wait For Log Entry    HOST: device 342d:e4f6, EP0 64 bytes
    # Keyboard EP1 IN, VIA raw HID EP2 IN and OUT, shared EP3 IN.
    Wait For Log Entry    HOST: configured: if 0 class 03 eps [81], if 1 class 03 eps [82 02], if 2 class 03 eps [83]
    Should Not Be In Log    HOST ERROR

QMK Accepts The Keyboard LED Report
    Create Log Tester    1
    Boot QMK With Hooks
    Execute Command    emulation RunFor "1.0"
    Execute Command    usb WriteDoubleWord 0xF00 0x0203
    Execute Command    emulation RunFor "0.05"
    Wait For Log Entry    HOST: SET_REPORT(output) -> ok
    Should Not Be In Log    HOST ERROR
