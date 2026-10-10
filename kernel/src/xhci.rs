//! xHCI (eXtensible Host Controller Interface) driver: minimal bring-up of
//! one USB HID boot-protocol keyboard, over the real xHCI PCI controller
//! (`pci::discover_xhci`) - this kernel's first keyboard input path, and
//! the reason the whole GOP framebuffer console effort (see CLAUDE.md)
//! mattered enough to keep pushing on: that console is write-only, and
//! until this module, so was Parallels itself.
//!
//! ## Why this is safe to attempt even where `qemu_device_region_safe` is
//! ## false
//!
//! Every register this module touches lives at an address firmware itself
//! reports for the xHCI controller's BAR0 (`pci::discover_xhci`: the host
//! address from the controller's own `PciIo.GetBarAttributes`, accepted
//! only when it agrees with the BAR read out of PCI configuration space).
//! **Not the mechanism Parallels confirmed**: there the address was the
//! raw BAR, read through `PciRootBridgeIo`. The two are the same wherever
//! the PCIe window is not translated, and the Raspberry Pi 4/400's is,
//! which is why it changed (2026-09-27); the new path is checked on QEMU
//! only, so the first Parallels or Pi boot is its first real test. That's a
//! fundamentally different situation from `virtio_mmio.rs`'s
//! `SLOT_BASE`/`gic.rs`'s `GICD_BASE` - both fixed, QEMU-shaped
//! conventions confirmed unsafe on Parallels by two decoded Synchronous
//! External Aborts (see `virtio_mmio.rs`'s module doc comment and
//! CLAUDE.md's "GOP framebuffer console, take four/five"). This address is
//! *discovered*, not guessed, the same category of thing as the GOP
//! framebuffer's own address - which real Parallels hardware testing
//! already confirmed is safe to map and write post-`exit_boot_services`.
//! `main.rs` therefore calls this module's `init` unconditionally whenever
//! a BAR was found, independent of `qemu_device_region_safe`.
//!
//! ## Scope: the narrowest slice that can type into the shell
//!
//! Every device present at boot is addressed (up to `MAX_DEVICES`), on a
//! root port or behind hubs; one keyboard and one storage device are
//! driven, each through its own endpoints. **Hubs** (since 2026-09-27,
//! for the Raspberry Pi 4/400, where every USB 2.0 device - the Pi 400's
//! keyboard included - sits behind an on-board hub): `configure_hub`
//! powers the downstream ports, then each port in turn is reset
//! (`reset_hub_port`) and its device addressed with a Route String before
//! the next is reset - a new device answers at address 0 and a USB 2.0
//! hub repeats traffic to every enabled port, so two at once would
//! collide. Every Slot Context comes from the one builder,
//! `slot_context`. Checked on QEMU by `make test-usb-hub`; a
//! Low/Full-speed device behind a *High-speed* hub also needs the
//! transaction-translator fields, which QEMU's Full-speed `usb-hub`
//! cannot exercise, so the Pi is their first test. No hot-plug (a device
//! must already be attached when `init` runs, and a hub's status-change
//! endpoint is never armed), no real HID report-descriptor *parsing* (boot
//! protocol's fixed 8-byte layout is assumed directly, same as every
//! BIOS/UEFI keyboard driver does - see the take-what-you-get note below
//! for what happens if the device doesn't actually deliver that shape),
//! and no interrupt-*driven* anything at the CPU level - every wait in
//! this module (command/setup-time control transfers) is a bounded
//! busy-poll, and the interrupt *endpoint*'s own data path (see below) is
//! checked the same non-blocking way from `poll_key`, matching this
//! kernel's existing driver style (`uart.rs`, `virtio_blk.rs`) and
//! necessary anyway since this runs identically whether or not the GIC
//! ended up initialized this boot.
//!
//! ## Real data comes from the interrupt endpoint, not `GET_REPORT` - a
//! ## real platform finding, not a design preference
//!
//! Every early version of this driver polled the device with `GET_REPORT`
//! HID class requests over the control endpoint (EP0) - the simpler of
//! the two mechanisms the USB HID spec allows, and the one that worked
//! immediately in every QEMU `usb-kbd` test. **Real Parallels hardware
//! testing showed it never worked there, and traced why precisely, not by
//! guessing:** `GET_REPORT`'s response data kept coming back as either
//! this driver's own `GET_REPORT` Setup packet bytes (byte-for-byte,
//! tracked exactly across two different requested lengths - ruling out a
//! buffer-size coincidence) or, in one test, what decoded cleanly as
//! Interface/HID/Endpoint descriptor content - never a live HID report. A
//! poisoned-buffer test (fill the DMA target with `0xee` immediately
//! before ringing the doorbell) confirmed the *data stage* genuinely
//! executes and genuinely overwrites the buffer - this isn't a stuck or
//! skipped transfer. The real, confirmed conclusion: a clean,
//! independent `GET_DESCRIPTOR(Device)` *standard* request, issued right
//! next to a failing `GET_REPORT`, came back as a perfectly valid device
//! descriptor (`bLength=0x12`, `bDescriptorType=0x01`, `idVendor=0x203a`,
//! Parallels' own real, registered USB vendor ID for this exact virtual
//! keyboard). Standard control requests reach the real device correctly;
//! HID *class* requests (`SET_PROTOCOL`, `GET_REPORT`) do not get
//! forwarded by Parallels' USB passthrough at all. See
//! `control_transfer`'s doc comment for where that leaves standard
//! requests (still used, for `GET_DESCRIPTOR`/`SET_CONFIGURATION`) and
//! `INT_RING`'s for the real fix: the interrupt endpoint is armed via a
//! standard xHCI *command* (Configure Endpoint - not a class *request* at
//! all) and, once armed, delivers reports with no request/response
//! exchange of any kind for `poll_key` to fail to reach.
//!
//! **A real, accepted consequence, not yet resolved either way:** since
//! `SET_PROTOCOL(Boot Protocol)` is a class request, it likely never
//! reaches the real device either (attempted anyway, non-fatally, in case
//! some future platform *does* forward it) - meaning a real keyboard may
//! well still be delivering its native Report Protocol layout over the
//! interrupt endpoint, not guaranteed to be the fixed 8-byte boot layout
//! this driver's `keycode_to_ascii` assumes. Many simple keyboards use
//! the same 8-byte layout for both modes regardless (nothing forces them
//! to differ), so this may simply work - `poll_key`'s report-change log
//! line makes it directly observable if it doesn't.
//!
//! ## EP0's Max Packet Size: still relevant, now only for setup-time
//! ## standard requests
//!
//! `SET_CONFIGURATION` has no data stage; `GET_DESCRIPTOR(Device)` is
//! exactly 18 bytes; `GET_DESCRIPTOR(Configuration)` is requested as 64
//! bytes but a short packet is fully expected and fine (see `CTRL_BUF`'s
//! doc comment). For USB2 speeds (Low/Full/High), the spec guarantees
//! `bMaxPacketSize0` is never smaller than 8, so an 18- or 64-byte
//! request... does *not* automatically fit in one packet the way an
//! 8-byte one always would. High speed is spec-fixed at 64 and Low speed
//! at 8, but Full speed may be 8, 16, 32 or 64, so for a Full-speed
//! device `address_and_classify` does the standard dance: read the first
//! 8 bytes of the Device descriptor, and if `bMaxPacketSize0` differs
//! from the guess of 8, correct EP0 with Evaluate Context. Added with hub
//! support, which brought the first Full-speed devices; every QEMU
//! Full-speed device reports 8, so the correction itself was checked by
//! starting from a wrong guess on purpose. USB3 (SuperSpeed/
//! SuperSpeedPlus) has no such gap at all: its EP0 Max Packet Size is
//! spec-*fixed* at 512, always enough regardless of request size, which
//! is what this driver declares for those speeds (`port_reset`'s
//! `ep0_max_packet_size` match) - confirmed necessary by real hardware,
//! not hypothetical: the first real Parallels keyboard found was on a
//! SuperSpeed port, unlike every QEMU `usb-kbd` test (always High Speed).
//!
//! ## `wfe`, and a real prerequisite fix this module depends on
//!
//! `shell/src/main.rs`'s main loop used to call `wfe()` between bytes.
//! That's only guaranteed to resume on a real event - on QEMU, the timer
//! tick's IRQ covers it for free, but real Parallels hardware runs with
//! `qemu_device_region_safe` false, meaning no GIC/timer at all, meaning
//! nothing was architecturally guaranteed to ever wake that `wfe` again.
//! Since this driver's `poll_key` only ever runs when the shell's
//! `try_read_char` loop calls it, an idle `wfe` there would have made
//! every keystroke permanently unreachable on exactly the platform this
//! driver was built for. Fixed alongside this module: that loop now
//! busy-polls instead - see `shell/src/main.rs`'s own comment.
//!
//! ## What isn't handled - real, documented gaps, not oversights
//!
//! No stall recovery for the *interrupt* endpoint specifically (EP0's
//! control transfers recover from any failure - see `recover_ep0` - but
//! an interrupt-endpoint Stall just logs and
//! re-arms the ring, without the Reset Endpoint/Set TR Dequeue Pointer
//! sequence real recovery needs); no auto-repeat (a held key reports once
//! per press, not repeatedly, by design - see [`poll_key`]);
//! unmapped keys (the function keys other than F2 and F3, ...) are
//! silently ignored, while the arrows, Home, End, Page Up/Down, Delete,
//! F2 and F3 send their VT100 sequences (`keycode_to_bytes`, since
//! 2026-10-07), and Escape a bare `ESC` (since 2026-10-09); only the *first* interrupt IN endpoint found in the
//! configuration descriptor is ever configured, so a composite
//! keyboard+something-else device with more than one would only get its
//! first endpoint driven.

use core::cell::UnsafeCell;
use crate::synccell::SyncCell;
use core::ptr::{read_volatile, write_volatile};

use crate::console;

type Trb = [u32; 4];

const TRB_TYPE_NORMAL: u32 = 1;
const TRB_TYPE_SETUP_STAGE: u32 = 2;
const TRB_TYPE_DATA_STAGE: u32 = 3;
const TRB_TYPE_STATUS_STAGE: u32 = 4;
const TRB_TYPE_LINK: u32 = 6;
const TRB_TYPE_ENABLE_SLOT_CMD: u32 = 9;
const TRB_TYPE_DISABLE_SLOT_CMD: u32 = 10;
const TRB_TYPE_ADDRESS_DEVICE_CMD: u32 = 11;
const TRB_TYPE_CONFIGURE_ENDPOINT_CMD: u32 = 12;
const TRB_TYPE_EVALUATE_CONTEXT_CMD: u32 = 13;
const TRB_TYPE_RESET_ENDPOINT_CMD: u32 = 14;
const TRB_TYPE_STOP_ENDPOINT_CMD: u32 = 15;
const TRB_TYPE_SET_TR_DEQUEUE_CMD: u32 = 16;
const TRB_TYPE_TRANSFER_EVENT: u32 = 32;
const TRB_TYPE_CMD_COMPLETION_EVENT: u32 = 33;
const TRB_TYPE_PORT_STATUS_CHANGE_EVENT: u32 = 34;

// Endpoint Context EP State (dword 0 bits 2:0), as the controller keeps
// it in the Output Device Context.
const EP_STATE_RUNNING: u32 = 1;
const EP_STATE_HALTED: u32 = 2;
const EP_STATE_STOPPED: u32 = 3;

const COMPLETION_SUCCESS: u32 = 1;
const COMPLETION_STALL_ERROR: u32 = 6;
const COMPLETION_CONTEXT_STATE_ERROR: u32 = 19;
const COMPLETION_STOPPED: u32 = 26;
const COMPLETION_STOPPED_LENGTH_INVALID: u32 = 27;
const COMPLETION_SHORT_PACKET: u32 = 13;

// Capability register offsets, from the PCI BAR base.
const CAP_HCSPARAMS1: u64 = 0x04;
const CAP_HCSPARAMS2: u64 = 0x08;
const CAP_HCCPARAMS1: u64 = 0x10;
const CAP_DBOFF: u64 = 0x14;
const CAP_RTSOFF: u64 = 0x18;

const HCCPARAMS1_CSZ: u32 = 1 << 2;

// Operational register offsets, from `op_base` (= BAR base + CAPLENGTH).
const OP_USBCMD: u64 = 0x00;
const OP_USBSTS: u64 = 0x04;
const OP_PAGESIZE: u64 = 0x08;
const OP_CRCR: u64 = 0x18;
const OP_DCBAAP: u64 = 0x30;
const OP_CONFIG: u64 = 0x38;
const OP_PORTSC_BASE: u64 = 0x400; // + (port - 1) * 0x10, port is 1-based

const USBCMD_RUN: u32 = 1 << 0;
const USBCMD_HCRST: u32 = 1 << 1;

const USBSTS_HCH: u32 = 1 << 0;
const USBSTS_HSE: u32 = 1 << 2;
const USBSTS_CNR: u32 = 1 << 11;

const CRCR_RCS: u64 = 1 << 0;
const CRCR_CRR: u32 = 1 << 3;

// PORTSC bits/fields. Most of the register must not be written back as
// read: the RW1C change bits (CSC/PEC/WRC/OCC/PRC/PLC/CEC) would be
// cleared, PED (writing 1 *disables* the port) tripped, PR and WPR (RW1S)
// would start a reset, and RsvdZ must be written 0. The only PORTSC write
// is `reg::PortStatus::write`, which writes back only `PORTSC_KEPT`.
const PORTSC_CCS: u32 = 1 << 0;
const PORTSC_PR: u32 = 1 << 4;
const PORTSC_PRC: u32 = 1 << 21;
const PORTSC_PED: u32 = 1 << 1;
const PORTSC_WRC: u32 = 1 << 19;
const PORTSC_PLC: u32 = 1 << 22;
const PORTSC_WPR: u32 = 1 << 31;
const PORTSC_PLS_SHIFT: u32 = 5;
const PORTSC_PLS_MASK: u32 = 0xf;
/// Link states after which a USB3 port needs a Warm Reset, not a Hot one
/// (xHCI 4.19.1.2; Linux's `hub_port_warm_reset_required`). Neither occurs
/// on a USB2 port, so seeing one also says the port is USB3, where Warm
/// Port Reset is defined.
const PLS_SS_INACTIVE: u32 = 6;
const PLS_COMPLIANCE: u32 = 10;
const PORTSC_SPEED_SHIFT: u32 = 10;
const PORTSC_SPEED_MASK: u32 = 0xf;
/// The RW1C change bits, 23:17: CEC, PLC, PRC, OCC, WRC, PEC, CSC. The
/// only bits a PORTSC write may clear.
const PORTSC_RW1C: u32 = (1 << 17) | (1 << 18) | (1 << 19) | (1 << 20) | (1 << 21) | (1 << 22) | (1 << 23);
/// The bits a PORTSC write writes back as read, by inclusion, as Linux's
/// `xhci_port_state_to_neutral` (its `XHCI_PORT_RO | XHCI_PORT_RWS`): the
/// read-only CCS, OCA and speed, and the RWS bits that hold settings, PLS
/// (ignored unless LWS is written 1), PP, PIC 15:14, WCE, WDE and WOE.
/// Every other bit is written 0.
const PORTSC_KEPT: u32 = PORTSC_CCS
    | (1 << 3)
    | (PORTSC_SPEED_MASK << PORTSC_SPEED_SHIFT)
    | (PORTSC_PLS_MASK << PORTSC_PLS_SHIFT)
    | (1 << 9)
    | (0x3 << 14)
    | (1 << 25)
    | (1 << 26)
    | (1 << 27);
/// The bits a PORTSC write may set: the two resets, all a caller asks for.
/// A bit is added here with its caller, once its meaning on both port
/// protocols is checked (WPR is reserved on a USB2 port; a caller sets it
/// only after reading a USB3-only link state, `portsc_needs_warm_reset`).
const PORTSC_SETTABLE: u32 = PORTSC_PR | PORTSC_WPR;

// Runtime interrupter register set 0, from `ir0_base` (= BAR base +
// RTSOFF + 0x20 - interrupter register sets start at RTSOFF+0x20, register
// set 0 is always present).
const IR_IMAN: u64 = 0x00;
const IR_ERSTSZ: u64 = 0x08;
const IR_ERSTBA: u64 = 0x10;
const IR_ERDP: u64 = 0x18;
// The RsvdP bits of each register written whole, kept by its
// `reg::Kept32`/`reg::Kept64` handle: ERSTSZ 31:16 and ERSTBA 5:0 (as
// Linux's `xhci_add_interrupter` and U-Boot keep them), CONFIG 31:10 and
// CRCR 5:4 (Linux's `xhci.h`). USBCMD keeps only 6:4 and 31:17: Linux's
// `xhci.h` calls 4:6 and 15:31 reserved and leaves 12 and 13 unmentioned,
// and xHCI 1.2 is recalled as defining 13:16 (the spec PDF was not read).
// Bits 12:16, which these sources do not settle, are written 0 as before,
// so no bit the firmware left set there is kept on a guess.
const ERSTSZ_RSVDP: u32 = 0xffff_0000;
const ERSTBA_RSVDP: u64 = 0x3f;
const CONFIG_RSVDP: u32 = 0xffff_fc00;
const CRCR_RSVDP: u64 = 0x30;
const USBCMD_RSVDP: u32 = 0xfffe_0070;

const EP_TYPE_CONTROL: u32 = 4;
const EP_TYPE_INTERRUPT_IN: u32 = 7;
const EP_TYPE_BULK_OUT: u32 = 2;
const EP_TYPE_BULK_IN: u32 = 6;

const MAX_SLOTS_ENABLED: usize = 8;
/// The most TRBs `Xhci::ring_publish` takes at once: a control transfer's
/// Setup, Data and Status stages.
const MAX_PUBLISH: usize = 3;
// The Pi 4's VL805 asks for 31 (HCSPARAMS2 0xfc000031, 2026-10-03); this
// held 8 until then. The count each controller asks for is logged at init.
const MAX_SCRATCHPAD_BUFFERS: usize = 32;
const CMD_RING_SIZE: usize = 16;
const EP0_RING_SIZE: usize = 16;
const INT_RING_SIZE: usize = 16;
const EVENT_RING_SIZE: usize = 16;

/// How many devices this driver can keep concurrently addressed - the
/// bound on the per-device DMA pools (`EP0_RINGS`,
/// `OUTPUT_DEVICE_CONTEXTS`) and the `Xhci::slots` array, *not* on the
/// controller's own slot count (`MAX_SLOTS_ENABLED`, 8 - hardware
/// assigns slot IDs from its own space; the pool index is this driver's
/// own bookkeeping). 8, matching `MAX_SLOTS_ENABLED`: a hub takes a
/// slot of its own, so the Raspberry Pi 4/400's layout (the on-board hub,
/// the keyboard behind it, a stick, a mouse) already needs 4, and QEMU's
/// hub rig (`make test-usb-hub`) 4 more with its tablet. Was 4 until hub
/// support. Ports beyond the pool are logged and skipped, not an error.
const MAX_DEVICES: usize = 8;

// Generous bound for every busy-poll in this module - real hardware/QEMU
// responses are microsecond-scale, so this is meant to catch a genuine
// stuck controller, not to be a tight budget.
//
// **Time-bounded (`CNTPCT_EL0`/`CNTFRQ_EL0`, via `timer::now_ticks`/
// `timer::frequency_hz`), not iteration-bounded - a real, confirmed fix,
// not a style choice.** This used to be a fixed iteration count
// (`POLL_ITERS`), on the reasoning that no interrupts or timer are
// available yet at this point in boot - true, but a fixed iteration
// count is only a valid proxy for real elapsed time if the host never
// preempts this vCPU for any real duration while it spins, which a real
// hypervisor doesn't guarantee: confirmed by a real, reproducible xHCI
// command-ring timeout on real Parallels hardware, observed failing on
// two *different* commands across two different boots (once very early
// - Enable Slot/Address Device - once much later - Configure Endpoint),
// a pattern that points at the wait mechanism itself rather than any one
// command's own logic. The ARM generic timer's `CNTPCT_EL0` counter
// needs no GIC and no interrupts either - it's a pure system-register
// read, the identical "safe on any ARMv8 CPU by construction" property
// `timer.rs`'s own `frequency_hz`/`arm` already rely on - so switching
// to it loses nothing this module's original no-interrupts-yet
// constraint required.
const POLL_TIMEOUT_MS: u64 = 1000;

/// Minimum boot port-scan settle wait: the scan always polls for at least this
/// long before it will enumerate, so a slow device isn't missed just because a
/// fast one already connected and its port-set looks "stable". See the scan
/// loop in [`init_inner`] for the real bug this fixes: on real Parallels a fast
/// SuperSpeed stick reports connected well before the slower synthetic keyboard
/// settles on its own port, and enumerating the moment the stick connected
/// missed the keyboard entirely (no hot-plug -> missed for the whole boot). The
/// keyboard settles within ~1s even as the only device, so 1.5s reliably covers
/// it; a debounce (below) extends the wait further if the set is still changing.
const SCAN_MIN_SETTLE_MS: u64 = 1500;
/// After [`SCAN_MIN_SETTLE_MS`], the scan proceeds once the set of connected
/// ports has *additionally* held steady this long (all present devices have
/// shown up) - so a device appearing right at the minimum-wait boundary still
/// gets caught.
const SCAN_DEBOUNCE_MS: u64 = 400;
/// Overall cap on the boot port-scan settle wait, so a port set that never
/// stops changing (or nothing connecting at all) can't spin here forever. Only
/// reached when the set is still flapping - a normal boot proceeds via the
/// min-settle + debounce path well before this.
const SCAN_SETTLE_CAP_MS: u64 = 5000;

/// A deadline `POLL_TIMEOUT_MS` from now, in the same `CNTPCT_EL0` units
/// `timer::now_ticks` returns - compare against `timer::now_ticks()` in
/// a `while` loop instead of counting fixed iterations. See
/// `POLL_TIMEOUT_MS`'s doc comment for why this replaced a fixed
/// iteration count.
fn poll_deadline() -> u64 {
    crate::timer::now_ticks() + crate::timer::frequency_hz() / 1000 * POLL_TIMEOUT_MS
}

#[derive(Debug)]
pub enum Error {
    ResetTimeout,
    HaltTimeout(u32),
    StartTimeout,
    NoPortConnected,
    NoKeyboardFound,
    PortResetTimeout,
    UnsupportedSpeed(u32),
    BadHubDescriptor,
    PortNotEnabled,
    RootPortNotEnabled(u32),
    TooManyScratchpadBuffers(u32),
    PageSizeNot4K(u32),
    CommandTimeout,
    CommandFailed(u32),
    TransferTimeout,
    TransferFailed(u32),
}

impl Error {
    /// Whether this is a transfer the device answered with a Stall (the
    /// endpoint is now Halted), as opposed to a timeout or another error.
    pub(crate) fn is_stall(&self) -> bool {
        matches!(self, Error::TransferFailed(COMPLETION_STALL_ERROR))
    }
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::ResetTimeout => write!(f, "controller reset timed out"),
            Error::HaltTimeout(usbsts) => write!(f, "controller handed over running did not halt before the reset (USBSTS {usbsts:#x})"),
            Error::StartTimeout => write!(f, "controller failed to start (USBSTS.HCH stuck set)"),
            Error::NoPortConnected => write!(f, "no device connected on any root port"),
            Error::NoKeyboardFound => write!(f, "devices found, but no boot-protocol keyboard among them"),
            Error::PortResetTimeout => write!(f, "port reset timed out"),
            Error::PortNotEnabled => write!(f, "hub port not enabled after reset"),
            Error::RootPortNotEnabled(portsc) => write!(f, "root port not enabled after reset, PORTSC {}", Portsc(*portsc)),
            Error::BadHubDescriptor => write!(f, "hub descriptor unreadable or out of range (1-15 ports)"),
            Error::UnsupportedSpeed(speed) => write!(f, "unsupported port speed {speed} (only Low/Full/High/SuperSpeed/SuperSpeedPlus are implemented)"),
            Error::TooManyScratchpadBuffers(n) => write!(f, "controller wants {n} scratchpad buffers, only {MAX_SCRATCHPAD_BUFFERS} are supported"),
            Error::PageSizeNot4K(reg) => write!(f, "controller's PAGESIZE is {reg:#x}, and scratchpad buffers here are 4 KB pages"),
            Error::CommandTimeout => write!(f, "command ring: timed out waiting for a completion event"),
            Error::CommandFailed(code) => write!(f, "command failed, completion code {code}"),
            Error::TransferTimeout => write!(f, "transfer: timed out waiting for a transfer event"),
            Error::TransferFailed(code) => write!(f, "transfer failed, completion code {code}"),
        }
    }
}

