//! The early fault reporter: a kernel-written dump for a fault taken while
//! the firmware's exception vectors are still installed.
//!
//! Before the kernel's vectors are live (`exceptions::install()` right
//! after `exit_boot_services` on an EL1 handoff; on the Pi's EL2 handoff,
//! the drop to EL1 inside the identity-map install, `el2.rs`), a
//! synchronous exception lands in the firmware's own vector table, and
//! what the firmware prints is up to the firmware.
//! EDK2's `DefaultExceptionHandler` writes one line to the serial port,
//! `Synchronous Exception at 0x<pc>`, and the register dump that would
//! follow it is a `DEBUG()` print, compiled out of a RELEASE build. The
//! pftf/RPi4 firmware the Raspberry Pi boots with is a RELEASE build (its
//! release zip ships only that one), so on a Pi the one line is all there
//! is: no ESR, no FAR, no link register, no module name. The first serial
//! capture from a Pi 4 (2026-10-01) showed exactly that, twice, during the
//! xHCI takeover, at an address outside the kernel image.
//!
//! This module gets the rest of the dump without replacing the firmware's
//! vectors. The UEFI CPU Architecture Protocol
//! (`EFI_CPU_ARCH_PROTOCOL.RegisterInterruptHandler`) lets a loaded image
//! register its own handler for an exception class; the firmware's
//! dispatcher then calls that handler, with the full saved context
//! (`EFI_SYSTEM_CONTEXT_AARCH64`: x0 to x30, SP, ELR, SPSR, ESR, FAR),
//! instead of its default one. The registration is the mechanism the
//! firmware's own GIC driver uses for the IRQ class, so it is a supported
//! path, not a trick, and it leaves VBAR_EL1 alone: the firmware's IRQ
//! handling, which its timer and events depend on, is untouched.
//!
//! [`arm`] registers [`report`] for the synchronous class and keeps the
//! serial console discovered at boot for it to print through, with raw
//! MMIO writes and no boot service, since a handler runs in the fault's
//! own context. It stays registered until the kernel's vectors are live
//! (see above), after which the firmware's dispatcher is never entered
//! again: on an EL1 handoff because VBAR_EL1 now names the kernel's table,
//! on an EL2 handoff because nothing routes to EL2 any more (the drop's
//! `HCR_EL2`), not because VBAR_EL2 changed, which it does not. The dump
//! names what the firmware's line does
//! not, in this order, the parts that read nothing first: the exception
//! class and fault status decoded from ESR, the faulting address, the link
//! and frame pointers, every general register (before any firmware memory
//! is read, since a Pi 4 boot on 2026-10-01 lost the rows to a fault in
//! the walk that followed them), then the images holding `elr` and `lr`,
//! a frame-pointer backtrace, and for each code address the loaded image
//! that holds it (from the firmware's debug image info table: the kernel
//! itself by its offset, a firmware driver by the module name in its PE
//! debug directory and the firmware-file GUID in its device path). Then
//! it halts, as the kernel's own fault handler does.
//!
//! What it cannot do: report a fault that corrupts the firmware's
//! dispatcher or its stack before the handler is reached, or one taken
//! with the console's own mapping gone. A board that still shows only the
//! firmware's line after this is armed has one of those, which is itself
//! an answer.

use core::fmt::{self, Write};
use core::sync::atomic::{AtomicBool, Ordering};

use uefi::boot::{self, OpenProtocolAttributes, OpenProtocolParams, SearchType};
use uefi::mem::memory_map::{MemoryMap, MemoryType};
use uefi::proto::unsafe_protocol;
use uefi::table::cfg::ConfigTableEntry;
use uefi::{Guid, Identify, Status};
use uefi_raw::protocol::loaded_image::LoadedImageProtocol;

use crate::console::Console;
use crate::fbconsole::FbConsole;
use crate::framebuffer;
use crate::synccell::SyncCell;

/// `EFI_EXCEPTION_TYPE` for the synchronous class on AArch64
/// (`EXCEPT_AARCH64_SYNCHRONOUS_EXCEPTIONS` in MdePkg's DebugSupport.h;
/// IRQ is 1, FIQ 2, SError 3).
const EXCEPT_SYNCHRONOUS: isize = 0;

/// The firmware's saved context, `EFI_SYSTEM_CONTEXT_AARCH64` from MdePkg's
/// DebugSupport.h. The SIMD registers sit between `SP` and `ELR` in that
/// struct, which the first QEMU run of this reporter showed by printing
/// `esr=0x3` and an odd `elr`: they are declared here only to put the
/// fields after them where the firmware wrote them.
#[repr(C)]
struct SystemContextAarch64 {
    /// x0..x30; x29 is the frame pointer, x30 the link register.
    x: [u64; 31],
    sp: u64,
    v: [[u64; 2]; 32],
    elr: u64,
    spsr: u64,
    fpsr: u64,
    esr: u64,
    far: u64,
}

