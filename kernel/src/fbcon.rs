// SPDX-License-Identifier: GPL-2.0-only
//! Framebuffer text console.
//!
//! Draws the kernel console on the linear framebuffer handed over by the
//! boot loader: a status bar on the first row and scrolling text below.
//! Terminal behaviour (cursor, wrapping, scrolling, escape sequences) comes
//! from the `omit_term` crate; this module turns character cells into
//! pixels using pre-rasterised Noto Sans Mono glyphs.
//!
//! Everything written through the kernel's print macros is mirrored here,
//! so the screen shows the same text as the serial console.

use core::fmt::{self, Write};

use noto_sans_mono_bitmap::{FontWeight, RasterHeight, get_raster, get_raster_width};
use omit_bootproto::{BootInfo, FRAMEBUFFER_BGR, FRAMEBUFFER_NONE};
use omit_term::screen::{Cell, Display, Style, Terminal};

use crate::arch::paging::{self, MapError};
use crate::sync::SpinLock;

const FONT_WEIGHT: FontWeight = FontWeight::Regular;
const FONT_HEIGHT: RasterHeight = RasterHeight::Size16;
const CELL_HEIGHT: usize = FONT_HEIGHT.val();
const CELL_WIDTH: usize = get_raster_width(FONT_WEIGHT, FONT_HEIGHT);

/// Rows reserved for the status bar.
const STATUS_ROWS: usize = 1;

/// A colour as (red, green, blue).
type Rgb = (u8, u8, u8);

/// Background of the text area. The boot tests check screenshots against
/// these exact values; keep `tools/xtask` in sync when changing them.
pub const BACKGROUND: Rgb = (0x10, 0x14, 0x1C);
const FOREGROUND: Rgb = (0xD0, 0xD4, 0xDC);
/// Background of the status bar.
pub const STATUS_BACKGROUND: Rgb = (0x2F, 0x6F, 0xDE);
const STATUS_FOREGROUND: Rgb = (0xFF, 0xFF, 0xFF);

/// The framebuffer as a drawing surface.
struct Surface {
    /// Virtual address of the first pixel.
    base: *mut u32,
    /// Visible width in pixels.
    width: usize,
    /// Pixels per scanline.
    stride: usize,
    /// Visible height in pixels.
    height: usize,
    /// Byte 0 of a pixel is blue (otherwise red).
    bgr: bool,
    /// Cleared while a process owns the display: nothing is drawn.
    enabled: bool,
}

/// Where the framebuffer is and what it looks like.
#[derive(Debug, Clone, Copy)]
pub struct Framebuffer {
    /// Virtual address of the first pixel.
    pub base: *mut u32,
    /// Visible width in pixels.
    pub width: usize,
    /// Pixels per scanline.
    pub stride: usize,
    /// Visible height in pixels.
    pub height: usize,
    /// Byte 0 of a pixel is blue (otherwise red).
    pub bgr: bool,
}

// SAFETY: `base` points at the framebuffer mapping, which is valid for the
// lifetime of the kernel and only ever accessed through the `CONSOLE` lock,
// so the surface may be used from whichever context holds that lock.
unsafe impl Send for Surface {}

impl Surface {
    fn encode(&self, (r, g, b): Rgb) -> u32 {
        let (first, third) = if self.bgr { (b, r) } else { (r, b) };
        u32::from(first) | (u32::from(g) << 8) | (u32::from(third) << 16)
    }

    fn put_pixel(&mut self, x: usize, y: usize, colour: u32) {
        if self.enabled && x < self.width && y < self.height {
            // SAFETY: the mapping covers `stride * height` pixels and
            // `x < width <= stride`, `y < height`, so the pixel is inside
            // it. Device memory is written with volatile accesses.
            unsafe { self.base.add(y * self.stride + x).write_volatile(colour) };
        }
    }
}

/// Mixes `fg` over `bg` with coverage `alpha` (0 = all background).
fn blend(fg: Rgb, bg: Rgb, alpha: u8) -> Rgb {
    let mix = |f: u8, b: u8| {
        let (f, b, a) = (u32::from(f), u32::from(b), u32::from(alpha));
        ((f * a + b * (255 - a)) / 255) as u8
    };
    (mix(fg.0, bg.0), mix(fg.1, bg.1), mix(fg.2, bg.2))
}

impl Surface {
    /// Paints the whole screen in the console's background colours,
    /// including the margins the text grid does not cover, so that the
    /// status bar reaches the screen edge.
    fn clear(&mut self) {
        let background = self.encode(BACKGROUND);
        let status_background = self.encode(STATUS_BACKGROUND);
        for y in 0..self.height {
            let colour = if y < STATUS_ROWS * CELL_HEIGHT {
                status_background
            } else {
                background
            };
            for x in 0..self.width {
                self.put_pixel(x, y, colour);
            }
        }
    }
}

impl Display for Surface {
    fn draw(&mut self, col: usize, row: usize, cell: Cell, cursor: bool) {
        let (mut fg, mut bg) = match cell.style {
            Style::Normal => (FOREGROUND, BACKGROUND),
            Style::Status => (STATUS_FOREGROUND, STATUS_BACKGROUND),
        };
        if cursor {
            (fg, bg) = (bg, fg);
        }
        let (x0, y0) = (col * CELL_WIDTH, row * CELL_HEIGHT);
        let glyph = get_raster(char::from(cell.ch), FONT_WEIGHT, FONT_HEIGHT);
        for y in 0..CELL_HEIGHT {
            for x in 0..CELL_WIDTH {
                let coverage = glyph
                    .as_ref()
                    .and_then(|glyph| glyph.pixel(x, y))
                    .unwrap_or(0);
                let colour = self.encode(blend(fg, bg, coverage));
                self.put_pixel(x0 + x, y0 + y, colour);
            }
        }
    }
}

