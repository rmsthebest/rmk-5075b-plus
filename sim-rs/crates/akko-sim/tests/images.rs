//! Every image run.sh snapshots loads, and its vector table is where the
//! core will look for it: the initial SP in SRAM, a Thumb reset vector in
//! flash, both through the alias at 0.

use akko_sim::{Board, Image, ROM_PARKED};
use cortex_m3_emu::{Bus, Size};

#[test]
fn every_image_loads_with_a_sane_vector_table() {
    for image in Image::ALL {
        let mut board = Board::new();
        board
            .load(image)
            .unwrap_or_else(|e| panic!("{image:?}: {e}"));
        let sp = board.chip.read(0, Size::Word);
        let reset = board.chip.read(4, Size::Word);
        assert!(
            (0x2000_0000..=0x2000_9000).contains(&sp),
            "{image:?}: SP 0x{sp:08x}"
        );
        assert!(
            reset & 1 == 1,
            "{image:?}: reset vector 0x{reset:08x} isn't Thumb"
        );
        assert!(
            (0x0800_0000..0x0804_0000).contains(&(reset & !1)),
            "{image:?}: reset vector 0x{reset:08x} isn't in flash"
        );
        assert!(
            board.chip.diags().is_empty(),
            "{image:?}: {:?}",
            board.chip.diags()
        );
    }
}

#[test]
fn rom_stub_parks() {
    let mut board = Board::new();
    assert_eq!(board.chip.read(0x1FFF_E000, Size::Word), 0x2000_1000);
    assert_eq!(board.chip.read(0x1FFF_E004, Size::Word), ROM_PARKED | 1);
    assert_eq!(board.chip.read(ROM_PARKED, Size::Half), 0xE7FE);
}
