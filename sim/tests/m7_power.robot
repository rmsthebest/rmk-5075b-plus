*** Comments ***
M7: what the firmware costs while idle, and how soon a key reaches USB.
The yardstick for making the firmware event-driven: Renode halts the CPU at
WFI/WFE, so the instructions executed per second of emulated time stand for
the time the CPU is awake, and hooks count how often each interrupt handler
runs and how often the CPU goes back to sleep (powercount.py). The numbers
are logged (Log To Console) and held under ${MAX_*} bounds, which are the
measured values with some headroom: lower them as the firmware gets quieter.
Latency is the time from a key's press to the device arming the keyboard's
IN endpoint (usb.py 0xF14), in the USB model's 100 us steps; the host's
1 ms polling comes on top.

*** Settings ***
Library             Process
Library             OperatingSystem
Library             Collections

*** Variables ***
${SIM}              ${CURDIR}/..
${RMK}              ${CURDIR}/../../target/thumbv7m-none-eabi/release/akko-rmk
${GPIO_KEY}         0x40000FF0
${FLASH_ERASE}      0xF8
${HOST_STEP}        0xF08
${HOST_READY}       0xF14
${KEYBOARD_EP}      1
${FN}               5 10
${RGB_TOG}          3 9
${KEY_A}            3 1
${DONGLE}           2 6
${CONNECTED}        0x325B
${HOST_SUSPEND}     0x0060
${SEL_DONGLE}       a6 30 d6
${ALL_UP}           a1 00 00 00 00 00 00 00 00 a1
@{HANDLERS}         SysTick    TIM2    EXTI0    EXTI1    EXTI2    EXTI3    EXTI4
...                 EXTI9_5    EXTI15_10    USB    UART3    SPIM2
# Bounds: per emulated second. Baseline 2026-10-29 (rmk-main b8106ae): lights
# on 30.5 M insns, 943 sleeps; off 24.6 M, 1003; asleep 1.14 M, 1004 (SysTick
# 1000); key held 36.0 M. Press 3-12 steps (mean 7.6), release 38-51.
# Parked scan: on 7.56 M, off 1.37 M, asleep 0.98 M; press 3-4 steps.
# Tickless SysTick: on 8.49 M, 280 sleeps; off 1.88 M, 297; asleep 1.06 M,
# 101; press 3 steps. Fewer wakes, but each costs more: every wake polls all
# of main's join (the knob's 5 ms poll and the 16 ms frames dominate).
# No polls (knob, radio 50 ms, suspended frames 250 ms, heartbeat 500 ms):
# on 7.55 M, 74 sleeps; off 0.90 M, 75 (the 16 ms frames); asleep 0.12 M, 18.
# Our runnables as tasks (a wake polls only its task): on 6.00 M, off 0.21 M,
# asleep 0.07 M, key held 33.4 M; press 3-4 steps.
${MAX_INSNS_LIGHTS_ON}      6800000
${MAX_WAKES_LIGHTS_ON}      90
${MAX_INSNS_LIGHTS_OFF}     300000
${MAX_WAKES_LIGHTS_OFF}     90
${MAX_INSNS_ASLEEP}         100000
${MAX_WAKES_ASLEEP}         25
${MAX_INSNS_KEY_HELD}       37000000
# Press (release) to IN endpoint armed, in 100 us steps: worst over the
# presses in `Press To Report Latency`. Releases wait out the 5 ms debounce.
${MAX_PRESS_WORST}          4
${MAX_RELEASE_WORST}        55

*** Keywords ***
Boot
    Create Log Tester    3
    Execute Command    include @${SIM}/akko.resc
    Execute Command    logLevel 3 gpio
    Execute Command    qspi WriteDoubleWord ${FLASH_ERASE} 1
    Execute Command    sysbus LoadELF @${RMK}
    Execute Command    machine Reset
    Wait For Log Entry    HOST: configured    timeout=4    pauseEmulation=true
    # Past the USB key's 2 s of white and the first settings save.
    Run For    4

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

Count Hooks
    [Documentation]    Counts every handler in @{HANDLERS} and every sleep.
    ${import}=    Set Variable    import sys; sys.path.append('${CURDIR}'); import powercount as k
    # From nm, as m4_rmk's Symbol (Run Process hangs on nm's output here).
    ${syms}=    Evaluate    {l.split()[2]: int(l.split()[0], 16) for l in subprocess.run(["arm-none-eabi-nm", $RMK], capture_output=True, text=True).stdout.splitlines() if len(l.split()) == 3}
    ...    modules=subprocess
    FOR    ${name}    IN    @{HANDLERS}
        Execute Command    cpu AddHook ${syms}[${name}] "${import}; k.hit('${name}')"
    END
    Execute Command    cpu AddHookAtWfiStateChange "${import}; k.sleep() if isInWfi else None"

Instructions
    ${v}=    Execute Command    cpu ExecutedInstructions
    RETURN    ${{ int($v.strip(), 0) }}

