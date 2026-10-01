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

use core::ffi::c_void;
use core::ptr::{self, NonNull};
use uefi::boot::{OpenProtocolAttributes, OpenProtocolParams, ScopedProtocol};
use uefi::proto::pci::root_bridge::PciRootBridgeIo;
use uefi::proto::pci::PciIoAddress;
use uefi::proto::unsafe_protocol;
use uefi::{Handle, Status};

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
    /// A config-space read of the BAR (either half of a 64-bit one) failed.
    ConfigRead,
    /// Found a serial controller, but firmware's own account of where its
    /// BAR decodes could not be had or did not add up - see [`BarError`].
    /// Read only through `{e:?}` log lines (`discover_uart16550`'s
    /// per-controller warning and `main.rs`'s summary), which the dead-code
    /// lint does not count (same as `madt.rs`'s `Acpi` variant).
    #[allow(dead_code)]
    Bar(BarError),
}

/// Enumerates every PCI root bridge's devices looking for a class 0x07
/// subclass 0x00 serial controller, returning the CPU address its BAR0
/// decodes at (firmware's, checked against the BAR - see [`bar0_address`]).
/// Must be
/// called before `exit_boot_services` — entirely boot-services-based, no
/// part of this can run after.
pub fn discover_uart16550() -> Result<usize, DiscoveryError> {
    let handles =
        uefi::boot::find_handles::<PciRootBridgeIo>().map_err(|_| DiscoveryError::NoRootBridge)?;
    let mut last_error = DiscoveryError::NoSerialDevice;

    for handle in handles {
        let Some(mut root_bridge) = open_root_bridge(handle) else {
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

            // A controller that cannot be resolved is skipped, not the end
            // of the search: a later one may still be usable. The last
            // failure is what the caller sees if none is.
            match uart16550_address(&mut root_bridge, addr) {
                Ok(base) => return Ok(base),
                Err(e) => {
                    log::warn!("Ouroboros kernel: pci: skipping the serial controller at {addr:?}: {e:?}");
                    last_error = e;
                }
            }
        }
    }

    Err(last_error)
}

/// One serial controller's CPU address, for [`discover_uart16550`].
fn uart16550_address(root_bridge: &mut PciRootBridgeIo, addr: &PciIoAddress) -> Result<usize, DiscoveryError> {
    let bus_addr = read_bar0_address(root_bridge, *addr)?;
    if bus_addr == 0 {
        return Err(DiscoveryError::Bar(BarError::Unassigned));
    }
    // Read-only, NOT exclusive: a serial controller firmware found is
    // likely firmware's own console, and an exclusive open would disconnect
    // it (framebuffer.rs's module doc has the same trap with GOP, confirmed
    // on Parallels).
    let handle = find_pci_io_handle(root_bridge.segment_nr(), addr).ok_or(DiscoveryError::Bar(BarError::NoPciIo))?;
    let io = open_read_only::<PciIo>(handle).map_err(|e| DiscoveryError::Bar(BarError::Refused(e.status())))?;
    let bar = bar0_address(&io, bus_addr).map_err(DiscoveryError::Bar)?;
    Ok(bar.cpu as usize)
}

/// `discover_xhci`'s result - the BAR address plus enough diagnostic state
/// for `main.rs` to re-print through the post-exit console, since this
/// module's own `log::info!` calls are lost once `fbconsole.rs` clears the
/// boot-services text console (see `discover_xhci`'s doc comment).
#[derive(Debug, Clone, Copy)]
pub struct XhciInfo {
    /// The CPU address `xhci.rs` maps and touches: firmware's host address
    /// for BAR0, not the BAR's own value (see `bar0_address`).
    pub base: u64,
    /// BAR0's raw value, a PCI bus address.
    pub bus: u64,
    /// Firmware's aperture translation, `bus = base + translation`
    /// (wrapping). 0 on QEMU; nonzero on the Raspberry Pi 4 and 400.
    pub translation: u64,
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
    /// A config-space read of the BAR (either half of a 64-bit one) failed.
    ConfigRead,
    /// Found an xHCI controller, but could not take it from firmware or
    /// learn where its BAR decodes - see [`BarError`]. Fails closed: a
    /// keyboard-less boot, never a guessed address.
    Bar(BarError),
    /// Not attempted: the `\NOXHCI` boot flag file is set (`bootflags.rs`).
    SkippedByFlag,
}

