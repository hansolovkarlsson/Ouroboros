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

use crate::disk::{Disk, DiskError, SECTOR_SIZE};

/// Most partitions `fsd` collects from one disk. A GPT can declare 128 entries,
/// but the overwhelming majority are empty; a handful of real partitions is all
/// we ever mount from.
pub const MAX_PARTITIONS: usize = 16;

/// The most GPT partition entries to scan (the usual `NumberOfPartitionEntries`
/// is 128) - a bound so a corrupt header can't run the scan away.
const MAX_GPT_ENTRIES: usize = 128;

/// Enumerate the disk's partition start LBAs into `out`, returning the count.
/// Detects GPT by the "EFI PART" signature at LBA 1 (a GPT disk also carries a
/// protective MBR at LBA 0); otherwise reads the classic MBR partition table.
/// Unused/empty entries and the GPT protective (type 0xEE) MBR entry are
/// skipped. The caller (`vfs::mount`) tries mounting a filesystem at each, so
/// this deliberately doesn't filter by partition *type* - a partition that
/// isn't a mountable filesystem just fails the mount and the next is tried.
pub fn discover(disk: &mut Disk, out: &mut [u64; MAX_PARTITIONS]) -> Result<usize, DiskError> {
    let mut hdr = [0u8; SECTOR_SIZE];
    disk.read_sector(1, &mut hdr)?;
    if &hdr[0..8] == b"EFI PART" {
        return discover_gpt(disk, &hdr, out);
    }

    let mut mbr = [0u8; SECTOR_SIZE];
    disk.read_sector(0, &mut mbr)?;
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

/// Parse the GPT partition-entry array (header already validated by its
/// signature). Reads entries sector-by-sector from `PartitionEntryLBA`,
/// collecting the start LBA of every entry with a non-zero type GUID.
fn discover_gpt(
    disk: &mut Disk,
    hdr: &[u8; SECTOR_SIZE],
    out: &mut [u64; MAX_PARTITIONS],
) -> Result<usize, DiskError> {
    let entry_lba = u64::from_le_bytes(hdr[72..80].try_into().unwrap_or([0; 8]));
    let num_entries = u32::from_le_bytes(hdr[80..84].try_into().unwrap_or([0; 4])) as usize;
    let entry_size = u32::from_le_bytes(hdr[84..88].try_into().unwrap_or([0; 4])) as usize;
    // Entry size is a power of two >= 128 (128 in practice), so it divides a
    // 512-byte sector evenly - an entry never straddles a sector boundary.
    if !(128..=SECTOR_SIZE).contains(&entry_size) || !SECTOR_SIZE.is_multiple_of(entry_size) {
        return Ok(0); // unsupported layout - degrade to "no partitions"
    }
    let per_sector = SECTOR_SIZE / entry_size;
    let total = num_entries.min(MAX_GPT_ENTRIES);

    let mut n = 0;
    let mut scanned = 0;
    let mut sector = entry_lba;
    while scanned < total {
        let mut buf = [0u8; SECTOR_SIZE];
        disk.read_sector(sector, &mut buf)?;
        for k in 0..per_sector {
            if scanned >= total {
                break;
            }
            scanned += 1;
            let base = k * entry_size;
            // A zero type GUID marks an unused entry.
            if buf[base..base + 16].iter().all(|&b| b == 0) {
                continue;
            }
            let start = u64::from_le_bytes(buf[base + 32..base + 40].try_into().unwrap_or([0; 8]));
            if start == 0 {
                continue;
            }
            if n < out.len() {
                out[n] = start;
                n += 1;
            } else {
                return Ok(n); // out full - stop
            }
        }
        sector += 1;
    }
    Ok(n)
}
