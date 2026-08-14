//! Discovers a console UART by enumerating PCI devices for a serial
//! controller — tried after both devicetree and ACPI/SPCR fail (see
//! `devicetree.rs`, `acpi.rs`). Confirmed on Parallels: it has an ACPI RSDP
//! and XSDT (both parse fine) but no SPCR table entry at all, even after
//! adding a serial port device to the VM's hardware. That means its console
//! isn't described via SPCR — this checks whether it's exposed as a PCI
//! device instead.
//!
//! Entirely boot-services-based (`PciRootBridgeIo`), unlike `devicetree.rs`/
//! `acpi.rs`'s find-pointer-then-parse-memory split — there's no
//! post-`exit_boot_services` half here, the whole discovery must run before.
//!
//! PCI class 0x07 subclass 0x00 ("Serial controller") is specifically the
//! 8250/16450/16550 family, per the PCI Code and ID Assignment spec — never
//! how a PL011 would be identified over PCI. A match here means the console
//! is a completely different device to the ones `devicetree.rs`/`acpi.rs`
//! look for, hence returning [`uart16550::Uart16550`]'s base address, not a
//! PL011 one.

use uefi::proto::pci::root_bridge::PciRootBridgeIo;

const CLASS_REGISTER: u8 = 2 * 4;
const BAR0_REGISTER: u8 = 4 * 4;

const PCI_CLASS_SIMPLE_COMMUNICATION_CONTROLLER: u8 = 0x07;
const PCI_SUBCLASS_SERIAL_CONTROLLER: u8 = 0x00;

#[derive(Debug, Clone, Copy)]
pub enum DiscoveryError {
    /// No handle on the system supports `PciRootBridgeIo` at all.
    NoRootBridge,
    /// Walked every device on every root bridge; none was a class 0x07
    /// subclass 0x00 serial controller.
    NoSerialDevice,
    /// Found a serial controller, but its BAR0 is I/O space, not memory
    /// space — this driver only speaks memory-mapped MMIO.
    UnsupportedAddressSpace,
    /// Found a serial controller with a memory BAR, but its type bits
    /// don't match either 32-bit or 64-bit memory (reserved/unknown).
    UnsupportedBarType,
}

/// Enumerates every PCI root bridge's devices looking for a class 0x07
/// subclass 0x00 serial controller, returning its BAR0 address. Must be
/// called before `exit_boot_services` — entirely boot-services-based, no
/// part of this can run after.
pub fn discover_uart16550() -> Result<usize, DiscoveryError> {
    let handles =
        uefi::boot::find_handles::<PciRootBridgeIo>().map_err(|_| DiscoveryError::NoRootBridge)?;

    for handle in handles {
        let Ok(mut root_bridge) = uefi::boot::open_protocol_exclusive::<PciRootBridgeIo>(handle)
        else {
            continue;
        };
        let Ok(tree) = root_bridge.enumerate() else {
            continue;
        };

        for addr in tree.iter() {
            let Ok(class_reg) = root_bridge
                .pci()
                .read_one::<u32>(addr.with_register(CLASS_REGISTER))
            else {
                continue;
            };
            let class = (class_reg >> 24) as u8;
            let subclass = (class_reg >> 16) as u8;
            if class != PCI_CLASS_SIMPLE_COMMUNICATION_CONTROLLER
                || subclass != PCI_SUBCLASS_SERIAL_CONTROLLER
            {
                continue;
            }

            return read_bar0_address(&mut root_bridge, *addr);
        }
    }

    Err(DiscoveryError::NoSerialDevice)
}

fn read_bar0_address(
    root_bridge: &mut PciRootBridgeIo,
    addr: uefi::proto::pci::PciIoAddress,
) -> Result<usize, DiscoveryError> {
    let bar0 = root_bridge
        .pci()
        .read_one::<u32>(addr.with_register(BAR0_REGISTER))
        .map_err(|_| DiscoveryError::UnsupportedAddressSpace)?;

    if bar0 & 0x1 != 0 {
        // Bit 0 set: I/O space BAR, not memory space.
        return Err(DiscoveryError::UnsupportedAddressSpace);
    }

    let base_low = (bar0 & !0xF) as u64;
    match (bar0 >> 1) & 0x3 {
        0b00 => Ok(base_low as usize),
        0b10 => {
            let bar1 = root_bridge
                .pci()
                .read_one::<u32>(addr.with_register(BAR0_REGISTER + 4))
                .map_err(|_| DiscoveryError::UnsupportedBarType)?;
            Ok((base_low | ((bar1 as u64) << 32)) as usize)
        }
        _ => Err(DiscoveryError::UnsupportedBarType),
    }
}