type InterruptHandler = unsafe extern "efiapi" fn(exception_type: isize, context: *const SystemContextAarch64);

/// `EFI_CPU_ARCH_PROTOCOL` (MdePkg's Protocol/Cpu.h). Only
/// `register_interrupt_handler` is called; the other members are declared
/// as opaque pointer-sized slots so the field offsets match the C struct.
#[repr(C)]
#[unsafe_protocol("26baccb1-6f42-11d4-bce7-0080c73c8881")]
struct CpuArch {
    flush_data_cache: usize,
    enable_interrupt: usize,
    disable_interrupt: usize,
    get_interrupt_state: usize,
    init: usize,
    register_interrupt_handler:
        unsafe extern "efiapi" fn(this: *mut CpuArch, exception_type: isize, handler: Option<InterruptHandler>) -> Status,
    get_timer_value: usize,
    set_memory_attributes: usize,
    number_of_timers: u32,
    dma_buffer_alignment: u32,
}

/// `EFI_DEBUG_IMAGE_INFO_TABLE_HEADER`, the config-table entry under
/// [`DEBUG_IMAGE_INFO_GUID`]: the DXE core's record of every loaded image.
#[repr(C)]
struct DebugImageInfoTableHeader {
    update_status: u32,
    table_size: u32,
    /// `table_size` entries of `EFI_DEBUG_IMAGE_INFO`, a pointer-sized
    /// union: null for an entry whose image was unloaded, else a pointer
    /// whose first `u32` is the entry's type.
    table: *const *const u32,
}

/// `EFI_DEBUG_IMAGE_INFO_NORMAL`, the entry type this reporter reads.
#[repr(C)]
struct DebugImageInfoNormal {
    image_info_type: u32,
    loaded_image: *const LoadedImageProtocol,
    image_handle: *const core::ffi::c_void,
}

const DEBUG_IMAGE_INFO_TYPE_NORMAL: u32 = 1;
const DEBUG_IMAGE_INFO_GUID: Guid = ConfigTableEntry::DEBUG_IMAGE_INFO_GUID;

/// A cap on the image table walk: a corrupted `table_size` must not turn
/// the report into a walk off the end of memory.
const MAX_IMAGES: u32 = 1024;
/// How many frames the frame-pointer backtrace prints at most.
const MAX_FRAMES: usize = 16;
/// The stack extent assumed when the memory map does not say: EDK2's
/// default DXE stack (`PcdCPUCorePrimaryStackSize`) is 128 KiB.
const STACK_FALLBACK: u64 = 128 << 10;

/// The console the report prints through: the serial console from
/// boot-time discovery, or, when there was none, a framebuffer console
/// made from [`FRAMEBUFFER`] at the first line of a report (making one
/// clears the screen, so not before there is a report to show).
static CONSOLE: SyncCell<Option<Console>> = SyncCell::new(None);
/// The GOP framebuffer, for a platform with no serial console (the Pi
/// over HDMI alone, Parallels).
static FRAMEBUFFER: SyncCell<Option<framebuffer::Info>> = SyncCell::new(None);
/// The end of the stack the kernel runs on, from the memory map at
/// [`arm`] time: the frame walk believes no record beyond it.
static STACK_END: SyncCell<u64> = SyncCell::new(0);
/// The RAM descriptors of the memory map at [`arm`] time (those
/// `mmu::is_general_ram` admits), `(start, end)` each, and their count:
/// the image walk reads a table entry, a loaded-image record, an image
/// header, a device path or a frame record only inside ONE of them
/// ([`in_ram`]). A pointer elsewhere is firmware data that is wrong, and
/// is skipped rather than read: a Pi 4 boot on 2026-10-01 had an entry
/// whose image claimed to hold `0x26e28`, and reading it faulted the
/// report itself; whether that address lies in a descriptor on the Pi is
/// what the `armed` log line's span now says. Descriptors rather than one
/// span, so a device window between two RAM ranges (the Pi 4's
/// peripherals at `0xfc000000`, inside an 8 GB board's RAM) is outside.
/// A hole inside a descriptor (a page the firmware's own tables do not
/// map, the 2026-10-02 kind of fault) cannot be told from here;
/// [`READING`] is for that case. Contiguous descriptors are merged as
/// they are read (the map splits RAM by type and attribute, QEMU's into
/// about thirty pieces), so an image whose pages span two of them is
/// inside one range here, and the cap of [`MAX_RAM_RANGES`] is reached
/// only by a map with that many separate holes in RAM; past it the rest
/// are dropped and [`RAM_TRUNCATED`] says so, since the walk is then
/// narrower than RAM, which is the safe direction.
static RAM: SyncCell<([(u64, u64); MAX_RAM_RANGES], usize)> = SyncCell::new(([(0, 0); MAX_RAM_RANGES], 0));
static RAM_TRUNCATED: SyncCell<bool> = SyncCell::new(false);
const MAX_RAM_RANGES: usize = 64;
/// The firmware memory the report is reading at this moment, or 0: set
/// around every read the report makes of firmware memory (the image
/// table, an entry, a loaded-image record, an image header, a device
/// path, a frame record), so a fault inside the report can say what it
/// was reading; 0 means none was under way.
static READING: SyncCell<u64> = SyncCell::new(0);
/// `\WALKFAULT` (`bootflags.rs`): the image walk reads [`UNMAPPED`] just
/// before its first image header, so the report faults inside itself.
static PLANT_WALK_FAULT: AtomicBool = AtomicBool::new(false);

