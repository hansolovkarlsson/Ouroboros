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
pub const NO_XHCI: &CStr16 = cstr16!("\\NOXHCI");

/// `\XHCINOWR`: discover the xHCI controller as usual, including taking it
/// from firmware's driver, but skip the PCI command-register write that turns
/// on Memory Space and Bus Master. Separates that write from the takeover.
/// The controller is then not brought up after the exit either (with Memory
/// Space off its registers do not decode), so the boot goes on without USB
/// and how far it gets is the result.
pub const XHCI_NO_WRITE: &CStr16 = cstr16!("\\XHCINOWR");

/// Whether the flag file `path` exists at the ESP's root.
pub fn present(path: &CStr16) -> bool {
    let Ok(mut sfs) = boot::get_image_file_system(boot::image_handle()) else {
        return false;
    };
    let Ok(mut root) = sfs.open_volume() else {
        return false;
    };
    let set = root.open(path, FileMode::Read, FileAttribute::empty()).is_ok();
    if set {
        log::warn!("Ouroboros kernel: boot flag {path} is set");
    }
    set
}
