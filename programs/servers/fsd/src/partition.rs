//! Partition discovery for `fsd`'s mount step (the GPT + multi-partition
//! milestone). Enumerates a disk's partitions - **MBR or GPT** - as a list of
//! start LBAs, which `vfs::Filesystem::mount` then probes for a mountable
//! filesystem. Bounded, fixed-buffer, no `alloc`, like everything in `fsd`.
//!
//! Partition *discovery* lives here rather than inside `fat32.rs` because it's
//! above any one filesystem: a GPT disk formatted by macOS/Linux, or an MBR
//! disk with several partitions, is discovered the same way regardless of what
//! filesystem sits in each partition - which is exactly what the "more
//! filesystems" arc needs.
//!
//! **GPT is validated, not just parsed** (the "GPT CRCs not checked on read"
//! robustness gap). Both the 92-byte header CRC32 and the partition-entry-array
//! CRC32 are verified against the values the header stores, per the UEFI spec's
//! GPT validation rules. A GPT whose *primary* header/array fails validation
//! falls back to the **backup** GPT at the last LBA (its own header + array);
//! only if both copies fail does discovery report no partitions. Clean disks
//! (including the ones `scripts/mkgpt.py` builds, which write correct CRCs) are
//! unaffected - they validate on the primary and never touch the backup path.

use crate::disk::{Disk, DiskError, SECTOR_SIZE};

/// Most partitions `fsd` collects from one disk. A GPT can declare 128 entries,
/// but the overwhelming majority are empty; a handful of real partitions is all
/// we ever mount from.
pub const MAX_PARTITIONS: usize = 16;

/// Cap on the partition-entry array we'll read and CRC, in sectors. The array
/// is `NumberOfPartitionEntries * SizeOfPartitionEntry` bytes - 16 KiB (32
/// sectors) for the universal 128 entries * 128 B, and this bound (64 KiB)
/// comfortably covers even 128 maximum-size (512 B) entries. A header
/// declaring a larger array is treated as unsupported rather than read
/// unbounded - the same "a corrupt header can't run the scan away" discipline
/// the old fixed entry cap had.
const MAX_GPT_ARR_SECTORS: usize = 128;

/// Enumerate the disk's partition start LBAs into `out`, returning the count.
/// Detects GPT by the "EFI PART" signature at LBA 1 *or* a protective (type
/// 0xEE) entry in the LBA-0 MBR (so a GPT whose primary header signature is
/// itself damaged is still recognized and recovered from the backup);
/// otherwise reads the classic MBR partition table. Unused/empty entries and
/// the GPT protective MBR entry are skipped. The caller (`vfs::mount`) tries
/// mounting a filesystem at each, so this deliberately doesn't filter by
/// partition *type* - a partition that isn't a mountable filesystem just fails
/// the mount and the next is tried.
pub fn discover(disk: &mut Disk, out: &mut [u64; MAX_PARTITIONS]) -> Result<usize, DiskError> {
    let mut lba1 = [0u8; SECTOR_SIZE];
    disk.read_sector(1, &mut lba1)?;
    let mut mbr = [0u8; SECTOR_SIZE];
    disk.read_sector(0, &mut mbr)?;

    if &lba1[0..8] == b"EFI PART" || mbr_has_protective(&mbr) {
        // Primary GPT header at LBA 1; on failure, the backup at the last LBA.
        if let Some(n) = try_gpt(disk, 1, out)? {
            return Ok(n);
        }
        if let Ok(capacity) = disk.capacity_sectors() {
            if capacity >= 2 {
                if let Some(n) = try_gpt(disk, capacity - 1, out)? {
                    return Ok(n);
                }
            }
        }
        // A GPT disk whose primary *and* backup both fail validation: report no
        // mountable partitions rather than trusting a corrupt table.
        return Ok(0);
    }

    // Classic MBR.
    let mut n = 0;
    for i in 0..4 {
        let e = 0x1be + i * 16;
        let ptype = mbr[e + 4];
        if ptype == 0x00 || ptype == 0xee {
            continue; // unused, or a GPT protective entry
        }
        let lba = u32::from_le_bytes([mbr[e + 8], mbr[e + 9], mbr[e + 10], mbr[e + 11]]) as u64;
        if lba == 0 {
            continue;
        }
        if n < out.len() {
            out[n] = lba;
            n += 1;
        }
    }
    Ok(n)
}

/// Whether the LBA-0 MBR is a GPT *protective* MBR: a valid `0x55AA` boot
/// signature and at least one partition entry of type `0xEE`. Lets `discover`
/// recognize a GPT disk even when the primary header's own "EFI PART"
/// signature has been corrupted (the backup header is then the way in).
fn mbr_has_protective(mbr: &[u8; SECTOR_SIZE]) -> bool {
    if mbr[510] != 0x55 || mbr[511] != 0xAA {
        return false;
    }
    (0..4).any(|i| mbr[0x1be + i * 16 + 4] == 0xEE)
}

