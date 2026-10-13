//! The **boot partition**: which partition of which disk the firmware loaded
//! this image from, read from the image's own device path before
//! `ExitBootServices`. Step 1 of `docs/roadmap/roadmap-one-disk.md`.
//!
//! Nothing after the exit knows which disk the system booted from: the block
//! device is whichever disk is found first (`syscall.rs`'s `BLOCK`, virtio
//! before USB), and `fsd` mounts the first partition that validates. The
//! firmware does know. The `LoadedImageDevicePath` protocol on the image
//! handle is the path `LoadImage` was given, and on a disk it runs through a
//! `HardDrive` media node: the partition's number (from 1), its start LBA and
//! size in blocks, and the disk's MBR signature or the partition's GPT GUID.
//! This module reads that node once and keeps it; [`is_boot_disk`] matches a
//! disk against it (step 3, `main.rs`'s `install_boot_disk`), and step 4 will
//! hand it to `fsd`.
//!
//! The protocol is opened on the IMAGE handle, never on the device handle:
//! an exclusive open of a protocol on a device handle makes the firmware
//! disconnect the drivers on it, and the ESP is still to be read after this
//! (the boot identity's counter file).
//!
//! A path with no `HardDrive` node is a supported case, not an error: a boot
//! over a network would have none (predicted). It is logged as such, [`get`]
//! answers `None`, and every later step falls back to today's behaviour. A
//! QEMU vvfat directory (`make run`) does have one, read 2026-10-13: `HD(1,
//! MBR,0xBE1AFDFA,0x3F,0xFBFC1)`, partition 1 at LBA 63, and the only boot
//! seen so far whose MBR signature is not 0.

use crate::synccell::SyncCell;
use uefi::boot;
use uefi::proto::device_path::media::{PartitionFormat, PartitionSignature};
use uefi::proto::device_path::text::{AllowShortcuts, DisplayOnly};
use uefi::proto::device_path::{DevicePathNodeEnum, LoadedImageDevicePath};

/// The disk's identity as the firmware's `HardDrive` node states it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum DiskSignature {
    /// No signature: the node's signature type 0, or an MBR signature of 0,
    /// which every image this project built until 2026-10-13 had.
    None,
    /// The MBR's 32-bit disk signature, bytes 440 to 443 of sector 0.
    Mbr(u32),
    /// The GPT partition's unique GUID, as its 16 bytes on disk.
    Gpt([u8; 16]),
    /// A signature type the UEFI specification does not list (3 and up),
    /// kept apart from `None` so that a signature that was there is never
    /// logged or matched as if it were absent (the review of #285).
    Unknown(u8),
}

/// The partition the kernel image was loaded from.
#[derive(Clone, Copy)]
pub(crate) struct BootPartition {
    /// From 1, in the partition table's own order.
    pub(crate) number: u32,
    /// First block, in the disk's logical blocks.
    pub(crate) start_lba: u64,
    /// Size, in the disk's logical blocks.
    pub(crate) size_blocks: u64,
    pub(crate) signature: DiskSignature,
}

static BOOT_PARTITION: SyncCell<Option<BootPartition>> = SyncCell::new(None);

/// The boot partition, or `None` when the firmware's path named none. Written
/// once, by [`discover`], before anything could read it.
pub(crate) fn get() -> Option<BootPartition> {
    // SAFETY: `discover` finished before exit_boot_services, before any
    // secondary core started, and nothing writes it after (class A in
    // docs/roadmap/roadmap-smp.md's inventory).
    unsafe { *BOOT_PARTITION.get() }
}

