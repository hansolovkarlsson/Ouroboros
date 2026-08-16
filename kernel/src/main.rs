#![no_main]
#![no_std]

extern crate alloc;

mod acpi;
mod console;
mod devicetree;
mod exceptions;
mod fat32;
mod fbconsole;
mod font;
mod framebuffer;
mod gic;
mod loader;
mod mmu;
mod pci;
mod syscall;
mod tasks;
mod timer;
mod uart;
mod uart16550;
mod virtio_blk;
mod virtio_console;
mod virtio_mmio;

use uefi::boot;
use uefi::prelude::*;

use console::Console;
use uart::Uart;
use uart16550::Uart16550;

/// Which register layout a discovered console needs — devicetree/ACPI SPCR
/// only ever identify a PL011; PCI enumeration only ever identifies a
/// 16550-family device (that's what PCI class 0x07/0x00 means). See
/// `pci.rs` for why these are genuinely different hardware, not just a
/// different address for the same driver.
enum ConsoleKind {
    Pl011,
    Uart16550,
}

/// Tries devicetree, then ACPI/SPCR, then PCI enumeration, logging why each
/// failed before trying the next. Must run before `exit_boot_services`:
/// devicetree/ACPI need the UEFI config table to find their blob pointers,
/// and PCI enumeration is entirely boot-services-based throughout (no
/// find-pointer-then-parse-memory split like the other two — there's
/// nothing to defer).
fn discover_console(
    dtb: Option<*const u8>,
    rsdp: Option<*const u8>,
) -> Option<(usize, ConsoleKind, &'static str)> {
    match unsafe { devicetree::discover_pl011(dtb) } {
        Ok(base) => return Some((base, ConsoleKind::Pl011, "devicetree")),
        Err(e) => log::warn!("Ouroboros kernel: devicetree console discovery failed ({e:?})"),
    }
    match unsafe { acpi::discover_pl011(rsdp) } {
        Ok(base) => return Some((base, ConsoleKind::Pl011, "ACPI SPCR")),
        Err(e) => log::warn!("Ouroboros kernel: ACPI SPCR console discovery failed ({e:?})"),
    }
    match pci::discover_uart16550() {
        Ok(base) => return Some((base, ConsoleKind::Uart16550, "PCI 16550")),
        Err(e) => log::warn!("Ouroboros kernel: PCI 16550 console discovery failed ({e:?})"),
    }
    None
}

