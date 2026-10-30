//! Each rule on a disk built in memory: the cases a QEMU rig cannot set up
//! (a hybrid MBR, a corrupt primary GPT, a header built to overflow, a read
//! failing on one copy and not the other) and the ones it can, so the two
//! callers' behaviour is pinned here first.

extern crate std;
use super::*;
use std::vec;
use std::vec::Vec;

const DISK: u64 = 4096;

/// A disk of `DISK` sectors, with reads of the sectors in `fail` failing.
struct Mem {
    sectors: Vec<[u8; SECTOR]>,
    fail: Vec<u64>,
}

impl Mem {
    fn new() -> Self {
        Mem { sectors: vec![[0u8; SECTOR]; DISK as usize], fail: Vec::new() }
    }

    fn reader(&self) -> impl FnMut(u64, &mut [u8; SECTOR]) -> bool + '_ {
        move |lba, buf| {
            if self.fail.contains(&lba) || lba >= DISK {
                return false;
            }
            *buf = self.sectors[lba as usize];
            true
        }
    }

    fn mbr(&mut self, entries: &[(u8, u32, u32)], signature: u32) {
        let s = &mut self.sectors[0];
        s[510] = 0x55;
        s[511] = 0xAA;
        s[440..444].copy_from_slice(&signature.to_le_bytes());
        for (i, &(kind, start, sectors)) in entries.iter().enumerate() {
            let e = 0x1BE + 16 * i;
            s[e + 4] = kind;
            s[e + 8..e + 12].copy_from_slice(&start.to_le_bytes());
            s[e + 12..e + 16].copy_from_slice(&sectors.to_le_bytes());
        }
    }

    /// A GPT copy: its header at `header`, its 128-entry array at `array`,
    /// holding `parts` as (type GUID, unique GUID, first LBA, last LBA).
    fn gpt(&mut self, header: u64, array: u64, parts: &[([u8; 16], [u8; 16], u64, u64)]) {
        let mut bytes = vec![0u8; 128 * 128];
        for (i, (kind, unique, first, last)) in parts.iter().enumerate() {
            let e = i * 128;
            bytes[e..e + 16].copy_from_slice(kind);
            bytes[e + 16..e + 32].copy_from_slice(unique);
            bytes[e + 32..e + 40].copy_from_slice(&first.to_le_bytes());
            bytes[e + 40..e + 48].copy_from_slice(&last.to_le_bytes());
        }
        for (i, chunk) in bytes.chunks(SECTOR).enumerate() {
            self.sectors[array as usize + i].copy_from_slice(chunk);
        }
        let h = &mut self.sectors[header as usize];
        h.fill(0);
        h[0..8].copy_from_slice(b"EFI PART");
        h[12..16].copy_from_slice(&92u32.to_le_bytes());
        h[72..80].copy_from_slice(&array.to_le_bytes());
        h[80..84].copy_from_slice(&128u32.to_le_bytes());
        h[84..88].copy_from_slice(&128u32.to_le_bytes());
        h[88..92].copy_from_slice(&crc32(&bytes).to_le_bytes());
        Self::seal(h);
    }

    /// The header's CRC, after a field has been changed.
    fn seal(h: &mut [u8; SECTOR]) {
        h[16..20].fill(0);
        let crc = crc32(&h[..92]);
        h[16..20].copy_from_slice(&crc.to_le_bytes());
    }

    /// A GPT disk: the protective MBR, a primary and a backup.
    fn gpt_disk(parts: &[([u8; 16], [u8; 16], u64, u64)]) -> Self {
        let mut m = Mem::new();
        m.mbr(&[(PROTECTIVE, 1, (DISK - 1) as u32)], 0);
        m.gpt(1, 2, parts);
        m.gpt(DISK - 1, DISK - 33, parts);
        m
    }
}

const ESP_GUID: [u8; 16] = [0x28, 0x73, 0x2a, 0xc1, 0x1f, 0xf8, 0xd2, 0x11, 0xba, 0x4b, 0, 0xa0, 0xc9, 0x3e, 0xc9, 0x3b];
const UNIQUE: [u8; 16] = [7; 16];
const OTHER: [u8; 16] = [9; 16];

