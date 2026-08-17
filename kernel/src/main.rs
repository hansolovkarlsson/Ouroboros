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
mod gicv2;
mod gicv3;
mod loader;
mod madt;
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
mod xhci;

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
    // A real, hardware-confirmed reason for this flag, not a hedge - and
    // confirmed twice over now, not once. Real Parallels hardware took a
    // genuine Synchronous External Abort (ESR_EL1 EC=0x25, DFSC=0x10 - a
    // real bus fault, not a permission/translation issue) reading
    // virtio_mmio's very first scan address (`FAR_EL1` matched
    // `virtio_mmio::SLOT_BASE` exactly) - see `virtio_mmio.rs`'s module
    // doc comment and CLAUDE.md's "GOP framebuffer console, take four".
    // With that scan skipped, a *second*, differently-addressed instance
    // of the identical fault signature showed up next, this time with
    // `FAR_EL1` matching the old hardcoded `gic.rs`'s `GICD_BASE` exactly
    // - see CLAUDE.md's "take five". That second crash is what the
    // MADT/GICv3 work (`madt.rs`, `gicv3.rs`, `gic_confirmed` below) fixed
    // properly - GIC/timer setup no longer depends on this heuristic at
    // all, it depends on a real MADT parse. **This flag now only gates
    // virtio-console/virtio-blk** (renamed from the old `qemu_device_region_safe`
    // to reflect that): `virtio_mmio::find_device`'s scan still has no
    // MADT-equivalent real-discovery mechanism of its own (MADT describes
    // interrupt controllers, not virtio transports), and Parallels'
    // own PCI device inventory (`pci::log_all_devices`, see CLAUDE.md's
    // "virtio-console" section) shows no virtio-blk/virtio-console device
    // there at all regardless - so this heuristic is still the only thing
    // standing between `find_device`'s scan and a repeat of that first
    // crash. This is still a heuristic, not a proof (a platform could in
    // principle have no early console yet still safely expose virtio-mmio),
    // but it's grounded in the one real data point this project has - QEMU,
    // the only platform this scan has ever been confirmed safe on, also
    // always has a working ACPI/SPCR console.
    let virtio_mmio_probe_safe = discovery.is_some();

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

    // xHCI controller discovery (kernel/src/xhci.rs) - also boot-services-only
    // (PCI config-space reads via PciRootBridgeIo), so has to happen here
    // too. Unlike virtio-mmio's fixed QEMU-shaped addresses, this one
    // is genuinely discovered (a PCI BAR, read directly out of the
    // controller's own configuration space) rather than guessed - see
    // xhci.rs's module doc comment for why that makes it safe to actually
    // use later regardless of `virtio_mmio_probe_safe`.
    let xhci_info = match pci::discover_xhci() {
        Ok(info) => {
            log::info!(
                "Ouroboros kernel: xHCI controller @ {:#x}, PCI command register {:#06x} -> {:#06x}",
                info.base,
                info.command_before,
                info.command_after
            );
            Some(info)
        }
        Err(e) => {
            log::warn!("Ouroboros kernel: xHCI discovery failed ({e})");
            None
        }
    };

    // MADT (GIC version/address) discovery - see madt.rs's module doc
    // comment for why this replaces `qemu_device_region_safe` for GIC/timer
    // setup specifically, rather than continuing to rely on the "an early
    // console was found" heuristic. Only reads plain memory (like
    // acpi::discover_pl011), so it's boot-services-safe, and logging it
    // here means the result is visible even on a platform where every
    // console mechanism otherwise fails.
    let gic_info = match unsafe { madt::discover(rsdp) } {
        Ok(info) => {
            log::info!(
                "Ouroboros kernel: MADT: GIC {:?}, GICD @ {:#x}, GICC/GICR @ {:#x} (size {:#x})",
                info.version,
                info.gicd_base,
                if info.version == madt::GicVersion::V2 {
                    info.gicc_base
                } else {
                    info.gicr_base
                },
                info.gicr_size
            );
            Some(info)
        }
        Err(e) => {
            log::warn!("Ouroboros kernel: MADT/GIC discovery failed ({e:?})");
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
        "Ouroboros kernel: loaded shell program, region {:#x}-{:#x}, entry {:#x}",
        program.base,
        program.base + program.size,
        program.entry
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
    // call returned. Sized for 4, not 2 (framebuffer + xHCI BAR), as of
    // the MADT/GICv3 work: GICD/GICR are genuinely discovered addresses
    // now (madt.rs), not assumed to live in the fixed low-1GB device
    // block the way gic.rs's old QEMU-devicetree-derived addresses did -
    // real Parallels hardware could put them anywhere, the same way its
    // xHCI BAR turned out not to respect that assumption either. See
    // `mmu.rs`'s `MAX_EXTRA_L1_TABLES` doc comment for the matching bump
    // on that side.
    let mut extra_devices = [(0u64, 0u64); 4];
    let mut extra_device_count = 0;
    if let Some(info) = fb_info {
        extra_devices[extra_device_count] = (info.base, info.size as u64);
        extra_device_count += 1;
    }
    if let Some(info) = xhci_info {
        // Size is a coarse over-estimate (the real BAR size comes from a
        // second PCI config-space read this driver doesn't bother doing -
        // `install_identity_map` only needs a nonzero size to know the
        // region isn't empty, and maps a whole 1GB block regardless, same
        // as the framebuffer fallback).
        extra_devices[extra_device_count] = (info.base, 0x10000);
        extra_device_count += 1;
    }
    if let Some(info) = gic_info {
        extra_devices[extra_device_count] = (info.gicd_base, 0x10000);
        extra_device_count += 1;
        let (secondary_base, secondary_size) = match info.version {
            madt::GicVersion::V2 => (info.gicc_base, 0x10000),
            madt::GicVersion::V3 => (info.gicr_base, info.gicr_size),
        };
        extra_devices[extra_device_count] = (secondary_base, secondary_size);
        extra_device_count += 1;
    }
    unsafe {
        mmu::install_identity_map(
            &memory_map,
            [(program.base, program.size), tasks::idle_region()],
            &extra_devices[..extra_device_count],
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
    // including the framebuffer console - has failed, *and* only if
    // `virtio_mmio_probe_safe` says the underlying scan isn't known to be
    // dangerous here. Confirmed dead on Parallels specifically (no such
    // PCI device, see CLAUDE.md's "virtio-console" section) and now
    // confirmed to crash outright there too, not just fail to find
    // anything (see `virtio_mmio_probe_safe`'s own comment above) - kept
    // only for a hypothetical future platform with no GOP but a real,
    // safely-probeable virtio-mmio console device.
    if virtio_mmio_probe_safe && !console::is_installed() {
        try_virtio_console();
    }

    // USB HID keyboard (kernel/src/xhci.rs) - the first keyboard input
    // path this kernel has ever had. Deliberately NOT gated on
    // `virtio_mmio_probe_safe`: unlike virtio-mmio, the xHCI BAR is a
    // genuinely discovered address (PCI config space), not a guessed
    // QEMU-shaped convention - see xhci.rs's module doc comment. Not
    // fatal if it fails (no controller found, no device connected, ...) -
    // xhci::init logs and leaves the keyboard uninstalled, same
    // best-effort posture as virtio-blk/virtio-console/the framebuffer
    // console above.
    if let Some(info) = xhci_info {
        // Re-printed through whatever console is live now (unlike the
        // boot-services-only log::info! this was first reported through
        // in pci::discover_xhci) - see XhciInfo's doc comment for why:
        // this is the only way to still have this diagnostic on screen if
        // xhci::init below crashes, on a platform (Parallels, confirmed)
        // whose only console is the write-only framebuffer this same
        // boot already had to fall back to.
        console::println!(
            "Ouroboros kernel: xhci: BAR {:#x}, PCI command register {:#06x} -> {:#06x}",
            info.base,
            info.command_before,
            info.command_after
        );
        unsafe { xhci::init(info.base) };
    }

    // The runtime storage stack (docs/CHANGELOG.md phases 3a/3b): virtio-blk
    // + FAT32, installed globally for fs_list_dir/fs_read_file
    // (syscall.rs) to use. Not fatal if it fails - see init_storage's doc
    // comment. Gated on the same `virtio_mmio_probe_safe` flag as
    // virtio-console above - virtio_blk::Device::discover() goes through
    // the identical `virtio_mmio::find_device` scan that's now confirmed
    // to crash on real Parallels hardware, so this is skipped there for
    // the same reason, not a separate decision. **Not** helped by the
    // MADT/GICv3 work below - MADT describes interrupt controllers, not
    // virtio transports, and Parallels' own PCI inventory
    // (`pci::log_all_devices`) shows no virtio-blk device there at all
    // regardless of whether the scan itself would be safe.
    if virtio_mmio_probe_safe {
        init_storage();
    } else {
        console::println!("Ouroboros kernel: skipping virtio-blk (unconfirmed-safe virtio-mmio scan on this platform) - disk commands won't work this boot");
    }

    // GIC/timer setup - gated on `gic_info` being `Some` (real MADT
    // discovery, `madt.rs`), not the console-discovery heuristic this used to share
    // with virtio-mmio above. The old shared gate existed because real
    // Parallels hardware directly confirmed (a decoded Synchronous
    // External Abort, `FAR_EL1` matching the old hardcoded `GICD_BASE`
    // exactly) that writing to `gic.rs`'s QEMU-devicetree-derived
    // addresses crashes just as hard as virtio-mmio's did - see
    // CLAUDE.md's "take five". `madt.rs`/`gicv2.rs`/`gicv3.rs` fix that
    // properly: the addresses used below now come from the platform's own
    // ACPI MADT, confirmed real for *this* boot, not a QEMU-shaped
    // convention borrowed from a devicetree dump - see `madt.rs`'s module
    // doc comment. Skipping this (when MADT parsing itself fails - a real,
    // live possibility on a platform whose MADT doesn't describe an
    // interrupt controller the way Parallels' SPCR doesn't describe a
    // console, still unconfirmed either way) means no preemption this
    // boot (no tick ever fires, so `tasks.rs`'s round-robin never switches
    // away from task 0) - a real, known limitation, not silently
    // swallowed: `tasks::start` below does a straight `eret` into task 0
    // with no dependency on any of this having run, so the interactive
    // shell still works, just without ever preempting - task 0 simply
    // keeps running forever, which is fine for a single always-runnable
    // interactive task. `timer::arm` is deliberately not called at all in
    // that case: it's harmless either way (pure system-register access,
    // see `timer.rs`'s module doc comment), but arming a timer whose
    // interrupt will never be forwarded anywhere is just dead work.
    if let Some(info) = gic_info {
        // Re-printed through whatever console is live now, same reason
        // xhci_info's diagnostic is re-printed below - the boot-services-only
        // log::info! this was first reported through is long gone by the
        // time the framebuffer console (the only one on Parallels) clears
        // the screen and takes over. Confirmed real and correct on real
        // Parallels hardware, not just QEMU: `GIC V3, GICD @ 0x2410000,
        // GICC/GICR @ 0x2500000 (size 0x40000)` - genuinely different
        // addresses from QEMU's, and MADT parsing itself never crashed or
        // hung there, resolving the biggest open risk in the MADT/GICv3
        // scoping plan (whether Parallels' MADT describes an interrupt
        // controller at all - it does).
        console::println!(
            "Ouroboros kernel: MADT: GIC {:?}, GICD @ {:#x}, GICC/GICR @ {:#x} (size {:#x})",
            info.version,
            info.gicd_base,
            if info.version == madt::GicVersion::V2 {
                info.gicc_base
            } else {
                info.gicr_base
            },
            info.gicr_size
        );
        // SAFETY: GICD/GICC/GICR (whichever this version needs) are
        // mapped by the identity map just installed above - either inside
        // the fixed low-1GB device block (QEMU) or via `extra_devices`
        // (any address outside it, same mechanism the xHCI BAR already
        // relies on) - and, on this branch, confirmed to be genuinely
        // backed by a real interrupt controller, not just a mapped-but-
        // empty guess. Also confirmed on real Parallels hardware
        // specifically, not just QEMU: with task switching separately
        // disabled for isolation (see `exceptions.rs`'s
        // `TASK_SWITCH_ENABLED` doc comment), `uptime` reported a real,
        // correctly-incrementing tick count there (e.g. 566 -> 687 ticks)
        // with the shell staying fully responsive - GIC/timer IRQ
        // delivery itself is solid on real hardware, not just emulated.
        unsafe {
            gic::configure(info);
            gic::init();
            gic::enable_interrupt(timer::INTID);
        }
        timer::arm(timer::TICK_INTERVAL_MS);
    } else {
        console::println!(
            "Ouroboros kernel: skipping GIC/timer init (no MADT-confirmed interrupt controller on this platform) - no preemption this boot"
        );
    }

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
/// comment), now confirmed both nonexistent there *and* built on a scan
/// (`virtio_mmio::find_device`) that's confirmed to crash real Parallels
/// hardware outright - see `virtio_mmio.rs`'s module doc comment for the
/// decoded exception. Only reached at all when `virtio_mmio_probe_safe`
/// (`main`'s caller) is true, and even then only after the framebuffer
/// console has had its chance.
///
/// **Runs here, after `mmu::install_identity_map`, unlike devicetree/ACPI/
/// PCI - a real constraint, not an arbitrary placement choice.**
/// `virtio_mmio::find_device`'s scan needs the low-1GB device region
/// mapped under *this kernel's own* translation tables (see its safety
/// comment) - `virtio_blk::Device::discover` (`init_storage`, below)
/// already only ever runs at this same point in boot, for the same
/// reason.
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
///
/// Only ever called when `main`'s `virtio_mmio_probe_safe` is true -
/// `virtio_blk::Device::discover` goes through the same
/// `virtio_mmio::find_device` scan confirmed to crash real Parallels
/// hardware (see that module's doc comment), so this whole function is
/// skipped there rather than "not fatal if it fails" the way every other
/// failure mode here is - a crash isn't a failure this function could
/// recover from and report.
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