#[repr(align(4096))]
struct Page(UnsafeCell<[u8; 4096]>);
unsafe impl Sync for Page {}

#[repr(align(64))]
pub(crate) struct Aligned64<T>(pub(crate) UnsafeCell<T>);
unsafe impl<T> Sync for Aligned64<T> {}

/// Every byte the xHCI controller (and, through it, the USB storage
/// driver) reaches by DMA, in ONE page-aligned static that owns its pages
/// outright: nothing else the kernel keeps can share a page with it,
/// because the struct is 4096-aligned and Rust rounds its size up to that
/// alignment.
///
/// **Why one pool, not one static per buffer:** on the Raspberry Pi 4/400
/// PCIe DMA is not cache-coherent (`docs/testing/testing-pi4.md` Risk 8),
/// so there this memory is mapped Normal Non-cacheable (`mmu.rs`, which
/// `main.rs` hands [`dma_region`] only when ACPI declares DMA
/// non-coherent, `_CCA 0`; everywhere else, QEMU and Parallels included,
/// it is ordinary write-back memory and nothing here may assume
/// otherwise). Mapping works on whole 4 KB pages, and the
/// buffers used to be separate statics sharing pages with ordinary kernel
/// data - `usb_msd.rs`'s `NEXT_TAG` atomic sat next to its `DATA_BUF`,
/// and exclusive (atomic) accesses to non-cacheable memory are not
/// guaranteed to work. The per-buffer statics below are now references
/// into this pool, so every use of them is unchanged.
#[repr(C, align(4096))]
pub(crate) struct DmaPool {
    scratchpad_pages: [Page; MAX_SCRATCHPAD_BUFFERS],
    output_device_contexts: [Aligned64<[u32; CTX_DWORDS_MAX * 32]>; MAX_DEVICES],
    input_context: Aligned64<[u32; CTX_DWORDS_MAX * 33]>,
    dcbaa: Aligned64<[u64; MAX_SLOTS_ENABLED + 1]>,
    // Here for its 64 bytes, which put the EP0 rings on a 256-byte
    // boundary so none of them crosses a page (the 8-entry scratchpad
    // array used to sit here and do the same; the assertions below say
    // when an order stops working).
    erst: Aligned64<[ErstEntry; 1]>,
    command_ring: Aligned64<[Trb; CMD_RING_SIZE]>,
    ep0_rings: [Aligned64<[Trb; EP0_RING_SIZE]>; MAX_DEVICES],
    int_ring: Aligned64<[Trb; INT_RING_SIZE]>,
    bulk_in_ring: Aligned64<[Trb; INT_RING_SIZE]>,
    bulk_out_ring: Aligned64<[Trb; INT_RING_SIZE]>,
    event_ring: Aligned64<[Trb; EVENT_RING_SIZE]>,
    ctrl_buf: Aligned64<[u8; 64]>,
    int_buf: Aligned64<[u8; 8]>,
    usb_cbw: Aligned64<[u8; 31]>,
    usb_csw: Aligned64<[u8; 13]>,
    usb_data: Aligned64<[u8; 512]>,
    scratchpad_array: Aligned64<[u64; MAX_SCRATCHPAD_BUFFERS]>,
}

static DMA_POOL: DmaPool = DmaPool {
    scratchpad_pages: [const { Page(UnsafeCell::new([0; 4096])) }; MAX_SCRATCHPAD_BUFFERS],
    output_device_contexts: [const { Aligned64(UnsafeCell::new([0; CTX_DWORDS_MAX * 32])) }; MAX_DEVICES],
    input_context: Aligned64(UnsafeCell::new([0; CTX_DWORDS_MAX * 33])),
    dcbaa: Aligned64(UnsafeCell::new([0; MAX_SLOTS_ENABLED + 1])),
    erst: Aligned64(UnsafeCell::new([ErstEntry { base: 0, size: 0, _reserved: 0 }])),
    command_ring: Aligned64(UnsafeCell::new([[0; 4]; CMD_RING_SIZE])),
    ep0_rings: [const { Aligned64(UnsafeCell::new([[0; 4]; EP0_RING_SIZE])) }; MAX_DEVICES],
    int_ring: Aligned64(UnsafeCell::new([[0; 4]; INT_RING_SIZE])),
    bulk_in_ring: Aligned64(UnsafeCell::new([[0; 4]; INT_RING_SIZE])),
    bulk_out_ring: Aligned64(UnsafeCell::new([[0; 4]; INT_RING_SIZE])),
    event_ring: Aligned64(UnsafeCell::new([[0; 4]; EVENT_RING_SIZE])),
    ctrl_buf: Aligned64(UnsafeCell::new([0; 64])),
    int_buf: Aligned64(UnsafeCell::new([0; 8])),
    usb_cbw: Aligned64(UnsafeCell::new([0; 31])),
    usb_csw: Aligned64(UnsafeCell::new([0; 13])),
    usb_data: Aligned64(UnsafeCell::new([0; 512])),
    scratchpad_array: Aligned64(UnsafeCell::new([0; MAX_SCRATCHPAD_BUFFERS])),
};

// xHCI placement rules for the pool's contents (xHCI 1.2 Table 6-1),
// checked at compile time: the layout satisfies them today only because of
// field order and the current constants, and a change to either (say
// `MAX_DEVICES`) would otherwise break them silently - the controller then
// reads a corrupt context, with nothing to show why. `DMA_POOL` itself is
// 4096-aligned, so an offset's page is the address's page.
const fn within(offset: usize, size: usize, boundary: usize) -> bool {
    offset / boundary == (offset + size - 1) / boundary
}
/// Every element of an array field of `len` elements of `size` bytes at
/// `offset` stays within `boundary`.
const fn each_within(offset: usize, size: usize, len: usize, boundary: usize) -> bool {
    let mut i = 0;
    while i < len {
        if !within(offset + i * size, size, boundary) {
            return false;
        }
        i += 1;
    }
    true
}
const PAGE: usize = 4096;
const _: () = {
    use core::mem::{offset_of, size_of};
    // Contexts and context-like structures: no 4 KB page crossing.
    assert!(within(offset_of!(DmaPool, dcbaa), size_of::<[u64; MAX_SLOTS_ENABLED + 1]>(), PAGE), "DCBAA crosses a page");
    assert!(within(offset_of!(DmaPool, scratchpad_array), size_of::<[u64; MAX_SCRATCHPAD_BUFFERS]>(), PAGE), "scratchpad array crosses a page");
    assert!(within(offset_of!(DmaPool, input_context), size_of::<[u32; CTX_DWORDS_MAX * 33]>(), PAGE), "Input Context crosses a page");
    assert!(
        each_within(offset_of!(DmaPool, output_device_contexts), size_of::<Aligned64<[u32; CTX_DWORDS_MAX * 32]>>(), MAX_DEVICES, PAGE),
        "an Output Device Context crosses a page"
    );
    assert!(within(offset_of!(DmaPool, erst), size_of::<[ErstEntry; 1]>(), PAGE), "ERST crosses a page");
    // ERSTBA's low six bits are RsvdP, kept from the register; the pool is
    // page-aligned, so this offset is the ERST address's alignment.
    assert!(offset_of!(DmaPool, erst) % 64 == 0, "ERST not 64-byte aligned");
    // CRCR's bits 5:4 are RsvdP, kept from the register, so the command
    // ring's address has to leave them clear.
    assert!(offset_of!(DmaPool, command_ring) % 64 == 0, "command ring not 64-byte aligned");
    // Transfer, command and event rings: xHCI forbids a ring segment that
    // crosses 64 KB. Checked against the PAGE, not against 64 KB: the pool
    // is only page-aligned, so an offset says where a ring is within its
    // page but nothing about where the pool's 64 KB boundaries fall (an
    // earlier version checked offsets against 64 KB and could not fail for
    // the case it named). A ring inside one page cannot cross 64 KB, and
    // every ring here is far smaller than a page.
    assert!(within(offset_of!(DmaPool, command_ring), size_of::<[Trb; CMD_RING_SIZE]>(), PAGE), "command ring crosses a page");
    assert!(
        each_within(offset_of!(DmaPool, ep0_rings), size_of::<Aligned64<[Trb; EP0_RING_SIZE]>>(), MAX_DEVICES, PAGE),
        "an EP0 ring crosses a page"
    );
    assert!(within(offset_of!(DmaPool, int_ring), size_of::<[Trb; INT_RING_SIZE]>(), PAGE), "interrupt ring crosses a page");
    assert!(within(offset_of!(DmaPool, bulk_in_ring), size_of::<[Trb; INT_RING_SIZE]>(), PAGE), "bulk IN ring crosses a page");
    assert!(within(offset_of!(DmaPool, bulk_out_ring), size_of::<[Trb; INT_RING_SIZE]>(), PAGE), "bulk OUT ring crosses a page");
    assert!(within(offset_of!(DmaPool, event_ring), size_of::<[Trb; EVENT_RING_SIZE]>(), PAGE), "event ring crosses a page");
};

/// The DMA pool's physical range `(base, size)`, page-aligned at both
/// ends - what `main.rs` hands `mmu.rs` to map non-cacheable.
pub(crate) fn dma_region() -> (u64, u64) {
    (&DMA_POOL as *const DmaPool as u64, core::mem::size_of::<DmaPool>() as u64)
}

/// `usb_msd.rs`'s three DMA buffers (command block, status, one sector),
/// in the pool with the rest.
pub(crate) static USB_CBW_BUF: &Aligned64<[u8; 31]> = &DMA_POOL.usb_cbw;
pub(crate) static USB_CSW_BUF: &Aligned64<[u8; 13]> = &DMA_POOL.usb_csw;
pub(crate) static USB_DATA_BUF: &Aligned64<[u8; 512]> = &DMA_POOL.usb_data;

// One name per piece of driver-owned DMA memory, each a reference into
// `DMA_POOL` above - single-instance, populated once, never touched
// concurrently (this driver has exactly one command/transfer in flight at
// a time).
static DCBAA: &Aligned64<[u64; MAX_SLOTS_ENABLED + 1]> = &DMA_POOL.dcbaa;
static SCRATCHPAD_ARRAY: &Aligned64<[u64; MAX_SCRATCHPAD_BUFFERS]> = &DMA_POOL.scratchpad_array;
static SCRATCHPAD_PAGES: &[Page; MAX_SCRATCHPAD_BUFFERS] = &DMA_POOL.scratchpad_pages;
static COMMAND_RING: &Aligned64<[Trb; CMD_RING_SIZE]> = &DMA_POOL.command_ring;
// One EP0 transfer ring per concurrently-addressed device (pool index =
// `Xhci::slots` index). Used to be a single shared ring, with a
// per-candidate-device reset dance: each device's Address Device command
// declares its own EP0 dequeue pointer, so a shared ring only ever
// worked because every non-keyboard device was *abandoned* before the
// next was tried - the ring's software position had to be rewound to
// the base each time to keep matching what the new device's Address
// Device declared. With devices staying concurrently addressed, each
// needs its own ring memory outright.
static EP0_RINGS: &[Aligned64<[Trb; EP0_RING_SIZE]>; MAX_DEVICES] = &DMA_POOL.ep0_rings;
// Transfer ring for the keyboard's interrupt IN endpoint - see
// `Device::poll_key`'s doc comment for why this replaced GET_REPORT
// control transfers entirely: real Parallels hardware testing showed
// HID *class* requests (SET_PROTOCOL, GET_REPORT) don't get forwarded to
// the real device by Parallels' USB passthrough (they came back as this
// driver's own Setup packet bytes, or cached descriptor data), while
// *standard* requests (GET_DESCRIPTOR) demonstrably do. The interrupt
// endpoint is configured via a standard Configure Endpoint *command*
// (not a class *request* - a completely different xHCI mechanism, not
// class-request-shaped at all) and, once armed, delivers real HID
// reports with no request/response round trip of any kind - hardware
// polls the device on its own schedule and DMAs new data straight into
// a buffer this driver posted ahead of time.
static INT_RING: &Aligned64<[Trb; INT_RING_SIZE]> = &DMA_POOL.int_ring;
// Transfer rings for the mass-storage device's bulk endpoint pair
// (usb_msd.rs drives them through `bulk_transfer` below) - same
// 16-entry shape as INT_RING, one ring per endpoint per the xHCI model.
static BULK_IN_RING: &Aligned64<[Trb; INT_RING_SIZE]> = &DMA_POOL.bulk_in_ring;
static BULK_OUT_RING: &Aligned64<[Trb; INT_RING_SIZE]> = &DMA_POOL.bulk_out_ring;
static EVENT_RING: &Aligned64<[Trb; EVENT_RING_SIZE]> = &DMA_POOL.event_ring;

#[repr(C)]
struct ErstEntry {
    base: u64,
    size: u32,
    _reserved: u32,
}
static ERST: &Aligned64<[ErstEntry; 1]> = &DMA_POOL.erst;

// Context word count: 8 dwords (32 bytes) normally, or 16 dwords (64
// bytes) if HCCPARAMS1.CSZ is set - a real, common controller
// configuration (QEMU's `qemu-xhci` defaults to it), not a rare edge
// case, so `init_inner` reads CSZ and picks the real value (`ctx_dwords`)
// at runtime. These statics are sized for the worst case (64-byte, the
// max this driver supports) and only ever partially used when CSZ=0 -
// simpler than two differently-sized static layouts, and the wasted
// space is a few hundred bytes at most.
const CTX_DWORDS_MAX: usize = 16;
// Input Context = 1 Input Control Context + 32 Device Context slots
// (Slot Context + EP0-EP15 IN/OUT) - always allocated at full size per
// spec, even though this driver only ever populates the Slot and EP0
// entries.
static INPUT_CONTEXT: &Aligned64<[u32; CTX_DWORDS_MAX * 33]> = &DMA_POOL.input_context;
// Output Device Contexts: same 32-entry shape as the Input Context,
// minus the Input Control Context - this is what the xHC itself writes
// each device's state into, via the DCBAA entry for that device's slot
// ID. One per concurrently-addressed device (pool index = `Xhci::slots`
// index): two live slots pointing their DCBAA entries at one shared
// context would have hardware writing both devices' state into the same
// memory - real corruption, not a bookkeeping nicety - which is why the
// old single `OUTPUT_DEVICE_CONTEXT` only ever worked while this driver
// abandoned every device but the keyboard.
static OUTPUT_DEVICE_CONTEXTS: &[Aligned64<[u32; CTX_DWORDS_MAX * 32]>; MAX_DEVICES] = &DMA_POOL.output_device_contexts;

/// Scratch buffer for control-transfer data stages. Sized 64, not 8 -
/// real Parallels hardware testing showed `GET_REPORT`'s 8-byte data
/// stage consistently reading back as this driver's own 8-byte Setup
/// packet, byte-for-byte - the same size as both the Setup packet itself
/// and the Data Stage request, which is suspicious enough on its own to
/// be worth ruling out directly: requesting more than the boot-protocol
/// report's real 8 bytes (perfectly legal - a device is always allowed
/// to terminate a control IN transfer early with a short packet, USB spec
/// section 5.5.3) means the Data Stage TRB's declared length can no
/// longer coincidentally equal the Setup packet's fixed 8-byte size,
/// which a same-size buffer never could either way this bug turns out.
static CTRL_BUF: &Aligned64<[u8; 64]> = &DMA_POOL.ctrl_buf;

/// DMA target for the interrupt endpoint's incoming HID reports - a real
/// boot-protocol report is always 8 bytes, so this doesn't need
/// `CTRL_BUF`'s "widened past 8" treatment (that was specifically about
/// a control-transfer/Setup-packet aliasing question that doesn't apply
/// to a Normal TRB, which has no Setup stage at all).
static INT_BUF: &Aligned64<[u8; 8]> = &DMA_POOL.int_buf;

unsafe fn read32(addr: u64) -> u32 {
    unsafe { read_volatile(addr as *const u32) }
}

/// `ring_publish`'s flip: `dmb oshst; str w1, [x0]; ret`, the barrier and
/// the one store that hands a batch of TRBs to the controller. Naked, like
/// `reg`'s `mmio_write32`, so the image holds these fixed instructions
/// whatever the profile: no edit can put a store between them, and
/// `scripts/check-xhci-barriers.py` can find them. Inlining it would leave
/// that check nothing to find.
#[unsafe(naked)]
unsafe extern "C" fn publish_cycle_word(ptr: *mut u32, val: u32) {
    core::arch::naked_asm!("dmb oshst", "str w1, [x0]", "ret");
}
unsafe fn read64(addr: u64) -> u64 {
    unsafe { read32(addr) as u64 | ((read32(addr + 4) as u64) << 32) }
}

/// What a register write that keeps RsvdP bits writes: `current`'s `rsvdp`
/// bits, and `val` in the rest. Pure and `const`, so the asserts below
/// check it at build time: QEMU reads 0 in every RsvdP field, so no rig can
/// see a merge that drops the kept bits.
const fn rsvdp_merge(current: u64, rsvdp: u64, val: u64) -> u64 {
    (current & rsvdp) | (val & !rsvdp)
}
/// What a PORTSC write writes: `current`'s [`PORTSC_KEPT`] bits, every
/// other bit 0, then `set` and a 1 in each change bit named in `clear`. A
/// `set` bit outside [`PORTSC_SETTABLE`], or a `clear` bit outside the
/// change bits, is dropped: it would disable the port, start a reset or
/// clear a change nobody asked to clear. Pure and `const` for the asserts
/// below, as [`rsvdp_merge`].
const fn portsc_merge(current: u32, set: u32, clear: u32) -> u32 {
    (current & PORTSC_KEPT) | (set & PORTSC_SETTABLE) | (clear & PORTSC_RW1C)
}
const _: () = {
    // A port read enabled with every change pending is written with PED
    // and the changes 0, the rest as read.
    assert!(portsc_merge(0x00fe_0203, 0, 0) == 0x0000_0201);
    // A reset sets PR and nothing else moves.
    assert!(portsc_merge(0x00fe_0203, PORTSC_PR, 0) == 0x0000_0211);
    // Clearing PRC writes PRC alone of the change bits.
    assert!(portsc_merge(0x00fe_0203, 0, PORTSC_PRC) == 0x0020_0201);
    // PED cannot be set, nor cleared through `clear`.
    assert!(portsc_merge(0x0000_0201, PORTSC_PED, PORTSC_PED) == 0x0000_0201);
    // A reset in progress (PR reads 1) is not asked for again, and RsvdZ
    // bits 2 and 29:28 reading 1 are written 0.
    assert!(portsc_merge(0x3000_0215, 0, 0) == 0x0000_0201);
    // The settings are kept: PLS 3, PP, PIC 2, WCE/WDE/WOE, with speed 4.
    assert!(portsc_merge(0x0e00_9267, 0, 0) == 0x0e00_9261);
};
const _: () = {
    // The kept bits survive, the new value fills the rest, and a stray bit
    // of the new value cannot land in a kept field.
    assert!(rsvdp_merge(0xffff_0000, 0xffff_0000, 1) == 0xffff_0001);
    assert!(rsvdp_merge(0x0000_ffff, 0xffff_0000, 1) == 1);
    assert!(rsvdp_merge(0x30, 0x30, 0x1000_0040) == 0x1000_0070);
    assert!(rsvdp_merge(0, 0x30, 0x1000_0070) == 0x1000_0040);
};

/// The xHCI registers, written only through a handle that knows how the
/// register must be written. A register with RsvdP bits is a [`reg::Kept32`]
/// or [`reg::Kept64`], whose `write` reads it and keeps those bits (RsvdP
/// is written back as read); every other register is a
/// [`reg::Whole32`] or [`reg::Whole64`], written whole. The handles' fields
/// and the barrier stores are private to this module, so outside it there is
/// no way to spell a register write that skips either: no bare `write32` to
/// USBCMD, and no handle for it but the one carrying its RsvdP mask. Until
/// 2026-10-04 the RsvdP helpers were opt-in beside a plain `write32` that
/// would take any address. Reads cannot clobber anything, so they are not
/// held to this: PORTSC and the RsvdP diagnostic read through their
/// handles, the rest with `read32`/`read64` at a block's `at`.
///
/// The handles are built only from their own register block, an
/// [`reg::OpRegs`], [`reg::Ir0Regs`] or [`reg::Doorbells`], and those only by
/// [`reg::locate`] from the capability registers, so a handle cannot be built
/// from another block's base: until the review of #207 a doorbell built
/// from the operational base was a whole write to USBCMD. Within a block,
/// a port or doorbell index past its range is checked only by a
/// `debug_assert` (the card carries a debug build); the callers bound it.
///
/// What this does not cover: TRBs and contexts are memory, written with
/// `write_volatile`, and nothing stops a raw pointer store to a register
/// address; `publish_cycle_word`, the ring's `dmb oshst; str`, takes any
/// pointer, so it too would store to a register without the `dsb sy` or the
/// RsvdP merge; a block's `at` gives a plain address for a read, which can
/// name another block's offset; and DCBAAP and ERDP are both a `Whole64`,
/// so a function taking one would take the other. PORTSC is a
/// [`reg::PortStatus`] of its own, whose write writes back only the
/// read-only and setting bits (until 2026-10-05 it was a `Whole32`, masked
/// by convention, and a port function would take a doorbell). Inside this
/// module any handle can still be built from a bare address, PORTSC's
/// included. The image check sees a rerouted
/// `Whole32::write` or `Whole64::write`, but not a `Kept` write routed
/// around them, since the whole writes keep their other callers.
mod reg {
    use super::*;

    /// The operational registers, at BAR + CAPLENGTH.
    #[derive(Clone, Copy)]
    pub(super) struct OpRegs(u64);
    /// Interrupter register set 0, at BAR + RTSOFF + 0x20 (the sets start at
    /// RTSOFF + 0x20, and set 0 is always present).
    #[derive(Clone, Copy)]
    pub(super) struct Ir0Regs(u64);
    /// The doorbell array, at BAR + DBOFF.
    #[derive(Clone, Copy)]
    pub(super) struct Doorbells(u64);

    /// The three register blocks, from the capability registers at
    /// `bar_base`. The only way to make one.
    pub(super) unsafe fn locate(bar_base: u64) -> (OpRegs, Ir0Regs, Doorbells) {
        let caplength = (unsafe { read32(bar_base) } & 0xff) as u64;
        let dboff = unsafe { read32(bar_base + CAP_DBOFF) } as u64 & !0x3;
        let rtsoff = unsafe { read32(bar_base + CAP_RTSOFF) } as u64 & !0x1f;
        (OpRegs(bar_base + caplength), Ir0Regs(bar_base + rtsoff + 0x20), Doorbells(bar_base + dboff))
    }

    impl OpRegs {
        /// The address of the register at `off`, for a read.
        pub(super) fn at(self, off: u64) -> u64 {
            self.0 + off
        }
    }
    impl Ir0Regs {
        /// The address of the register at `off`, for a read.
        pub(super) fn at(self, off: u64) -> u64 {
            self.0 + off
        }
    }

    /// A 32-bit register with no RsvdP bits, written whole.
    #[derive(Clone, Copy)]
    pub(super) struct Whole32(u64);
    /// A 64-bit register with no RsvdP bits, written whole.
    #[derive(Clone, Copy)]
    pub(super) struct Whole64(u64);
    /// A root port's PORTSC, written only through [`portsc_merge`], which
    /// writes back as read only the read-only and setting bits: written
    /// back as read, a 1 in PED disables the port, a 1 in a change bit
    /// clears a change still pending and a 1 in PR starts another reset.
    /// Its own type, so no doorbell can be passed for it. Like every
    /// handle here, it is unforgeable only outside this module.
    #[derive(Clone, Copy)]
    pub(super) struct PortStatus(u64);
    /// A 32-bit register with RsvdP bits, written keeping them.
    #[derive(Clone, Copy)]
    pub(super) struct Kept32 {
        addr: u64,
        rsvdp: u32,
    }
    /// A 64-bit register with RsvdP bits, written keeping them.
    #[derive(Clone, Copy)]
    pub(super) struct Kept64 {
        addr: u64,
        rsvdp: u64,
    }