/// Read the image's device path, log it, and keep its `HardDrive` node. Must
/// run before `exit_boot_services`.
pub(crate) fn discover() {
    let protocol = match boot::open_protocol_exclusive::<LoadedImageDevicePath>(boot::image_handle()) {
        Ok(protocol) => protocol,
        Err(e) => {
            log::info!("Ouroboros kernel: boot partition: unknown, the image has no device path ({:?})", e.status());
            return;
        }
    };
    // An image loaded from a memory buffer (a chainloader, a network boot)
    // has the protocol installed with a NULL interface, and ScopedProtocol's
    // Deref unwraps it: `get` is the only access that cannot panic (the
    // review of #285).
    let Some(path) = protocol.get() else {
        log::info!("Ouroboros kernel: boot partition: unknown, the image's device path is null (loaded from memory)");
        return;
    };
    match path.to_string16(DisplayOnly(false), AllowShortcuts(false)) {
        Ok(text) => log::info!("Ouroboros kernel: booted from {text}"),
        Err(_) => log::info!("Ouroboros kernel: booted from a path the firmware cannot print"),
    }

    // The FIRST HardDrive node, on purpose. Where a partition holds a table of
    // its own, edk2's path nests them, HD(outer)/HD(inner), and the image came
    // from the innermost; but what step 3 matches is the disk's own table,
    // which holds the outermost. A nesting is logged so it is seen.
    let mut nodes = path.node_iter().filter_map(|node| match node.as_enum() {
        Ok(DevicePathNodeEnum::MediaHardDrive(hd)) => Some(hd),
        _ => None,
    });
    let node = nodes.next();
    let nested = nodes.count();
    if nested > 0 {
        log::info!("Ouroboros kernel: boot partition: {nested} more partition(s) nested inside it, keeping the disk's own (the first)");
    }
    let Some(hd) = node else {
        log::info!("Ouroboros kernel: boot partition: unknown, the path names no partition");
        return;
    };
    let signature = match hd.partition_signature() {
        PartitionSignature::Mbr(bytes) if u32::from_le_bytes(bytes) != 0 => DiskSignature::Mbr(u32::from_le_bytes(bytes)),
        PartitionSignature::Guid(guid) => DiskSignature::Gpt(guid.to_bytes()),
        PartitionSignature::Unknown { signature_type, .. } => DiskSignature::Unknown(signature_type),
        _ => DiskSignature::None,
    };
    let found = BootPartition {
        number: hd.partition_number(),
        start_lba: hd.partition_start(),
        size_blocks: hd.partition_size(),
        signature,
    };
    let table = match hd.partition_format() {
        PartitionFormat::MBR => "MBR",
        PartitionFormat::GPT => "GPT",
        _ => "an unknown table",
    };
    match signature {
        DiskSignature::None => log::info!(
            "Ouroboros kernel: boot partition: {} of {table} at LBA {}, {} blocks, disk signature none",
            found.number, found.start_lba, found.size_blocks
        ),
        DiskSignature::Mbr(sig) => log::info!(
            "Ouroboros kernel: boot partition: {} of {table} at LBA {}, {} blocks, disk signature {sig:#010x}",
            found.number, found.start_lba, found.size_blocks
        ),
        DiskSignature::Gpt(guid) => log::info!(
            "Ouroboros kernel: boot partition: {} of {table} at LBA {}, {} blocks, partition GUID {}",
            found.number, found.start_lba, found.size_blocks, uefi::Guid::from_bytes(guid)
        ),
        DiskSignature::Unknown(kind) => log::info!(
            "Ouroboros kernel: boot partition: {} of {table} at LBA {}, {} blocks, signature of unknown type {kind}",
            found.number, found.start_lba, found.size_blocks
        ),
    }

    // SAFETY: one core, before exit_boot_services; no reader exists yet.
    unsafe {
        *BOOT_PARTITION.get() = Some(found);
    }
}

/// Whether a disk is the one the boot partition is on, read through `read`
/// (one 512-byte sector by LBA, `false` on a failed read). `None` when the
/// boot partition is unknown, so the caller keeps today's order. Step 3 of
/// docs/roadmap/roadmap-one-disk.md.
///
/// The disk's own table must hold the boot partition: on MBR, entry
/// `number` with the same start and size, and the disk's signature (bytes
/// 440-443) equal to the one the firmware read there, 0 included, since the
/// node's field is that same word; on GPT, entry `number` with the same
/// start, size and unique GUID. A signature of a type the specification does
/// not list is matched on the table alone.
pub(crate) fn is_boot_disk(read: &mut dyn FnMut(u64, &mut [u8; 512]) -> bool) -> Option<bool> {
    let boot = get()?;
    let mut sector = [0u8; 512];
    if !read(0, &mut sector) || sector[510] != 0x55 || sector[511] != 0xaa {
        return Some(false);
    }
    let gpt = sector[446 + 4] == 0xee;
    if !gpt {
        let disk_sig = u32::from_le_bytes([sector[440], sector[441], sector[442], sector[443]]);
        match boot.signature {
            DiskSignature::Mbr(sig) if sig != disk_sig => return Some(false),
            DiskSignature::None if disk_sig != 0 => return Some(false),
            DiskSignature::Gpt(_) => return Some(false),
            _ => {}
        }
        if !(1..=4).contains(&boot.number) {
            return Some(false);
        }
        let entry = 446 + 16 * (boot.number as usize - 1);
        let start = u32::from_le_bytes([sector[entry + 8], sector[entry + 9], sector[entry + 10], sector[entry + 11]]);
        let size = u32::from_le_bytes([sector[entry + 12], sector[entry + 13], sector[entry + 14], sector[entry + 15]]);
        return Some(sector[entry + 4] != 0 && u64::from(start) == boot.start_lba && u64::from(size) == boot.size_blocks);
    }
    // GPT: the primary header at LBA 1, then the boot entry in the array.
    if !read(1, &mut sector) || &sector[0..8] != b"EFI PART" {
        return Some(false);
    }
    let field = |s: &[u8; 512], at: usize, len: usize| s[at..at + len].iter().rev().fold(0u64, |v, b| v << 8 | u64::from(*b));
    let array_lba = field(&sector, 72, 8);
    let count = field(&sector, 80, 4);
    let entry_size = field(&sector, 84, 4);
    if boot.number == 0 || u64::from(boot.number) > count || !(128..=512).contains(&entry_size) || 512 % entry_size != 0 {
        return Some(false);
    }
    let offset = (u64::from(boot.number) - 1) * entry_size;
    if !read(array_lba + offset / 512, &mut sector) {
        return Some(false);
    }
    let at = (offset % 512) as usize;
    let first = field(&sector, at + 32, 8);
    let last = field(&sector, at + 40, 8);
    let mut unique = [0u8; 16];
    unique.copy_from_slice(&sector[at + 16..at + 32]);
    let guid_ok = match boot.signature {
        DiskSignature::Gpt(guid) => guid == unique,
        DiskSignature::Unknown(_) => true,
        _ => false,
    };
    Some(guid_ok && first == boot.start_lba && last + 1 == first + boot.size_blocks)
}
