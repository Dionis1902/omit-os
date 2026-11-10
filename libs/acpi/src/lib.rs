// SPDX-License-Identifier: GPL-2.0-only
//! Minimal ACPI table parser.
//!
//! Parses only the static tables the kernel needs to find its interrupt
//! controllers: the RSDP, the RSDT/XSDT, and the MADT ("APIC" table). There
//! is no AML interpreter.
//!
//! All functions take byte slices and validate lengths and checksums, so
//! firmware-provided data is never trusted. The crate has no `unsafe` code;
//! the kernel is responsible for turning physical table addresses into
//! slices.

#![no_std]
#![forbid(unsafe_code)]

/// The ACPI power-management timer: a counter running at a fixed
/// 3.579545 MHz, read from an I/O port. A reference clock that needs no
/// calibration itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PmTimer {
    /// The I/O port of the 32-bit counter register.
    pub port: u16,
    /// Whether the counter is 32 bits wide (otherwise the low 24 bits
    /// count and the rest read as zero).
    pub wide: bool,
}

/// Frequency of the power-management timer, in Hz.
pub const PM_TIMER_HZ: u32 = 3_579_545;

impl PmTimer {
    /// Finds the power-management timer in the Fixed ACPI Description
    /// Table (signature `FACP`). `None` if `table` is another table, or
    /// the machine has no such timer, or it is not in I/O port space.
    pub fn parse(table: &Table<'_>) -> Option<Self> {
        // Offsets within the table body (the table minus its header).
        const BLOCK: usize = 76 - SDT_HEADER_SIZE;
        const LENGTH: usize = 91 - SDT_HEADER_SIZE;
        const FLAGS: usize = 112 - SDT_HEADER_SIZE;
        const EXTENDED_BLOCK: usize = 208 - SDT_HEADER_SIZE;
        /// Flag: the timer counts 32 bits.
        const TIMER_32_BIT: u32 = 1 << 8;
        /// Address space of an extended address: system I/O.
        const SPACE_IO: u8 = 1;
        if &table.signature() != b"FACP" {
            return None;
        }
        let body = table.body();
        let dword = |at: usize| {
            body.get(at..at + 4)
                .and_then(|bytes| bytes.try_into().ok())
                .map(u32::from_le_bytes)
        };
        if *body.get(LENGTH)? != 4 {
            return None;
        }
        let wide = dword(FLAGS)? & TIMER_32_BIT != 0;
        // The extended address, where present and set, takes precedence.
        if let Some(extended) = body.get(EXTENDED_BLOCK..EXTENDED_BLOCK + 12) {
            let address = u64::from_le_bytes(extended[4..12].try_into().ok()?);
            if address != 0 {
                let port = u16::try_from(address).ok()?;
                return (extended[0] == SPACE_IO).then_some(Self { port, wide });
            }
        }
        let port = u16::try_from(dword(BLOCK)?)
            .ok()
            .filter(|&port| port != 0)?;
        Some(Self { port, wide })
    }
}

#[cfg(test)]
extern crate std;

/// Size of the header common to all system description tables.
pub const SDT_HEADER_SIZE: usize = 36;
/// Size of an ACPI 1.0 RSDP.
pub const RSDP_V1_SIZE: usize = 20;
/// Size of an ACPI 2.0+ RSDP.
pub const RSDP_V2_SIZE: usize = 36;

/// Why a table was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcpiError {
    /// The slice is shorter than the structure it should contain.
    Truncated,
    /// The signature is not the expected one.
    BadSignature,
    /// The bytes do not sum to zero.
    BadChecksum,
    /// A length field is inconsistent with the data.
    BadLength,
}

impl core::fmt::Display for AcpiError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::Truncated => "table is truncated",
            Self::BadSignature => "unexpected table signature",
            Self::BadChecksum => "table checksum is wrong",
            Self::BadLength => "table length is inconsistent",
        })
    }
}

fn checksum_ok(bytes: &[u8]) -> bool {
    bytes.iter().fold(0u8, |sum, &b| sum.wrapping_add(b)) == 0
}

fn u16_at(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    let mut raw = [0u8; 4];
    raw.copy_from_slice(&bytes[offset..offset + 4]);
    u32::from_le_bytes(raw)
}

