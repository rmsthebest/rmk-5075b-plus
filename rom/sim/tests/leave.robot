*** Comments ***
Scenario 3: leaving the bootloader, with GO (jump to the app) and RESET
(the updater's -R).

*** Settings ***
Resource            rom.resource
Test Setup          Start The ROM

*** Variables ***
@{TINY_APP}         0x00    0x10    0x00    0x20    0x09    0x00    0x00    0x08
...                 0xFE    0xE7    0x00    0x00    0x00    0x00    0x00    0x00

*** Keywords ***
Download The Tiny App
    Command    0x5E    0    ${APP}    1
    Reply Should Be    5e 00
    Command    0x6A    0    ${APP}    16    @{TINY_APP}
    Reply Should Be    6a 00

*** Test Cases ***
GO Jumps To The App With VTOR At 0
    [Documentation]    9b .. <addr> <ms>: acks, takes USB down (the host sees a
    ...    detach), sets VTOR = 0 and MSP = [addr], jumps to [addr + 4].
    Wait For Enumeration
    Download The Tiny App
    Command    0x9B    0    ${APP}    0
    Reply Should Be    9b 00
    Wait For Log Entry    HOST: device detached    timeout=1    pauseEmulation=true
    Execute Command    emulation RunFor "0.05"
    ${pc}=    Execute Command    cpu PC
    Should Be Equal As Integers    ${pc.strip()}    0x08000008
    ${sp}=    Execute Command    cpu SP
    Should Be Equal As Integers    ${sp.strip()}    0x20001000
    ${vtor}=    Execute Command    sysbus ReadDoubleWord 0xE000ED08
    Should Be Equal As Integers    ${vtor.strip()}    0

GO Outside Flash Is Refused
    Wait For Enumeration
    Command    0x9B    0    0x20000000    0
    Reply Should Be    9b 01

RESET Resets The Chip
    [Documentation]    83 .. <ms>: acks, takes USB down, waits, then SYSRESETREQ.
    ...    Booting from flash (BOOT0 = 0) then runs the downloaded app.
    Wait For Enumeration
    Download The Tiny App
    Command    0x83    0    10
    Reply Should Be    83 00
    Wait For Log Entry    HOST: device detached    timeout=1    pauseEmulation=true
    Execute Command    emulation RunFor "0.1"
    ${pc}=    Execute Command    cpu PC
    Should Be Equal As Integers    ${pc.strip()}    0x08000008