fn mbr_boot(number: u32, start: u64, size: u64, signature: DiskSignature) -> BootPartition {
    BootPartition { table: Table::Mbr, number, start_lba: start, size_blocks: size, signature }
}

fn gpt_boot(signature: DiskSignature) -> BootPartition {
    BootPartition { table: Table::Gpt, number: 1, start_lba: 34, size_blocks: 100, signature }
}

fn ask(m: &Mem, boot: &BootPartition) -> Option<Boot> {
    is_boot_disk(boot, &mut m.reader(), DISK)
}

#[test]
fn crc32_matches_the_standard_check_value() {
    assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    assert_eq!(crc32_finish(crc32_update(crc32_update(CRC_INIT, b"1234"), b"56789")), 0xCBF4_3926);
}

#[test]
fn mbr_matches_on_entry_start_size_and_signature() {
    let mut m = Mem::new();
    m.mbr(&[(0x0C, 2048, 1000), (0x83, 4000, 50)], 0x1234_5678);
    assert_eq!(ask(&m, &mbr_boot(1, 2048, 1000, DiskSignature::Mbr(0x1234_5678))), Some(Boot::Yes));
    assert_eq!(ask(&m, &mbr_boot(1, 2048, 1000, DiskSignature::Mbr(0x1234_5679))), Some(Boot::No));
    assert_eq!(ask(&m, &mbr_boot(1, 2049, 1000, DiskSignature::None)), Some(Boot::No));
    assert_eq!(ask(&m, &mbr_boot(1, 2048, 999, DiskSignature::None)), Some(Boot::No));
    assert_eq!(ask(&m, &mbr_boot(2, 2048, 1000, DiskSignature::None)), Some(Boot::No), "the entry by its number");
    assert_eq!(ask(&m, &mbr_boot(2, 4000, 50, DiskSignature::None)), Some(Boot::Yes), "no signature: the table alone");
    assert_eq!(ask(&m, &mbr_boot(2, 4000, 50, DiskSignature::Unknown(5))), Some(Boot::Yes));
    assert_eq!(ask(&m, &mbr_boot(5, 2048, 1000, DiskSignature::None)), Some(Boot::No), "numbers run 1 to 4");
    assert_eq!(ask(&m, &mbr_boot(0, 2048, 1000, DiskSignature::None)), Some(Boot::No));
    assert_eq!(ask(&m, &mbr_boot(1, 2048, 1000, DiskSignature::Gpt(UNIQUE))), Some(Boot::No));
}

#[test]
fn mbr_signature_zero_is_a_signature_not_none() {
    let mut m = Mem::new();
    m.mbr(&[(0x0C, 2048, 1000)], 0x11);
    assert_eq!(ask(&m, &mbr_boot(1, 2048, 1000, DiskSignature::Mbr(0))), Some(Boot::No));
}

#[test]
fn mbr_without_the_boot_signature_or_with_an_unused_entry_is_no() {
    let mut m = Mem::new();
    m.mbr(&[(0x0C, 2048, 1000)], 0);
    m.sectors[0][511] = 0;
    assert_eq!(ask(&m, &mbr_boot(1, 2048, 1000, DiskSignature::None)), Some(Boot::No));
    let mut m = Mem::new();
    m.mbr(&[(0, 2048, 1000)], 0);
    assert_eq!(ask(&m, &mbr_boot(1, 2048, 1000, DiskSignature::None)), Some(Boot::No));
}

#[test]
fn mbr_sector_zero_unread_is_unreadable() {
    let mut m = Mem::new();
    m.mbr(&[(0x0C, 2048, 1000)], 0);
    m.fail.push(0);
    assert_eq!(ask(&m, &mbr_boot(1, 2048, 1000, DiskSignature::None)), Some(Boot::Unreadable));
}

