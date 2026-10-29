//! The system bus the core loads, stores and fetches through.

/// Access width. The value is the width in bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Size {
    Byte = 1,
    Half = 2,
    Word = 4,
}

impl Size {
    pub const fn bytes(self) -> u32 {
        self as u32
    }

    /// The bits an access of this width carries.
    pub const fn mask(self) -> u32 {
        match self {
            Size::Byte => 0xFF,
            Size::Half => 0xFFFF,
            Size::Word => 0xFFFF_FFFF,
        }
    }
}

/// Everything outside the core's private peripheral bus.
///
/// Data accesses never fail: like Renode (and unlike a real bus fault), an
/// access nothing answers reads as 0 and is dropped, and the chip model
/// records it as an error for the test to fail on. Values are little-endian
/// and only the low `size` bytes are meaningful.
pub trait Bus {
    fn read(&mut self, addr: u32, size: Size) -> u32;
    fn write(&mut self, addr: u32, size: Size, value: u32);

    /// An instruction fetch: `None` where there's no memory to execute
    /// from, which the core takes as a bus fault.
    fn fetch(&mut self, addr: u32) -> Option<u16>;

    /// Runs whatever the chip has due at or before `now` (core ticks, see
    /// [`crate::TICK_HZ`]). Called before every instruction and whenever a
    /// sleeping core's clock jumps.
    fn advance(&mut self, _now: u64) {}

    /// When the chip next needs `advance`, if ever. A sleeping core jumps
    /// straight there.
    fn next_event(&self) -> u64 {
        u64::MAX
    }

    /// The interrupt request lines into the NVIC, read before every
    /// instruction.
    fn irq_lines(&mut self) -> IrqLines {
        IrqLines::default()
    }

    /// The core went to sleep (WFI/WFE); `deep` is SCR.SLEEPDEEP.
    fn sleep(&mut self, _deep: bool) {}
}

/// External interrupt inputs, bit n for IRQ n (exception 16 + n).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IrqLines {
    /// Lines held high now.
    pub level: u64,
    /// Lines that went high since the last read, even if already low again.
    pub raised: u64,
}

/// A plain little-endian memory: flash, SRAM, ROM.
#[derive(Clone)]
pub struct Memory {
    bytes: Vec<u8>,
}

impl Memory {
    pub fn new(size: usize) -> Self {
        Memory {
            bytes: vec![0; size],
        }
    }

    pub fn len(&self) -> u32 {
        self.bytes.len() as u32
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// Whether an access of `size` at `offset` lies wholly inside.
    pub fn contains(&self, offset: u32, size: Size) -> bool {
        (offset as u64 + size.bytes() as u64) <= self.bytes.len() as u64
    }

    /// Unaligned accesses are fine, as for normal memory on the M3; the
    /// caller checks `contains` first.
    pub fn read(&self, offset: u32, size: Size) -> u32 {
        let o = offset as usize;
        let b = &self.bytes;
        match size {
            Size::Byte => b[o] as u32,
            Size::Half => u16::from_le_bytes([b[o], b[o + 1]]) as u32,
            Size::Word => u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]),
        }
    }

    pub fn write(&mut self, offset: u32, size: Size, value: u32) {
        let o = offset as usize;
        let n = size.bytes() as usize;
        self.bytes[o..o + n].copy_from_slice(&value.to_le_bytes()[..n]);
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn bytes_mut(&mut self) -> &mut [u8] {
        &mut self.bytes
    }

    pub fn fill(&mut self, value: u8) {
        self.bytes.fill(value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn little_endian_and_unaligned() {
        let mut m = Memory::new(8);
        m.write(1, Size::Word, 0x1122_3344);
        assert_eq!(m.bytes(), &[0, 0x44, 0x33, 0x22, 0x11, 0, 0, 0]);
        assert_eq!(m.read(2, Size::Half), 0x2233);
        assert_eq!(m.read(4, Size::Byte), 0x11);
        m.write(7, Size::Byte, 0x1AB);
        assert_eq!(m.read(4, Size::Word), 0xAB00_0011);
    }

    #[test]
    fn contains_checks_the_whole_access() {
        let m = Memory::new(8);
        assert!(m.contains(4, Size::Word));
        assert!(!m.contains(5, Size::Word));
        assert!(m.contains(7, Size::Byte));
        assert!(!m.contains(u32::MAX, Size::Byte));
    }
}
