*** Comments ***
M3: USB through musb, with the m3-usb image (CDC-ACM log and commands plus
an HID boot keyboard) on the USB model's scripted host (sim/peripherals/usb.py).
tests/qmk_reference.robot runs the QMK build on the same model.
"In DFU" means the CPU is parked in the ROM stub at 0x1FFFE008 (see sim/akko.resc).

*** Settings ***
Library             Process
Library             OperatingSystem

*** Variables ***
${SIM}              ${CURDIR}/..
${M3}               ${CURDIR}/../../target/thumbv7m-none-eabi/release/m3-usb
${GOLDEN_USB}       ${CURDIR}/../golden/qmk_usb_init.txt
${USB_INIT_REGS}    ^(RCC\\+0x(014|034|040|06c|07c|088)|SFM)
${ROM_PARKED}       0x1FFFE008
${HOST_CMD}         0xF00
${HOST_STATE}       0xF04
${GPIOB_ODR}        0x40000414
${GPIOC_ODR}        0x40000814
${WIN_LOCK_LED}     0x01
${CAPS_LED}         0x20
${GPIO_KEY}         0x40000FF0
${ESC_DOWN}         0x10000
${AIRCR}            0xE000ED0C
${SYSRESETREQ}      0x05FA0004

*** Keywords ***
Boot
    Create Log Tester    1
    Execute Command    include @${SIM}/akko.resc
    Execute Command    sysbus LoadELF @${M3}
    Execute Command    machine Reset

Run For
    [Arguments]    ${seconds}
    Execute Command    emulation RunFor "${seconds}"

Host
    [Arguments]    ${cmd}
    Execute Command    usb WriteDoubleWord ${HOST_CMD} ${cmd}

Host State Should Be
    [Arguments]    ${expected}
    ${v}=    Execute Command    usb ReadDoubleWord ${HOST_STATE}
    Should Be Equal As Integers    ${v.strip()}    ${expected}

Boot And Enumerate
    Boot
    Run For    0.3
    Host State Should Be    2

Send Line
    [Documentation]    Types ${text} and CR into the CDC port, one OUT packet per byte.
    [Arguments]    ${text}
    ${codes}=    Evaluate    [(ord(c) << 8) | 0x02 for c in $text]
    FOR    ${cmd}    IN    @{codes}
        Host    ${cmd}
    END
    Host    0x0D02

CDC Should Show
    [Arguments]    ${pattern}
    Wait For Log Entry    HOST CDC: .*${pattern}    timeout=2    treatAsRegex=true    pauseEmulation=true

LED Is On
    [Arguments]    ${odr}    ${bit}
    ${v}=    Execute Command    sysbus ReadDoubleWord ${odr}
    ${on}=    Evaluate    (int("${v.strip()}", 0) & ${bit}) == 0
    RETURN    ${on}

PC Should Be In ROM
    ${pc}=    Execute Command    cpu PC
    Should Be Equal As Integers    ${pc.strip()}    ${ROM_PARKED}

PC Should Be In Flash
    ${pc}=    Execute Command    cpu PC
    ${pc}=    Convert To Integer    ${pc.strip()}
    Should Be True    0x08000000 <= ${pc} < 0x08040000    PC 0x${pc} is not in flash

No Host Errors
    Should Not Be In Log    HOST ERROR
    Should Not Be In Log    LOCKED
    Should Not Be In Log    non existing peripheral

*** Test Cases ***
USB Bring-Up Matches QMK
    # Clocks, reset, FIFO clock, PHY and the 48 MHz prescaler, as ChibiOS's wb32_usb_init.
    ${trace}=    Run Process    ${SIM}/trace.sh    ${M3}    0.3    ${USB_INIT_REGS}
    ${golden}=    Get File    ${GOLDEN_USB}
    Should Be Equal    ${trace.stdout.strip()}    ${golden.strip()}

Enumerates As A CDC-ACM And HID Composite Device
    Boot And Enumerate
    Wait For Log Entry    HOST: device 1209:0001, EP0 64 bytes
    Wait For Log Entry    HOST: string 2 = '5075B Plus M3 USB test'
    # IN and OUT of one number are separate FIFOs: CDC data OUT and the
    # notification IN share EP1, the keyboard's OUT reuses EP2.
    Wait For Log Entry    HOST: configured: if 0 class 02 eps [81], if 1 class 0a eps [01 82], if 2 class 03 eps [83 02]
    No Host Errors

Attaches 50 ms After Start
    # The bus is held disconnected for at least 50 ms first, like QMK's
    # init_usb_driver. It is a busy-wait (cortex_m::asm::delay, n loop
    # iterations of 2 instructions for n cycles), so it never ends early but
    # runs long: ~96 ms on Renode's 100 MIPS, ~75 ms on the chip.
    Boot
    Run For    0.049
    Host State Should Be    0
    Run For    0.1
    Host State Should Be    1

Log Starts With The Boot Banner
    Boot And Enumerate
    Host    0x01
    CDC Should Show    \\[\\s+0\\] m3-usb: boot
    CDC Should Show    \\[\\s+0\\] no panic record from the previous run
    CDC Should Show    \\[\\s+\\d+\\] usb: address 5
    CDC Should Show    \\[\\s+\\d+\\] usb: configured
    CDC Should Show    \\[\\s+\\d+\\] cdc: port opened
    No Host Errors

