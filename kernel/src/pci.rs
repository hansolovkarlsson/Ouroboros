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
//! look for, hence returning [`crate::uart16550::Uart16550`]'s base address, not a
//! PL011 one.

use uefi::proto::pci::root_bridge::PciRootBridgeIo;

const CLASS_REGISTER: u8 = 2 * 4;
const COMMAND_REGISTER: u8 = 4; // dword 1
const BAR0_REGISTER: u8 = 4 * 4;

const PCI_CLASS_SIMPLE_COMMUNICATION_CONTROLLER: u8 = 0x07;
const PCI_SUBCLASS_SERIAL_CONTROLLER: u8 = 0x00;

const PCI_CLASS_SERIAL_BUS_CONTROLLER: u8 = 0x0c;
const PCI_SUBCLASS_USB: u8 = 0x03;
const PCI_PROG_IF_XHCI: u8 = 0x30;

// PCI Command register bits (offset 0x04, low 16 bits of COMMAND_REGISTER).
// A real, confirmed bug lived here once: this was `1 << 0`, which is
// actually I/O Space Enable, not Memory Space Enable (bit 1) - PCI
// Command register bit numbering, confirmed against the PCI Local Bus
// spec after real Parallels hardware testing showed the observed
// before/after values (0x0010 -> 0x0015) only ever set bits 0 and 2, and
// a real xHCI controller has no I/O-space BAR to enable at all. This is
// why every prior test - on QEMU *and* Parallels - kept reading
// 0xffffffff / taking an External Abort no matter what else changed
// (write width, unconditional vs conditional, BAR reassignment): Memory
// Space was never actually being enabled by any of those attempts.
const CMD_MEMORY_SPACE: u16 = 1 << 1;
const CMD_BUS_MASTER: u16 = 1 << 2;


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

/// `discover_xhci`'s result - the BAR address plus enough diagnostic state
/// for `main.rs` to re-print through the post-exit console, since this
/// module's own `log::info!` calls are lost once `fbconsole.rs` clears the
/// boot-services text console (see `discover_xhci`'s doc comment).
#[derive(Debug, Clone, Copy)]
pub struct XhciInfo {
    pub base: u64,
    /// PCI Command register's low 16 bits as first observed.
    pub command_before: u16,
    /// Same register re-read after this function's enable attempt (a
    /// no-op read if it was already enabled) - compare against
    /// `command_before` to see whether the write actually took effect.
    pub command_after: u16,
}

#[derive(Debug, Clone, Copy)]
pub enum XhciDiscoveryError {
    /// No handle on the system supports `PciRootBridgeIo` at all.
    NoRootBridge,
    /// Walked every device on every root bridge; none was a class 0x0c
    /// subclass 0x03 prog-if 0x30 xHCI controller.
    NotFound,
    /// Found an xHCI controller, but its BAR0 reads back as `0` - firmware
    /// never assigned it a real address. This driver no longer tries to
    /// fix that itself (see `discover_xhci`'s doc comment for why a write-
    /// based fix crashed real Parallels hardware) - a keyboard-less boot
    /// is the only safe outcome here now.
    Unassigned,
    /// Found an xHCI controller, but its BAR0 is I/O space, not memory
    /// space - real xHCI hardware always uses a memory BAR (the spec
    /// requires it), so this would mean a genuinely unexpected device.
    UnsupportedAddressSpace,
    /// Found an xHCI controller with a memory BAR, but its type bits
    /// don't match either 32-bit or 64-bit memory (reserved/unknown).
    UnsupportedBarType,
}

impl core::fmt::Display for XhciDiscoveryError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            XhciDiscoveryError::NoRootBridge => write!(f, "no PCI root bridge found"),
            XhciDiscoveryError::NotFound => write!(f, "no xHCI controller found on any PCI root bridge"),
            XhciDiscoveryError::Unassigned => write!(f, "xHCI controller's BAR0 was never assigned an address by firmware"),
            XhciDiscoveryError::UnsupportedAddressSpace => write!(f, "xHCI controller's BAR0 is I/O space, not memory space"),
            XhciDiscoveryError::UnsupportedBarType => write!(f, "xHCI controller's BAR0 has an unsupported type"),
        }
    }
}

