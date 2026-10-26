*** Comments ***
The known-good QMK build (flashed and in daily use) run in the simulator. These
tests check the simulator's models against real firmware: if one fails after a
model change, the model is wrong, not QMK.

*** Variables ***
${SIM}          ${CURDIR}/..
${QMK_BIN}      ${CURDIR}/../../../akko_5075b_plus_fixes.bin
${GOLDEN}       ${CURDIR}/../golden/qmk_sysctl_writes.txt
${GOLDEN_CLOCK}    ${CURDIR}/../golden/qmk_clock_init.txt

*** Keywords ***
Boot QMK
    Execute Command    include @${SIM}/akko.resc
    Execute Command    sysbus LoadBinary @${QMK_BIN} 0x08000000
    Execute Command    machine Reset

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
