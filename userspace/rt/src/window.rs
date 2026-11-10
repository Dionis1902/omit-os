// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
//! A window on the desktop.
//!
//! [`Window::open`] connects to the desktop and gets a window whose
//! pixels are memory shared with it: draw with [`Window::pixels`], then
//! call [`Window::present`] to have the change shown. [`Window::event`]
//! delivers key presses and pointer events. The protocol is in the
//! `omit_window` crate.

use alloc::vec::Vec;

use omit_abi::Errno;
use omit_window::{CHANNEL, Decoder, Request};

/// Something that happened to a window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// A key was pressed: ASCII, or a special key from
    /// [`omit_abi::key`].
    Key(i32),
    /// The pointer moved or a button changed while the left button was
    /// held after a press in the window. Bit 0 of `buttons` is the left
    /// button.
    Pointer {
        /// Pixels from the left edge of the window's contents.
        x: i32,
        /// Pixels from the top edge.
        y: i32,
        /// Buttons held.
        buttons: u32,
    },
    /// The window was closed, or the desktop is gone. Nothing more will
    /// arrive.
    Closed,
}

/// Why a window could not be opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenError {
    /// No desktop is running.
    NoDesktop,
    /// The desktop closed the connection or sent something unexpected.
    Refused,
    /// A system call failed.
    System(Errno),
}

impl core::fmt::Display for OpenError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NoDesktop => write!(f, "no desktop is running"),
            Self::Refused => write!(f, "the desktop refused the window"),
            Self::System(error) => write!(f, "{error}"),
        }
    }
}

/// An open window.
pub struct Window {
    connection: u64,
    decoder: Decoder,
    pixels: *mut u32,
    mapped: usize,
    width: usize,
    height: usize,
    closed: bool,
}

// SAFETY: a `Window` owns its mapping and its connection; the pointer is
// only that mapping's address, and nothing about either is tied to the
// thread that opened the window.
unsafe impl Send for Window {}

impl Window {
    /// Opens a window with a content area of `width` x `height` pixels,
    /// initially black.
    pub fn open(title: &str, width: usize, height: usize) -> Result<Self, OpenError> {
        let connection = crate::channel_connect(CHANNEL).map_err(|error| match error {
            Errno::NotFound => OpenError::NoDesktop,
            error => OpenError::System(error),
        })?;
        let mut window = Self {
            connection,
            decoder: Decoder::new(),
            pixels: core::ptr::null_mut(),
            mapped: 0,
            width,
            height,
            closed: false,
        };
        window
            .send(&Request::Create {
                width: width as u32,
                height: height as u32,
                title: title.into(),
            })
            .map_err(OpenError::System)?;
        // The first thing the desktop sends is the answer.
        let token = loop {
            match window.decoder.next_event() {
                Ok(Some(omit_window::Event::Created {
                    token,
                    width: w,
                    height: h,
                })) if (w as usize, h as usize) == (width, height) => break token,
                Ok(None) => {}
                _ => return Err(OpenError::Refused),
            }
            if !window.receive(true).map_err(OpenError::System)? {
                return Err(OpenError::Refused);
            }
        };
        let (address, length) = crate::shared_map(token).map_err(OpenError::System)?;
        window.pixels = address.cast();
        window.mapped = length;
        if length < width * height * 4 {
            return Err(OpenError::Refused);
        }
        Ok(window)
    }

    /// The descriptor of the connection to the desktop, for waiting on
    /// several windows (or a window and something else) with
    /// [`crate::poll`]: it is readable when an event may have arrived.
    pub fn descriptor(&self) -> u64 {
        self.connection
    }

    /// Width of the content area in pixels.
    pub fn width(&self) -> usize {
        self.width
    }

    /// Height of the content area in pixels.
    pub fn height(&self) -> usize {
        self.height
    }

    /// The window's pixels, row by row from the top, `0x00RRGGBB`.
    /// Changes become visible after [`Window::present`].
    pub fn pixels(&mut self) -> &mut [u32] {
        // SAFETY: `open` checked that the mapping holds `width * height`
        // pixels; it is page-aligned and stays mapped until `drop`. The
        // desktop only reads this memory.
        unsafe { core::slice::from_raw_parts_mut(self.pixels, self.width * self.height) }
    }

    /// Asks the desktop to show the whole window as it is now.
    pub fn present(&mut self) {
        self.present_area(0, 0, self.width, self.height);
    }

    /// Asks the desktop to show a rectangle of the window as it is now.
    pub fn present_area(&mut self, x: usize, y: usize, width: usize, height: usize) {
        let request = Request::Present {
            x: x as u32,
            y: y as u32,
            width: width as u32,
            height: height as u32,
        };
        if self.send(&request).is_err() {
            self.closed = true;
        }
    }

    /// Changes the text in the title bar.
    pub fn set_title(&mut self, title: &str) {
        let request = Request::SetTitle {
            title: title.into(),
        };
        if self.send(&request).is_err() {
            self.closed = true;
        }
    }

    fn send(&mut self, request: &Request) -> Result<(), Errno> {
        crate::write_all(self.connection, &request.encode())
    }

    /// Reads what has arrived into the decoder; with `wait`, blocks for
    /// at least one byte. Returns `false` at the end of the connection.
    fn receive(&mut self, wait: bool) -> Result<bool, Errno> {
        let mut buffer = [0u8; 256];
        let count = if wait {
            crate::read(self.connection, &mut buffer)?
        } else {
            match crate::read_no_wait(self.connection, &mut buffer)? {
                Some(count) => count,
                None => return Ok(true),
            }
        };
        self.decoder.push(&buffer[..count]);
        Ok(count != 0)
    }

    /// The next event. Waits up to `timeout_ms` milliseconds for one
    /// (`None` waits without limit) and returns `None` if the time passes.
    pub fn event(&mut self, timeout_ms: Option<u64>) -> Option<Event> {
        loop {
            if self.closed {
                return Some(Event::Closed);
            }
            match self.decoder.next_event() {
                Ok(Some(omit_window::Event::Key { code })) => return Some(Event::Key(code)),
                Ok(Some(omit_window::Event::Pointer { x, y, buttons })) => {
                    return Some(Event::Pointer { x, y, buttons });
                }
                Ok(Some(omit_window::Event::Created { .. })) | Err(_) => {
                    self.closed = true;
                    continue;
                }
                Ok(None) => {}
            }
            if let Some(timeout) = timeout_ms {
                match crate::poll(
                    &[self.connection],
                    timeout.min(omit_abi::POLL_TIMEOUT_MAX),
                    false,
                ) {
                    Ok(0) => return None,
                    Ok(_) => {}
                    Err(_) => {
                        self.closed = true;
                        continue;
                    }
                }
            }
            if !matches!(self.receive(true), Ok(true)) {
                self.closed = true;
            }
        }
    }

    /// The events that have already arrived, without waiting.
    pub fn pending_events(&mut self) -> Vec<Event> {
        let mut events = Vec::new();
        while let Some(event) = self.event(Some(0)) {
            events.push(event);
            if event == Event::Closed {
                break;
            }
        }
        events
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        if !self.pixels.is_null() {
            // SAFETY: the mapping is not used after this; `pixels()`
            // borrows `self`, so no slice of it is alive.
            let _ = unsafe { crate::unmap(self.pixels.cast(), self.mapped) };
        }
        let _ = crate::close(self.connection);
    }
}
