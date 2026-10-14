//! Partition tables, read: the MBR and GPT parsing the kernel and `fsd` both
//! need, once. The kernel asks whether a disk is the one it booted from
//! ([`is_boot_disk`], one-disk step 3); `fsd` lists a disk's partitions to
//! find its root ([`discover`]). Until 2026-10-14 each had its own copy of
//! the GPT validation (both CRCs, the backup header, the bounds) and of the
//! CRC itself, and only QEMU boots reached either (`ROADMAP.md`, "The
//! boot-disk matcher in a pure crate", from the medium review of #287).
//!
//! A disk is read through a closure, one 512-byte sector by LBA, `false` on
//! a failed read ([`ReadSector`]), so the kernel passes its block device and
//! `fsd` its `BLOCK_READ` call, and the tests pass a disk built in memory.
//! A failed read is kept apart from a table that does not validate wherever
//! the answer depends on it: decision D6 (Hans, 2026-10-13) takes an
//! unreadable disk as not the boot disk, with a line saying the read failed,
//! and that needs the matcher to say which it was.
//!
//! Every value read from a disk is bounded with checked arithmetic: a disk
//! only being asked must never panic the kernel or `fsd` (the review of #287
//! found a header built to overflow the sum).
//!
//! Pure: no I/O, no syscalls, no heap, so it builds and is tested on the
//! host (`make test`).

#![no_std]

/// The sector size every table here is read in.
pub const SECTOR: usize = 512;

/// One sector read by LBA into the buffer, `false` when the read failed.
pub type ReadSector<'a> = dyn FnMut(u64, &mut [u8; SECTOR]) -> bool + 'a;

// --- CRC-32 -----------------------------------------------------------------

/// The initial CRC-32 register. Fold data in with [`crc32_update`], then
/// [`crc32_finish`]; [`crc32`] is the whole-buffer form.
pub const CRC_INIT: u32 = 0xFFFF_FFFF;

/// CRC-32 (IEEE 802.3, reflected polynomial `0xEDB88320`), the one GPT and
/// `zlib.crc32` use. Table-free and bitwise: run at most over one header and
/// a 64 KiB entry array, so speed is no concern and a 1 KiB table would only
/// cost space.
pub fn crc32(data: &[u8]) -> u32 {
    crc32_finish(crc32_update(CRC_INIT, data))
}

/// Fold `data` into a running register, so an array can be checked a sector
/// at a time.
pub fn crc32_update(mut crc: u32, data: &[u8]) -> u32 {
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    crc
}

/// The final inversion, register to stored value.
pub fn crc32_finish(crc: u32) -> u32 {
    crc ^ 0xFFFF_FFFF
}

fn le(bytes: &[u8]) -> u64 {
    bytes.iter().rev().fold(0u64, |v, b| v << 8 | u64::from(*b))
}

// --- MBR --------------------------------------------------------------------

/// One of an MBR's four entries, as it stands: an unused one has type 0.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct MbrEntry {
    pub kind: u8,
    pub start: u32,
    pub sectors: u32,
}

/// The MBR's entry `index` (0 to 3) in `sector`.
pub fn mbr_entry(sector: &[u8; SECTOR], index: usize) -> MbrEntry {
    let e = 0x1BE + 16 * (index & 3);
    MbrEntry {
        kind: sector[e + 4],
        start: u32::from_le_bytes([sector[e + 8], sector[e + 9], sector[e + 10], sector[e + 11]]),
        sectors: u32::from_le_bytes([sector[e + 12], sector[e + 13], sector[e + 14], sector[e + 15]]),
    }
}

/// Whether `sector` ends in the `0x55 0xAA` boot signature.
pub fn has_boot_signature(sector: &[u8; SECTOR]) -> bool {
    sector[510] == 0x55 && sector[511] == 0xAA
}

/// The MBR's 32-bit disk signature, bytes 440 to 443.
pub fn mbr_signature(sector: &[u8; SECTOR]) -> u32 {
    u32::from_le_bytes([sector[440], sector[441], sector[442], sector[443]])
}

/// The type of a GPT's protective MBR entry.
pub const PROTECTIVE: u8 = 0xEE;

/// Whether `sector` is a GPT's protective MBR: the boot signature and an
/// entry of type 0xEE, also in a hybrid MBR that has real entries beside it.
pub fn is_protective(sector: &[u8; SECTOR]) -> bool {
    has_boot_signature(sector) && (0..4).any(|i| mbr_entry(sector, i).kind == PROTECTIVE)
}