#[test]
fn an_unlisted_table_is_matched_against_nothing() {
    let m = Mem::new();
    let boot = BootPartition { table: Table::Unknown, ..mbr_boot(1, 2048, 1000, DiskSignature::None) };
    assert_eq!(ask(&m, &boot), None);
}

#[test]
fn gpt_matches_on_entry_start_size_and_unique_guid() {
    let m = Mem::gpt_disk(&[(ESP_GUID, UNIQUE, 34, 133)]);
    assert_eq!(ask(&m, &gpt_boot(DiskSignature::Gpt(UNIQUE))), Some(Boot::Yes));
    assert_eq!(ask(&m, &gpt_boot(DiskSignature::Gpt(OTHER))), Some(Boot::No));
    assert_eq!(ask(&m, &gpt_boot(DiskSignature::None)), Some(Boot::Yes));
    assert_eq!(ask(&m, &gpt_boot(DiskSignature::Mbr(0))), Some(Boot::No));
    let wrong_size = BootPartition { size_blocks: 101, ..gpt_boot(DiskSignature::Gpt(UNIQUE)) };
    assert_eq!(ask(&m, &wrong_size), Some(Boot::No));
    let past = BootPartition { number: 2, ..gpt_boot(DiskSignature::None) };
    assert_eq!(ask(&m, &past), Some(Boot::No), "an unused entry holds nothing");
}

#[test]
fn a_corrupt_primary_is_read_from_the_backup() {
    let mut m = Mem::gpt_disk(&[(ESP_GUID, UNIQUE, 34, 133)]);
    m.sectors[1][20] ^= 1; // a header field, the CRC left stale
    assert_eq!(ask(&m, &gpt_boot(DiskSignature::Gpt(UNIQUE))), Some(Boot::Yes));
    let mut m = Mem::gpt_disk(&[(ESP_GUID, UNIQUE, 34, 133)]);
    m.sectors[2][40] ^= 1; // the primary's array, its CRC left stale
    assert_eq!(ask(&m, &gpt_boot(DiskSignature::Gpt(UNIQUE))), Some(Boot::Yes));
}

#[test]
fn a_stale_primary_does_not_decide() {
    // A primary that validates says no on its own: a backup is not asked.
    let mut m = Mem::gpt_disk(&[(ESP_GUID, UNIQUE, 34, 133)]);
    m.gpt(1, 2, &[(ESP_GUID, OTHER, 34, 133)]);
    assert_eq!(ask(&m, &gpt_boot(DiskSignature::Gpt(UNIQUE))), Some(Boot::No));
    // "EFI PART" with a stale CRC is not a primary: the backup decides.
    let mut m = Mem::gpt_disk(&[(ESP_GUID, UNIQUE, 34, 133)]);
    m.sectors[1][16] ^= 1;
    assert_eq!(ask(&m, &gpt_boot(DiskSignature::Gpt(UNIQUE))), Some(Boot::Yes));
}

#[test]
fn both_copies_corrupt_is_no() {
    let mut m = Mem::gpt_disk(&[(ESP_GUID, UNIQUE, 34, 133)]);
    m.sectors[1][20] ^= 1;
    m.sectors[(DISK - 1) as usize][20] ^= 1;
    assert_eq!(ask(&m, &gpt_boot(DiskSignature::Gpt(UNIQUE))), Some(Boot::No));
}