#[entry]
fn main() -> Status {
    uefi::helpers::init().unwrap();

    log::info!("Ouroboros kernel: UEFI stage alive");

    // Must happen before exit_boot_services: the devicetree/ACPI pointers
    // live in the UEFI configuration table, and PCI enumeration needs boot
    // services throughout - see discover_console's doc comment.
    let dtb = devicetree::find_dtb();
    let rsdp = acpi::find_rsdp();

    // Deliberately still before exit_boot_services, even though some of
    // this parsing doesn't itself need boot services: logging the result
    // here goes through the UEFI console, which works on any platform, so
    // we get a trustworthy diagnostic before ever touching raw MMIO —
    // which, without a confirmed address, might not be mapped to anything
    // at all and fault instead of printing. Confirmed the hard way: writing
    // to a hardcoded "fallback" address whenever discovery failed
    // hard-crashed real Parallels hardware where that address wasn't
    // mapped. So there is no fallback anymore — no confirmed address means
    // no post-exit console, full stop.
    let discovery = discover_console(dtb, rsdp);
    if let Some((base, _kind, source)) = &discovery {
        log::info!("Ouroboros kernel: console @ {base:#x} (via {source})");
    } else {
        // Diagnostic only - see pci::log_all_devices's doc comment. Cheap,
        // and specifically useful on a platform (Parallels) where the
        // normal three mechanisms all fail and it isn't obvious why: this
        // answers "is there a virtio-pci device on the bus at all" while
        // we still have a working boot-services console to log through.
        pci::log_all_devices();
    }

    // GOP framebuffer discovery - also boot-services-only (see
    // framebuffer.rs's module doc comment), so it has to happen here
    // regardless of whether a byte-stream console was already found: the
    // framebuffer console is only tried as a last-resort fallback (see
    // `try_framebuffer_console` below), but discovering it, and folding
    // its address into the identity map, both have to happen now or not
    // at all.
    let fb_info = match framebuffer::discover() {
        Ok(info) => {
            log::info!(
                "Ouroboros kernel: GOP framebuffer @ {:#x}, size={:#x}, {}x{}, stride={}, format={:?}",
                info.base,
                info.size,
                info.width,
                info.height,
                info.stride,
                info.format
            );
            Some(info)
        }
        Err(e) => {
            log::warn!("Ouroboros kernel: GOP framebuffer discovery failed ({e})");
            None
        }
    };

    // Also boot-services-only (a filesystem read and a page allocation) -
    // see loader.rs's module doc comment for why this happens now rather
    // than after a real runtime disk driver exists. A failure here means
    // there is nothing to run, so it's fatal - same fail-fast posture as
    // uefi::helpers::init()'s unwrap() above.
    let program = match loader::load() {
        Ok(program) => program,
        Err(e) => panic!("Ouroboros kernel: failed to load shell program: {e}"),
    };
    log::info!(
        "Ouroboros kernel: loaded shell program, region {:#x}-{:#x}",
        program.base,
        program.base + program.size
    );

    // SAFETY: no boot-services protocol references (console, allocator, or
    // otherwise) are held past this call. Nothing below this point may use
    // log::*, alloc, or UEFI protocols — only the raw MMIO in `uart`/
    // `uart16550`, and only when `discovery` gave us an address to trust.
    // The returned memory map is kept, not discarded: mmu.rs uses it to
    // identity-map real discovered RAM instead of a hardcoded address.
    let memory_map = unsafe { boot::exit_boot_services(None) };

    // First thing after exit, before anything else gets a chance to fault:
    // a bad access is still possible (e.g. the UART write below, if
    // `discovery` ever resolves an address that isn't actually valid on
    // some untested platform), but it now reports through the exception
    // handler and halts, instead of taking the whole VM down the way an
    // untested address once did on Parallels.
    exceptions::install();

    if let Some((base, kind, _source)) = discovery {
        // SAFETY: `base` came from the platform's own devicetree, ACPI
        // tables, or PCI configuration space.
        let console = match kind {
            ConsoleKind::Pl011 => Console::Pl011(unsafe { Uart::new(base) }),
            ConsoleKind::Uart16550 => Console::Uart16550(unsafe { Uart16550::new(base) }),
        };
        console::install(console);
        console::println!("Ouroboros kernel: boot services exited, console live");
    }

    // SAFETY: called after exit_boot_services, with the memory map that
    // call returned.
    unsafe {
        mmu::install_identity_map(
            &memory_map,
            [(program.base, program.size), tasks::idle_region()],
            fb_info.map(|info| (info.base, info.size as u64)),
        )
    };
    console::println!("Ouroboros kernel: identity map installed, MMU running on our own tables");

    // A fourth console-discovery fallback: the GOP framebuffer, tried
    // right after devicetree/ACPI/PCI - ahead of virtio-console below,
    // not after it. This ordering is deliberate, not the original design
    // (which tried virtio-console first): a real-Parallels-hardware test
    // confirmed the framebuffer mapping and direct-write visibility both
    // work correctly here (a temporary raw full-screen fill diagnostic
    // turned the whole display solid white, then was removed once that
    // was confirmed - see CLAUDE.md's "GOP framebuffer console, take
    // three"), while that same test then froze solid, before reaching
    // this point, while still inside try_virtio_console() below - see
    // that function's own doc comment for why its virtio-mmio scan is
    // now the prime suspect for an unconfirmed, possibly platform-unsafe
    // assumption. Trying the now-hardware-validated framebuffer console
    // first means Parallels gets a working console before that riskier
    // scan ever runs, and - as a real side benefit - if virtio-mmio
    // scanning does fault on some future platform, there will finally be
    // a console installed to report the exception through instead of a
    // silent freeze.
    if !console::is_installed() {
        try_framebuffer_console(fb_info);
    }

    // A fifth and final fallback, tried only if every mechanism above -
    // including the framebuffer console - has failed. Confirmed dead on
    // Parallels specifically (no such PCI device, see CLAUDE.md's
    // "virtio-console" section) and now suspected of being actively
    // unsafe there too (see `try_virtio_console`'s doc comment) - kept
    // only for a hypothetical future platform with no GOP but a real
    // virtio-mmio console device.
    if !console::is_installed() {
        try_virtio_console();
    }

    // The runtime storage stack (docs/CHANGELOG.md phases 3a/3b): virtio-blk
    // + FAT32, installed globally for fs_list_dir/fs_read_file
    // (syscall.rs) to use. Not fatal if it fails - see init_storage's doc
    // comment.
    init_storage();

    // SAFETY: GICD/GICC are mapped by the identity map just installed
    // above (both fall within the low-1GB device block).
    unsafe {
        gic::init();
        gic::enable_interrupt(timer::INTID);
    }
    timer::arm(timer::TICK_INTERVAL_MS);

    // SAFETY: both EL0 regions were just mapped EL0-accessible above.
    unsafe { tasks::init(&program) };

    console::println!("Ouroboros kernel: shell ready - type and press Enter");

    // Unmasked last, right before dropping to EL0: nothing before this
    // point expects to be interrupted, and everything after (task 0, or
    // halt()'s wfe loop if it ever somehow got back here) is fine being
    // woken by the tick - which, from here on, is also what drives every
    // further task switch (`tasks::on_tick`).
    unsafe {
        core::arch::asm!("msr daifclr, #2", options(nostack, preserves_flags));
    }

    // SAFETY: called after tasks::init().
    unsafe { tasks::start() }
}

