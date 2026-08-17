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
//! Every register this module touches lives at an address read directly
//! out of the xHCI controller's own PCI configuration-space BAR
//! (`pci::discover_xhci`, boot-services `PciRootBridgeIo` - the identical,
//! already-proven-safe-on-real-Parallels-hardware mechanism
//! `pci::discover_uart16550`/`log_all_devices` already use). That's a
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
//! One port, one device, one slot, one interrupt IN endpoint. No hot-plug
//! (the device must already be attached at the moment `init` runs), no
//! hubs (route string is always 0 - the device must be directly attached
//! to a root port), no real HID report-descriptor *parsing* (boot
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
//! 8-byte one always would - a real, accepted gap: this driver still
//! skips the standard "read 8 bytes, then Evaluate Context to correct Max
//! Packet Size" dance for USB2 speeds, which means a Low/Full-speed
//! device whose real `bMaxPacketSize0` is smaller than this driver's
//! fixed per-speed guess could genuinely see a multi-packet Babble on
//! `GET_DESCRIPTOR(Configuration)` - untested, since every real device
//! this driver has run against so far has been High Speed (QEMU) or
//! SuperSpeed (Parallels), never Low/Full. USB3 (SuperSpeed/
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
//! setup-time control transfers do recover from a Stall - see
//! `recover_from_stall` - but an interrupt-endpoint Stall just logs and
//! re-arms the ring, without the Reset Endpoint/Set TR Dequeue Pointer
//! sequence real recovery needs); no auto-repeat (a held key reports once
//! per press, not repeatedly, by design - see [`Device::poll_key`]);
//! unmapped keys (function keys, arrows, ...) are silently ignored; only
//! the first of up to 6 simultaneously-pressed keys in a report is ever
//! surfaced; only the *first* interrupt IN endpoint found in the
//! configuration descriptor is ever configured, so a composite
//! keyboard+something-else device with more than one would only get its
//! first endpoint driven.

use core::cell::UnsafeCell;
use core::ptr::{read_volatile, write_volatile};

use crate::console;

type Trb = [u32; 4];

const TRB_TYPE_NORMAL: u32 = 1;
const TRB_TYPE_SETUP_STAGE: u32 = 2;
const TRB_TYPE_DATA_STAGE: u32 = 3;
const TRB_TYPE_STATUS_STAGE: u32 = 4;
const TRB_TYPE_LINK: u32 = 6;
const TRB_TYPE_ENABLE_SLOT_CMD: u32 = 9;
const TRB_TYPE_ADDRESS_DEVICE_CMD: u32 = 11;
const TRB_TYPE_CONFIGURE_ENDPOINT_CMD: u32 = 12;
const TRB_TYPE_RESET_ENDPOINT_CMD: u32 = 14;
const TRB_TYPE_SET_TR_DEQUEUE_CMD: u32 = 16;
const TRB_TYPE_TRANSFER_EVENT: u32 = 32;
const TRB_TYPE_CMD_COMPLETION_EVENT: u32 = 33;
const TRB_TYPE_PORT_STATUS_CHANGE_EVENT: u32 = 34;

const COMPLETION_SUCCESS: u32 = 1;
const COMPLETION_STALL_ERROR: u32 = 6;
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
const OP_CRCR: u64 = 0x18;
const OP_DCBAAP: u64 = 0x30;
const OP_CONFIG: u64 = 0x38;
const OP_PORTSC_BASE: u64 = 0x400; // + (port - 1) * 0x10, port is 1-based

const USBCMD_RUN: u32 = 1 << 0;
const USBCMD_HCRST: u32 = 1 << 1;

const USBSTS_HCH: u32 = 1 << 0;
const USBSTS_CNR: u32 = 1 << 11;

const CRCR_RCS: u64 = 1 << 0;

// PORTSC bits/fields. The RW1C status bits (CSC/PEC/WRC/OCC/PRC/PLC/CEC)
// and PED (R/W, but writing 1 *disables* the port - not a "preserve as
// read" bit) all have to be masked to 0 whenever writing PORTSC for an
// unrelated reason, or they'd be spuriously cleared/tripped. See
// `portsc_preserve` below.
const PORTSC_CCS: u32 = 1 << 0;
const PORTSC_PR: u32 = 1 << 4;
const PORTSC_PRC: u32 = 1 << 21;
const PORTSC_SPEED_SHIFT: u32 = 10;
const PORTSC_SPEED_MASK: u32 = 0xf;
const PORTSC_WRITE_CLEAR_MASK: u32 =
    (1 << 1) | (1 << 17) | (1 << 18) | (1 << 19) | (1 << 20) | (1 << 21) | (1 << 22) | (1 << 23);

// Runtime interrupter register set 0, from `ir0_base` (= BAR base +
// RTSOFF + 0x20 - interrupter register sets start at RTSOFF+0x20, register
// set 0 is always present).
const IR_ERSTSZ: u64 = 0x08;
const IR_ERSTBA: u64 = 0x10;
const IR_ERDP: u64 = 0x18;

const EP_TYPE_CONTROL: u32 = 4;
const EP_TYPE_INTERRUPT_IN: u32 = 7;

const MAX_SLOTS_ENABLED: usize = 8;
const MAX_SCRATCHPAD_BUFFERS: usize = 8;
const CMD_RING_SIZE: usize = 16;
const EP0_RING_SIZE: usize = 16;
const INT_RING_SIZE: usize = 16;
const EVENT_RING_SIZE: usize = 16;

