// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: GPL-2.0-only
//! Hardware compatibility database.
//!
//! Maps PCI vendor and device IDs to human-readable names and to what Omit
//! OS can do with the device. It is the single place that answers "is this
//! hardware supported, and by which driver?".
//!
//! An entry claims support only for a driver that exists in this
//! repository and has been validated at least in emulation; see the
//! `validated` field. Everything else is listed, at most, by name.
//!
//! The database is compiled in. It is deliberately small: it covers the
//! devices the system has drivers for and the ones commonly present in the
//! virtual machines it is tested on.

#![no_std]
#![forbid(unsafe_code)]

#[cfg(test)]
extern crate std;

/// Whether, and how, a device is supported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Support {
    /// Driven by a driver written for Omit OS.
    Native {
        /// The driver's name.
        driver: &'static str,
    },
    /// Driven by an unmodified Linux driver on the LinuxKPI layer.
    Linux {
        /// The driver's name.
        driver: &'static str,
    },
    /// Needed by the platform and handled by core kernel code or left to
    /// firmware; no separate driver is involved.
    Platform,
    /// Recognised, but there is no driver.
    Unsupported,
}

/// Where a driver has been shown to work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Validation {
    /// Not applicable (no driver).
    None,
    /// Tested against an emulated device only.
    Emulation,
}

/// One known device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceInfo {
    /// PCI vendor ID.
    pub vendor: u16,
    /// PCI device ID.
    pub device: u16,
    /// Product name.
    pub name: &'static str,
    /// Support status.
    pub support: Support,
    /// How far the claimed support has been validated.
    pub validated: Validation,
}

const fn entry(
    vendor: u16,
    device: u16,
    name: &'static str,
    support: Support,
    validated: Validation,
) -> DeviceInfo {
    DeviceInfo {
        vendor,
        device,
        name,
        support,
        validated,
    }
}

const VIRTIO_BLK: Support = Support::Native {
    driver: "virtio-blk",
};

const VIRTIO_NET: Support = Support::Native {
    driver: "virtio-net",
};

/// Known devices, sorted by vendor then device ID.
pub const DEVICES: &[DeviceInfo] = &[
    entry(
        0x1234,
        0x1111,
        "QEMU standard VGA",
        Support::Platform,
        Validation::None,
    ),
    entry(
        0x1AF4,
        0x1000,
        "VirtIO network device (transitional)",
        VIRTIO_NET,
        Validation::Emulation,
    ),
    entry(
        0x1AF4,
        0x1001,
        "VirtIO block device (transitional)",
        VIRTIO_BLK,
        Validation::Emulation,
    ),
    entry(
        0x1AF4,
        0x1003,
        "VirtIO console (transitional)",
        Support::Unsupported,
        Validation::None,
    ),
    entry(
        0x1AF4,
        0x1005,
        "VirtIO entropy source (transitional)",
        Support::Unsupported,
        Validation::None,
    ),
    entry(
        0x1AF4,
        0x1041,
        "VirtIO network device",
        VIRTIO_NET,
        Validation::Emulation,
    ),
    entry(
        0x1AF4,
        0x1042,
        "VirtIO block device",
        VIRTIO_BLK,
        Validation::Emulation,
    ),
    entry(
        0x1AF4,
        0x1050,
        "VirtIO GPU",
        Support::Unsupported,
        Validation::None,
    ),
    entry(
        0x1B36,
        0x000D,
        "QEMU xHCI USB controller",
        Support::Native { driver: "xhci" },
        Validation::Emulation,
    ),
    entry(
        0x1B36,
        0x0010,
        "QEMU NVMe controller",
        Support::Native { driver: "nvme" },
        Validation::Emulation,
    ),
    entry(
        0x1B36,
        0x0011,
        "QEMU pvpanic device",
        Support::Linux {
            driver: "pvpanic-pci",
        },
        Validation::Emulation,
    ),
    entry(
        0x8086,
        0x100E,
        "Intel 82540EM Gigabit Ethernet",
        Support::Native { driver: "e1000" },
        Validation::Emulation,
    ),
    entry(
        0x8086,
        0x100F,
        "Intel 82545EM Gigabit Ethernet",
        Support::Native { driver: "e1000" },
        Validation::Emulation,
    ),
    entry(
        0x8086,
        0x10D3,
        "Intel 82574L Gigabit Ethernet",
        Support::Unsupported,
        Validation::None,
    ),
    entry(
        0x8086,
        0x1237,
        "Intel 440FX host bridge",
        Support::Platform,
        Validation::None,
    ),
    entry(
        0x8086,
        0x25AB,
        "Intel 6300ESB watchdog timer",
        Support::Linux { driver: "i6300esb" },
        Validation::Emulation,
    ),
    entry(
        0x8086,
        0x2918,
        "Intel ICH9 LPC interface",
        Support::Platform,
        Validation::None,
    ),
    entry(
        0x8086,
        0x2922,
        "Intel ICH9 SATA controller (AHCI)",
        Support::Native { driver: "ahci" },
        Validation::Emulation,
    ),
    entry(
        0x8086,
        0x2930,
        "Intel ICH9 SMBus controller",
        Support::Unsupported,
        Validation::None,
    ),
    entry(
        0x8086,
        0x29C0,
        "Intel Q35 host bridge",
        Support::Platform,
        Validation::None,
    ),
    entry(
        0x8086,
        0x7000,
        "Intel PIIX3 ISA bridge",
        Support::Platform,
        Validation::None,
    ),
];

