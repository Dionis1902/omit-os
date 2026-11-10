// SPDX-License-Identifier: GPL-2.0-only
//! Windows of other programs.
//!
//! A program connects to the desktop's channel and asks for a window
//! (the protocol is in the `omit_window` crate). The desktop creates a
//! shared memory segment for the window's pixels, tells the program its
//! token, and from then on copies from that memory whenever the program
//! says something changed. Input for the window is sent to the program.
//!
//! The other program is not trusted: its messages are validated by the
//! protocol decoder, the pixel memory is created (and so sized) by the
//! desktop, nothing ever waits for the program, and a program that
//! misbehaves or stops reading loses its window, nothing more.

use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;

use omit_gfx::desktop::{App, Context, Desktop};
use omit_gfx::geometry::Rect;
use omit_gfx::surface::Canvas;
use omit_rt::abi::POLL_MAX;
use omit_window::{CHANNEL, Decoder, Event, Request};

/// The connection descriptors the main loop should wait on.
pub type Watched = Rc<RefCell<Vec<u64>>>;

/// One end of a connection to a program, never waited for.
struct Connection {
    fd: u64,
    decoder: Decoder,
    /// The connection ended or broke the protocol.
    dead: bool,
    watched: Watched,
}

impl Connection {
    fn new(fd: u64, watched: &Watched) -> Self {
        watched.borrow_mut().push(fd);
        Self {
            fd,
            decoder: Decoder::new(),
            dead: false,
            watched: watched.clone(),
        }
    }

    /// The next request that has arrived, if any.
    fn request(&mut self) -> Option<Request> {
        loop {
            if self.dead {
                return None;
            }
            match self.decoder.next_request() {
                Ok(Some(request)) => return Some(request),
                Ok(None) => {}
                Err(_) => {
                    self.dead = true;
                    return None;
                }
            }
            let mut buffer = [0u8; 256];
            match omit_rt::read_no_wait(self.fd, &mut buffer) {
                Ok(Some(count)) if count != 0 => self.decoder.push(&buffer[..count]),
                Ok(None) => return None,
                // End of the connection, or an error.
                _ => {
                    self.dead = true;
                    return None;
                }
            }
        }
    }

    /// Sends an event. A program that has let 64 KiB of events pile up
    /// unread is not waited for: it loses the connection.
    fn send(&mut self, event: Event) {
        if !self.dead && omit_rt::write_no_wait(self.fd, &event.encode()) != Ok(true) {
            self.dead = true;
        }
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.watched.borrow_mut().retain(|&fd| fd != self.fd);
        let _ = omit_rt::close(self.fd);
    }
}

/// The window of another program.
struct Remote {
    connection: Connection,
    title: String,
    /// The shared pixel memory: `width * height` pixels.
    pixels: *const u32,
    mapped: usize,
    width: usize,
    height: usize,
}

impl App for Remote {
    fn title(&self) -> &str {
        &self.title
    }

    fn draw(&mut self, canvas: &mut Canvas<'_>, _width: i32, _height: i32) {
        let (pixels, width) = (self.pixels, self.width);
        let area = Rect::new(0, 0, self.width as i32, self.height as i32);
        canvas.rows(area, |x, y, row| {
            // SAFETY: `rows` only reports positions inside `area`, so the
            // source range lies within the `width * height` pixels of the
            // mapping, which lives as long as `self`. The other program
            // may be writing these pixels at this moment; a raw copy
            // tolerates that (the result is then a mix of old and new,
            // corrected by the program's next `Present`).
            unsafe {
                core::ptr::copy_nonoverlapping(
                    pixels.add(y as usize * width + x as usize),
                    row.as_mut_ptr(),
                    row.len(),
                );
            }
        });
    }

    /// The other program drew for the size it asked for; the protocol
    /// has no way to tell it about another.
    fn resizable(&self) -> bool {
        false
    }

    fn key(&mut self, code: i32, _context: &mut Context) {
        self.connection.send(Event::Key { code });
    }

    fn click(&mut self, x: i32, y: i32, _context: &mut Context) {
        self.connection.send(Event::Pointer { x, y, buttons: 1 });
    }

    fn pointer(&mut self, x: i32, y: i32, buttons: u32, _context: &mut Context) {
        self.connection.send(Event::Pointer { x, y, buttons });
    }

    fn tick(&mut self, context: &mut Context) -> bool {
        let mut changed = false;
        while let Some(request) = self.connection.request() {
            match request {
                Request::Present { .. } => changed = true,
                Request::SetTitle { title } => {
                    self.title = title;
                    changed = true;
                }
                // A second window on one connection is not in the
                // protocol.
                Request::Create { .. } => self.connection.dead = true,
            }
        }
        if self.connection.dead {
            context.close();
        }
        changed
    }
}

impl Drop for Remote {
    fn drop(&mut self) {
        // SAFETY: nothing uses the mapping after this.
        let _ = unsafe { omit_rt::unmap(self.pixels.cast_mut().cast(), self.mapped) };
    }
}

/// Accepts connections and turns them into windows.
pub struct Server {
    listener: u64,
    /// Connections that have not asked for their window yet.
    pending: Vec<Connection>,
    watched: Watched,
    /// Largest window accepted.
    limit: (usize, usize),
}

impl Server {
    /// Starts listening. `None` if the channel cannot be claimed (then
    /// the desktop simply has no windows of other programs).
    pub fn new(screen_width: usize, screen_height: usize) -> Option<Self> {
        let listener = omit_rt::channel_listen(CHANNEL).ok()?;
        Some(Self {
            listener,
            pending: Vec::new(),
            watched: Rc::new(RefCell::new(Vec::new())),
            limit: (screen_width, screen_height),
        })
    }

    /// The descriptors to wait on: the listener and every connection.
    pub fn descriptors(&self) -> Vec<u64> {
        let mut fds = alloc::vec![self.listener];
        fds.extend(self.watched.borrow().iter().copied());
        fds
    }

    /// Accepts new connections and opens the windows that have been
    /// asked for. Call whenever [`Server::descriptors`] are ready.
    pub fn serve(&mut self, desktop: &mut Desktop) {
        while let Ok(Some(fd)) = omit_rt::channel_accept_no_wait(self.listener) {
            // One descriptor per window can be waited on; more programs
            // than that are turned away.
            if self.watched.borrow().len() + 1 >= POLL_MAX as usize {
                let _ = omit_rt::close(fd);
            } else {
                self.pending.push(Connection::new(fd, &self.watched));
            }
        }
        let mut index = 0;
        while index < self.pending.len() {
            let request = self.pending[index].request();
            if request.is_none() && !self.pending[index].dead {
                index += 1;
                continue;
            }
            // Either the window is created now or the connection ends.
            let connection = self.pending.swap_remove(index);
            if let Some(Request::Create {
                width,
                height,
                title,
            }) = request
            {
                self.open(desktop, connection, width as usize, height as usize, title);
            }
        }
    }

    fn open(
        &self,
        desktop: &mut Desktop,
        connection: Connection,
        width: usize,
        height: usize,
        title: String,
    ) {
        if width > self.limit.0 || height > self.limit.1 {
            return;
        }
        let Ok((address, token)) = omit_rt::shared_create(width * height * 4) else {
            return;
        };
        let mut remote = Remote {
            connection,
            title,
            pixels: address.cast_const().cast(),
            mapped: (width * height * 4).next_multiple_of(4096),
            width,
            height,
        };
        remote.connection.send(Event::Created {
            token,
            width: width as u32,
            height: height as u32,
        });
        desktop.open(Box::new(remote), width as i32, height as i32);
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = omit_rt::close(self.listener);
    }
}
