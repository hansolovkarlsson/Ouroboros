//! **Boot flag files**: empty files at the ESP's root that switch off one
//! boot step, so a hardware round trip can bisect a hang without a rebuild
//! (`touch /Volumes/<card>/NOXHCI`, boot, look; delete it, boot again).
//! Diagnostics for the bench, not configuration: every flag here names what
//! it switches off, and the boot log says when one is set.
//!
//! Two flags are not diagnostics but test faults: `\MSDSTALL` makes the
//! USB stick stall, so QEMU can exercise the bulk recovery that no ordinary
//! run reaches (it belongs on a test image, and `usb_msd.rs` ignores it for
//! any stick that is not QEMU's); `\EARLYFAULT` takes a fault inside the
//! firmware's code before the exit, so QEMU can exercise the early fault
//! reporter (`earlyfault.rs`) that only a board has needed.
//!
//! Read before `exit_boot_services`, through the same image file system
//! `bootid.rs` uses, and before the xHCI takeover in `main.rs`, which can take
//! a USB boot disk away. A flag that cannot be read (no file system, any
//! error) counts as absent: the default boot is the one that runs.

use uefi::boot;
use uefi::proto::media::file::{File, FileAttribute, FileMode};
use uefi::{cstr16, CStr16};

/// `\NOXHCI`: skip xHCI discovery entirely, so the kernel boots with no USB
/// (no keyboard, no USB storage). Settles whether a hang is inside
/// `pci::discover_xhci` at all.
const NO_XHCI: &CStr16 = cstr16!("\\NOXHCI");

/// `\XHCINOWR`: discover the xHCI controller as usual, including taking it
/// from firmware's driver, but skip the PCI command-register write that turns
/// on Memory Space and Bus Master. Separates that write from the takeover.
/// The controller is then not brought up after the exit either (with Memory
/// Space off its registers do not decode), so the boot goes on without USB
/// and how far it gets is the result.
const XHCI_NO_WRITE: &CStr16 = cstr16!("\\XHCINOWR");

/// `\FBCON`: do not install the discovered serial console after the exit,
/// so the framebuffer console on HDMI takes over. A serial console, when one
/// is discovered (the Pi's firmware describes one in ACPI SPCR), otherwise
/// wins, and then nothing after `exit_boot_services` reaches HDMI: without a
/// serial cable that looks exactly like a hang.
const FB_CONSOLE: &CStr16 = cstr16!("\\FBCON");

/// `\MSDSTALL`: make QEMU's stick stall, two ways, for `usb_msd.rs`'s
/// recovery to repair. The signature of every seventh CBW is corrupted (tag
/// 3 mod 7, each retry takes a tag too), which `usb-storage` answers with a
/// Bulk-OUT Stall, repaired by the BOT retry; and the CSW of every command
/// with tag 5 mod 7 is read as 12 bytes, which it answers with a Bulk-IN
/// Stall while the CSW stays owed, repaired in place. About 30 of each in a
/// boot from the stick. A test fault for `test-usb-hub.py --stall`,
/// honoured only for a stick whose INQUIRY vendor is `QEMU`: a real device
/// answers these differently, and could hang.
const MSD_STALL: &CStr16 = cstr16!("\\MSDSTALL");

/// `\EARLYFAULT`: the second test fault. Just before the xHCI takeover,
/// where the Pi 4's first serial boot died with only the firmware's one
/// line to show, ask the firmware to copy into an address nothing maps,
/// so the fault is taken inside the firmware's own code while its vectors
/// are still installed. What the serial console then shows is the early
/// fault reporter's dump (`earlyfault.rs`), or, if the reporter is not
/// armed, the firmware's line alone. For `scripts/test-early-fault.py`;
/// on a card it only ends the boot with that dump.
const EARLY_FAULT: &CStr16 = cstr16!("\\EARLYFAULT");

/// `\WALKFAULT`: the third test fault, for the reporter's own report.
/// With it set, the reporter reads an address nothing maps just before it
/// reads the first image header of its image-naming walk, so the report
/// faults inside itself the way a Pi 4's did on 2026-10-01 (frame 10, an
/// entry claiming an address below the board's RAM). What the console
/// must show is the register rows, printed before the walk, and then the
/// one line a nested fault prints, with `far` and the memory under read.
/// Only with `\EARLYFAULT`, which is what makes a report happen; for
/// `scripts/test-early-fault.py`.
const WALK_FAULT: &CStr16 = cstr16!("\\WALKFAULT");

/// `\SMPFAULT`: core 1 takes an undefined instruction right after its
/// `core 1 up` line (`smp.rs`), so `make test-smp` can see a secondary's
/// fault reported with its core number while the boot core's shell goes
/// on. A test fault, like `\EARLYFAULT`.
const SMP_FAULT: &CStr16 = cstr16!("\\SMPFAULT");

/// Which boot flags are set, read once by [`read`].
#[derive(Clone, Copy, Default)]
pub struct Flags {
    /// [`NO_XHCI`]
    pub no_xhci: bool,
    /// [`XHCI_NO_WRITE`]
    pub xhci_no_write: bool,
    /// [`FB_CONSOLE`]
    pub fb_console: bool,
    /// [`MSD_STALL`]
    pub msd_stall: bool,
    /// [`EARLY_FAULT`]
    pub early_fault: bool,
    /// [`WALK_FAULT`]
    pub walk_fault: bool,
    /// [`SMP_FAULT`]
    pub smp_fault: bool,
}

/// Reads every flag file at the ESP's root, opening the volume once. If the
/// volume cannot be opened, every flag is absent, and the log says so: a
/// flag file on the card that was never read must not look like one that
/// was read and changed nothing.
pub fn read() -> Flags {
    let root = boot::get_image_file_system(boot::image_handle())
        .and_then(|mut sfs| sfs.open_volume());
    let mut root = match root {
        Ok(root) => root,
        Err(e) => {
            log::warn!("Ouroboros kernel: boot flags not read ({:?}), all taken as absent", e.status());
            return Flags::default();
        }
    };
    let mut present = |path: &CStr16| {
        let set = root.open(path, FileMode::Read, FileAttribute::empty()).is_ok();
        if set {
            log::warn!("Ouroboros kernel: boot flag {path} is set");
        }
        set
    };
    Flags {
        no_xhci: present(NO_XHCI),
        xhci_no_write: present(XHCI_NO_WRITE),
        fb_console: present(FB_CONSOLE),
        msd_stall: present(MSD_STALL),
        early_fault: present(EARLY_FAULT),
        walk_fault: present(WALK_FAULT),
        smp_fault: present(SMP_FAULT),
    }
}