    pub(super) fn usbcmd(op: OpRegs) -> Kept32 {
        Kept32 { addr: op.at(OP_USBCMD), rsvdp: USBCMD_RSVDP }
    }
    pub(super) fn config(op: OpRegs) -> Kept32 {
        Kept32 { addr: op.at(OP_CONFIG), rsvdp: CONFIG_RSVDP }
    }
    pub(super) fn crcr(op: OpRegs) -> Kept64 {
        Kept64 { addr: op.at(OP_CRCR), rsvdp: CRCR_RSVDP }
    }
    /// DCBAAP's low 6 bits are RsvdZ: written 0, which a 64-byte-aligned
    /// array does.
    pub(super) fn dcbaap(op: OpRegs) -> Whole64 {
        Whole64(op.at(OP_DCBAAP))
    }
    /// A root port's PORTSC, `port` 1-based.
    pub(super) fn portsc(op: OpRegs, port: u32) -> PortStatus {
        debug_assert!(port >= 1, "PORTSC: ports are 1-based");
        PortStatus(op.at(OP_PORTSC_BASE + ((port - 1) as u64) * 0x10))
    }
    pub(super) fn erstsz(ir: Ir0Regs) -> Kept32 {
        Kept32 { addr: ir.at(IR_ERSTSZ), rsvdp: ERSTSZ_RSVDP }
    }
    pub(super) fn erstba(ir: Ir0Regs) -> Kept64 {
        Kept64 { addr: ir.at(IR_ERSTBA), rsvdp: ERSTBA_RSVDP }
    }
    /// ERDP has no RsvdP bits; its EHB is RW1C, written 0.
    pub(super) fn erdp(ir: Ir0Regs) -> Whole64 {
        Whole64(ir.at(IR_ERDP))
    }
    /// Doorbell `n` (0 the command ring, else a slot ID, at most 255).
    /// Bits 15:8 are RsvdZ.
    pub(super) fn doorbell(db: Doorbells, n: u32) -> Whole32 {
        debug_assert!(n <= 255, "doorbell: past the 256-entry array");
        Whole32(db.0 + (n as u64) * 4)
    }

    impl Whole32 {
        /// A register write, with the barrier every one of them needs in front
        /// of it: `dsb sy`, so each load and store the CPU made before it is
        /// complete before the controller sees the write. A register write is how
        /// the controller is told to look at memory (a doorbell after a TRB,
        /// DCBAAP/CRCR/ERSTBA/Run after the structures they point at) or that
        /// memory may be reused (ERDP after the event is read), and the pool's
        /// stores and loads are Normal accesses that nothing orders against a
        /// Device store without one. In the write itself, so there is no way to
        /// write a register without it; until 2026-10-03 it was placed by hand
        /// before each such write, and one site had `dmb sy` instead. Linux's
        /// `writel` does the same with the lighter `dmb oshst`; `dsb sy` is the
        /// barrier the Pi 4 has run every doorbell behind.
        pub(super) unsafe fn write(self, val: u32) {
            unsafe { mmio_write32(self.0, val) }
        }
    }

    impl PortStatus {
        pub(super) unsafe fn read(self) -> u32 {
            unsafe { read32(self.0) }
        }
        /// Writes the port through [`portsc_merge`]: `current`'s kept bits,
        /// then `set` (PR or WPR) and a 1 in each change bit in `clear`.
        /// `current` is the caller's latest read of this port, the value it
        /// decided on, so the write and the decision see one state; only
        /// its kept bits reach the register. A `set` outside
        /// [`PORTSC_SETTABLE`], or a `clear` outside the change bits, is a
        /// caller's bug: a debug build, which the card carries, stops on
        /// it; a release build drops it.
        pub(super) unsafe fn write(self, current: u32, set: u32, clear: u32) {
            debug_assert!(set & !PORTSC_SETTABLE == 0, "PORTSC: a set bit that is not PR or WPR");
            debug_assert!(clear & !PORTSC_RW1C == 0, "PORTSC: a clear bit outside the change bits");
            unsafe { Whole32(self.0).write(portsc_merge(current, set, clear)) }
        }
    }

    impl Whole64 {
        /// A 64-bit register as two 32-bit writes, low half first, the order
        /// xHCI section 5.1 gives for Dword writes, and what every driver that
        /// works on the Pi 4's VL805 does: the firmware's own (edk2 `XhciDxe`),
        /// Linux (`xhci_write_64` is `lo_hi_writeq`) and U-Boot. A single 64-bit
        /// store crosses the BCM2711's PCIe bridge, which has not been shown to
        /// carry one intact; the first command timed out on the board while the
        /// store was 64-bit (2026-10-03).
        /// One barrier, then both halves: the barrier orders everything before
        /// the low half, and the high half needs nothing more (ERDP is written on
        /// every event, so a second `dsb sy` there would be paid on the keyboard's
        /// hot path for nothing).
        pub(super) unsafe fn write(self, val: u64) {
            unsafe { mmio_write64(self.0, val as u32, (val >> 32) as u32) }
        }
    }

    impl Kept32 {
        /// The RsvdP bits as they read now, what a write would keep.
        pub(super) unsafe fn kept_bits(self) -> u32 {
            unsafe { read32(self.addr) & self.rsvdp }
        }
        /// The write that keeps the register's RsvdP bits: reads it, keeps
        /// them, puts `val` in the other bits and writes it through
        /// [`Whole32::write`], so with its barrier. A `val` with a bit in the RsvdP field is
        /// a caller's bug (an address not aligned enough, say), so a debug
        /// build, which the card carries, stops on it; a release build
        /// masks it.
        pub(super) unsafe fn write(self, val: u32) {
            debug_assert!(val & self.rsvdp == 0, "a value with bits in the register's RsvdP field");
            let current = unsafe { read32(self.addr) };
            let merged = rsvdp_merge(current as u64, self.rsvdp as u64, val as u64) as u32;
            // Through `Whole32::write`, not `mmio_write32`: one caller per
            // barrier store, so the image check fails if `Whole32::write`
            // is routed around it (a second caller kept it green, the
            // review of #207). Routing THIS write around `Whole32::write`
            // is not seen by that check: the doorbells and PORTSC still
            // call it.
            unsafe { Whole32(self.addr).write(merged) }
        }
    }

    impl Kept64 {
        /// [`Kept32::kept_bits`] for a 64-bit register.
        pub(super) unsafe fn kept_bits(self) -> u64 {
            unsafe { read64(self.addr) & self.rsvdp }
        }
        /// [`Kept32::write`] for a 64-bit register: two halves, low first,
        /// as [`Whole64::write`].
        pub(super) unsafe fn write(self, val: u64) {
            debug_assert!(val & self.rsvdp == 0, "a value with bits in the register's RsvdP field");
            let current = unsafe { read64(self.addr) };
            let merged = rsvdp_merge(current, self.rsvdp, val);
            unsafe { Whole64(self.addr).write(merged) }
        }
    }

    /// The barrier and the register store as fixed instructions, never
    /// inlined, so the image holds `dsb sy; str w1, [x0]; ret` whatever the
    /// profile or the compiler does around it: QEMU has no write buffer to show
    /// the barrier missing, so `scripts/check-xhci-barriers.py` (run by `make
    /// test`) finds this sequence in the built kernel, and a call to it.
    /// A Rust `asm!` barrier before a `write_volatile` compiles to different
    /// shapes in debug and release, and no check can tell its `dsb sy` from
    /// another module's.
    #[unsafe(naked)]
    unsafe extern "C" fn mmio_write32(addr: u64, val: u32) {
        core::arch::naked_asm!("dsb sy", "str w1, [x0]", "ret");
    }

    /// [`Whole64::write`]'s form of [`mmio_write32`], fixed for the same
    /// check: `dsb sy; str w1, [x0]; str w2, [x0, #4]; ret`, low half first.
    #[unsafe(naked)]
    unsafe extern "C" fn mmio_write64(addr: u64, lo: u32, hi: u32) {
        core::arch::naked_asm!("dsb sy", "str w1, [x0]", "str w2, [x0, #4]", "ret");
    }
}

/// USB HID boot-protocol keyboard report is 8 bytes: byte 0 = modifier
/// bitmask, byte 1 = reserved, bytes 2-7 = up to 6 simultaneously-pressed
/// key usage IDs (0 = no key, per USB HID Usage Tables page 0x07).
type Report = [u8; 8];

const MOD_LCTRL: u8 = 1 << 0;
const MOD_LSHIFT: u8 = 1 << 1;
const MOD_RCTRL: u8 = 1 << 4;
const MOD_RSHIFT: u8 = 1 << 5;

/// The bytes one key press sends: a single byte for the keys
/// [`keycode_to_ascii`] maps, or the VT100/xterm sequence for a navigation
/// or function key (DevTools's editor note, item 2,
/// `docs/handoffs/closed/2026-10-05-from-edit-editor-console.md`). Modifiers do not
/// change a navigation key's sequence: xterm's modified forms
/// (`ESC [ 1 ; 5 A` for Ctrl+Up) are not sent.
#[derive(Clone, Copy)]
struct KeyBytes {
    bytes: [u8; KEY_BYTES_MAX],
    len: usize,
}

/// The longest sequence a key sends (`ESC [ 5 ~`).
const KEY_BYTES_MAX: usize = 4;

fn keycode_to_bytes(keycode: u8, shift: bool, ctrl: bool) -> Option<KeyBytes> {
    let seq: &[u8] = match keycode {
        0x52 => b"\x1b[A",  // Up
        0x51 => b"\x1b[B",  // Down
        0x4f => b"\x1b[C",  // Right
        0x50 => b"\x1b[D",  // Left
        0x4a => b"\x1b[H",  // Home
        0x4d => b"\x1b[F",  // End
        0x4b => b"\x1b[5~", // Page Up
        0x4e => b"\x1b[6~", // Page Down
        0x4c => b"\x1b[3~", // Delete (forward)
        0x3b => b"\x1bOQ",  // F2
        0x3c => b"\x1bOR",  // F3
        // Escape: a bare ESC, what a terminal sends. Every sequence above
        // starts with the same byte, so the key is told apart by where it
        // ends: `pending_last` marks its one byte as a whole key for the
        // kernel's queue (decided 2026-10-09). A program reading bytes tells
        // it apart by the quiet after it, as on any terminal; the shell's
        // `keyseq` still drops a printable byte typed straight after it.
        0x29 => b"\x1b",
        _ => {
            let byte = keycode_to_ascii(keycode, shift, ctrl)?;
            return Some(KeyBytes { bytes: [byte, 0, 0, 0], len: 1 });
        }
    };
    let mut bytes = [0u8; KEY_BYTES_MAX];
    bytes[..seq.len()].copy_from_slice(seq);
    Some(KeyBytes { bytes, len: seq.len() })
}

/// USB HID keycode -> ASCII, boot-protocol Usage IDs 0x04-0x38 (letters,
/// digits, and the punctuation/whitespace keys this shell's line editor
/// cares about). `None` for anything unmapped (the modifiers
/// themselves, ...; Escape and the navigation and function keys are
/// [`keycode_to_bytes`]'s) - a real, documented gap, not a bug; see
/// module doc comment. A held Ctrl maps letters to the classic C0
/// control bytes (Ctrl+A = 0x01 ... Ctrl+Z = 0x1a) - added for the
/// `fg` escape hatch specifically (Ctrl+C = 0x03, ETX, intercepted
/// kernel-side to reclaim the keyboard - see
/// `syscall.rs::poll_keyboard_byte`), general because the general
/// mapping is the same three lines. Ctrl+\ = 0x1c too, the way out a
/// program in raw keyboard mode cannot opt out of (`KBD_MODE`).
fn keycode_to_ascii(keycode: u8, shift: bool, ctrl: bool) -> Option<u8> {
    if ctrl {
        return match keycode {
            0x04..=0x1d => Some(1 + (keycode - 0x04)), // Ctrl+A..Ctrl+Z
            // Ctrl+\, the way out in every keyboard mode, on the backslash
            // key: 0x31, or 0x32, the same position on an ISO keyboard (see
            // the 0x32 arm below).
            0x31 | 0x32 => Some(crate::tasks::KEY_QUIT),
            _ => None,
        };
    }
    match keycode {
        0x04..=0x1d => Some(if shift { b'A' + (keycode - 0x04) } else { b'a' + (keycode - 0x04) }),
        0x1e..=0x26 => {
            // '1'-'9', with shifted symbols above the number row.
            if shift {
                Some(*b"!@#$%^&*("
                    .get((keycode - 0x1e) as usize)
                    .unwrap_or(&b'?'))
            } else {
                Some(b'1' + (keycode - 0x1e))
            }
        }
        0x27 => Some(if shift { b')' } else { b'0' }),
        0x28 => Some(b'\r'), // Enter
        0x2a => Some(0x08),  // Backspace
        // Tab, the shell's filename completion; Shift+Tab is a Tab too (no
        // back-tab sequence is sent). Unmapped until 2026-10-09, so on
        // Parallels and the Pi, where the USB keyboard is the only one, Tab
        // completion could not be typed at all.
        0x2b => Some(b'\t'),
        0x2c => Some(b' '),  // Space
        0x2d => Some(if shift { b'_' } else { b'-' }),
        0x2e => Some(if shift { b'+' } else { b'=' }),
        0x2f => Some(if shift { b'{' } else { b'[' }),
        0x30 => Some(if shift { b'}' } else { b']' }),
        // Backslash/pipe - missing until the pipeline milestone made
        // `|` a character worth typing; found by the scripted
        // real-hardware smoke test (the chord arrived, the keymap
        // dropped it), which means no physical keyboard could type a
        // pipeline on Parallels either.
        // 0x32 is the ISO keyboard's key in the backslash position, beside
        // Enter (Swedish, UK, German); PS/2 cannot tell the two apart, and
        // keys here are mapped by their US position, so it types what 0x31
        // does. Without it an ISO keyboard typed nothing there, and could not
        // type Ctrl+\, the way out of a raw program. 0x64, the ISO key beside
        // left Shift, has no US position and stays unmapped: making Ctrl+<
        // the uncatchable kill would be a trap (second high review of #235).
        0x31 | 0x32 => Some(if shift { b'|' } else { b'\\' }),
        0x33 => Some(if shift { b':' } else { b';' }),
        0x34 => Some(if shift { b'"' } else { b'\'' }),
        0x36 => Some(if shift { b'<' } else { b',' }),
        0x37 => Some(if shift { b'>' } else { b'.' }),
        0x38 => Some(if shift { b'?' } else { b'/' }),
        _ => None,
    }
}

/// Controller-global driver state plus the pool of concurrently-
/// addressed devices - split from the original all-in-one `Device`
/// struct when multi-device support landed. Controller-owned things
/// (command/event ring producer/consumer state, `db_base`/`ir0_base`,
/// `ctx_dwords`) live here directly; everything per-device lives in a
/// [`DeviceSlot`] (whose index into `slots` is also which entry of
/// `EP0_RINGS`/`OUTPUT_DEVICE_CONTEXTS` that device owns); the
/// keyboard-specific state (interrupt ring position, report
/// edge-detection) lives in [`KeyboardState`], of which at most one
/// exists - this driver still drives exactly one keyboard, it just no
/// longer *abandons* every other device to do it.
struct Xhci {
    db_base: reg::Doorbells,
    ir0_base: reg::Ir0Regs,
    /// Kept for runtime port rescans (`rescan_ports`) - the boot scan's
    /// locals weren't enough once "scan again later" became a real
    /// operation (Parallels attaches passthrough USB seconds after the
    /// boot scan runs).
    op_base: reg::OpRegs,
    max_ports: u32,
    /// 8 or 16, from HCCPARAMS1.CSZ - see `CTX_DWORDS_MAX`'s doc comment.
    ctx_dwords: usize,
    cmd_enqueue: usize,
    cmd_cycle: bool,
    evt_dequeue: usize,
    evt_cycle: bool,
    /// Set once the first command timeout has been dumped this boot.
    timeout_dumped: bool,
    slots: [Option<DeviceSlot>; MAX_DEVICES],
    keyboard: Option<KeyboardState>,
    storage: Option<StorageState>,
}

/// The activated mass-storage device's bulk endpoint pair - filled in
/// by `activate_storage`, driven by `usb_msd.rs` through
/// [`Xhci::bulk_transfer`]. At most one, same first-wins policy as the
/// keyboard.
struct StorageState {
    /// Pool index into `Xhci::slots`.
    slot: usize,
    in_dci: u32,
    out_dci: u32,
    in_enqueue: usize,
    in_cycle: bool,
    out_enqueue: usize,
    out_cycle: bool,
}

/// One concurrently-addressed device: where it sits, its speed, the slot
/// ID hardware assigned it, whether it is a hub, and this driver's
/// producer position on its own EP0 transfer ring (`EP0_RINGS[pool
/// index]`). Everything a Slot Context needs is here, and [`slot_context`]
/// is the one place that turns it into one.
struct DeviceSlot {
    loc: Location,
    speed: u32, // xHCI speed ID (1=Full, 2=Low, 3=High, 4=Super, 5=SuperPlus) - every Slot Context carries it
    slot_id: u32,
    ep0_enqueue: usize,
    ep0_cycle: bool,
    /// Set once the device is known to be a hub ([`configure_hub`]); every
    /// later Slot Context for this slot must keep saying so.
    hub: Option<HubInfo>,
}

impl DeviceSlot {
    /// A freshly addressed device: EP0 ring at its start, not yet known to
    /// be a hub.
    fn new(loc: Location, speed: u32, slot_id: u32) -> Self {
        DeviceSlot { loc, speed, slot_id, ep0_enqueue: 0, ep0_cycle: true, hub: None }
    }

    /// A pool entry kept occupied after a failed setup whose hardware slot
    /// could not be released, so it is never handed to another device
    /// (see `Scan::note` and `rescan_ports`). Slot ID 0 is never a
    /// real hardware slot, so nothing can ring its doorbell, and no
    /// keyboard, storage or hub state ever points at it.
    fn tombstone(loc: Location) -> Self {
        DeviceSlot::new(loc, 0, 0)
    }
}

/// Where a device sits in the USB tree: the root port it hangs off, and
/// the route through any hubs between (xHCI's Route String: one 4-bit
/// hub port number per tier, tier 1 in bits 3:0). A device directly on a
/// root port has route 0 and depth 0.
///
/// `tt_hub_slot`/`tt_port` name the high-speed hub whose transaction
/// translator talks to this device, and are nonzero only for a Low- or
/// Full-speed device below a High-speed hub - the Raspberry Pi 4/400's
/// keyboard behind its on-board VIA hub. QEMU's `usb-hub` is Full-speed,
/// so `make test-usb-hub` never sets them; the board is their first test.
#[derive(Clone, Copy)]
struct Location {
    root_port: u32, // 1-based
    route: u32,
    depth: u32,
    tt_hub_slot: u32,
    tt_port: u32,
}

impl Location {
    fn root(port: u32) -> Self {
        Location { root_port: port, route: 0, depth: 0, tt_hub_slot: 0, tt_port: 0 }
    }

    /// The device on downstream port `port` of the hub at `self`, running
    /// at `speed`, where `hub_slot`/`hub_speed` are the hub's own.
    fn below(&self, hub_slot: u32, hub_speed: u32, port: u32, speed: u32) -> Self {
        let slow = speed == SPEED_FULL || speed == SPEED_LOW;
        let (tt_hub_slot, tt_port) = if !slow {
            (0, 0)
        } else if hub_speed == SPEED_HIGH {
            (hub_slot, port) // this hub is the translator
        } else {
            (self.tt_hub_slot, self.tt_port) // a slow hub: inherit its translator (0 on a root port)
        };
        Location {
            root_port: self.root_port,
            route: self.route | (port << (4 * self.depth)),
            depth: self.depth + 1,
            tt_hub_slot,
            tt_port,
        }
    }
}

/// Printed as the root port followed by each hub port on the way:
/// `5` for a root-port device, `5.1` for port 1 of a hub on root port 5.
impl core::fmt::Display for Location {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}", self.root_port)?;
        for tier in 0..self.depth {
            write!(f, ".{}", (self.route >> (4 * tier)) & 0xf)?;
        }
        Ok(())
    }
}

/// A hub's Slot Context fields, from its hub descriptor.
#[derive(Clone, Copy)]
struct HubInfo {
    ports: u32,
    /// TT Think Time, `wHubCharacteristics` bits 6:5 - meaningful only for
    /// a High-speed hub.
    ttt: u32,
}

// xHCI speed IDs (the default PSIV mapping; also a Slot Context's Speed
// field). Named everywhere speed is compared, never as bare numbers.
const SPEED_FULL: u32 = 1;
const SPEED_LOW: u32 = 2;
const SPEED_HIGH: u32 = 3;
const SPEED_SUPER: u32 = 4;
const SPEED_SUPER_PLUS: u32 = 5;

/// The first three dwords of `dev`'s Slot Context, with `context_entries`
/// (the highest valid endpoint DCI). The ONLY place a Slot Context is
/// built: Address Device, Evaluate Context and every Configure Endpoint
/// take their route, speed, root port, hub fields and translator fields
/// from here, so no command can describe a device behind a hub as if it
/// sat on a root port (which is what hand-written `Route String=0` at
/// three sites did until hub support).
fn slot_context(dev: &DeviceSlot, context_entries: u32) -> [u32; 3] {
    slot_context_as(dev, dev.hub, context_entries)
}

/// [`slot_context`] with the hub fields taken from `hub` rather than from
/// `dev`'s record: for `configure_hub`, whose Configure Endpoint describes
/// the slot as a hub before the record may say so (it says so only once
/// the controller has accepted it). The one builder; `slot_context` is it
/// applied to the record.
fn slot_context_as(dev: &DeviceSlot, hub: Option<HubInfo>, context_entries: u32) -> [u32; 3] {
    let (hub, ports, ttt) = match hub {
        Some(h) => (1, h.ports, h.ttt),
        None => (0, 0, 0),
    };
    [
        dev.loc.route | (dev.speed << 20) | (hub << 26) | (context_entries << 27), // MTT=0: single TT
        (dev.loc.root_port << 16) | (ports << 24),
        dev.loc.tt_hub_slot | (dev.loc.tt_port << 8) | (ttt << 16),
    ]
}

struct KeyboardState {
    /// Pool index into `Xhci::slots` of the device this state belongs to.
    slot: usize,
    /// Device Context Index of the interrupt IN endpoint, once configured
    /// (see the Configure Endpoint step in `activate_keyboard`) - also
    /// the doorbell target value for it, per the xHCI spec's doorbell
    /// register definition.
    int_dci: u32,
    int_enqueue: usize,
    int_cycle: bool,
    last_report: Report,
    /// Whether the *previous* `poll_key` call failed - logging only on
    /// the ok->error and error->ok transitions (not every occurrence)
    /// keeps a persistent-failure run from flooding the screen the same
    /// way an earlier, unconditional per-poll diagnostic once did.
    had_error: bool,
    /// Newly-pressed keycodes from the most recently processed report,
    /// translated to ASCII, beyond the first one already returned from
    /// that report - see `poll_key`'s doc comment for the real,
    /// confirmed dropped-keystroke bug this fixes. Sized 6: `buf[2..8]`
    /// holds at most 6 simultaneous keycodes, and - since the
    /// mass-storage milestone - a report can be processed from *inside*
    /// a bulk-transfer wait (see `wait_transfer_event`), where there's
    /// no caller to hand the first key to, so all 6 may need to queue.
    /// Bytes, not keys: a navigation key is a sequence of up to
    /// `KEY_BYTES_MAX`, so the queue holds six of the longest.
    pending: [u8; 6 * KEY_BYTES_MAX],
    /// Per byte of `pending`: it is the last of its key. The kernel's
    /// keyboard queue ends a USB key there (`keyseq::Source::Usb`), which is
    /// what makes the Escape key, a bare `ESC`, a complete key at once.
    pending_last: [bool; 6 * KEY_BYTES_MAX],
    pending_len: usize,
}