/// `\WALKFAULT`'s plant: see [`PLANT_WALK_FAULT`]. Takes effect in the
/// report `\EARLYFAULT` causes; on its own it changes nothing.
pub fn plant_walk_fault() {
    PLANT_WALK_FAULT.store(true, Ordering::SeqCst);
}
/// The largest image the walk believes: a loaded driver is a few MB; an
/// entry claiming more is wrong data.
const MAX_IMAGE_SIZE: u64 = 64 << 20;
/// Where the kernel image sits, so an address in it is named by offset.
static IMAGE_RANGE: SyncCell<(u64, u64)> = SyncCell::new((0, 0));
/// The debug image info table header, read from the config table at
/// [`arm`] time, so the handler reads memory only.
static IMAGE_TABLE: SyncCell<u64> = SyncCell::new(0);
/// Set on entry to [`report`]: a fault inside the report itself prints a
/// short line and halts instead of recursing through the same path.
static ENTERED: AtomicBool = AtomicBool::new(false);

/// Why [`arm`] could not register the reporter. Each is logged by the
/// caller and the boot goes on without it.
#[derive(Debug, Clone, Copy)]
pub enum ArmError {
    /// Neither a serial console nor a framebuffer: nothing to print on.
    NoConsole,
    /// No handle carries `EFI_CPU_ARCH_PROTOCOL`.
    NoCpuProtocol,
    /// The protocol is there but could not be opened.
    OpenRefused(Status),
    /// `RegisterInterruptHandler` refused: `ALREADY_STARTED` means the
    /// firmware (or something else) has a handler of its own for the
    /// synchronous class, and this reporter will not displace it.
    RegisterRefused(Status),
}

impl fmt::Display for ArmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ArmError::NoConsole => write!(f, "no serial console and no framebuffer to report on"),
            ArmError::NoCpuProtocol => write!(f, "no EFI_CPU_ARCH_PROTOCOL"),
            ArmError::OpenRefused(s) => write!(f, "opening EFI_CPU_ARCH_PROTOCOL failed ({s:?})"),
            ArmError::RegisterRefused(s) => write!(f, "RegisterInterruptHandler refused ({s:?})"),
        }
    }
}

