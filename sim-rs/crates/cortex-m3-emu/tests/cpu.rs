//! Instruction and exception semantics on a bare test bus: flash at 0
//! (vector table, then code at 0x40), SRAM at 0x2000_0000. Programs are
//! hand-encoded; each encoding is checked with the disassembler (itself
//! checked against LLVM), so a slip in a test can't test the wrong thing.

use cortex_m3_emu::cpu::{C, N, V, Z};
use cortex_m3_emu::{Bus, Config, CoreDiag, Cpu, Memory, Size, decode, disasm};

struct TestBus {
    flash: Memory,
    ram: Memory,
    irq: u64,
}

impl Bus for TestBus {
    fn read(&mut self, addr: u32, size: Size) -> u32 {
        match addr {
            0..0x1000 => self.flash.read(addr, size),
            0x2000_0000..0x2000_1000 => self.ram.read(addr - 0x2000_0000, size),
            _ => panic!("read 0x{addr:08x}"),
        }
    }
    fn write(&mut self, addr: u32, size: Size, value: u32) {
        match addr {
            0x2000_0000..0x2000_1000 => self.ram.write(addr - 0x2000_0000, size, value),
            _ => panic!("write 0x{addr:08x}"),
        }
    }
    fn fetch(&mut self, addr: u32) -> Option<u16> {
        (addr < 0x1000).then(|| self.flash.read(addr, Size::Half) as u16)
    }
    fn irq_lines(&mut self) -> cortex_m3_emu::IrqLines {
        cortex_m3_emu::IrqLines {
            level: self.irq,
            raised: 0,
        }
    }
}

const CODE: u32 = 0x40;
const STACK: u32 = 0x2000_0800;

/// Assembles `(encoding, expected disassembly)` pairs at `at`; 32-bit
/// encodings are written high halfword first, as the ARM ARM shows them.
fn put(bus: &mut TestBus, at: u32, code: &[(u32, &str)]) -> u32 {
    let mut a = at;
    for &(enc, text) in code {
        let (h1, h2, len) = if enc > 0xFFFF {
            ((enc >> 16) as u16, enc as u16, 4)
        } else {
            (enc as u16, 0, 2)
        };
        let insn = decode(h1, h2, false);
        if !text.is_empty() {
            // "" marks data.
            assert_eq!(insn.len, len, "{text}: length");
            assert_eq!(
                disasm(&insn, a, None),
                text,
                "encoding 0x{enc:x} at 0x{a:x}"
            );
        }
        bus.flash.write(a, Size::Half, h1 as u32);
        if len == 4 {
            bus.flash.write(a + 2, Size::Half, h2 as u32);
        }
        a += len as u32;
    }
    a
}

fn machine(code: &[(u32, &str)]) -> (Cpu, TestBus) {
    machine_at(CODE, code)
}

/// With the code elsewhere, for tests whose IRQ vectors would cover 0x40.
fn machine_at(at: u32, code: &[(u32, &str)]) -> (Cpu, TestBus) {
    let mut bus = TestBus {
        flash: Memory::new(0x1000),
        ram: Memory::new(0x1000),
        irq: 0,
    };
    bus.flash.write(0, Size::Word, STACK);
    bus.flash.write(4, Size::Word, at | 1);
    put(&mut bus, at, code);
    let mut cpu = Cpu::new(Config::default());
    cpu.reset(&mut bus);
    (cpu, bus)
}

fn steps(cpu: &mut Cpu, bus: &mut TestBus, n: usize) {
    for _ in 0..n {
        cpu.step(bus);
    }
    assert!(
        cpu.diags.is_empty(),
        "{:?}",
        cpu.diags.iter().map(|d| d.to_string()).collect::<Vec<_>>()
    );
}

fn flags(cpu: &Cpu) -> String {
    [(N, 'N'), (Z, 'Z'), (C, 'C'), (V, 'V')]
        .iter()
        .map(|&(f, ch)| if cpu.apsr & f != 0 { ch } else { '-' })
        .collect()
}