impl Xhci {
    /// Writes `trbs` in order at the current producer position of `ring`,
    /// advancing `enqueue`/`cycle` and wrapping through the ring's fixed
    /// Link TRB (the last slot) when it reaches the end, and publishes them
    /// to the controller at once. Returns each TRB's physical address at
    /// the same index (the rest zero), for callers that match a later
    /// completion event by pointer (see `push_command`). A batch is one TD,
    /// or a control transfer's Setup, Data and Status stage TDs together,
    /// as Linux's `xhci_queue_ctrl_tx` publishes them; at most
    /// `MAX_PUBLISH` TRBs, which a caller's wrong count turns into a halt
    /// before anything is written. The one way a TRB reaches a ring the
    /// controller may be reading; the only other TRB stores are a ring's
    /// setup (zeroing it and writing its Link TRB), made before the
    /// command that gives the controller the ring.
    ///
    /// **The order is the contract.** A TRB belongs to the controller as
    /// soon as its cycle bit matches the ring's, and the controller may
    /// re-read the dequeue TRB without a new doorbell. So every word of
    /// the batch is written first with the FIRST TRB's cycle bit still
    /// software's, then `dmb oshst`, then that one bit flipped: the
    /// controller sees all of the batch or none of it, never a fresh cycle
    /// bit over a stale pointer or length, and never a Setup stage whose
    /// Data and Status TRBs are not there yet. This is Linux's
    /// `giveback_first_trb`; `dmb oshst` is its `dma_wmb()` on arm64, the
    /// write-side twin of `event_ring_pop`'s `dmb oshld`. Until 2026-10-03
    /// each TRB was one `write_volatile` of all four words, which the
    /// compiler may split into stores in any order, cycle word included,
    /// and which published the TRB as it was written.
    ///
    /// The Link TRB's own cycle bit is rewritten on every wrap, not just
    /// set once at ring-init time: it's logically "produced" exactly like
    /// any real TRB each time the ring cycles, and a stale cycle bit there
    /// would make the second (and every further) lap invalid to hardware.
    /// A real, easy-to-miss class of bug in a first xHCI driver - this
    /// only actually matters once a ring wraps more than once, which the
    /// command ring (at most 2-3 commands ever issued) never does in this
    /// driver's own testing, but the EP0 ring (one `poll_key` per shell
    /// loop iteration, indefinitely) certainly will. Only its cycle word
    /// is written: its pointer words are fixed at ring setup. A wrap inside
    /// a batch is covered by the same barrier, since the controller stops
    /// at the held first TRB and reads nothing past it until the flip.
    unsafe fn ring_publish(ring_ptr: *mut Trb, ring_len: usize, enqueue: &mut usize, cycle: &mut bool, trbs: &[Trb]) -> [u64; MAX_PUBLISH] {
        assert!(trbs.len() <= MAX_PUBLISH);
        let mut addrs = [0u64; MAX_PUBLISH];
        let Some(first) = trbs.first() else { return addrs };
        let first_dw3_ptr = unsafe { ring_ptr.add(*enqueue).cast::<u32>().add(3) };
        let first_dw3 = (first[3] & !1) | (*cycle as u32);
        for (i, trb) in trbs.iter().enumerate() {
            let dw3 = (trb[3] & !1) | (*cycle as u32);
            let slot_ptr = unsafe { ring_ptr.add(*enqueue) };
            let words = slot_ptr.cast::<u32>();
            unsafe {
                write_volatile(words, trb[0]);
                write_volatile(words.add(1), trb[1]);
                write_volatile(words.add(2), trb[2]);
                // The first is held: its cycle bit software's until the flip.
                write_volatile(words.add(3), if i == 0 { dw3 ^ 1 } else { dw3 });
            }
            addrs[i] = slot_ptr as u64;
            *enqueue += 1;
            if *enqueue == ring_len - 1 {
                let link_dw3 = unsafe { ring_ptr.add(ring_len - 1).cast::<u32>().add(3) };
                let link = unsafe { read_volatile(link_dw3) };
                unsafe { write_volatile(link_dw3, (link & !1) | (*cycle as u32)) };
                *cycle = !*cycle;
                *enqueue = 0;
            }
        }
        unsafe { publish_cycle_word(first_dw3_ptr, first_dw3) };
        addrs
    }

    /// One TRB, through [`ring_publish`](Self::ring_publish); returns its
    /// physical address.
    unsafe fn ring_push(ring_ptr: *mut Trb, ring_len: usize, enqueue: &mut usize, cycle: &mut bool, trb: Trb) -> u64 {
        let addrs = unsafe { Self::ring_publish(ring_ptr, ring_len, enqueue, cycle, &[trb]) };
        addrs[0]
    }

    /// Undoes a failed setup's hold on pool entry `idx`: if a hardware
    /// slot was enabled for it, a Disable Slot command releases the slot
    /// and its DCBAA entry is cleared, so nothing in hardware refers to
    /// the entry's Output Device Context any more; then the entry is
    /// emptied. Returns whether the entry is now free. `false` means the
    /// controller refused the Disable Slot, and the slot may still be live.
    fn release_slot(&mut self, dcbaa: &mut [u64; MAX_SLOTS_ENABLED + 1], idx: usize) -> bool {
        let slot_id = self.slots[idx].as_ref().map_or(0, |d| d.slot_id);
        if slot_id != 0 {
            let cmd_ptr = self.push_command([0, 0, 0, (TRB_TYPE_DISABLE_SLOT_CMD << 10) | (slot_id << 24)]);
            if let Err(e) = self.wait_command_completion(cmd_ptr) {
                console::println!("Ouroboros kernel: xhci: slot {slot_id}: Disable Slot failed ({e}), its pool entry stays reserved");
                return false;
            }
            dcbaa[slot_id as usize] = 0;
        }
        self.slots[idx] = None;
        true
    }

    /// The first empty pool entry, or `None` when the pool is full. The one
    /// way an entry is chosen, by the boot scan and the rescan alike: an
    /// entry is empty (`None`) only while nothing in hardware refers to it
    /// (never set up, or released by [`release_slot`](Self::release_slot)),
    /// so the first empty one is always safe, and an entry a failed setup
    /// gave back is used again rather than lost for the rest of the scan.
    /// Taking it does not mark it: the caller sets it up at once.
    fn free_entry(&self) -> Option<usize> {
        self.slots.iter().position(Option::is_none)
    }

    fn push_command(&mut self, trb: Trb) -> u64 {
        let ring_ptr = COMMAND_RING.0.get().cast::<Trb>();
        let addr = unsafe { Self::ring_push(ring_ptr, CMD_RING_SIZE, &mut self.cmd_enqueue, &mut self.cmd_cycle, trb) };
        unsafe { reg::doorbell(self.db_base, 0).write(0) }; // doorbell 0, target field unused for the command ring
        addr
    }

    /// `idx` is the device's pool index - its own EP0 ring
    /// (`EP0_RINGS[idx]`) and its own producer position
    /// (`slots[idx].ep0_enqueue`/`ep0_cycle`). A `None` slot is an
    /// internal caller bug; degrading to a no-op (returning a null
    /// pointer no completion will ever match) beats a panic-handler
    /// hang. Published at once by [`ring_publish`](Self::ring_publish): a
    /// control request's Setup, Data and Status TRBs reach the controller
    /// together. Returns their addresses, all null for an empty slot.
    fn push_ep0(&mut self, idx: usize, trbs: &[Trb]) -> [u64; MAX_PUBLISH] {
        let ring_ptr = EP0_RINGS[idx].0.get().cast::<Trb>();
        let Some(slot) = self.slots[idx].as_mut() else { return [0; MAX_PUBLISH] };
        unsafe { Self::ring_publish(ring_ptr, EP0_RING_SIZE, &mut slot.ep0_enqueue, &mut slot.ep0_cycle, trbs) }
    }

    fn ring_ep0_doorbell(&self, idx: usize) {
        let Some(slot) = self.slots[idx].as_ref() else { return };
        unsafe {
            // Doorbell target 1 = the default control endpoint (EP0).
            reg::doorbell(self.db_base, slot.slot_id).write(1);
        }
    }

    /// Posts one fresh Normal TRB targeting `INT_BUF` on the keyboard's
    /// interrupt ring and rings its doorbell - "arms" the endpoint for
    /// one more incoming report. Called once during setup and again
    /// after every report `poll_key` consumes, so the ring never runs
    /// dry: hardware only has something to DMA a new report *into* if
    /// software keeps posting fresh buffers ahead of it. No-op if no
    /// keyboard was ever activated.
    fn repost_interrupt_buffer(&mut self) {
        // `self.keyboard` and `self.slots` are disjoint fields, so
        // holding `kb` mutably while reading the slot below is fine.
        let Some(kb) = self.keyboard.as_mut() else { return };
        let Some(slot) = self.slots[kb.slot].as_ref() else { return };
        let buf_addr = INT_BUF.0.get() as u64;
        let ring_ptr = INT_RING.0.get().cast::<Trb>();
        unsafe {
            Self::ring_push(
                ring_ptr,
                INT_RING_SIZE,
                &mut kb.int_enqueue,
                &mut kb.int_cycle,
                [buf_addr as u32, (buf_addr >> 32) as u32, 8, (1 << 5) | (TRB_TYPE_NORMAL << 10)], // IOC
            );
            // Doorbell target = the endpoint's own Device Context Index,
            // per the xHCI spec's doorbell register definition (target
            // values above 1 map 1:1 to DCI - see `db_base`'s other
            // caller, `ring_ep0_doorbell`, for the EP0/DCI=1 case).
            reg::doorbell(self.db_base, slot.slot_id).write(kb.int_dci);
        }
    }

    /// Non-blocking: `None` if the TRB at the current consumer position
    /// doesn't yet carry the expected cycle bit (not produced by hardware
    /// yet). No Link TRBs on the event-ring side - a single-segment event
    /// ring just wraps at `EVENT_RING_SIZE`, mirroring how hardware wraps
    /// its own producer side (ERSTSZ tells it the segment size).
    fn event_ring_pop(&mut self) -> Option<Trb> {
        let ring_ptr = EVENT_RING.0.get().cast::<Trb>();
        let slot_ptr = unsafe { ring_ptr.add(self.evt_dequeue) };
        // The word with the cycle bit FIRST, and the rest only after a
        // load barrier. The controller writes an event and then hands it
        // over by its cycle bit; reading all four words at once (as this
        // used to) lets the CPU satisfy the other three before the one
        // that says the event is new, and accept a fresh cycle bit with a
        // stale completion code or pointer. Invisible on QEMU; on the Pi,
        // where the ring is non-cacheable memory a device writes, it is
        // the ordering that makes the event whole.
        let dw3 = unsafe { read_volatile(slot_ptr.cast::<u32>().add(3)) };
        if (dw3 & 1 != 0) != self.evt_cycle {
            return None;
        }
        // `dmb oshld`: the loads are ordered against a device's writes,
        // which is what Linux's `dma_rmb()` uses on arm64.
        unsafe { core::arch::asm!("dmb oshld", options(nostack, preserves_flags)) };
        let words = slot_ptr.cast::<u32>();
        let trb: Trb = unsafe { [read_volatile(words), read_volatile(words.add(1)), read_volatile(words.add(2)), dw3] };
        self.evt_dequeue += 1;
        if self.evt_dequeue == EVENT_RING_SIZE {
            self.evt_dequeue = 0;
            self.evt_cycle = !self.evt_cycle;
        }
        let new_erdp = unsafe { ring_ptr.add(self.evt_dequeue) } as u64;
        // The write's barrier orders the reads of the event above before
        // the controller is told its slot is free.
        unsafe { reg::erdp(self.ir0_base).write(new_erdp) };
        Some(trb)
    }

    /// Hands a Transfer Event that is not the one being waited on to the
    /// keyboard, if it is the keyboard's (its slot ID, word 3 bits 31:24,
    /// and endpoint DCI, bits 20:16, both match). Returns whether it was.
    /// A keyboard report must be *routed*, never dropped: dropping it also
    /// leaves the interrupt buffer unreposted, and the keyboard is dead
    /// for the rest of the boot. Both waits below use this, so a report
    /// arriving during a bulk transfer or during a controller command
    /// goes to the same place.
    fn route_keyboard_event(&mut self, trb: Trb) -> bool {
        let event_slot = trb[3] >> 24;
        let event_dci = (trb[3] >> 16) & 0x1f;
        let kb_ids = self.keyboard.as_ref().and_then(|kb| self.slots[kb.slot].as_ref().map(|s| (s.slot_id, kb.int_dci)));
        if kb_ids != Some((event_slot, event_dci)) {
            return false;
        }
        self.process_keyboard_report(trb[2] >> 24);
        true
    }

    /// Polls the event ring for a Command Completion Event whose Command
    /// TRB Pointer matches `cmd_ptr`, skipping anything else - a stray
    /// Port Status Change Event around the port-reset step is expected,
    /// not an error - except a keyboard report, which is routed
    /// ([`route_keyboard_event`](Self::route_keyboard_event)).
    ///
    /// **A real bug lived here until 2026-09-27: every transfer event was
    /// dropped**, keyboard reports included. After the boot scan the
    /// keyboard's endpoint is armed, and the one runtime path that issues
    /// commands is `mount -a`'s rescan - typed on that same keyboard, so
    /// the Enter key's release arrived during the rescan's Enable Slot,
    /// was dropped, and the keyboard typed nothing more that boot. Found
    /// on QEMU while checking the rescan (the log line was `unexpected
    /// event type=32 while waiting for command completion`).
    fn wait_command_completion(&mut self, cmd_ptr: u64) -> Result<Trb, Error> {
        let deadline = poll_deadline();
        while crate::timer::now_ticks() < deadline {
            let Some(trb) = self.event_ring_pop() else { continue };
            let trb_type = (trb[3] >> 10) & 0x3f;
            if trb_type != TRB_TYPE_CMD_COMPLETION_EVENT {
                if trb_type == TRB_TYPE_TRANSFER_EVENT {
                    if self.route_keyboard_event(trb) {
                        continue;
                    }
                    // Not the keyboard's: named by slot, endpoint and code,
                    // so a request that recovery stopped (Stopped, the
                    // expected by-product of Stop Endpoint on a request in
                    // progress) reads differently from a genuinely stray
                    // completion.
                    let (slot, dci, code) = (trb[3] >> 24, (trb[3] >> 16) & 0x1f, trb[2] >> 24);
                    if code == COMPLETION_STOPPED || code == COMPLETION_STOPPED_LENGTH_INVALID {
                        console::println!("Ouroboros kernel: xhci: slot {slot} DCI {dci}: stopped request's event consumed");
                    } else {
                        console::println!("Ouroboros kernel: xhci: stray transfer event (slot {slot}, DCI {dci}, code {code}) while waiting for command completion, skipping");
                    }
                } else if trb_type != TRB_TYPE_PORT_STATUS_CHANGE_EVENT {
                    console::println!("Ouroboros kernel: xhci: unexpected event type={trb_type} while waiting for command completion");
                }
                continue;
            }
            let ptr = (trb[0] as u64) | ((trb[1] as u64) << 32);
            if ptr != cmd_ptr {
                continue;
            }
            let code = trb[2] >> 24;
            if code != COMPLETION_SUCCESS {
                return Err(Error::CommandFailed(code));
            }
            return Ok(trb);
        }
        self.dump_on_command_timeout(cmd_ptr);
        Err(Error::CommandTimeout)
    }

    /// One dump, the first time a command times out this boot: whether the
    /// controller saw the rings at all (the register readbacks, CRCR's
    /// Command Ring Running bit), whether it hit a DMA error (USBSTS.HSE),
    /// and whether it wrote an event this driver did not see (the event
    /// ring's memory at the dequeue slot). `cmd_ptr` is the command that
    /// timed out.
    fn dump_on_command_timeout(&mut self, cmd_ptr: u64) {
        if self.timeout_dumped {
            return;
        }
        self.timeout_dumped = true;
        let (op, ir) = (self.op_base, self.ir0_base);
        let (usbsts, crcr, dcbaap, iman, erstsz, erstba, erdp) = unsafe {
            (
                read32(op.at(OP_USBSTS)),
                read32(op.at(OP_CRCR)),
                read64(op.at(OP_DCBAAP)),
                read32(ir.at(IR_IMAN)),
                read32(ir.at(IR_ERSTSZ)),
                read64(ir.at(IR_ERSTBA)),
                read64(ir.at(IR_ERDP)),
            )
        };
        console::println!(
            "Ouroboros kernel: xhci: command timeout: USBSTS {usbsts:#x} (HCH {}, HSE {}), CRCR.CRR {}, IMAN {iman:#x}",
            usbsts & USBSTS_HCH != 0,
            usbsts & USBSTS_HSE != 0,
            crcr & CRCR_CRR != 0
        );
        console::println!(
            "Ouroboros kernel: xhci: command timeout: DCBAAP {dcbaap:#x} (pool {:#x}), ERSTSZ {erstsz:#x}, ERSTBA {erstba:#x} (pool {:#x}), ERDP {erdp:#x}",
            DCBAA.0.get() as u64,
            ERST.0.get() as u64
        );
        let read_words = |p: *const u32, n: usize| {
            let mut w = [0u32; 4];
            for (i, slot) in w.iter_mut().enumerate().take(n) {
                *slot = unsafe { read_volatile(p.add(i)) };
            }
            w
        };
        let evt = read_words(unsafe { EVENT_RING.0.get().cast::<Trb>().add(self.evt_dequeue) }.cast(), 4);
        let erst0 = read_words(ERST.0.get().cast(), 3);
        let cmd = read_words(cmd_ptr as *const u32, 4);
        console::println!(
            "Ouroboros kernel: xhci: command timeout: event slot {} {evt:08x?} (cycle wanted {}), ERST[0] {:08x?}, the command @ {cmd_ptr:#x} {cmd:08x?}",
            self.evt_dequeue,
            self.evt_cycle as u32,
            &erst0[..3]
        );
    }

    /// Waits for a Transfer Event *from `expected_slot_id`
    /// specifically* - with more than one device concurrently addressed,
    /// "the next transfer event" and "this device's transfer event" are
    /// no longer the same thing, so an event carrying a different slot
    /// ID (bits 31:24 of TRB word 3) is skipped and logged rather than
    /// mistaken for the completion being waited on. (In practice no
    /// other slot can produce transfer events *during setup* - the
    /// keyboard's interrupt endpoint is deliberately armed only after
    /// the whole port scan finishes, see `init_inner` - but the filter
    /// is what makes that an ordering nicety instead of a correctness
    /// dependency.)
    ///
    /// **And from the request's own TRBs** (`td`, the addresses
    /// `ring_push` or `ring_publish` returned for it): an event from the right slot that
    /// points at any other TRB is a leftover from an earlier request - a
    /// late completion after a timeout, or the Stopped event of a request
    /// that recovery cancelled - and is skipped and logged, never taken
    /// as this request's answer. Matching by slot alone once let exactly
    /// that pair each later request with the previous one's answer. Every
    /// TRB of the request is accepted, not only the last: an error in a
    /// control transfer's data stage is reported against the data-stage
    /// TRB.
    fn wait_transfer_event(&mut self, expected_slot_id: u32, td: &[u64]) -> Result<Trb, Error> {
        let deadline = poll_deadline();
        while crate::timer::now_ticks() < deadline {
            let Some(trb) = self.event_ring_pop() else { continue };
            let trb_type = (trb[3] >> 10) & 0x3f;
            if trb_type != TRB_TYPE_TRANSFER_EVENT {
                if trb_type != TRB_TYPE_PORT_STATUS_CHANGE_EVENT {
                    console::println!("Ouroboros kernel: xhci: unexpected event type={trb_type} while waiting for a transfer event");
                }
                continue;
            }
            let event_slot = trb[3] >> 24;
            let code = trb[2] >> 24;
            if event_slot != expected_slot_id {
                // A keystroke arriving mid-bulk-transfer lands here, and is
                // routed rather than dropped (see `route_keyboard_event`).
                // Anything else is genuinely unexpected and logged.
                if !self.route_keyboard_event(trb) {
                    console::println!("Ouroboros kernel: xhci: transfer event for slot {event_slot} while waiting on slot {expected_slot_id}, skipping");
                }
                continue;
            }
            let ptr = (trb[0] as u64) | ((trb[1] as u64) << 32);
            if !td.contains(&ptr) {
                let dci = (trb[3] >> 16) & 0x1f;
                console::println!("Ouroboros kernel: xhci: stale transfer event (slot {event_slot}, DCI {dci}, code {code}) from an earlier request, skipping");
                continue;
            }
            // Short Packet is success with a residue (normal for a
            // device-terminated IN transfer) - the residue lives in the
            // returned TRB's word 2 if a caller ever needs it.
            if code != COMPLETION_SUCCESS && code != COMPLETION_SHORT_PACKET {
                return Err(Error::TransferFailed(code));
            }
            return Ok(trb);
        }
        Err(Error::TransferTimeout)
    }

    /// Recovers EP0 of pool entry `idx` after ANY failed control transfer,
    /// so the next one on that endpoint starts clean. Two things can be
    /// left behind: the endpoint Halted (a Stall, or a USB transaction
    /// error), or the request's TRBs still queued on a Running endpoint (a
    /// timeout), where a late completion would be taken for the next
    /// request's. So: read the endpoint's state from the Output Device
    /// Context; a Halted endpoint gets Reset Endpoint, a Running one Stop
    /// Endpoint; then Set TR Dequeue points the controller at this
    /// driver's *current* enqueue position, past the aborted request.
    ///
    /// **Why the current position, not the ring's start** (the Stall-only
    /// version this replaced rewound to the start): the slots after the
    /// start still hold the earlier requests' TRBs with the cycle bit the
    /// controller now expects, so after a rewind the controller could run
    /// on into them once the new TRBs were done. At the current enqueue
    /// position everything ahead carries the previous lap's cycle bit, so
    /// the controller stops where the next request ends. The software ring
    /// state is left as it is; the two agree by construction.
    ///
    /// **Why every failure and not only a Stall:** the hub path sends many
    /// control transfers through the hub's one EP0 and promises that one
    /// bad port costs only that port; a timeout that left EP0 unusable
    /// would cost every port after it, the keyboard's included.
    ///
    /// **The state read is not trusted alone.** When it names no command
    /// (a state that is neither Running, Halted nor Stopped), the failure
    /// decides: a timeout leaves the request queued (Stop Endpoint),
    /// anything the controller reported halts the endpoint (Reset
    /// Endpoint). And when the controller answers Context State Error
    /// (the endpoint was not in the state read - it halted just after the
    /// read, say), the other command is sent.
    ///
    /// Best-effort otherwise: a failed command is logged and Set TR
    /// Dequeue still runs, since giving up would leave the endpoint
    /// certainly unusable - except when the controller stops answering
    /// commands at all (Command Timeout), where the rest would only add a
    /// second full timeout to every failure. The event a stopped request
    /// still produces is consumed and logged by `wait_command_completion`
    /// ("stopped request's event consumed"); one arriving later is
    /// skipped by `wait_transfer_event`, which accepts only the current
    /// request's own TRBs.
    fn recover_ep0(&mut self, idx: usize, cause: &Error) {
        let Some((slot_id, enqueue, cycle)) = self.slots[idx].as_ref().map(|s| (s.slot_id, s.ep0_enqueue, s.ep0_cycle)) else {
            return;
        };
        // EP0 is DCI 1: its context follows the Slot Context.
        let state = unsafe { read_volatile(OUTPUT_DEVICE_CONTEXTS[idx].0.get().cast::<u32>().add(self.ctx_dwords)) } & 0x7;
        console::println!("Ouroboros kernel: xhci: slot {slot_id}: EP0 recovering after {cause} (endpoint state {state})");
        // The command, from the state when it names one, else from the
        // failure itself: a timeout leaves a request queued (Stop), a
        // failure reported by the controller halts the endpoint (Reset).
        // Already Stopped needs neither.
        let from_cause = if matches!(cause, Error::TransferTimeout) {
            TRB_TYPE_STOP_ENDPOINT_CMD
        } else {
            TRB_TYPE_RESET_ENDPOINT_CMD
        };
        let first = match state {
            EP_STATE_HALTED => Some(TRB_TYPE_RESET_ENDPOINT_CMD),
            EP_STATE_RUNNING => Some(TRB_TYPE_STOP_ENDPOINT_CMD),
            EP_STATE_STOPPED => None,
            _ => Some(from_cause),
        };
        if let Some(cmd) = first {
            match self.ep0_command(cmd, slot_id) {
                Ok(()) => {}
                // The endpoint was not in the state read: it moved (a
                // Running endpoint that halted just after the read) or the
                // read was out of date. The other command matches the other
                // state.
                Err(Error::CommandFailed(COMPLETION_CONTEXT_STATE_ERROR)) => {
                    let other = if cmd == TRB_TYPE_RESET_ENDPOINT_CMD {
                        TRB_TYPE_STOP_ENDPOINT_CMD
                    } else {
                        TRB_TYPE_RESET_ENDPOINT_CMD
                    };
                    console::println!("Ouroboros kernel: xhci: slot {slot_id}: EP0 not in the state read, sending the other command");
                    if let Err(e) = self.ep0_command(other, slot_id) {
                        console::println!("Ouroboros kernel: xhci: slot {slot_id}: EP0 recovery command failed ({e})");
                    }
                }
                // The controller is not answering commands: give up rather
                // than pay another full timeout for Set TR Dequeue, on every
                // failure (a hub's settle loop would otherwise take about
                // three times as long per unreadable port).
                Err(Error::CommandTimeout) => {
                    console::println!("Ouroboros kernel: xhci: slot {slot_id}: EP0 recovery abandoned (controller not answering commands)");
                    return;
                }
                Err(e) => console::println!("Ouroboros kernel: xhci: slot {slot_id}: EP0 recovery command failed ({e})"),
            }
        }
        if let Err(e) = self.set_tr_dequeue(EP0_RINGS[idx].0.get() as u64, enqueue, cycle, 1, slot_id) {
            console::println!("Ouroboros kernel: xhci: slot {slot_id}: EP0 Set TR Dequeue failed ({e})");
        }
    }