/// How many devices this driver can keep concurrently addressed - the
/// bound on the per-device DMA pools (`EP0_RINGS`,
/// `OUTPUT_DEVICE_CONTEXTS`) and the `Xhci::slots` array, *not* on the
/// controller's own slot count (`MAX_SLOTS_ENABLED`, 8 - hardware
/// assigns slot IDs from its own space; the pool index is this driver's
/// own bookkeeping). 4 covers everything real hardware has shown so
/// far (Parallels' virtual mouse + keyboard) plus a passed-through
/// storage device and one spare. Ports beyond the pool are logged and
/// skipped, not an error.
const MAX_DEVICES: usize = 4;

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
    StartTimeout,
    NoPortConnected,
    NoKeyboardFound,
    PortResetTimeout,
    UnsupportedSpeed(u32),
    TooManyScratchpadBuffers(u32),
    CommandTimeout,
    CommandFailed(u32),
    TransferTimeout,
    TransferFailed(u32),
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::ResetTimeout => write!(f, "controller reset timed out"),
            Error::StartTimeout => write!(f, "controller failed to start (USBSTS.HCH stuck set)"),
            Error::NoPortConnected => write!(f, "no device connected on any root port"),
            Error::NoKeyboardFound => write!(f, "devices found, but no boot-protocol keyboard among them"),
            Error::PortResetTimeout => write!(f, "port reset timed out"),
            Error::UnsupportedSpeed(speed) => write!(f, "unsupported port speed {speed} (only Low/Full/High/SuperSpeed/SuperSpeedPlus are implemented)"),
            Error::TooManyScratchpadBuffers(n) => write!(f, "controller wants {n} scratchpad buffers, only {MAX_SCRATCHPAD_BUFFERS} are supported"),
            Error::CommandTimeout => write!(f, "command ring: timed out waiting for a completion event"),
            Error::CommandFailed(code) => write!(f, "command failed, completion code {code}"),
            Error::TransferTimeout => write!(f, "control transfer: timed out waiting for a transfer event"),
            Error::TransferFailed(code) => write!(f, "control transfer failed, completion code {code}"),
        }
    }
}

#[repr(align(4096))]
struct Page(UnsafeCell<[u8; 4096]>);
unsafe impl Sync for Page {}

#[repr(align(64))]
struct Aligned64<T>(UnsafeCell<T>);
unsafe impl<T> Sync for Aligned64<T> {}

// One static per piece of driver-owned DMA memory, same idiom as
// `virtio_blk.rs`'s DESC_TABLE/AVAIL_RING/USED_RING and `mmu.rs`'s
// `Table` - single-instance, populated once, never touched concurrently
// (this driver has exactly one command/transfer in flight at a time).
static DCBAA: Aligned64<[u64; MAX_SLOTS_ENABLED + 1]> = Aligned64(UnsafeCell::new([0; MAX_SLOTS_ENABLED + 1]));
static SCRATCHPAD_ARRAY: Aligned64<[u64; MAX_SCRATCHPAD_BUFFERS]> = Aligned64(UnsafeCell::new([0; MAX_SCRATCHPAD_BUFFERS]));
static SCRATCHPAD_PAGES: [Page; MAX_SCRATCHPAD_BUFFERS] = [
    Page(UnsafeCell::new([0; 4096])), Page(UnsafeCell::new([0; 4096])),
    Page(UnsafeCell::new([0; 4096])), Page(UnsafeCell::new([0; 4096])),
    Page(UnsafeCell::new([0; 4096])), Page(UnsafeCell::new([0; 4096])),
    Page(UnsafeCell::new([0; 4096])), Page(UnsafeCell::new([0; 4096])),
];
static COMMAND_RING: Aligned64<[Trb; CMD_RING_SIZE]> = Aligned64(UnsafeCell::new([[0; 4]; CMD_RING_SIZE]));
// One EP0 transfer ring per concurrently-addressed device (pool index =
// `Xhci::slots` index). Used to be a single shared ring, with a
// per-candidate-device reset dance: each device's Address Device command
// declares its own EP0 dequeue pointer, so a shared ring only ever
// worked because every non-keyboard device was *abandoned* before the
// next was tried - the ring's software position had to be rewound to
// the base each time to keep matching what the new device's Address
// Device declared. With devices staying concurrently addressed, each
// needs its own ring memory outright.
static EP0_RINGS: [Aligned64<[Trb; EP0_RING_SIZE]>; MAX_DEVICES] = [
    Aligned64(UnsafeCell::new([[0; 4]; EP0_RING_SIZE])),
    Aligned64(UnsafeCell::new([[0; 4]; EP0_RING_SIZE])),
    Aligned64(UnsafeCell::new([[0; 4]; EP0_RING_SIZE])),
    Aligned64(UnsafeCell::new([[0; 4]; EP0_RING_SIZE])),
];
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
static INT_RING: Aligned64<[Trb; INT_RING_SIZE]> = Aligned64(UnsafeCell::new([[0; 4]; INT_RING_SIZE]));
static EVENT_RING: Aligned64<[Trb; EVENT_RING_SIZE]> = Aligned64(UnsafeCell::new([[0; 4]; EVENT_RING_SIZE]));

#[repr(C)]
struct ErstEntry {
    base: u64,
    size: u32,
    _reserved: u32,
}
static ERST: Aligned64<[ErstEntry; 1]> = Aligned64(UnsafeCell::new([ErstEntry { base: 0, size: 0, _reserved: 0 }]));

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
static INPUT_CONTEXT: Aligned64<[u32; CTX_DWORDS_MAX * 33]> = Aligned64(UnsafeCell::new([0; CTX_DWORDS_MAX * 33]));
// Output Device Contexts: same 32-entry shape as the Input Context,
// minus the Input Control Context - this is what the xHC itself writes
// each device's state into, via the DCBAA entry for that device's slot
// ID. One per concurrently-addressed device (pool index = `Xhci::slots`
// index): two live slots pointing their DCBAA entries at one shared
// context would have hardware writing both devices' state into the same
// memory - real corruption, not a bookkeeping nicety - which is why the
// old single `OUTPUT_DEVICE_CONTEXT` only ever worked while this driver
// abandoned every device but the keyboard.
static OUTPUT_DEVICE_CONTEXTS: [Aligned64<[u32; CTX_DWORDS_MAX * 32]>; MAX_DEVICES] = [
    Aligned64(UnsafeCell::new([0; CTX_DWORDS_MAX * 32])),
    Aligned64(UnsafeCell::new([0; CTX_DWORDS_MAX * 32])),
    Aligned64(UnsafeCell::new([0; CTX_DWORDS_MAX * 32])),
    Aligned64(UnsafeCell::new([0; CTX_DWORDS_MAX * 32])),
];

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
static CTRL_BUF: Aligned64<[u8; 64]> = Aligned64(UnsafeCell::new([0; 64]));

