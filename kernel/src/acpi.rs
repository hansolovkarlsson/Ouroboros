//! Discovers the platform's console UART from the ACPI SPCR (Serial Port
//! Console Redirection) table — the actual mechanism, unlike devicetree
//! (`devicetree.rs`), since both QEMU's and Parallels' firmware are
//! confirmed ACPI-oriented and neither publishes a devicetree.
//!
//! Hand-rolled rather than pulling in the `acpi` crate: this only needs a
//! handful of fixed-offset struct reads (RSDP -> XSDT -> one matching
//! table), nothing like devicetree's variable-length tree format that
//! justified pulling in `fdt`.
//!
//! Same two-phase split as devicetree discovery, for the same reason:
//! `find_rsdp` needs the UEFI configuration table, so it must run before
//! `exit_boot_services`; `discover_pl011` only reads plain memory, so it
//! runs on the same side (before exit) so its result can be logged through
//! the UEFI console before any raw MMIO is touched.

use core::mem::size_of;
use core::ptr;

use uefi::table::cfg::ConfigTableEntry;

/// Finds the ACPI RSDP pointer via the UEFI configuration table, preferring
/// ACPI 2.0+ (whose RSDP carries the 64-bit XSDT pointer this module reads)
/// over the ACPI 1.0 entry. Must be called before `exit_boot_services`.
pub fn find_rsdp() -> Option<*const u8> {
    uefi::system::with_config_table(|entries| {
        find_guid(entries, ConfigTableEntry::ACPI2_GUID)
            .or_else(|| find_guid(entries, ConfigTableEntry::ACPI_GUID))
    })
}

fn find_guid(entries: &[ConfigTableEntry], guid: uefi::Guid) -> Option<*const u8> {
    entries
        .iter()
        .find(|entry| entry.guid == guid)
        .map(|entry| entry.address.cast::<u8>())
}

#[derive(Debug, Clone, Copy)]
pub enum DiscoveryError {
    /// No ACPI RSDP entry in the UEFI configuration table.
    NoRsdp,
    /// Found a pointer, but it doesn't start with the RSDP signature.
    BadRsdpSignature,
    /// RSDP is ACPI 1.0 (no XSDT) — not supported, only 32-bit RSDT
    /// pointers, unexpected on any aarch64 platform this targets.
    NoXsdt,
    /// XSDT pointer doesn't start with the XSDT signature.
    BadXsdtSignature,
    /// Walked every XSDT entry; none had the SPCR signature.
    NoSpcr,
    /// SPCR exists, but its console isn't in system memory (e.g. I/O port
    /// or PCI config space) — this driver only speaks memory-mapped PL011.
    UnsupportedAddressSpace,
    /// SPCR exists and is memory-mapped, but its interface type isn't one
    /// this driver's PL011 register layout understands.
    UnsupportedInterface,
}

#[repr(C, packed)]
struct Rsdp {
    signature: [u8; 8],
    checksum: u8,
    oem_id: [u8; 6],
    revision: u8,
    rsdt_address: u32,
    length: u32,
    xsdt_address: u64,
    extended_checksum: u8,
    reserved: [u8; 3],
}

#[repr(C, packed)]
struct SdtHeader {
    signature: [u8; 4],
    length: u32,
    revision: u8,
    checksum: u8,
    oem_id: [u8; 6],
    oem_table_id: [u8; 8],
    oem_revision: u32,
    creator_id: u32,
    creator_revision: u32,
}

#[repr(C, packed)]
struct GenericAddress {
    address_space_id: u8,
    register_bit_width: u8,
    register_bit_offset: u8,
    access_size: u8,
    address: u64,
}

/// Only the fields discovery actually needs; SPCR has more (baud rate,
/// interrupt info, PCI location) that this doesn't read.
#[repr(C, packed)]
struct Spcr {
    header: SdtHeader,
    interface_type: u8,
    _reserved: [u8; 3],
    base_address: GenericAddress,
}

const ACPI_ADDRESS_SPACE_SYSTEM_MEMORY: u8 = 0;

/// ACPI 6.x "Interface Type" values compatible with this driver's PL011
/// register layout: full ARM PL011, and the ARM SBSA Generic UART (a
/// register-compatible PL011 subset used by server-class platforms).
const INTERFACE_TYPE_ARM_PL011: u8 = 0x03;
const INTERFACE_TYPE_ARM_SBSA_GENERIC_UART: u8 = 0x0e;
const INTERFACE_TYPE_ARM_SBSA_GENERIC_UART_2X: u8 = 0x0a;

/// Parses the ACPI tables reachable from `rsdp` (if present) and resolves
/// the SPCR table to a PL011-compatible UART base address. Safe to call
/// either side of `exit_boot_services` — only reads plain memory, no UEFI
/// service. Does not verify any ACPI checksum; trusts the firmware-provided
/// pointer the same way devicetree discovery trusts the DTB pointer.
///
/// # Safety
/// `rsdp`, if `Some`, must point to a valid ACPI RSDP that remains mapped
/// for the lifetime of this call (true for the pointer `find_rsdp` returns).
pub unsafe fn discover_pl011(rsdp: Option<*const u8>) -> Result<usize, DiscoveryError> {
    let rsdp_ptr = rsdp.ok_or(DiscoveryError::NoRsdp)?;
    let rsdp = unsafe { ptr::read_unaligned(rsdp_ptr.cast::<Rsdp>()) };
    if &rsdp.signature != b"RSD PTR " {
        return Err(DiscoveryError::BadRsdpSignature);
    }
    if rsdp.revision < 2 || rsdp.xsdt_address == 0 {
        return Err(DiscoveryError::NoXsdt);
    }

    let xsdt_ptr = rsdp.xsdt_address as *const u8;
    let xsdt_header = unsafe { ptr::read_unaligned(xsdt_ptr.cast::<SdtHeader>()) };
    if &xsdt_header.signature != b"XSDT" {
        return Err(DiscoveryError::BadXsdtSignature);
    }

    let entry_count = (xsdt_header.length as usize - size_of::<SdtHeader>()) / size_of::<u64>();
    let entries_ptr = unsafe { xsdt_ptr.add(size_of::<SdtHeader>()) }.cast::<u64>();

    for i in 0..entry_count {
        let table_addr = unsafe { ptr::read_unaligned(entries_ptr.add(i)) } as *const u8;
        let header = unsafe { ptr::read_unaligned(table_addr.cast::<SdtHeader>()) };
        if &header.signature != b"SPCR" {
            continue;
        }

        let spcr = unsafe { ptr::read_unaligned(table_addr.cast::<Spcr>()) };
        if spcr.base_address.address_space_id != ACPI_ADDRESS_SPACE_SYSTEM_MEMORY {
            return Err(DiscoveryError::UnsupportedAddressSpace);
        }
        return match spcr.interface_type {
            INTERFACE_TYPE_ARM_PL011
            | INTERFACE_TYPE_ARM_SBSA_GENERIC_UART
            | INTERFACE_TYPE_ARM_SBSA_GENERIC_UART_2X => Ok(spcr.base_address.address as usize),
            _ => Err(DiscoveryError::UnsupportedInterface),
        };
    }

    Err(DiscoveryError::NoSpcr)
}