/// Try to read, **validate, and parse** the GPT copy whose header is at
/// `header_lba` (LBA 1 for the primary, the last LBA for the backup). Returns
/// `Some(count)` with the collected start LBAs in `out` if the header and its
/// partition-entry array both pass their CRC32 checks; `None` if this copy is
/// missing, malformed, or fails validation (so the caller can try the other
/// copy). A read error touching this copy is treated as "invalid copy" (`None`)
/// rather than a hard error, so a bad primary never masks a good backup.
///
/// Validation follows the UEFI spec: verify the header CRC32 (over `HeaderSize`
/// bytes with the CRC field taken as zero), then the array CRC32 (over
/// `NumberOfPartitionEntries * SizeOfPartitionEntry` bytes), using the
/// (now-trusted) size/location fields the header carries.
fn try_gpt(
    disk: &mut Disk,
    header_lba: u64,
    out: &mut [u64; MAX_PARTITIONS],
) -> Result<Option<usize>, DiskError> {
    let mut hdr = [0u8; SECTOR_SIZE];
    if disk.read_sector(header_lba, &mut hdr).is_err() {
        return Ok(None);
    }
    if &hdr[0..8] != b"EFI PART" {
        return Ok(None);
    }
    let header_size = u32::from_le_bytes([hdr[12], hdr[13], hdr[14], hdr[15]]) as usize;
    if !(92..=SECTOR_SIZE).contains(&header_size) {
        return Ok(None);
    }

    // Header CRC32: over the first `header_size` bytes with the CRC field
    // (bytes 16..20) taken as zero.
    let stored_hdr_crc = u32::from_le_bytes([hdr[16], hdr[17], hdr[18], hdr[19]]);
    let mut h = hdr;
    h[16..20].copy_from_slice(&[0, 0, 0, 0]);
    if crc32(&h[..header_size]) != stored_hdr_crc {
        return Ok(None);
    }

    let entry_lba = u64::from_le_bytes(hdr[72..80].try_into().unwrap_or([0; 8]));
    let num_entries = u32::from_le_bytes([hdr[80], hdr[81], hdr[82], hdr[83]]) as usize;
    let entry_size = u32::from_le_bytes([hdr[84], hdr[85], hdr[86], hdr[87]]) as usize;
    let stored_arr_crc = u32::from_le_bytes([hdr[88], hdr[89], hdr[90], hdr[91]]);
    // Entry size is a power of two >= 128 (128 in practice), so it divides a
    // 512-byte sector evenly - an entry never straddles a sector boundary.
    if !(128..=SECTOR_SIZE).contains(&entry_size) || !SECTOR_SIZE.is_multiple_of(entry_size) {
        return Ok(None);
    }
    let array_bytes = match num_entries.checked_mul(entry_size) {
        Some(b) if b > 0 && b <= MAX_GPT_ARR_SECTORS * SECTOR_SIZE => b,
        _ => return Ok(None), // zero-length or larger than we'll read - unsupported
    };
    let per_sector = SECTOR_SIZE / entry_size;

    // One pass over exactly `array_bytes`: fold each sector's bytes into the
    // running CRC *and* collect start LBAs. Entries are only committed if the
    // CRC matches at the end, so a corrupt array yields `None` and the stale
    // `out` is never read (the caller uses the returned count).
    let mut crc = CRC_INIT;
    let mut n = 0usize;
    let mut remaining = array_bytes;
    let mut sector = entry_lba;
    while remaining > 0 {
        let mut buf = [0u8; SECTOR_SIZE];
        if disk.read_sector(sector, &mut buf).is_err() {
            return Ok(None);
        }
        let take = remaining.min(SECTOR_SIZE);
        crc = crc32_update(crc, &buf[..take]);
        for k in 0..per_sector {
            let base = k * entry_size;
            if base + entry_size > take {
                break; // this entry lies past the CRC'd array bytes
            }
            // A zero type GUID marks an unused entry.
            if buf[base..base + 16].iter().all(|&b| b == 0) {
                continue;
            }
            let start = u64::from_le_bytes(buf[base + 32..base + 40].try_into().unwrap_or([0; 8]));
            if start != 0 && n < out.len() {
                out[n] = start;
                n += 1;
            }
        }
        remaining -= take;
        sector += 1;
    }
    if crc32_finish(crc) != stored_arr_crc {
        return Ok(None); // partition-entry array is corrupt
    }
    Ok(Some(n))
}

/// The initial CRC-32 register value (pre-inversion). Fold data in with
/// [`crc32_update`], then [`crc32_finish`] to get the final value; [`crc32`]
/// is the whole-buffer convenience.
const CRC_INIT: u32 = 0xFFFF_FFFF;

/// CRC-32 (IEEE 802.3, reflected polynomial `0xEDB88320`) of one buffer -
/// the same CRC `zlib.crc32` / GPT use. Table-free bitwise: run once at mount
/// over at most the header (≤512 B) and the entry array (≤64 KiB), so speed
/// isn't a concern and a 1 KiB lookup table isn't worth the `.data`.
fn crc32(data: &[u8]) -> u32 {
    crc32_finish(crc32_update(CRC_INIT, data))
}

/// Fold `data` into a running CRC-32 register (start from [`CRC_INIT`]),
/// allowing a large buffer to be CRC'd sector-by-sector without holding it
/// all in memory.
fn crc32_update(mut crc: u32, data: &[u8]) -> u32 {
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    crc
}

/// Final inversion turning a running CRC-32 register into the stored value.
fn crc32_finish(crc: u32) -> u32 {
    crc ^ 0xFFFF_FFFF
}
