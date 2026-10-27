//! A 25-series SPI NOR flash on [`Qspi`], as `embedded-storage-async`
//! `NorFlash`, with commands as QMK's `drivers/flash/flash_spi.c`: 9F RDID,
//! 05 RDSR, 06 WREN, 03 READ, 02 PP (256-byte pages), 20 SE (4 KB sectors).
//!
//! Waiting for a program or erase polls the status register with a 100 us
//! (program) or 1 ms (erase) timer in between, so other tasks keep running.
//! Reads are polled byte by byte (3 MHz, about 2.7 us a byte, as QMK), so
//! they yield to other tasks every `YIELD_EVERY`: RMK's storage walks every
//! item for some operations, which held up the key scan for tens of ms.
//!
//! [`SharedNor::partition`] hands out a window of the chip with offsets
//! from 0; nothing outside it can be read or written through that handle.
//! Partitions share the chip: each operation holds it, so one task's erase
//! can't interleave with another's read.

use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::mutex::Mutex;
use embassy_futures::yield_now;
use embassy_time::{Duration, Instant, Timer};
use embedded_storage::nor_flash::{ErrorType, NorFlashError, NorFlashErrorKind};
use embedded_storage_async::nor_flash::{NorFlash, ReadNorFlash};

use crate::gpio::{Level, Output};
use crate::qspi::Qspi;

const RDID: u8 = 0x9F;
const RDSR: u8 = 0x05;
const WREN: u8 = 0x06;
const READ: u8 = 0x03;
const PP: u8 = 0x02;
const SE: u8 = 0x20;

/// The longest a run of reads keeps other tasks waiting.
const YIELD_EVERY: Duration = Duration::from_micros(500);
/// Bytes read between checks of `YIELD_EVERY` (about 0.35 ms at 3 MHz).
const READ_CHUNK: usize = 128;

const WIP: u8 = 0x01;
const WEL: u8 = 0x02;

pub const PAGE_SIZE: u32 = 256;
pub const SECTOR_SIZE: u32 = 4096;

// Datasheet maxima of common parts are 3 ms per page and 400 ms per sector.
const PROGRAM_TIMEOUT: Duration = Duration::from_millis(50);
const ERASE_TIMEOUT: Duration = Duration::from_millis(2000);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Outside the partition (or chip).
    OutOfBounds,
    NotAligned,
    /// WEL didn't set after WREN: no chip, or it's write protected.
    WriteEnable,
    /// WIP still set after the timeout.
    Timeout,
}

impl NorFlashError for Error {
    fn kind(&self) -> NorFlashErrorKind {
        match self {
            Error::OutOfBounds => NorFlashErrorKind::OutOfBounds,
            Error::NotAligned => NorFlashErrorKind::NotAligned,
            _ => NorFlashErrorKind::Other,
        }
    }
}

pub struct SpiNor {
    spi: Qspi,
    cs: Output,
    jedec_id: [u8; 3],
    /// When reads last let other tasks run.
    last_yield: Instant,
}

impl SpiNor {
    /// `cs` must start high (deselected).
    pub fn new(spi: Qspi, cs: Output) -> Self {
        let mut f = SpiNor { spi, cs, jedec_id: [0; 3], last_yield: Instant::from_ticks(0) };
        let mut id = [0; 3];
        f.command(&[RDID], &mut id);
        f.jedec_id = id;
        f
    }

    /// Manufacturer, memory type, capacity code.
    pub fn jedec_id(&self) -> [u8; 3] {
        self.jedec_id
    }

    /// Size from the JEDEC capacity code (2^code bytes), if it looks sane.
    pub fn capacity(&self) -> Option<u32> {
        match self.jedec_id {
            [0x00, ..] | [0xFF, ..] => None,
            [_, _, code @ 16..=24] => Some(1 << code),
            _ => None,
        }
    }

    /// One command: chip select low, send `tx`, then read `rx.len()` bytes.
    fn command(&mut self, tx: &[u8], rx: &mut [u8]) {
        self.cs.set_level(Level::Low);
        self.spi.write(tx);
        if !rx.is_empty() {
            self.spi.read(rx);
        }
        self.cs.set_level(Level::High);
    }

    fn status(&mut self) -> u8 {
        let mut s = [0];
        self.command(&[RDSR], &mut s);
        s[0]
    }

    async fn wait_idle(&mut self, poll: Duration, timeout: Duration) -> Result<(), Error> {
        let deadline = Instant::now() + timeout;
        loop {
            if self.status() & WIP == 0 {
                return Ok(());
            }
            if Instant::now() > deadline {
                return Err(Error::Timeout);
            }
            Timer::after(poll).await;
        }
    }

    fn write_enable(&mut self) -> Result<(), Error> {
        self.command(&[WREN], &mut []);
        if self.status() & WEL == 0 {
            return Err(Error::WriteEnable);
        }
        Ok(())
    }

    fn header(cmd: u8, addr: u32) -> [u8; 4] {
        [cmd, (addr >> 16) as u8, (addr >> 8) as u8, addr as u8]
    }