/// DMA target for the interrupt endpoint's incoming HID reports - a real
/// boot-protocol report is always 8 bytes, so this doesn't need
/// `CTRL_BUF`'s "widened past 8" treatment (that was specifically about
/// a control-transfer/Setup-packet aliasing question that doesn't apply
/// to a Normal TRB, which has no Setup stage at all).
static INT_BUF: Aligned64<[u8; 8]> = Aligned64(UnsafeCell::new([0; 8]));

unsafe fn read32(addr: u64) -> u32 {
    unsafe { read_volatile(addr as *const u32) }
}
unsafe fn write32(addr: u64, val: u32) {
    unsafe { write_volatile(addr as *mut u32, val) }
}
unsafe fn write64(addr: u64, val: u64) {
    unsafe { write_volatile(addr as *mut u64, val) }
}

fn portsc_preserve(current: u32) -> u32 {
    current & !PORTSC_WRITE_CLEAR_MASK
}

/// USB HID boot-protocol keyboard report is 8 bytes: byte 0 = modifier
/// bitmask, byte 1 = reserved, bytes 2-7 = up to 6 simultaneously-pressed
/// key usage IDs (0 = no key, per USB HID Usage Tables page 0x07).
type Report = [u8; 8];

const MOD_LSHIFT: u8 = 1 << 1;
const MOD_RSHIFT: u8 = 1 << 5;

/// USB HID keycode -> ASCII, boot-protocol Usage IDs 0x04-0x38 (letters,
/// digits, and the punctuation/whitespace keys this shell's line editor
/// cares about). `None` for anything unmapped (function keys, arrows,
/// modifiers themselves, ...) - a real, documented gap, not a bug; see
/// module doc comment.
fn keycode_to_ascii(keycode: u8, shift: bool) -> Option<u8> {
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
        0x2c => Some(b' '),  // Space
        0x2d => Some(if shift { b'_' } else { b'-' }),
        0x2e => Some(if shift { b'+' } else { b'=' }),
        0x2f => Some(if shift { b'{' } else { b'[' }),
        0x30 => Some(if shift { b'}' } else { b']' }),
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
    db_base: u64,
    ir0_base: u64,
    /// 8 or 16, from HCCPARAMS1.CSZ - see `CTX_DWORDS_MAX`'s doc comment.
    ctx_dwords: usize,
    cmd_enqueue: usize,
    cmd_cycle: bool,
    evt_dequeue: usize,
    evt_cycle: bool,
    slots: [Option<DeviceSlot>; MAX_DEVICES],
    keyboard: Option<KeyboardState>,
}

/// One concurrently-addressed device: its root port, speed, the slot ID
/// hardware assigned it, and this driver's producer position on its own
/// EP0 transfer ring (`EP0_RINGS[pool index]`).
struct DeviceSlot {
    port: u32,  // 1-based
    speed: u32, // PORTSC's PSIV value - needed again at Configure Endpoint time to rebuild the Slot Context
    slot_id: u32,
    ep0_enqueue: usize,
    ep0_cycle: bool,
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
    /// confirmed dropped-keystroke bug this fixes. Sized 5 (not 6):
    /// `buf[2..8]` holds at most 6 simultaneous keycodes, and the first
    /// qualifying one is always returned immediately rather than queued,
    /// so at most 5 can ever need to wait here.
    pending: [u8; 5],
    pending_len: usize,
}

impl Xhci {
    /// Advances `enqueue`/`cycle` after writing `trb` into `ring` at the
    /// current producer position, wrapping through the ring's fixed Link
    /// TRB (the last slot) when it reaches the end. Returns the physical
    /// address `trb` was written to - callers that need to match a later
    /// Command Completion Event by pointer (see `push_command`) use this.
    ///
    /// The Link TRB's own cycle bit is rewritten on every wrap, not just
    /// set once at ring-init time: it's logically "produced" exactly like
    /// any real TRB each time the ring cycles, and a stale cycle bit there
    /// would make the second (and every further) lap invalid to hardware.
    /// A real, easy-to-miss class of bug in a first xHCI driver - this
    /// only actually matters once a ring wraps more than once, which the
    /// command ring (at most 2-3 commands ever issued) never does in this
    /// driver's own testing, but the EP0 ring (one `poll_key` per shell
    /// loop iteration, indefinitely) certainly will.
    unsafe fn ring_push(ring_ptr: *mut Trb, ring_len: usize, enqueue: &mut usize, cycle: &mut bool, mut trb: Trb) -> u64 {
        trb[3] = (trb[3] & !1) | (*cycle as u32);
        let slot_ptr = unsafe { ring_ptr.add(*enqueue) };
        unsafe { write_volatile(slot_ptr, trb) };
        let addr = slot_ptr as u64;
        *enqueue += 1;
        if *enqueue == ring_len - 1 {
            let link_ptr = unsafe { ring_ptr.add(ring_len - 1) };
            let mut link = unsafe { read_volatile(link_ptr) };
            link[3] = (link[3] & !1) | (*cycle as u32);
            unsafe { write_volatile(link_ptr, link) };
            *cycle = !*cycle;
            *enqueue = 0;
        }
        addr
    }

    fn push_command(&mut self, trb: Trb) -> u64 {
        let ring_ptr = COMMAND_RING.0.get().cast::<Trb>();
        let addr = unsafe { Self::ring_push(ring_ptr, CMD_RING_SIZE, &mut self.cmd_enqueue, &mut self.cmd_cycle, trb) };
        unsafe {
            core::arch::asm!("dsb sy", options(nostack, preserves_flags));
            write32(self.db_base, 0); // doorbell 0, target field unused for the command ring
        }
        addr
    }

    /// `idx` is the device's pool index - its own EP0 ring
    /// (`EP0_RINGS[idx]`) and its own producer position
    /// (`slots[idx].ep0_enqueue`/`ep0_cycle`). A `None` slot is an
    /// internal caller bug; degrading to a no-op (returning a null
    /// pointer no completion will ever match) beats a panic-handler
    /// hang.
    fn push_ep0(&mut self, idx: usize, trb: Trb) -> u64 {
        let ring_ptr = EP0_RINGS[idx].0.get().cast::<Trb>();
        let Some(slot) = self.slots[idx].as_mut() else { return 0 };
        unsafe { Self::ring_push(ring_ptr, EP0_RING_SIZE, &mut slot.ep0_enqueue, &mut slot.ep0_cycle, trb) }
    }