/// Whether the disk is read as GPT: "EFI PART" at LBA 1, or a protective
/// MBR, so a GPT whose primary header is damaged is still known as one and
/// read from its backup. `None` when a read failed. The one test, so `fsd`'s
/// `format`, which writes MBR partitions only, refuses exactly the disks
/// [`discover`] reads as GPT (the medium review of #288).
pub fn is_gpt(read: &mut ReadSector) -> Option<bool> {
    let mut lba1 = [0u8; SECTOR];
    let mut lba0 = [0u8; SECTOR];
    if !read(1, &mut lba1) || !read(0, &mut lba0) {
        return None;
    }
    Some(&lba1[0..8] == b"EFI PART" || is_protective(&lba0))
}

// --- GPT --------------------------------------------------------------------

/// What one GPT copy turned out to be.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GptCopy {
    /// The header and the entry array both passed their CRCs.
    Valid,
    /// Missing, malformed, out of bounds, or failing a CRC.
    Invalid,
    /// A read it needed failed.
    Unreadable,
}

/// The primary header's LBA.
pub const GPT_PRIMARY: u64 = 1;

/// The largest entry array read: 128 entries of 128 bytes is 32 sectors,
/// and anything past 64 KiB is refused rather than read unbounded.
pub const MAX_GPT_ARRAY_SECTORS: u64 = 128;

/// The backup header's LBA on a disk of `capacity` sectors: the last, when
/// there is one past the primary.
pub fn gpt_backup(capacity: u64) -> Option<u64> {
    capacity.checked_sub(1).filter(|last| *last > GPT_PRIMARY)
}

/// Read and validate the GPT copy whose header is at `header_lba`, by the
/// UEFI specification's rules: the header's CRC over `HeaderSize` bytes
/// with its CRC field as zero, then the array's CRC over exactly `count *
/// entry size` bytes. Each entry is handed to `entry` with its index as the
/// array goes by, in the same pass as the CRC, so a caller must keep what it
/// is handed and act on it only when this answers [`GptCopy::Valid`]. With
/// `capacity` known, an array reaching past the disk is invalid.
pub fn read_gpt(
    read: &mut ReadSector,
    header_lba: u64,
    capacity: Option<u64>,
    entry: &mut dyn FnMut(u64, &[u8]),
) -> GptCopy {
    let mut header = [0u8; SECTOR];
    if !read(header_lba, &mut header) {
        return GptCopy::Unreadable;
    }
    if &header[0..8] != b"EFI PART" {
        return GptCopy::Invalid;
    }
    let header_size = le(&header[12..16]) as usize;
    if !(92..=SECTOR).contains(&header_size) {
        return GptCopy::Invalid;
    }
    let stored = le(&header[16..20]) as u32;
    let mut zeroed = header;
    zeroed[16..20].fill(0);
    if crc32(&zeroed[..header_size]) != stored {
        return GptCopy::Invalid;
    }
    let array_lba = le(&header[72..80]);
    let count = le(&header[80..84]);
    let entry_size = le(&header[84..88]);
    let stored_array = le(&header[88..92]) as u32;
    // A power of two from 128 to a sector, so an entry never straddles two.
    if !(128..=SECTOR as u64).contains(&entry_size) || !(SECTOR as u64).is_multiple_of(entry_size) {
        return GptCopy::Invalid;
    }
    let Some(array_bytes) = count
        .checked_mul(entry_size)
        .filter(|b| *b > 0 && *b <= MAX_GPT_ARRAY_SECTORS * SECTOR as u64)
    else {
        return GptCopy::Invalid;
    };
    let Some(array_end) = array_lba.checked_add(array_bytes.div_ceil(SECTOR as u64)) else {
        return GptCopy::Invalid;
    };
    if capacity.is_some_and(|c| array_end > c) {
        return GptCopy::Invalid;
    }
    let mut crc = CRC_INIT;
    let mut sector = [0u8; SECTOR];
    let mut done = 0u64;
    let mut lba = array_lba;
    while done < array_bytes {
        if !read(lba, &mut sector) {
            return GptCopy::Unreadable;
        }
        let take = (array_bytes - done).min(SECTOR as u64) as usize;
        crc = crc32_update(crc, &sector[..take]);
        let mut at = 0;
        while at + entry_size as usize <= take {
            entry((done + at as u64) / entry_size, &sector[at..at + entry_size as usize]);
            at += entry_size as usize;
        }
        done += take as u64;
        lba += 1;
    }
    if crc32_finish(crc) != stored_array {
        return GptCopy::Invalid;
    }
    GptCopy::Valid
}

// --- Partitions, for a root -------------------------------------------------