fn u64_at(bytes: &[u8], offset: usize) -> u64 {
    let mut raw = [0u8; 8];
    raw.copy_from_slice(&bytes[offset..offset + 8]);
    u64::from_le_bytes(raw)
}

/// The Root System Description Pointer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rsdp {
    /// 0 for ACPI 1.0, 2 or more for ACPI 2.0+.
    pub revision: u8,
    /// Physical address of the RSDT (32-bit table pointers).
    pub rsdt_address: u32,
    /// Physical address of the XSDT (64-bit table pointers), if present.
    pub xsdt_address: Option<u64>,
}

impl Rsdp {
    /// Parses and validates an RSDP. `bytes` must hold at least
    /// [`RSDP_V1_SIZE`] bytes, and [`RSDP_V2_SIZE`] if the revision turns
    /// out to be 2 or higher.
    pub fn parse(bytes: &[u8]) -> Result<Self, AcpiError> {
        if bytes.len() < RSDP_V1_SIZE {
            return Err(AcpiError::Truncated);
        }
        if &bytes[..8] != b"RSD PTR " {
            return Err(AcpiError::BadSignature);
        }
        if !checksum_ok(&bytes[..RSDP_V1_SIZE]) {
            return Err(AcpiError::BadChecksum);
        }
        let revision = bytes[15];
        let rsdt_address = u32_at(bytes, 16);
        if revision < 2 {
            return Ok(Self {
                revision,
                rsdt_address,
                xsdt_address: None,
            });
        }

        if bytes.len() < RSDP_V2_SIZE {
            return Err(AcpiError::Truncated);
        }
        let length = u32_at(bytes, 20) as usize;
        if length < RSDP_V2_SIZE || length > bytes.len() {
            return Err(AcpiError::BadLength);
        }
        if !checksum_ok(&bytes[..length]) {
            return Err(AcpiError::BadChecksum);
        }
        let xsdt = u64_at(bytes, 24);
        Ok(Self {
            revision,
            rsdt_address,
            xsdt_address: (xsdt != 0).then_some(xsdt),
        })
    }
}

/// Reads the `length` field of a table header without validating the rest.
/// The kernel uses this to learn how many bytes to map before calling
/// [`Table::parse`].
pub fn table_length(header: &[u8]) -> Result<usize, AcpiError> {
    if header.len() < SDT_HEADER_SIZE {
        return Err(AcpiError::Truncated);
    }
    let length = u32_at(header, 4) as usize;
    if length < SDT_HEADER_SIZE {
        return Err(AcpiError::BadLength);
    }
    Ok(length)
}

/// A validated system description table.
#[derive(Debug, Clone, Copy)]
pub struct Table<'a> {
    bytes: &'a [u8],
}

impl<'a> Table<'a> {
    /// Validates the header, length and checksum of the table at the start
    /// of `bytes`. Bytes beyond the table's own length are ignored.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, AcpiError> {
        let length = table_length(bytes)?;
        let bytes = bytes.get(..length).ok_or(AcpiError::Truncated)?;
        if !checksum_ok(bytes) {
            return Err(AcpiError::BadChecksum);
        }
        Ok(Self { bytes })
    }

    /// The four-character table signature, e.g. `b"APIC"`.
    pub fn signature(&self) -> [u8; 4] {
        [self.bytes[0], self.bytes[1], self.bytes[2], self.bytes[3]]
    }

    /// The table contents after the common header.
    pub fn body(&self) -> &'a [u8] {
        &self.bytes[SDT_HEADER_SIZE..]
    }

    /// For an RSDT or XSDT: the physical addresses of the tables it lists.
    /// Any other table yields [`AcpiError::BadSignature`].
    pub fn root_entries(&self) -> Result<impl Iterator<Item = u64> + 'a, AcpiError> {
        let width = match &self.signature() {
            b"RSDT" => 4,
            b"XSDT" => 8,
            _ => return Err(AcpiError::BadSignature),
        };
        Ok(self.body().chunks_exact(width).map(move |entry| {
            if width == 4 {
                u64::from(u32_at(entry, 0))
            } else {
                u64_at(entry, 0)
            }
        }))
    }
}