    fn ring_ep0_doorbell(&self, idx: usize) {
        let Some(slot) = self.slots[idx].as_ref() else { return };
        unsafe {
            core::arch::asm!("dsb sy", options(nostack, preserves_flags));
            // Doorbell target 1 = the default control endpoint (EP0).
            write32(self.db_base + (slot.slot_id as u64) * 4, 1);
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
            core::arch::asm!("dsb sy", options(nostack, preserves_flags));
            // Doorbell target = the endpoint's own Device Context Index,
            // per the xHCI spec's doorbell register definition (target
            // values above 1 map 1:1 to DCI - see `db_base`'s other
            // caller, `ring_ep0_doorbell`, for the EP0/DCI=1 case).
            write32(self.db_base + (slot.slot_id as u64) * 4, kb.int_dci);
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
        let trb = unsafe { read_volatile(slot_ptr) };
        if (trb[3] & 1 != 0) != self.evt_cycle {
            return None;
        }
        self.evt_dequeue += 1;
        if self.evt_dequeue == EVENT_RING_SIZE {
            self.evt_dequeue = 0;
            self.evt_cycle = !self.evt_cycle;
        }
        let new_erdp = unsafe { ring_ptr.add(self.evt_dequeue) } as u64;
        unsafe {
            core::arch::asm!("dmb sy", options(nostack, preserves_flags));
            write64(self.ir0_base + IR_ERDP, new_erdp);
        }
        Some(trb)
    }

    /// Polls the event ring for a Command Completion Event whose Command
    /// TRB Pointer matches `cmd_ptr`, skipping (and logging) anything
    /// else - a stray Port Status Change Event around the port-reset step
    /// is expected, not an error.
    fn wait_command_completion(&mut self, cmd_ptr: u64) -> Result<Trb, Error> {
        let deadline = poll_deadline();
        while crate::timer::now_ticks() < deadline {
            let Some(trb) = self.event_ring_pop() else { continue };
            let trb_type = (trb[3] >> 10) & 0x3f;
            if trb_type != TRB_TYPE_CMD_COMPLETION_EVENT {
                if trb_type != TRB_TYPE_PORT_STATUS_CHANGE_EVENT {
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
        Err(Error::CommandTimeout)
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
    fn wait_transfer_event(&mut self, expected_slot_id: u32) -> Result<Trb, Error> {
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
            if event_slot != expected_slot_id {
                console::println!("Ouroboros kernel: xhci: transfer event for slot {event_slot} while waiting on slot {expected_slot_id}, skipping");
                continue;
            }
            let code = trb[2] >> 24;
            if code != COMPLETION_SUCCESS {
                return Err(Error::TransferFailed(code));
            }
            return Ok(trb);
        }
        Err(Error::TransferTimeout)
    }

    /// Recovers EP0 after a Stall completion code on one of the standard
    /// setup-time control transfers (`control_transfer` is no longer
    /// called from the polling path at all - see `INT_RING`'s doc
    /// comment - but a real device can still Stall a one-off
    /// `GET_DESCRIPTOR`/`SET_CONFIGURATION`, and without recovery every
    /// *later* control transfer on EP0 would fail too). Standard xHCI
    /// Stall recovery: a
    /// Reset Endpoint command clears the halt, then a Set TR Dequeue
    /// Pointer command re-synchronizes hardware's dequeue tracking with
    /// this driver's own ring state, which is reset to the ring's start
    /// (`ep0_enqueue = 0`) at the same time - the two must agree, or the
    /// next transfer this driver enqueues would land somewhere hardware
    /// doesn't expect. Best-effort: doesn't fail if either command times
    /// out, since giving up on recovery entirely would be worse than a
    /// best-effort attempt that might not fully succeed.
    fn recover_from_stall(&mut self, idx: usize) {
        console::println!("Ouroboros kernel: xhci: EP0 stalled, recovering");
        let Some(slot_id) = self.slots[idx].as_ref().map(|s| s.slot_id) else { return };
        let reset_ep = self.push_command([0, 0, 0, (TRB_TYPE_RESET_ENDPOINT_CMD << 10) | (1 << 16) | (slot_id << 24)]);
        let _ = self.wait_command_completion(reset_ep);

        if let Some(slot) = self.slots[idx].as_mut() {
            slot.ep0_enqueue = 0;
            slot.ep0_cycle = true;
        }
        let ep0_ring_addr = EP0_RINGS[idx].0.get() as u64;
        let set_dequeue = self.push_command([
            (ep0_ring_addr as u32) | 1, // DCS=1, matching the software reset above
            (ep0_ring_addr >> 32) as u32,
            0,
            (TRB_TYPE_SET_TR_DEQUEUE_CMD << 10) | (1 << 16) | (slot_id << 24),
        ]);
        let _ = self.wait_command_completion(set_dequeue);
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
        self.push_ep0(idx, [
            setup_param_lo,
            setup_param_hi,
            8, // TRB Transfer Length is always 8 - the setup packet itself, not wLength
            (1 << 6) | (TRB_TYPE_SETUP_STAGE << 10) | (trt << 16), // IDT
        ]);

        if w_length != 0 {
            let buf_addr = CTRL_BUF.0.get() as u64;
            self.push_ep0(idx, [
                buf_addr as u32,
                (buf_addr >> 32) as u32,
                w_length as u32,
                (TRB_TYPE_DATA_STAGE << 10) | ((data_in as u32) << 16),
            ]);
        }

        // Status stage direction is the opposite of the data stage's; if
        // there was no data stage, it's always IN.
        let status_dir_in = if w_length == 0 { true } else { !data_in };
        self.push_ep0(idx, [0, 0, 0, (1 << 5) | (TRB_TYPE_STATUS_STAGE << 10) | ((status_dir_in as u32) << 16)]); // IOC

        self.ring_ep0_doorbell(idx);
        if let Err(e) = self.wait_transfer_event(slot_id) {
            if let Error::TransferFailed(COMPLETION_STALL_ERROR) = e {
                self.recover_from_stall(idx);
            }
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
    fn poll_key(&mut self) -> Option<u8> {
        // Drain any keycodes still queued from the last processed report
        // before touching the event ring at all.
        {
            let kb = self.keyboard.as_mut()?;
            if kb.pending_len > 0 {
                let ascii = kb.pending[0];
                kb.pending.copy_within(1..kb.pending_len, 0);
                kb.pending_len -= 1;
                return Some(ascii);
            }
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
        // - routing, not luck. Nothing else generates transfer events
        // today (no other endpoint is ever armed), so a mismatch is
        // logged as genuinely unexpected rather than silently dropped.
        let event_slot = event[3] >> 24;
        let event_dci = (event[3] >> 16) & 0x1f;
        if event_slot != kb_slot_id || event_dci != kb_dci {
            console::println!("Ouroboros kernel: xhci: transfer event for slot {event_slot} DCI {event_dci} on interrupt poll (keyboard is slot {kb_slot_id} DCI {kb_dci}), ignoring");
            return None;
        }

        let code = event[2] >> 24;
        if code != COMPLETION_SUCCESS && code != COMPLETION_SHORT_PACKET {
            // Logged only on the ok->error transition - see `had_error`'s
            // doc comment for why (an unconditional per-event version of
            // this flooded the screen during earlier debugging).
            let newly_failed = {
                let kb = self.keyboard.as_mut()?;
                let newly = !kb.had_error;
                kb.had_error = true;
                newly
            };
            if newly_failed {
                console::println!("Ouroboros kernel: xhci: interrupt endpoint error (completion code {code})");
            }
            self.repost_interrupt_buffer(); // keep the ring armed even after an error
            return None;
        }
        {
            let kb = self.keyboard.as_mut()?;
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
        // shell command in between doesn't leave the endpoint unable to
        // receive the next report.
        self.repost_interrupt_buffer();

        let kb = self.keyboard.as_mut()?;
        if buf == kb.last_report {
            return None;
        }
        let previous = kb.last_report;
        kb.last_report = buf;

        let shift = buf[0] & (MOD_LSHIFT | MOD_RSHIFT) != 0;
        let mut result = None;
        for &keycode in &buf[2..8] {
            if keycode == 0 || keycode < 4 {
                continue; // no key, or error rollover (1-3)
            }
            if previous[2..8].contains(&keycode) {
                continue; // already down last poll - not a new press
            }
            let Some(ascii) = keycode_to_ascii(keycode, shift) else {
                continue;
            };
            if result.is_none() {
                result = Some(ascii);
            } else {
                // Can't overflow: at most 6 keycodes total in buf[2..8],
                // one already consumed by `result`, `pending` sized 5.
                kb.pending[kb.pending_len] = ascii;
                kb.pending_len += 1;
            }
        }
        result
    }
}

struct XhciCell(UnsafeCell<Option<Xhci>>);
unsafe impl Sync for XhciCell {}
static XHCI: XhciCell = XhciCell(UnsafeCell::new(None));

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
    match unsafe { init_inner(bar_base) } {
        Ok(()) => console::println!("Ouroboros kernel: xhci: keyboard ready"),
        Err(e) => console::println!("Ouroboros kernel: xhci: keyboard not available ({e})"),
    }
}

unsafe fn init_inner(bar_base: u64) -> Result<(), Error> {
    let caplength = (unsafe { read32(bar_base) } & 0xff) as u64;
    let op_base = bar_base + caplength;
    let hcsparams1 = unsafe { read32(bar_base + CAP_HCSPARAMS1) };
    let hcsparams2 = unsafe { read32(bar_base + CAP_HCSPARAMS2) };
    let hccparams1 = unsafe { read32(bar_base + CAP_HCCPARAMS1) };
    let dboff = unsafe { read32(bar_base + CAP_DBOFF) } as u64 & !0x3;
    let rtsoff = unsafe { read32(bar_base + CAP_RTSOFF) } as u64 & !0x1f;
    let db_base = bar_base + dboff;
    let ir0_base = bar_base + rtsoff + 0x20;

    // 32-byte contexts normally, 64-byte if the controller requires it
    // (HCCPARAMS1.CSZ) - a real, common configuration (QEMU's `qemu-xhci`
    // defaults to it), not a rare corner. Every context-layout offset
    // below is computed from this rather than a compile-time constant -
    // see CTX_DWORDS_MAX's doc comment.
    let ctx_dwords: usize = if hccparams1 & HCCPARAMS1_CSZ != 0 { 16 } else { 8 };

    let max_slots = hcsparams1 & 0xff;
    let max_ports = (hcsparams1 >> 24) & 0xff;
    console::println!("Ouroboros kernel: xhci: controller @ {bar_base:#x}, max_slots={max_slots} max_ports={max_ports}");

    // Wait for the controller to report ready before touching anything
    // else, then reset it unconditionally - same "don't assume a clean
    // slate" discipline as virtio_blk.rs::Device::init, since UEFI's own
    // xHCI driver (however it found and booted from this same hardware)
    // may have already initialized it.
    if !unsafe { poll_until(|| read32(op_base + OP_USBSTS) & USBSTS_CNR == 0) } {
        return Err(Error::ResetTimeout);
    }
    unsafe { write32(op_base + OP_USBCMD, USBCMD_HCRST) };
    if !unsafe { poll_until(|| read32(op_base + OP_USBCMD) & USBCMD_HCRST == 0 && read32(op_base + OP_USBSTS) & USBSTS_CNR == 0) } {
        return Err(Error::ResetTimeout);
    }

    let slots_enabled = (max_slots as usize).min(MAX_SLOTS_ENABLED) as u32;
    unsafe { write32(op_base + OP_CONFIG, slots_enabled) };

    let scratchpad_count = (((hcsparams2 >> 27) & 0x1f) | (((hcsparams2 >> 21) & 0x1f) << 5)) as usize;
    if scratchpad_count > MAX_SCRATCHPAD_BUFFERS {
        return Err(Error::TooManyScratchpadBuffers(scratchpad_count as u32));
    }
    let dcbaa = unsafe { &mut *DCBAA.0.get() };
    if scratchpad_count > 0 {
        let scratchpad_array = unsafe { &mut *SCRATCHPAD_ARRAY.0.get() };
        for i in 0..scratchpad_count {
            scratchpad_array[i] = SCRATCHPAD_PAGES[i].0.get() as u64;
        }
        dcbaa[0] = SCRATCHPAD_ARRAY.0.get() as u64;
    }
    unsafe { write64(op_base + OP_DCBAAP, DCBAA.0.get() as u64) };

    // Command ring: producer cycle state starts at 1 (RCS=1), the Link
    // TRB (last slot) pre-set to match.
    {
        let ring = unsafe { &mut *COMMAND_RING.0.get() };
        let ring_addr = COMMAND_RING.0.get() as u64;
        ring[CMD_RING_SIZE - 1] = [ring_addr as u32, (ring_addr >> 32) as u32, 0, (1 << 1) | (TRB_TYPE_LINK << 10) | 1]; // TC, cycle=1
    }
    unsafe { write64(op_base + OP_CRCR, (COMMAND_RING.0.get() as u64) | CRCR_RCS) };

    // (Per-device EP0 transfer rings are initialized as each device
    // claims its pool entry during the port scan - see
    // `try_keyboard_on_port` - not up front here.)

    // Event ring: one segment, consumer cycle state starts at 1 too.
    {
        let erst = unsafe { &mut *ERST.0.get() };
        erst[0] = ErstEntry { base: EVENT_RING.0.get() as u64, size: EVENT_RING_SIZE as u32, _reserved: 0 };
    }
    unsafe {
        write32(ir0_base + IR_ERSTSZ, 1);
        write64(ir0_base + IR_ERSTBA, ERST.0.get() as u64);
        write64(ir0_base + IR_ERDP, EVENT_RING.0.get() as u64);
    }

    unsafe { write32(op_base + OP_USBCMD, USBCMD_RUN) };
    if !unsafe { poll_until(|| read32(op_base + OP_USBSTS) & USBSTS_HCH == 0) } {
        return Err(Error::StartTimeout);
    }

    let mut xhci = Xhci {
        db_base,
        ir0_base,
        ctx_dwords,
        cmd_enqueue: 0,
        cmd_cycle: true,
        evt_dequeue: 0,
        evt_cycle: true,
        slots: [None, None, None, None],
        keyboard: None,
    };

    // Port scan: wait for at least one port to report a connected device.
    // No hot-plug - every device this driver will ever consider must
    // already be attached by the time this loop finds it.
    let mut any_connected = false;
    let deadline = poll_deadline();
    'wait: while crate::timer::now_ticks() < deadline {
        for port in 1..=max_ports {
            let portsc_addr = op_base + OP_PORTSC_BASE + ((port - 1) as u64) * 0x10;
            if unsafe { read32(portsc_addr) } & PORTSC_CCS != 0 {
                any_connected = true;
                break 'wait;
            }
        }
    }
    if !any_connected {
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
    let mut next_idx = 0usize;
    let mut keyboard_candidate: Option<(usize, u8, u16, u8)> = None;
    let mut last_error: Option<Error> = None;
    for port in 1..=max_ports {
        let portsc_addr = op_base + OP_PORTSC_BASE + ((port - 1) as u64) * 0x10;
        if unsafe { read32(portsc_addr) } & PORTSC_CCS == 0 {
            continue;
        }
        if next_idx >= MAX_DEVICES {
            console::println!("Ouroboros kernel: xhci: more connected devices than the {MAX_DEVICES}-entry pool, skipping port {port}");
            continue;
        }
        match unsafe { setup_device_on_port(&mut xhci, dcbaa, next_idx, port, portsc_addr) } {
            Ok(candidate) => {
                if let Some((ep, mps, bi)) = candidate {
                    if keyboard_candidate.is_none() {
                        console::println!("Ouroboros kernel: xhci: port {port}: boot-protocol keyboard - activating after the scan");
                        keyboard_candidate = Some((next_idx, ep, mps, bi));
                    } else {
                        console::println!("Ouroboros kernel: xhci: port {port}: a second keyboard - left addressed, not driven (first one wins)");
                    }
                }
                next_idx += 1;
            }
            Err(e) => {
                console::println!("Ouroboros kernel: xhci: port {port} setup failed ({e}), continuing with other ports");
                // Discard the software state, but *sacrifice* the pool
                // entry rather than reuse it: the failed setup may
                // already have bound this entry's Output Device Context
                // into hardware's DCBAA for an enabled slot, and handing
                // the same context to the next device would be exactly
                // the two-slots-one-context corruption the per-device
                // pools exist to prevent.
                xhci.slots[next_idx] = None;
                next_idx += 1;
                last_error = Some(e);
            }
        }
    }

    let Some((kb_idx, ep, mps, bi)) = keyboard_candidate else {
        return Err(last_error.unwrap_or(Error::NoKeyboardFound));
    };
    unsafe { activate_keyboard(&mut xhci, kb_idx, ep, mps, bi)? };

    unsafe { *XHCI.0.get() = Some(xhci) };
    Ok(())
}

/// Brings up whatever device is on `port` (Enable Slot through Address
/// Device, both unconditional - every USB device needs them regardless
/// of what it turns out to be) into pool entry `idx`, reads and logs its
/// Device and Configuration descriptors (every interface's
/// class/subclass/protocol - see [`log_interfaces`]), and classifies it.
/// Returns `Ok(Some((bEndpointAddress, wMaxPacketSize, bInterval)))` of
/// the interrupt IN endpoint if this is a boot-protocol keyboard
/// (activation happens later, post-scan - see [`activate_keyboard`]),
/// `Ok(None)` for any other device (which *stays addressed* in its
/// slot - the multi-device point of this function), or `Err` for a real
/// setup failure.
///
/// Deliberately does **not** send `SET_CONFIGURATION`: descriptors are
/// fully readable in the addressed state (that's how any OS picks a
/// configuration in the first place), and activating a configuration
/// belongs to whatever driver actually drives the device - today only
/// the keyboard's activation step does it.
///
/// # Safety
/// Same requirements as `init_inner` - must run after the controller has
/// been reset and started.
unsafe fn setup_device_on_port(
    xhci: &mut Xhci,
    dcbaa: &mut [u64; MAX_SLOTS_ENABLED + 1],
    idx: usize,
    port: u32,
    portsc_addr: u64,
) -> Result<Option<(u8, u16, u8)>, Error> {
    console::println!("Ouroboros kernel: xhci: device connected on port {port}");

    // Reset the port, then clear the resulting Port Reset Change bit.
    let current = unsafe { read32(portsc_addr) };
    unsafe { write32(portsc_addr, portsc_preserve(current) | PORTSC_PR) };
    if !unsafe { poll_until(|| read32(portsc_addr) & PORTSC_PRC != 0) } {
        return Err(Error::PortResetTimeout);
    }
    let current = unsafe { read32(portsc_addr) };
    unsafe { write32(portsc_addr, portsc_preserve(current) | PORTSC_PRC) };

    let speed = (current >> PORTSC_SPEED_SHIFT) & PORTSC_SPEED_MASK;
    // Default PSIV mapping (no Protocol Speed ID table override present -
    // true for every controller this has been tested against, QEMU's and
    // Parallels' real one alike): 1=Full, 2=Low, 3=High, 4=SuperSpeed,
    // 5=SuperSpeedPlus. `ep0_max_packet_size` matches the port's real
    // speed, not the fixed 8 the module doc comment's reasoning depends
    // on for USB2 (Low/Full/High) - USB3 (SuperSpeed/SuperSpeedPlus) EP0
    // Max Packet Size is a spec-fixed constant, 512, not merely "any
    // value >= 8 works" the way USB2's is. Confirmed necessary, not
    // hypothetical: the first real Parallels keyboard found was on a
    // SuperSpeed port (speed=4) - real hardware, unlike every QEMU
    // `usb-kbd` test so far, which always attaches as High Speed.
    let ep0_max_packet_size: u16 = match speed {
        1 | 2 => 8,
        3 => 64,
        4 | 5 => 512,
        _ => return Err(Error::UnsupportedSpeed(speed)),
    };
    console::println!("Ouroboros kernel: xhci: port {port} reset, speed={speed}");

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
    xhci.slots[idx] = Some(DeviceSlot { port, speed, slot_id, ep0_enqueue: 0, ep0_cycle: true });

    // Input Context: Input Control Context (A0=slot, A1=EP0) + Slot
    // Context + EP0 Context, laid out using `ctx_dwords` (32- or 64-byte
    // contexts, whichever HCCPARAMS1.CSZ says this controller needs).
    {
        let ctx_dwords = xhci.ctx_dwords;
        let ctx = unsafe { &mut *INPUT_CONTEXT.0.get() };
        ctx.fill(0);
        ctx[1] = (1 << 0) | (1 << 1); // Add Context Flags: A0 | A1

        let slot = &mut ctx[ctx_dwords..2 * ctx_dwords];
        slot[0] = (speed << 20) | (1 << 27); // Route String=0, Speed, Context Entries=1
        slot[1] = port << 16; // Root Hub Port Number

        let ep0_ring_addr = EP0_RINGS[idx].0.get() as u64;
        let ep0 = &mut ctx[2 * ctx_dwords..3 * ctx_dwords];
        ep0[1] = (3 << 1) | (EP_TYPE_CONTROL << 3) | ((ep0_max_packet_size as u32) << 16); // CErr=3, EP Type=Control
        ep0[2] = (ep0_ring_addr as u32) | 1; // TR Dequeue Pointer | DCS=1
        ep0[3] = (ep0_ring_addr >> 32) as u32;
        ep0[4] = 8; // Average TRB Length
    }

    let cmd_ptr = xhci.push_command([INPUT_CONTEXT.0.get() as u32, (INPUT_CONTEXT.0.get() as u64 >> 32) as u32, 0, (TRB_TYPE_ADDRESS_DEVICE_CMD << 10) | (slot_id << 24)]);
    xhci.wait_command_completion(cmd_ptr)?;
    console::println!("Ouroboros kernel: xhci: slot {slot_id} addressed");

    // GET_DESCRIPTOR(Device) - informational only (logs the real device's
    // vendor/product IDs), but also serves as a live confirmation that
    // *standard* control requests reach the real device correctly, unlike
    // HID class requests - see `control_transfer`'s doc comment.
    let mut device_desc = [0u8; 18];
    xhci.control_transfer(idx, 0x80, 0x06, 0x0100, 0, &mut device_desc, true)?;
    console::println!("Ouroboros kernel: xhci: GET_DESCRIPTOR(Device) -> {device_desc:02x?}");

    // GET_DESCRIPTOR(Configuration) - a standard request, so (per the
    // above) expected to reach the real device correctly. Requests a
    // generous 64 bytes in one shot rather than the textbook two-step
    // "read 9 bytes for wTotalLength, then re-read that many" dance -
    // this device's full descriptor set (Configuration + Interface + HID
    // + Endpoint, confirmed by direct inspection during earlier debugging)
    // is well under 64 bytes, and a short packet for whatever's left over
    // is normal, expected USB behavior (same reasoning as `CTRL_BUF`'s
    // own doc comment).
    let mut config_desc = [0u8; 64];
    xhci.control_transfer(idx, 0x80, 0x06, 0x0200, 0, &mut config_desc, true)?;

    log_interfaces(port, &config_desc);

    let candidate = find_keyboard_interrupt_endpoint(&config_desc);
    if candidate.is_none() {
        // A real device, just not a boot-protocol keyboard - it stays
        // addressed in its slot, ready for whenever a driver for its
        // class exists (the whole point of the multi-device scan).
        console::println!("Ouroboros kernel: xhci: port {port} device left addressed (no driver for this device class yet)");
    }
    Ok(candidate)
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
unsafe fn activate_keyboard(
    xhci: &mut Xhci,
    idx: usize,
    endpoint_address: u8,
    max_packet_size: u16,
    b_interval: u8,
) -> Result<(), Error> {
    let Some((port, speed, slot_id)) = xhci.slots[idx].as_ref().map(|s| (s.port, s.speed, s.slot_id)) else {
        return Err(Error::NoKeyboardFound);
    };

    // SET_CONFIGURATION(1) - a standard request, no data stage. Assumes
    // configuration value 1, universal for a device this simple (see
    // module doc comment on scope).
    xhci.control_transfer(idx, 0x00, 0x09, 1, 0, &mut [], false)?;
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
    match xhci.control_transfer(idx, 0x21, 0x0b, 0, 0, &mut [], false) {
        Ok(()) => console::println!("Ouroboros kernel: xhci: boot protocol set"),
        Err(e) => console::println!("Ouroboros kernel: xhci: SET_PROTOCOL failed, continuing anyway ({e})"),
    }

    let endpoint_number = (endpoint_address & 0x0f) as u32;
    let dci = endpoint_number * 2 + 1; // IN direction
    xhci.keyboard = Some(KeyboardState {
        slot: idx,
        int_dci: dci,
        int_enqueue: 0,
        int_cycle: true,
        last_report: [0; 8],
        had_error: false,
        pending: [0; 5],
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
    // is just bInterval-1; for Low/Full speed it's a direct 1-255ms frame
    // count, needing an actual log2 conversion (1ms = 8 * 125us).
    let interval_field: u32 = if speed == 1 || speed == 2 {
        let target_units = (b_interval as u32).max(1) * 8;
        let mut n = 0u32;
        while (1u32 << n) < target_units {
            n += 1;
        }
        n
    } else {
        (b_interval as u32).saturating_sub(1)
    };

    // Input Context for Configure Endpoint: Input Control Context
    // (A0=slot, A_dci=this endpoint) + a freshly-rebuilt Slot Context
    // (same Route String/Speed/Root Hub Port Number as Address Device
    // set, but Context Entries bumped from 1 to `dci` - required
    // whenever the highest-indexed valid endpoint changes) + the new
    // Endpoint Context itself, laid out with `ctx_dwords`-sized contexts
    // exactly like Address Device's Input Context was.
    {
        let ctx_dwords = xhci.ctx_dwords;
        let ctx = unsafe { &mut *INPUT_CONTEXT.0.get() };
        ctx.fill(0);
        ctx[1] = (1 << 0) | (1 << dci); // Add Context Flags: A0 | A_dci

        let slot = &mut ctx[ctx_dwords..2 * ctx_dwords];
        slot[0] = (speed << 20) | (dci << 27); // Route String=0, Speed, Context Entries=dci
        slot[1] = port << 16; // Root Hub Port Number

        let int_ring_addr = INT_RING.0.get() as u64;
        let ep_off = ctx_dwords * (1 + dci as usize);
        let ep = &mut ctx[ep_off..ep_off + ctx_dwords];
        ep[0] = interval_field << 16; // Interval
        ep[1] = (3 << 1) | (EP_TYPE_INTERRUPT_IN << 3) | ((max_packet_size as u32) << 16); // CErr=3, EP Type=Interrupt In
        ep[2] = (int_ring_addr as u32) | 1; // TR Dequeue Pointer | DCS=1
        ep[3] = (int_ring_addr >> 32) as u32;
        ep[4] = 8; // Average TRB Length (a boot-protocol report's real size)
    }

    // Interrupt transfer ring - same shape as EP0_RING/COMMAND_RING, own
    // independent cycle state.
    {
        let ring = unsafe { &mut *INT_RING.0.get() };
        let ring_addr = INT_RING.0.get() as u64;
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
/// `docs/roadmap.md`). Mass storage (class 0x08) gets an explicit
/// callout since it's the class the next milestone is waiting to see.
/// Same bounded-walk shape as [`find_keyboard_interrupt_endpoint`], and
/// the same tolerance for a short-arrived buffer.
fn log_interfaces(port: u32, desc: &[u8]) {
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
/// keyboard. Returns `(bEndpointAddress, wMaxPacketSize masked to the low
/// 11 bits - SuperSpeed's own multiplier bits in the upper bits are
/// ignored, not needed for a report this small, bInterval)`. A malformed
/// or short-arrived buffer just stops the walk early (a zero `bLength` or
/// a length that would run past the end of `desc`) rather than panicking,
/// since `desc` is a fixed-size caller buffer that's genuinely allowed to
/// be only partially filled by a short USB packet.
fn find_keyboard_interrupt_endpoint(desc: &[u8]) -> Option<(u8, u16, u8)> {
    const DESCRIPTOR_TYPE_INTERFACE: u8 = 4;
    const DESCRIPTOR_TYPE_ENDPOINT: u8 = 5;
    const INTERFACE_CLASS_HID: u8 = 3;
    const INTERFACE_PROTOCOL_KEYBOARD: u8 = 1;
    const ENDPOINT_ATTR_TYPE_MASK: u8 = 0x03;
    const ENDPOINT_ATTR_TYPE_INTERRUPT: u8 = 0x03;
    const ENDPOINT_ADDRESS_DIR_IN: u8 = 0x80;

    let mut i = 0usize;
    let mut in_keyboard_interface = false;
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
        } else if b_descriptor_type == DESCRIPTOR_TYPE_ENDPOINT && b_length >= 7 && in_keyboard_interface {
            let b_endpoint_address = desc[i + 2];
            let bm_attributes = desc[i + 3];
            if bm_attributes & ENDPOINT_ATTR_TYPE_MASK == ENDPOINT_ATTR_TYPE_INTERRUPT
                && b_endpoint_address & ENDPOINT_ADDRESS_DIR_IN != 0
            {
                let w_max_packet_size = (desc[i + 4] as u16) | ((desc[i + 5] as u16) << 8);
                let b_interval = desc[i + 6];
                return Some((b_endpoint_address, w_max_packet_size & 0x7ff, b_interval));
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
/// wire this in.
pub fn poll_key() -> Option<u8> {
    unsafe { (*XHCI.0.get()).as_mut() }.and_then(Xhci::poll_key)
}
