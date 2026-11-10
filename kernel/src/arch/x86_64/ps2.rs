// SPDX-License-Identifier: GPL-2.0-only
//! PS/2 keyboard via the legacy 8042 keyboard controller.
//!
//! The controller is used exactly as the firmware left it (keyboard port
//! enabled, scancode translation to set 1 on); the driver only reads
//! scancodes when the keyboard interrupt fires. That is enough for virtual
//! machines and for hardware with a real PS/2 port.
//!
//! USB keyboards are *not* supported: they only appear here on machines
//! whose firmware keeps emulating a PS/2 keyboard after boot services end,
//! which many do not.

use omit_term::scancode::Decoder;

use super::port::inb;
use crate::sync::SpinLock;

const DATA_PORT: u16 = 0x60;
const STATUS_PORT: u16 = 0x64;
const STATUS_OUTPUT_FULL: u8 = 1 << 0;
/// The byte in the output buffer came from the auxiliary (mouse) port.
const STATUS_AUX_DATA: u8 = 1 << 5;

/// Called with each decoded character.
pub type KeyHandler = fn(u8);

struct Keyboard {
    decoder: Decoder,
    handler: Option<KeyHandler>,
}

static KEYBOARD: SpinLock<Keyboard> = SpinLock::new(Keyboard {
    decoder: Decoder::new(),
    handler: None,
});

/// Reads pending bytes from the controller, feeding keyboard scancodes to
/// `decoder`, and returns... nothing: decoded characters go to `emit`.
fn drain(keyboard: &mut Keyboard, deliver: bool) {
    // Bounded so a controller stuck with "output full" cannot hang the
    // kernel (a missing controller reads as 0xFF, which has that bit set).
    for _ in 0..64 {
        // SAFETY: ports 0x60 and 0x64 are the 8042 data and status ports on
        // PC-compatible hardware. Reading the status port has no side
        // effects; reading the data port pops the controller's output
        // buffer, which only this driver consumes.
        let (status, byte) = unsafe {
            let status = inb(STATUS_PORT);
            if status & STATUS_OUTPUT_FULL == 0 {
                return;
            }
            (status, inb(DATA_PORT))
        };
        if !deliver || status & STATUS_AUX_DATA != 0 {
            continue;
        }
        if let (Some(ch), Some(handler)) = (keyboard.decoder.feed(byte), keyboard.handler) {
            handler(ch);
        }
    }
}

/// Discards anything already waiting in the controller and starts passing
/// decoded key presses to `handler`.
///
/// The caller routes the keyboard interrupt to [`handle_interrupt`].
pub fn init(handler: KeyHandler) {
    let mut keyboard = KEYBOARD.lock();
    drain(&mut keyboard, false);
    keyboard.handler = Some(handler);
}

/// Keyboard interrupt handler.
pub fn handle_interrupt() {
    drain(&mut KEYBOARD.lock(), true);
}