#[test]
fn reset_takes_sp_and_pc_from_the_vector_table() {
    let (cpu, _) = machine(&[(0xE7FE, "b 0x40")]);
    assert_eq!(cpu.r[13], STACK);
    assert_eq!(cpu.pc(), CODE);
    assert_eq!(cpu.r[14], 0xFFFF_FFFF);
}

#[test]
fn add_and_subtract_flags() {
    let (mut cpu, mut bus) = machine(&[
        (0x2001, "movs r0, #0x1"),
        (0x3802, "subs r0, #0x2"),
        (0xF06F_4000, "mvn r0, #0x80000000"),
        (0x3001, "adds r0, #0x1"),
        (0x3801, "subs r0, #0x1"),
        (0x2101, "movs r1, #0x1"),
        (0x1A40, "subs r0, r0, r1"),
    ]);
    steps(&mut cpu, &mut bus, 2);
    assert_eq!(
        (cpu.r[0], flags(&cpu).as_str()),
        (0xFFFF_FFFF, "N---"),
        "1 - 2 borrows"
    );
    steps(&mut cpu, &mut bus, 2);
    assert_eq!(
        (cpu.r[0], flags(&cpu).as_str()),
        (0x8000_0000, "N--V"),
        "INT_MAX + 1 overflows"
    );
    steps(&mut cpu, &mut bus, 1);
    assert_eq!(
        (cpu.r[0], flags(&cpu).as_str()),
        (0x7FFF_FFFF, "--CV"),
        "INT_MIN - 1 overflows, no borrow"
    );
    steps(&mut cpu, &mut bus, 2);
    assert_eq!((cpu.r[0], flags(&cpu).as_str()), (0x7FFF_FFFE, "--C-"));
}

#[test]
fn shifts_set_carry_from_the_last_bit_out() {
    let (mut cpu, mut bus) = machine(&[
        (0x2003, "movs r0, #0x3"),
        (0x0840, "lsrs r0, r0, #0x1"),
        (0x0840, "lsrs r0, r0, #0x1"),
        (0x0840, "lsrs r0, r0, #0x1"),
        (0x2101, "movs r1, #0x1"),
        (0x07C9, "lsls r1, r1, #0x1f"),
        (0x0049, "lsls r1, r1, #0x1"),
    ]);
    steps(&mut cpu, &mut bus, 2);
    assert_eq!((cpu.r[0], flags(&cpu).as_str()), (1, "--C-"));
    steps(&mut cpu, &mut bus, 1);
    assert_eq!((cpu.r[0], flags(&cpu).as_str()), (0, "-ZC-"));
    steps(&mut cpu, &mut bus, 1);
    assert_eq!((cpu.r[0], flags(&cpu).as_str()), (0, "-Z--"));
    steps(&mut cpu, &mut bus, 2);
    assert_eq!((cpu.r[1], flags(&cpu).as_str()), (0x8000_0000, "N---"));
    steps(&mut cpu, &mut bus, 1);
    assert_eq!((cpu.r[1], flags(&cpu).as_str()), (0, "-ZC-"));
}

#[test]
fn it_block_runs_one_side() {
    for (r0, want) in [(0, 1), (5, 2)] {
        let (mut cpu, mut bus) = machine(&[
            (0x2800, "cmp r0, #0x0"),
            (0xBF0C, "ite eq"),
            (0x2101, "movs r1, #0x1"), // moveq in the block
            (0x2102, "movs r1, #0x2"), // movne
            (0x2207, "movs r2, #0x7"), // after the block: flag-setting again
        ]);
        cpu.r[0] = r0;
        steps(&mut cpu, &mut bus, 4);
        assert_eq!(cpu.r[1], want);
        let after_cmp = if r0 == 0 { "-ZC-" } else { "--C-" };
        assert_eq!(
            flags(&cpu),
            after_cmp,
            "the 16-bit movs in the block don't set flags"
        );
        steps(&mut cpu, &mut bus, 1);
        assert_eq!(cpu.r[2], 7);
        assert_eq!(flags(&cpu), "--C-", "after the block they do");
        assert!(!cpu.it.in_block());
    }
}

