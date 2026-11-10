// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: GPL-2.0-only
//! LinuxKPI watchdog core.
//!
//! Linux watchdog drivers describe their device with a
//! `struct watchdog_device` and a table of operations and hand both to
//! `watchdog_register_device`. On Linux the watchdog core then exposes the
//! device to user space; here it is exposed to the rest of the kernel
//! through the functions at the end of this module.

use alloc::string::String;
use core::ffi::{c_int, c_uint, c_ulong, c_void};

use super::{Device, EBUSY, EINVAL, status_to_result};
use crate::sync::SpinLock;

const WDIOF_CARDRESET: u32 = 0x0020;
const WDOG_ACTIVE: c_ulong = 1 << 0;
const WDOG_STOP_ON_UNREGISTER: c_ulong = 1 << 4;

/// `struct watchdog_info`.
#[repr(C)]
pub struct WatchdogInfo {
    options: u32,
    firmware_version: u32,
    identity: [u8; 32],
}

/// `struct watchdog_ops`.
#[repr(C)]
pub struct WatchdogOps {
    owner: *mut c_void,
    start: Option<unsafe extern "C" fn(*mut WatchdogDevice) -> c_int>,
    stop: Option<unsafe extern "C" fn(*mut WatchdogDevice) -> c_int>,
    ping: Option<unsafe extern "C" fn(*mut WatchdogDevice) -> c_int>,
    status: Option<unsafe extern "C" fn(*mut WatchdogDevice) -> c_uint>,
    pub(super) set_timeout: Option<unsafe extern "C" fn(*mut WatchdogDevice, c_uint) -> c_int>,
    get_timeleft: Option<unsafe extern "C" fn(*mut WatchdogDevice) -> c_uint>,
}

/// `struct watchdog_device`.
#[repr(C)]
pub struct WatchdogDevice {
    info: *const WatchdogInfo,
    ops: *const WatchdogOps,
    bootstatus: c_uint,
    pub(super) timeout: c_uint,
    min_timeout: c_uint,
    max_timeout: c_uint,
    driver_data: *mut c_void,
    pub(super) status: c_ulong,
}

/// The registered watchdog. Linux supports several; one is enough here.
struct Registered(*mut WatchdogDevice);

// SAFETY: the pointer refers to driver memory that stays valid until the
// driver unregisters; all use goes through the `WATCHDOG` lock.
unsafe impl Send for Registered {}

static WATCHDOG: SpinLock<Option<Registered>> = SpinLock::new(None);

/// `watchdog_init_timeout`: applies a timeout given as a module parameter,
/// if it is within the device's limits.
///
/// # Safety
///
/// `wdd` must point to a valid `struct watchdog_device`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn watchdog_init_timeout(
    wdd: *mut WatchdogDevice,
    timeout_parm: c_uint,
    _dev: *mut Device,
) -> c_int {
    if timeout_parm == 0 {
        // No parameter given: keep the driver's default.
        return 0;
    }
    // SAFETY: guaranteed by the caller.
    let device = unsafe { &mut *wdd };
    if timeout_parm < device.min_timeout || timeout_parm > device.max_timeout {
        return -EINVAL;
    }
    device.timeout = timeout_parm;
    0
}

/// `watchdog_register_device`.
///
/// # Safety
///
/// `wdd` must point to a `struct watchdog_device` that stays valid, with
/// valid `info` and `ops`, until it is unregistered.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn watchdog_register_device(wdd: *mut WatchdogDevice) -> c_int {
    // SAFETY: guaranteed by the caller.
    let device = unsafe { &*wdd };
    if device.info.is_null() || device.ops.is_null() {
        return -EINVAL;
    }
    let mut slot = WATCHDOG.lock();
    if slot.is_some() {
        return -EBUSY;
    }
    *slot = Some(Registered(wdd));
    0
}

