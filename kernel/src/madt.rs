//! Discovers the platform's real GIC (Generic Interrupt Controller) version
//! and register addresses from the ACPI MADT — ACPI's signature for this
//! table is literally `"APIC"`, its historical x86 name, not `"MADT"`.
//!
//! Exists to replace `main.rs`'s `qemu_device_region_safe` heuristic for
//! GIC/timer setup specifically with genuine discovery. `gic.rs`'s
//! original addresses (`0x0800_0000`/`0x0801_0000`) were only ever
//! confirmed via a QEMU-internal devicetree dump — a QEMU-shaped
//! convention real Parallels hardware already directly disproved (a
//! decoded Synchronous External Abort with `FAR_EL1` matching `GICD_BASE`
//! exactly — see CLAUDE.md's "take five"). MADT is the platform's own,
//! genuinely portable way of describing this, the same role SPCR plays
//! for the console (`acpi.rs`).
//!
//! Reuses `acpi::find_table` (the RSDP -> XSDT walk `discover_pl011`
//! already needed) rather than a second table-walking implementation.
//! Same two-phase split as every other discovery module: only reads plain
//! memory, so it's safe to call on either side of `exit_boot_services`,
//! but is called on the boot-services side (alongside `fb_info`/`xhci_info`
//! in `main.rs`) so the result can be logged through the still-working
//! UEFI console before anything raw-MMIO touches the addresses it finds.
//!
//! Struct field layouts (`MadtGicc`/`MadtGicd`/`MadtGicr`) and the
//! `TYPE_*` constants are cross-checked against Linux's own
//! `include/acpi/actbl2.h` (`acpi_madt_generic_interrupt`/
//! `_distributor`/`_redistributor`) rather than transcribed from memory —
//! same discipline `gic.rs`/`mmu.rs` already hold their register-bit
//! sourcing to.

use core::mem::size_of;
use core::ptr;

use crate::acpi;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GicVersion {
    V2,
    V3,
}

#[derive(Debug, Clone, Copy)]
pub struct GicInfo {
    pub version: GicVersion,
    pub gicd_base: u64,
    /// Only meaningful when `version == V2` — the GICC (CPU interface)
    /// MMIO base.
    pub gicc_base: u64,
    /// Only meaningful when `version == V3` — a Redistributor region base.
    pub gicr_base: u64,
    /// Only meaningful when `version == V3` — how much of `gicr_base` to
    /// map (from a GICR structure's own declared length when one exists;
    /// a conservative one-CPU-frame-pair default, `0x2_0000`, when this
    /// came from a GICC entry's own `gicr_base_address` field instead,
    /// which carries no length of its own — see [`discover`]'s doc
    /// comment on that fallback).
    pub gicr_size: u64,
}

#[derive(Debug, Clone, Copy)]
pub enum DiscoveryError {
    /// RSDP/XSDT-level failure finding the MADT at all — wraps
    /// `acpi::DiscoveryError` for everything before the table itself was
    /// located. Only ever consulted via this enum's derived `Debug` (in
    /// `main.rs`'s `{e:?}` diagnostic log line), which the dead-code
    /// lint doesn't count as a real read - `#[allow]`'d rather than
    /// dropping a field with genuine diagnostic value.
    #[allow(dead_code)]
    Acpi(acpi::DiscoveryError),
    /// XSDT walked fine; no table had the MADT ("APIC") signature.
    NoMadt,
    /// MADT exists, but had no GIC Distributor (Type 0x0C) structure —
    /// exactly one is required by spec.
    NoGicd,
    /// The Distributor structure's own `version` byte named something
    /// other than 2 or 3 — this driver only has GICv2/GICv3 backends.
    /// Same `Debug`-only-read situation as `Acpi` above.
    #[allow(dead_code)]
    UnsupportedGicVersion(u8),
    /// GICD said v2, but no GIC CPU Interface (Type 0x0B) structure with
    /// a usable `base_address` was found.
    NoGicc,
    /// GICD said v3, but neither a GICR (Type 0x0E) structure nor a GICC
    /// entry's own `gicr_base_address` field described a redistributor
    /// region.
    NoRedistributor,
}

const TYPE_GICC: u8 = 11;
const TYPE_GICD: u8 = 12;
const TYPE_GICR: u8 = 14;

/// Offset of `gicr_base_address` within `MadtGicc` — read directly via a
/// bounds-checked raw offset rather than the full `MadtGicc` struct,
/// because that field was only added in a later ACPI revision than the
/// structure's original definition: an older/shorter GICC entry (a real,
/// live possibility, not hypothetical — this is exactly the kind of
/// table-revision skew `find_table`'s callers have to expect) must not be
/// read past its own declared `length`.
const GICC_GICR_BASE_OFFSET: usize = 60;
/// One CPU's worth of Redistributor frames (RD_base + SGI_base, 64KB
/// each) — the conservative default `gicr_size` when a region's real
/// extent isn't known (the GICC-field fallback path, see [`GicInfo`]).
const GICR_DEFAULT_SIZE: u64 = 0x2_0000;

#[repr(C, packed)]
struct SubtableHeader {
    kind: u8,
    length: u8,
}

/// Only the fields this module actually reads; `base_address` (offset 32,
/// GICv2's CPU interface MMIO base) and `gicr_base_address` (offset 60)
/// are read directly by raw offset instead (see [`GICC_GICR_BASE_OFFSET`]
/// and this struct's doc comment above) since the structure has grown
/// across ACPI revisions and a shorter/older entry must not be read past
/// its own declared length.
const GICC_BASE_ADDRESS_OFFSET: usize = 32;