/// `read_bar0_address` is shared with the serial path, so its errors
/// arrive as [`DiscoveryError`]. Mapped variant by variant, with no
/// catch-all, so a new failure cannot be reported as a different one.
impl From<DiscoveryError> for XhciDiscoveryError {
    fn from(e: DiscoveryError) -> Self {
        match e {
            DiscoveryError::NoRootBridge => XhciDiscoveryError::NoRootBridge,
            DiscoveryError::NoSerialDevice => XhciDiscoveryError::NotFound,
            DiscoveryError::UnsupportedAddressSpace => XhciDiscoveryError::UnsupportedAddressSpace,
            DiscoveryError::UnsupportedBarType => XhciDiscoveryError::UnsupportedBarType,
            DiscoveryError::ConfigRead => XhciDiscoveryError::ConfigRead,
            DiscoveryError::Bar(e) => XhciDiscoveryError::Bar(e),
        }
    }
}

impl core::fmt::Display for XhciDiscoveryError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            XhciDiscoveryError::NoRootBridge => write!(f, "no PCI root bridge found"),
            XhciDiscoveryError::NotFound => write!(f, "no xHCI controller found on any PCI root bridge"),
            XhciDiscoveryError::Unassigned => write!(f, "xHCI controller's BAR0 was never assigned an address by firmware"),
            XhciDiscoveryError::UnsupportedAddressSpace => write!(f, "xHCI controller's BAR0 is I/O space, not memory space"),
            XhciDiscoveryError::UnsupportedBarType => write!(f, "xHCI controller's BAR0 has an unsupported type"),
            XhciDiscoveryError::ConfigRead => write!(f, "xHCI controller's BAR0 could not be read from config space"),
            XhciDiscoveryError::Bar(e) => write!(f, "xHCI controller: {e}"),
            XhciDiscoveryError::SkippedByFlag => write!(f, "skipped: the \\NOXHCI boot flag is set"),
        }
    }
}

/// Enumerates every PCI root bridge's devices looking for a class 0x0c
/// subclass 0x03 prog-if 0x30 xHCI (USB3) host controller, returning the
/// CPU address its BAR0 decodes at (always a 64-bit memory BAR on real xHCI
/// hardware, per the spec - `read_bar0_address` still handles the 32-bit
/// case for completeness).
///
/// Unlike `virtio_mmio.rs`'s address (a fixed, QEMU-shaped convention
/// confirmed unsafe on real Parallels hardware - see that module's doc
/// comment), this address is genuinely *discovered*: firmware's own host
/// address for the BAR (`bar0_address`, `PciIo.GetBarAttributes`),
/// accepted only when it agrees with the BAR read out of the device's own
/// configuration space, not guessed from a QEMU-specific memory layout.
/// The two differ on the Raspberry Pi 4 and 400, whose PCIe window is
/// translated.
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
///
/// `enable_write` is false only under the `\XHCINOWR` boot flag
/// (`bootflags.rs`): the command-register write below is then skipped and
/// logged as skipped, to separate it from the takeover on real hardware.
pub fn discover_xhci(enable_write: bool) -> Result<XhciInfo, XhciDiscoveryError> {
    let handles =
        uefi::boot::find_handles::<PciRootBridgeIo>().map_err(|_| XhciDiscoveryError::NoRootBridge)?;
    let mut last_error = XhciDiscoveryError::NotFound;

    for handle in handles {
        let Some(mut root_bridge) = open_root_bridge(handle) else {
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

            // A controller that cannot be taken or resolved is skipped, not
            // the end of the search: a later one may still be usable. The
            // last failure is what the caller sees if none is.
            match take_xhci(&mut root_bridge, addr, enable_write) {
                Ok(info) => return Ok(info),
                Err(e) => {
                    log::warn!("Ouroboros kernel: pci: skipping the xHCI controller at {addr:?}: {e}");
                    last_error = e;
                }
            }
        }
    }

    Err(last_error)
}

