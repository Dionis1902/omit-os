// SPDX-License-Identifier: GPL-2.0-only
//! The screen as a resource for programs.
//!
//! One process at a time can own the display. While it does, the text
//! console keeps recording output but stops drawing, and keyboard and
//! mouse input goes to the owner (see [`crate::input`]). The owner draws
//! by handing rectangles of pixels to [`blit`]; the framebuffer itself is
//! never mapped into a process, so a program cannot draw outside the
//! screen or keep drawing after it lost ownership. When the owner
//! releases the display or exits — for whatever reason — the console
//! repaints itself.

use alloc::vec::Vec;

use omit_abi::{Blit, DisplayInfo, Errno};

use crate::sync::SpinLock;
use crate::{fbcon, input};

/// Process ID of the display owner.
static OWNER: SpinLock<Option<u64>> = SpinLock::new(None);

/// The size of the screen, if there is one.
pub fn info() -> Option<DisplayInfo> {
    fbcon::framebuffer().map(|fb| DisplayInfo {
        width: fb.width as u32,
        height: fb.height as u32,
    })
}

/// Makes process `pid` the display owner.
pub fn acquire(pid: u64) -> Result<(), Errno> {
    if fbcon::framebuffer().is_none() {
        return Err(Errno::NotFound);
    }
    {
        let mut owner = OWNER.lock();
        if owner.is_some() {
            return Err(Errno::Busy);
        }
        *owner = Some(pid);
    }
    fbcon::suspend();
    input::set_grabbed(true);
    Ok(())
}

/// Ends process `pid`'s ownership of the display, if it has it, and gives
/// the screen back to the text console. Returns whether it was the owner.
pub fn release(pid: u64) -> bool {
    {
        let mut owner = OWNER.lock();
        if *owner != Some(pid) {
            return false;
        }
        *owner = None;
    }
    input::set_grabbed(false);
    fbcon::resume();
    true
}

/// Whether process `pid` owns the display.
pub fn is_owner(pid: u64) -> bool {
    *OWNER.lock() == Some(pid)
}

/// Copies the rectangle described by `request` to the screen on behalf of
/// the display owner `pid`. `read` copies bytes from the caller's memory
/// (address, destination) and reports whether that worked.
pub fn blit(
    pid: u64,
    request: &Blit,
    mut read: impl FnMut(u64, &mut [u8]) -> bool,
) -> Result<(), Errno> {
    if !is_owner(pid) {
        return Err(Errno::Busy);
    }
    let fb = fbcon::framebuffer().ok_or(Errno::NotFound)?;
    let (x, y) = (request.x as usize, request.y as usize);
    let (width, height) = (request.width as usize, request.height as usize);
    let stride = request.stride as usize;
    // u32 values on a 64-bit machine: the sums cannot overflow.
    if width == 0 || height == 0 {
        return Ok(());
    }
    if x + width > fb.width || y + height > fb.height || stride < width {
        return Err(Errno::InvalidArgument);
    }

    let mut row = Vec::new();
    row.try_reserve_exact(width * 4)
        .map_err(|_| Errno::NoSpace)?;
    row.resize(width * 4, 0u8);
    for line in 0..height {
        let source = (line as u64)
            .checked_mul(stride as u64 * 4)
            .and_then(|offset| request.pixels.checked_add(offset))
            .ok_or(Errno::Fault)?;
        if !read(source, &mut row) {
            return Err(Errno::Fault);
        }
        if !fb.bgr {
            // The screen wants red first; programs supply blue first.
            for pixel in row.chunks_exact_mut(4) {
                pixel.swap(0, 2);
            }
        }
        // SAFETY: the rectangle was checked against the screen size, so
        // the destination lies within the framebuffer mapping
        // (`stride * height` pixels, `x + width <= width <= stride`). The
        // source is a kernel buffer of exactly `width` pixels. The console
        // does not draw while a process owns the display, and only the
        // owner's single thread gets here, so nothing else writes to the
        // framebuffer; it is ordinary memory as far as writes go.
        unsafe {
            let destination = fb.base.add((y + line) * fb.stride + x).cast::<u8>();
            core::ptr::copy_nonoverlapping(row.as_ptr(), destination, width * 4);
        }
    }
    Ok(())
}
