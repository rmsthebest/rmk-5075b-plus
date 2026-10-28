*** Comments ***
Scenario 2: a download into the sim's flash through ERASE, WRITE_PAGE and
READ, as wb32-dfu-updater_cli -D does, and the address checks.

*** Settings ***
Resource            rom.resource
Test Setup          Start The ROM

*** Variables ***
# A 16-byte app: SP 0x20001000, reset 0x08000009, then `b .` at 0x08000008.
@{TINY_APP}         0x00    0x10    0x00    0x20    0x09    0x00    0x00    0x08
...                 0xFE    0xE7    0x00    0x00    0x00    0x00    0x00    0x00

*** Test Cases ***
A Page Is Erased, Written And Read Back
    Wait For Enumeration
    # The FMC line comes before the reply, and the log tester only matches
    # after its previous match.
    Command    0x5E    0    ${APP}    1
    Wait For Log Entry    fmc: FMC op 0x08 at 0x08000000    timeout=1    pauseEmulation=true
    Reply Should Be    5e 00
    Command    0x6A    0    ${APP}    16    @{TINY_APP}
    Wait For Log Entry    fmc: FMC op 0x0c at 0x08000000    timeout=1    pauseEmulation=true
    Reply Should Be    6a 00
    ${v}=    Execute Command    sysbus ReadDoubleWord 0x08000004
    Should Be Equal As Integers    ${v.strip()}    0x08000009
    Command    0x72    0    ${APP}    16
    ${hex}=    Hex    @{TINY_APP}
    Reply Should Be    72 00 ${hex}

Reads Outside Flash And Blocks 4-7 Are Refused
    [Documentation]    The ROM itself and the factory blocks 0-3 aren't reachable
    ...    (address check 0x1FFFE8CC): ARGERR.
    Wait For Enumeration
    Command    0x72    0    0x1FFFE000    16
    Reply Should Be    72 01
    Command    0x72    0    0x1FFFF000    16
    Reply Should Be    72 01
    Command    0x72    0    0x1FFFF400    4
    Reply Should Be    72 00 00 00 00 00

An Erase Outside Flash Is Refused
    Wait For Enumeration
    Command    0x5E    0    0x1FFFF400    1
    Reply Should Be    5e 01
    Should Not Be In Log    FMC op    timeout=0