    /// Set TR Dequeue Pointer for endpoint `dci` of `slot_id`, to slot
    /// `enqueue` of the ring at `ring_addr` with DCS `cycle`: the producer's
    /// current position, so the controller resumes where the next TRB will
    /// be written. Shared by `recover_ep0` and `reset_storage_endpoint`;
    /// see `recover_ep0`'s doc comment for why the current position.
    fn set_tr_dequeue(&mut self, ring_addr: u64, enqueue: usize, cycle: bool, dci: u32, slot_id: u32) -> Result<(), Error> {
        let dequeue = ring_addr + (enqueue * size_of::<Trb>()) as u64;
        let ptr = self.push_command([
            (dequeue as u32) | (cycle as u32), // DCS = the cycle the next TRB will carry
            (dequeue >> 32) as u32,
            0,
            (TRB_TYPE_SET_TR_DEQUEUE_CMD << 10) | (dci << 16) | (slot_id << 24),
        ]);
        self.wait_command_completion(ptr).map(|_| ())
    }

    /// Issues Reset Endpoint or Stop Endpoint (`cmd`) for EP0 of `slot_id`
    /// and waits for it.
    fn ep0_command(&mut self, cmd: u32, slot_id: u32) -> Result<(), Error> {
        let ptr = self.push_command([0, 0, 0, (cmd << 10) | (1 << 16) | (slot_id << 24)]);
        self.wait_command_completion(ptr).map(|_| ())
    }

    /// Recovers a *bulk* endpoint on the storage device after a Stall/halt
    /// (or a failed transfer more generally) - the BOT "reset recovery"
    /// primitive `usb_msd.rs` invokes between retries, and the fix for the
    /// real-Parallels symptom where storage reads "degrade to I/O errors
    /// shortly after mount": once the keyboard's interrupt endpoint is armed
    /// on the same shared xHCI controller, a bulk transfer can eventually
    /// stall, and with no recovery the halted endpoint stays halted so every
    /// later command fails permanently.
    ///
    /// A Reset Endpoint clears the endpoint's Halted state, CLEAR_FEATURE
    /// (ENDPOINT_HALT) clears the device's end of it, then a Set TR
    /// Dequeue Pointer moves hardware's dequeue to the current enqueue
    /// position, past the failed transfer. The first and last are controller
    /// *commands*, not device *class requests* - so they work on Parallels,
    /// whose passthrough doesn't forward class requests (see
    /// `control_transfer`'s doc comment). CLEAR_FEATURE is a *standard*
    /// request, the kind Parallels does forward (`GET_DESCRIPTOR`,
    /// `SET_CONFIGURATION`), but it has not been run there.
    ///
    /// **Why the current position, not the ring's start** (the version
    /// this replaced rewound to the start): the slots after the start still
    /// hold earlier TRBs with the cycle bit the controller then expects, so
    /// it could run on into a stale Normal TRB, a DMA into an old buffer,
    /// once the new ones were done. At the current enqueue position
    /// everything ahead carries the previous lap's cycle bit, so the
    /// controller stops where the next transfer ends. The software ring
    /// state is left as it is; the two agree by construction. The same
    /// reasoning as `recover_ep0`'s.
    ///
    /// **Set TR Dequeue is sent after Reset Endpoint succeeds, and also
    /// after it fails with Context State Error.** That error means the
    /// endpoint was not Halted: either it is Running (the healthy direction,
    /// which the caller resets too, or a pure timeout), where Set TR Dequeue
    /// is refused the same way and changes nothing; or it is Stopped, left
    /// so by an earlier recovery whose Reset Endpoint took and whose Set TR
    /// Dequeue did not, where only Set TR Dequeue can move the dequeue
    /// pointer off the failed transfer. Without it, that endpoint would
    /// restart on the failed transfer at every later doorbell. Any other
    /// failure returns `Err` without it. A pure timeout is still not
    /// recovered (the endpoint stays Running, its transfer queued).
    fn reset_storage_endpoint(&mut self, dir_in: bool) -> Result<(), Error> {
        let (slot_idx, dci, enqueue, cycle) = {
            let st = self.storage.as_ref().ok_or(Error::NoPortConnected)?;
            if dir_in {
                (st.slot, st.in_dci, st.in_enqueue, st.in_cycle)
            } else {
                (st.slot, st.out_dci, st.out_enqueue, st.out_cycle)
            }
        };
        let slot_id = self.slots[slot_idx].as_ref().ok_or(Error::NoPortConnected)?.slot_id;
        let ring_addr = if dir_in { BULK_IN_RING.0.get() } else { BULK_OUT_RING.0.get() } as u64;

        let reset_ep = self.push_command([0, 0, 0, (TRB_TYPE_RESET_ENDPOINT_CMD << 10) | (dci << 16) | (slot_id << 24)]);
        match self.wait_command_completion(reset_ep) {
            Ok(_) => {
                // The host endpoint was Halted and Reset Endpoint (sent
                // without TSP) has put the host's data toggle back to DATA0.
                // CLEAR_FEATURE(ENDPOINT_HALT) (USB 2.0 9.4.5) puts the
                // device's toggle there too, and clears the device's halt
                // when a Stall caused this one; after a Babble or a
                // Transaction Error the device was never halted, and the
                // request then only realigns the toggles. Only here: sent
                // where Reset Endpoint was refused (the endpoint was not
                // Halted), it would reset the device's toggle and not the
                // host's. A Reset Endpoint with TSP=1 would keep the host's
                // toggle, and this would then be wrong. Best-effort: a
                // refusal is logged (control_transfer recovers EP0) and the
                // host side is still repaired.
                let ep_addr = (dci / 2) as u16 | if dir_in { 0x80 } else { 0 };
                if let Err(e) = self.control_transfer(slot_idx, 0x02, 0x01, 0, ep_addr, &mut [], false) {
                    console::println!("Ouroboros kernel: xhci: storage endpoint {ep_addr:#04x}: CLEAR_FEATURE(ENDPOINT_HALT) failed ({e})");
                }
            }
            Err(Error::CommandFailed(COMPLETION_CONTEXT_STATE_ERROR)) => {}
            Err(e) => return Err(e),
        }
        self.set_tr_dequeue(ring_addr, enqueue, cycle, dci, slot_id)
    }

    /// A standard or class control transfer over EP0: Setup Stage (always
    /// Immediate Data - see module doc comment on why 8 bytes always fits
    /// one packet regardless of the real device's Max Packet Size),
    /// optional Data Stage, Status Stage. `data` is read into (device to
    /// host) for `data_in`, otherwise ignored (`data.len()` must be 0 - no
    /// OUT data transfers are needed anywhere this driver calls this).
    ///
    /// **Only ever used for *standard* requests now** (`GET_DESCRIPTOR`,
    /// `SET_CONFIGURATION`) - real Parallels hardware testing confirmed
    /// those get genuine, live data back, while HID *class* requests
    /// (`SET_PROTOCOL`, `GET_REPORT`) came back reading either as this
    /// driver's own Setup packet bytes or stale/cached descriptor data,
    /// never real device state - Parallels' USB passthrough apparently
    /// doesn't forward class requests to the real device at all. See
    /// `INT_RING`'s doc comment for how keyboard *data* actually reaches
    /// this driver now (the interrupt endpoint, configured via a
    /// standard xHCI *command*, not a class *request* - unaffected by
    /// this gap).
    // One over clippy's argument limit purely from the added pool
    // index - the other seven mirror the USB Setup packet's own field
    // structure plus the data buffer, and bundling them into a struct
    // would just rename the same eight things.
    #[allow(clippy::too_many_arguments)]
    fn control_transfer(&mut self, idx: usize, bm_request_type: u8, b_request: u8, w_value: u16, w_index: u16, data: &mut [u8], data_in: bool) -> Result<(), Error> {
        let Some(slot_id) = self.slots[idx].as_ref().map(|s| s.slot_id) else {
            return Err(Error::TransferTimeout);
        };
        let w_length = data.len() as u16;
        let trt: u32 = if w_length == 0 { 0 } else if data_in { 3 } else { 2 };
        let setup_param_lo = (bm_request_type as u32) | ((b_request as u32) << 8) | ((w_value as u32) << 16);
        let setup_param_hi = (w_index as u32) | ((w_length as u32) << 16);
        // The request's TRBs, built first and published together; their
        // addresses are what `wait_transfer_event` matches the completion
        // against.
        let mut trbs: [Trb; MAX_PUBLISH] = [[0; 4]; MAX_PUBLISH];
        let mut td_len = 0usize;
        trbs[td_len] = [
            setup_param_lo,
            setup_param_hi,
            8, // TRB Transfer Length is always 8 - the setup packet itself, not wLength
            (1 << 6) | (TRB_TYPE_SETUP_STAGE << 10) | (trt << 16), // IDT
        ];
        td_len += 1;

        if w_length != 0 {
            let buf_addr = CTRL_BUF.0.get() as u64;
            trbs[td_len] = [
                buf_addr as u32,
                (buf_addr >> 32) as u32,
                w_length as u32,
                (TRB_TYPE_DATA_STAGE << 10) | ((data_in as u32) << 16),
            ];
            td_len += 1;
        }

        // Status stage direction is the opposite of the data stage's; if
        // there was no data stage, it's always IN.
        let status_dir_in = if w_length == 0 { true } else { !data_in };
        trbs[td_len] = [0, 0, 0, (1 << 5) | (TRB_TYPE_STATUS_STAGE << 10) | ((status_dir_in as u32) << 16)]; // IOC
        td_len += 1;

        let td = self.push_ep0(idx, &trbs[..td_len]);
        self.ring_ep0_doorbell(idx);
        if let Err(e) = self.wait_transfer_event(slot_id, &td[..td_len]) {
            self.recover_ep0(idx, &e);
            return Err(e);
        }

        if w_length != 0 && data_in {
            let src = CTRL_BUF.0.get().cast::<u8>();
            for (i, byte) in data.iter_mut().enumerate() {
                *byte = unsafe { read_volatile(src.add(i)) };
            }
        }
        Ok(())
    }

    /// Non-blocking check of the interrupt endpoint's transfer ring - the
    /// real replacement for the original GET_REPORT-control-transfer
    /// design (see `INT_RING`'s doc comment for why). Nothing is sent to
    /// the device here at all: hardware polls the physical keyboard on
    /// its own schedule (per the endpoint's configured interval) and,
    /// when new data arrives, DMAs it into whatever buffer this driver
    /// most recently posted via `repost_interrupt_buffer`, then queues a
    /// Transfer Event - this function's entire job is checking whether
    /// that event has shown up yet.
    ///
    /// Compares against the previously seen report (edge detection: a key
    /// held down reports the same byte pattern every time hardware
    /// re-polls it, and only the *first* report where a given keycode
    /// appears should ever produce a keystroke - no auto-repeat) and
    /// returns the ASCII translation of the first newly-pressed, mapped
    /// key, if any.
    ///
    /// **A real, confirmed bug lived here: a single report can legitimately
    /// contain more than one newly-pressed keycode at once** - most likely
    /// when a poll gets skipped (this task preempted between two hardware
    /// samples, missing an intermediate report - a real, new possibility
    /// once real preemption started working on Parallels, see CLAUDE.md's
    /// "MADT/GICv3" section), but not exclusively: two keys genuinely
    /// pressed within one poll interval can do it too, preemption or not.
    /// The original version of this function returned on the *first*
    /// qualifying keycode in `buf[2..8]` after already recording the whole
    /// report as `last_report` - so any second new keycode in that same
    /// report was silently gone forever: it would never be "new" again on
    /// a later poll, since `last_report` already included it. Confirmed on
    /// real Parallels hardware: `uptime` typed via a scripted test
    /// occasionally arrived at the shell one character short (e.g.
    /// `uptme`), even though the xHCI debug log below showed every
    /// expected report, including the dropped character's own. Fixed by
    /// draining every qualifying keycode from a report into `pending`
    /// (bounded at 5 - `buf[2..8]` has at most 6 slots, and the first
    /// match is always returned immediately rather than queued) instead of
    /// discarding everything past the first.
    fn poll_key(&mut self) -> Option<(u8, bool)> {
        // Drain any keycodes still queued from the last processed report
        // before touching the event ring at all.
        if let Some(k) = self.pop_pending_key() {
            return Some(k);
        }
        let (kb_slot_id, kb_dci) = {
            let kb = self.keyboard.as_ref()?;
            let slot = self.slots[kb.slot].as_ref()?;
            (slot.slot_id, kb.int_dci)
        };

        let event = self.event_ring_pop()?;
        let trb_type = (event[3] >> 10) & 0x3f;
        if trb_type == TRB_TYPE_PORT_STATUS_CHANGE_EVENT {
            return None; // not expected post-setup, but harmless if it happens
        }
        if trb_type != TRB_TYPE_TRANSFER_EVENT {
            console::println!("Ouroboros kernel: xhci: unexpected event type={trb_type} on interrupt poll");
            return None;
        }
        // With more than one device concurrently addressed, a transfer
        // event is only a keyboard report if its slot ID (word 3 bits
        // 31:24) and endpoint DCI (bits 20:16) both match the keyboard's
        // - routing, not luck. (Bulk-transfer completions never appear
        // here in practice - storage commands consume their own events
        // synchronously inside wait_transfer_event - so a mismatch is
        // still logged as genuinely unexpected.)
        let event_slot = event[3] >> 24;
        let event_dci = (event[3] >> 16) & 0x1f;
        if event_slot != kb_slot_id || event_dci != kb_dci {
            console::println!("Ouroboros kernel: xhci: transfer event for slot {event_slot} DCI {event_dci} on interrupt poll (keyboard is slot {kb_slot_id} DCI {kb_dci}), ignoring");
            return None;
        }

        let code = event[2] >> 24;
        self.process_keyboard_report(code);
        self.pop_pending_key()
    }

    /// One synchronous bulk transfer for the storage device: a single
    /// Normal TRB (IOC set) on the IN or OUT ring, doorbell, then wait
    /// for this slot's transfer event - `usb_msd.rs`'s only way of
    /// moving bytes. A keystroke arriving mid-transfer is routed to the
    /// keyboard handler by `wait_transfer_event`, not lost. Short
    /// Packet completions count as success (a device-terminated IN
    /// transfer is normal - e.g. a 36-byte INQUIRY answered with less).
    fn bulk_transfer(&mut self, dir_in: bool, buf_addr: u64, len: u32) -> Result<(), Error> {
        let Some(st) = self.storage.as_mut() else {
            return Err(Error::NoPortConnected);
        };
        let slot_idx = st.slot;
        let (dci, ring_ptr, ring_len) = if dir_in {
            (st.in_dci, BULK_IN_RING.0.get().cast::<Trb>(), INT_RING_SIZE)
        } else {
            (st.out_dci, BULK_OUT_RING.0.get().cast::<Trb>(), INT_RING_SIZE)
        };
        let (enqueue, cycle) = if dir_in {
            (&mut st.in_enqueue, &mut st.in_cycle)
        } else {
            (&mut st.out_enqueue, &mut st.out_cycle)
        };
        let trb_addr = unsafe {
            Self::ring_push(
                ring_ptr,
                ring_len,
                enqueue,
                cycle,
                [buf_addr as u32, (buf_addr >> 32) as u32, len, (1 << 5) | (TRB_TYPE_NORMAL << 10)], // IOC
            )
        };
        let Some(slot_id) = self.slots[slot_idx].as_ref().map(|s| s.slot_id) else {
            return Err(Error::NoPortConnected);
        };
        unsafe { reg::doorbell(self.db_base, slot_id).write(dci) };
        self.wait_transfer_event(slot_id, &[trb_addr]).map(|_| ())
    }

    /// Pops the oldest queued byte, if any, and whether it is the last of
    /// its key.
    fn pop_pending_key(&mut self) -> Option<(u8, bool)> {
        let kb = self.keyboard.as_mut()?;
        if kb.pending_len == 0 {
            return None;
        }
        let byte = kb.pending[0];
        let last = kb.pending_last[0];
        kb.pending.copy_within(1..kb.pending_len, 0);
        kb.pending_last.copy_within(1..kb.pending_len, 0);
        kb.pending_len -= 1;
        Some((byte, last))
    }

    /// The keyboard-report half of the old `poll_key`, factored out so
    /// `wait_transfer_event` can route a keyboard event arriving *during
    /// a bulk transfer* through the exact same logic instead of dropping
    /// it (which would also have left the interrupt buffer unreposted -
    /// a permanently dead keyboard the first time someone typed during
    /// disk I/O). Handles the completion code (transition-logged errors,
    /// re-arm), reads the report, re-arms the endpoint, and queues every
    /// newly-pressed mapped key into `pending` - callers pop from there.
    fn process_keyboard_report(&mut self, code: u32) {
        if code != COMPLETION_SUCCESS && code != COMPLETION_SHORT_PACKET {
            // Logged only on the ok->error transition - see `had_error`'s
            // doc comment for why (an unconditional per-event version of
            // this flooded the screen during earlier debugging).
            let newly_failed = match self.keyboard.as_mut() {
                Some(kb) => {
                    let newly = !kb.had_error;
                    kb.had_error = true;
                    newly
                }
                None => false,
            };
            if newly_failed {
                console::println!("Ouroboros kernel: xhci: interrupt endpoint error (completion code {code})");
            }
            self.repost_interrupt_buffer(); // keep the ring armed even after an error
            return;
        }
        if let Some(kb) = self.keyboard.as_mut() {
            if kb.had_error {
                console::println!("Ouroboros kernel: xhci: interrupt endpoint recovered");
                kb.had_error = false;
            }
        }

        let mut buf: Report = [0; 8];
        let src = INT_BUF.0.get().cast::<u8>();
        for (i, byte) in buf.iter_mut().enumerate() {
            *byte = unsafe { read_volatile(src.add(i)) };
        }

        // Re-arm before doing anything else with the data, so a slow
        // consumer in between doesn't leave the endpoint unable to
        // receive the next report.
        self.repost_interrupt_buffer();

        let Some(kb) = self.keyboard.as_mut() else { return };
        if buf == kb.last_report {
            return;
        }
        let previous = kb.last_report;
        kb.last_report = buf;

        let shift = buf[0] & (MOD_LSHIFT | MOD_RSHIFT) != 0;
        let ctrl = buf[0] & (MOD_LCTRL | MOD_RCTRL) != 0;
        for &keycode in &buf[2..8] {
            if keycode == 0 || keycode < 4 {
                continue; // no key, or error rollover (1-3)
            }
            if previous[2..8].contains(&keycode) {
                continue; // already down last poll - not a new press
            }
            let Some(key) = keycode_to_bytes(keycode, shift, ctrl) else {
                continue;
            };
            // A report holds at most 6 keycodes (buf[2..8]), each at most
            // KEY_BYTES_MAX bytes, and pending is sized for that. It is
            // drained before each event pop in `poll_key`, but not between
            // reports handled inside `wait_transfer_event` during a bulk
            // transfer, so several reports can pile up there; once it is
            // full a further key is dropped, unlogged. A sequence goes in
            // whole or not at all, so the queue never holds half of one;
            // the kernel's keyboard queue (`keyseq::KeyQueue`) relies on
            // that, reading a USB key's rest from here before the serial
            // line, ending the key at the byte `pending_last` marks, and
            // otherwise holding a USB key open far longer than a serial
            // bare Escape (a second: only a keyboard gone mid-report
            // reaches it).
            if kb.pending_len + key.len <= kb.pending.len() {
                let end = kb.pending_len + key.len;
                kb.pending[kb.pending_len..end].copy_from_slice(&key.bytes[..key.len]);
                kb.pending_last[kb.pending_len..end].fill(false);
                kb.pending_last[end - 1] = true;
                kb.pending_len = end;
            }
        }
    }
}

static XHCI: SyncCell<Option<Xhci>> = SyncCell::new(None);

/// Brings up the xHCI controller at `bar_base` (from `pci::discover_xhci`)
/// and, if a device is already connected on some root port, enumerates it
/// as a USB HID boot-protocol keyboard - see module doc comment for the
/// full scope and reasoning. Not fatal if anything here fails: logged and
/// left uninstalled, same "best-effort, log and move on" posture as
/// `main.rs::init_storage`/`try_virtio_console` - a keyboard-less boot is
/// a real, survivable degradation, not a reason to halt.
///
/// # Safety
/// `bar_base`'s containing region must already be mapped (device or RAM)
/// under this kernel's own translation tables - true after
/// `mmu::install_identity_map` has run with it folded into
/// `extra_devices`.
pub unsafe fn init(bar_base: u64) {
    let (pool_base, pool_size) = dma_region();
    console::println!("Ouroboros kernel: xhci: DMA pool @ {pool_base:#x}, {pool_size:#x} bytes");
    match unsafe { init_inner(bar_base) } {
        Ok(()) => console::println!("Ouroboros kernel: xhci: keyboard ready"),
        Err(e) => console::println!("Ouroboros kernel: xhci: keyboard not available ({e})"),
    }
}

/// The boot scan's running result: the first keyboard and storage device
/// found (activated after the scan - see
/// `init_inner`), the hubs still to expand, and the last setup error.
/// One place records a device's outcome ([`Scan::note`]), whether it sat
/// on a root port or behind a hub.
#[derive(Default)]
struct Scan {
    /// Pool entry, and the keyboard's endpoint.
    keyboard: Option<(usize, KeyboardEndpoint)>,
    /// Pool entry, then the `DeviceClass::Storage` fields.
    storage: Option<(usize, u8, u8, u8)>,
    /// Each hub still to expand: its pool entry and its
    /// `bConfigurationValue`.
    hubs: [(usize, u8); MAX_DEVICES],
    hub_count: usize,
    last_error: Option<Error>,
}

impl Scan {
    fn note(
        &mut self,
        xhci: &mut Xhci,
        dcbaa: &mut [u64; MAX_SLOTS_ENABLED + 1],
        idx: usize,
        loc: Location,
        result: Result<DeviceClass, Error>,
    ) {
        match result {
            Ok(DeviceClass::Keyboard(kb)) => {
                if self.keyboard.is_none() {
                    console::println!("Ouroboros kernel: xhci: port {loc}: boot-protocol keyboard - activating after the scan");
                    self.keyboard = Some((idx, kb));
                } else {
                    console::println!("Ouroboros kernel: xhci: port {loc}: a second keyboard - left addressed, not driven (first one wins)");
                }
            }
            Ok(DeviceClass::Storage(ep_in, ep_out, config)) => {
                if self.storage.is_none() {
                    console::println!("Ouroboros kernel: xhci: port {loc}: USB mass storage - activating after the scan");
                    self.storage = Some((idx, ep_in, ep_out, config));
                } else {
                    console::println!("Ouroboros kernel: xhci: port {loc}: a second storage device - left addressed, not driven (first one wins)");
                }
            }
            Ok(DeviceClass::Hub(config_value)) => {
                // NOT bounded by the pool: a hub whose setup fails gives its
                // pool entry back (`release_slot`), and a later hub can take
                // it, so over one scan more hubs can be found than there are
                // entries. `hub_count` counts every hub queued, so the queue
                // is checked, not assumed; a hub past it stays addressed and
                // its devices are not enumerated.
                if self.hub_count == self.hubs.len() {
                    console::println!("Ouroboros kernel: xhci: port {loc}: hub - too many hubs this scan, devices behind it not enumerated");
                    return;
                }
                console::println!("Ouroboros kernel: xhci: port {loc}: hub - its ports are brought up after the root ports");
                self.hubs[self.hub_count] = (idx, config_value);
                self.hub_count += 1;
            }
            Ok(DeviceClass::Other) => {}
            Err(e) => {
                console::println!("Ouroboros kernel: xhci: port {loc} setup failed ({e}), continuing with other ports");
                // Free the pool entry only once the hardware has let go
                // of it. The failed setup may have enabled a slot whose
                // DCBAA entry points at this entry's Output Device
                // Context, and handing that context to another device
                // while the slot lives would be exactly the two-slots-
                // one-context corruption the per-device pools exist to
                // prevent. So the slot is disabled first (`release_slot`);
                // then the entry is `None` again. For a device on a ROOT
                // port, the port is then unowned, so a later `mount -a`
                // rescan can retry it - the recovery path for a stick too
                // slow for the boot scan. A device BEHIND A HUB is not
                // retried: the rescan walks root ports only, and the hub
                // still owns its root port (rescan behind hubs is a
                // ROADMAP.md follow-up). If the controller will not
                // disable the slot, the entry is kept as a tombstone
                // instead: never reused, never retried.
                if !xhci.release_slot(dcbaa, idx) {
                    xhci.slots[idx] = Some(DeviceSlot::tombstone(loc));
                }
                self.last_error = Some(e);
            }
        }
    }
}