    /// Lets other tasks run if reads have kept them waiting `YIELD_EVERY`.
    async fn pause(&mut self) {
        if self.last_yield.elapsed() >= YIELD_EVERY {
            yield_now().await;
            self.last_yield = Instant::now();
        }
    }

    async fn read_at(&mut self, addr: u32, buf: &mut [u8]) -> Result<(), Error> {
        self.pause().await;
        self.wait_idle(Duration::from_micros(100), ERASE_TIMEOUT).await?;
        self.cs.set_level(Level::Low);
        self.spi.write(&Self::header(READ, addr));
        for chunk in buf.chunks_mut(READ_CHUNK) {
            self.spi.read(chunk);
            // Chip select stays low: the chip just waits for more clocks.
            self.pause().await;
        }
        self.cs.set_level(Level::High);
        Ok(())
    }

    async fn program_at(&mut self, mut addr: u32, mut data: &[u8]) -> Result<(), Error> {
        while !data.is_empty() {
            // A page program wraps inside its page: stop at the boundary.
            let room = (PAGE_SIZE - addr % PAGE_SIZE) as usize;
            let (now, rest) = data.split_at(room.min(data.len()));
            self.wait_idle(Duration::from_micros(100), ERASE_TIMEOUT).await?;
            self.write_enable()?;
            self.cs.set_level(Level::Low);
            self.spi.write(&Self::header(PP, addr));
            self.spi.write(now);
            self.cs.set_level(Level::High);
            self.wait_idle(Duration::from_micros(100), PROGRAM_TIMEOUT).await?;
            addr += now.len() as u32;
            data = rest;
        }
        Ok(())
    }

    async fn erase_sector_at(&mut self, addr: u32) -> Result<(), Error> {
        self.wait_idle(Duration::from_micros(100), ERASE_TIMEOUT).await?;
        self.write_enable()?;
        self.command(&Self::header(SE, addr), &mut []);
        self.wait_idle(Duration::from_millis(1), ERASE_TIMEOUT).await
    }
}

/// The chip, for its partitions to share.
pub struct SharedNor {
    nor: Mutex<NoopRawMutex, SpiNor>,
    jedec_id: [u8; 3],
    capacity: Option<u32>,
}

impl SharedNor {
    pub fn new(nor: SpiNor) -> Self {
        SharedNor { jedec_id: nor.jedec_id(), capacity: nor.capacity(), nor: Mutex::new(nor) }
    }

    pub fn jedec_id(&self) -> [u8; 3] {
        self.jedec_id
    }

    /// The window `start .. start + len` of the chip (both sector aligned).
    /// `None` if the chip's size is unknown or the window doesn't fit in it,
    /// so an unexpectedly small chip can't wrap a write around onto offset 0.
    pub fn partition(&self, start: u32, len: u32) -> Option<Partition<'_>> {
        let end = start.checked_add(len)?;
        if start % SECTOR_SIZE != 0 || len % SECTOR_SIZE != 0 || len == 0 || end > self.capacity? {
            return None;
        }
        Some(Partition { nor: &self.nor, start, len })
    }
}

/// A window of the chip, offsets from 0. See [`SharedNor::partition`].
pub struct Partition<'a> {
    nor: &'a Mutex<NoopRawMutex, SpiNor>,
    start: u32,
    len: u32,
}

impl Partition<'_> {
    /// Where the window starts on the chip.
    pub fn start(&self) -> u32 {
        self.start
    }

    /// `offset .. offset + len` inside the window, `offset` a multiple of `align`.
    fn check(&self, offset: u32, len: usize, align: u32) -> Result<(), Error> {
        let end = (offset as u64) + len as u64;
        if end > self.len as u64 {
            return Err(Error::OutOfBounds);
        }
        if offset % align != 0 {
            return Err(Error::NotAligned);
        }
        Ok(())
    }
}

impl ErrorType for Partition<'_> {
    type Error = Error;
}

impl ReadNorFlash for Partition<'_> {
    const READ_SIZE: usize = 1;

    async fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Error> {
        self.check(offset, bytes.len(), 1)?;
        self.nor.lock().await.read_at(self.start + offset, bytes).await
    }

    fn capacity(&self) -> usize {
        self.len as usize
    }
}

impl NorFlash for Partition<'_> {
    const WRITE_SIZE: usize = 1;
    const ERASE_SIZE: usize = SECTOR_SIZE as usize;

    async fn erase(&mut self, from: u32, to: u32) -> Result<(), Error> {
        if to < from || to % SECTOR_SIZE != 0 {
            return Err(Error::NotAligned);
        }
        self.check(from, (to - from) as usize, SECTOR_SIZE)?;
        let mut nor = self.nor.lock().await;
        let mut a = from;
        while a < to {
            nor.erase_sector_at(self.start + a).await?;
            a += SECTOR_SIZE;
        }
        Ok(())
    }

    async fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Error> {
        self.check(offset, bytes.len(), 1)?;
        self.nor.lock().await.program_at(self.start + offset, bytes).await
    }
}