#[test]
fn call_push_pop_return() {
    let (mut cpu, mut bus) = machine(&[
        (0xF000_F802, "bl 0x48"),
        (0x2005, "movs r0, #0x5"),
        (0xE7FE, "b 0x46"),
        (0xB510, "push {r4, lr}"),
        (0x2409, "movs r4, #0x9"),
        (0x0020, "movs r0, r4"),
        (0xBD10, "pop {r4, pc}"),
    ]);
    cpu.r[4] = 0x1234;
    steps(&mut cpu, &mut bus, 6);
    assert_eq!(cpu.r[0], 5);
    assert_eq!(cpu.r[4], 0x1234, "r4 restored");
    assert_eq!(cpu.r[13], STACK);
    assert_eq!(cpu.pc(), 0x46);
}

#[test]
fn ldm_stm_and_writeback() {
    let (mut cpu, mut bus) = machine(&[
        (0xC00E, "stm r0!, {r1, r2, r3}"),
        (0x3808, "subs r0, #0x8"),
        (0xC818, "ldm r0!, {r3, r4}"),
        (0xE910_0060, "ldmdb r0, {r5, r6}"),
    ]);
    cpu.r[0] = 0x2000_0100;
    (cpu.r[1], cpu.r[2], cpu.r[3]) = (11, 22, 33);
    steps(&mut cpu, &mut bus, 4);
    assert_eq!(cpu.r[0], 0x2000_010C);
    assert_eq!((cpu.r[3], cpu.r[4]), (22, 33));
    assert_eq!((cpu.r[5], cpu.r[6]), (22, 33));
}

#[test]
fn table_branch_and_cbz() {
    let (mut cpu, mut bus) = machine(&[
        (0xE8DF_F001, "tbb [pc, r1]"),
        (0x0201, ""), // table: entries 1, 2 (halfwords forward from 0x44)
        (0x2001, "movs r0, #0x1"),
        (0x2002, "movs r0, #0x2"),
        (0xB108, "cbz r0, 0x50"),
        (0xE7FE, "b 0x4c"),
    ]);
    cpu.r[1] = 1; // entry 1 = 2 halfwords: skips to 0x48
    steps(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.r[0], 2);
    steps(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.pc(), 0x4C, "r0 != 0: cbz falls through");
}

#[test]
fn divide_and_long_multiply() {
    let (mut cpu, mut bus) = machine(&[
        (0xFBB0_F2F1, "udiv r2, r0, r1"),
        (0xFB90_F3F1, "sdiv r3, r0, r1"),
        (0xFBA0_4501, "umull r4, r5, r0, r1"),
        (0xFB80_6701, "smull r6, r7, r0, r1"),
        (0xFBB0_F2F8, "udiv r2, r0, r8"),
    ]);
    (cpu.r[0], cpu.r[1]) = ((-7i32) as u32, 2);
    steps(&mut cpu, &mut bus, 5);
    assert_eq!(cpu.r[2], 0, "divide by zero gives 0 without DIV_0_TRP");
    assert_eq!(cpu.r[3] as i32, -3, "rounds toward zero");
    assert_eq!((cpu.r[5], cpu.r[4]), (1, 0xFFFF_FFF2));
    assert_eq!(((cpu.r[7] as u64) << 32 | cpu.r[6] as u64) as i64, -14);
}

/// SysTick at RVR 99: a tick every 100 cycles of 96 MHz. The handler counts
/// in r4; the main loop sleeps in WFI.
fn systick_machine() -> (Cpu, TestBus) {
    let (cpu, mut bus) = machine(&[
        (0xF24E_0010, "movw r0, #0xe010"),
        (0xF2CE_0000, "movt r0, #0xe000"),
        (0x2163, "movs r1, #0x63"),
        (0x6041, "str r1, [r0, #0x4]"),
        (0x2107, "movs r1, #0x7"),
        (0x6001, "str r1, [r0]"),
        (0xBF30, "wfi"),
        (0xE7FD, "b 0x50"),
    ]);
    put(
        &mut bus,
        0x80,
        &[(0x3401, "adds r4, #0x1"), (0x4770, "bx lr")],
    );
    bus.flash.write(15 * 4, Size::Word, 0x81);
    let mut cpu = cpu;
    cpu.reset(&mut bus);
    (cpu, bus)
}

