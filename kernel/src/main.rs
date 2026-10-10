#![no_main]
#![no_std]

extern crate alloc;

mod acpi;
mod block;
mod bootflags;
mod bootid;
mod console;
mod devicetree;
mod dtranges;
mod earlyfault;
mod el2;
mod exceptions;
mod fbconsole;
mod fbdev;
mod font;
mod framebuffer;
mod gic;
mod gicv2;
mod gicv3;
mod loader;
mod madt;
mod mmu;
mod pci;
mod power;
mod supervisor;
mod syscall;
mod synccell;
mod tasks;
mod timer;
mod uart;
mod usb_msd;
mod uart16550;
mod virtio_blk;
mod virtio_console;
mod virtio_mmio;
mod virtio_net;
mod virtio_rng;
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
#[derive(Clone, Copy)]
enum ConsoleKind {
    Pl011,
    Uart16550,
}

impl ConsoleKind {
    /// The driver for a console of this kind at `base`.
    ///
    /// # Safety
    /// `base` must be the register base of such a device, as the
    /// platform's own devicetree, ACPI tables or PCI configuration space
    /// reported it; nothing checks that a write there lands anywhere.
    unsafe fn console(self, base: usize) -> Console {
        match self {
            ConsoleKind::Pl011 => Console::Pl011(unsafe { Uart::new(base) }),
            ConsoleKind::Uart16550 => Console::Uart16550(unsafe { Uart16550::new(base) }),
        }
    }
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

/// The kernel's own stack, 256 KB in the image's `.bss`, 16-aligned as
/// AArch64 requires of SP. The firmware's stack is not the kernel's to
/// use: on the Raspberry Pi it is 16 KB (`PcdCPUCorePrimaryStackSize` in
/// pftf's `RPi4.dsc`) at the top of RAM with the firmware's page tables
/// directly below it, and the kernel's deepest calls, with the firmware's
/// timer interrupt landing on top, overflowed it and wrote stack frames
/// over the tables; the TLB hid that until a cold page was walked, which
/// was every firmware fault the board showed from 2026-10-01 to 10-02
/// (`docs/testing/testing-pi4.md` section 6). QEMU's firmware gives 128 KB
/// with the tables elsewhere, so no rig saw it. [`main`] switches to this
/// stack as its first act and never returns to the firmware's; everything
/// after, the exit, the drop (`SP_EL1` is set from SP) and the exception
/// handlers included, runs here.
const KERNEL_STACK_SIZE: usize = 256 * 1024;
#[repr(C, align(16))]
struct KernelStack([u8; KERNEL_STACK_SIZE]);
static KERNEL_STACK: synccell::SyncCell<KernelStack> = synccell::SyncCell::new(KernelStack([0; KERNEL_STACK_SIZE]));

/// [`main`]'s own stack pointer on the firmware's stack, read after its
/// prologue (so the firmware's SP at entry less `main`'s frame, which is
/// why it is not called the firmware's SP), kept for `earlyfault.rs`: its
/// frame walk crosses from the kernel's stack back into the firmware's
/// frames that called the kernel, which lie at and above this value.
static ENTRY_SP: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// The word at the base of [`KERNEL_STACK`], written before the switch
/// and checked by both fault reporters: an overflow of the kernel's stack
/// overwrites it first. Not a guard page, which would fault at once: that
/// needs a 4 KB page unmapped inside the kernel's own block in `mmu.rs`
/// and is on the roadmap; until then the canary names the overflow in a
/// dump instead of leaving it to look like something else, which is what
/// the firmware's stack did for two days.
const STACK_CANARY: u64 = 0x5141_5141_5141_5141;

/// The kernel's stack as `(base, end)`, and [`ENTRY_SP`].
pub(crate) fn stacks() -> ((u64, u64), u64) {
    let base = KERNEL_STACK.get() as u64;
    ((base, base + KERNEL_STACK_SIZE as u64), ENTRY_SP.load(core::sync::atomic::Ordering::Relaxed))
}

/// Whether [`STACK_CANARY`] still sits at the kernel stack's base: false
/// means the stack overflowed its 256 KB and wrote below it.
pub(crate) fn stack_canary_intact() -> bool {
    // SAFETY: the base word of a static the kernel owns, written once in
    // `main` before the switch; read volatile since nothing the compiler
    // can see writes it.
    unsafe { core::ptr::read_volatile(KERNEL_STACK.get().cast::<u64>()) == STACK_CANARY }
}

#[entry]
fn main() -> Status {
    // First, before any call: onto the kernel's own stack (see
    // KERNEL_STACK). The firmware's SP is kept, the new SP set, and
    // `kernel_main` entered; it never returns, so there is no way back to
    // the firmware's stack and this function's own frame is the last thing
    // the firmware's stack holds of the kernel.
    let (top, entry_sp): (u64, u64);
    unsafe {
        core::arch::asm!("mov {0}, sp", out(reg) entry_sp, options(nomem, nostack, preserves_flags));
    }
    ENTRY_SP.store(entry_sp, core::sync::atomic::Ordering::Relaxed);
    // SAFETY: the base word of the kernel's own stack, which nothing has
    // used yet.
    unsafe { core::ptr::write_volatile(KERNEL_STACK.get().cast::<u64>(), STACK_CANARY) };
    top = KERNEL_STACK.get() as u64 + KERNEL_STACK_SIZE as u64;
    unsafe {
        // x29 is left as it is: this function's own frame record, on the
        // firmware's stack, so a frame-pointer walk from the kernel's stack
        // crosses into the firmware's frames that called the kernel
        // (earlyfault.rs's backtrace allows that one crossing).
        core::arch::asm!(
            "mov sp, {top}",
            "bl {kernel_main}",
            top = in(reg) top,
            kernel_main = sym kernel_main,
            options(noreturn),
        );
    }
}

/// Which build this is: the commit, `+dirty` if the tree differed from it,
/// and the profile. Set by `build.rs`, which says why it reruns every build.
/// Logged in the first line and again in whichever line announces the
/// console after the exit (serial, `\FBCON`, or a fallback), so a capture
/// from any console carries it.
const BUILD: &str = env!("OUROBOROS_BUILD");

/// The kernel proper, on its own stack. Never returns.
extern "C" fn kernel_main() -> ! {
    uefi::helpers::init().unwrap();

    let ((stack_base, stack_end), entry_sp) = stacks();
    let sp: u64;
    unsafe { core::arch::asm!("mov {0}, sp", out(reg) sp, options(nomem, nostack, preserves_flags)) };
    log::info!(
        "Ouroboros kernel: UEFI stage alive, build {BUILD}, on {} (sp {sp:#x}, the kernel's stack {stack_base:#x}..{stack_end:#x}, the entry's sp on the firmware's was {entry_sp:#x})",
        if (stack_base..stack_end).contains(&sp) { "its own stack" } else { "the FIRMWARE'S stack, which the switch should have left" }
    );
    // Where firmware loaded this image, so an address in the firmware's own
    // exception report ("Synchronous Exception at 0x...", which on a board
    // without a serial cable is all HDMI shows) can be placed inside this
    // image or outside it, and turned into an offset for the disassembly.
    let image_range = boot::open_protocol_exclusive::<uefi::proto::loaded_image::LoadedImage>(boot::image_handle())
        .map(|image| {
            let (base, size) = image.info();
            (base as u64, base as u64 + size)
        })
        .unwrap_or((0, 0));
    log::info!("Ouroboros kernel: image @ {:#x}..{:#x}", image_range.0, image_range.1);

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
    let mut pci_inventory = None;
    if let Some((base, _kind, source)) = &discovery {
        log::info!("Ouroboros kernel: console @ {base:#x} (via {source})");
    } else {
        // Diagnostic only - see pci::log_all_devices's doc comment. Cheap,
        // and specifically useful on a platform (Parallels) where the
        // normal three mechanisms all fail and it isn't obvious why: this
        // answers "is there a virtio-pci device on the bus at all" while
        // we still have a working boot-services console to log through.
        // The returned inventory is re-printed later, through whatever
        // post-exit console installs - the UEFI-console rendering of
        // these lines is unreadable in practice on real Parallels
        // hardware (fbconsole clears the screen on install, ~2s after
        // power-on - see log_all_devices's doc comment).
        pci_inventory = Some(pci::log_all_devices());
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

    // The early fault reporter (earlyfault.rs): from here until
    // `exceptions::install()`, a synchronous exception still goes through
    // the firmware's vectors, and a RELEASE firmware (the Pi's pftf build)
    // prints one line for it, with no ESR, FAR or link register.
    // Registering the kernel's own handler through the firmware's CPU
    // protocol gets the whole dump, on the serial console just discovered
    // or, without one, on the framebuffer (the Pi over HDMI alone,
    // Parallels), with raw writes the handler can make from a fault. The
    // first Pi 4 serial boot (2026-10-01) died in the xHCI takeover below
    // with only the firmware's line to show for it; this reads the rest.
    // SAFETY: the same address the post-exit console will use, from the
    // platform's own devicetree, ACPI tables or PCI config space.
    let early_serial = discovery.map(|(base, kind, _)| unsafe { kind.console(base) });
    let early_on = if early_serial.is_some() { "the serial console" } else { "the framebuffer" };
    match earlyfault::arm(early_serial, fb_info, image_range) {
        Ok(()) => log::info!(
            "Ouroboros kernel: early fault reporter armed: a fault before the kernel's own vectors reports on {early_on}"
        ),
        Err(e) => log::warn!(
            "Ouroboros kernel: early fault reporter not armed ({e}); a fault before the kernel's own vectors shows only what the firmware prints"
        ),
    }

    // Whether the firmware declares DMA non-coherent (ACPI `_CCA 0`, the
    // Raspberry Pi 4/400's PCIe root: docs/testing/testing-pi4.md Risk 8).
    // Decides whether the xHCI/USB DMA pool is mapped non-cacheable below.
    // Not unconditional: under a hypervisor that emulates the device (as
    // Parallels emulates xHCI), the host reads guest memory through a
    // cacheable mapping, and a guest-side non-cacheable one could disagree
    // with it; where DMA is coherent the pool stays ordinary memory.
    let dma_noncoherent = match unsafe { acpi::dma_noncoherent(rsdp) } {
        Ok(Some(at)) => {
            log::info!(
                "Ouroboros kernel: ACPI declares DMA non-coherent (_CCA 0, in {} at offset {:#x}): the DMA pool will be mapped non-cacheable",
                core::str::from_utf8(&at.table).unwrap_or("????"),
                at.offset
            );
            true
        }
        Ok(None) => {
            log::info!("Ouroboros kernel: ACPI declares no non-coherent DMA (no _CCA 0): the DMA pool stays cacheable");
            false
        }
        Err(e) => {
            log::warn!("Ouroboros kernel: DMA coherence unknown ({e:?}), assuming coherent: the DMA pool stays cacheable");
            false
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
    // The cores the MADT describes, one GICC entry each (multi-core step 1,
    // docs/roadmap/roadmap-smp.md): the count and this core's MPIDR on one
    // line, then one line per core, so a rig can count them and step 2 can
    // match each `core N up` against its MPIDR. Logged here, before the
    // exit, for the same reason as the GIC line above.
    {
        let cores = madt::cores();
        log::info!(
            "Ouroboros kernel: MADT: {} cores{} (this core affinity {:#x}, mpidr {:#x})",
            cores.count,
            if cores.truncated { ", more than fit listed" } else { "" },
            madt::affinity(gicv3::read_mpidr()),
            gicv3::read_mpidr()
        );
        for i in 0..cores.count {
            log::info!(
                "Ouroboros kernel: MADT: core {}: mpidr {:#x}, {}",
                i,
                cores.mpidr[i],
                if cores.enabled[i] { "enabled" } else { "disabled" }
            );
        }
    }

    // Read the PSCI conduit (hvc/smc) from ACPI's FADT now, in the same
    // before-exit_boot_services window as the MADT parse, so the `POWER`
    // syscall can power the machine off later without touching ACPI again. A
    // failure is non-fatal - power-off just falls back to a CPU halt.
    unsafe { power::discover_conduit(rsdp) };

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
        program.end(),
        program.entry
    );

    // The filesystem server, same boot-services window - but unlike the
    // shell, optional: booting without one just means no filesystem this
    // boot (every FS request fails with "no such task", which the shell
    // reports as its no-filesystem message), same graceful degradation
    // the FAT16-vvfat dev loop has always had.
    let fsd = match loader::load_fsd() {
        Ok(fsd) => {
            log::info!(
                "Ouroboros kernel: loaded filesystem server, region {:#x}-{:#x}, entry {:#x}",
                fsd.base,
                fsd.end(),
                fsd.entry
            );
            Some(fsd)
        }
        Err(e) => {
            log::warn!("Ouroboros kernel: no filesystem server ({e}) - disk commands won't work this boot");
            None
        }
    };

    // The console server, same boot-services window and same optional
    // posture as the filesystem server: booting without one just means
    // the kernel's own console handles all output (Stage 1a has clients
    // still printing via the kernel's PUTC anyway, so a missing COND.BIN
    // is invisible for now).
    let cond = match loader::load_cond() {
        Ok(cond) => {
            log::info!(
                "Ouroboros kernel: loaded console server, region {:#x}-{:#x}, entry {:#x}",
                cond.base,
                cond.end(),
                cond.entry
            );
            Some(cond)
        }
        Err(e) => {
            log::warn!("Ouroboros kernel: no console server ({e}) - the kernel console handles all output this boot");
            None
        }
    };

    let netd = match loader::load_netd() {
        Ok(netd) => {
            log::info!(
                "Ouroboros kernel: loaded network server, region {:#x}-{:#x}, entry {:#x}",
                netd.base,
                netd.end(),
                netd.entry
            );
            Some(netd)
        }
        Err(e) => {
            log::warn!("Ouroboros kernel: no network server ({e}) - no network this boot");
            None
        }
    };

    let accountd = match loader::load_accountd() {
        Ok(accountd) => {
            log::info!(
                "Ouroboros kernel: loaded account server, region {:#x}-{:#x}, entry {:#x}",
                accountd.base,
                accountd.end(),
                accountd.entry
            );
            Some(accountd)
        }
        Err(e) => {
            log::warn!("Ouroboros kernel: no account server ({e}) - no self-service passwd this boot");
            None
        }
    };

    // The boot identity (step 3 of docs/roadmap/roadmap-session-auth.md):
    // the counter's stores are a UEFI variable and an ESP file, and the
    // entropy is EFI_RNG_PROTOCOL, all boot services, so it is established
    // here, after the loader and before the xHCI takeover below (the counter
    // file is on the ESP, which may be on USB), and read by the BOOT_ID
    // syscall afterwards.
    bootid::establish();

    // xHCI controller discovery (kernel/src/xhci.rs) - boot-services-only
    // (PciRootBridgeIo and the controller's own PciIo), so it happens before
    // the exit, and it happens LAST: taking the controller stops firmware's
    // USB stack on it, and the boot disk may be a USB stick (a Pi 4/400
    // booted from USB). Every read of the ESP - the loader's programs above,
    // the boot identity's counter file - must be done by now. Unlike
    // virtio-mmio's fixed QEMU-shaped addresses, this one is genuinely
    // discovered: firmware's own CPU address for the controller's BAR,
    // checked against the BAR itself (`pci::bar0_address`; they differ on
    // the Pi, whose PCIe window is translated), rather than guessed - see
    // xhci.rs's module doc comment for why that makes it safe to actually
    // use later regardless of `virtio_mmio_probe_safe`.
    // Boot flag files (bootflags.rs), read here, before the takeover: `\NOXHCI`
    // skips this step, `\XHCINOWR` skips only its command-register write.
    // Bench diagnostics for bisecting a hang on real hardware, except
    // `\MSDSTALL`, a QEMU test fault: it asks usb_msd.rs to stall QEMU's stick
    // (armed there only for a stick whose vendor is QEMU).
    // Repeated here, next to the step under suspicion, so it is still on
    // screen when a firmware exception report lands below it.
    log::info!("Ouroboros kernel: image @ {:#x}..{:#x}, taking the xHCI controller next", image_range.0, image_range.1);
    // `\FBCON` (`fb_console_forced`): leave the discovered serial console
    // uninstalled after the exit, so the framebuffer console below takes HDMI.
    let bootflags::Flags { no_xhci, xhci_no_write, fb_console: fb_console_forced, msd_stall, early_fault, walk_fault } =
        bootflags::read();
    if msd_stall {
        usb_msd::inject_stalls();
    }
    if walk_fault {
        // `\WALKFAULT`: the report that `\EARLYFAULT` causes faults inside
        // its own image walk; the rows must survive it.
        earlyfault::plant_walk_fault();
    }
    if early_fault {
        // `\EARLYFAULT`: a fault inside the firmware's own code, here, at
        // the step the Pi 4 died in; the boot ends in the early fault
        // reporter's dump. Does not return.
        earlyfault::plant_firmware_fault();
    }
    let xhci_result = if no_xhci {
        Err(pci::XhciDiscoveryError::SkippedByFlag)
    } else {
        pci::discover_xhci(!xhci_no_write)
    };
    let xhci_info = match xhci_result {
        Ok(info) => {
            log::info!(
                "Ouroboros kernel: xHCI controller @ {:#x} (BAR {:#x}, translation {:#x}), PCI command register {:#06x} -> {:#06x}",
                info.base,
                info.bus,
                info.translation,
                info.command_before,
                info.command_after
            );
            if xhci_no_write {
                // Memory Space stays off, so the controller's registers do
                // not decode (QEMU reads all-ones and the bring-up hangs);
                // under this flag the result is how far the boot gets.
                log::warn!("Ouroboros kernel: xhci: not brought up (\\XHCINOWR)");
                None
            } else {
                Some(info)
            }
        }
        Err(e) => {
            log::warn!("Ouroboros kernel: xHCI discovery failed ({e})");
            None
        }
    };

    // SAFETY: no boot-services protocol references (console, allocator, or
    // otherwise) are held past this call. Nothing below this point may use
    // log::*, alloc, or UEFI protocols — only the raw MMIO in `uart`/
    // `uart16550`, and only when `discovery` gave us an address to trust.
    // The returned memory map is kept, not discarded: mmu.rs uses it to
    // identity-map real discovered RAM instead of a hardcoded address.
    // The last line before the exit, so a fault can be placed on one side of
    // it (before this line, it is in the discovery above).
    log::info!("Ouroboros kernel: exiting boot services");
    let memory_map = unsafe { boot::exit_boot_services(None) };
    if fb_console_forced {
        progress_square(fb_info, 1);
    }

    // IRQs masked from here until the first `eret` into task 0, by our own
    // instruction and not by trusting what the firmware left: EDK2's
    // ExitBootServices does disable interrupts, but the kernel's whole
    // single-core argument (`synccell.rs`) is that EL1 never runs
    // unmasked, and vector slot 5 (IRQ at EL1h) now halts to prove it.
    unsafe {
        core::arch::asm!("msr daifset, #2", options(nostack, preserves_flags));
    }

    // First thing after exit, before anything else gets a chance to fault:
    // a bad access is still possible (e.g. the UART write below, if
    // `discovery` ever resolves an address that isn't actually valid on
    // some untested platform), but it now reports through the exception
    // handler and halts, instead of taking the whole VM down the way an
    // untested address once did on Parallels. On an EL2 handoff (the
    // Raspberry Pi) this write is made all the same but takes effect only
    // at the drop to EL1 inside `mmu::install_identity_map` below; until
    // then a fault goes through the firmware's EL2 vectors, where
    // `earlyfault.rs`'s handler is registered. `install` is the one owner
    // of the write (it refuses it only under a VHE firmware, see there).
    exceptions::install();
    if fb_console_forced {
        progress_square(fb_info, 2);
    }

    // `\FBCON`: the framebuffer console right away, on the firmware's page
    // tables, so everything from here on (the MMU switch included, and any
    // exception report, which needs an installed console) reaches HDMI. Built
    // for the Pi 400, whose boot went silent after the exit with no serial
    // cable to say where.
    if fb_console_forced {
        if let Some(info) = fb_info {
            // SAFETY: the firmware's translation tables are still live
            // (mmu.rs has not switched TTBR0 yet), UEFI requires them to
            // identity-map memory, and the firmware's own text console was
            // drawing into this framebuffer moments ago, so it is mapped and
            // writable at `info.base`. After the switch our tables map it
            // too (`extra_devices` below), so the console survives it.
            let fb = unsafe { fbconsole::FbConsole::new(&info) };
            console::install(Console::Framebuffer(fb));
            console::println!("Ouroboros kernel: framebuffer console live early (\\FBCON), on the firmware's page tables, build {BUILD}");
        }
    }

    // Kept for the identity map below, which maps the console explicitly.
    let console_base = discovery.as_ref().map(|&(base, _, _)| base as u64);
    // `\FBCON` replaces the serial console only when there is a framebuffer
    // to replace it with; without one, the flag would leave no console at all.
    let serial_console = discovery.filter(|_| !(fb_console_forced && fb_info.is_some()));
    let serial_console_base = serial_console.as_ref().map(|&(base, _, _)| base as u64);
    if let Some((base, kind, _source)) = serial_console {
        // SAFETY: `base` came from the platform's own devicetree, ACPI
        // tables, or PCI configuration space.
        let console = unsafe { kind.console(base) };
        console::install(console);
        console::println!("Ouroboros kernel: boot services exited, console live, build {BUILD}");
    }
    // The exception level the firmware handed off at, stated before any
    // `_EL1` register is relied on: at EL2 (the Raspberry Pi) every write
    // after this line goes to a register the running level does not use.
    // See `el2.rs`.
    console::println!("Ouroboros kernel: running at EL{} after the exit", el2::current_el());

    // SAFETY: called after exit_boot_services, with the memory map that
    // call returned. Up to five regions (`mmu::MAX_EXTRA_DEVICES`): the
    // console, framebuffer, xHCI BAR, GICD and GICR, all discovered
    // addresses (madt.rs for the GIC), none assumed to live in the fixed
    // low-1GB device block. `mmu.rs`'s `MAX_EXTRA_L1_TABLES` is sized to
    // match, so each can have its own L1 table past 512GB.
    let mut extra_devices = [(0u64, 0u64); mmu::MAX_EXTRA_DEVICES];
    let mut extra_device_count = 0;
    // The discovered serial console, mapped because it is the console: the
    // fixed low-1GB device block is a QEMU-shaped convention, and the Pi 4's
    // PL011 (0xfe201000) is far above it. Mapped under `\FBCON` too, where it
    // goes unused, since it costs one block and keeps the map the same.
    if let Some(base) = console_base {
        extra_devices[extra_device_count] = (base, 0x1000);
        extra_device_count += 1;
    }
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
    // The boot EL0 regions, one per task slot (auto-sized to NUM_TASKS so
    // raising the slot count needs no edit here): slot 0 the loaded program,
    // slot 1 idle, slots 2/3/4 the filesystem/console/network servers ((0, 0)
    // - "no region" - for any that wasn't loaded), and every spawnable slot
    // (5..) stays (0, 0) until `tasks::spawn` fills it in.
    // `install_identity_map` already treats a zero-size region as "no region"
    // (see `overlaps_any`).
    let mut el0_regions = [(0u64, 0u64); tasks::NUM_TASKS];
    el0_regions[0] = (program.base, program.size);
    el0_regions[1] = tasks::idle_region();
    el0_regions[2] = fsd.as_ref().map_or((0, 0), |f| (f.base, f.size));
    el0_regions[3] = cond.as_ref().map_or((0, 0), |c| (c.base, c.size));
    el0_regions[4] = netd.as_ref().map_or((0, 0), |n| (n.base, n.size));
    el0_regions[5] = accountd.as_ref().map_or((0, 0), |a| (a.base, a.size));
    console::println!("Ouroboros kernel: installing our own identity map");
    unsafe {
        // The xHCI/USB DMA pool is mapped Normal Non-cacheable only where
        // the firmware declared DMA non-coherent (above,
        // docs/testing/testing-pi4.md Risk 8), since nothing in xhci.rs or
        // usb_msd.rs maintains caches. The framebuffer is NOT: it stays
        // ordinary memory and fbdev.rs/fbconsole.rs clean every write out
        // to memory for the display engine (Risk 7) - correct on bare
        // metal and under a hypervisor alike, and a scroll reads cached
        // memory rather than uncached.
        let uncached = [xhci::dma_region()];
        let uncached_count = usize::from(dma_noncoherent);
        mmu::install_identity_map(
            memory_map,
            el0_regions,
            &extra_devices[..extra_device_count],
            &uncached[..uncached_count],
        )
    };
    // True at EL1 on every platform now: on an EL2 handoff the install
    // dropped to EL1 on the way (`el2.rs`), and logged it.
    console::println!("Ouroboros kernel: identity map installed, MMU running on our own tables");
    // The console's own device mapping gives way to RAM's when RAM's span
    // covers its 1GB block (the Pi 4 with RAM above 3GB): the UART is then
    // cacheable and its output may never leave the cache. Asked of the
    // hardware walker, not inferred from the plan.
    if let Some(base) = serial_console_base {
        if !mmu::walks_as_device(base) {
            console::println!(
                "Ouroboros kernel: WARNING: console {base:#x} is not mapped as a device (its 1GB block is RAM's), its output may stop here"
            );
        }
    }
    tasks::init_runtime_allocator();

    // Make the framebuffer available to the console server's FB_* syscalls
    // whenever one was discovered and is now mapped - independent of which
    // console the kernel itself installs below (on QEMU + ramfb a
    // byte-stream console wins the kernel's own slot, but cond can still
    // render to the framebuffer). SAFETY: fb_info.base was just folded
    // into the identity map above, same contract as FbConsole::new.
    if let Some(info) = fb_info {
        unsafe { fbdev::install(&info) };
    }

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

    // Re-print the boot-services PCI inventory (captured above, only on
    // the no-early-console path) through the console that actually
    // survives - see log_all_devices's doc comment for why the
    // boot-services log::info! rendering of the same lines is
    // effectively invisible on real Parallels hardware. Never runs on
    // the normal QEMU dev loop (ACPI console found -> inventory never
    // captured).
    if let Some((devices, count)) = &pci_inventory {
        if console::is_installed() {
            for id in &devices[..*count] {
                let pci::PciDeviceId { vendor, device, class, subclass, prog_if } = *id;
                console::println!(
                    "Ouroboros kernel: PCI device: vendor={vendor:#06x} device={device:#06x} class={class:#04x} subclass={subclass:#04x} prog_if={prog_if:#04x}"
                );
            }
        }
    }

    // USB HID keyboard (kernel/src/xhci.rs) - the first keyboard input
    // path this kernel has ever had. Deliberately NOT gated on
    // `virtio_mmio_probe_safe`: unlike virtio-mmio, the xHCI address is
    // genuinely discovered (firmware's own address for the BAR, checked
    // against config space), not a guessed QEMU-shaped convention - see
    // xhci.rs's module doc comment. Not
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
            "Ouroboros kernel: xhci: CPU {:#x} (BAR {:#x}, translation {:#x}), PCI command register {:#06x} -> {:#06x}",
            info.base,
            info.bus,
            info.translation,
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
        // Network stack, Stage 1 (docs/ROADMAP.md): the virtio-net driver +
        // a raw-frame ARP round-trip proof. Same gate as storage - the
        // virtio-mmio scan crashes real Parallels hardware, and Parallels
        // exposes virtio-net over PCI anyway (needs a transport this project
        // doesn't have yet). Silent when no NIC is attached (only
        // `make run-net` attaches one).
        init_net();
        // The entropy source behind the RANDOM syscall (password salts). Same
        // gate and the same "silent when absent" rule as the NIC: only a QEMU
        // run started with `-device virtio-rng-device` has one, and userland is
        // built to degrade loudly without it.
        init_entropy();
    } else {
        console::println!("Ouroboros kernel: skipping virtio-blk (unconfirmed-safe virtio-mmio scan on this platform) - disk commands won't work this boot");
    }

    // Second disk path: USB mass storage over the xHCI driver, if the
    // multi-device scan activated one and nothing else mounted
    // (first-mounted-wins - virtio stays the QEMU dev loop's primary).
    // On real Parallels hardware the passed-through stick usually
    // attaches a few seconds *after* this point (confirmed by the
    // enumeration diagnostics), so this boot-time attempt mostly serves
    // QEMU; the shell's `mount` command covers the late-attach case.
    syscall::try_install_usb_block_device();

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
        let nic_intid = syscall::net_intid();
        unsafe {
            gic::configure(info);
            gic::init();
            gic::enable_interrupt(timer::INTID);
            // IRQ-driven NIC receive: enable the NIC's receive interrupt at
            // the GIC (an SPI - see virtio_net::intid / the GIC backends'
            // SPI paths) and tell the IRQ handler which INTID it is, so a
            // delivered frame wakes the network server immediately rather
            // than at the next tick's poll. Only when a NIC was actually
            // installed (init_net, behind virtio_mmio_probe_safe - QEMU
            // only); the tick-poll fallback covers everything otherwise.
            if let Some(intid) = nic_intid {
                gic::enable_interrupt(intid);
                exceptions::set_net_intid(intid);
            }
        }
        timer::arm(timer::TICK_INTERVAL_MS);
        if let Some(intid) = nic_intid {
            console::println!(
                "Ouroboros kernel: NIC receive interrupt enabled (GIC INTID {intid})"
            );
        }
    } else {
        console::println!(
            "Ouroboros kernel: skipping GIC/timer init (no MADT-confirmed interrupt controller on this platform) - no preemption this boot"
        );
    }

    // SAFETY: every loaded EL0 region was just mapped EL0-accessible above.
    // Every console mechanism has had its turn by now: say so if a
    // boot-time EL0 region was refused by `build_view` back when there was
    // nothing to say it through (the framebuffer platforms).
    mmu::report_deferred_warnings();
    unsafe { tasks::init(&program, fsd.as_ref(), cond.as_ref(), netd.as_ref(), accountd.as_ref()) };

    console::println!("Ouroboros kernel: shell ready - type and press Enter");

    // Hand the screen over to the console server. On a framebuffer-only
    // platform (Parallels), the kernel's own fbconsole and cond both draw
    // to the same framebuffer at independent cursors, so once cond is
    // loaded to render there, the kernel goes quiet on its console -
    // steady-state operational logs (task exited, mount/USB diagnostics)
    // would otherwise corrupt cond's output. Fault reports still print
    // (println_force). Only armed when the kernel console is actually a
    // framebuffer and cond exists to take it over; on a byte-stream
    // console (QEMU's UART) it stays off, keeping those logs for dev.
    if cond.is_some() && console::is_framebuffer() {
        console::set_quiet(true);
    }

    // IRQs stay masked at EL1 all the way into task 0: `tasks::start`'s
    // `eret` restores task 0's saved SPSR (0, EL0t with DAIF clear), and
    // THAT is what unmasks the tick, for EL0 only. There used to be an
    // explicit `msr daifclr, #2` here, which opened a window of a few
    // instructions in which a tick could land at EL1 inside `start`: the
    // trampoline would have saved that EL1 frame as task 0's context, and a
    // tick between `start`'s `msr elr_el1` and its `eret` would have sent
    // the `eret` to a kernel address in EL0 mode. Never seen (the window is
    // sub-microsecond), found by review of the single-core argument in
    // `synccell.rs`, which holds only if EL1 never runs unmasked.
    // From here on the tick is also what drives every further task switch
    // (`tasks::on_tick`).

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
    console::println!("Ouroboros kernel: virtio-console live (fallback - every other mechanism failed), build {BUILD}");
}

/// Under `\FBCON`: a solid white square at the top-right of the screen, the
/// `n`th from the right edge, drawn with plain stores and no console. For the
/// stretch just after `exit_boot_services` where there is no console yet to
/// print through: square 1 means the exit returned, square 2 that the
/// exception vectors are written (live at once on an EL1 handoff; on the
/// Pi's EL2 handoff, live from the drop to EL1 inside the identity-map
/// install, see `el2.rs`). The early console's clear wipes them, so
/// squares still on screen mean the boot stopped before that clear. White is
/// the same in every GOP pixel format.
fn progress_square(fb_info: Option<framebuffer::Info>, n: usize) {
    const SIDE: usize = 48;
    let Some(info) = fb_info else { return };
    let Some(x0) = info.width.checked_sub(n * (SIDE + 16)) else { return };
    let y0 = 16;
    if y0 + SIDE > info.height {
        return;
    }
    let base = info.base as *mut u32;
    for y in y0..y0 + SIDE {
        for x in x0..x0 + SIDE {
            // SAFETY: inside the framebuffer by the bounds above; the
            // firmware's identity map still covers it (see the early
            // console's SAFETY comment in `main`).
            unsafe { base.add(y * info.stride + x).write_volatile(0xffff_ffff) };
        }
    }
    let stride_bytes = (info.stride * 4) as u64;
    mmu::clean_to_poc(info.base + (y0 * info.stride + x0) as u64 * 4, (SIDE * 4) as u64, SIDE as u64, stride_bytes);
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
    console::println!("Ouroboros kernel: framebuffer console live (fallback - no byte-stream console installed), build {BUILD}");
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

    // Custody goes to the BLOCK cell for the filesystem server to
    // reach via the BLOCK_* syscalls - the kernel no longer mounts
    // (or contains) any filesystem itself; the server's own boot-time
    // auto-mount takes it from here.
    syscall::install_block_device(block::BlockDevice::Virtio(device));
    console::println!("Ouroboros kernel: block device installed for the filesystem server");
}

/// Brings up the virtio-net driver and hands it to the `NET` cell for the
/// network server (`netd`) to reach through the gated `NET_SEND`/`NET_RECV`
/// syscalls - the DMA-owning driver stays in EL1 (no IOMMU), the protocol
/// stack lives in userland (`netd`), the `BLOCK_*` -> fsd pattern. (Stage 1
/// proved the driver with an in-kernel ARP probe; Stage 2 moved all traffic
/// to `netd`, so this no longer sends anything itself.)
///
/// Silent when no NIC is attached: plain `make run`/`run-image` don't attach
/// one (only `make run-net`/`run-image-net` do), so a `NotFound` returns
/// quietly rather than logging on every normal dev boot.
fn init_net() {
    let mut device = match unsafe { virtio_net::Device::discover() } {
        Ok(d) => d,
        Err(virtio_net::Error::NotFound) => return, // no NIC this boot - stay quiet
        Err(e) => {
            console::println!("Ouroboros kernel: virtio-net discovery failed ({e})");
            return;
        }
    };
    if let Err(e) = unsafe { device.init() } {
        console::println!("Ouroboros kernel: virtio-net init failed ({e})");
        return;
    }
    let mac = device.mac();
    console::println!(
        "Ouroboros kernel: virtio-net ready, MAC {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    );

    // Hand the NIC to the NET cell for the network server (netd) to reach
    // via the gated NET_SEND/NET_RECV syscalls - the kernel keeps only the
    // DMA-owning driver, the BLOCK_* -> fsd pattern. The protocol stack
    // (ARP/IP/ICMP) and any actual traffic now live in userland; this
    // function no longer sends anything itself.
    syscall::install_net_device(device);
    console::println!("Ouroboros kernel: NIC installed for the network server");
}

/// Discovers and installs the virtio-rng entropy device the `RANDOM` syscall
/// serves. Absence is the ordinary case (only `-device virtio-rng-device`
/// provides one), so `NotFound` returns quietly - userland then sees
/// `RANDOM_UNAVAILABLE` and falls back to its documented weak path.
fn init_entropy() {
    let mut device = match unsafe { virtio_rng::Device::discover() } {
        Ok(d) => d,
        Err(virtio_rng::Error::NotFound) => return, // no RNG this boot - stay quiet
        Err(e) => {
            console::println!("Ouroboros kernel: virtio-rng discovery failed ({e})");
            return;
        }
    };
    if let Err(e) = unsafe { device.init() } {
        console::println!("Ouroboros kernel: virtio-rng init failed ({e})");
        return;
    }
    // Prove the device actually answers before advertising it: a driver that
    // initializes but returns nothing would hand userland a silent zero salt,
    // which is worse than having no device at all.
    match unsafe { device.next_u64() } {
        Some(_) => {
            syscall::install_rng_device(device);
            console::println!("Ouroboros kernel: virtio-rng ready, entropy available to userland");
        }
        None => console::println!("Ouroboros kernel: virtio-rng returned no entropy - not installed"),
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
