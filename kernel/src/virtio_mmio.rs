//! virtio-mmio transport: device discovery and the register layout shared
//! by every virtio-mmio device (block, console, net, ...), independent of
//! what any specific device does with it. `virtio_blk.rs`,
//! `virtio_console.rs`, `virtio_net.rs` and `virtio_rng.rs` use it.
//!
//! ## Where the transports are: the firmware says, nothing is scanned blind
//!
//! A transport is probed only when the firmware declares it: ACPI's DSDT or
//! an SSDT names each one as a `Device` with `_HID "LNRO0005"` and a `_CRS`
//! holding its register block and its GIC interrupt (`acpi::
//! virtio_mmio_transports`, the scan itself in the `amlscan` crate).
//! [`find_device`] takes that list as a required argument, so no caller can
//! reach a virtio-mmio address the firmware did not give. A declaration
//! counts only when its `_STA` says the device is present: Parallels 27.1
//! declares a virtio GPU behind a `_STA` method, and in a VM without that
//! GPU its registers fault too. A platform that declares none present
//! (Parallels, the Pi) is never touched.
//!
//! **Why, twice over.** QEMU's `virt` machine puts 32 transports 0x200
//! bytes apart from `0xa000000` (its DSDT declares exactly those, SPI 16 up
//! for their interrupts), and until 2026-11-04 this module scanned that
//! range, hardcoded, behind a heuristic in `main.rs`: a serial console found
//! before the exit was taken to mean QEMU. On Parallels the first slot's
//! read is a synchronous external abort (`ESR_EL1 0x96000010`, EC `0x25`,
//! DFSC `0x10`, `FAR_EL1` the slot's address): seen in 2026-08, when every
//! console mechanism failed there and the heuristic held (`docs/
//! CHANGELOG.md`'s "GOP framebuffer console, round three"), and again on
//! 2026-11-04, when Parallels 27.1 gave its VM a serial console and the
//! heuristic opened the scan (`docs/testing/testing-parallels.md`,
//! "Parallels 27.1 and the one-disk stick"). This kernel has no resumable
//! EL1 synchronous-fault path (`exceptions.rs`), so a probe cannot fail
//! soft; not probing an undeclared address is the only safe rule.
//!
//! A declared slot is unpopulated until a `-device virtio-*-device` is
//! attached to it (observed: the last-numbered slot fills first), so
//! [`find_device`] checks every declared one for the magic value and the
//! device id. Directly confirmed empirically, not just read from the spec:
//! peeking the block device's slot via the QEMU monitor (`xp/1xw`) before
//! writing any driver code showed `MagicValue=0x74726976` ("virt"),
//! `Version=2`, `DeviceID=2`, `VendorID=0x554d4551` ("QEMU") - exactly
//! the modern (non-legacy) register layout below.
//!
//! **Modern, not legacy - a deliberate choice, not a default.** QEMU's
//! `virtio-mmio` transport defaults to `force-legacy=true` (confirmed via
//! `-device virtio-mmio,help`'s printed default), which uses an older,
//! more complex register interface (page-frame-number-based queue setup,
//! a `GuestPageSize`/`QueueAlign` dance instead of explicit 64-bit
//! desc/avail/used addresses). The Makefile passes
//! `-global virtio-mmio.force-legacy=false` specifically to get the
//! modern interface instead - simpler to drive correctly, and the more
//! likely match for a real (non-QEMU) hypervisor's default, since
//! "legacy" mode exists purely for backward compatibility with old guest
//! drivers this project has no need to imitate.
//!
//! ## The device is not necessarily in its reset state when we get it
//!
//! A real, confirmed finding, not a hypothetical: peeking the block
//! device's Status register the same way (before our kernel ever runs)
//! showed `0xf` - `ACKNOWLEDGE|DRIVER|FEATURES_OK|DRIVER_OK` - already
//! set. Root cause, once traced through: `loader.rs`'s UEFI filesystem
//! reads (and the firmware's own boot process, which is how it finds and
//! loads this kernel to begin with) go through EDK2's own bundled
//! virtio-blk driver talking to this exact device, entirely during boot
//! services, before our kernel exists. `find_device` therefore resets
//! the device (`Status = 0`) as its first step unconditionally, per the
//! virtio spec's own requirement for a driver taking ownership of a
//! device - not assuming a clean slate.

use core::ptr::{read_volatile, write_volatile};

pub use amlscan::{Transport, Transports};

const MAGIC_VALUE: u64 = 0x74726976; // "virt", little-endian bytes of the ASCII string