Info Command Answers
    Boot And Enumerate
    Host    0x01
    Send Line    info
    Run For    0.05
    CDC Should Show    m3-usb: up \\d+ ms, configured true, suspended false
    Send Line    bogus
    Run For    0.05
    CDC Should Show    unknown command "bogus" \\(info, type, panic, dfu\\)
    No Host Errors

Type Command Types m3 ok And Enter
    Boot And Enumerate
    Send Line    type
    Run For    0.1
    # The log line comes before the host has read the last key-up, so check
    # it first and keep the HID lines it passes over.
    Wait For Log Entry    HOST CDC: .*hid: typed 6 keys    treatAsRegex=true    keep=true
    FOR    ${key}    IN    10    20    2c    12    0e    28
        Wait For Log Entry    HOST HID 3: 00 00 ${key} 00 00 00 00 00
        Wait For Log Entry    HOST HID 3: 00 00 00 00 00 00 00 00
    END
    No Host Errors

Caps Lock LED Shows The Time Base Until The Host Takes Over
    Boot
    Run For    0.01
    ${on}=    LED Is On    ${GPIOC_ODR}    ${CAPS_LED}
    Should Be True    ${on}    Caps Lock LED is off: TIM2 counter not moving
    Run For    0.3
    Host State Should Be    2
    Host    0x0003
    Run For    0.02
    ${on}=    LED Is On    ${GPIOC_ODR}    ${CAPS_LED}
    Should Not Be True    ${on}
    CDC Should Show    tim2: counter \\d+ -> \\d+ across 1 ms of CPU cycles

Host Caps Lock Drives The Caps Lock LED
    Boot And Enumerate
    Host    0x0203
    Run For    0.02
    ${on}=    LED Is On    ${GPIOC_ODR}    ${CAPS_LED}
    Should Be True    ${on}
    Host    0x0003
    Run For    0.02
    ${on}=    LED Is On    ${GPIOC_ODR}    ${CAPS_LED}
    Should Not Be True    ${on}
    No Host Errors

Suspend And Resume Are Reported In Order
    Boot And Enumerate
    ${on}=    LED Is On    ${GPIOB_ODR}    ${WIN_LOCK_LED}
    Should Be True    ${on}    Win Lock LED is off while configured
    Host    0x04
    Run For    0.02
    Host State Should Be    3
    ${on}=    LED Is On    ${GPIOB_ODR}    ${WIN_LOCK_LED}
    Should Not Be True    ${on}    Win Lock LED is on while suspended
    Host    0x05
    Run For    0.05
    ${on}=    LED Is On    ${GPIOB_ODR}    ${WIN_LOCK_LED}
    Should Be True    ${on}    Win Lock LED is off after resume
    Host    0x01
    Run For    0.02
    CDC Should Show    \\[\\s+\\d+\\] usb: suspended
    CDC Should Show    \\[\\s+\\d+\\] usb: resumed
    Send Line    info
    Run For    0.05
    CDC Should Show    configured true, suspended false
    No Host Errors

Suspend Works After Every Bus Reset
    # A bus reset clears POWER.SUSEN and INTRUSBE.SUSIE; the driver sets them again.
    Boot And Enumerate
    Host    0x06
    Run For    0.3
    Host State Should Be    2
    Host    0x04
    Run For    0.02
    Host State Should Be    3
    Should Not Be In Log    did not detect suspend
    No Host Errors

dfu Command Enters DFU
    Boot And Enumerate
    Send Line    dfu
    Run For    0.01
    PC Should Be In ROM

Esc Enters DFU While Configured
    Boot And Enumerate
    Execute Command    sysbus WriteDoubleWord ${GPIO_KEY} ${ESC_DOWN}
    Run For    0.002
    PC Should Be In ROM

A Panic Enters DFU And Is Reported After The Next Boot
    Boot And Enumerate
    Send Line    panic
    Run For    0.01
    PC Should Be In ROM
    # What wb32-dfu-updater_cli -R does: a system reset, RAM kept.
    Execute Command    sysbus WriteDoubleWord ${AIRCR} ${SYSRESETREQ}
    Run For    0.3
    Host State Should Be    2
    Host    0x01
    CDC Should Show    previous run panicked: akko-5075b-plus/src/bin/m3-usb.rs:\\d+:\\d+: requested over USB
    # Reported once only: the boot after that finds no record.
    Execute Command    sysbus WriteDoubleWord ${AIRCR} ${SYSRESETREQ}
    Run For    0.3
    Host    0x01
    Run For    0.02
    CDC Should Show    \\[\\s+0\\] no panic record from the previous run

Without A Host It Returns To DFU After 30 s
    Boot
    Host    0x07
    Run For    29.9
    PC Should Be In Flash
    Run For    0.2
    PC Should Be In ROM

Configured Stays Up Past 30 s
    Boot And Enumerate
    Run For    31
    PC Should Be In Flash
    Host State Should Be    2
