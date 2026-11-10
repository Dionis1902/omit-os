// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: GPL-2.0-only
//! LinuxKPI notifier chains and power-off handlers.
//!
//! Linux drivers hook into system events by registering callbacks. Two
//! kinds are implemented:
//!
//! * the *panic notifier chain*: called by the Omit OS panic handler after
//!   it has printed its report (see [`call_panic_notifiers`]);
//! * *sys-off handlers*: called when the system shuts down in an orderly
//!   way (see [`call_power_off_handlers`]).

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::ffi::{c_char, c_int, c_ulong, c_void};
use core::sync::atomic::{AtomicPtr, Ordering};

use super::{Device, EINVAL};
use crate::sync::SpinLock;

const NOTIFY_STOP_MASK: c_int = 0x8000;

/// `struct notifier_block`.
#[repr(C)]
pub struct NotifierBlock {
    notifier_call: Option<unsafe extern "C" fn(*mut NotifierBlock, c_ulong, *mut c_void) -> c_int>,
    next: *mut NotifierBlock,
    pub(super) priority: c_int,
}

/// `struct atomic_notifier_head`: the first block of a chain. The blocks
/// are linked through their `next` fields in descending priority order.
#[repr(C)]
pub struct AtomicNotifierHead {
    head: AtomicPtr<NotifierBlock>,
}

/// The chain Linux drivers register on to learn about a kernel panic.
#[unsafe(no_mangle)]
#[allow(
    non_upper_case_globals,
    reason = "the name is fixed by the Linux drivers that refer to it"
)]
pub static panic_notifier_list: AtomicNotifierHead = AtomicNotifierHead {
    head: AtomicPtr::new(core::ptr::null_mut()),
};

/// Serialises changes to notifier chains. Chains are *called* without it,
/// as on Linux, because the panic path must not wait for a lock.
static CHAIN_LOCK: SpinLock<()> = SpinLock::new(());

/// `atomic_notifier_chain_register`.
///
/// # Safety
///
/// `chain` must point to a chain head and `block` to a notifier block that
/// stays valid until it is unregistered and is not on any chain.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn atomic_notifier_chain_register(
    chain: *mut AtomicNotifierHead,
    block: *mut NotifierBlock,
) -> c_int {
    if chain.is_null() || block.is_null() {
        return -EINVAL;
    }
    let _guard = CHAIN_LOCK.lock();
    // SAFETY: the caller guarantees both pointers; the lock excludes other
    // writers, and every block on the chain is valid while registered.
    // The new block is fully initialised before it becomes reachable, so
    // a concurrent walk (from an interrupt that panics) sees a consistent
    // chain.
    unsafe {
        let priority = (*block).priority;
        let mut link: *const AtomicPtr<NotifierBlock> = &raw const (*chain).head;
        loop {
            let next = (*link).load(Ordering::Acquire);
            if next.is_null() || (*next).priority < priority {
                (*block).next = next;
                (*link).store(block, Ordering::Release);
                return 0;
            }
            // `next` is a plain pointer field; it is only written under
            // the lock, and read here under the lock.
            link = (&raw const (*next).next).cast();
        }
    }
}

/// `atomic_notifier_chain_unregister`.
///
/// # Safety
///
/// `chain` must point to a chain head.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn atomic_notifier_chain_unregister(
    chain: *mut AtomicNotifierHead,
    block: *mut NotifierBlock,
) -> c_int {
    const ENOENT: c_int = 2;
    if chain.is_null() || block.is_null() {
        return -EINVAL;
    }
    let _guard = CHAIN_LOCK.lock();
    // SAFETY: as in `atomic_notifier_chain_register`.
    unsafe {
        let mut link: *const AtomicPtr<NotifierBlock> = &raw const (*chain).head;
        loop {
            let next = (*link).load(Ordering::Acquire);
            if next.is_null() {
                return -ENOENT;
            }
            if next == block {
                (*link).store((*block).next, Ordering::Release);
                return 0;
            }
            link = (&raw const (*next).next).cast();
        }
    }
}