/// An entry of the MADT that the kernel understands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MadtEntry {
    /// A processor's local APIC.
    LocalApic {
        /// The local APIC ID.
        apic_id: u8,
        /// Whether the processor is usable.
        enabled: bool,
    },
    /// An I/O APIC.
    IoApic {
        /// The I/O APIC's ID.
        id: u8,
        /// Physical address of its registers.
        address: u32,
        /// First global system interrupt it handles.
        gsi_base: u32,
    },
    /// A legacy ISA interrupt that is wired differently from the default
    /// identity mapping.
    InterruptSourceOverride {
        /// The ISA IRQ number.
        irq: u8,
        /// The global system interrupt it is connected to.
        gsi: u32,
        /// MPS INTI flags: polarity in bits 0-1, trigger mode in bits 2-3.
        flags: u16,
    },
    /// 64-bit replacement for the local APIC address in the MADT header.
    LocalApicAddressOverride {
        /// Physical address of the local APIC registers.
        address: u64,
    },
}

/// The Multiple APIC Description Table.
#[derive(Debug, Clone, Copy)]
pub struct Madt<'a> {
    /// Physical address of the local APIC registers.
    pub local_apic_address: u32,
    /// Whether the machine also has legacy 8259 PICs that must be masked.
    pub has_legacy_pics: bool,
    entries: &'a [u8],
}

impl<'a> Madt<'a> {
    /// Interprets a validated table as a MADT.
    pub fn parse(table: &Table<'a>) -> Result<Self, AcpiError> {
        if &table.signature() != b"APIC" {
            return Err(AcpiError::BadSignature);
        }
        let body = table.body();
        if body.len() < 8 {
            return Err(AcpiError::Truncated);
        }
        Ok(Self {
            local_apic_address: u32_at(body, 0),
            has_legacy_pics: u32_at(body, 4) & 1 != 0,
            entries: &body[8..],
        })
    }

    /// Iterates over the entries the kernel understands. Unknown entry
    /// types are skipped; iteration stops at the first malformed entry.
    pub fn entries(&self) -> impl Iterator<Item = MadtEntry> + 'a {
        let mut rest = self.entries;
        core::iter::from_fn(move || {
            loop {
                if rest.len() < 2 {
                    return None;
                }
                let (kind, length) = (rest[0], usize::from(rest[1]));
                if length < 2 || length > rest.len() {
                    return None;
                }
                let (entry, tail) = rest.split_at(length);
                rest = tail;
                let parsed = match (kind, length) {
                    (0, 8..) => Some(MadtEntry::LocalApic {
                        apic_id: entry[3],
                        enabled: u32_at(entry, 4) & 1 != 0,
                    }),
                    (1, 12..) => Some(MadtEntry::IoApic {
                        id: entry[2],
                        address: u32_at(entry, 4),
                        gsi_base: u32_at(entry, 8),
                    }),
                    (2, 10..) => Some(MadtEntry::InterruptSourceOverride {
                        irq: entry[3],
                        gsi: u32_at(entry, 4),
                        flags: u16_at(entry, 8),
                    }),
                    (5, 12..) => Some(MadtEntry::LocalApicAddressOverride {
                        address: u64_at(entry, 4),
                    }),
                    _ => None,
                };
                if parsed.is_some() {
                    return parsed;
                }
            }
        })
    }
}

/// How a legacy ISA interrupt reaches an I/O APIC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IsaRoute {
    /// The global system interrupt the IRQ is connected to.
    pub gsi: u32,
    /// The line is asserted low (otherwise high).
    pub active_low: bool,
    /// The line is level-triggered (otherwise edge-triggered).
    pub level_triggered: bool,
}

