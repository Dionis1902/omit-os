// SPDX-License-Identifier: GPL-2.0-only
//! `winplugin.dll` — a Windows library that is not imported by any
//! program but loaded on request with `LoadLibrary`, and that itself
//! imports from another library, `winsample.dll`.

#![no_std]

use core::ffi::c_void;
use core::sync::atomic::{AtomicU32, Ordering};

// Linked for the runtime support (panic handler, memory functions).
use omit_win_samples as _;

#[link(name = "winsample", kind = "raw-dylib")]
unsafe extern "system" {
    safe fn sample_add(a: i32, b: i32) -> i32;
    safe fn sample_attach_count() -> u32;
}

/// 1 if loaded together with a program, 2 if loaded on request; 0 before
/// the initialisation routine has run.
static HOW_LOADED: AtomicU32 = AtomicU32::new(0);
/// What `winsample.dll` reported when this library was initialised: it
/// must already have been initialised itself.
static DEPENDENCY_READY: AtomicU32 = AtomicU32::new(0);

/// The library's entry point.
#[unsafe(no_mangle)]
pub extern "system" fn _DllMainCRTStartup(
    _module: *mut c_void,
    reason: u32,
    reserved: *mut c_void,
) -> i32 {
    if reason == 1 {
        HOW_LOADED.store(if reserved.is_null() { 2 } else { 1 }, Ordering::Relaxed);
        DEPENDENCY_READY.store(sample_attach_count(), Ordering::Relaxed);
    }
    1
}

/// Computes `value + 100` using the other library.
#[unsafe(no_mangle)]
pub extern "system" fn plugin_compute(value: i32) -> i32 {
    sample_add(value, 100)
}

/// See [`HOW_LOADED`].
#[unsafe(no_mangle)]
pub extern "system" fn plugin_how_loaded() -> u32 {
    HOW_LOADED.load(Ordering::Relaxed)
}

/// See [`DEPENDENCY_READY`].
#[unsafe(no_mangle)]
pub extern "system" fn plugin_dependency_ready() -> u32 {
    DEPENDENCY_READY.load(Ordering::Relaxed)
}