/// Enumerates every PCI root bridge's devices looking for a class 0x0c
/// subclass 0x03 prog-if 0x30 xHCI (USB3) host controller, returning its
/// BAR0 address (always a 64-bit memory BAR on real xHCI hardware, per the
/// spec - `read_bar0_address` still handles the 32-bit case for
/// completeness).
///
/// Unlike `virtio_mmio.rs`'s address (a fixed, QEMU-shaped convention
/// confirmed unsafe on real Parallels hardware - see that module's doc
/// comment), this address is genuinely *discovered*: read directly out of
/// the device's own PCI configuration space via the same
/// `PciRootBridgeIo` mechanism `discover_uart16550`/`log_all_devices`
/// already use safely on real Parallels hardware, not guessed from a
/// QEMU-specific memory layout.
///
/// **Two real, confirmed hardware findings shaped this function's current
/// shape - not style choices.**
///
/// An earlier version *wrote* to PCI config space unconditionally - a
/// BAR-reassignment probe (write all-1s, read back the size mask,
/// write a chosen address) for the case where firmware left BAR0
/// unassigned (genuinely necessary on this project's own QEMU dev loop -
/// `edk2-stable202408-prebuilt.qemu.org` doesn't allocate BAR resources or
/// enable a device's Command register unless some UEFI driver binds to
/// it, and nothing ever binds to an unused xHCI controller when the
/// kernel loads over virtio-mmio instead), plus an unconditional
/// Command-register *word* (u16) write to enable Memory Space + Bus
/// Master. **Tested on real Parallels hardware, and it crashed the whole
/// VM**, not just this kernel: Parallels' own hypervisor log
/// (`libMonitorArm.dylib`) recorded `mon.abort.message = PANIC@11.28
/// UEFI-exception-ArmPciCpuIo2Dxe.dll` - a fault *inside firmware's own*
/// PCI config-space-I/O driver, before this kernel ever gets control, let
/// alone a chance to report anything through its own exception handler
/// (which only exists post-`exit_boot_services` - see `exceptions.rs`).
///
/// Once every write was removed and this function reduced to pure reads
/// (matching `discover_uart16550`/`log_all_devices`'s
/// long-safe discipline): the *next* real Parallels boot got past
/// firmware cleanly and into this kernel's own post-exit code, but
/// `xhci.rs::init_inner`'s very first register read then took a genuine
/// Synchronous External Abort - `ESR_EL1` decoding to EC 0x25 (Data
/// Abort) with DFSC 0x10 (Synchronous External abort, the same real-bus-
/// fault signature `virtio_mmio.rs`/`gic.rs` hit earlier this project),
/// `FAR_EL1` matching the BAR address exactly. Firmware genuinely had
/// assigned a real BAR this time (`0x10007000` - a real, low, sane
/// address unlike QEMU's quirk) - the read still faulted because Memory
/// Space was never enabled, and real Parallels hardware, unlike QEMU's
/// lenient TCG model, raises a genuine bus abort for a transaction to a
/// disabled BAR rather than silently returning `0xffffffff`. So the
/// Command-register enable *is* necessary after all - what actually
/// crashed firmware the first time was something about the *word-width*
/// write specifically, not the general idea of writing it. This version
/// writes the *dword* (u32) containing Command+Status instead (PCI config
/// space's natural, always-supported access granularity), and only when
/// the desired bits aren't already set - both a real behavior change
/// aimed at the suspected width-support gap and a way to keep this write
/// as rare as possible whether or not that guess is exactly right.
///
/// If a BAR reads back as genuinely unassigned (`0`), that's reported as
/// [`XhciDiscoveryError::Unassigned`] - no write-based recovery is
/// attempted for that case on any platform, per the first finding above.
///
/// Must be called before `exit_boot_services` - entirely boot-services-based.
///
/// Returns diagnostic info alongside the base address (not just the
/// address alone) specifically so `main.rs` can re-print it through the
/// *post-exit* console: this function's own `log::info!` calls only ever
/// reach the boot-services text console, which gets overwritten the
/// moment `fbconsole.rs` clears the screen for its own use - on a
/// platform with no other console (Parallels' real, confirmed shape),
/// that diagnostic output is otherwise unrecoverable the instant a crash
/// happens later in the boot, which is exactly what made the two
/// Command-register findings documented above so slow to pin down.
pub fn discover_xhci() -> Result<XhciInfo, XhciDiscoveryError> {
    let handles =
        uefi::boot::find_handles::<PciRootBridgeIo>().map_err(|_| XhciDiscoveryError::NoRootBridge)?;

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
            let prog_if = (class_reg >> 8) as u8;
            if class != PCI_CLASS_SERIAL_BUS_CONTROLLER
                || subclass != PCI_SUBCLASS_USB
                || prog_if != PCI_PROG_IF_XHCI
            {
                continue;
            }

            // Enable Memory Space + Bus Master if not already set - see
            // this function's doc comment for why this write exists at
            // all (real Parallels hardware directly confirmed a
            // Synchronous External Abort reading an otherwise-correctly-
            // assigned BAR with Memory Space still disabled - not a
            // hypothetical), why it's now a dword (u32) write rather than
            // the word (u16) write that crashed firmware outright, and
            // why it's conditional (skip the write entirely if the bits
            // already read as set, minimizing how often this even runs).
            let command_before = root_bridge
                .pci()
                .read_one::<u32>(addr.with_register(COMMAND_REGISTER))
                .map(|v| (v & 0xffff) as u16)
                .unwrap_or(0xffff); // sentinel distinct from any real 16-bit command value's low byte pattern - read itself failed

            let mut command_after = command_before;
            if command_before & (CMD_MEMORY_SPACE | CMD_BUS_MASTER) != (CMD_MEMORY_SPACE | CMD_BUS_MASTER) {
                if let Ok(command_status) = root_bridge
                    .pci()
                    .read_one::<u32>(addr.with_register(COMMAND_REGISTER))
                {
                    let new_command_status = command_status | (CMD_MEMORY_SPACE | CMD_BUS_MASTER) as u32;
                    let _ = root_bridge
                        .pci()
                        .write_one::<u32>(addr.with_register(COMMAND_REGISTER), new_command_status);
                }
                command_after = root_bridge
                    .pci()
                    .read_one::<u32>(addr.with_register(COMMAND_REGISTER))
                    .map(|v| (v & 0xffff) as u16)
                    .unwrap_or(0xffff);
                log::info!(
                    "Ouroboros kernel: xhci: PCI command register was {command_before:#06x}, wrote+read back {command_after:#06x}"
                );
            } else {
                log::info!("Ouroboros kernel: xhci: PCI command register already {command_before:#06x}, no write needed");
            }

            let base = read_bar0_address(&mut root_bridge, *addr)
                .map(|base| base as u64)
                .map_err(|e| match e {
                    DiscoveryError::UnsupportedAddressSpace => XhciDiscoveryError::UnsupportedAddressSpace,
                    _ => XhciDiscoveryError::UnsupportedBarType,
                })?;
            if base == 0 {
                return Err(XhciDiscoveryError::Unassigned);
            }
            return Ok(XhciInfo { base, command_before, command_after });
        }
    }

    Err(XhciDiscoveryError::NotFound)
}