/// Registers [`report`] as the firmware's handler for synchronous
/// exceptions, printing through `serial` when there is one, else on
/// `framebuffer` (a console made from it at the first report), else not
/// at all. Boot services only: it opens a protocol and reads the memory
/// map. `image_range` is the kernel image's `[base, end)`.
///
/// On success the reporter is live from here until the kernel's vectors
/// are live (`exceptions::install()` on an EL1 handoff, the drop on an EL2
/// one). Nothing unregisters it: the firmware's vector table stops being
/// used at that point, and the handler reads only memory and the console,
/// so it is safe to be reached at any time before.
pub fn arm(
    serial: Option<Console>,
    framebuffer: Option<framebuffer::Info>,
    image_range: (u64, u64),
) -> Result<(), ArmError> {
    if serial.is_none() && framebuffer.is_none() {
        return Err(ArmError::NoConsole);
    }
    let handles =
        boot::locate_handle_buffer(SearchType::ByProtocol(&CpuArch::GUID)).map_err(|_| ArmError::NoCpuProtocol)?;
    let &handle = handles.first().ok_or(ArmError::NoCpuProtocol)?;
    // SAFETY: a non-exclusive open of a protocol the firmware's CPU driver
    // publishes for exactly this use; the interface is not uninstalled
    // while boot services run, and the open is closed when `cpu` drops.
    let cpu = unsafe {
        boot::open_protocol::<CpuArch>(
            OpenProtocolParams { handle, agent: boot::image_handle(), controller: None },
            OpenProtocolAttributes::GetProtocol,
        )
    }
    .map_err(|e| ArmError::OpenRefused(e.status()))?;

    // Everything the handler reads is stored before the registration, so
    // a fault the moment after it has a console and the tables.
    unsafe {
        *CONSOLE.get() = serial;
        *FRAMEBUFFER.get() = framebuffer;
        *IMAGE_RANGE.get() = image_range;
        let (stack, ram, count, truncated) = map_facts();
        *STACK_END.get() = stack;
        *RAM.get() = (ram, count);
        *RAM_TRUNCATED.get() = truncated;
        *IMAGE_TABLE.get() = uefi::system::with_config_table(|entries| {
            entries
                .iter()
                .find(|entry| entry.guid == DEBUG_IMAGE_INFO_GUID)
                .map_or(0, |entry| entry.address as u64)
        });
    }

    let this = core::ptr::from_ref::<CpuArch>(&cpu).cast_mut();
    // SAFETY: calling the protocol's own member on its own interface
    // pointer, with a handler of the signature the protocol declares.
    let status = unsafe { (cpu.register_interrupt_handler)(this, EXCEPT_SYNCHRONOUS, Some(report)) };
    if status.is_success() {
        // The walk's bound, stated, so a dump can be read against it: on
        // a board the question is whether an address a bad entry claims
        // (`0x26e28` on the Pi 4, 2026-10-01) falls inside any range.
        let (ranges, count) = unsafe { &*RAM.get() };
        let lo = ranges[..*count].iter().map(|r| r.0).min().unwrap_or(0);
        let hi = ranges[..*count].iter().map(|r| r.1).max().unwrap_or(0);
        log::info!(
            "Ouroboros kernel: early fault reporter: image walk bounded to {count} RAM range(s), {lo:#x}..{hi:#x}{}",
            if unsafe { *RAM_TRUNCATED.get() } { " (the map had more; the rest are outside the walk)" } else { "" }
        );
        Ok(())
    } else {
        unsafe {
            *CONSOLE.get() = None;
            *FRAMEBUFFER.get() = None;
        }
        Err(ArmError::RegisterRefused(status))
    }
}

/// Two facts from one read of the memory map. The end of the stack this
/// code runs on: the end of the descriptor holding the current SP (the
/// firmware's DXE stack, which the kernel inherits until it switches to
/// its own), or SP plus [`STACK_FALLBACK`] if the map does not cover it.
/// And the RAM ranges: the descriptors `mmu::is_general_ram` admits,
/// contiguous ones merged (the map is sorted by address), up to
/// [`MAX_RAM_RANGES`] of them with a flag for any dropped past that; none
/// if the map could not be read, which makes the image walk read nothing.
/// A descriptor whose extent does not compute (a corrupt page count, the
/// class of bad firmware record this module exists for) is skipped, not
/// trusted: this runs before the reporter exists to catch a panic.
fn map_facts() -> (u64, [(u64, u64); MAX_RAM_RANGES], usize, bool) {
    let sp: u64;
    // SAFETY: reads the stack pointer, nothing else.
    unsafe { core::arch::asm!("mov {}, sp", out(reg) sp, options(nomem, nostack, preserves_flags)) };
    let mut ranges = [(0u64, 0u64); MAX_RAM_RANGES];
    let Ok(map) = boot::memory_map(MemoryType::LOADER_DATA) else {
        return (sp + STACK_FALLBACK, ranges, 0, false);
    };
    let mut stack = sp + STACK_FALLBACK;
    let mut count = 0usize;
    let mut truncated = false;
    for d in map.entries() {
        let start = d.phys_start;
        let Some(end) = d.page_count.checked_mul(4096).and_then(|len| start.checked_add(len)) else {
            continue;
        };
        if start >= end {
            continue;
        }
        if start <= sp && sp < end {
            stack = end;
        }
        if !crate::mmu::is_general_ram(d.ty) {
            continue;
        }
        if count > 0 && ranges[count - 1].1 == start {
            ranges[count - 1].1 = end;
        } else if count < MAX_RAM_RANGES {
            ranges[count] = (start, end);
            count += 1;
        } else {
            truncated = true;
        }
    }
    (stack, ranges, count, truncated)
}