#[test]
fn unreadable_is_kept_apart_from_invalid() {
    // Primary unread, backup valid: the backup decides, either way.
    let mut m = Mem::gpt_disk(&[(ESP_GUID, UNIQUE, 34, 133)]);
    m.fail.push(1);
    assert_eq!(ask(&m, &gpt_boot(DiskSignature::Gpt(UNIQUE))), Some(Boot::Yes));
    assert_eq!(ask(&m, &gpt_boot(DiskSignature::Gpt(OTHER))), Some(Boot::No));
    // Primary unread, backup unread too: nothing was said.
    m.fail.push(DISK - 1);
    assert_eq!(ask(&m, &gpt_boot(DiskSignature::Gpt(UNIQUE))), Some(Boot::Unreadable));
    // Primary unread, backup invalid: still nothing was said.
    let mut m = Mem::gpt_disk(&[(ESP_GUID, UNIQUE, 34, 133)]);
    m.fail.push(1);
    m.sectors[(DISK - 1) as usize][20] ^= 1;
    assert_eq!(ask(&m, &gpt_boot(DiskSignature::Gpt(UNIQUE))), Some(Boot::Unreadable));
    // Primary read and invalid, backup unread: a no, the backup being only
    // the second path (some sticks report a sector too many).
    let mut m = Mem::gpt_disk(&[(ESP_GUID, UNIQUE, 34, 133)]);
    m.sectors[1][20] ^= 1;
    m.fail.push(DISK - 1);
    assert_eq!(ask(&m, &gpt_boot(DiskSignature::Gpt(UNIQUE))), Some(Boot::No));
    // The primary's array unread: that copy is unreadable, and the backup
    // decides.
    let mut m = Mem::gpt_disk(&[(ESP_GUID, UNIQUE, 34, 133)]);
    m.fail.push(2);
    assert_eq!(ask(&m, &gpt_boot(DiskSignature::Gpt(UNIQUE))), Some(Boot::Yes));
}

#[test]
fn a_hybrid_mbr_is_read_as_the_table_the_firmware_named() {
    // A protective entry beside a real one: the firmware booted the GPT.
    let mut m = Mem::gpt_disk(&[(ESP_GUID, UNIQUE, 34, 133)]);
    m.mbr(&[(PROTECTIVE, 1, 33), (0x0C, 34, 100)], 0xAB);
    assert_eq!(ask(&m, &gpt_boot(DiskSignature::Gpt(UNIQUE))), Some(Boot::Yes));
    // Named as MBR, its real entry is read as MBR.
    assert_eq!(ask(&m, &mbr_boot(2, 34, 100, DiskSignature::Mbr(0xAB))), Some(Boot::Yes));
    // And discover lists the GPT's partitions.
    let mut out = [Partition::default(); 8];
    assert_eq!(discover(&mut m.reader(), Some(DISK), &mut out), Ok(1));
    assert_eq!(out[0].start, 34);
}

#[test]
fn headers_built_to_overflow_are_invalid_not_a_panic() {
    let mut reads = Vec::new();
    // An array LBA at the top of u64.
    let mut m = Mem::gpt_disk(&[(ESP_GUID, UNIQUE, 34, 133)]);
    m.sectors[1][72..80].copy_from_slice(&u64::MAX.to_le_bytes());
    Mem::seal(&mut m.sectors[1]);
    reads.push(read_gpt(&mut m.reader(), 1, Some(DISK), &mut |_, _| {}));
    // A count whose product with the entry size overflows.
    let mut m = Mem::gpt_disk(&[(ESP_GUID, UNIQUE, 34, 133)]);
    m.sectors[1][80..84].copy_from_slice(&u32::MAX.to_le_bytes());
    m.sectors[1][84..88].copy_from_slice(&512u32.to_le_bytes());
    Mem::seal(&mut m.sectors[1]);
    reads.push(read_gpt(&mut m.reader(), 1, Some(DISK), &mut |_, _| {}));
    // An entry size of 0, and one that does not divide a sector.
    for size in [0u32, 96, 200, 1024] {
        let mut m = Mem::gpt_disk(&[(ESP_GUID, UNIQUE, 34, 133)]);
        m.sectors[1][84..88].copy_from_slice(&size.to_le_bytes());
        Mem::seal(&mut m.sectors[1]);
        reads.push(read_gpt(&mut m.reader(), 1, Some(DISK), &mut |_, _| {}));
    }
    // A header size out of range.
    let mut m = Mem::gpt_disk(&[(ESP_GUID, UNIQUE, 34, 133)]);
    m.sectors[1][12..16].copy_from_slice(&600u32.to_le_bytes());
    reads.push(read_gpt(&mut m.reader(), 1, Some(DISK), &mut |_, _| {}));
    // An array past the disk.
    let mut m = Mem::gpt_disk(&[(ESP_GUID, UNIQUE, 34, 133)]);
    m.sectors[1][72..80].copy_from_slice(&(DISK - 2).to_le_bytes());
    Mem::seal(&mut m.sectors[1]);
    reads.push(read_gpt(&mut m.reader(), 1, Some(DISK), &mut |_, _| {}));
    assert!(reads.iter().all(|r| *r == GptCopy::Invalid), "{reads:?}");
}

