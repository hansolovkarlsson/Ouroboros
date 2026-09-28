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
    /// `find_table` walked every XSDT entry; none matched the requested
    /// signature. `discover_pl011` maps this back to `NoSpcr` at its own
    /// call site so its public error shape doesn't change.
    TableNotFound,
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
pub(crate) struct SdtHeader {
    pub(crate) signature: [u8; 4],
    pub(crate) length: u32,
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

/// Walks RSDP -> XSDT looking for a table whose header signature matches
/// `signature` (e.g. `b"SPCR"`, `b"APIC"` — MADT's signature is `"APIC"`,
/// its historical x86 name, not `"MADT"`). Returns a pointer to that
/// table's own header (callers cast/parse from there). Shared by
/// `discover_pl011` (SPCR) and `madt::discover` (MADT) — pulled out once a
/// second real caller needed the identical walk, not speculatively.
///
/// Safe to call either side of `exit_boot_services` — only reads plain
/// memory, no UEFI service. Does not verify any ACPI checksum; trusts the
/// firmware-provided pointer the same way devicetree discovery trusts the
/// DTB pointer.
///
/// # Safety
/// `rsdp`, if `Some`, must point to a valid ACPI RSDP that remains mapped
/// for the lifetime of this call (true for the pointer `find_rsdp` returns).
pub unsafe fn find_table(
    rsdp: Option<*const u8>,
    signature: &[u8; 4],
) -> Result<*const u8, DiscoveryError> {
    for table_addr in unsafe { xsdt_entries(rsdp) }? {
        let header = unsafe { ptr::read_unaligned(table_addr.cast::<SdtHeader>()) };
        if &header.signature == signature {
            return Ok(table_addr);
        }
    }
    Err(DiscoveryError::TableNotFound)
}

/// Validates the RSDP and XSDT reachable from `rsdp` and returns every
/// table pointer the XSDT lists, in order - the one walk `find_table`
/// (first match) and `dma_noncoherent` (every DSDT/SSDT) share, so the
/// validation cannot drift between them.
///
/// # Safety
/// Same as [`find_table`].
unsafe fn xsdt_entries(rsdp: Option<*const u8>) -> Result<impl Iterator<Item = *const u8>, DiscoveryError> {
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

    let entry_count = (xsdt_header.length as usize).saturating_sub(size_of::<SdtHeader>()) / size_of::<u64>();
    let entries_ptr = unsafe { xsdt_ptr.add(size_of::<SdtHeader>()) }.cast::<u64>();
    // SAFETY: the caller's contract - the XSDT and its entries are mapped.
    Ok((0..entry_count).map(move |i| unsafe { ptr::read_unaligned(entries_ptr.add(i)) } as *const u8))
}

/// Where a `Name(_CCA, Zero)` was found: the table's signature and the
/// byte offset of the `NameOp` inside it, so the boot log names the match
/// and a wrong one can be looked up in a disassembly of that table.
#[derive(Debug, Clone, Copy)]
pub struct CcaZero {
    pub table: [u8; 4],
    pub offset: usize,
}

/// Whether the firmware declares any device's DMA NOT cache-coherent:
/// `Ok(Some(where))` if its DSDT or any SSDT contains `Name(_CCA, Zero)`.
///
/// `_CCA` ("cache coherency attribute") is ACPI's own statement of
/// whether a device's DMA is coherent with the CPU caches; 0 means it is
/// not, and the OS must keep that device's DMA memory out of the caches.
/// The Raspberry Pi 4/400's pftf firmware says it of the PCIe root, which
/// the VL805 xHCI sits behind (`Name(_CCA, 0) // Mark the PCI noncoherent`,
/// edk2-platforms `Platform/RaspberryPi/AcpiTables/Pci.asl`); QEMU's `virt`
/// says `_CCA 1`. This kernel parses no AML, so the object is found by its
/// fixed encoding - NameOp, the name `_CCA`, and a zero, as `ZeroOp`
/// (`08 5F 43 43 41 00`) or `ByteConst 0` (`08 5F 43 43 41 0A 00`) - a
/// byte scan, not an interpretation: it answers "does any device say
/// non-coherent", not which one. Every platform this kernel meets has at
/// most the one PCIe root doing DMA, so "any" is the question.
///
/// **What it cannot see, stated because the fallback is "coherent":** a
/// `_CCA` returned by a Method (`Method(_CCA) { Return(Zero) }`), one
/// declared under a path (`Name(\_SB.PCI0._CCA, Zero)`, where the name
/// string carries a prefix before `_CCA`), or any other encoding than the
/// two above. The pftf firmware uses the plain `Name` form this finds;
/// firmware that did not would leave the DMA pool cacheable on a
/// non-coherent machine, and the boot log's `ACPI declares no non-coherent
/// DMA` line is where that shows (`docs/testing/testing-pi4.md` Risk 8).
///
/// Must be called while the tables are mapped (before or after
/// `exit_boot_services` - they are plain memory).
///
/// # Safety
/// Same as [`find_table`].
pub unsafe fn dma_noncoherent(rsdp: Option<*const u8>) -> Result<Option<CcaZero>, DiscoveryError> {
    for table in unsafe { xsdt_entries(rsdp) }? {
        let header = unsafe { ptr::read_unaligned(table.cast::<SdtHeader>()) };
        let aml = if &header.signature == b"SSDT" {
            table
        } else if &header.signature == b"FACP" {
            // The DSDT hangs off the FADT: X_DSDT (64-bit, offset 140)
            // when the table is long enough and it is set, else DSDT
            // (32-bit, offset 40).
            let x_dsdt = if header.length >= 148 {
                unsafe { ptr::read_unaligned(table.add(140).cast::<u64>()) }
            } else {
                0
            };
            let dsdt = unsafe { ptr::read_unaligned(table.add(40).cast::<u32>()) } as u64;
            let addr = if x_dsdt != 0 { x_dsdt } else { dsdt };
            if addr == 0 {
                continue;
            }
            addr as *const u8
        } else {
            continue;
        };
        if let Some(offset) = unsafe { aml_cca_zero_offset(aml) } {
            let table = unsafe { ptr::read_unaligned(aml.cast::<SdtHeader>()) }.signature;
            return Ok(Some(CcaZero { table, offset }));
        }
    }
    Ok(None)
}

/// The byte offset, within the AML table at `table` (a DSDT or SSDT,
/// header included), of the first `Name(_CCA, Zero)` in either zero
/// encoding, if any.
unsafe fn aml_cca_zero_offset(table: *const u8) -> Option<usize> {
    let header = unsafe { ptr::read_unaligned(table.cast::<SdtHeader>()) };
    let len = header.length as usize;
    if len <= size_of::<SdtHeader>() {
        return None;
    }
    let bytes = unsafe { core::slice::from_raw_parts(table, len) };
    const NAME_CCA: [u8; 5] = [0x08, b'_', b'C', b'C', b'A'];
    bytes
        .windows(7)
        .position(|w| w[..5] == NAME_CCA && (w[5] == 0x00 || (w[5] == 0x0a && w[6] == 0x00)))
        .or_else(|| (bytes[len - 6..].starts_with(&NAME_CCA) && bytes[len - 1] == 0x00).then_some(len - 6))
}

/// Parses the ACPI tables reachable from `rsdp` (if present) and resolves
/// the SPCR table to a PL011-compatible UART base address. Safe to call
/// either side of `exit_boot_services` — only reads plain memory, no UEFI
/// service.
///
/// # Safety
/// Same as [`find_table`].
pub unsafe fn discover_pl011(rsdp: Option<*const u8>) -> Result<usize, DiscoveryError> {
    let table_addr = match unsafe { find_table(rsdp, b"SPCR") } {
        Ok(addr) => addr,
        Err(DiscoveryError::TableNotFound) => return Err(DiscoveryError::NoSpcr),
        Err(e) => return Err(e),
    };

    let spcr = unsafe { ptr::read_unaligned(table_addr.cast::<Spcr>()) };
    if spcr.base_address.address_space_id != ACPI_ADDRESS_SPACE_SYSTEM_MEMORY {
        return Err(DiscoveryError::UnsupportedAddressSpace);
    }
    match spcr.interface_type {
        INTERFACE_TYPE_ARM_PL011
        | INTERFACE_TYPE_ARM_SBSA_GENERIC_UART
        | INTERFACE_TYPE_ARM_SBSA_GENERIC_UART_2X => Ok(spcr.base_address.address as usize),
        _ => Err(DiscoveryError::UnsupportedInterface),
    }
}