/// Diagnostic only, not used for discovery: logs every PCI device's
/// vendor:device and class:subclass, for cases where none of the three
/// normal console-discovery mechanisms found anything and it's not
/// obvious why. Added specifically to answer a real open question on
/// Parallels: does it expose its console (or anything) as a virtio-pci
/// device (vendor `0x1af4`) at all, when `virtio_mmio.rs`'s address-range
/// scan also comes up empty post-exit? A successful walk that finds
/// nothing (as opposed to [`DiscoveryError::NoRootBridge`]) already
/// proves PCI enumeration itself works on this platform - see
/// `discover_uart16550`, which already reaches `NoSerialDevice` there,
/// not `NoRootBridge`.
///
/// Must be called before `exit_boot_services`, same as
/// `discover_uart16550` - entirely boot-services-based.
///
/// Also *returns* what it logged, because the `log::info!` lines alone
/// turned out to be unreadable on the one platform this diagnostic
/// exists for: on real Parallels hardware the framebuffer console
/// clears the screen the moment it installs, boot reaches the shell in
/// about two seconds, and the UEFI-console rendering of these lines is
/// gone long before a human (or `prlctl capture`) can catch it -
/// confirmed by screenshotting a real boot at 0.4-second intervals and
/// never seeing anything but the finished shell. `main.rs` re-prints
/// the returned inventory through the post-exit console once one is
/// installed, the same stash-and-reprint pattern the xHCI bring-up's
/// diagnostics already needed for the identical reason.
pub fn log_all_devices() -> ([PciDeviceId; MAX_LOGGED_DEVICES], usize) {
    let mut devices = [PciDeviceId::default(); MAX_LOGGED_DEVICES];
    let mut count = 0usize;

    let Ok(handles) = uefi::boot::find_handles::<PciRootBridgeIo>() else {
        log::warn!("Ouroboros kernel: PCI device dump: no root bridge found");
        return (devices, count);
    };

    for handle in handles {
        let Ok(mut root_bridge) = uefi::boot::open_protocol_exclusive::<PciRootBridgeIo>(handle)
        else {
            continue;
        };
        let Ok(tree) = root_bridge.enumerate() else {
            continue;
        };

        for addr in tree.iter() {
            let Ok(vendor_device) = root_bridge.pci().read_one::<u32>(addr.with_register(0)) else {
                continue;
            };
            let Ok(class_reg) = root_bridge
                .pci()
                .read_one::<u32>(addr.with_register(CLASS_REGISTER))
            else {
                continue;
            };
            let id = PciDeviceId {
                vendor: vendor_device as u16,
                device: (vendor_device >> 16) as u16,
                class: (class_reg >> 24) as u8,
                subclass: (class_reg >> 16) as u8,
                prog_if: (class_reg >> 8) as u8,
            };
            let PciDeviceId { vendor, device, class, subclass, prog_if } = id;
            log::info!(
                "Ouroboros kernel: PCI device: vendor={vendor:#06x} device={device:#06x} class={class:#04x} subclass={subclass:#04x} prog_if={prog_if:#04x}"
            );
            if count < devices.len() {
                devices[count] = id;
                count += 1;
            }
        }
    }

    if count == 0 {
        log::info!("Ouroboros kernel: PCI device dump: root bridge(s) found, but zero devices enumerated");
    }
    (devices, count)
}

/// One enumerated PCI function's identity, captured by
/// [`log_all_devices`] so the inventory survives past
/// `exit_boot_services` for re-printing (see that function's doc
/// comment). `prog_if` is included because it's what distinguishes,
/// e.g., an AHCI SATA controller (class `0x01`/`0x06`/prog-if `0x01`)
/// or an xHCI controller (`0x0c`/`0x03`/`0x30`) from siblings sharing
/// a class:subclass pair.
#[derive(Clone, Copy, Default)]
pub struct PciDeviceId {
    pub vendor: u16,
    pub device: u16,
    pub class: u8,
    pub subclass: u8,
    pub prog_if: u8,
}

/// Upper bound on how many devices [`log_all_devices`] records -
/// generous for the handful of devices a Parallels/QEMU VM exposes
/// (five observed on real Parallels hardware), bounded because the
/// result lives in a fixed array with no heap after boot services.
pub const MAX_LOGGED_DEVICES: usize = 16;

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
