#![no_main]
#![no_std]

extern crate alloc;

mod acpi;
mod console;
mod devicetree;
mod exceptions;
mod fat32;
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
    }

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
    unsafe { mmu::install_identity_map(&memory_map, [(program.base, program.size), tasks::idle_region()]) };
    console::println!("Ouroboros kernel: identity map installed, MMU running on our own tables");

    // Phase 3a (docs/roadmap.md): the first piece of a runtime storage
    // stack, proven end to end by reading back a sector and checking it
    // against a value nothing but a real disk read could produce. Not
    // fatal if it fails - nothing else depends on this driver yet, unlike
    // `program` above.
    probe_virtio_blk();

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

/// Discovers and initializes the virtio-blk device, then reads sector 0
/// back and checks it for the MBR boot signature (`0x55 0xAA` at bytes
/// 510-511) - proof the whole pipeline (discovery, feature negotiation,
/// virtqueue setup, a real request round-trip) actually moved bytes off
/// the disk, not just that no error was returned. `esp.img`'s first
/// sector is a real MBR (see the Makefile/`README`), so this signature
/// is a property of the actual disk contents, not something this code
/// could produce by accident.
fn probe_virtio_blk() {
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
    match unsafe { device.read_sector(0, &mut sector) } {
        Ok(()) => {
            let signature = (sector[511] as u16) << 8 | sector[510] as u16;
            console::println!(
                "Ouroboros kernel: virtio-blk read sector 0, boot signature {signature:#06x} ({})",
                if signature == 0xaa55 { "valid MBR" } else { "unexpected" }
            );
        }
        Err(e) => {
            console::println!("Ouroboros kernel: virtio-blk read failed ({e})");
            return;
        }
    }

    probe_fat32(device);
}

/// Phase 3b (docs/roadmap.md): mounts the FAT32 partition on `device`,
/// lists `\EFI\BOOT`, and reads `BOOTAA64.EFI` and
/// `\EFI\OUROBORO\INIT.CFG` back - proof the reader actually walks real
/// directory/FAT/cluster-chain structures, not just that mounting didn't
/// error. `OUROBORO`, not `OUROBOROS` (see `loader.rs`'s `CONFIG_PATH`
/// doc comment): every path component used here fits an 8.3 short name
/// cleanly, which matters because this reader doesn't parse long-filename
/// (LFN) entries yet (see fat32.rs's module doc comment) - a real,
/// confirmed gap this project sidestepped by renaming its own directory
/// rather than parsing around it. Only works when booted via
/// `make run-image` (real FAT32) - see fat32.rs's module doc comment for
/// why `make run`'s vvfat (FAT16) can't satisfy this.
fn probe_fat32(device: virtio_blk::Device) {
    let mut fs = match fat32::Fs::mount(device) {
        Ok(fs) => fs,
        Err(e) => {
            console::println!("Ouroboros kernel: FAT32 mount failed ({e})");
            return;
        }
    };
    console::println!("Ouroboros kernel: FAT32 mounted");

    let list_result = fs.list_dir("/EFI/BOOT", |name, is_dir, size| {
        console::println!("Ouroboros kernel:   {name}{} ({size} bytes)", if is_dir { "/" } else { "" });
    });
    if let Err(e) = list_result {
        console::println!("Ouroboros kernel: FAT32 list /EFI/BOOT failed ({e})");
        return;
    }

    // Independently checkable at test time against `ls -la` on the built
    // BOOTAA64.efi (not hardcoded here - its size is this same kernel
    // binary's own size, which would make a hardcoded expected value
    // self-referential and wrong the moment this very code changes it)
    // and its PE header magic ("MZ") - properties of the actual file
    // contents, not something a buggy reader could produce by accident.
    let mut buf = [0u8; 128];
    match fs.read_file("/EFI/BOOT/BOOTAA64.EFI", &mut buf) {
        Ok(size) => {
            console::println!(
                "Ouroboros kernel: FAT32 read BOOTAA64.EFI, size {size} bytes, magic {:02x} {:02x} ({})",
                buf[0],
                buf[1],
                if &buf[0..2] == b"MZ" { "valid PE magic" } else { "unexpected magic" }
            );
        }
        Err(e) => console::println!("Ouroboros kernel: FAT32 read BOOTAA64.EFI failed ({e})"),
    }

    // The same file loader.rs read via UEFI boot services earlier this
    // boot (see the "loaded shell program" log line above) - reading it
    // again here, independently, via the runtime driver instead, and
    // getting the same content back is a real cross-check, not a
    // duplicate demo.
    let mut cfg = [0u8; 64];
    match fs.read_file("/EFI/OUROBORO/INIT.CFG", &mut cfg) {
        Ok(size) => {
            let n = (size as usize).min(cfg.len());
            let text = core::str::from_utf8(&cfg[..n]).unwrap_or("<not utf8>");
            console::println!("Ouroboros kernel: FAT32 read INIT.CFG ({size} bytes): {text}");
        }
        Err(e) => console::println!("Ouroboros kernel: FAT32 read INIT.CFG failed ({e})"),
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