/// One partition as [`discover`] finds it: where it starts, and whether its
/// type marks it as a root filesystem. The type is what lets `fsd` name the
/// root on a disk with more than one partition beside the boot one, rather
/// than take the first that mounts (`docs/roadmap/roadmap-one-disk.md`).
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Partition {
    pub start: u64,
    pub root: bool,
}

/// The MBR type of a Linux filesystem, which every MBR root of ours carries.
/// MBR has no type for a root as such; this is the nearest, and a data
/// partition can carry it too, a cost Hans accepted on 2026-10-14.
pub const MBR_LINUX: u8 = 0x83;

/// The Discoverable Partitions Specification's "Root Partition (64-bit
/// ARM/AArch64)", `b921b045-1df0-41c3-af44-4c6f280d3fae`, in the on-disk
/// byte order (the first three fields little-endian). Only this GUID marks a
/// GPT root: the generic "Linux filesystem data" GUID marks any Linux
/// partition, a data one as much as a root.
pub const GPT_ROOT_AARCH64: [u8; 16] = [
    0x45, 0xb0, 0x21, 0xb9, 0xf0, 0x1d, 0xc3, 0x41, 0xaf, 0x44, 0x4c, 0x6f, 0x28, 0x0d, 0x3f, 0xae,
];

/// Whether an MBR type byte marks a root filesystem.
pub fn is_root_type(mbr_type: u8) -> bool {
    mbr_type == MBR_LINUX
}

/// A read [`discover`] could not do without: sector 0 or 1.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Unreadable;

/// The disk's partitions into `out`, in table order, returning how many:
/// GPT when [`is_gpt`] says so (the primary, else the backup when
/// `capacity` is known, else none), otherwise the MBR's four entries. An
/// unused entry (type 0, a zero type GUID), a protective one, and one
/// starting at LBA 0 are left out. Nothing else is filtered by type: a
/// partition holding no filesystem just fails the mount. The MBR is read
/// without asking for the boot signature, as `fsd` always has. A GPT copy's
/// read failing is that copy invalid, so a bad primary never hides a good
/// backup; only sector 0 or 1 unread is [`Unreadable`].
pub fn discover(read: &mut ReadSector, capacity: Option<u64>, out: &mut [Partition]) -> Result<usize, Unreadable> {
    if is_gpt(read).ok_or(Unreadable)? {
        let copies = [Some(GPT_PRIMARY), capacity.and_then(gpt_backup)];
        for header in copies.into_iter().flatten() {
            let mut n = 0;
            let found = read_gpt(read, header, capacity, &mut |_, bytes| {
                if bytes[..16].iter().all(|&b| b == 0) {
                    return;
                }
                let start = le(&bytes[32..40]);
                if start != 0 && n < out.len() {
                    out[n] = Partition { start, root: bytes[..16] == GPT_ROOT_AARCH64 };
                    n += 1;
                }
            });
            if found == GptCopy::Valid {
                return Ok(n);
            }
        }
        // Both copies failing: no partitions, rather than a corrupt table
        // trusted.
        return Ok(0);
    }
    let mut mbr = [0u8; SECTOR];
    if !read(0, &mut mbr) {
        return Err(Unreadable);
    }
    let mut n = 0;
    for i in 0..4 {
        let e = mbr_entry(&mbr, i);
        if e.kind == 0 || e.kind == PROTECTIVE || e.start == 0 {
            continue;
        }
        if n < out.len() {
            out[n] = Partition { start: u64::from(e.start), root: is_root_type(e.kind) };
            n += 1;
        }
    }
    Ok(n)
}

// --- The boot disk ----------------------------------------------------------

/// The partition table the firmware says the boot partition is in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Table {
    Mbr,
    Gpt,
    /// A format the specification does not list: matched against nothing.
    Unknown,
}

/// The disk's identity as the firmware's `HardDrive` node states it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DiskSignature {
    /// The node carries no signature (its signature type 0), so a disk is
    /// matched on its table alone (the review of #287: it had also stood for
    /// an MBR signature of 0, which made "no signature" a claim of 0).
    None,
    /// The MBR's 32-bit disk signature, 0 included.
    Mbr(u32),
    /// The GPT partition's unique GUID, as its 16 bytes on disk.
    Gpt([u8; 16]),
    /// A signature type the UEFI specification does not list (3 and up),
    /// kept apart from `None` so that a signature that was there is never
    /// logged or matched as if it were absent (the review of #285).
    Unknown(u8),
}

