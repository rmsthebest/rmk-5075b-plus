*** Comments ***
tests/m7_power.robot's measurements on the vendor's QMK build, for the README
comparison. Not part of sim/run.sh: it needs a QMK ELF with symbols (for
spi_flash_start, as qmk-hooks.resc, and SysTick_Handler), built from the
vendor's source, and QMK's busy loop runs at about 10 real minutes per
emulated second here, so each case takes about 25 minutes:
  renode-test --variable QMK:<elf> sim/compare/qmk_power.robot
renode-test runs a file's cases one after another; to run them at once, put
each case in a file of its own. QMK never sleeps, so 0.2 s is enough.
Its key reports go to the radio module in the simulation (its settings can't
be read here), so latency isn't measured: the README takes it from the source.

*** Settings ***
Library             Process
Library             OperatingSystem
Library             Collections

*** Variables ***
${SIM}              ${CURDIR}/..
${TESTS}            ${CURDIR}/../tests
${QMK}              ${EMPTY}
${GPIO_KEY}         0x40000FF0
${FN}               5 10
${RGB_TOG}          3 9
${KEY_A}            3 1

*** Keywords ***
Sym
    [Arguments]    ${name}
    ${addr}=    Evaluate    [int(l.split()[0], 16) for l in subprocess.run(["arm-none-eabi-nm", $QMK], capture_output=True, text=True).stdout.splitlines() if l.endswith(" " + $name)][0]
    ...    modules=subprocess
    RETURN    ${addr}

Boot
    Should Not Be Empty    ${QMK}    pass --variable QMK:<the QMK ELF>
    Create Log Tester    3
    Execute Command    include @${SIM}/akko.resc
    Execute Command    logLevel 3 gpio
    Execute Command    sysbus LoadELF @${QMK}
    Execute Command    machine Reset
    ${a}=    Sym    spi_flash_start
    Execute Command    cpu AddHook ${a} "self.SetRegisterUlong(0, 0); self.PC = self.LR"
    Wait For Log Entry    HOST: configured    timeout=6    pauseEmulation=true
    Run For    0.5

Run For
    [Arguments]    ${seconds}
    Execute Command    emulation RunFor "${seconds}"

Key
    [Arguments]    ${pos}    ${down}
    ${row}    ${col}=    Evaluate    "${pos}".split()
    Execute Command    sysbus WriteDoubleWord ${GPIO_KEY} ${{ (${down} << 16) | (${row} << 8) | ${col} }}

Fn Tap
    [Arguments]    ${pos}
    Key    ${FN}    1
    Run For    0.03
    Key    ${pos}    1
    Run For    0.03
    Key    ${pos}    0
    Run For    0.03
    Key    ${FN}    0
    Run For    0.03

Count Sleeps
    ${import}=    Set Variable    import sys; sys.path.append('${TESTS}'); import powercount as k
    Execute Command    cpu AddHookAtWfiStateChange "${import}; k.sleep() if isInWfi else None"
    ${a}=    Sym    SysTick_Handler
    Execute Command    cpu AddHook ${a} "${import}; k.hit('SysTick')"

Instructions
    ${v}=    Execute Command    cpu ExecutedInstructions
    RETURN    ${{ int($v.strip(), 0) }}

Measure
    [Arguments]    ${label}    ${seconds}=1
    Execute Command    python "import sys; sys.path.append('${TESTS}'); import powercount as k; k.clear()"
    ${before}=    Instructions
    Run For    ${seconds}
    ${after}=    Instructions
    ${out}=    Execute Command    python "import powercount as k; print(k.report())"
    ${line}=    Evaluate    "QMKPOWER %s: %.2f M insns/s; %s" % ($label, (${after} - ${before}) / ${seconds} / 1e6, $out.strip())
    Log To Console    \n${line}

*** Test Cases ***
QMK Lights On
    Boot
    Count Sleeps
    Measure    lights on, idle    0.2

QMK Lights Off
    Boot
    Fn Tap    ${RGB_TOG}
    Run For    0.2
    Count Sleeps
    Measure    lights off, idle    0.2

QMK Key Held
    Boot
    Key    ${KEY_A}    1
    Run For    0.1
    Count Sleeps
    Measure    key held    0.2