/// Whether `[addr, addr+len)`, `len` nonzero, lies inside one RAM
/// descriptor of [`arm`] time. An empty read, or a map that gave no
/// descriptor, refuses.
fn in_ram(addr: u64, len: u64) -> bool {
    if len == 0 {
        return false;
    }
    let Some(end) = addr.checked_add(len) else { return false };
    let (ranges, count) = unsafe { &*RAM.get() };
    ranges[..*count].iter().any(|&(lo, hi)| lo <= addr && end <= hi)
}

/// Marks `addr` as the firmware memory under read, for the nested-fault
/// line; `reading(0)` once the read is done. A volatile store: its only
/// reader is the exception handler the fault jumps to, which the compiler
/// cannot see, so an ordinary store could be sunk past the read it marks
/// or merged with the `reading(0)` after it.
fn reading(addr: u64) {
    unsafe { core::ptr::write_volatile(READING.get(), addr) };
}

/// An address no firmware maps: bit 47, the top of a 48-bit address space,
/// far above any RAM or device window a platform of this class has.
const UNMAPPED: u64 = 1 << 47;

/// `\EARLYFAULT`'s test fault (`bootflags.rs`): asks the firmware's own
/// `CopyMem` boot service to write eight bytes at [`UNMAPPED`], so the data
/// abort is taken with the PC inside the firmware's DXE core, not the
/// kernel, which is the case the reporter exists for: the firmware's line
/// alone places such a PC in no image the kernel knows. The dump should
/// name the DXE core for `elr` and this kernel for the frames above it.
/// Never returns: a copy that comes back means the platform mapped the
/// address, and that is reported and the boot halted, so that a test
/// looking for the dump does not find a shell instead.
pub fn plant_firmware_fault() -> ! {
    log::warn!("Ouroboros kernel: boot flag \\EARLYFAULT: asking the firmware to copy to {UNMAPPED:#x}, which nothing maps");
    let source = 0u64;
    match uefi::table::system_table_raw() {
        Some(system_table) => {
            // SAFETY: the system table and its boot services are live until
            // `exit_boot_services`, which this runs before; the destination
            // is meant to fault.
            unsafe {
                let boot_services = (*system_table.as_ptr()).boot_services;
                ((*boot_services).copy_mem)(UNMAPPED as *mut u8, core::ptr::from_ref(&source).cast::<u8>(), 8);
            }
            log::error!("Ouroboros kernel: the copy to {UNMAPPED:#x} returned: this platform maps it, and the test fault proved nothing");
        }
        None => log::error!("Ouroboros kernel: no system table, so the test fault was not planted"),
    }
    crate::halt()
}

fn out(args: fmt::Arguments) {
    // SAFETY: single core, and the handler runs with exceptions masked by
    // the firmware's vector entry; nothing else touches the cells while a
    // report runs.
    let console = unsafe { &mut *CONSOLE.get() };
    if console.is_none() {
        if let Some(info) = unsafe { *FRAMEBUFFER.get() } {
            // SAFETY: the firmware's translation tables are live and
            // identity-map the framebuffer its own text console was
            // drawing into moments ago, the same ground `\FBCON` stands
            // on in main.rs. Clears the screen: the report follows.
            *console = Some(Console::Framebuffer(unsafe { FbConsole::new(&info) }));
        }
    }
    if let Some(console) = console.as_mut() {
        let _ = console.write_fmt(args);
    }
}

macro_rules! line {
    ($($arg:tt)*) => {
        out(format_args!("{}\n", format_args!($($arg)*)))
    };
}