/// Tries virtio-mmio for a console device - originally "the real lead for
/// Parallels console output" (see `virtio_console.rs`'s module doc
/// comment), now confirmed both nonexistent *and* possibly unsafe to even
/// probe for there - see below - which is why this runs last, after the
/// framebuffer console, not first.
///
/// **Runs here, after `mmu::install_identity_map`, unlike devicetree/ACPI/
/// PCI - a real constraint, not an arbitrary placement choice.**
/// `virtio_mmio::find_device`'s scan needs the low-1GB device region
/// mapped under *this kernel's own* translation tables (see its safety
/// comment) - `virtio_blk::Device::discover` (`init_storage`, below)
/// already only ever runs at this same point in boot, for the same
/// reason.
///
/// **A real, unconfirmed assumption lives inside `find_device`'s scan,
/// and real-Parallels-hardware testing points at it being wrong there.**
/// The scan reads a magic-value register at 32 fixed, QEMU-specific
/// addresses, and `virtio_mmio.rs`'s own comment says an unpopulated slot
/// "reads as 0, not the magic value" on real hardware - stated as fact,
/// but never actually confirmed the way this project's other platform
/// conventions have been. A real bus fabric commonly raises an external
/// abort for a read to genuinely unbacked device-memory space instead of
/// QEMU's lenient software-emulated bus, which just returns a fixed
/// pattern as a courtesy. This is now the prime suspect for a real
/// symptom: a temporary raw-framebuffer-fill diagnostic confirmed the
/// framebuffer mapping and display update both work correctly on real
/// Parallels hardware (see CLAUDE.md's "GOP framebuffer console, take
/// three"), yet the boot still froze solid at that exact point in the
/// old ordering, which ran this function immediately afterward with no
/// console yet installed to report a fault through - a silent freeze
/// looks identical to "the scan just found nothing." Reordered so the
/// now-validated framebuffer console goes first: Parallels gets a working
/// console before this unconfirmed scan ever runs, and if it does fault
/// on some future platform, there will finally be a console to report it
/// through instead of nothing.
fn try_virtio_console() {
    let mut device = match unsafe { virtio_console::Device::discover() } {
        Ok(device) => device,
        Err(e) => {
            console::println!("Ouroboros kernel: virtio-console discovery failed ({e})");
            return;
        }
    };
    if let Err(e) = unsafe { device.init() } {
        console::println!("Ouroboros kernel: virtio-console init failed ({e})");
        return;
    }
    console::install(Console::Virtio(device));
    console::println!("Ouroboros kernel: virtio-console live (fallback - every other mechanism failed)");
}