unsafe fn init_inner(bar_base: u64) -> Result<(), Error> {
    let (op_base, ir0_base, db_base) = unsafe { reg::locate(bar_base) };
    let hcsparams1 = unsafe { read32(bar_base + CAP_HCSPARAMS1) };
    let hcsparams2 = unsafe { read32(bar_base + CAP_HCSPARAMS2) };
    let hccparams1 = unsafe { read32(bar_base + CAP_HCCPARAMS1) };

    // 32-byte contexts normally, 64-byte if the controller requires it
    // (HCCPARAMS1.CSZ) - a real, common configuration (QEMU's `qemu-xhci`
    // defaults to it), not a rare corner. Every context-layout offset
    // below is computed from this rather than a compile-time constant -
    // see CTX_DWORDS_MAX's doc comment.
    let ctx_dwords: usize = if hccparams1 & HCCPARAMS1_CSZ != 0 { 16 } else { 8 };

    let max_slots = hcsparams1 & 0xff;
    let max_ports = (hcsparams1 >> 24) & 0xff;
    let scratchpad_count = (((hcsparams2 >> 27) & 0x1f) | (((hcsparams2 >> 21) & 0x1f) << 5)) as usize;
    console::println!("Ouroboros kernel: xhci: controller @ {bar_base:#x}, max_slots={max_slots} max_ports={max_ports} scratchpads={scratchpad_count}");

    // The controller as the firmware handed it over, read before anything
    // is waited on or written, so a capture has it however this goes on:
    // whether it is still running (R/S, HCH; xHCI 5.4.1 says HCRST is not
    // to be set while HCHalted is 0, so a running one is halted below
    // first), whether it is ready (CNR), and the USBCMD RsvdP bits the
    // reset write keeps. QEMU reads 0 in every RsvdP field; the board is
    // the only place a kept bit can show. Read only: no probe here
    // performs the operation in question.
    let (usbcmd, usbsts) = unsafe { (read32(op_base.at(OP_USBCMD)), read32(op_base.at(OP_USBSTS))) };
    console::println!(
        "Ouroboros kernel: xhci: as handed over: USBCMD {usbcmd:#x} (R/S {}, RsvdP {:#x}), USBSTS {usbsts:#x} (HCH {}, CNR {})",
        usbcmd & USBCMD_RUN != 0,
        usbcmd & USBCMD_RSVDP,
        usbsts & USBSTS_HCH != 0,
        usbsts & USBSTS_CNR != 0
    );

    // Wait for the controller to report ready before touching anything
    // else, then halt it if it is running and reset it unconditionally - same "don't assume a clean
    // slate" discipline as virtio_blk.rs::Device::init, since UEFI's own
    // xHCI driver (however it found and booted from this same hardware)
    // may have already initialized it.
    if !unsafe { poll_until(|| read32(op_base.at(OP_USBSTS)) & USBSTS_CNR == 0) } {
        return Err(Error::ResetTimeout);
    }
    // xHCI 5.4.1: HCRST is not to be set while HCHalted is 0, so a
    // controller handed over running is stopped first, as Linux's
    // `xhci_halt` does: R/S cleared (USBCMD's interrupt enables with it,
    // which the reset clears anyway), then HCH waited for. After the CNR
    // wait, since no operational register is written before CNR clears.
    // Only when HCH is 0: the Pi's firmware hands the controller over
    // halted (2026-10-04), and there nothing is written here.
    let found_running = unsafe { read32(op_base.at(OP_USBSTS)) } & USBSTS_HCH == 0;
    if found_running {
        unsafe { reg::usbcmd(op_base).write(0) };
        if !unsafe { poll_until(|| read32(op_base.at(OP_USBSTS)) & USBSTS_HCH != 0) } {
            return Err(Error::HaltTimeout(unsafe { read32(op_base.at(OP_USBSTS)) }));
        }
    }
    // HCH again with nothing between the read and the reset write, said
    // after it: a print first would take milliseconds over serial, and a
    // controller halting as it was handed over could finish meanwhile.
    // Beside it, whether the controller was found running: the line says
    // what was read, not that the halt ran, so `found running true` with
    // `HCH true` is a halt done here, and with `HCH false` one that did
    // not happen (shown so by a mutation that skipped it, 2026-10-04).
    let usbsts_at_reset = unsafe { read32(op_base.at(OP_USBSTS)) };
    unsafe { reg::usbcmd(op_base).write(USBCMD_HCRST) };
    console::println!(
        "Ouroboros kernel: xhci: at the reset write: USBSTS {usbsts_at_reset:#x} (HCH {}, found running {found_running})",
        usbsts_at_reset & USBSTS_HCH != 0
    );
    if !unsafe { poll_until(|| read32(op_base.at(OP_USBCMD)) & USBCMD_HCRST == 0 && read32(op_base.at(OP_USBSTS)) & USBSTS_CNR == 0) } {
        return Err(Error::ResetTimeout);
    }
    let (usbcmd_rsvdp, config_rsvdp, crcr_rsvdp, erstsz_rsvdp, erstba_rsvdp) = unsafe {
        (
            reg::usbcmd(op_base).kept_bits(),
            reg::config(op_base).kept_bits(),
            reg::crcr(op_base).kept_bits(),
            reg::erstsz(ir0_base).kept_bits(),
            reg::erstba(ir0_base).kept_bits(),
        )
    };
    console::println!(
        "Ouroboros kernel: xhci: RsvdP after the reset, kept by each write: USBCMD {usbcmd_rsvdp:#x}, CONFIG {config_rsvdp:#x}, CRCR {crcr_rsvdp:#x}, ERSTSZ {erstsz_rsvdp:#x}, ERSTBA {erstba_rsvdp:#x}"
    );

    let slots_enabled = (max_slots as usize).min(MAX_SLOTS_ENABLED) as u32;
    unsafe { reg::config(op_base).write(slots_enabled) };

    if scratchpad_count > MAX_SCRATCHPAD_BUFFERS {
        return Err(Error::TooManyScratchpadBuffers(scratchpad_count as u32));
    }
    // A scratchpad buffer is one controller page, PAGESIZE-sized and
    // -aligned; bit 0 is 4 KB, the size of `Page`. A controller that
    // cannot do 4 KB would write past each one into its neighbour.
    let pagesize = unsafe { read32(op_base.at(OP_PAGESIZE)) };
    if scratchpad_count > 0 && pagesize & 1 == 0 {
        return Err(Error::PageSizeNot4K(pagesize));
    }
    let dcbaa = unsafe { &mut *DCBAA.0.get() };
    if scratchpad_count > 0 {
        let scratchpad_array = unsafe { &mut *SCRATCHPAD_ARRAY.0.get() };
        for i in 0..scratchpad_count {
            scratchpad_array[i] = SCRATCHPAD_PAGES[i].0.get() as u64;
        }
        dcbaa[0] = SCRATCHPAD_ARRAY.0.get() as u64;
    }
    // Each register below hands the controller a structure just written
    // to the DMA pool; the write's barrier (`mmio_write32`/`mmio_write64`)
    // orders those stores first.
    unsafe { reg::dcbaap(op_base).write(DCBAA.0.get() as u64) };

    // Command ring: producer cycle state starts at 1 (RCS=1), the Link
    // TRB (last slot) pre-set to match.
    {
        let ring = unsafe { &mut *COMMAND_RING.0.get() };
        let ring_addr = COMMAND_RING.0.get() as u64;
        ring[CMD_RING_SIZE - 1] = [ring_addr as u32, (ring_addr >> 32) as u32, 0, (1 << 1) | (TRB_TYPE_LINK << 10) | 1]; // TC, cycle=1
    }
    unsafe { reg::crcr(op_base).write((COMMAND_RING.0.get() as u64) | CRCR_RCS) };

    // (Per-device EP0 transfer rings are initialized as each device
    // claims its pool entry during the port scan - see
    // `try_keyboard_on_port` - not up front here.)

    // Event ring: one segment, consumer cycle state starts at 1 too.
    {
        let erst = unsafe { &mut *ERST.0.get() };
        erst[0] = ErstEntry { base: EVENT_RING.0.get() as u64, size: EVENT_RING_SIZE as u32, _reserved: 0 };
    }
    // ERSTSZ, ERDP, then ERSTBA: edk2's `XhcInitSched` order (the firmware
    // that drives the Pi 4's VL805 before us), whose comments give it as
    // the spec's initialization sequence (not checked against the spec
    // itself); U-Boot also sets ERDP first. The controller
    // fetches the ERST when ERSTBA is written, so the dequeue pointer is in
    // place before anything can start on the segment. Until 2026-10-03
    // ERDP came last, as in Linux's `xhci_add_interrupter`, which works on
    // the Pi too; this removes the dependence on the controller not
    // sampling ERDP at the ERSTBA write. ERSTSZ and ERSTBA keep their
    // RsvdP bits; ERDP has none and is written whole: its EHB is RW1C,
    // written 0.
    unsafe { reg::erstsz(ir0_base).write(1) };
    unsafe { reg::erdp(ir0_base).write(EVENT_RING.0.get() as u64) };
    unsafe { reg::erstba(ir0_base).write(ERST.0.get() as u64) };

    // And reads the scratchpad array at Run.
    unsafe { reg::usbcmd(op_base).write(USBCMD_RUN) };
    if !unsafe { poll_until(|| read32(op_base.at(OP_USBSTS)) & USBSTS_HCH == 0) } {
        return Err(Error::StartTimeout);
    }
    // A failed DMA fetch at Run (the ERST, the scratchpad array) sets HSE
    // and halts the controller; said here, not a command timeout later.
    let usbsts = unsafe { read32(op_base.at(OP_USBSTS)) };
    if usbsts & USBSTS_HSE != 0 {
        console::println!("Ouroboros kernel: xhci: WARNING: Host System Error after Run (USBSTS {usbsts:#x})");
    }

    let mut xhci = Xhci {
        db_base,
        ir0_base,
        op_base,
        max_ports,
        ctx_dwords,
        cmd_enqueue: 0,
        cmd_cycle: true,
        evt_dequeue: 0,
        evt_cycle: true,
        timeout_dumped: false,
        slots: [const { None }; MAX_DEVICES],
        keyboard: None,
        storage: None,
    };

    // Port scan: wait for the set of connected ports to *settle*, not just for
    // the first port to connect. No hot-plug - every device this driver will
    // ever consider must already be attached by the time enumeration runs -
    // which is exactly why enumerating too early is fatal: on real Parallels a
    // fast SuperSpeed stick reports connected well before the slower synthetic
    // keyboard settles, and the old "break on the first connected port" then
    // enumerated immediately and missed the keyboard for the whole boot
    // (confirmed by a boot-log capture: with the stick present, only the
    // stick's port showed up, and the scan reported "no boot-protocol keyboard
    // among them"). Debounce instead: poll every port's CCS into a bitmask and
    // proceed only once it's held steady for SCAN_DEBOUNCE_MS (all present
    // devices have shown up), or the SCAN_SETTLE_CAP_MS cap is hit. The
    // keyboard, which settles within ~1s even as the only device, is reliably
    // present by enumerate time this way. (The mask's exact bit positions
    // don't matter - it's only a change detector; `1 << port` for port <=
    // max_ports (<= 255) is masked to the low bits, which is fine here.)
    let hz = crate::timer::frequency_hz();
    let debounce_ticks = hz / 1000 * SCAN_DEBOUNCE_MS;
    let start = crate::timer::now_ticks();
    let min_deadline = start + hz / 1000 * SCAN_MIN_SETTLE_MS;
    let cap = start + hz / 1000 * SCAN_SETTLE_CAP_MS;
    let mut connected_mask = 0u64;
    let mut stable_since = start;
    loop {
        let mut mask = 0u64;
        for port in 1..=max_ports {
            let portsc = reg::portsc(op_base, port);
            if unsafe { portsc.read() } & PORTSC_CCS != 0 {
                mask |= 1u64 << (port % 64);
            }
        }
        let now = crate::timer::now_ticks();
        if mask != connected_mask {
            connected_mask = mask;
            stable_since = now;
        }
        // Proceed once we've waited the minimum *and* the set has held steady
        // for the debounce, or the overall cap is hit (which also covers "no
        // device ever connected" -> the guard below returns NoPortConnected).
        let settled = connected_mask != 0 && now - stable_since >= debounce_ticks;
        if now >= cap || (now >= min_deadline && settled) {
            break;
        }
    }
    if connected_mask == 0 {
        return Err(Error::NoPortConnected);
    }

    // Enumerate *every* connected port (bounded by the device pool), not
    // just until a keyboard turns up - the multi-device change. Each
    // successfully-set-up device *stays* addressed in its own pool slot
    // with its interfaces logged (the old scan abandoned everything that
    // wasn't a boot-protocol keyboard; classifying rather than assuming
    // was itself learned the hard way - a real Parallels VM exposes at
    // least a virtual mouse/tablet over this same controller, and an
    // early version of this driver drove the mouse's report stream
    // believing it was the keyboard). Per-port failures are logged and
    // skipped, not fatal to the scan - one misbehaving device shouldn't
    // cost the keyboard. The keyboard itself is only *activated*
    // (SET_CONFIGURATION through endpoint arming) after the whole scan
    // finishes - see `activate_keyboard`'s doc comment for the real
    // ordering constraint behind that.
    let mut scan = Scan::default();
    for port in 1..=max_ports {
        let portsc = reg::portsc(op_base, port);
        if unsafe { portsc.read() } & PORTSC_CCS == 0 {
            continue;
        }
        let loc = Location::root(port);
        let Some(idx) = xhci.free_entry() else {
            console::println!("Ouroboros kernel: xhci: more connected devices than the {MAX_DEVICES}-entry pool, skipping port {loc}");
            continue;
        };
        let result = unsafe { setup_device_on_port(&mut xhci, dcbaa, idx, port, portsc) };
        scan.note(&mut xhci, dcbaa, idx, loc, result);
    }

    // Hubs, breadth first: each one found (on a root port, or behind
    // another hub) has its ports brought up and the devices on them
    // addressed and classified exactly like root-port devices. Every USB
    // 2.0 device on a Raspberry Pi 4/400 is behind such a hub.
    //
    // ONE PORT AT A TIME, start to finish: a device answers at the default
    // address 0 from its port's reset until Address Device gives it one,
    // and a USB 2.0 hub repeats downstream traffic to every enabled port -
    // so two ports reset before either device is addressed means two
    // devices answering at address 0 at once, and their replies collide.
    // Each port is therefore checked (pool room, tier depth), reset, and
    // its device addressed before the next port is reset; a port whose
    // device cannot be addressed is disabled again, so it cannot answer at
    // address 0 during a later port's turn. QEMU routes by port and cannot
    // show a collision; a real hub with two devices on it can. Linux does
    // the same (`hub_port_init` under `address0_mutex`).
    let mut next_hub = 0usize;
    while next_hub < scan.hub_count {
        let (hub_idx, config_value) = scan.hubs[next_hub];
        next_hub += 1;
        let Some((hub_slot, hub_speed, hub_loc)) = xhci.slots[hub_idx].as_ref().map(|d| (d.slot_id, d.speed, d.loc)) else {
            continue;
        };
        let (connected, count) = match unsafe { configure_hub(&mut xhci, hub_idx, config_value) } {
            Ok(found) => found,
            Err(e) => {
                console::println!("Ouroboros kernel: xhci: port {hub_loc}: hub setup failed ({e}), devices behind it not enumerated");
                // The same rule as any failed device (`Scan::note`):
                // release its slot, or keep the entry if the controller
                // will not let go. Released, its root port is free again,
                // so `mount -a` can retry it - as a root-port device; the
                // rescan does not expand hubs.
                if !xhci.release_slot(dcbaa, hub_idx) {
                    xhci.slots[hub_idx] = Some(DeviceSlot::tombstone(hub_loc));
                }
                scan.last_error = Some(e);
                continue;
            }
        };
        for &hub_port in &connected[..count] {
            // Both refusals come BEFORE the reset, so a skipped port is
            // never left enabled at address 0: the tier check, and a free
            // pool entry found up front. If the reset then fails, the entry
            // simply stays empty: nothing was bound to it in hardware (no
            // Enable Slot ran), so the next port can have it.
            if hub_loc.depth + 1 > 5 {
                console::println!("Ouroboros kernel: xhci: port {hub_loc}.{hub_port}: deeper than USB's five hub tiers, skipping");
                continue;
            }
            let Some(idx) = xhci.free_entry() else {
                console::println!("Ouroboros kernel: xhci: device pool full, skipping port {hub_loc}.{hub_port}");
                continue;
            };
            let speed = match reset_hub_port(&mut xhci, hub_idx, hub_port) {
                Ok(speed) => speed,
                Err(e) => {
                    // One port's failure costs that port, not the hub: the
                    // keyboard on another port must still come up.
                    console::println!("Ouroboros kernel: xhci: port {hub_loc}.{hub_port}: reset failed ({e}), skipping");
                    disable_hub_port(&mut xhci, hub_idx, hub_port);
                    continue;
                }
            };
            let loc = hub_loc.below(hub_slot, hub_speed, hub_port, speed);
            let result = unsafe { address_and_classify(&mut xhci, dcbaa, idx, loc, speed) };
            if result.is_err() {
                disable_hub_port(&mut xhci, hub_idx, hub_port);
            }
            scan.note(&mut xhci, dcbaa, idx, loc, result);
        }
    }
    let Scan { keyboard: keyboard_candidate, storage: storage_candidate, last_error, .. } = scan;

    // Storage first, keyboard last: the keyboard's activation *arms* an
    // endpoint that generates unsolicited events, so it stays the final
    // step (see activate_keyboard's doc comment); storage's bulk
    // endpoints only ever transfer on command, safe to configure here.
    // Storage activation failure is non-fatal to the keyboard.
    if let Some((st_idx, ep_in, ep_out, config)) = storage_candidate {
        if let Err(e) = unsafe { activate_storage(&mut xhci, st_idx, ep_in, ep_out, config) } {
            console::println!("Ouroboros kernel: xhci: storage activation failed ({e}) - continuing without it");
        }
    }

    let kb_result = match keyboard_candidate {
        Some((kb_idx, kb)) => unsafe { activate_keyboard(&mut xhci, kb_idx, kb) },
        None => Err(last_error.unwrap_or(Error::NoKeyboardFound)),
    };

    // Install the controller state if *anything* usable came out of the
    // scan - a storage-only controller (no keyboard) must not lose its
    // activated device just because init's keyboard-shaped result is an
    // error.
    if kb_result.is_ok() || xhci.storage.is_some() {
        unsafe { *XHCI.get() = Some(xhci) };
    }
    kb_result
}

/// Activates the mass-storage device the scan found: `SET_CONFIGURATION`
/// then one Configure Endpoint command adding *both* bulk endpoint
/// contexts, with fresh transfer rings. Unlike the keyboard's interrupt
/// endpoint, bulk endpoints only transfer on command - nothing is armed
/// here, so this can safely run before the keyboard's activation.
///
/// # Safety
/// Same requirements as `init_inner`.
unsafe fn activate_storage(xhci: &mut Xhci, idx: usize, ep_in: u8, ep_out: u8, config_value: u8) -> Result<(), Error> {
    let in_dci = ((ep_in & 0x0f) as u32) * 2 + 1;
    let out_dci = ((ep_out & 0x0f) as u32) * 2;
    let max_dci = in_dci.max(out_dci);
    let Some((speed, slot_id, slot_words)) =
        xhci.slots[idx].as_ref().map(|s| (s.speed, s.slot_id, slot_context(s, max_dci)))
    else {
        return Err(Error::NoPortConnected); // slot vanished - can't happen through the scan's own flow
    };

    xhci.control_transfer(idx, 0x00, 0x09, config_value as u16, 0, &mut [], false)?; // SET_CONFIGURATION

    // Bulk max packet size is fixed by speed: 64 at Full Speed, 512 at
    // High Speed, 1024 at SuperSpeed(+). Both real platforms' storage
    // devices enumerate SuperSpeed (confirmed by the enumeration checks);
    // Full Speed is a stick behind a Full-speed hub (QEMU's `usb-hub`),
    // which this used to declare as 1024. Low Speed has no bulk
    // endpoints. MaxBurst stays 0 (no SuperSpeed endpoint companion
    // parsing - a burst of 1 is always legal for the host to declare).
    let max_packet: u32 = match speed {
        SPEED_FULL => 64,
        SPEED_HIGH => 512,
        _ => 1024,
    };

    {
        let ctx_dwords = xhci.ctx_dwords;
        let ctx = unsafe { &mut *INPUT_CONTEXT.0.get() };
        ctx.fill(0);
        ctx[1] = (1 << 0) | (1 << in_dci) | (1 << out_dci); // A0 | A_in | A_out

        ctx[ctx_dwords..ctx_dwords + 3].copy_from_slice(&slot_words);

        for (dci, ring_addr, ep_type) in [
            (in_dci, BULK_IN_RING.0.get() as u64, EP_TYPE_BULK_IN),
            (out_dci, BULK_OUT_RING.0.get() as u64, EP_TYPE_BULK_OUT),
        ] {
            let off = ctx_dwords * (1 + dci as usize);
            let ep = &mut ctx[off..off + ctx_dwords];
            ep[1] = (3 << 1) | (ep_type << 3) | (max_packet << 16); // CErr=3
            ep[2] = (ring_addr as u32) | 1; // TR Dequeue Pointer | DCS=1
            ep[3] = (ring_addr >> 32) as u32;
            ep[4] = 512; // Average TRB Length - one sector, the only transfer size used
        }
    }

    for ring_cell in [&BULK_IN_RING, &BULK_OUT_RING] {
        let ring = unsafe { &mut *ring_cell.0.get() };
        let ring_addr = ring_cell.0.get() as u64;
        *ring = [[0; 4]; INT_RING_SIZE];
        ring[INT_RING_SIZE - 1] = [ring_addr as u32, (ring_addr >> 32) as u32, 0, (1 << 1) | (TRB_TYPE_LINK << 10) | 1];
    }

    let cmd_ptr = xhci.push_command([
        INPUT_CONTEXT.0.get() as u32,
        (INPUT_CONTEXT.0.get() as u64 >> 32) as u32,
        0,
        (TRB_TYPE_CONFIGURE_ENDPOINT_CMD << 10) | (slot_id << 24),
    ]);
    xhci.wait_command_completion(cmd_ptr)?;

    xhci.storage = Some(StorageState {
        slot: idx,
        in_dci,
        out_dci,
        in_enqueue: 0,
        in_cycle: true,
        out_enqueue: 0,
        out_cycle: true,
    });
    console::println!(
        "Ouroboros kernel: xhci: storage bulk endpoints configured (IN {ep_in:#04x} DCI {in_dci}, OUT {ep_out:#04x} DCI {out_dci})"
    );
    Ok(())
}

/// A boot-protocol keyboard as its descriptors describe it: what
/// [`activate_keyboard`] needs. Named fields rather than a tuple: two of
/// these are `u8`s that a positional tuple would let swap silently.
#[derive(Clone, Copy)]
struct KeyboardEndpoint {
    /// `bInterfaceNumber` of the keyboard interface: `SET_PROTOCOL` is
    /// addressed to it (wIndex), and it is not always 0 on a composite
    /// keyboard.
    interface: u8,
    /// `bEndpointAddress` of the interrupt IN endpoint.
    address: u8,
    /// `wMaxPacketSize` as the descriptor states it, unmasked: bits 10:0
    /// are the packet size, bits 12:11 a High-speed endpoint's extra
    /// transactions per microframe (Max ESIT Payload needs both).
    max_packet: u16,
    /// `bInterval`.
    interval: u8,
    /// `bConfigurationValue`, for `SET_CONFIGURATION`.
    config: u8,
}

/// What the scan found a device to be - decided from its Configuration
/// descriptor after Address Device.
enum DeviceClass {
    /// A boot-protocol keyboard.
    Keyboard(KeyboardEndpoint),
    /// A mass-storage (BOT/SCSI) device: its bulk `(IN, OUT)` endpoint
    /// addresses, then its `bConfigurationValue`.
    Storage(u8, u8, u8),
    /// A hub (`bDeviceClass` 0x09): its ports are brought up and the
    /// devices behind them enumerated after the root-port scan - see
    /// [`configure_hub`].
    Hub(u8),
    /// Anything else - left addressed, no driver.
    Other,
}

/// The most downstream ports [`configure_hub`] brings up on one hub: a
/// Route String holds one 4-bit port number per tier, so 15 is the
/// architectural limit, not a policy.
const MAX_HUB_PORTS: usize = 15;