/// The handler the firmware calls. Prints the dump and halts.
unsafe extern "efiapi" fn report(exception_type: isize, context: *const SystemContextAarch64) {
    if ENTERED.swap(true, Ordering::SeqCst) {
        // A fault while reporting: say so with what is cheapest to read
        // and stop, rather than loop through the report again. The rows
        // of the first report are already out (they print before any
        // firmware memory is read); this names what was being read.
        let (esr, far, elr) =
            if context.is_null() { (0, 0, 0) } else { unsafe { ((*context).esr, (*context).far, (*context).elr) } };
        // On its own line: the first report was mid-way through an `elr`,
        // `lr` or `frame` line when the read faulted.
        let reading = unsafe { core::ptr::read_volatile(READING.get()) };
        if reading == 0 {
            line!("\nOuroboros kernel: EARLY EXCEPTION while reporting one: esr={esr:#x} far={far:#x} elr={elr:#x}, with no firmware memory under read; halted");
        } else {
            line!(
                "\nOuroboros kernel: EARLY EXCEPTION while reporting one: esr={esr:#x} far={far:#x} elr={elr:#x}, while reading firmware memory at {reading:#x}; halted"
            );
        }
        crate::halt();
    }
    if context.is_null() {
        line!("Ouroboros kernel: EARLY EXCEPTION type={exception_type} with no context; halted");
        crate::halt();
    }
    // SAFETY: the firmware's dispatcher passes a pointer to the context it
    // saved on the exception stack; it stays valid until the handler
    // returns, and this one never does.
    let ctx = unsafe { &*context };

    line!(
        "Ouroboros kernel: EARLY EXCEPTION (firmware vectors) type={exception_type} esr={:#x} far={:#x} elr={:#x} sp={:#x} lr={:#x} fp={:#x} spsr={:#x}",
        ctx.esr,
        ctx.far,
        ctx.elr,
        ctx.sp,
        ctx.x[30],
        ctx.x[29],
        ctx.spsr
    );
    out(format_args!("Ouroboros kernel:   esr: "));
    describe_esr(ctx.esr);
    out(format_args!("\n"));
    // The rows first: they need nothing but the context the firmware
    // saved. Everything after them reads firmware memory (the image table,
    // image headers, the stack) and can fault; on a Pi 4 on 2026-10-01 the
    // walk did, at frame 10, and the rows printed after it were lost.
    for row in 0..8 {
        out(format_args!("Ouroboros kernel:  "));
        for i in row * 4..(row * 4 + 4).min(31) {
            out(format_args!(" x{i:<2}={:#018x}", ctx.x[i]));
        }
        out(format_args!("\n"));
    }
    out(format_args!("Ouroboros kernel:   elr "));
    place(ctx.elr);
    out(format_args!("\n"));
    out(format_args!("Ouroboros kernel:   lr  "));
    place(ctx.x[30]);
    out(format_args!("\n"));
    backtrace(ctx);
    line!("Ouroboros kernel: halted in the early fault reporter");
    crate::halt()
}

/// The exception class and, for an abort, the fault status, in words.
/// The encodings are ESR_ELx's (Arm ARM D19.2.37 for the EC field, the
/// ISS encodings for data and instruction aborts for the FSC field).
fn describe_esr(esr: u64) {
    let ec = (esr >> 26) & 0x3f;
    let iss = esr & 0x1ff_ffff;
    let class = match ec {
        0x00 => "unknown reason",
        0x0e => "illegal execution state",
        0x15 => "svc",
        0x18 => "trapped msr/mrs/system instruction",
        0x20 => "instruction abort from a lower EL",
        0x21 => "instruction abort, same EL",
        0x22 => "pc alignment fault",
        0x24 => "data abort from a lower EL",
        0x25 => "data abort, same EL",
        0x26 => "sp alignment fault",
        0x2c => "floating-point exception",
        0x2f => "serror",
        0x30 | 0x31 => "breakpoint",
        0x32 | 0x33 => "software step",
        0x34 | 0x35 => "watchpoint",
        0x3c => "brk",
        _ => "other class",
    };
    out(format_args!("ec={ec:#04x} ({class})"));
    if matches!(ec, 0x20 | 0x21 | 0x24 | 0x25) {
        let fsc = iss & 0x3f;
        let level = fsc & 0x3;
        let status = match fsc >> 2 {
            0b0000 => "address size fault",
            0b0001 => "translation fault",
            0b0010 => "access flag fault",
            0b0011 => "permission fault",
            0b0100 if level == 0 => "synchronous external abort, not on a table walk",
            0b0100 => "synchronous external abort",
            0b0101 => "synchronous external abort on a table walk",
            0b0110 if level == 0 => "parity or ecc error",
            0b0111 => "parity or ecc error on a table walk",
            0b1000 if level == 1 => "alignment fault",
            0b1100 if level == 0 => "tlb conflict",
            _ => "other status",
        };
        out(format_args!(", fsc={fsc:#04x} ({status}"));
        if matches!(fsc >> 2, 0b0000..=0b0011 | 0b0101 | 0b0111) {
            out(format_args!(" at level {level}"));
        }
        out(format_args!(")"));
        if matches!(ec, 0x24 | 0x25) {
            let write = (iss >> 6) & 1 == 1;
            out(format_args!(", {}", if write { "write" } else { "read" }));
            if (iss >> 24) & 1 == 1 {
                let size = 1u64 << ((iss >> 22) & 0x3);
                out(format_args!(" of {size} byte{}", if size == 1 { "" } else { "s" }));
            }
        }
    } else {
        out(format_args!(", iss={iss:#x}"));
    }
}