#[test]
fn an_entry_whose_ends_overflow_is_no_match() {
    let m = Mem::gpt_disk(&[(ESP_GUID, UNIQUE, u64::MAX, u64::MAX)]);
    let boot = BootPartition { start_lba: u64::MAX, size_blocks: u64::MAX, ..gpt_boot(DiskSignature::None) };
    assert_eq!(ask(&m, &boot), Some(Boot::No), "two overflows are not a match");
}

#[test]
fn discover_lists_mbr_entries_with_their_root_type() {
    let mut m = Mem::new();
    m.mbr(&[(0x0C, 2048, 100), (0, 3000, 10), (0x07, 4000, 10), (0x83, 5000, 10)], 1);
    let mut out = [Partition::default(); 8];
    assert_eq!(discover(&mut m.reader(), Some(DISK), &mut out), Ok(3));
    assert_eq!(&out[..3], &[
        Partition { start: 2048, sectors: 100, root: false },
        Partition { start: 4000, sectors: 10, root: false },
        Partition { start: 5000, sectors: 10, root: true },
    ]);
    // An entry at LBA 0 is the MBR itself, never a partition.
    let mut m = Mem::new();
    m.mbr(&[(0x0C, 0, 100), (0x83, 5000, 10)], 1);
    assert_eq!(discover(&mut m.reader(), Some(DISK), &mut out), Ok(1));
    assert_eq!(out[0].start, 5000);
}

#[test]
fn discover_lists_gpt_entries_with_the_aarch64_root_guid_only() {
    let linux_data = [0xaf, 0x3d, 0xc6, 0x0f, 0x83, 0x84, 0x72, 0x47, 0x8e, 0x79, 0x3d, 0x69, 0xd8, 0x47, 0x7d, 0xe4];
    let m = Mem::gpt_disk(&[
        (ESP_GUID, UNIQUE, 34, 133),
        ([0; 16], OTHER, 200, 300), // unused: a zero type GUID
        (linux_data, OTHER, 400, 500),
        (GPT_ROOT_AARCH64, OTHER, 600, 700),
    ]);
    let mut out = [Partition::default(); 8];
    assert_eq!(discover(&mut m.reader(), Some(DISK), &mut out), Ok(3));
    assert_eq!(&out[..3], &[
        Partition { start: 34, sectors: 100, root: false },
        Partition { start: 400, sectors: 101, root: false },
        Partition { start: 600, sectors: 101, root: true },
    ]);
}

#[test]
fn discover_takes_the_backup_and_trusts_no_corrupt_table() {
    let parts = [(ESP_GUID, UNIQUE, 34, 133)];
    let mut out = [Partition::default(); 8];
    // A primary whose "EFI PART" is gone: the protective MBR says GPT, and
    // the backup is read.
    let mut m = Mem::gpt_disk(&parts);
    m.sectors[1][0] = 0;
    assert_eq!(discover(&mut m.reader(), Some(DISK), &mut out), Ok(1));
    // Without the capacity there is no backup to read.
    assert_eq!(discover(&mut m.reader(), None, &mut out), Ok(0));
    // Both corrupt: no partitions.
    m.sectors[(DISK - 1) as usize][20] ^= 1;
    assert_eq!(discover(&mut m.reader(), Some(DISK), &mut out), Ok(0));
    // Sector 0 unread is the one failure discover reports.
    let mut m = Mem::gpt_disk(&parts);
    m.fail.push(0);
    assert_eq!(discover(&mut m.reader(), Some(DISK), &mut out), Err(Unreadable));
}