/// Calls every callback on the panic notifier chain. Called by the panic
/// handler with interrupts disabled; takes no locks.
pub fn call_panic_notifiers() {
    let mut block = panic_notifier_list.head.load(Ordering::Acquire);
    // Bounded, in case the panic was caused by this very list being
    // corrupted into a cycle.
    for _ in 0..64 {
        if block.is_null() {
            return;
        }
        // SAFETY: blocks on the chain are valid while registered (the
        // registering driver's contract), and nothing unregisters during
        // a panic: interrupts are off and there is one CPU. The callback
        // is called the way Linux calls it.
        unsafe {
            let next = (*block).next;
            if let Some(call) = (*block).notifier_call
                && call(block, 0, core::ptr::null_mut()) & NOTIFY_STOP_MASK != 0
            {
                return;
            }
            block = next;
        }
    }
}

/// `SYS_OFF_MODE_POWER_OFF`.
const MODE_POWER_OFF: c_int = 1;

/// `struct sys_off_data`.
#[repr(C)]
pub struct SysOffData {
    mode: c_int,
    cb_data: *mut c_void,
    cmd: *const c_char,
    dev: *mut Device,
}

struct SysOffHandler {
    mode: c_int,
    priority: c_int,
    callback: unsafe extern "C" fn(*mut SysOffData) -> c_int,
    cb_data: *mut c_void,
}

/// Registered handlers, as the pointers handed back to the drivers.
struct Handlers(Vec<*mut SysOffHandler>);

// SAFETY: the pointers are heap allocations owned by this list, and the
// callback data belongs to trusted kernel drivers that expect to be called
// from any context; all access goes through the lock.
unsafe impl Send for Handlers {}

static SYS_OFF: SpinLock<Handlers> = SpinLock::new(Handlers(Vec::new()));

/// `register_sys_off_handler`. Returns an opaque handle, or an error
/// pointer.
///
/// # Safety
///
/// `callback` must be callable with a `struct sys_off_data` until the
/// handler is unregistered.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn register_sys_off_handler(
    mode: c_int,
    priority: c_int,
    callback: Option<unsafe extern "C" fn(*mut SysOffData) -> c_int>,
    cb_data: *mut c_void,
) -> *mut c_void {
    let Some(callback) = callback else {
        // ERR_PTR(-EINVAL)
        return (-(EINVAL as isize)) as *mut c_void;
    };
    let handler = Box::into_raw(Box::new(SysOffHandler {
        mode,
        priority,
        callback,
        cb_data,
    }));
    SYS_OFF.lock().0.push(handler);
    handler.cast()
}

/// `unregister_sys_off_handler`: accepts null and error pointers, as on
/// Linux.
///
/// # Safety
///
/// `handler` must be null, an error pointer, or a handle returned by
/// [`register_sys_off_handler`] that has not been unregistered.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn unregister_sys_off_handler(handler: *mut c_void) {
    let handler: *mut SysOffHandler = handler.cast();
    let removed = {
        let mut list = SYS_OFF.lock();
        list.0
            .iter()
            .position(|&registered| registered == handler)
            .map(|index| list.0.remove(index))
    };
    if let Some(handler) = removed {
        // SAFETY: created by `Box::into_raw` in `register_sys_off_handler`
        // and just removed from the list, so this is the only reference.
        drop(unsafe { Box::from_raw(handler) });
    }
}

/// Calls the power-off handlers, highest priority first. Called when the
/// system shuts down in an orderly way. Returns how many were called.
pub fn call_power_off_handlers() -> usize {
    let mut handlers: Vec<(c_int, unsafe extern "C" fn(*mut SysOffData) -> c_int, usize)> = {
        let list = SYS_OFF.lock();
        list.0
            .iter()
            // SAFETY: pointers on the list are valid until unregistered.
            .map(|&handler| unsafe { &*handler })
            .filter(|handler| handler.mode == MODE_POWER_OFF)
            .map(|handler| (handler.priority, handler.callback, handler.cb_data as usize))
            .collect()
    };
    handlers.sort_by_key(|&(priority, _, _)| core::cmp::Reverse(priority));
    for &(_, callback, cb_data) in &handlers {
        let mut data = SysOffData {
            mode: MODE_POWER_OFF,
            cb_data: cb_data as *mut c_void,
            cmd: core::ptr::null(),
            dev: core::ptr::null_mut(),
        };
        // SAFETY: a registered callback, called with the structure it
        // expects; no lock is held, so it may unregister itself.
        unsafe { callback(&raw mut data) };
    }
    handlers.len()
}