// Register byte offsets, modern (non-legacy) interface only - see module
// doc comment for why legacy is deliberately not supported.
pub(crate) const REG_MAGIC_VALUE: usize = 0x000;
pub(crate) const REG_VERSION: usize = 0x004;
pub(crate) const REG_DEVICE_ID: usize = 0x008;
pub(crate) const REG_DEVICE_FEATURES: usize = 0x010;
pub(crate) const REG_DEVICE_FEATURES_SEL: usize = 0x014;
pub(crate) const REG_DRIVER_FEATURES: usize = 0x020;
pub(crate) const REG_DRIVER_FEATURES_SEL: usize = 0x024;
pub(crate) const REG_QUEUE_SEL: usize = 0x030;
pub(crate) const REG_QUEUE_NUM_MAX: usize = 0x034;
pub(crate) const REG_QUEUE_NUM: usize = 0x038;
pub(crate) const REG_QUEUE_READY: usize = 0x044;
pub(crate) const REG_QUEUE_NOTIFY: usize = 0x050;
// Interrupt handling (used by IRQ-driven drivers - virtio_net's RX path).
// InterruptStatus's low bits say why the device interrupted (bit 0 = used
// buffer notification, bit 1 = configuration change); the driver must write
// the same bits back to InterruptACK, or the device won't raise the next
// interrupt (virtio-mmio spec section 4.2.2). A polling-only driver
// (virtio_blk) never touches these.
pub(crate) const REG_INTERRUPT_STATUS: usize = 0x060;
pub(crate) const REG_INTERRUPT_ACK: usize = 0x064;
pub(crate) const REG_STATUS: usize = 0x070;
pub(crate) const REG_QUEUE_DESC_LOW: usize = 0x080;
pub(crate) const REG_QUEUE_DESC_HIGH: usize = 0x084;
pub(crate) const REG_QUEUE_DRIVER_LOW: usize = 0x090;
pub(crate) const REG_QUEUE_DRIVER_HIGH: usize = 0x094;
pub(crate) const REG_QUEUE_DEVICE_LOW: usize = 0x0a0;
pub(crate) const REG_QUEUE_DEVICE_HIGH: usize = 0x0a4;
pub(crate) const REG_CONFIG: usize = 0x100;

// Status register bits (written to REG_STATUS to progress through the
// device initialization state machine - virtio spec section 3.1).
pub(crate) const STATUS_ACKNOWLEDGE: u32 = 1;
pub(crate) const STATUS_DRIVER: u32 = 2;
pub(crate) const STATUS_DRIVER_OK: u32 = 4;
pub(crate) const STATUS_FEATURES_OK: u32 = 8;

/// The first declared transport holding a device matching `device_id`
/// (virtio's device id registry: 1 net, 2 block, 3 console, 4 entropy)
/// whose declared block covers the registers and the `config_len` bytes
/// of config space the caller's driver reads. Only the transports in
/// `declared` are read: the firmware's list, never an address of this
/// kernel's own.
///
/// A declared transport that does not walk as device memory in this
/// kernel's own tables (`mmu::walks_as_device`, its first and last byte),
/// or is too short for the registers, is skipped, not read.
///
/// # Safety
/// The kernel's identity map must be installed (`mmu::install_identity_map`).
pub unsafe fn find_device(declared: &Transports, device_id: u32, config_len: usize) -> Option<Transport> {
    for t in declared.as_slice() {
        // The registers lie below `REG_CONFIG` (0x100), and the caller's
        // config space from there: a block too short for both would have
        // the driver read past what was declared (the reviews of #407).
        // QEMU declares 0x200, Parallels 0x4000.
        if (t.size as usize) < REG_CONFIG + config_len {
            continue;
        }
        let Some(last) = t.base.checked_add(u64::from(t.size) - 1) else { continue };
        if !crate::mmu::walks_as_device(t.base) || !crate::mmu::walks_as_device(last) {
            continue;
        }
        // SAFETY: inside the always-mapped device region, and declared.
        let magic = unsafe { read_reg(t.base, REG_MAGIC_VALUE) } as u64;
        if magic != MAGIC_VALUE {
            continue; // A declared slot with nothing attached.
        }
        // SAFETY: same as above.
        let id = unsafe { read_reg(t.base, REG_DEVICE_ID) };
        if id == device_id {
            return Some(*t);
        }
    }
    None
}

/// # Safety
/// `base + offset` must be a valid, mapped virtio-mmio register.
pub(crate) unsafe fn read_reg(base: u64, offset: usize) -> u32 {
    unsafe { read_volatile((base as usize + offset) as *const u32) }
}

/// # Safety
/// `base + offset` must be a valid, mapped virtio-mmio register.
pub(crate) unsafe fn write_reg(base: u64, offset: usize, value: u32) {
    unsafe { write_volatile((base as usize + offset) as *mut u32, value) }
}
