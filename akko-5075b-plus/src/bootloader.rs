//! QK_BOOT (Fn + Right Shift + Esc): straight into the ROM DFU bootloader on
//! the press, through boot-guard's magic word, as QMK's bootloader_jump().
//! RMK's own handling of the Bootloader action only resets (on the release)
//! on chips it doesn't know.

use rmk::event::ActionEvent;
use rmk::macros::processor;
use rmk::types::action::{Action, KeyboardAction};

#[processor(subscribe = [ActionEvent])]
pub struct Bootloader;

impl Bootloader {
    pub fn new() -> Self {
        Bootloader
    }

    async fn on_action_event(&mut self, e: ActionEvent) {
        if e.action == Action::KeyboardControl(KeyboardAction::Bootloader) && e.keyboard_event.pressed {
            boot_guard::request_bootloader();
        }
    }
}