#[test]
fn discover_trusts_only_a_copy_that_validated() {
    // The primary's array unread: that copy said nothing, and the backup is
    // read, not the entries the primary handed over before its read failed
    // (the review of #296: every other corrupt-primary case was Invalid).
    let parts: Vec<_> = (0..6u64).map(|i| (ESP_GUID, UNIQUE, 100 + i * 10, 105 + i * 10)).collect();
    let mut out = [Partition::default(); 8];
    let mut m = Mem::gpt_disk(&parts);
    m.fail.push(2);
    assert_eq!(discover(&mut m.reader(), Some(DISK), &mut out), Ok(6));
    // Its second array sector unread, after the first handed four entries
    // over: still the backup's six.
    let mut m = Mem::gpt_disk(&parts);
    m.fail.push(3);
    out = [Partition::default(); 8];
    assert_eq!(discover(&mut m.reader(), Some(DISK), &mut out), Ok(6));
    assert_eq!(out[5].start, 150);
    // Both arrays unread: no partitions.
    m.fail.push(DISK - 32);
    assert_eq!(discover(&mut m.reader(), Some(DISK), &mut out), Ok(0));
}

#[test]
fn an_entry_index_past_four_is_no_entry() {
    let mut m = Mem::new();
    m.mbr(&[(0x0C, 2048, 1000)], 0);
    assert_eq!(mbr_entry(&m.sectors[0], 4), None);
    assert_eq!(mbr_entry(&m.sectors[0], 0).map(|e| e.start), Some(2048));
}

#[test]
fn a_gpt_entry_ending_before_it_starts_has_no_size() {
    let m = Mem::gpt_disk(&[(ESP_GUID, UNIQUE, 500, 400)]);
    let mut out = [Partition::default(); 2];
    assert_eq!(discover(&mut m.reader(), Some(DISK), &mut out), Ok(1));
    assert_eq!(out[0].sectors, 0);
}

#[test]
fn discover_stops_at_the_room_it_is_given() {
    let parts: Vec<_> = (0..10u64).map(|i| (ESP_GUID, UNIQUE, 100 + i * 10, 105 + i * 10)).collect();
    let m = Mem::gpt_disk(&parts);
    let mut out = [Partition::default(); 4];
    assert_eq!(discover(&mut m.reader(), Some(DISK), &mut out), Ok(4));
    assert_eq!(out[3].start, 130);
}

#[test]
fn is_gpt_by_header_or_protective_entry() {
    let m = Mem::gpt_disk(&[(ESP_GUID, UNIQUE, 34, 133)]);
    assert_eq!(is_gpt(&mut m.reader()), Some(true));
    let mut m = Mem::new();
    m.mbr(&[(PROTECTIVE, 1, 100)], 0);
    assert_eq!(is_gpt(&mut m.reader()), Some(true), "a protective MBR alone");
    let mut m = Mem::new();
    m.mbr(&[(0x0C, 2048, 100)], 0);
    assert_eq!(is_gpt(&mut m.reader()), Some(false));
    m.fail.push(1);
    assert_eq!(is_gpt(&mut m.reader()), None);
}