#[test]
fn systick_interrupts_wake_wfi() {
    let (mut cpu, mut bus) = systick_machine();
    let period = 100 * Config::default().systick_ticks;
    cpu.run_until(&mut bus, 10 * period + period / 2);
    assert!(cpu.diags.is_empty(), "{:?}", cpu.diags);
    assert_eq!(cpu.r[4], 10, "ten ticks");
    // Asleep most of the time: per tick, the handler (2), the loop's branch
    // and WFI (2) and the wake. Far fewer instructions than cycles.
    assert!(cpu.instructions < 60, "{} instructions", cpu.instructions);
    assert_eq!(cpu.r[13], STACK, "stack balanced");
    assert_eq!(cpu.ipsr, 0);
}

#[test]
fn exception_frame_realigns_the_stack() {
    let (mut cpu, mut bus) = systick_machine();
    cpu.r[13] = STACK - 4; // not 8-byte aligned
    let period = 100 * Config::default().systick_ticks;
    cpu.run_until(&mut bus, period + period / 2);
    assert_eq!(cpu.r[4], 1);
    assert_eq!(
        cpu.r[13],
        STACK - 4,
        "realigned on entry, restored on return"
    );
}

#[test]
fn primask_holds_the_interrupt_and_wfi_still_wakes() {
    let (mut cpu, mut bus) = systick_machine();
    cpu.primask = true;
    let period = 100 * Config::default().systick_ticks;
    cpu.run_until(&mut bus, 3 * period + period / 2);
    assert_eq!(cpu.r[4], 0, "masked");
    assert!(cpu.ppb.pending & (1 << 15) != 0, "SysTick pending");
    cpu.primask = false;
    cpu.step(&mut bus);
    assert_eq!(cpu.ipsr, 15, "taken once unmasked");
}

#[test]
fn undefined_instruction_escalates_to_hardfault() {
    let (mut cpu, mut bus) = machine(&[(0xDE00, "udf #0x0")]);
    bus.flash.write(3 * 4, Size::Word, 0x81);
    put(&mut bus, 0x80, &[(0xE7FE, "b 0x80")]);
    cpu.step(&mut bus);
    assert_eq!(cpu.ipsr, 3, "HardFault (UsageFault disabled)");
    assert_eq!(cpu.pc(), 0x80);
    assert_ne!(cpu.ppb.hfsr & (1 << 30), 0, "FORCED");
    assert_ne!(cpu.ppb.cfsr & (1 << 16), 0, "UNDEFINSTR");
    assert!(matches!(cpu.diags[..], [CoreDiag::Fault { pc: 0x40, .. }]));
    // The stacked PC is the faulting instruction.
    assert_eq!(
        bus.ram.read(STACK - 0x20 + 24 - 0x2000_0000, Size::Word),
        0x40
    );
}

#[test]
fn external_interrupt_level_and_priority() {
    let (mut cpu, mut bus) = machine_at(
        0x100,
        &[
            (0xF24E_1100, "movw r1, #0xe100"),
            (0xF2CE_0100, "movt r1, #0xe000"),
            (0x2202, "movs r2, #0x2"),
            (0x600A, "str r2, [r1]"), // ISER0: enable IRQ 1
            (0xE7FE, "b 0x10c"),
        ],
    );
    put(
        &mut bus,
        0x80,
        &[(0x3401, "adds r4, #0x1"), (0x4770, "bx lr")],
    );
    bus.flash.write((16 + 1) * 4, Size::Word, 0x81);
    cpu.reset(&mut bus);
    bus.irq = 1 << 1; // asserted before it's enabled
    steps(&mut cpu, &mut bus, 4);
    assert_eq!(cpu.r[4], 0);
    steps(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.ipsr, 17, "taken once enabled");
    // adds, bx lr (return), and in again: the line is still high.
    steps(&mut cpu, &mut bus, 3);
    assert_eq!(
        (cpu.ipsr, cpu.r[4]),
        (17, 1),
        "level still high: pends again on return"
    );
    bus.irq = 0;
    steps(&mut cpu, &mut bus, 3);
    assert_eq!((cpu.ipsr, cpu.r[4]), (0, 2));
    assert_eq!(cpu.pc(), 0x10C);
}