#[repr(C, packed)]
struct MadtGicd {
    header: SubtableHeader,
    _reserved: u16,
    _gic_id: u32,
    base_address: u64,
    _global_irq_base: u32,
    version: u8,
    _reserved2: [u8; 3],
}

#[repr(C, packed)]
struct MadtGicr {
    header: SubtableHeader,
    _flags: u8,
    _reserved: u8,
    base_address: u64,
    length: u32,
}

/// Parses the MADT reachable from `rsdp` (if present) and resolves the
/// real GIC version and register addresses. Safe to call either side of
/// `exit_boot_services` — only reads plain memory, no UEFI service.
///
/// Deliberately conservative in the same way every other discovery module
/// in this project is: any structure whose declared `length` doesn't
/// cover a field this code wants to read is treated as not having that
/// field, never read out-of-bounds; an unrecognized GIC version, or a
/// version whose required companion structure (GICC for v2, a
/// redistributor description for v3) is missing, is a hard error, never
/// a guess.
///
/// # Safety
/// `rsdp`, if `Some`, must point to a valid ACPI RSDP that remains mapped
/// for the lifetime of this call (true for the pointer
/// `acpi::find_rsdp` returns).
pub unsafe fn discover(rsdp: Option<*const u8>) -> Result<GicInfo, DiscoveryError> {
    let table_addr = match unsafe { acpi::find_table(rsdp, b"APIC") } {
        Ok(addr) => addr,
        Err(acpi::DiscoveryError::TableNotFound) => return Err(DiscoveryError::NoMadt),
        Err(e) => return Err(DiscoveryError::Acpi(e)),
    };

    let header = unsafe { ptr::read_unaligned(table_addr.cast::<acpi::SdtHeader>()) };
    let table_len = header.length as usize;

    // MADT body, after the standard SdtHeader: a 4-byte legacy local
    // interrupt controller address (x86-only, ignored here) + a 4-byte
    // flags field, then the variable-length interrupt controller
    // structures themselves.
    let mut offset = size_of::<acpi::SdtHeader>() + 8;

    let mut gicd: Option<(u64, u8)> = None; // (base_address, version)
    let mut gicc_base: u64 = 0; // first Type 0x0B entry's own MMIO base (v2)
    let mut gicc_gicr_base: u64 = 0; // first Type 0x0B entry's gicr_base_address, if any (v3 fallback)
    let mut gicr: Option<(u64, u64)> = None; // (base_address, length) from a Type 0x0E entry

    while offset + size_of::<SubtableHeader>() <= table_len {
        let entry_ptr = unsafe { table_addr.add(offset) };
        let sub = unsafe { ptr::read_unaligned(entry_ptr.cast::<SubtableHeader>()) };
        // A zero-length entry would loop forever; an entry claiming to
        // extend past the table's own declared length means either this
        // table or this parse is wrong - stop rather than read garbage.
        if sub.length == 0 || offset + sub.length as usize > table_len {
            break;
        }
        let entry_len = sub.length as usize;

        match sub.kind {
            TYPE_GICD if gicd.is_none() && entry_len >= size_of::<MadtGicd>() => {
                let entry = unsafe { ptr::read_unaligned(entry_ptr.cast::<MadtGicd>()) };
                gicd = Some((entry.base_address, entry.version));
            }
            TYPE_GICC => {
                if gicc_base == 0 && entry_len >= GICC_BASE_ADDRESS_OFFSET + 8 {
                    let p = unsafe { entry_ptr.add(GICC_BASE_ADDRESS_OFFSET) }.cast::<u64>();
                    gicc_base = unsafe { ptr::read_unaligned(p) };
                }
                if gicc_gicr_base == 0 && entry_len >= GICC_GICR_BASE_OFFSET + 8 {
                    let p = unsafe { entry_ptr.add(GICC_GICR_BASE_OFFSET) }.cast::<u64>();
                    gicc_gicr_base = unsafe { ptr::read_unaligned(p) };
                }
            }
            TYPE_GICR if gicr.is_none() && entry_len >= size_of::<MadtGicr>() => {
                let entry = unsafe { ptr::read_unaligned(entry_ptr.cast::<MadtGicr>()) };
                gicr = Some((entry.base_address, entry.length as u64));
            }
            _ => {}
        }

        offset += entry_len;
    }

    let (gicd_base, version_byte) = gicd.ok_or(DiscoveryError::NoGicd)?;

    match version_byte {
        2 => {
            if gicc_base == 0 {
                return Err(DiscoveryError::NoGicc);
            }
            Ok(GicInfo {
                version: GicVersion::V2,
                gicd_base,
                gicc_base,
                gicr_base: 0,
                gicr_size: 0,
            })
        }
        3 => {
            let (gicr_base, gicr_size) = if let Some((base, len)) = gicr {
                (base, len)
            } else if gicc_gicr_base != 0 {
                (gicc_gicr_base, GICR_DEFAULT_SIZE)
            } else {
                return Err(DiscoveryError::NoRedistributor);
            };
            Ok(GicInfo {
                version: GicVersion::V3,
                gicd_base,
                gicc_base: 0,
                gicr_base,
                gicr_size,
            })
        }
        other => Err(DiscoveryError::UnsupportedGicVersion(other)),
    }
}