/// `watchdog_unregister_device`: stops the watchdog if the driver asked
/// for that, then forgets it.
///
/// # Safety
///
/// `wdd` must be the device previously registered.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn watchdog_unregister_device(wdd: *mut WatchdogDevice) {
    let mut slot = WATCHDOG.lock();
    if slot.as_ref().is_some_and(|registered| registered.0 == wdd) {
        // SAFETY: still registered, so the device and its operations are
        // valid; `stop` is a driver function taking this device.
        unsafe {
            let device = &mut *wdd;
            let active = device.status & WDOG_ACTIVE != 0;
            if active
                && device.status & WDOG_STOP_ON_UNREGISTER != 0
                && let Some(stop) = (*device.ops).stop
            {
                stop(wdd);
            }
        }
        *slot = None;
    }
}

/// What is known about the registered watchdog.
#[derive(Debug, Clone)]
pub struct WatchdogStatus {
    /// The driver's name for the device.
    pub identity: String,
    /// Seconds until expiry after each keep-alive.
    pub timeout: u32,
    /// Smallest and largest timeout the device supports.
    pub limits: (u32, u32),
    /// Whether the watchdog is counting down.
    pub active: bool,
    /// Whether the driver found that the last reset was caused by the
    /// watchdog expiring.
    pub caused_last_reset: bool,
}

/// Runs `f` on the registered watchdog device.
fn with_device<R>(
    f: impl FnOnce(*mut WatchdogDevice, &WatchdogOps) -> R,
) -> Result<R, &'static str> {
    let slot = WATCHDOG.lock();
    let registered = slot.as_ref().ok_or("no watchdog device")?;
    // SAFETY: a registered device and its operations table stay valid
    // until unregistered, which cannot happen while the lock is held.
    let ops = unsafe { &*(*registered.0).ops };
    Ok(f(registered.0, ops))
}

/// Describes the registered watchdog, if any.
pub fn status() -> Option<WatchdogStatus> {
    with_device(|wdd, _| {
        // SAFETY: see `with_device`; `info` was checked at registration.
        let (device, info) = unsafe { (&*wdd, &*(*wdd).info) };
        let end = info
            .identity
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(info.identity.len());
        WatchdogStatus {
            identity: String::from_utf8_lossy(&info.identity[..end]).into(),
            timeout: device.timeout,
            limits: (device.min_timeout, device.max_timeout),
            active: device.status & WDOG_ACTIVE != 0,
            caused_last_reset: device.bootstatus & WDIOF_CARDRESET != 0,
        }
    })
    .ok()
}

/// Starts the countdown. Unless [`ping`] is called within the timeout, the
/// hardware resets the machine.
pub fn start() -> Result<(), &'static str> {
    with_device(|wdd, ops| {
        let start = ops.start.ok_or("driver cannot start the watchdog")?;
        // SAFETY: a driver operation called with its own registered device.
        status_to_result(unsafe { start(wdd) })?;
        // SAFETY: see `with_device`.
        unsafe { (*wdd).status |= WDOG_ACTIVE };
        Ok(())
    })?
}

/// Stops the countdown.
pub fn stop() -> Result<(), &'static str> {
    with_device(|wdd, ops| {
        let stop = ops.stop.ok_or("driver cannot stop the watchdog")?;
        // SAFETY: a driver operation called with its own registered device.
        status_to_result(unsafe { stop(wdd) })?;
        // SAFETY: see `with_device`.
        unsafe { (*wdd).status &= !WDOG_ACTIVE };
        Ok(())
    })?
}

/// Restarts the countdown ("keep-alive").
pub fn ping() -> Result<(), &'static str> {
    with_device(|wdd, ops| {
        // Linux falls back to `start` for drivers without `ping`.
        let ping = ops.ping.or(ops.start).ok_or("driver cannot ping")?;
        // SAFETY: a driver operation called with its own registered device.
        status_to_result(unsafe { ping(wdd) })
    })?
}

/// Sets the timeout in seconds.
pub fn set_timeout(seconds: u32) -> Result<(), &'static str> {
    with_device(|wdd, ops| {
        // SAFETY: see `with_device`.
        let (min, max) = unsafe { ((*wdd).min_timeout, (*wdd).max_timeout) };
        if seconds < min || seconds > max {
            return Err("timeout outside the range the device supports");
        }
        let set = ops.set_timeout.ok_or("driver cannot change the timeout")?;
        // SAFETY: a driver operation called with its own registered device
        // and a timeout within its advertised limits.
        status_to_result(unsafe { set(wdd, seconds) })
    })?
}