/// Vendor names, sorted by ID.
const VENDORS: &[(u16, &str)] = &[
    (0x1002, "AMD (ATI)"),
    (0x1022, "AMD"),
    (0x10DE, "NVIDIA"),
    (0x10EC, "Realtek"),
    (0x1234, "QEMU"),
    (0x14E4, "Broadcom"),
    (0x15AD, "VMware"),
    (0x1AF4, "Red Hat (VirtIO)"),
    (0x1B36, "Red Hat (QEMU)"),
    (0x8086, "Intel"),
];

/// Looks up a device by its PCI IDs.
pub fn lookup(vendor: u16, device: u16) -> Option<&'static DeviceInfo> {
    DEVICES
        .binary_search_by_key(&(vendor, device), |info| (info.vendor, info.device))
        .ok()
        .map(|index| &DEVICES[index])
}

/// The name of a PCI vendor, if known.
pub fn vendor_name(vendor: u16) -> Option<&'static str> {
    VENDORS
        .binary_search_by_key(&vendor, |&(id, _)| id)
        .ok()
        .map(|index| VENDORS[index].1)
}

/// A short description of the support status, for listings.
pub fn describe(support: Support) -> (&'static str, &'static str) {
    match support {
        Support::Native { driver } => ("native driver", driver),
        Support::Linux { driver } => ("Linux driver", driver),
        Support::Platform => ("platform device", ""),
        Support::Unsupported => ("no driver", ""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tables_are_sorted_and_unique() {
        assert!(
            DEVICES
                .windows(2)
                .all(|w| (w[0].vendor, w[0].device) < (w[1].vendor, w[1].device))
        );
        assert!(VENDORS.windows(2).all(|w| w[0].0 < w[1].0));
    }

    #[test]
    fn lookups() {
        let watchdog = lookup(0x8086, 0x25AB).unwrap();
        assert_eq!(watchdog.name, "Intel 6300ESB watchdog timer");
        assert_eq!(watchdog.support, Support::Linux { driver: "i6300esb" });
        assert_eq!(
            lookup(0x1AF4, 0x1001).unwrap().support,
            Support::Native {
                driver: "virtio-blk"
            }
        );
        assert_eq!(lookup(0x8086, 0xFFFF), None);
        assert_eq!(lookup(0xFFFF, 0x1001), None);
        assert_eq!(vendor_name(0x8086), Some("Intel"));
        assert_eq!(vendor_name(0x0001), None);
    }

    #[test]
    fn support_claims_are_backed_by_validation() {
        for info in DEVICES {
            let has_driver = matches!(info.support, Support::Native { .. } | Support::Linux { .. });
            assert_eq!(
                has_driver,
                info.validated != Validation::None,
                "{}: a driver must be claimed exactly when it has been validated",
                info.name
            );
            assert!(
                vendor_name(info.vendor).is_some(),
                "{}: unknown vendor",
                info.name
            );
        }
    }

    #[test]
    fn descriptions() {
        assert_eq!(
            describe(Support::Linux { driver: "x" }),
            ("Linux driver", "x")
        );
        assert_eq!(describe(Support::Unsupported), ("no driver", ""));
    }
}