/// The partition the kernel image was loaded from, as the firmware's
/// `HardDrive` node names it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct BootPartition {
    /// The table the firmware read it from. A disk is read by this table,
    /// not by guessing from its sector 0, so a hybrid MBR (an 0xEE entry
    /// beside real ones) is read as the GPT the firmware booted (the review
    /// of #287).
    pub table: Table,
    /// From 1, in the partition table's own order.
    pub number: u32,
    /// First block, in the disk's logical blocks.
    pub start_lba: u64,
    /// Size, in the disk's logical blocks.
    pub size_blocks: u64,
    pub signature: DiskSignature,
}

/// What a disk's table says of the boot partition.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Boot {
    /// The disk's own table holds it.
    Yes,
    /// The table was read and does not hold it.
    No,
    /// A read the answer needed failed, so the table said nothing.
    Unreadable,
}

impl From<bool> for Boot {
    fn from(holds: bool) -> Self {
        if holds { Boot::Yes } else { Boot::No }
    }
}

/// Whether the disk read through `read`, of `capacity` sectors, holds
/// `boot`. `None` for a table of a format the specification does not list,
/// so the caller keeps the order from before one-disk step 3.
///
/// The disk is read by the table the firmware named. On MBR: entry `number`
/// in use with the same start and size, and, when the node carries an MBR
/// signature, the disk's equal to it, 0 included. On GPT: the primary
/// header, or the backup at the last sector when the primary does not
/// validate, then entry `number` with the same start, size and unique GUID.
/// A node with no signature, or one of an unlisted type, is matched on the
/// table alone.
///
/// [`Boot::Unreadable`] only when the answer needed a read that failed: a
/// copy that validated says yes or no even if the other could not be read
/// (the second high review of #291), and a backup that could not be read
/// beside a primary that was read and did not validate is a no, since the
/// backup is only the second path and some sticks report a sector too many.
pub fn is_boot_disk(boot: &BootPartition, read: &mut ReadSector, capacity: u64) -> Option<Boot> {
    match boot.table {
        Table::Unknown => None,
        Table::Mbr => {
            let mut sector = [0u8; SECTOR];
            if !read(0, &mut sector) {
                return Some(Boot::Unreadable);
            }
            Some(Boot::from(mbr_holds(boot, &sector)))
        }
        Table::Gpt => {
            let primary = gpt_holds(boot, read, GPT_PRIMARY, capacity);
            if let Some(Ok(holds)) = primary {
                return Some(Boot::from(holds));
            }
            let backup = gpt_backup(capacity).map(|last| gpt_holds(boot, read, last, capacity));
            Some(match (primary, backup) {
                (_, Some(Some(Ok(holds)))) => Boot::from(holds),
                (Some(Err(())), _) => Boot::Unreadable,
                _ => Boot::No,
            })
        }
    }
}

fn mbr_holds(boot: &BootPartition, sector: &[u8; SECTOR]) -> bool {
    if !has_boot_signature(sector) || !(1..=4).contains(&boot.number) {
        return false;
    }
    match boot.signature {
        DiskSignature::Mbr(sig) if sig != mbr_signature(sector) => return false,
        DiskSignature::Gpt(_) => return false,
        _ => {}
    }
    let e = mbr_entry(sector, boot.number as usize - 1);
    e.kind != 0 && u64::from(e.start) == boot.start_lba && u64::from(e.sectors) == boot.size_blocks
}

/// One GPT copy asked of the boot partition: `Some(Ok(holds))` when it
/// validated, `Some(Err(()))` when a read failed, `None` when it did not
/// validate.
fn gpt_holds(boot: &BootPartition, read: &mut ReadSector, header: u64, capacity: u64) -> Option<Result<bool, ()>> {
    let wanted = u64::from(boot.number).checked_sub(1);
    let mut found = None;
    let copy = read_gpt(read, header, Some(capacity), &mut |index, bytes| {
        if Some(index) == wanted {
            let mut unique = [0u8; 16];
            unique.copy_from_slice(&bytes[16..32]);
            found = Some((unique, le(&bytes[32..40]), le(&bytes[40..48])));
        }
    });
    match copy {
        GptCopy::Unreadable => return Some(Err(())),
        GptCopy::Invalid => return None,
        GptCopy::Valid => {}
    }
    let Some((unique, first, last)) = found else { return Some(Ok(false)) };
    let guid_ok = match boot.signature {
        DiskSignature::Gpt(guid) => guid == unique,
        DiskSignature::Mbr(_) => false,
        DiskSignature::None | DiskSignature::Unknown(_) => true,
    };
    // Both ends must be computed: two overflows are not a match (the medium
    // review of #287 found `None == None` taken as one).
    let ends = matches!((last.checked_add(1), first.checked_add(boot.size_blocks)), (Some(a), Some(b)) if a == b);
    Some(Ok(guid_ok && first == boot.start_lba && ends))
}

#[cfg(test)]
mod tests;
