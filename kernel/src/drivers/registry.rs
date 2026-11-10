// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: GPL-2.0-only
//! Record of which driver is bound to which device.

use alloc::string::String;
use alloc::vec::Vec;

use crate::sync::SpinLock;

/// Where a driver's implementation comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// Written for Omit OS.
    Native,
    /// An unmodified Linux driver running on the LinuxKPI layer.
    Linux,
}

impl Origin {
    /// Short label for listings.
    pub fn label(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::Linux => "linux-kpi",
        }
    }
}

/// One driver-to-device binding.
#[derive(Debug, Clone)]
pub struct Binding {
    /// The driver's name.
    pub driver: String,
    /// Native or Linux.
    pub origin: Origin,
    /// The device, as `bus:device.function`.
    pub device: String,
}

static BINDINGS: SpinLock<Vec<Binding>> = SpinLock::new(Vec::new());

/// Records that `driver` now controls `device`.
pub fn bind(driver: &str, origin: Origin, device: &str) {
    BINDINGS.lock().push(Binding {
        driver: driver.into(),
        origin,
        device: device.into(),
    });
}

/// Removes the binding of `device`, if any.
pub fn unbind(device: &str) {
    BINDINGS.lock().retain(|binding| binding.device != device);
}

/// All current bindings, in the order they were made.
pub fn bindings() -> Vec<Binding> {
    BINDINGS.lock().clone()
}
