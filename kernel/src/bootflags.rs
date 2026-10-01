//! **Boot flag files**: empty files at the ESP's root that switch off one
//! boot step, so a hardware round trip can bisect a hang without a rebuild
//! (`touch /Volumes/<card>/NOXHCI`, boot, look; delete it, boot again).
//! Diagnostics for the bench, not configuration: every flag here names what
//! it switches off, and the boot log says when one is set.
//!
//! One flag is not a diagnostic but a test fault: `\MSDSTALL` makes the
//! USB stick stall, so QEMU can exercise the bulk recovery that no ordinary
//! run reaches. It belongs on a test image, and `usb_msd.rs` ignores it for
//! any stick that is not QEMU's.
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

/// `\MSDSTALL`: corrupt the signature of every seventh CBW `usb_msd.rs`
/// sends. QEMU's `usb-storage` answers a bad signature with a Stall, so the
/// bulk recovery (`xhci::reset_storage_endpoint` and the BOT retry) runs
/// once per seven commands, about 30 times in a boot from the stick. A test
/// fault for `test-usb-hub.py --stall`, honoured only for a stick whose
/// INQUIRY vendor is `QEMU`: a real device answers a bad CBW differently,
/// and would hang.
const MSD_STALL: &CStr16 = cstr16!("\\MSDSTALL");

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
    }
}