Measure
    [Documentation]    Runs ${seconds} and returns a dict per second:
    ...    insns, sleeps and each handler's count.
    [Arguments]    ${label}    ${seconds}=1
    Execute Command    python "import sys; sys.path.append('${CURDIR}'); import powercount as k; k.clear()"
    ${before}=    Instructions
    Run For    ${seconds}
    ${after}=    Instructions
    ${out}=    Execute Command    python "import powercount as k; print(k.report())"
    ${counts}=    Evaluate    {k: int(v) / ${seconds} for k, v in (f.split("=") for f in $out.split())}
    Set To Dictionary    ${counts}    insns=${{ (${after} - ${before}) / ${seconds} }}
    ${sleeps}=    Evaluate    $counts.get("sleeps", 0)
    Set To Dictionary    ${counts}    sleeps=${sleeps}
    ${line}=    Evaluate    "POWER %s: %.2f M insns/s, %d sleeps/s; " % ($label, $counts["insns"] / 1e6, $counts["sleeps"]) + ", ".join("%s %d" % (k, v) for k, v in sorted($counts.items()) if k not in ("insns", "sleeps"))
    Log To Console    \n${line}
    Log    ${line}
    RETURN    ${counts}

On The Dongle Asleep
    [Documentation]    Fn+Y, the link up, then the host suspends: asleep.
    Fn Tap    ${DONGLE}
    Wait For Log Entry    radio: <- ${SEL_DONGLE}    timeout=1    pauseEmulation=true
    Execute Command    uart3 WriteDoubleWord 0xE4 ${CONNECTED}
    Wait For Log Entry    radio: <- ${ALL_UP}    timeout=1    pauseEmulation=true
    Run For    2.5
    Execute Command    uart3 WriteDoubleWord 0xE4 ${HOST_SUSPEND}
    Run For    0.5

Ready Step
    Execute Command    usb WriteDoubleWord ${HOST_READY} ${KEYBOARD_EP}
    ${v}=    Execute Command    usb ReadDoubleWord ${HOST_READY}
    RETURN    ${{ int($v.strip(), 0) }}

Host Step
    ${v}=    Execute Command    usb ReadDoubleWord ${HOST_STEP}
    RETURN    ${{ int($v.strip(), 0) }}

Press Latency
    [Documentation]    Presses (or releases) ${pos}; the steps until the
    ...    keyboard endpoint is armed with a new report.
    [Arguments]    ${pos}    ${down}
    ${before}=    Ready Step
    ${pressed}=    Host Step
    Key    ${pos}    ${down}
    FOR    ${_}    IN RANGE    100
        Run For    0.0001
        ${ready}=    Ready Step
        IF    ${ready} != ${before} and ${ready} != 0xFFFFFFFF    BREAK
    END
    Should Be True    ${ready} != ${before}    no report within 10 ms
    RETURN    ${{ ${ready} - ${pressed} }}

*** Test Cases ***
Idle Power With The Lights On
    Boot
    Count Hooks
    ${c}=    Measure    lights on, idle
    Should Be True    ${c}[insns] <= ${MAX_INSNS_LIGHTS_ON}
    Should Be True    ${c}[sleeps] <= ${MAX_WAKES_LIGHTS_ON}

Idle Power With The Lights Off
    Boot
    Fn Tap    ${RGB_TOG}
    Run For    1
    Count Hooks
    ${c}=    Measure    lights off, idle
    Should Be True    ${c}[insns] <= ${MAX_INSNS_LIGHTS_OFF}
    Should Be True    ${c}[sleeps] <= ${MAX_WAKES_LIGHTS_OFF}

Idle Power Asleep On The Dongle
    Boot
    On The Dongle Asleep
    Count Hooks
    ${c}=    Measure    asleep on the dongle
    Should Be True    ${c}[insns] <= ${MAX_INSNS_ASLEEP}
    Should Be True    ${c}[sleeps] <= ${MAX_WAKES_ASLEEP}

Power With A Key Held
    Boot
    Key    ${KEY_A}    1
    Run For    0.1
    Count Hooks
    ${c}=    Measure    key held
    Key    ${KEY_A}    0
    Should Be True    ${c}[insns] <= ${MAX_INSNS_KEY_HELD}

Press To Report Latency
    [Documentation]    Presses and releases at 10 phases of the millisecond,
    ...    after idling (so an event-driven scan is parked when they come).
    Boot
    @{presses}=    Create List
    @{releases}=    Create List
    FOR    ${phase}    IN RANGE    10
        Run For    ${{ 0.05 + ${phase} * 0.0001 }}
        ${p}=    Press Latency    ${KEY_A}    1
        Run For    0.03
        ${r}=    Press Latency    ${KEY_A}    0
        Append To List    ${presses}    ${p}
        Append To List    ${releases}    ${r}
    END
    ${line}=    Evaluate    "LATENCY (100 us steps) press %s worst %d mean %.1f; release %s worst %d mean %.1f" % ($presses, max($presses), sum($presses) / 10, $releases, max($releases), sum($releases) / 10)
    Log To Console    \n${line}
    Log    ${line}
    Should Be True    max($presses) <= ${MAX_PRESS_WORST}
    Should Be True    max($releases) <= ${MAX_RELEASE_WORST}
