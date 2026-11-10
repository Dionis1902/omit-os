// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: GPL-2.0-only
//! PS/2 keyboard and mouse via the legacy 8042 controller.
//!
//! The keyboard port is used exactly as the firmware left it (enabled,
//! scancode translation to set 1 on); the driver only reads scancodes when
//! the keyboard interrupt fires. The mouse ("auxiliary") port is usually
//! left disabled by the firmware, so [`init_mouse`] enables it and tells
//! the mouse to start reporting. That is enough for virtual machines and
//! for hardware with real PS/2 ports.
//!
//! USB keyboards and mice are *not* supported: they only appear here on
//! machines whose firmware keeps emulating PS/2 devices after boot
//! services end, which many do not.

use omit_term::mouse::{self, Report};
use omit_term::scancode::{Decoder, Key};

use super::port::{inb, outb};
use crate::sync::SpinLock;

const DATA_PORT: u16 = 0x60;
const STATUS_PORT: u16 = 0x64;
const COMMAND_PORT: u16 = 0x64;
const STATUS_OUTPUT_FULL: u8 = 1 << 0;
const STATUS_INPUT_FULL: u8 = 1 << 1;
/// The byte in the output buffer came from the auxiliary (mouse) port.
const STATUS_AUX_DATA: u8 = 1 << 5;

const COMMAND_READ_CONFIG: u8 = 0x20;
const COMMAND_WRITE_CONFIG: u8 = 0x60;
const COMMAND_ENABLE_AUX: u8 = 0xA8;
const COMMAND_WRITE_AUX: u8 = 0xD4;
const CONFIG_AUX_INTERRUPT: u8 = 1 << 1;
const CONFIG_AUX_CLOCK_OFF: u8 = 1 << 5;

const MOUSE_SET_DEFAULTS: u8 = 0xF6;
const MOUSE_ENABLE_REPORTING: u8 = 0xF4;
const MOUSE_ACK: u8 = 0xFA;

/// Called with each key press.
pub type KeyHandler = fn(Key);
/// Called with each mouse report.
pub type MouseHandler = fn(Report);

struct Controller {
    keys: Decoder,
    mouse: mouse::Decoder,
    key_handler: Option<KeyHandler>,
    mouse_handler: Option<MouseHandler>,
}

static CONTROLLER: SpinLock<Controller> = SpinLock::new(Controller {
    keys: Decoder::new(),
    mouse: mouse::Decoder::new(),
    key_handler: None,
    mouse_handler: None,
});

/// Reads pending bytes from the controller and, if `deliver` is set,
/// decodes them and passes the results to the handlers.
fn drain(controller: &mut Controller, deliver: bool) {
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
        if !deliver {
            continue;
        }
        if status & STATUS_AUX_DATA != 0 {
            if let (Some(report), Some(handler)) =
                (controller.mouse.feed(byte), controller.mouse_handler)
            {
                handler(report);
            }
        } else {
            let key = controller.keys.feed_key(byte);
            crate::input::modifiers_held(controller.keys.modifiers());
            if let (Some(key), Some(handler)) = (key, controller.key_handler) {
                handler(key);
            }
        }
    }
}

/// Discards anything already waiting in the controller and starts passing
/// key presses to `handler`.
///
/// The caller routes the keyboard interrupt to [`handle_interrupt`].
pub fn init(handler: KeyHandler) {
    let mut controller = CONTROLLER.lock();
    drain(&mut controller, false);
    controller.key_handler = Some(handler);
}

/// Number of status polls before giving up on the controller; each takes
/// about a microsecond on real hardware, so this is tens of milliseconds.
const POLL_LIMIT: u32 = 50_000;

/// Waits until the controller can accept a byte, then writes `value` to
/// `port`. Returns `false` on timeout.
fn send(port: u16, value: u8) -> bool {
    for _ in 0..POLL_LIMIT {
        // SAFETY: reading the 8042 status port has no side effects.
        if unsafe { inb(STATUS_PORT) } & STATUS_INPUT_FULL == 0 {
            // SAFETY: the controller's input buffer is empty, so it accepts
            // a command (port 0x64) or data byte (port 0x60). The callers
            // only send the documented commands named above.
            unsafe { outb(port, value) };
            return true;
        }
        core::hint::spin_loop();
    }
    false
}

/// Waits for a byte from the controller.
fn receive() -> Option<u8> {
    for _ in 0..POLL_LIMIT {
        // SAFETY: see `drain`.
        unsafe {
            if inb(STATUS_PORT) & STATUS_OUTPUT_FULL != 0 {
                return Some(inb(DATA_PORT));
            }
        }
        core::hint::spin_loop();
    }
    None
}

/// Sends a command byte to the mouse and checks that it acknowledges.
fn mouse_command(command: u8) -> bool {
    send(COMMAND_PORT, COMMAND_WRITE_AUX)
        && send(DATA_PORT, command)
        && receive() == Some(MOUSE_ACK)
}

/// Enables the mouse port and the mouse itself, and starts passing its
/// reports to `handler`. Returns `false` if there is no PS/2 mouse (the
/// controller or the mouse did not answer).
///
/// The caller routes the mouse interrupt to [`handle_interrupt`].
pub fn init_mouse(handler: MouseHandler) -> bool {
    // The lock keeps interrupts off, so the interrupt handler cannot
    // consume the answers to the commands below.
    let mut controller = CONTROLLER.lock();
    drain(&mut controller, false);

    let configured = send(COMMAND_PORT, COMMAND_ENABLE_AUX)
        && send(COMMAND_PORT, COMMAND_READ_CONFIG)
        && receive().is_some_and(|config| {
            let config = (config | CONFIG_AUX_INTERRUPT) & !CONFIG_AUX_CLOCK_OFF;
            send(COMMAND_PORT, COMMAND_WRITE_CONFIG) && send(DATA_PORT, config)
        });
    let ready =
        configured && mouse_command(MOUSE_SET_DEFAULTS) && mouse_command(MOUSE_ENABLE_REPORTING);
    drain(&mut controller, false);
    if ready {
        controller.mouse_handler = Some(handler);
    }
    ready
}

/// Interrupt handler for both the keyboard and the mouse interrupt: either
/// way, whatever the controller has is read and sorted by origin.
pub fn handle_interrupt() {
    drain(&mut CONTROLLER.lock(), true);
}
