//! Discovers the platform's console UART from the devicetree UEFI hands
//! off, instead of assuming QEMU's fixed PL011 address. This is what's
//! meant to make console output portable to Parallels (or any other
//! UEFI+devicetree platform) instead of only working on QEMU's `virt`
//! machine.
//!
//! `find_dtb` reads the UEFI configuration table for the devicetree blob
//! pointer, so it must run before `exit_boot_services` (it's UEFI table
//! data, gone once boot services are torn down). `discover_pl011` parses
//! that blob (via the `fdt` crate) and doesn't touch any firmware service,
//! so it works either side of `exit_boot_services` — `main.rs` deliberately
//! calls it *before*, so the result can be logged through the UEFI console
//! (safe on any platform) rather than only via the raw MMIO UART this module
//! resolves an address for, which isn't validated until it's actually
//! written to.

use fdt::Fdt;
use uefi::{guid, Guid};

/// `EFI_DTB_TABLE_GUID`, per the UEFI spec — the devicetree blob, when
/// firmware provides one, is published under this configuration table GUID.
const DTB_GUID: Guid = guid!("b1b621d5-f19c-41a5-830b-d9152c69aae0");

/// Finds the devicetree blob pointer via the UEFI configuration table.
/// Must be called before `exit_boot_services`.
pub fn find_dtb() -> Option<*const u8> {
    uefi::system::with_config_table(|entries| {
        entries
            .iter()
            .find(|entry| entry.guid == DTB_GUID)
            .map(|entry| entry.address.cast::<u8>())
    })
}

/// Why [`discover_pl011`] fell back to a hardcoded address, kept distinct
/// per failure point so a platform that publishes no devicetree at all
/// (e.g. an ACPI-only firmware) can be told apart from one that publishes a
/// devicetree whose console this driver just doesn't understand yet.
#[derive(Debug, Clone, Copy)]
pub enum DiscoveryError {
    /// No `EFI_DTB_TABLE_GUID` entry in the UEFI configuration table.
    NoDtb,
    /// A devicetree pointer was found, but `fdt` couldn't parse it.
    MalformedDtb,
    /// Parsed fine, but neither `/chosen/stdout` nor a `compatible =
    /// "arm,pl011"` node exists.
    NoConsoleNode,
    /// A console node exists but isn't PL011-compatible — this driver
    /// doesn't know its register layout.
    UnsupportedConsole,
    /// A PL011 node exists but has no usable `reg` property.
    NoRegProperty,
}

/// Parses `dtb` (if present) and resolves it to a PL011 UART base address.
/// Only understands PL011: if the platform's console is some other UART
/// type, this returns `Err` rather than guess at a register layout our
/// driver doesn't implement. Safe to call either side of
/// `exit_boot_services` — this only touches the blob itself, not any UEFI
/// service.
///
/// # Safety
/// `dtb`, if `Some`, must point to a valid devicetree blob that remains
/// mapped for the lifetime of this call (true for the pointer `find_dtb`
/// returns: firmware places the DTB in memory that ExitBootServices does
/// not reclaim).
pub unsafe fn discover_pl011(dtb: Option<*const u8>) -> Result<usize, DiscoveryError> {
    let dtb = dtb.ok_or(DiscoveryError::NoDtb)?;
    let fdt = unsafe { Fdt::from_ptr(dtb) }.map_err(|_| DiscoveryError::MalformedDtb)?;

    let node = fdt
        .chosen()
        .stdout()
        .or_else(|| fdt.find_compatible(&["arm,pl011"]))
        .ok_or(DiscoveryError::NoConsoleNode)?;

    let is_pl011 = node
        .compatible()
        .is_some_and(|c| c.all().any(|s| s == "arm,pl011"));
    if !is_pl011 {
        return Err(DiscoveryError::UnsupportedConsole);
    }

    let region = node
        .reg()
        .and_then(|mut regs| regs.next())
        .ok_or(DiscoveryError::NoRegProperty)?;
    Ok(region.starting_address as usize)
}
