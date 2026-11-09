// SPDX-License-Identifier: GPL-2.0-only
//! 16550-compatible UART driver for the kernel console (COM1, 115200 8N1).
//!
//! Output is polled. Input is interrupt-driven once [`enable_receive_interrupt`]
//! has been called: received bytes are handed to a callback from the
//! interrupt handler.

use core::fmt;
use core::sync::atomic::{AtomicBool, Ordering};

use super::port::{inb, outb};
use crate::sync::{SpinLock, SpinLockGuard};

/// I/O port base of the first PC serial port.
const COM1: u16 = 0x3F8;

// Register offsets (with DLAB clear unless noted).
const DATA: u16 = 0; // RBR/THR; divisor low byte when DLAB is set
const INT_ENABLE: u16 = 1; // IER; divisor high byte when DLAB is set
const FIFO_CTRL: u16 = 2;
const LINE_CTRL: u16 = 3;
const MODEM_CTRL: u16 = 4;
const LINE_STATUS: u16 = 5;

const LSR_DATA_READY: u8 = 1 << 0;
const LSR_THR_EMPTY: u8 = 1 << 5;
/// Interrupt-enable bit: received data available.
const IER_RX_AVAILABLE: u8 = 1 << 0;

/// Upper bound on polls of the line status register per byte. Keeps a
/// missing or wedged UART from hanging the kernel.
const TX_SPIN_LIMIT: u32 = 1_000_000;

/// Serial initialisation failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SerialError {
    /// The loopback self-test did not echo the test byte: there is no
    /// working UART at the port.
    LoopbackFailed,
}

/// A 16550 UART at a fixed I/O port base.
pub struct SerialPort {
    base: u16,
}

impl SerialPort {
    /// Programs the UART and runs its loopback self-test.
    fn init(&mut self) -> Result<(), SerialError> {
        let base = self.base;
        // SAFETY: `base` is the I/O port block of a PC UART (COM1). Writing
        // its registers has no effect on memory safety; if no UART is
        // present the writes are ignored and the loopback test below fails.
        unsafe {
            outb(base + INT_ENABLE, 0x00); // no interrupts, we poll
            outb(base + LINE_CTRL, 0x80); // DLAB on
            outb(base + DATA, 0x01); // divisor 1 => 115200 baud
            outb(base + INT_ENABLE, 0x00);
            outb(base + LINE_CTRL, 0x03); // 8N1, DLAB off
            outb(base + FIFO_CTRL, 0xC7); // enable + clear FIFOs
            outb(base + MODEM_CTRL, 0x1E); // loopback mode
            outb(base + DATA, 0xAE);
            if inb(base + DATA) != 0xAE {
                return Err(SerialError::LoopbackFailed);
            }
            outb(base + MODEM_CTRL, 0x0F); // normal operation, DTR+RTS
        }
        Ok(())
    }

    fn write_byte(&mut self, byte: u8) {
        // SAFETY: reads and writes of the UART's own registers; see `init`.
        unsafe {
            let mut spins = 0;
            while inb(self.base + LINE_STATUS) & LSR_THR_EMPTY == 0 && spins < TX_SPIN_LIMIT {
                core::hint::spin_loop();
                spins += 1;
            }
            outb(self.base + DATA, byte);
        }
    }
}

impl fmt::Write for SerialPort {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for byte in s.bytes() {
            if byte == b'\n' {
                self.write_byte(b'\r');
            }
            self.write_byte(byte);
        }
        Ok(())
    }
}

static CONSOLE: SpinLock<SerialPort> = SpinLock::new(SerialPort { base: COM1 });
static READY: AtomicBool = AtomicBool::new(false);

/// Initialises the console UART. Until this succeeds, console output is
/// discarded.
pub fn init() -> Result<(), SerialError> {
    CONSOLE.lock().init()?;
    READY.store(true, Ordering::Release);
    Ok(())
}

/// A handle for writing to the console.
pub enum Console {
    /// The console is initialised and locked for this writer.
    Locked(SpinLockGuard<'static, SerialPort>),
    /// The console lock was already held by the interrupted context (only
    /// possible on the panic/exception path); writes bypass the lock.
    Stolen(SerialPort),
    /// The console is not initialised; writes are dropped.
    Disabled,
}

impl fmt::Write for Console {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        match self {
            Self::Locked(port) => port.write_str(s),
            Self::Stolen(port) => port.write_str(s),
            Self::Disabled => Ok(()),
        }
    }
}

/// Returns a writer for the kernel console.
///
/// On a single CPU the lock can only be found held when a panic or CPU
/// exception interrupted a writer. Blocking would then deadlock and lose the
/// diagnostic, so the writer falls back to driving the UART directly. The
/// UART has no multi-step state on the transmit path, so the worst outcome
/// is interleaved characters.
pub fn console() -> Console {
    if !READY.load(Ordering::Acquire) {
        return Console::Disabled;
    }
    match CONSOLE.try_lock() {
        Some(guard) => Console::Locked(guard),
        None => Console::Stolen(SerialPort { base: COM1 }),
    }
}

/// Called from interrupt context with each received byte.
pub type ReceiveHandler = fn(u8);

static RECEIVE_HANDLER: SpinLock<Option<ReceiveHandler>> = SpinLock::new(None);

/// Makes the UART raise its interrupt when data arrives and passes each
/// received byte to `handler`.
///
/// The caller is responsible for routing the UART's interrupt line to
/// [`handle_interrupt`].
pub fn enable_receive_interrupt(handler: ReceiveHandler) {
    *RECEIVE_HANDLER.lock() = Some(handler);
    // Holding the console lock keeps the write from interleaving with the
    // DLAB sequence in `init`.
    let _console = CONSOLE.lock();
    // SAFETY: COM1's interrupt-enable register; see `SerialPort::init`.
    // Enabling the receive interrupt has no effect on memory safety. `init`
    // already set OUT2, which gates the interrupt line on PC hardware.
    unsafe { outb(COM1 + INT_ENABLE, IER_RX_AVAILABLE) };
}

/// Interrupt handler for the UART: drains the receive FIFO.
pub fn handle_interrupt() {
    let handler = *RECEIVE_HANDLER.lock();
    // SAFETY: reads of COM1's line-status and data registers. Reading the
    // data register pops the receive FIFO, which is exactly the intent, and
    // nothing else in the kernel reads it. The loop is bounded so a stuck
    // status bit cannot hang the handler.
    unsafe {
        for _ in 0..256 {
            if inb(COM1 + LINE_STATUS) & LSR_DATA_READY == 0 {
                break;
            }
            let byte = inb(COM1 + DATA);
            if let Some(handler) = handler {
                handler(byte);
            }
        }
    }
}
