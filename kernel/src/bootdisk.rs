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
//! This module reads that node once and keeps it; step 3 matches a disk
//! against it, and step 4 hands it to `fsd`.
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
    #[allow(dead_code, reason = "matched against a disk by step 3 of docs/roadmap/roadmap-one-disk.md")]
    pub(crate) signature: DiskSignature,
}

static BOOT_PARTITION: SyncCell<Option<BootPartition>> = SyncCell::new(None);

/// The boot partition, or `None` when the firmware's path named none. Written
/// once, by [`discover`], before anything could read it.
#[allow(dead_code, reason = "read by step 3 of docs/roadmap/roadmap-one-disk.md, the block device chosen by it")]
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