// USB 2.0 hub class requests (USB 2.0 spec, chapter 11.24) and the
// port-status bits read back from them.
const HUB_DESCRIPTOR_TYPE: u16 = 0x29;
const HUB_REQ_OTHER_OUT: u8 = 0x23; // host-to-device, class, other (a port)
const HUB_REQ_OTHER_IN: u8 = 0xa3;
const HUB_REQ_DEVICE_IN: u8 = 0xa0;
const HUB_GET_STATUS: u8 = 0x00;
const HUB_CLEAR_FEATURE: u8 = 0x01;
const HUB_SET_FEATURE: u8 = 0x03;
const HUB_GET_DESCRIPTOR: u8 = 0x06;
const PORT_FEAT_ENABLE: u16 = 1;
const PORT_FEAT_RESET: u16 = 4;
const PORT_FEAT_POWER: u16 = 8;
const PORT_FEAT_C_CONNECTION: u16 = 16;
const PORT_FEAT_C_RESET: u16 = 20;
const PORT_STAT_CONNECTION: u16 = 1 << 0;
const PORT_STAT_ENABLE: u16 = 1 << 1;
const PORT_STAT_LOW_SPEED: u16 = 1 << 9;
const PORT_STAT_HIGH_SPEED: u16 = 1 << 10;
const PORT_CHANGE_RESET: u16 = 1 << 4;

/// Reset recovery after a port reset completes, before the device is
/// addressed (USB 2.0 7.1.7.5, TRSTRCY).
const HUB_RESET_RECOVERY_MS: u64 = 10;

/// Busy-waits `ms` milliseconds on the generic timer - the same
/// no-interrupts-needed clock as [`poll_deadline`].
fn delay_ms(ms: u64) {
    let until = crate::timer::now_ticks() + crate::timer::frequency_hz() / 1000 * ms;
    while crate::timer::now_ticks() < until {}
}

/// Makes the hub in pool entry `idx` usable and reports which of its
/// ports have something plugged in: `SET_CONFIGURATION(config_value)`, its
/// hub descriptor, a Configure Endpoint that tells the controller this
/// slot is a hub (port count and TT think time - the controller needs them
/// to schedule a slower device's transactions through the hub's
/// transaction translator), then power on every downstream port and wait
/// for the set of connected ports to settle, as the root-port scan does.
/// Returns the connected port numbers, in order. It resets none
/// of them: each is reset by [`reset_hub_port`] only when its turn to be
/// addressed comes (see the hub loop in `init_inner` for why).
///
/// A port whose status cannot be read is logged and skipped; only a
/// failure of the hub itself (configuration, descriptor, the Configure
/// Endpoint, powering a port) fails the hub.
///
/// Everything here is a hub *class* request. Parallels' passthrough
/// drops class requests (see `control_transfer`'s doc comment), but no
/// Parallels device has been a hub; the Raspberry Pi's on-board hub is a
/// physical device, which answers them.
///
/// The hub's status-change interrupt endpoint is never configured: no
/// hot-plug, the same as the root ports. A device plugged into the hub
/// after boot is not seen.
///
/// # Safety
/// Same requirements as `init_inner`.
unsafe fn configure_hub(xhci: &mut Xhci, idx: usize, config_value: u8) -> Result<([u32; MAX_HUB_PORTS], usize), Error> {
    xhci.control_transfer(idx, 0x00, 0x09, config_value as u16, 0, &mut [], false)?; // SET_CONFIGURATION

    // The hub descriptor: bNbrPorts at 2, wHubCharacteristics at 3-4 (TT
    // think time in bits 6:5), bPwrOn2PwrGood at 5 (in 2 ms units). 9
    // bytes covers up to 7 ports; a longer one arrives short-read-safe.
    let mut desc = [0u8; 9];
    xhci.control_transfer(idx, HUB_REQ_DEVICE_IN, HUB_GET_DESCRIPTOR, HUB_DESCRIPTOR_TYPE << 8, 0, &mut desc, true)?;
    let ports = desc[2] as u32;
    if desc[1] != HUB_DESCRIPTOR_TYPE as u8 || ports == 0 || ports as usize > MAX_HUB_PORTS {
        return Err(Error::BadHubDescriptor);
    }
    let characteristics = u16::from_le_bytes([desc[3], desc[4]]);
    let ttt = ((characteristics >> 5) & 0x3) as u32;
    let power_good_ms = desc[5] as u64 * 2;

    // Tell the controller this slot is a hub: a Configure Endpoint that
    // adds only the Slot Context (A0), which is how Hub, Number of Ports
    // and TT Think Time are set (Evaluate Context does not take them).
    // The software record says "hub" only once the controller has
    // accepted it: the Slot Context is built with the hub fields given
    // explicitly (`slot_context_as`), and the record is set after the
    // command succeeds.
    let info = HubInfo { ports, ttt };
    let Some((slot_id, hub_speed, hub_loc, slot_words)) =
        xhci.slots[idx].as_ref().map(|d| (d.slot_id, d.speed, d.loc, slot_context_as(d, Some(info), 1)))
    else {
        return Err(Error::NoPortConnected);
    };
    {
        let ctx_dwords = xhci.ctx_dwords;
        let ctx = unsafe { &mut *INPUT_CONTEXT.0.get() };
        ctx.fill(0);
        ctx[1] = 1 << 0; // A0: the Slot Context only
        ctx[ctx_dwords..ctx_dwords + 3].copy_from_slice(&slot_words);
    }
    let cmd_ptr = xhci.push_command([
        INPUT_CONTEXT.0.get() as u32,
        (INPUT_CONTEXT.0.get() as u64 >> 32) as u32,
        0,
        (TRB_TYPE_CONFIGURE_ENDPOINT_CMD << 10) | (slot_id << 24),
    ]);
    xhci.wait_command_completion(cmd_ptr)?;
    if let Some(dev) = xhci.slots[idx].as_mut() {
        dev.hub = Some(info);
    }
    console::println!("Ouroboros kernel: xhci: port {hub_loc}: hub with {ports} ports (speed={hub_speed}, TT think time {ttt})");

    for port in 1..=ports {
        xhci.control_transfer(idx, HUB_REQ_OTHER_OUT, HUB_SET_FEATURE, PORT_FEAT_POWER, port as u16, &mut [], false)?;
    }
    delay_ms(power_good_ms);

    // Which ports have something plugged in: polled until the set SETTLES,
    // the same rule and the same constants as the root-port scan in
    // `init_inner` (at least SCAN_MIN_SETTLE_MS, then SCAN_DEBOUNCE_MS
    // with no change, at most SCAN_SETTLE_CAP_MS). A single look after the
    // power-good time is how the root-port scan once missed a slow
    // keyboard for a whole boot ("Mode B", see SCAN_MIN_SETTLE_MS), and
    // nothing retries behind a hub later: a slow device missed here is
    // gone until the next boot, which on a Pi 400 would be the keyboard.
    // Unlike the root ports, an empty hub is allowed to settle empty.
    let hz = crate::timer::frequency_hz();
    let start = crate::timer::now_ticks();
    let min_deadline = start + hz / 1000 * SCAN_MIN_SETTLE_MS;
    let cap = start + hz / 1000 * SCAN_SETTLE_CAP_MS;
    let debounce_ticks = hz / 1000 * SCAN_DEBOUNCE_MS;
    let mut mask = 0u16; // bit p = port p connected
    let mut unreadable: u16;
    let mut stable_since = start;
    loop {
        let mut now_mask = 0u16;
        unreadable = 0;
        for port in 1..=ports {
            match hub_port_status(xhci, idx, port) {
                Ok((status, _)) if status & PORT_STAT_CONNECTION != 0 => now_mask |= 1 << port,
                Ok(_) => {}
                Err(_) => unreadable |= 1 << port,
            }
        }
        let now = crate::timer::now_ticks();
        if now_mask != mask {
            mask = now_mask;
            stable_since = now;
        }
        if now >= cap || (now >= min_deadline && now - stable_since >= debounce_ticks) {
            break;
        }
        delay_ms(20);
    }

    let mut connected = [0u32; MAX_HUB_PORTS];
    let mut count = 0usize;
    for port in 1..=ports {
        if unreadable & (1 << port) != 0 {
            console::println!("Ouroboros kernel: xhci: port {hub_loc}.{port}: status unreadable, skipping");
        } else if mask & (1 << port) != 0 {
            let _ = xhci.control_transfer(idx, HUB_REQ_OTHER_OUT, HUB_CLEAR_FEATURE, PORT_FEAT_C_CONNECTION, port as u16, &mut [], false);
            connected[count] = port;
            count += 1;
        }
    }
    Ok((connected, count))
}

/// Resets downstream port `port` of the hub in pool entry `idx` and
/// returns the speed of the device on it, as an xHCI speed ID. From here
/// until Address Device the device answers at address 0, so the caller
/// addresses it before resetting any other port.
fn reset_hub_port(xhci: &mut Xhci, idx: usize, port: u32) -> Result<u32, Error> {
    xhci.control_transfer(idx, HUB_REQ_OTHER_OUT, HUB_SET_FEATURE, PORT_FEAT_RESET, port as u16, &mut [], false)?;
    let deadline = poll_deadline();
    let status = loop {
        let (status, change) = hub_port_status(xhci, idx, port)?;
        if change & PORT_CHANGE_RESET != 0 {
            break status;
        }
        if crate::timer::now_ticks() >= deadline {
            return Err(Error::PortResetTimeout);
        }
        delay_ms(1);
    };
    xhci.control_transfer(idx, HUB_REQ_OTHER_OUT, HUB_CLEAR_FEATURE, PORT_FEAT_C_RESET, port as u16, &mut [], false)?;
    if status & PORT_STAT_ENABLE == 0 {
        return Err(Error::PortNotEnabled);
    }
    delay_ms(HUB_RESET_RECOVERY_MS);
    let speed = if status & PORT_STAT_LOW_SPEED != 0 {
        SPEED_LOW
    } else if status & PORT_STAT_HIGH_SPEED != 0 {
        SPEED_HIGH
    } else {
        SPEED_FULL
    };
    let loc = xhci.slots[idx].as_ref().map(|d| d.loc);
    if let Some(loc) = loc {
        console::println!("Ouroboros kernel: xhci: port {loc}.{port}: device connected, reset, speed={speed}");
    }
    Ok(speed)
}

/// Disables downstream port `port` of the hub in pool entry `idx`, so a
/// device that was reset but never addressed stops answering at address
/// 0. Best effort: nothing better is left to do if the hub refuses.
fn disable_hub_port(xhci: &mut Xhci, idx: usize, port: u32) {
    let _ = xhci.control_transfer(idx, HUB_REQ_OTHER_OUT, HUB_CLEAR_FEATURE, PORT_FEAT_ENABLE, port as u16, &mut [], false);
}

/// `GET_STATUS` on one downstream port of the hub in pool entry `idx`:
/// `(wPortStatus, wPortChange)`.
fn hub_port_status(xhci: &mut Xhci, idx: usize, port: u32) -> Result<(u16, u16), Error> {
    let mut st = [0u8; 4];
    xhci.control_transfer(idx, HUB_REQ_OTHER_IN, HUB_GET_STATUS, 0, port as u16, &mut st, true)?;
    Ok((u16::from_le_bytes([st[0], st[1]]), u16::from_le_bytes([st[2], st[3]])))
}

/// The first five dwords of pool entry `idx`'s EP0 (default control
/// endpoint) context with `max_packet_size`: CErr=3, EP Type=Control, the
/// entry's own ring (`EP0_RINGS[idx]`) with DCS=1, Average TRB Length 8.
/// The one EP0 context builder, for Address Device and for the Evaluate
/// Context that corrects a Full-speed device's packet size - the same
/// one-builder rule as [`slot_context`].
fn ep0_context(idx: usize, max_packet_size: u32) -> [u32; 5] {
    let ring_addr = EP0_RINGS[idx].0.get() as u64;
    [
        0,
        (3 << 1) | (EP_TYPE_CONTROL << 3) | (max_packet_size << 16),
        (ring_addr as u32) | 1, // TR Dequeue Pointer | DCS=1
        (ring_addr >> 32) as u32,
        8, // Average TRB Length
    ]
}

/// PORTSC decoded for a log line: the raw value, then the fields a port's
/// bring-up turns on.
struct Portsc(u32);

impl core::fmt::Display for Portsc {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let v = self.0;
        write!(
            f,
            "{v:#010x} (CCS {}, PED {}, PLS {}, speed {})",
            v & PORTSC_CCS,
            (v & PORTSC_PED) >> 1,
            portsc_pls(v),
            portsc_speed(v)
        )
    }
}

fn portsc_speed(v: u32) -> u32 {
    (v >> PORTSC_SPEED_SHIFT) & PORTSC_SPEED_MASK
}
fn portsc_pls(v: u32) -> u32 {
    (v >> PORTSC_PLS_SHIFT) & PORTSC_PLS_MASK
}
/// Enabled with a speed: what Address Device needs of a root port.
fn portsc_enabled(v: u32) -> bool {
    v & PORTSC_PED != 0 && portsc_speed(v) != 0
}
/// In a link state that a Hot Reset cannot leave (see `PLS_SS_INACTIVE`).
fn portsc_needs_warm_reset(v: u32) -> bool {
    matches!(portsc_pls(v), PLS_SS_INACTIVE | PLS_COMPLIANCE)
}

/// Resets root port `port`, Hot (`PR`, waiting for PRC) or Warm (`WPR`,
/// waiting for WRC and PRC, which a Warm Reset sets both of), then clears
/// the change bits a reset leaves: PRC, WRC and PLC, each only if it is
/// set, so a USB2 port, where WRC is reserved, never has a 1 written
/// there. Returns PORTSC as the reset left it. A timeout is logged with
/// the port's state before it is returned.
///
/// # Safety
/// `portsc` is the port's PORTSC register.
unsafe fn reset_root_port(port: u32, portsc: reg::PortStatus, warm: bool) -> Result<u32, Error> {
    let (set, done, kind) = if warm { (PORTSC_WPR, PORTSC_WRC | PORTSC_PRC, "warm") } else { (PORTSC_PR, PORTSC_PRC, "hot") };
    let current = unsafe { portsc.read() };
    unsafe { portsc.write(current, set, 0) };
    if !unsafe { poll_until(|| portsc.read() & done == done) } {
        let now = unsafe { portsc.read() };
        console::println!("Ouroboros kernel: xhci: port {port}: {kind} reset timed out: {}", Portsc(now));
        return Err(Error::PortResetTimeout);
    }
    let after = unsafe { portsc.read() };
    unsafe { portsc.write(after, 0, after & (PORTSC_PRC | PORTSC_WRC | PORTSC_PLC)) };
    Ok(after)
}

/// Watches root port `port` for up to `POLL_TIMEOUT_MS` until it is
/// enabled with a speed or disconnected, logging each change of its PORTSC (a change,
/// not every read, and at most `MAX_LOGGED` of them, so a flapping link
/// cannot flood the console), and returns the last value read. `when`
/// names the step for the log.
///
/// # Safety
/// `portsc` is the port's PORTSC register.
unsafe fn watch_port_enable(port: u32, portsc: reg::PortStatus, when: &str) -> u32 {
    const MAX_LOGGED: u32 = 16;
    let deadline = poll_deadline();
    let mut last = unsafe { portsc.read() };
    let mut changes = 0u32;
    loop {
        if portsc_enabled(last) {
            console::println!("Ouroboros kernel: xhci: port {port}: enabled {when}: {}", Portsc(last));
            return last;
        }
        if last & PORTSC_CCS == 0 {
            console::println!("Ouroboros kernel: xhci: port {port}: disconnected {when}: {}", Portsc(last));
            return last;
        }
        if crate::timer::now_ticks() >= deadline {
            console::println!("Ouroboros kernel: xhci: port {port}: not enabled {POLL_TIMEOUT_MS} ms {when} ({changes} changes): {}", Portsc(last));
            return last;
        }
        let now = unsafe { portsc.read() };
        if now != last {
            changes += 1;
            if changes <= MAX_LOGGED {
                console::println!("Ouroboros kernel: xhci: port {port}: {when}, now {}", Portsc(now));
            }
            last = now;
        }
    }
}

/// Resets root port `port` and hands the device on it to
/// [`address_and_classify`]. The reset is the only root-port-specific
/// step: a device behind a hub is reset by the hub ([`configure_hub`]).
///
/// # Safety
/// Same requirements as `init_inner` - must run after the controller has
/// been reset and started.
unsafe fn setup_device_on_port(
    xhci: &mut Xhci,
    dcbaa: &mut [u64; MAX_SLOTS_ENABLED + 1],
    idx: usize,
    port: u32,
    portsc: reg::PortStatus,
) -> Result<DeviceClass, Error> {
    console::println!("Ouroboros kernel: xhci: device connected on port {port}");

    // A USB3 port in SS.Inactive or Compliance is Warm Reset at once, as
    // Linux does; any other port gets a Hot Reset. A port that does not
    // come out of its reset enabled with a speed is not addressed. On the
    // Pi 4 a USB3 stick on its SuperSpeed root port came out of a Hot Reset
    // with speed 0, at boot and on a rescan (2026-10-03), with nothing
    // logged to say why. Each step below logs the port's state, so a
    // capture says which one, if any, brought it up: a wait for link
    // training to finish, then, from a state that requires one, a Warm
    // Reset.
    let before = unsafe { portsc.read() };
    let warm_first = portsc_needs_warm_reset(before);
    if warm_first {
        console::println!("Ouroboros kernel: xhci: port {port}: link state {} before the reset, warm reset: {}", portsc_pls(before), Portsc(before));
    }
    let mut current = unsafe { reset_root_port(port, portsc, warm_first)? };
    if !portsc_enabled(current) {
        console::println!(
            "Ouroboros kernel: xhci: port {port}: not enabled after the {} reset; before it {}, after it {}",
            if warm_first { "warm" } else { "hot" },
            Portsc(before),
            Portsc(current)
        );
        current = unsafe { watch_port_enable(port, portsc, "after the reset") };
        if !warm_first && !portsc_enabled(current) && current & PORTSC_CCS != 0 && portsc_needs_warm_reset(current) {
            console::println!("Ouroboros kernel: xhci: port {port}: link state {}, warm reset", portsc_pls(current));
            current = unsafe { reset_root_port(port, portsc, true)? };
            if !portsc_enabled(current) {
                current = unsafe { watch_port_enable(port, portsc, "after the warm reset") };
            }
        }
        if !portsc_enabled(current) {
            return Err(Error::RootPortNotEnabled(current));
        }
    }

    // Default PSIV mapping (no Protocol Speed ID table override present -
    // true for every controller this has been tested against, QEMU's and
    // Parallels' real one alike): 1=Full, 2=Low, 3=High, 4=SuperSpeed,
    // 5=SuperSpeedPlus - the same numbering a Slot Context's Speed field
    // uses, which is why a hub child's speed (from its hub port status)
    // is converted to it too.
    let speed = portsc_speed(current);
    console::println!("Ouroboros kernel: xhci: port {port} reset, speed={speed}");
    unsafe { address_and_classify(xhci, dcbaa, idx, Location::root(port), speed) }
}

/// Brings up the device at `loc`, already reset and running at `speed`
/// (Enable Slot through Address Device, both unconditional - every USB
/// device needs them regardless of what it turns out to be) into pool
/// entry `idx`, reads and logs its Device and Configuration descriptors
/// (every interface's class/subclass/protocol - see [`log_interfaces`]),
/// and classifies it. The device *stays addressed* in its slot whatever
/// it is - the multi-device point of this function; `Err` is a real setup
/// failure.
///
/// Deliberately does **not** send `SET_CONFIGURATION`: descriptors are
/// fully readable in the addressed state (that's how any OS picks a
/// configuration in the first place), and activating a configuration
/// belongs to whatever driver actually drives the device.
///
/// # Safety
/// Same requirements as `init_inner`.
unsafe fn address_and_classify(
    xhci: &mut Xhci,
    dcbaa: &mut [u64; MAX_SLOTS_ENABLED + 1],
    idx: usize,
    loc: Location,
    speed: u32,
) -> Result<DeviceClass, Error> {
    // `ep0_max_packet_size` matches the device's real speed. USB3
    // (SuperSpeed/SuperSpeedPlus) EP0 Max Packet Size is a spec-fixed
    // constant, 512, not merely "any value >= 8 works" the way USB2's is.
    // Confirmed necessary, not hypothetical: the first real Parallels
    // keyboard found was on a SuperSpeed port (speed=4) - real hardware,
    // unlike every QEMU `usb-kbd` test so far, which always attaches as
    // High Speed. High speed is spec-fixed at 64 and Low speed at 8; Full
    // speed may be 8, 16, 32 or 64, so 8 is a first guess corrected from
    // the device's own descriptor below.
    let ep0_max_packet_size: u16 = match speed {
        SPEED_FULL | SPEED_LOW => 8,
        SPEED_HIGH => 64,
        SPEED_SUPER | SPEED_SUPER_PLUS => 512,
        _ => return Err(Error::UnsupportedSpeed(speed)),
    };

    // Enable Slot.
    let cmd_ptr = xhci.push_command([0, 0, 0, TRB_TYPE_ENABLE_SLOT_CMD << 10]);
    let completion = xhci.wait_command_completion(cmd_ptr)?;
    let slot_id = completion[3] >> 24;
    console::println!("Ouroboros kernel: xhci: slot {slot_id} enabled");

    // Claim pool entry `idx` for this device: a fresh EP0 ring (Link TRB
    // rewritten with cycle=1, matching the fresh DeviceSlot ring state
    // below - a reused entry's Link TRB may carry a stale cycle bit from
    // a previous occupant's laps), a zeroed Output Device Context, and
    // its DCBAA entry (which must point at the context before Address
    // Device is issued - the xHC writes the resulting device state
    // there). Per-device rings replaced the old single shared EP0 ring
    // and its per-candidate rewind dance - see `EP0_RINGS`'s doc
    // comment for the history.
    {
        let ring = unsafe { &mut *EP0_RINGS[idx].0.get() };
        let ring_addr = EP0_RINGS[idx].0.get() as u64;
        *ring = [[0; 4]; EP0_RING_SIZE];
        ring[EP0_RING_SIZE - 1] = [ring_addr as u32, (ring_addr >> 32) as u32, 0, (1 << 1) | (TRB_TYPE_LINK << 10) | 1]; // TC, cycle=1
        unsafe { (*OUTPUT_DEVICE_CONTEXTS[idx].0.get()).fill(0) };
    }
    dcbaa[slot_id as usize] = OUTPUT_DEVICE_CONTEXTS[idx].0.get() as u64;
    let dev = DeviceSlot::new(loc, speed, slot_id);
    let slot_words = slot_context(&dev, 1);
    xhci.slots[idx] = Some(dev);

    // Input Context: Input Control Context (A0=slot, A1=EP0) + Slot
    // Context + EP0 Context, laid out using `ctx_dwords` (32- or 64-byte
    // contexts, whichever HCCPARAMS1.CSZ says this controller needs).
    {
        let ctx_dwords = xhci.ctx_dwords;
        let ctx = unsafe { &mut *INPUT_CONTEXT.0.get() };
        ctx.fill(0);
        ctx[1] = (1 << 0) | (1 << 1); // Add Context Flags: A0 | A1
        ctx[ctx_dwords..ctx_dwords + 3].copy_from_slice(&slot_words);

        ctx[2 * ctx_dwords..2 * ctx_dwords + 5].copy_from_slice(&ep0_context(idx, ep0_max_packet_size as u32));
    }

    let cmd_ptr = xhci.push_command([INPUT_CONTEXT.0.get() as u32, (INPUT_CONTEXT.0.get() as u64 >> 32) as u32, 0, (TRB_TYPE_ADDRESS_DEVICE_CMD << 10) | (slot_id << 24)]);
    xhci.wait_command_completion(cmd_ptr)?;
    console::println!("Ouroboros kernel: xhci: slot {slot_id} addressed (port {loc})");

    // Full speed only: learn the real EP0 Max Packet Size before any
    // request longer than 8 bytes. The first 8 bytes of the Device
    // descriptor fit one packet whatever it is, and byte 7 is
    // bMaxPacketSize0; a mismatch is corrected with Evaluate Context. A
    // device answering in packets larger than the declared size is a
    // babble error on real hardware. This was a documented gap while
    // every device seen was High or SuperSpeed; a keyboard behind a hub
    // (QEMU's, and likely the Pi 400's) is the first Full-speed one.
    if speed == SPEED_FULL {
        // Not fatal: a device that refuses this read keeps the guess of 8,
        // which is what every Full-speed device got before this probe
        // existed. (One that refuses it will most likely refuse the
        // 18-byte read below as well, and fail there.)
        let mut head = [0u8; 8];
        let real = match xhci.control_transfer(idx, 0x80, 0x06, 0x0100, 0, &mut head, true) {
            Ok(()) => head[7] as u32,
            Err(e) => {
                console::println!("Ouroboros kernel: xhci: slot {slot_id}: EP0 packet-size probe failed ({e}), keeping {ep0_max_packet_size}");
                ep0_max_packet_size as u32
            }
        };
        if matches!(real, 8 | 16 | 32 | 64) && real != ep0_max_packet_size as u32 {
            let ctx_dwords = xhci.ctx_dwords;
            {
                let ctx = unsafe { &mut *INPUT_CONTEXT.0.get() };
                ctx.fill(0);
                ctx[1] = 1 << 1; // A1: only EP0's Max Packet Size is evaluated
                ctx[2 * ctx_dwords..2 * ctx_dwords + 5].copy_from_slice(&ep0_context(idx, real));
            }
            let cmd_ptr = xhci.push_command([
                INPUT_CONTEXT.0.get() as u32,
                (INPUT_CONTEXT.0.get() as u64 >> 32) as u32,
                0,
                (TRB_TYPE_EVALUATE_CONTEXT_CMD << 10) | (slot_id << 24),
            ]);
            xhci.wait_command_completion(cmd_ptr)?;
            console::println!("Ouroboros kernel: xhci: slot {slot_id}: EP0 max packet size {ep0_max_packet_size} -> {real}");
        }
    }

    // GET_DESCRIPTOR(Device) - informational (logs the real device's
    // vendor/product IDs) and the hub test: a hub says so in
    // bDeviceClass (byte 4), not only in its interface.
    let mut device_desc = [0u8; 18];
    xhci.control_transfer(idx, 0x80, 0x06, 0x0100, 0, &mut device_desc, true)?;
    console::println!("Ouroboros kernel: xhci: GET_DESCRIPTOR(Device) -> {device_desc:02x?}");

    // GET_DESCRIPTOR(Configuration) - a standard request. Requests a
    // generous 64 bytes in one shot rather than the textbook two-step
    // "read 9 bytes for wTotalLength, then re-read that many" dance -
    // this device's full descriptor set (Configuration + Interface + HID
    // + Endpoint, confirmed by direct inspection during earlier debugging)
    // is well under 64 bytes, and a short packet for whatever's left over
    // is normal, expected USB behavior (same reasoning as `CTRL_BUF`'s
    // own doc comment).
    let mut config_desc = [0u8; 64];
    xhci.control_transfer(idx, 0x80, 0x06, 0x0200, 0, &mut config_desc, true)?;

    log_interfaces(loc, &config_desc);

    // bConfigurationValue, the Configuration descriptor's byte 5: what
    // SET_CONFIGURATION must name for whichever driver takes the device
    // (not always 1, USB 2.0 9.6.3).
    let config_value = config_desc[5];
    const CLASS_HUB: u8 = 0x09;
    if device_desc[4] == CLASS_HUB {
        if speed == SPEED_SUPER || speed == SPEED_SUPER_PLUS {
            // A SuperSpeed hub speaks the USB 3 hub protocol (descriptor
            // type 0x2A, SET_HUB_DEPTH, a different port-status layout),
            // which `configure_hub` does not. Refused by name rather than
            // failing on a stalled 0x29 descriptor read. The Raspberry Pi's
            // on-board hub is USB 2.0.
            console::println!("Ouroboros kernel: xhci: port {loc}: SuperSpeed hub - not supported (USB 2.0 hubs only), devices behind it not enumerated");
            return Ok(DeviceClass::Other);
        }
        return Ok(DeviceClass::Hub(config_value));
    }
    if let Some(kb) = find_keyboard_interrupt_endpoint(&config_desc) {
        return Ok(DeviceClass::Keyboard(KeyboardEndpoint { config: config_value, ..kb }));
    }
    if let Some((ep_in, ep_out)) = find_msd_bulk_endpoints(&config_desc) {
        return Ok(DeviceClass::Storage(ep_in, ep_out, config_value));
    }
    // A real device, just neither of the classes this kernel drives - it
    // stays addressed in its slot, ready for whenever a driver exists
    // (the whole point of the multi-device scan).
    console::println!("Ouroboros kernel: xhci: port {loc} device left addressed (no driver for this device class yet)");
    Ok(DeviceClass::Other)
}