impl Madt<'_> {
    /// Resolves ISA IRQ `irq` (0-15) to its global system interrupt and
    /// signalling, applying any interrupt source override. Without an
    /// override, ISA IRQs map to the same-numbered GSI, edge-triggered and
    /// active-high.
    pub fn isa_route(&self, irq: u8) -> IsaRoute {
        for entry in self.entries() {
            if let MadtEntry::InterruptSourceOverride {
                irq: source,
                gsi,
                flags,
            } = entry
                && source == irq
            {
                return IsaRoute {
                    gsi,
                    // 0b11 = active low; 0b00 (bus default) and 0b01 = high.
                    active_low: flags & 0b11 == 0b11,
                    // 0b11 = level; 0b00 (bus default) and 0b01 = edge.
                    level_triggered: (flags >> 2) & 0b11 == 0b11,
                };
            }
        }
        IsaRoute {
            gsi: u32::from(irq),
            active_low: false,
            level_triggered: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec;
    use std::vec::Vec;

    fn fix_checksum(bytes: &mut [u8], at: usize) {
        bytes[at] = 0;
        let sum = bytes.iter().fold(0u8, |s, &b| s.wrapping_add(b));
        bytes[at] = 0u8.wrapping_sub(sum);
    }

    fn table(signature: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut t = vec![0u8; SDT_HEADER_SIZE];
        t[..4].copy_from_slice(signature);
        t[4..8].copy_from_slice(&((SDT_HEADER_SIZE + body.len()) as u32).to_le_bytes());
        t[8] = 1; // revision
        t.extend_from_slice(body);
        fix_checksum(&mut t, 9);
        t
    }

    fn rsdp_v2(rsdt: u32, xsdt: u64) -> Vec<u8> {
        let mut r = vec![0u8; RSDP_V2_SIZE];
        r[..8].copy_from_slice(b"RSD PTR ");
        r[15] = 2;
        r[16..20].copy_from_slice(&rsdt.to_le_bytes());
        r[20..24].copy_from_slice(&(RSDP_V2_SIZE as u32).to_le_bytes());
        r[24..32].copy_from_slice(&xsdt.to_le_bytes());
        let sum = r[..20].iter().fold(0u8, |s, &b| s.wrapping_add(b));
        r[8] = 0u8.wrapping_sub(sum);
        fix_checksum(&mut r, 32);
        r
    }

    #[test]
    fn parses_rsdp_v2_and_v1() {
        let r = rsdp_v2(0x1000, 0x1_0000_2000);
        assert_eq!(
            Rsdp::parse(&r),
            Ok(Rsdp {
                revision: 2,
                rsdt_address: 0x1000,
                xsdt_address: Some(0x1_0000_2000)
            })
        );

        let mut v1 = vec![0u8; RSDP_V1_SIZE];
        v1[..8].copy_from_slice(b"RSD PTR ");
        v1[16..20].copy_from_slice(&0xABCDu32.to_le_bytes());
        fix_checksum(&mut v1, 8);
        assert_eq!(
            Rsdp::parse(&v1),
            Ok(Rsdp {
                revision: 0,
                rsdt_address: 0xABCD,
                xsdt_address: None
            })
        );
    }

    #[test]
    fn rejects_bad_rsdp() {
        let good = rsdp_v2(0x1000, 0x2000);
        assert_eq!(Rsdp::parse(&good[..10]), Err(AcpiError::Truncated));
        assert_eq!(Rsdp::parse(&good[..24]), Err(AcpiError::Truncated));

        let mut bad = good.clone();
        bad[0] = b'X';
        assert_eq!(Rsdp::parse(&bad), Err(AcpiError::BadSignature));
        let mut bad = good.clone();
        bad[16] ^= 1;
        assert_eq!(Rsdp::parse(&bad), Err(AcpiError::BadChecksum));
        let mut bad = good.clone();
        bad[30] ^= 1; // only the extended checksum covers this byte
        assert_eq!(Rsdp::parse(&bad), Err(AcpiError::BadChecksum));
        let mut bad = good.clone();
        bad[20] = 200;
        assert_eq!(Rsdp::parse(&bad), Err(AcpiError::BadLength));
    }

    #[test]
    fn validates_tables_and_lists_root_entries() {
        let xsdt = table(
            b"XSDT",
            &[0x00, 0x10, 0, 0, 1, 0, 0, 0, 0x00, 0x20, 0, 0, 0, 0, 0, 0],
        );
        let t = Table::parse(&xsdt).unwrap();
        assert_eq!(&t.signature(), b"XSDT");
        assert_eq!(
            t.root_entries().unwrap().collect::<Vec<_>>(),
            [0x1_0000_1000, 0x2000]
        );

        let rsdt = table(b"RSDT", &[0x00, 0x10, 0, 0, 0x00, 0x20, 0, 0]);
        let t = Table::parse(&rsdt).unwrap();
        assert_eq!(
            t.root_entries().unwrap().collect::<Vec<_>>(),
            [0x1000, 0x2000]
        );

        // Trailing bytes after the table are ignored.
        let mut padded = rsdt.clone();
        padded.extend_from_slice(&[0xFF; 7]);
        assert!(Table::parse(&padded).is_ok());

        assert_eq!(table_length(&rsdt).unwrap(), rsdt.len());
        assert!(matches!(
            Table::parse(&rsdt[..rsdt.len() - 1]),
            Err(AcpiError::Truncated)
        ));
        assert!(matches!(
            Table::parse(&rsdt[..10]),
            Err(AcpiError::Truncated)
        ));
        let mut bad = rsdt.clone();
        bad[40] ^= 0x80;
        assert!(matches!(Table::parse(&bad), Err(AcpiError::BadChecksum)));
        let mut bad = rsdt.clone();
        bad[4..8].copy_from_slice(&8u32.to_le_bytes());
        assert!(matches!(Table::parse(&bad), Err(AcpiError::BadLength)));

        let other = table(b"FACP", &[0; 8]);
        assert!(matches!(
            Table::parse(&other).unwrap().root_entries().map(|_| ()),
            Err(AcpiError::BadSignature)
        ));
    }

    fn sample_madt() -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&0xFEE0_0000u32.to_le_bytes());
        body.extend_from_slice(&1u32.to_le_bytes()); // PC-AT compatible
        body.extend_from_slice(&[0, 8, 0, 0, 1, 0, 0, 0]); // local APIC 0, enabled
        body.extend_from_slice(&[0, 8, 1, 1, 0, 0, 0, 0]); // local APIC 1, disabled
        body.extend_from_slice(&[1, 12, 2, 0]); // I/O APIC id 2
        body.extend_from_slice(&0xFEC0_0000u32.to_le_bytes());
        body.extend_from_slice(&0u32.to_le_bytes());
        body.extend_from_slice(&[2, 10, 0, 0, 2, 0, 0, 0, 0, 0]); // IRQ0 -> GSI2
        body.extend_from_slice(&[2, 10, 0, 9, 9, 0, 0, 0, 0x0F, 0]); // IRQ9 level/low
        body.extend_from_slice(&[4, 6, 0xFF, 0, 0, 1]); // NMI: unknown to us
        body.extend_from_slice(&[5, 12, 0, 0, 0, 0, 0xE0, 0xFE, 1, 0, 0, 0]);
        table(b"APIC", &body)
    }

    #[test]
    fn parses_madt_entries() {
        let bytes = sample_madt();
        let madt = Madt::parse(&Table::parse(&bytes).unwrap()).unwrap();
        assert_eq!(madt.local_apic_address, 0xFEE0_0000);
        assert!(madt.has_legacy_pics);
        let entries: Vec<_> = madt.entries().collect();
        assert_eq!(
            entries,
            [
                MadtEntry::LocalApic {
                    apic_id: 0,
                    enabled: true
                },
                MadtEntry::LocalApic {
                    apic_id: 1,
                    enabled: false
                },
                MadtEntry::IoApic {
                    id: 2,
                    address: 0xFEC0_0000,
                    gsi_base: 0
                },
                MadtEntry::InterruptSourceOverride {
                    irq: 0,
                    gsi: 2,
                    flags: 0
                },
                MadtEntry::InterruptSourceOverride {
                    irq: 9,
                    gsi: 9,
                    flags: 0x0F
                },
                MadtEntry::LocalApicAddressOverride {
                    address: 0x1_FEE0_0000
                },
            ]
        );
    }

    #[test]
    fn resolves_isa_routes() {
        let bytes = sample_madt();
        let madt = Madt::parse(&Table::parse(&bytes).unwrap()).unwrap();
        assert_eq!(
            madt.isa_route(0),
            IsaRoute {
                gsi: 2,
                active_low: false,
                level_triggered: false
            }
        );
        assert_eq!(
            madt.isa_route(9),
            IsaRoute {
                gsi: 9,
                active_low: true,
                level_triggered: true
            }
        );
        assert_eq!(
            madt.isa_route(4),
            IsaRoute {
                gsi: 4,
                active_low: false,
                level_triggered: false
            }
        );
    }

    #[test]
    fn madt_stops_at_malformed_entries() {
        let mut body = Vec::new();
        body.extend_from_slice(&[0; 8]);
        body.extend_from_slice(&[0, 8, 0, 7, 1, 0, 0, 0]);
        body.extend_from_slice(&[1, 0]); // zero length: malformed
        body.extend_from_slice(&[0, 8, 0, 9, 1, 0, 0, 0]);
        let bytes = table(b"APIC", &body);
        let madt = Madt::parse(&Table::parse(&bytes).unwrap()).unwrap();
        assert_eq!(madt.entries().count(), 1);

        // Entry claims more bytes than remain.
        let mut body = vec![0u8; 8];
        body.extend_from_slice(&[1, 12, 0, 0]);
        let bytes = table(b"APIC", &body);
        let madt = Madt::parse(&Table::parse(&bytes).unwrap()).unwrap();
        assert_eq!(madt.entries().count(), 0);

        let not_madt = table(b"HPET", &[0; 16]);
        assert!(matches!(
            Madt::parse(&Table::parse(&not_madt).unwrap()),
            Err(AcpiError::BadSignature)
        ));
        let short = table(b"APIC", &[0; 4]);
        assert!(matches!(
            Madt::parse(&Table::parse(&short).unwrap()),
            Err(AcpiError::Truncated)
        ));
    }

    #[test]
    fn pm_timer_from_the_fadt() {
        let fadt = |port: u32, length: u8, flags: u32, extended: Option<(u8, u64)>| {
            let mut body = vec![0u8; 244 - SDT_HEADER_SIZE];
            body[76 - SDT_HEADER_SIZE..80 - SDT_HEADER_SIZE].copy_from_slice(&port.to_le_bytes());
            body[91 - SDT_HEADER_SIZE] = length;
            body[112 - SDT_HEADER_SIZE..116 - SDT_HEADER_SIZE]
                .copy_from_slice(&flags.to_le_bytes());
            if let Some((space, address)) = extended {
                body[208 - SDT_HEADER_SIZE] = space;
                body[212 - SDT_HEADER_SIZE..220 - SDT_HEADER_SIZE]
                    .copy_from_slice(&address.to_le_bytes());
            }
            table(b"FACP", &body)
        };
        let parse = |bytes: &[u8]| PmTimer::parse(&Table::parse(bytes).unwrap());
        // What QEMU's q35 machine reports: port 0x608, 32 bits.
        assert_eq!(
            parse(&fadt(0x608, 4, 1 << 8, None)),
            Some(PmTimer {
                port: 0x608,
                wide: true
            })
        );
        assert_eq!(
            parse(&fadt(0x408, 4, 0, None)),
            Some(PmTimer {
                port: 0x408,
                wide: false
            })
        );
        // The extended address wins when set and must be an I/O port.
        assert_eq!(
            parse(&fadt(0x408, 4, 0, Some((1, 0x1808)))),
            Some(PmTimer {
                port: 0x1808,
                wide: false
            })
        );
        assert_eq!(parse(&fadt(0x408, 4, 0, Some((0, 0xFED0_0000)))), None);
        assert_eq!(parse(&fadt(0x408, 4, 0, Some((1, 0x1_0000)))), None);
        // No timer: no port, or no register length.
        assert_eq!(parse(&fadt(0, 4, 0, None)), None);
        assert_eq!(parse(&fadt(0x608, 0, 0, None)), None);
        assert_eq!(parse(&fadt(0x1_0000, 4, 0, None)), None);
        // An ACPI 1.0 table, too short for the extended fields.
        let mut short = vec![0u8; 116 - SDT_HEADER_SIZE];
        short[76 - SDT_HEADER_SIZE..80 - SDT_HEADER_SIZE].copy_from_slice(&0x608u32.to_le_bytes());
        short[91 - SDT_HEADER_SIZE] = 4;
        assert_eq!(
            parse(&table(b"FACP", &short)),
            Some(PmTimer {
                port: 0x608,
                wide: false
            })
        );
        // Truncated before the flags, and another table altogether.
        assert_eq!(parse(&table(b"FACP", &short[..60])), None);
        assert_eq!(parse(&table(b"APIC", &short)), None);
    }
}
