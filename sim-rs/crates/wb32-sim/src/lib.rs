//! The WB32FQ95 (as on the Akko 5075B Plus) for cortex-m3-emu.
//!
//! Register facts here come from the Renode models in ../../sim/peripherals
//! (checked against the QMK binary), the vendor's wb32fq95xx.h and the
//! reference manual (RM2905025), never from our wb32-pac or wb32-hal: those
//! are what the simulator tests. Each model cites its sources.

pub mod chip;
pub mod diag;
pub mod load;

pub use chip::Wb32;
pub use diag::Diag;
pub use load::LoadError;