/// Takes one xHCI controller from firmware and resolves its CPU address,
/// for [`discover_xhci`]. Taking it stops firmware's USB stack on that
/// controller, so nothing may read the boot disk afterwards: see
/// `discover_xhci`'s placement in `main.rs`.
fn take_xhci(
    root_bridge: &mut PciRootBridgeIo,
    addr: &PciIoAddress,
    enable_write: bool,
) -> Result<XhciInfo, XhciDiscoveryError> {
    // Resolve the address FIRST, through a read-only open that leaves
    // firmware's driver running. Only a controller whose address checks out
    // is taken: taking one stops firmware's USB stack on it, and enabling
    // it turns on DMA, so a controller that then failed the check would be
    // left live with no driver and, on a USB-booted machine, the boot disk
    // gone for nothing.
    let pci_io_handle =
        find_pci_io_handle(root_bridge.segment_nr(), addr).ok_or(XhciDiscoveryError::Bar(BarError::NoPciIo))?;
    let bus_addr = read_bar0_address(root_bridge, *addr).map_err(XhciDiscoveryError::from)?;
    // Checked on the raw BAR, before firmware's translation can turn an
    // unassigned 0 into a plausible CPU address.
    if bus_addr == 0 {
        return Err(XhciDiscoveryError::Unassigned);
    }
    let bar = {
        let io = open_read_only::<PciIo>(pci_io_handle)
            .map_err(|e| XhciDiscoveryError::Bar(BarError::Refused(e.status())))?;
        bar0_address(&io, bus_addr).map_err(|e| match e {
            BarError::Unassigned => XhciDiscoveryError::Unassigned,
            e => XhciDiscoveryError::Bar(e),
        })?
    };

    // Now take it: an exclusive open of ITS OWN PciIo stops firmware's xHCI
    // driver (and the USB devices under it) and nothing else. This used to
    // be an exclusive open of the whole root bridge, which firmware refuses
    // (ACCESS_DENIED) when any driver anywhere on the bus will not stop -
    // see `open_root_bridge`. Held until this function returns, like the
    // root-bridge open it replaces, so the Command-register write below
    // lands on a device no firmware driver is managing.
    let _taken = uefi::boot::open_protocol_exclusive::<PciIo>(pci_io_handle)
        .map_err(|e| XhciDiscoveryError::Bar(BarError::Refused(e.status())))?;

    // Enable Memory Space + Bus Master if not already set - see
    // `discover_xhci`'s doc comment for why this write exists at
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
    if !enable_write {
        log::warn!(
            "Ouroboros kernel: xhci: PCI command register {command_before:#06x}, write skipped (\\XHCINOWR)"
        );
    } else if command_before & (CMD_MEMORY_SPACE | CMD_BUS_MASTER) != (CMD_MEMORY_SPACE | CMD_BUS_MASTER) {
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

    Ok(XhciInfo {
        base: bar.cpu,
        bus: bar.bus,
        translation: bar.translation,
        command_before,
        command_after,
    })
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
        let Some(mut root_bridge) = open_root_bridge(handle) else {
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

/// Reads a device's raw BAR0: a PCI *bus* address, which is not always the
/// address the CPU reaches the device at. [`bar0_address`] turns it into
/// that one.
fn read_bar0_address(
    root_bridge: &mut PciRootBridgeIo,
    addr: PciIoAddress,
) -> Result<u64, DiscoveryError> {
    let bar0 = root_bridge
        .pci()
        .read_one::<u32>(addr.with_register(BAR0_REGISTER))
        .map_err(|_| DiscoveryError::ConfigRead)?;

    if bar0 & 0x1 != 0 {
        // Bit 0 set: I/O space BAR, not memory space.
        return Err(DiscoveryError::UnsupportedAddressSpace);
    }

    let base_low = (bar0 & !0xF) as u64;
    match (bar0 >> 1) & 0x3 {
        0b00 => Ok(base_low),
        0b10 => {
            let bar1 = root_bridge
                .pci()
                .read_one::<u32>(addr.with_register(BAR0_REGISTER + 4))
                .map_err(|_| DiscoveryError::ConfigRead)?;
            Ok(base_low | ((bar1 as u64) << 32))
        }
        _ => Err(DiscoveryError::UnsupportedBarType),
    }
}

/// Opens a root bridge read-only (`GetProtocol`), never exclusively.
///
/// Every root-bridge open in this module was once `open_protocol_exclusive`,
/// which asks firmware to disconnect every driver bound anywhere below the
/// bridge. Two ways that goes wrong, and the first is observed: firmware
/// refuses (`ACCESS_DENIED`) when any one of those drivers will not stop,
/// which on QEMU is the network stack on the default NIC once an RNG is
/// present (`make run-usb-kbd` found no xHCI controller from 2026-08-29 to
/// 2026-09-27); and when it succeeds, it tears down firmware's per-device
/// `PciIo` handles, which [`find_pci_io_handle`] now needs. Enumeration and
/// config-space reads need no ownership at all.
fn open_root_bridge(handle: Handle) -> Option<ScopedProtocol<PciRootBridgeIo>> {
    open_read_only::<PciRootBridgeIo>(handle).ok()
}

/// `open_protocol` with `GetProtocol`: a lookup that takes nothing from
/// any driver (see `framebuffer.rs`'s module doc for the bug that taught
/// this project the difference).
fn open_read_only<P: uefi::proto::ProtocolPointer + ?Sized>(handle: Handle) -> uefi::Result<ScopedProtocol<P>> {
    // SAFETY: GetProtocol changes no driver's ownership, and every caller
    // uses the protocol only within its own scope, before
    // exit_boot_services.
    unsafe {
        uefi::boot::open_protocol::<P>(
            OpenProtocolParams { handle, agent: uefi::boot::image_handle(), controller: None },
            OpenProtocolAttributes::GetProtocol,
        )
    }
}

/// `EFI_PCI_IO_PROTOCOL` (UEFI spec, "PCI I/O Protocol"), declared here
/// because neither `uefi` 0.39 nor `uefi-raw` 0.15 has it. Only the two
/// members this module calls are typed; the rest hold their place so the
/// offsets match the spec's layout.
#[repr(C)]
#[unsafe_protocol("4cf5b200-68b8-4ca5-9eec-b23e3f50029a")]
struct PciIo {
    _poll_mem: usize,
    _poll_io: usize,
    _mem: [usize; 2],
    _io: [usize; 2],
    _pci: [usize; 2],
    _copy_mem: usize,
    _map: usize,
    _unmap: usize,
    _allocate_buffer: usize,
    _free_buffer: usize,
    _flush: usize,
    get_location: unsafe extern "efiapi" fn(
        this: *const PciIo,
        segment: *mut usize,
        bus: *mut usize,
        device: *mut usize,
        function: *mut usize,
    ) -> Status,
    _attributes: usize,
    get_bar_attributes: unsafe extern "efiapi" fn(
        this: *const PciIo,
        bar_index: u8,
        supports: *mut u64,
        resources: *mut *mut c_void,
    ) -> Status,
    _set_bar_attributes: usize,
    _rom_size: u64,
    _rom_image: *mut c_void,
}

/// Why a device's BAR could not be resolved to a CPU address. Every case
/// fails closed: the caller goes without the device rather than touching
/// an address firmware did not vouch for.
#[derive(Debug, Clone, Copy)]
pub enum BarError {
    /// No firmware `PciIo` handle sits at the device's segment/bus/device/
    /// function.
    NoPciIo,
    /// Firmware refused to open the device's `PciIo`.
    Refused(Status),
    /// `GetBarAttributes(0)` failed or returned no descriptor.
    NoAttributes(Status),
    /// The descriptor is not a memory-range QWORD address-space descriptor.
    NotMemory,
    /// Firmware's host address and translation do not reproduce the BAR's
    /// own value: `cpu + translation != bus`. Firmware and config space
    /// disagree about the device, so neither is trusted.
    Inconsistent { bus: u64, cpu: u64, translation: u64 },
    /// Firmware places the BAR at CPU address 0: never assigned.
    Unassigned,
}

impl core::fmt::Display for BarError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            BarError::NoPciIo => write!(f, "no firmware PciIo handle at its location"),
            BarError::Refused(status) => write!(f, "firmware refused its PciIo ({status:?})"),
            BarError::NoAttributes(status) => write!(f, "no BAR0 attributes from firmware ({status:?})"),
            BarError::NotMemory => write!(f, "BAR0 attributes are not a memory range"),
            BarError::Inconsistent { bus, cpu, translation } => write!(
                f,
                "BAR0 {bus:#x} does not equal firmware's CPU {cpu:#x} + translation {translation:#x}"
            ),
            BarError::Unassigned => write!(f, "BAR0 was never assigned an address by firmware"),
        }
    }
}