/// Names the loaded image holding `addr` and the offset into it.
fn place(addr: u64) {
    out(format_args!("{addr:#x}"));
    let (base, end) = unsafe { *IMAGE_RANGE.get() };
    if base != 0 && (base..end).contains(&addr) {
        out(format_args!(" = kernel + {:#x}", addr - base));
        return;
    }
    match find_image(addr) {
        Some(image) => {
            out(format_args!(" = "));
            match image.name {
                Some(name) => out(format_args!("{}", BytesDisplay(name))),
                None => out(format_args!("an image with no debug entry")),
            }
            if let Some(guid) = image.file_guid {
                out(format_args!(" (firmware file {guid})"));
            }
            out(format_args!(" @ {:#x} + {:#x}", image.base, addr - image.base));
        }
        None => out(format_args!(" (in no loaded image)")),
    }
}

/// Frame records are `(previous fp, lr)` pairs that x29 chains through.
/// Each is believed only while it is 8-aligned (GCC's firmware code keeps
/// them 16-aligned; LLVM's kernel code puts them 8 past that, seen on
/// QEMU), above the last one, and between the faulting SP and the end of
/// the stack ([`STACK_END`]), so a frame pointer that was never one ends
/// the walk instead of reading from anywhere, and a read that would fault
/// cannot cut the report short.
fn backtrace(ctx: &SystemContextAarch64) {
    let mut fp = ctx.x[29];
    let mut last = ctx.sp.saturating_sub(16);
    let end = unsafe { *STACK_END.get() }.saturating_sub(16);
    for n in 1..=MAX_FRAMES {
        if fp == 0 || !fp.is_multiple_of(8) || fp <= last || fp < ctx.sp || fp > end {
            return;
        }
        // SAFETY: `fp` lies within the live stack by the check above, 8-aligned;
        // the record is two u64s read as such.
        reading(fp);
        let (next, lr) = unsafe { ((fp as *const u64).read_volatile(), (fp as *const u64).add(1).read_volatile()) };
        reading(0);
        out(format_args!("Ouroboros kernel:   frame {n}: fp={fp:#x} lr="));
        place(lr);
        out(format_args!("\n"));
        last = fp;
        fp = next;
    }
}

struct Image {
    base: u64,
    name: Option<&'static [u8]>,
    file_guid: Option<Guid>,
}

/// The loaded image whose `[base, base+size)` holds `addr`, from the
/// firmware's debug image info table. Every pointer in the walk is
/// firmware-written and believed only inside the RAM span of `arm` time
/// ([`in_ram`]): the table, each entry, each loaded-image record, the
/// image itself (page-aligned, at most [`MAX_IMAGE_SIZE`]) and its device
/// path. The walk is bounded by [`MAX_IMAGES`], each image is read only
/// within its own declared size, and [`READING`] names the image or path
/// under read, for the line a fault inside the report prints.
fn find_image(addr: u64) -> Option<Image> {
    let header_addr = unsafe { *IMAGE_TABLE.get() };
    if header_addr == 0 || !in_ram(header_addr, size_of::<DebugImageInfoTableHeader>() as u64) {
        return None;
    }
    // SAFETY: the config table pointed here at `arm` time; the DXE core
    // keeps the header for the life of boot services; it lies in RAM.
    reading(header_addr);
    let header = unsafe { &*(header_addr as *const DebugImageInfoTableHeader) };
    let (table, count) = (header.table, header.table_size.min(MAX_IMAGES));
    reading(0);
    if table.is_null() || count == 0 || !in_ram(table as u64, u64::from(count) * 8) {
        return None;
    }
    for i in 0..count {
        // SAFETY: `table` has `table_size` pointer-sized entries, in RAM.
        reading(table as u64 + u64::from(i) * 8);
        let entry = unsafe { *table.add(i as usize) };
        reading(0);
        if entry.is_null() || !in_ram(entry as u64, size_of::<DebugImageInfoNormal>() as u64) {
            continue;
        }
        reading(entry as u64);
        let (kind, loaded_ptr) = unsafe { (*entry, (*entry.cast::<DebugImageInfoNormal>()).loaded_image) };
        reading(0);
        if kind != DEBUG_IMAGE_INFO_TYPE_NORMAL
            || loaded_ptr.is_null()
            || !in_ram(loaded_ptr as u64, size_of::<LoadedImageProtocol>() as u64)
        {
            continue;
        }
        reading(loaded_ptr as u64);
        let loaded = unsafe { &*loaded_ptr };
        let (base, size, path) = (loaded.image_base as u64, loaded.image_size, loaded.file_path as u64);
        reading(0);
        if base == 0
            || size == 0
            || size > MAX_IMAGE_SIZE
            || !base.is_multiple_of(4096)
            || !in_ram(base, size)
            || !(base..base + size).contains(&addr)
        {
            continue;
        }
        reading(base);
        if PLANT_WALK_FAULT.swap(false, Ordering::SeqCst) {
            // `\WALKFAULT`: the fault a bad entry would take, taken here on
            // purpose, once, with the image base named as under read.
            unsafe { core::ptr::read_volatile(UNMAPPED as *const u8) };
        }
        let name = unsafe { pe_codeview_name(base as *const u8, size) };
        let file_guid = if path != 0 && in_ram(path, 20) {
            reading(path);
            unsafe { firmware_file_guid(path as *const u8) }
        } else {
            None
        };
        reading(0);
        return Some(Image { base, name, file_guid });
    }
    None
}

