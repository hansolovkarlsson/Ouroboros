//! **Boot flag files**: empty files at the ESP's root that switch off one
//! boot step, so a hardware round trip can bisect a hang without a rebuild
//! (`touch /Volumes/<card>/NOXHCI`, boot, look; delete it, boot again).
//! Diagnostics for the bench, not configuration: every flag here names what
//! it switches off, and the boot log says when one is set.
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

/// Which boot flags are set, read once by [`read`].
#[derive(Clone, Copy, Default)]
pub struct Flags {
    /// [`NO_XHCI`]
    pub no_xhci: bool,
    /// [`XHCI_NO_WRITE`]
    pub xhci_no_write: bool,
    /// [`FB_CONSOLE`]
    pub fb_console: bool,
}

/// Reads every flag file at the ESP's root, opening the volume once. If the
/// volume cannot be opened, every flag is absent.
pub fn read() -> Flags {
    let Ok(mut sfs) = boot::get_image_file_system(boot::image_handle()) else {
        return Flags::default();
    };
    let Ok(mut root) = sfs.open_volume() else {
        return Flags::default();
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
    }
}