struct Console {
    surface: Surface,
    terminal: Terminal,
}

static CONSOLE: SpinLock<Option<Console>> = SpinLock::new(None);

/// What [`init`] set up.
#[derive(Debug, Clone, Copy)]
pub struct ConsoleInfo {
    /// Framebuffer width in pixels.
    pub width: u32,
    /// Framebuffer height in pixels.
    pub height: u32,
    /// Text columns.
    pub cols: usize,
    /// Text rows, including the status bar.
    pub rows: usize,
}

/// Why the framebuffer console is not available.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FbconError {
    /// The firmware provided no usable framebuffer.
    NoFramebuffer,
    /// The framebuffer is too small for even a status bar and one line.
    TooSmall,
    /// The framebuffer could not be mapped.
    Map(MapError),
}

impl fmt::Display for FbconError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoFramebuffer => write!(f, "no framebuffer"),
            Self::TooSmall => write!(f, "framebuffer is too small"),
            Self::Map(e) => write!(f, "cannot map framebuffer: {e}"),
        }
    }
}

/// Maps the framebuffer described by `boot_info`, clears it and starts
/// mirroring console output to it.
///
/// `boot_info` must have passed validation, and the frame allocator and
/// heap must be initialised.
pub fn init(boot_info: &BootInfo) -> Result<ConsoleInfo, FbconError> {
    if boot_info.framebuffer_format == FRAMEBUFFER_NONE {
        return Err(FbconError::NoFramebuffer);
    }
    let width = boot_info.framebuffer_width as usize;
    let height = boot_info.framebuffer_height as usize;
    let stride = boot_info.framebuffer_stride as usize;
    let terminal = Terminal::new(width / CELL_WIDTH, height / CELL_HEIGHT, STATUS_ROWS)
        .ok_or(FbconError::TooSmall)?;

    let bytes = (stride * height * 4) as u64;
    let base =
        paging::map_framebuffer(boot_info.framebuffer_phys, bytes).map_err(FbconError::Map)?;
    let mut surface = Surface {
        base: base as *mut u32,
        width,
        stride,
        height,
        bgr: boot_info.framebuffer_format == FRAMEBUFFER_BGR,
        enabled: true,
    };
    surface.clear();
    let info = ConsoleInfo {
        width: boot_info.framebuffer_width,
        height: boot_info.framebuffer_height,
        cols: terminal.cols(),
        rows: terminal.rows(),
    };
    let mut console = Console { surface, terminal };
    console.terminal.set_status("", &mut console.surface);
    console.terminal.redraw(&mut console.surface);
    // Show what was logged before the screen existed.
    crate::log::replay_early_log(|text| {
        console.terminal.write_str(text, &mut console.surface);
    });
    *CONSOLE.lock() = Some(console);
    Ok(info)
}

struct Writer<'a>(&'a mut Console);

impl Write for Writer<'_> {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        self.0.terminal.write_str(text, &mut self.0.surface);
        Ok(())
    }
}

/// Mirrors console output to the screen. Does nothing before [`init`], or
/// if the console is busy in the context this call interrupted (which can
/// only happen on the panic and exception paths; the serial console still
/// gets the message).
pub fn write_fmt(args: fmt::Arguments<'_>) {
    if let Some(mut guard) = CONSOLE.try_lock()
        && let Some(console) = guard.as_mut()
    {
        let _ = Writer(console).write_fmt(args);
    }
}

/// Replaces the text of the status bar.
pub fn set_status(text: &str) {
    if let Some(console) = CONSOLE.lock().as_mut() {
        console.terminal.set_status(text, &mut console.surface);
    }
}

/// The framebuffer, for the code that draws on behalf of a display owner.
pub fn framebuffer() -> Option<Framebuffer> {
    CONSOLE.lock().as_ref().map(|console| Framebuffer {
        base: console.surface.base,
        width: console.surface.width,
        stride: console.surface.stride,
        height: console.surface.height,
        bgr: console.surface.bgr,
    })
}

/// Stops drawing. Console output is still recorded and appears when
/// [`resume`] is called.
pub fn suspend() {
    if let Some(console) = CONSOLE.lock().as_mut() {
        console.surface.enabled = false;
    }
}

fn repaint(console: &mut Console) {
    console.surface.enabled = true;
    console.surface.clear();
    console.terminal.redraw(&mut console.surface);
}

/// Starts drawing again and repaints the whole screen.
pub fn resume() {
    if let Some(console) = CONSOLE.lock().as_mut() {
        repaint(console);
    }
}

/// For the panic and fatal-exception paths: makes sure the console is
/// visible, taking the screen away from a display owner if there is one.
/// Does nothing if the console is locked by the interrupted context.
pub fn resume_for_emergency() {
    if let Some(mut guard) = CONSOLE.try_lock()
        && let Some(console) = guard.as_mut()
        && !console.surface.enabled
    {
        repaint(console);
    }
}