#[test]
fn blank_is_nothing_a_table_reader_or_firmware_takes() {
    let m = Mem::new();
    assert_eq!(is_blank(&mut m.reader()), Some(true), "zeros");
    let mut m = Mem::new();
    m.sectors[0][..446].fill(0xFF);
    m.sectors[1].fill(0xFF);
    assert_eq!(is_blank(&mut m.reader()), Some(true), "boot code and no signature, no entries");
    let mut m = Mem::new();
    m.mbr(&[], 0);
    assert_eq!(is_blank(&mut m.reader()), Some(false), "the signature alone");
    let mut m = Mem::new();
    m.mbr(&[(0x0C, 2048, 100)], 0);
    m.sectors[0][510..512].fill(0);
    assert_eq!(is_blank(&mut m.reader()), Some(false), "a damaged signature, an entry fsd lists");
    let mut m = Mem::new();
    m.sectors[0][0x1BE + 4] = 0x0C;
    assert_eq!(is_blank(&mut m.reader()), Some(true), "an entry at LBA 0, which discover leaves out");
    let mut m = Mem::gpt_disk(&[(ESP_GUID, UNIQUE, 34, 133)]);
    m.sectors[0][510..512].fill(0);
    assert_eq!(is_blank(&mut m.reader()), Some(false), "a GPT whose protective MBR is unsigned");
    let mut m = Mem::new();
    m.sectors[0][0x1BE + 4] = PROTECTIVE;
    assert_eq!(is_blank(&mut m.reader()), Some(false), "an unsigned protective entry, which EDK2 reads");
    let mut m = Mem::new();
    m.fail.push(0);
    assert_eq!(is_blank(&mut m.reader()), None);
    let mut m = Mem::new();
    m.fail.push(1);
    assert_eq!(is_blank(&mut m.reader()), None);
}

#[test]
fn boot_start_answers_the_table_as_the_disk_is_read() {
    let esp = mbr_boot(1, 2048, 1000, DiskSignature::Mbr(0x1234));
    let mut m = Mem::new();
    m.mbr(&[(0x0C, 2048, 1000)], 0x1234);
    assert_eq!(boot_start(&esp, &mut m.reader(), DISK), Ok(2048));
    // `partition`'s table: its own signature, the same entry.
    m.mbr(&[(0x83, 2048, 1000)], 0x5678);
    assert_eq!(boot_start(&esp, &mut m.reader(), DISK), Ok(0));
    m.fail.push(0);
    assert_eq!(boot_start(&esp, &mut m.reader(), DISK), Err(Unreadable));
    let unknown = BootPartition { table: Table::Unknown, ..esp };
    assert_eq!(boot_start(&unknown, &mut m.reader(), DISK), Ok(0));
}

#[test]
fn boot_start_takes_no_gpt_backup_the_disk_is_not_read_by() {
    let boot = gpt_boot(DiskSignature::Gpt(UNIQUE));
    let m = Mem::gpt_disk(&[(ESP_GUID, UNIQUE, 34, 133)]);
    assert_eq!(boot_start(&boot, &mut m.reader(), DISK), Ok(34));
    // The primary gone with a protective MBR kept: still GPT, read from the
    // backup, as discover reads it.
    let mut m = Mem::gpt_disk(&[(ESP_GUID, UNIQUE, 34, 133)]);
    m.sectors[1] = [0u8; SECTOR];
    assert_eq!(boot_start(&boot, &mut m.reader(), DISK), Ok(34));
    // `erase` and `partition`: sectors 0 and 1 rewritten as an MBR, the
    // backup at the disk's end left whole. is_boot_disk still finds the
    // partition there; discover reads the MBR, so no start is answered.
    m.sectors[0] = [0u8; SECTOR];
    m.mbr(&[(0x83, 2048, 1000)], 0x5678);
    assert_eq!(ask(&m, &boot), Some(Boot::Yes), "the backup alone still validates");
    assert_eq!(boot_start(&boot, &mut m.reader(), DISK), Ok(0));
    // An MBR entry under a GPT header fsd reads by: not the table it reads.
    let mut m = Mem::gpt_disk(&[(ESP_GUID, UNIQUE, 34, 133)]);
    m.sectors[0] = [0u8; SECTOR];
    m.mbr(&[(0x0C, 2048, 1000)], 0x1234);
    let esp = mbr_boot(1, 2048, 1000, DiskSignature::Mbr(0x1234));
    assert_eq!(ask(&m, &esp), Some(Boot::Yes), "the MBR alone holds it");
    assert_eq!(boot_start(&esp, &mut m.reader(), DISK), Ok(0));
    // A failed read is not an answer.
    let mut m = Mem::gpt_disk(&[(ESP_GUID, UNIQUE, 34, 133)]);
    m.fail.push(0);
    assert_eq!(boot_start(&boot, &mut m.reader(), DISK), Err(Unreadable));
}