/// Where a BAR decodes, as firmware reports it.
#[derive(Debug, Clone, Copy)]
struct BarAddress {
    cpu: u64,
    bus: u64,
    translation: u64,
}

/// The firmware `PciIo` handle for the device at `segment`/`addr`, matched
/// by `GetLocation`.
fn find_pci_io_handle(segment: u32, addr: &PciIoAddress) -> Option<Handle> {
    let (bus, dev, fun) = (addr.bus as usize, addr.dev as usize, addr.fun as usize);
    let handles = uefi::boot::find_handles::<PciIo>().ok()?;
    handles.into_iter().find(|&handle| {
        let Ok(io) = open_read_only::<PciIo>(handle) else {
            return false;
        };
        let (mut s, mut b, mut d, mut f) = (0usize, 0usize, 0usize, 0usize);
        // SAFETY: a live protocol instance and four valid out-pointers.
        let status = unsafe { (io.get_location)(&*io, &mut s, &mut b, &mut d, &mut f) };
        status.is_success() && (s, b, d, f) == (segment as usize, bus, dev, fun)
    })
}

/// BAR0's CPU address, from firmware's `GetBarAttributes` rather than from
/// the BAR itself.
///
/// **Why: the Raspberry Pi 4 and 400.** A BAR holds a PCI bus address, and
/// on QEMU (translation 0, logged) and Parallels (the raw BAR worked) that
/// was also the CPU address, so nothing ever showed the difference. The
/// BCM2711's PCIe window is translated: under the pftf/EDK2 firmware the
/// VL805 xHCI's BAR reads near `0xF800_0000` while the CPU reaches it at
/// `0x6_0000_0000` (edk2-platforms `RPi4.dsc`: `PcdBcm27xxPciBusMmioAdr`,
/// `PcdBcm27xxPciCpuMmioAdr`).
///
/// **Why firmware's word and not arithmetic of our own:** per UEFI 2.7,
/// `GetBarAttributes` returns the *host* address in `AddrRangeMin`, and
/// EDK2's `PciIo.c` does the conversion itself (`host = device -
/// translation`). So no sign convention is ours to get wrong. What is ours
/// is a check that can fail: the host address plus the translation must
/// give back the BAR's own value, read independently from config space.
/// If it does not, firmware and the device disagree and the device is not
/// used.
fn bar0_address(io: &PciIo, bus: u64) -> Result<BarAddress, BarError> {
    const QWORD_ADDRESS_SPACE_DESCRIPTOR: u8 = 0x8A;
    const RESOURCE_TYPE_MEMORY: u8 = 0;

    let mut resources: *mut c_void = ptr::null_mut();
    // SAFETY: a live protocol instance; `supports` is optional (null), and
    // `resources` is an out-pointer firmware fills with a pool allocation.
    let status = unsafe { (io.get_bar_attributes)(io, 0, ptr::null_mut(), &mut resources) };
    let Some(descriptor) = NonNull::new(resources.cast::<u8>()) else {
        return Err(BarError::NoAttributes(status));
    };
    if !status.is_success() {
        // Firmware allocated and then failed: still ours to free.
        // SAFETY: a pool allocation firmware handed us, not read.
        let _ = unsafe { uefi::boot::free_pool(descriptor) };
        return Err(BarError::NoAttributes(status));
    }
    let base = descriptor.as_ptr();
    // The ACPI QWORD Address Space Descriptor (ACPI 6.x, 6.4.3.5.1), packed:
    // tag at 0, resource type at 3, AddrRangeMin at 0x0E, translation at 0x1E.
    // SAFETY: firmware allocated a full descriptor plus end tag.
    let (tag, resource_type, cpu, translation) = unsafe {
        (
            *base,
            *base.add(3),
            ptr::read_unaligned(base.add(0x0E).cast::<u64>()),
            ptr::read_unaligned(base.add(0x1E).cast::<u64>()),
        )
    };
    // SAFETY: allocated by firmware for us to free (UEFI spec), and not
    // read again.
    let _ = unsafe { uefi::boot::free_pool(descriptor) };

    if tag != QWORD_ADDRESS_SPACE_DESCRIPTOR || resource_type != RESOURCE_TYPE_MEMORY {
        return Err(BarError::NotMemory);
    }
    if cpu.wrapping_add(translation) != bus {
        return Err(BarError::Inconsistent { bus, cpu, translation });
    }
    if cpu == 0 {
        return Err(BarError::Unassigned);
    }
    Ok(BarAddress { cpu, bus, translation })
}
