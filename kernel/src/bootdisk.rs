//! The **boot partition**: which partition of which disk the firmware loaded
//! this image from, read from the image's own device path before
//! `ExitBootServices`. Step 1 of `docs/roadmap/roadmap-one-disk.md`.
//!
//! Until one-disk step 3 nothing after the exit knew which disk the system
//! booted from: the block device was whichever disk was found first
//! (`syscall.rs`'s `BLOCK`, virtio before USB). The firmware does know. The `LoadedImageDevicePath` protocol on the image
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
    /// The node carries no signature (its signature type 0), so a disk is
    /// matched on its table alone (the review of #287: it had also stood for
    /// an MBR signature of 0, which made "no signature" a claim of 0).
    None,
    /// The MBR's 32-bit disk signature, bytes 440 to 443 of sector 0, 0
    /// included: every image this project built until 2026-10-13 had 0.
    Mbr(u32),
    /// The GPT partition's unique GUID, as its 16 bytes on disk.
    Gpt([u8; 16]),
    /// A signature type the UEFI specification does not list (3 and up),
    /// kept apart from `None` so that a signature that was there is never
    /// logged or matched as if it were absent (the review of #285).
    Unknown(u8),
}

/// The partition table the firmware says the boot partition is in.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Table {
    Mbr,
    Gpt,
    /// A format the specification does not list: matched against nothing.
    Unknown,
}

/// The partition the kernel image was loaded from.
#[derive(Clone, Copy)]
pub(crate) struct BootPartition {
    /// The table the firmware read it from. A disk is read by this table,
    /// not by guessing from its sector 0, so a hybrid MBR (an 0xEE entry
    /// beside real ones) is read as the GPT the firmware booted (the review
    /// of #287).
    pub(crate) table: Table,
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
        PartitionSignature::Mbr(bytes) => DiskSignature::Mbr(u32::from_le_bytes(bytes)),
        PartitionSignature::Guid(guid) => DiskSignature::Gpt(guid.to_bytes()),
        PartitionSignature::Unknown { signature_type, .. } => DiskSignature::Unknown(signature_type),
        _ => DiskSignature::None,
    };
    let table = match hd.partition_format() {
        PartitionFormat::MBR => Table::Mbr,
        PartitionFormat::GPT => Table::Gpt,
        _ => Table::Unknown,
    };
    let found = BootPartition {
        table,
        number: hd.partition_number(),
        start_lba: hd.partition_start(),
        size_blocks: hd.partition_size(),
        signature,
    };
    let table = match table {
        Table::Mbr => "MBR",
        Table::Gpt => "GPT",
        Table::Unknown => "an unknown table",
    };
    match signature {
        DiskSignature::None => log::info!(
            "Ouroboros kernel: boot partition: {} of {table} at LBA {}, {} blocks, no signature",
            found.number, found.start_lba, found.size_blocks
        ),
        DiskSignature::Mbr(0) => log::info!(
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
/// (one 512-byte sector by LBA, `false` on a failed read) on a disk of
/// `capacity` sectors. `None` when the boot partition is unknown, or in a
/// table of a format the specification does not list, so the caller keeps
/// the order from before step 3 (docs/roadmap/roadmap-one-disk.md).
///
/// The disk is read by the table the firmware named, not by guessing from
/// its sector 0. On MBR: entry `number` with the same start and size, and,
/// when the node carries a signature, the disk's bytes 440-443 equal to it,
/// 0 included. On GPT: the primary header at LBA 1, or the backup at the
/// last sector when the primary is not there (as `fsd`'s partition.rs
/// reads it), then entry `number` with the same start, size and unique GUID.
/// A node with no signature, or one of a type the specification does not
/// list, is matched on the table alone. Every value read from the disk is
/// bounded with checked arithmetic: a disk only being asked must never halt
/// the boot (the review of #287 found a header that overflowed the sum).
pub(crate) fn is_boot_disk(read: &mut dyn FnMut(u64, &mut [u8; 512]) -> bool, capacity: u64) -> Option<bool> {
    let boot = get()?;
    match boot.table {
        Table::Unknown => None,
        Table::Mbr => Some(mbr_holds(&boot, read)),
        Table::Gpt => Some(gpt_holds(&boot, read, capacity)),
    }
}

fn le(bytes: &[u8]) -> u64 {
    bytes.iter().rev().fold(0u64, |v, b| v << 8 | u64::from(*b))
}

fn mbr_holds(boot: &BootPartition, read: &mut dyn FnMut(u64, &mut [u8; 512]) -> bool) -> bool {
    let mut sector = [0u8; 512];
    if !read(0, &mut sector) || sector[510] != 0x55 || sector[511] != 0xaa {
        return false;
    }
    let disk_sig = le(&sector[440..444]) as u32;
    match boot.signature {
        DiskSignature::Mbr(sig) if sig != disk_sig => return false,
        DiskSignature::Gpt(_) => return false,
        _ => {}
    }
    if !(1..=4).contains(&boot.number) {
        return false;
    }
    let entry = 446 + 16 * (boot.number as usize - 1);
    sector[entry + 4] != 0
        && le(&sector[entry + 8..entry + 12]) == boot.start_lba
        && le(&sector[entry + 12..entry + 16]) == boot.size_blocks
}

fn gpt_holds(boot: &BootPartition, read: &mut dyn FnMut(u64, &mut [u8; 512]) -> bool, capacity: u64) -> bool {
    let mut sector = [0u8; 512];
    let header_at = |lba: u64, sector: &mut [u8; 512], read: &mut dyn FnMut(u64, &mut [u8; 512]) -> bool| {
        read(lba, sector) && &sector[0..8] == b"EFI PART"
    };
    if !header_at(1, &mut sector, read) {
        let Some(last) = capacity.checked_sub(1) else { return false };
        if last <= 1 || !header_at(last, &mut sector, read) {
            return false;
        }
    }
    let array_lba = le(&sector[72..80]);
    let count = le(&sector[80..84]);
    let entry_size = le(&sector[84..88]);
    if boot.number == 0 || u64::from(boot.number) > count || !(128..=512).contains(&entry_size) || 512 % entry_size != 0 {
        return false;
    }
    let offset = (u64::from(boot.number) - 1) * entry_size;
    let Some(lba) = array_lba.checked_add(offset / 512) else { return false };
    if lba >= capacity || !read(lba, &mut sector) {
        return false;
    }
    let at = (offset % 512) as usize;
    let first = le(&sector[at + 32..at + 40]);
    let last = le(&sector[at + 40..at + 48]);
    let guid_ok = match boot.signature {
        DiskSignature::Gpt(guid) => guid[..] == sector[at + 16..at + 32],
        DiskSignature::Mbr(_) => false,
        DiskSignature::None | DiskSignature::Unknown(_) => true,
    };
    guid_ok && first == boot.start_lba && last.checked_add(1) == first.checked_add(boot.size_blocks)
}
