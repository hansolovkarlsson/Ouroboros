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
/// last sector when the primary does not validate, each with both CRCs
/// checked as `fsd`'s partition.rs checks them, then entry `number` with the
/// same start, size and unique GUID.
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
    let disk_sig = u32::from_le_bytes([sector[440], sector[441], sector[442], sector[443]]);
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
    // The primary header, else the backup at the last sector, each taken only
    // if it validates as fsd's partition.rs validates it (both CRCs): a stale
    // "EFI PART" left by an earlier table, or a corrupt primary the firmware
    // itself skipped, must not decide (the medium review of #287).
    if let Some(found) = gpt_entry(boot, read, 1, capacity) {
        return found;
    }
    match capacity.checked_sub(1) {
        Some(last) if last > 1 => gpt_entry(boot, read, last, capacity).unwrap_or(false),
        _ => false,
    }
}

/// The largest entry array read, as fsd's `MAX_GPT_ARR_SECTORS`: 128 entries
/// of 128 bytes is 32 sectors, and anything past 64 KiB is refused.
const MAX_GPT_ARRAY_SECTORS: u64 = 128;

/// Whether the GPT whose header is at `header_lba` holds the boot partition,
/// or `None` when that header or its entry array does not validate.
fn gpt_entry(
    boot: &BootPartition,
    read: &mut dyn FnMut(u64, &mut [u8; 512]) -> bool,
    header_lba: u64,
    capacity: u64,
) -> Option<bool> {
    let mut header = [0u8; 512];
    if !read(header_lba, &mut header) || &header[0..8] != b"EFI PART" {
        return None;
    }
    let header_size = le(&header[12..16]) as usize;
    if !(92..=512).contains(&header_size) {
        return None;
    }
    let stored = le(&header[16..20]) as u32;
    let mut zeroed = header;
    zeroed[16..20].fill(0);
    if crc32_finish(crc32_update(CRC_INIT, &zeroed[..header_size])) != stored {
        return None;
    }
    let array_lba = le(&header[72..80]);
    let count = le(&header[80..84]);
    let entry_size = le(&header[84..88]);
    let stored_array = le(&header[88..92]) as u32;
    if !(128..=512).contains(&entry_size) || 512 % entry_size != 0 {
        return None;
    }
    let array_bytes = count.checked_mul(entry_size).filter(|b| *b > 0 && *b <= MAX_GPT_ARRAY_SECTORS * 512)?;
    let array_end = array_lba.checked_add(array_bytes.div_ceil(512))?;
    if array_end > capacity {
        return None;
    }
    // One pass: the CRC over exactly `array_bytes`, and the boot entry kept
    // when its sector goes by. It counts only if the CRC holds.
    let wanted = u64::from(boot.number).checked_sub(1).map(|i| i * entry_size);
    let mut entry = None;
    let mut crc = CRC_INIT;
    let mut sector = [0u8; 512];
    let mut done = 0u64;
    let mut lba = array_lba;
    while done < array_bytes {
        if !read(lba, &mut sector) {
            return None;
        }
        let take = (array_bytes - done).min(512) as usize;
        crc = crc32_update(crc, &sector[..take]);
        if let Some(offset) = wanted.filter(|o| (done..done + take as u64).contains(o)) {
            let at = (offset - done) as usize;
            let mut unique = [0u8; 16];
            unique.copy_from_slice(&sector[at + 16..at + 32]);
            entry = Some((unique, le(&sector[at + 32..at + 40]), le(&sector[at + 40..at + 48])));
        }
        done += take as u64;
        lba += 1;
    }
    if crc32_finish(crc) != stored_array {
        return None;
    }
    let Some((unique, first, last)) = entry else { return Some(false) };
    let guid_ok = match boot.signature {
        DiskSignature::Gpt(guid) => guid == unique,
        DiskSignature::Mbr(_) => false,
        DiskSignature::None | DiskSignature::Unknown(_) => true,
    };
    // Both ends must be computed: two overflows are not a match (the medium
    // review of #287 found `None == None` taken as one).
    let ends = matches!((last.checked_add(1), first.checked_add(boot.size_blocks)), (Some(a), Some(b)) if a == b);
    Some(guid_ok && first == boot.start_lba && ends)
}

/// CRC-32 (IEEE, reflected `0xEDB88320`), the one GPT and `zlib.crc32` use,
/// table-free and bitwise as fsd's partition.rs has it: run at most over one
/// header and a 64 KiB entry array at boot.
const CRC_INIT: u32 = 0xFFFF_FFFF;

fn crc32_update(mut crc: u32, data: &[u8]) -> u32 {
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    crc
}

fn crc32_finish(crc: u32) -> u32 {
    crc ^ 0xFFFF_FFFF
}

/// The boot partition's start LBA when the disk read through `read` holds it,
/// else 0: what `BLOCK_BOOT_START` answers for a device about to be
/// installed. For `MOUNT`'s rescan, which installs a stick whatever it is,
/// so a boot stick taken there after a failed boot-time init still tells
/// `fsd` (the high review of #288).
pub(crate) fn boot_start(read: &mut dyn FnMut(u64, &mut [u8; 512]) -> bool, capacity: u64) -> u64 {
    match (is_boot_disk(read, capacity), get()) {
        (Some(true), Some(boot)) => boot.start_lba,
        _ => 0,
    }
}