#[test]
fn svc_enters_its_handler_and_returns_after_it() {
    let (mut cpu, mut bus) = machine(&[(0xDF00, "svc #0x0"), (0xE7FE, "b 0x42")]);
    put(
        &mut bus,
        0x80,
        &[(0x2401, "movs r4, #0x1"), (0x4770, "bx lr")],
    );
    bus.flash.write(11 * 4, Size::Word, 0x81);
    steps(&mut cpu, &mut bus, 1);
    assert_eq!((cpu.ipsr, cpu.pc()), (11, 0x80));
    steps(&mut cpu, &mut bus, 2);
    assert_eq!((cpu.ipsr, cpu.pc(), cpu.r[4]), (0, 0x42, 1));
}

#[test]
fn strex_fails_after_an_exception_clears_the_monitor() {
    let (mut cpu, mut bus) = machine(&[
        (0xE850_1F00, "ldrex r1, [r0]"),
        (0xE840_1200, "strex r2, r1, [r0]"),
        (0xE840_1300, "strex r3, r1, [r0]"),
        (0xE850_1F00, "ldrex r1, [r0]"),
        (0xDF00, "svc #0x0"),
        (0xE840_1500, "strex r5, r1, [r0]"),
    ]);
    put(&mut bus, 0x80, &[(0x4770, "bx lr")]);
    bus.flash.write(11 * 4, Size::Word, 0x81);
    cpu.r[0] = 0x2000_0100;
    steps(&mut cpu, &mut bus, 3);
    assert_eq!(
        (cpu.r[2], cpu.r[3]),
        (0, 1),
        "first STREX stores, second has no reservation"
    );
    steps(&mut cpu, &mut bus, 4);
    assert_eq!(cpu.r[5], 1, "the SVC's entry and return cleared it");
}

#[test]
fn wfe_consumes_a_pending_event_then_sleeps() {
    let (mut cpu, mut bus) = machine(&[
        (0xBF40, "sev"),
        (0xBF20, "wfe"),
        (0xBF20, "wfe"),
        (0xE7FE, "b 0x46"),
    ]);
    cpu.run_until(&mut bus, 1_000_000);
    assert_eq!(cpu.sleep, cortex_m3_emu::Sleep::Wfe);
    assert_eq!(cpu.instructions, 3, "the first WFE returned at once");
    assert_eq!(
        cpu.now, 1_000_000,
        "asleep until the deadline, nothing to wake it"
    );
    assert_eq!(cpu.sleeps, 1);
}

#[test]
fn basepri_masks_lower_priorities_only() {
    let (mut cpu, mut bus) = systick_machine();
    // SysTick at priority 0x80; BASEPRI 0x80 masks it (equal isn't higher).
    cpu.ppb.priority[15] = 0x80;
    cpu.basepri = 0x80;
    let period = 100 * Config::default().systick_ticks;
    cpu.run_until(&mut bus, 2 * period + period / 2);
    assert_eq!(cpu.r[4], 0);
    cpu.basepri = 0x90;
    cpu.step(&mut bus);
    assert_eq!(cpu.ipsr, 15, "0x80 is higher than BASEPRI 0x90");
}

#[test]
fn cyccnt_counts_at_96_mhz() {
    let (mut cpu, mut bus) = machine(&[
        (0xF241_0000, "movw r0, #0x1000"),
        (0xF2CE_0000, "movt r0, #0xe000"),
        (0x2101, "movs r1, #0x1"),
        (0x6001, "str r1, [r0]"),
        (0x2364, "movs r3, #0x64"),
        (0x3B01, "subs r3, #0x1"),
        (0xD1FD, "bne 0x4e"),
        (0x6842, "ldr r2, [r0, #0x4]"),
    ]);
    steps(&mut cpu, &mut bus, 4 + 1 + 200 + 1);
    // Time advances as an instruction starts, so the read sees 202 after
    // the enable (movs, the loop's 200, the ldr itself) at 100 MIPS:
    // 2.02 us = 193.92 cycles.
    let want = 202 * Config::default().insn_ticks / Config::default().dwt_ticks;
    assert_eq!(cpu.r[2] as u64, want);
}