/// The module name EDK2's GenFw leaves in a PE image's CodeView debug
/// entry (an NB10, RSDS or MTOC record holding the build path of the
/// module's `.dll`), as the path's last component. `None` when the image
/// is not a PE32+ with such an entry, or any step would read outside
/// `[base, base+size)`.
///
/// # Safety
/// `base..base+size` must be readable.
unsafe fn pe_codeview_name(base: *const u8, size: u64) -> Option<&'static [u8]> {
    let rd32 = |off: u64| -> Option<u32> {
        if off.checked_add(4)? > size {
            return None;
        }
        Some(unsafe { base.add(off as usize).cast::<u32>().read_unaligned() })
    };
    let rd16 = |off: u64| -> Option<u16> {
        if off.checked_add(2)? > size {
            return None;
        }
        Some(unsafe { base.add(off as usize).cast::<u16>().read_unaligned() })
    };
    if rd16(0)? != 0x5a4d {
        return None;
    }
    let pe = u64::from(rd32(0x3c)?);
    if rd32(pe)? != 0x0000_4550 {
        return None;
    }
    let opt = pe + 24;
    if rd16(opt)? != 0x20b {
        return None;
    }
    if rd32(opt + 108)? <= 6 {
        return None;
    }
    let dir_rva = u64::from(rd32(opt + 112 + 6 * 8)?);
    let dir_size = u64::from(rd32(opt + 112 + 6 * 8 + 4)?);
    if dir_rva == 0 {
        return None;
    }
    for n in 0..(dir_size / 28).min(16) {
        let entry = dir_rva + n * 28;
        if rd32(entry + 12)? != 2 {
            continue;
        }
        let mut data = u64::from(rd32(entry + 20)?);
        if data == 0 {
            data = u64::from(rd32(entry + 24)?);
        }
        let path = match rd32(data)? {
            0x3031_424e => data + 16, // "NB10"
            0x5344_5352 => data + 24, // "RSDS"
            0x434f_544d => data + 20, // "MTOC": a u32 signature and a 16-byte Mach-O UUID
            _ => continue,
        };
        let mut len = 0u64;
        while path + len < size && len < 512 {
            if unsafe { *base.add((path + len) as usize) } == 0 {
                break;
            }
            len += 1;
        }
        if len == 0 {
            return None;
        }
        let bytes = unsafe { core::slice::from_raw_parts(base.add(path as usize), len as usize) };
        let start = bytes.iter().rposition(|&b| b == b'/' || b == b'\\').map_or(0, |p| p + 1);
        return Some(&bytes[start..]);
    }
    None
}

/// The firmware-file GUID from a loaded image's device path, when its
/// first node is a PIWG firmware file node (type 4, subtype 6): how a
/// driver loaded from the firmware volume is identified when its PE
/// carries no name. The GUID maps to a module through its `.inf`.
///
/// # Safety
/// `path` is a device path the firmware wrote, with at least 20 bytes
/// readable (the caller checked it lies in RAM).
unsafe fn firmware_file_guid(path: *const u8) -> Option<Guid> {
    let (kind, sub, len) = unsafe { (*path, *path.add(1), u16::from_le_bytes([*path.add(2), *path.add(3)])) };
    if kind != 4 || sub != 6 || len != 20 {
        return None;
    }
    let mut bytes = [0u8; 16];
    for (i, b) in bytes.iter_mut().enumerate() {
        *b = unsafe { *path.add(4 + i) };
    }
    Some(Guid::from_bytes(bytes))
}

/// Prints bytes as text, printable ASCII only, for a module name read
/// from firmware memory.
struct BytesDisplay(&'static [u8]);

impl fmt::Display for BytesDisplay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for &b in self.0 {
            f.write_char(if b.is_ascii_graphic() || b == b' ' { b as char } else { '?' })?;
        }
        Ok(())
    }
}