/// Installs the GOP framebuffer console - the real answer for Parallels
/// (see `framebuffer.rs`/`fbconsole.rs`'s module doc comments), tried
/// right after devicetree/ACPI/PCI, ahead of virtio-console - see
/// `try_virtio_console`'s doc comment for why that's now deliberately
/// last rather than first.
///
/// Note what's already lost by the time this can run: `fb_info` had to be
/// discovered before `exit_boot_services` (boot-services-only protocol),
/// but the console itself can't be *installed* until after
/// `mmu::install_identity_map` has run with this framebuffer's address
/// folded in (`FbConsole::new`'s safety requirement) - so every boot
/// message between `exit_boot_services` and here only ever reached a
/// byte-stream console, if one existed at all. On a framebuffer-only
/// platform, the first thing that will ever actually appear on screen is
/// whatever's printed right after this call.
fn try_framebuffer_console(fb_info: Option<framebuffer::Info>) {
    let Some(info) = fb_info else {
        console::println!("Ouroboros kernel: no GOP framebuffer was discovered, no console available");
        return;
    };
    // SAFETY: `mmu::install_identity_map` was just called above with
    // `(info.base, info.size)` folded into its `framebuffer` argument -
    // either it already fell inside the discovered RAM span, or it got its
    // own device-block mapping. Either way it's mapped and writable now.
    let fb = unsafe { fbconsole::FbConsole::new(&info) };
    console::install(Console::Framebuffer(fb));
    console::println!("Ouroboros kernel: framebuffer console live (fallback - every byte-stream mechanism failed)");
}

/// Discovers and initializes the virtio-blk device, reads sector 0 back
/// as a sanity check (the MBR boot signature, `0x55 0xAA` at bytes
/// 510-511 - a property of the actual disk contents, not something this
/// code could produce by accident), then mounts a FAT32 filesystem on it
/// if there is one and installs it (`syscall::install_fs`) so
/// `fs_list_dir`/`fs_read_file` - and therefore the shell's `ls`/`cat`/
/// `cd` - have something to operate on. Not fatal if any step fails -
/// nothing else depends on storage existing, unlike `program` above; the
/// shell just won't have working disk commands (expected, not a bug, when
/// booted via `make run`'s vvfat disk - FAT16, not FAT32, see
/// `fat32.rs`'s module doc comment).
fn init_storage() {
    let mut device = match unsafe { virtio_blk::Device::discover() } {
        Ok(device) => device,
        Err(e) => {
            console::println!("Ouroboros kernel: virtio-blk discovery failed ({e})");
            return;
        }
    };
    if let Err(e) = unsafe { device.init() } {
        console::println!("Ouroboros kernel: virtio-blk init failed ({e})");
        return;
    }
    console::println!("Ouroboros kernel: virtio-blk ready, capacity {} sectors", device.capacity_sectors());

    let mut sector = [0u8; 512];
    if let Err(e) = unsafe { device.read_sector(0, &mut sector) } {
        console::println!("Ouroboros kernel: virtio-blk read failed ({e})");
        return;
    }
    let signature = (sector[511] as u16) << 8 | sector[510] as u16;
    if signature != 0xaa55 {
        console::println!(
            "Ouroboros kernel: virtio-blk sector 0 has no MBR signature ({signature:#06x}) - not attempting a FAT32 mount"
        );
        return;
    }

    match fat32::Fs::mount(device) {
        Ok(fs) => {
            console::println!("Ouroboros kernel: FAT32 mounted, disk commands available");
            syscall::install_fs(fs);
        }
        Err(e) => console::println!("Ouroboros kernel: FAT32 mount failed ({e}) - disk commands won't work this boot"),
    }
}

/// Parks the core forever instead of returning to firmware. `wfe` is a
/// low-power spin (wait-for-event) rather than a busy loop.
fn halt() -> ! {
    loop {
        unsafe {
            core::arch::asm!("wfe", options(nomem, nostack, preserves_flags));
        }
    }
}