/// Walks a raw Configuration descriptor set for a mass-storage
/// interface (`bInterfaceClass=0x08`, subclass `0x06` SCSI-transparent,
/// protocol `0x50` Bulk-Only Transport - exactly what both QEMU's
/// `usb-storage` and the real Parallels-passed-through stick present,
/// confirmed by the enumeration checks) and returns its bulk
/// `(IN, OUT)` endpoint addresses. Same bounded-walk shape as
/// [`find_keyboard_interrupt_endpoint`]/[`log_interfaces`].
fn find_msd_bulk_endpoints(desc: &[u8]) -> Option<(u8, u8)> {
    const DESCRIPTOR_TYPE_INTERFACE: u8 = 4;
    const DESCRIPTOR_TYPE_ENDPOINT: u8 = 5;
    const CLASS_MASS_STORAGE: u8 = 0x08;
    const SUBCLASS_SCSI: u8 = 0x06;
    const PROTOCOL_BOT: u8 = 0x50;
    const ATTR_TYPE_BULK: u8 = 0x02;
    const DIR_IN: u8 = 0x80;

    let mut i = 0usize;
    let mut in_msd_interface = false;
    let mut ep_in: Option<u8> = None;
    let mut ep_out: Option<u8> = None;
    while i + 2 <= desc.len() {
        let b_length = desc[i] as usize;
        if b_length == 0 || i + b_length > desc.len() {
            break;
        }
        let b_type = desc[i + 1];
        if b_type == DESCRIPTOR_TYPE_INTERFACE && b_length >= 9 {
            in_msd_interface = desc[i + 5] == CLASS_MASS_STORAGE
                && desc[i + 6] == SUBCLASS_SCSI
                && desc[i + 7] == PROTOCOL_BOT;
        } else if b_type == DESCRIPTOR_TYPE_ENDPOINT && b_length >= 7 && in_msd_interface {
            let addr = desc[i + 2];
            if desc[i + 3] & 0x03 == ATTR_TYPE_BULK {
                if addr & DIR_IN != 0 {
                    ep_in.get_or_insert(addr);
                } else {
                    ep_out.get_or_insert(addr);
                }
            }
        }
        i += b_length;
    }
    match (ep_in, ep_out) {
        (Some(i), Some(o)) => Some((i, o)),
        _ => None,
    }
}

/// Activates the keyboard the scan found: `SET_CONFIGURATION` +
/// `SET_PROTOCOL`, then Configure Endpoint for its interrupt IN endpoint
/// and the first buffer arm. Runs strictly *after* the whole port scan,
/// by design and not for tidiness: the moment the interrupt endpoint is
/// armed, a keystroke DMAs a report and queues a Transfer Event at any
/// time - which would interleave with a later device's own setup-time
/// command/transfer waits if activation happened mid-scan.
/// `wait_transfer_event`'s slot filter would survive that, but ordering
/// makes it a non-event instead of a recoverable one.
///
/// # Safety
/// Same requirements as `init_inner`.
unsafe fn activate_keyboard(xhci: &mut Xhci, idx: usize, kb: KeyboardEndpoint) -> Result<(), Error> {
    let KeyboardEndpoint { interface, address: endpoint_address, max_packet: raw_max_packet, interval: b_interval, config: config_value } = kb;
    let max_packet_size = raw_max_packet & 0x7ff; // the packet size alone (bits 10:0)
    let endpoint_number = (endpoint_address & 0x0f) as u32;
    let dci = endpoint_number * 2 + 1; // IN direction
    // The Slot Context for Configure Endpoint below (`slot_context`: the
    // same location and speed Address Device used, but Context Entries
    // bumped from 1 to `dci` - required whenever the highest-indexed
    // valid endpoint changes), taken in the same lookup as the speed.
    let Some((speed, slot_id, slot_words)) =
        xhci.slots[idx].as_ref().map(|s| (s.speed, s.slot_id, slot_context(s, dci)))
    else {
        return Err(Error::NoKeyboardFound);
    };

    // SET_CONFIGURATION - a standard request, no data stage, naming the
    // device's own bConfigurationValue (it used to assume 1).
    xhci.control_transfer(idx, 0x00, 0x09, config_value as u16, 0, &mut [], false)?;
    console::println!("Ouroboros kernel: xhci: configuration set");

    // SET_PROTOCOL(Boot Protocol) - a HID *class* request
    // (bmRequestType=0x21 host-to-device/class/interface, bRequest=0x0b),
    // wValue=0 selects Boot Protocol, no data stage. Attempted, but
    // deliberately non-fatal (`?` would abort the whole keyboard setup
    // over this one call) and no longer trusted to have taken effect:
    // real Parallels hardware testing established that HID class
    // requests aren't forwarded to the real device by Parallels' USB
    // passthrough at all (see `control_transfer`'s doc comment) - so this
    // is attempted in case it *does* work on some other platform, but
    // this driver now also copes with the device staying in native
    // Report Protocol mode regardless (see `poll_key`'s handling of
    // whatever byte pattern the interrupt endpoint actually delivers).
    // wIndex is the keyboard's own interface (it used to be 0 always).
    match xhci.control_transfer(idx, 0x21, 0x0b, 0, interface as u16, &mut [], false) {
        Ok(()) => console::println!("Ouroboros kernel: xhci: boot protocol set"),
        Err(e) => console::println!("Ouroboros kernel: xhci: SET_PROTOCOL failed, continuing anyway ({e})"),
    }

    xhci.keyboard = Some(KeyboardState {
        slot: idx,
        int_dci: dci,
        int_enqueue: 0,
        int_cycle: true,
        last_report: [0; 8],
        had_error: false,
        pending: [0; 6 * KEY_BYTES_MAX],
        pending_last: [false; 6 * KEY_BYTES_MAX],
        pending_len: 0,
    });
    console::println!(
        "Ouroboros kernel: xhci: interrupt IN endpoint {endpoint_address:#04x} (DCI {dci}), max_packet_size={max_packet_size}, bInterval={b_interval}"
    );

    // xHCI's Endpoint Context Interval field is always "2^Interval *
    // 125us", for every speed - but the USB descriptor's own bInterval
    // means something different depending on speed: for High/Super/
    // SuperSpeedPlus it's already a 1-based log2 exponent (1-16) per the
    // USB 2.0/3.x specs' own encoding for those speeds, so the xHCI field
    // is just bInterval-1 (0-15); for Low/Full speed it's a direct
    // 1-255ms frame count, needing an actual log2 conversion (1ms = 8 *
    // 125us).
    //
    // The Low/Full conversion rounds DOWN and clamps to 3-10 (1 ms to
    // 128 ms), the range xHCI allows for those speeds: a device must be
    // polled at least as often as it asks, and a value past 10 is a
    // Parameter Error on Configure Endpoint - no keyboard at all. This
    // used to round up and never clamp, which went unnoticed while every
    // keyboard seen was High or SuperSpeed; a keyboard behind a hub (the
    // Pi 400's, and QEMU's `usb-hub`) is the first Full-speed one. Same
    // rule as Linux (`xhci_parse_frame_interval`).
    let interval_field: u32 = if speed == SPEED_FULL || speed == SPEED_LOW {
        let frames = (b_interval as u32).max(1) * 8;
        (31 - frames.leading_zeros()).clamp(3, 10) // floor(log2(frames))
    } else {
        (b_interval as u32).clamp(1, 16) - 1
    };
    console::println!("Ouroboros kernel: xhci: interrupt endpoint Interval field {interval_field} (every {} us)", 125u32 << interval_field);

    // Input Context for Configure Endpoint: Input Control Context
    // (A0=slot, A_dci=this endpoint) + the rebuilt Slot Context from
    // above + the new Endpoint Context itself, laid out with
    // `ctx_dwords`-sized contexts exactly like Address Device's Input
    // Context was.
    {
        let ctx_dwords = xhci.ctx_dwords;
        let ctx = unsafe { &mut *INPUT_CONTEXT.0.get() };
        ctx.fill(0);
        ctx[1] = (1 << 0) | (1 << dci); // Add Context Flags: A0 | A_dci
        ctx[ctx_dwords..ctx_dwords + 3].copy_from_slice(&slot_words);

        let int_ring_addr = INT_RING.0.get() as u64;
        let ep_off = ctx_dwords * (1 + dci as usize);
        let ep = &mut ctx[ep_off..ep_off + ctx_dwords];
        ep[0] = interval_field << 16; // Interval
        ep[1] = (3 << 1) | (EP_TYPE_INTERRUPT_IN << 3) | ((max_packet_size as u32) << 16); // CErr=3, EP Type=Interrupt In
        ep[2] = (int_ring_addr as u32) | 1; // TR Dequeue Pointer | DCS=1
        ep[3] = (int_ring_addr >> 32) as u32;
        // Average TRB Length (a boot-protocol report's real size), and
        // Max ESIT Payload: the most this periodic endpoint moves per
        // service interval, which the controller budgets bandwidth by.
        // For USB 2 that is the packet size times the transactions per
        // microframe (wMaxPacketSize bits 12:11, extra transactions of a
        // High-speed high-bandwidth endpoint; 0 for a keyboard). Left at 0
        // until 2026-09-27: QEMU ignores it, but a controller that honours
        // it could refuse the endpoint or give it no bandwidth, and the
        // Pi's VL805 is untried.
        let esit_payload = max_packet_size as u32 * (((raw_max_packet >> 11) & 0x3) as u32 + 1);
        ep[4] = 8 | (esit_payload << 16);
    }

    // Interrupt transfer ring - same shape as EP0_RING/COMMAND_RING, own
    // independent cycle state.
    {
        let ring = unsafe { &mut *INT_RING.0.get() };
        let ring_addr = INT_RING.0.get() as u64;
        // Zeroed like the EP0 and bulk rings: the keyboard's producer
        // state starts at slot 0 with cycle 1, and a TRB left from an
        // earlier activation could carry cycle 1 and look owned.
        *ring = [[0; 4]; INT_RING_SIZE];
        ring[INT_RING_SIZE - 1] = [ring_addr as u32, (ring_addr >> 32) as u32, 0, (1 << 1) | (TRB_TYPE_LINK << 10) | 1];
    }

    let cmd_ptr = xhci.push_command([
        INPUT_CONTEXT.0.get() as u32,
        (INPUT_CONTEXT.0.get() as u64 >> 32) as u32,
        0,
        (TRB_TYPE_CONFIGURE_ENDPOINT_CMD << 10) | (slot_id << 24),
    ]);
    xhci.wait_command_completion(cmd_ptr)?;
    console::println!("Ouroboros kernel: xhci: interrupt endpoint configured");

    // Arms the endpoint for its first incoming report - see
    // `repost_interrupt_buffer`'s doc comment.
    xhci.repost_interrupt_buffer();

    Ok(())
}

/// Logs every interface descriptor's class/subclass/protocol from a raw
/// Configuration descriptor set - the multi-device scan's classification
/// evidence, printed for *every* enumerated device so a boot screenshot
/// answers "what is this device" directly (the exact question the
/// USB-storage scoping needs answered for a passed-through stick - see
/// `docs/ROADMAP.md`). Mass storage (class 0x08) gets an explicit
/// callout since it's the class the next milestone is waiting to see.
/// Same bounded-walk shape as [`find_keyboard_interrupt_endpoint`], and
/// the same tolerance for a short-arrived buffer.
fn log_interfaces(port: Location, desc: &[u8]) {
    const DESCRIPTOR_TYPE_INTERFACE: u8 = 4;
    const CLASS_MASS_STORAGE: u8 = 0x08;

    let mut i = 0usize;
    while i + 2 <= desc.len() {
        let b_length = desc[i] as usize;
        if b_length == 0 || i + b_length > desc.len() {
            break;
        }
        if desc[i + 1] == DESCRIPTOR_TYPE_INTERFACE && b_length >= 9 {
            let class = desc[i + 5];
            let subclass = desc[i + 6];
            let protocol = desc[i + 7];
            console::println!(
                "Ouroboros kernel: xhci: port {port}: interface class={class:#04x} subclass={subclass:#04x} protocol={protocol:#04x}"
            );
            if class == CLASS_MASS_STORAGE {
                console::println!("Ouroboros kernel: xhci: port {port}: USB mass storage interface - recognized, not driven yet");
            }
        }
        i += b_length;
    }
}

/// Walks a raw Configuration descriptor set (Configuration + Interface +
/// class-specific + Endpoint descriptors, concatenated exactly as
/// `GET_DESCRIPTOR(Configuration)` returns them) looking for an interrupt
/// IN endpoint that belongs to a HID Boot-Protocol-Keyboard interface
/// specifically (`bInterfaceClass=3`, `bInterfaceProtocol=1`) - not just
/// any interrupt IN endpoint. A real, confirmed necessity, not caution
/// for its own sake: a real Parallels VM also exposes at least a virtual
/// mouse/tablet over the same controller, with its own interrupt IN
/// endpoint on a different interface, and an earlier version of this
/// function (which matched the first interrupt IN endpoint found,
/// period) ended up configuring *that* endpoint on a boot where the
/// mouse happened to enumerate on a lower-numbered port than the
/// keyboard. Returns the endpoint with its interface number and its
/// `wMaxPacketSize` unmasked ([`KeyboardEndpoint`]: `activate_keyboard`
/// splits the packet size from the High-speed multiplier bits itself);
/// the configuration value is left for the caller. A malformed
/// or short-arrived buffer just stops the walk early (a zero `bLength` or
/// a length that would run past the end of `desc`) rather than panicking,
/// since `desc` is a fixed-size caller buffer that's genuinely allowed to
/// be only partially filled by a short USB packet.
fn find_keyboard_interrupt_endpoint(desc: &[u8]) -> Option<KeyboardEndpoint> {
    const DESCRIPTOR_TYPE_INTERFACE: u8 = 4;
    const DESCRIPTOR_TYPE_ENDPOINT: u8 = 5;
    const INTERFACE_CLASS_HID: u8 = 3;
    const INTERFACE_PROTOCOL_KEYBOARD: u8 = 1;
    const ENDPOINT_ATTR_TYPE_MASK: u8 = 0x03;
    const ENDPOINT_ATTR_TYPE_INTERRUPT: u8 = 0x03;
    const ENDPOINT_ADDRESS_DIR_IN: u8 = 0x80;

    let mut i = 0usize;
    let mut in_keyboard_interface = false;
    let mut interface = 0u8;
    while i + 2 <= desc.len() {
        let b_length = desc[i] as usize;
        if b_length == 0 || i + b_length > desc.len() {
            break;
        }
        let b_descriptor_type = desc[i + 1];
        if b_descriptor_type == DESCRIPTOR_TYPE_INTERFACE && b_length >= 9 {
            let b_interface_class = desc[i + 5];
            let b_interface_protocol = desc[i + 7];
            in_keyboard_interface = b_interface_class == INTERFACE_CLASS_HID && b_interface_protocol == INTERFACE_PROTOCOL_KEYBOARD;
            interface = desc[i + 2]; // bInterfaceNumber
        } else if b_descriptor_type == DESCRIPTOR_TYPE_ENDPOINT && b_length >= 7 && in_keyboard_interface {
            let b_endpoint_address = desc[i + 2];
            let bm_attributes = desc[i + 3];
            if bm_attributes & ENDPOINT_ATTR_TYPE_MASK == ENDPOINT_ATTR_TYPE_INTERRUPT
                && b_endpoint_address & ENDPOINT_ADDRESS_DIR_IN != 0
            {
                let w_max_packet_size = (desc[i + 4] as u16) | ((desc[i + 5] as u16) << 8);
                let b_interval = desc[i + 6];
                // `config` is not in this descriptor walk's reach; the
                // caller fills it in.
                return Some(KeyboardEndpoint {
                    interface,
                    address: b_endpoint_address,
                    max_packet: w_max_packet_size,
                    interval: b_interval,
                    config: 0,
                });
            }
        }
        i += b_length;
    }
    None
}

unsafe fn poll_until(mut cond: impl FnMut() -> bool) -> bool {
    let deadline = poll_deadline();
    while crate::timer::now_ticks() < deadline {
        if cond() {
            return true;
        }
    }
    false
}

/// Non-blocking: `None` if no keyboard was ever installed (no controller
/// found, or `init` failed - see its doc comment), or one was but nothing
/// new is pressed this poll. Called from `syscall.rs`'s `TRY_READ_CHAR`
/// dispatch arm as a fallback when the byte-stream console has nothing
/// waiting - see that module for why no shell/ABI changes were needed to
/// wire this in. The flag is true on the last byte of a key.
pub fn poll_key() -> Option<(u8, bool)> {
    unsafe { (*XHCI.get()).as_mut() }.and_then(Xhci::poll_key)
}

/// Whether an activated mass-storage device exists - `usb_msd.rs`'s
/// gate, and `main.rs`/`syscall.rs`'s "is there anything to mount".
pub(crate) fn storage_present() -> bool {
    unsafe { (*XHCI.get()).as_ref() }.is_some_and(|x| x.storage.is_some())
}

/// One synchronous bulk transfer on the storage device's IN (`dir_in`)
/// or OUT ring - see [`Xhci::bulk_transfer`]. `usb_msd.rs`'s transport.
pub(crate) fn storage_bulk(dir_in: bool, buf_addr: u64, len: u32) -> Result<(), Error> {
    match unsafe { (*XHCI.get()).as_mut() } {
        Some(x) => x.bulk_transfer(dir_in, buf_addr, len),
        None => Err(Error::NoPortConnected),
    }
}

/// Reset-recover the storage device's IN (`dir_in`) or OUT bulk endpoint
/// after a stalled/failed transfer - [`Xhci::reset_storage_endpoint`]:
/// Reset Endpoint, CLEAR_FEATURE(ENDPOINT_HALT) to the device over EP0 when
/// the endpoint was halted, Set TR Dequeue. `usb_msd.rs` uses it between
/// command retries and to clear a stalled data stage or CSW read in place.
pub(crate) fn storage_reset_endpoint(dir_in: bool) -> Result<(), Error> {
    match unsafe { (*XHCI.get()).as_mut() } {
        Some(x) => x.reset_storage_endpoint(dir_in),
        None => Err(Error::NoPortConnected),
    }
}

/// Runtime port rescan - the `mount` syscall's first half, for devices
/// that attached *after* the boot scan (real Parallels passes USB
/// through a few seconds post-boot, confirmed by the enumeration
/// diagnostics). Walks every connected port not already bound to a
/// pool slot, sets the device up exactly like the boot scan
/// (`setup_device_on_port`), and activates the first mass-storage
/// device found if none is active yet. Keyboards found here are *not*
/// activated (the boot keyboard, if any, stays the one driven -
/// runtime keyboard switchover is out of scope); other classes are
/// left addressed as usual.
pub(crate) fn rescan_ports() {
    let Some(x) = (unsafe { (*XHCI.get()).as_mut() }) else {
        return;
    };
    let dcbaa = unsafe { &mut *DCBAA.0.get() };
    for port in 1..=x.max_ports {
        let portsc = reg::portsc(x.op_base, port);
        if unsafe { portsc.read() } & PORTSC_CCS == 0 {
            continue;
        }
        if x.slots.iter().flatten().any(|s| s.loc.root_port == port) {
            continue; // already owned by the boot scan or a prior rescan (a hub's devices share its root port)
        }
        let Some(idx) = x.free_entry() else {
            console::println!("Ouroboros kernel: xhci: rescan: device pool full, skipping port {port}");
            break;
        };
        match unsafe { setup_device_on_port(x, dcbaa, idx, port, portsc) } {
            Ok(DeviceClass::Storage(ep_in, ep_out, config)) if x.storage.is_none() => {
                console::println!("Ouroboros kernel: xhci: rescan: port {port}: USB mass storage - activating");
                if let Err(e) = unsafe { activate_storage(x, idx, ep_in, ep_out, config) } {
                    console::println!("Ouroboros kernel: xhci: rescan: storage activation failed ({e})");
                }
            }
            Ok(DeviceClass::Keyboard(..)) => {
                console::println!("Ouroboros kernel: xhci: rescan: port {port}: a keyboard - left addressed (rescan only drives storage)");
            }
            Ok(DeviceClass::Hub(_)) => {
                console::println!("Ouroboros kernel: xhci: rescan: port {port}: a hub - left addressed (devices behind a hub are found at boot only)");
            }
            Ok(_) => {}
            Err(e) => {
                console::println!("Ouroboros kernel: xhci: rescan: port {port} setup failed ({e})");
                // The same rule as the boot scan (`Scan::note`): release the
                // slot the failed setup may have enabled, which frees the
                // entry and lets a later `mount -a` try the port again; if
                // the controller will not let go, keep the entry as a
                // tombstone, so the port reads as owned and is never
                // retried into a half-configured device, and the entry is
                // never handed out while hardware may still use it.
                if !x.release_slot(dcbaa, idx) {
                    x.slots[idx] = Some(DeviceSlot::tombstone(Location::root(port)));
                }
            }
        }
    }
}
