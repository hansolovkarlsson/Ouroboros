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

// The boot partition's types and the matcher live in the pure `parttable`
// crate since 2026-10-14, shared with `fsd`'s partition reader and tested on
// the host (`make test`), where every case no QEMU rig sets up is reached.
pub(crate) use parttable::{Boot, BootPartition, DiskSignature, Table};

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
/// `capacity` sectors: [`parttable::is_boot_disk`] against the partition the
/// firmware named, whose doc states the rules. `None` when the boot
/// partition is unknown, or in a table of a format the specification does
/// not list, so the caller keeps the order from before step 3
/// (docs/roadmap/roadmap-one-disk.md).
pub(crate) fn is_boot_disk(read: &mut dyn FnMut(u64, &mut [u8; 512]) -> bool, capacity: u64) -> Option<Boot> {
    parttable::is_boot_disk(&get()?, read, capacity)
}

/// What `BLOCK_BOOT_START` answers, read from the installed device's table at
/// every call (since 2026-10-30): [`parttable::boot_start`], the boot
/// partition's start while the disk, as `fsd` reads it, holds it, else 0, and
/// 0 when the firmware named no partition. A rewritten table is answered as
/// it now stands, a boot stick `MOUNT`'s rescan takes after a failed
/// boot-time init still tells `fsd` (the high review of #288), and a failed
/// read is an error, never 0, so `fsd` does not take a failing boot disk for
/// one without the ESP. Asked again once on a failed read, as D6 asks at
/// boot (the third medium review of #377), but not logged: the call is
/// `fsd`'s, which says what it does with the error (the second).
pub(crate) fn boot_start(read: &mut dyn FnMut(u64, &mut [u8; 512]) -> bool, capacity: u64) -> Result<u64, parttable::Unreadable> {
    let Some(boot) = get() else { return Ok(0) };
    parttable::boot_start(&boot, read, capacity).or_else(|_| parttable::boot_start(&boot, read, capacity))
}

/// [`is_boot_disk`], asked again once when it answered
/// [`Boot::Unreadable`], and logged when it does so again: decision D6
/// (Hans, 2026-10-13, docs/roadmap/roadmap-one-disk.md), "retry once, then
/// log". The caller then treats the disk as not the boot disk, and the line
/// has said the read failed rather than that the table did not match. (The
/// first review of #291 had it taken as the boot disk instead, which the
/// second review found putting a dead or unrelated disk first; that went
/// back to Hans as a question rather than be decided again in a review.)
pub(crate) fn is_boot_disk_retrying(
    read: &mut dyn FnMut(u64, &mut [u8; 512]) -> bool,
    capacity: u64,
    name: &str,
) -> Option<Boot> {
    let answer = is_boot_disk(read, capacity);
    if answer != Some(Boot::Unreadable) {
        return answer;
    }
    crate::console::println!("Ouroboros kernel: {name}: a read of its partition table failed - asking again");
    let answer = is_boot_disk(read, capacity);
    if answer == Some(Boot::Unreadable) {
        crate::console::println!(
            "Ouroboros kernel: {name}: reading its partition table failed twice - taken as not the boot disk, though it may be"
        );
    }
    answer
}

/// The boot partition's start LBA when [`is_boot_disk`] answered yes, else
/// 0, for the boot path's install line (the medium review of #288 found two
/// copies). `BLOCK_BOOT_START` itself is [`boot_start`]. A firmware
/// counting in blocks other than 512 bytes cannot be answered wrongly here:
/// its LBAs would not match the disk's table, and the disk is no boot disk.
pub(crate) fn start_if(matched: Option<Boot>) -> u64 {
    match (matched, get()) {
        (Some(Boot::Yes), Some(boot)) => boot.start_lba,
        _ => 0,
    }
}
