extern crate std;
use super::*;
use std::vec::Vec;

/// QEMU `virt`'s first two virtio-mmio declarations, byte for byte from
/// its reference DSDT (`tests/data/acpi/aarch64/virt/DSDT`, QEMU 11.1.3).
const VR00: &[u8] = &[
    0x5b, 0x82, 0x41, 0x04, b'V', b'R', b'0', b'0', 0x08, b'_', b'H', b'I', b'D', 0x0d, b'L', b'N', b'R', b'O', b'0',
    b'0', b'0', b'5', 0x00, 0x08, b'_', b'U', b'I', b'D', 0x00, 0x08, b'_', b'C', b'C', b'A', 0x01, 0x08, b'_', b'C',
    b'R', b'S', 0x11, 0x1a, 0x0a, 0x17, 0x86, 0x09, 0x00, 0x01, 0x00, 0x00, 0x00, 0x0a, 0x00, 0x02, 0x00, 0x00, 0x89,
    0x06, 0x00, 0x01, 0x01, 0x30, 0x00, 0x00, 0x00, 0x79, 0x00,
];
const VR01: &[u8] = &[
    0x5b, 0x82, 0x41, 0x04, b'V', b'R', b'0', b'1', 0x08, b'_', b'H', b'I', b'D', 0x0d, b'L', b'N', b'R', b'O', b'0',
    b'0', b'0', b'5', 0x00, 0x08, b'_', b'U', b'I', b'D', 0x01, 0x08, b'_', b'C', b'C', b'A', 0x01, 0x08, b'_', b'C',
    b'R', b'S', 0x11, 0x1a, 0x0a, 0x17, 0x86, 0x09, 0x00, 0x01, 0x00, 0x02, 0x00, 0x0a, 0x00, 0x02, 0x00, 0x00, 0x89,
    0x06, 0x00, 0x01, 0x01, 0x31, 0x00, 0x00, 0x00, 0x79, 0x00,
];

/// A table: a header, then `body`. The header's contents are not read.
fn table(body: &[&[u8]]) -> Vec<u8> {
    let mut t = std::vec![0u8; HEADER_LEN];
    t[..4].copy_from_slice(b"DSDT");
    for b in body {
        t.extend_from_slice(b);
    }
    let len = t.len() as u32;
    t[4..8].copy_from_slice(&len.to_le_bytes());
    t
}

fn scan(t: &[u8]) -> Transports {
    let mut found = Transports::new();
    virtio_mmio(t, &mut found);
    found
}

/// A `Device` named `name` around `body`, with a one-byte PkgLength when
/// it fits and a two-byte one otherwise.
fn device(name: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut d = std::vec![0x5b, 0x82];
    let inner = 4 + body.len();
    if inner + 1 < 0x40 {
        d.push((inner + 1) as u8);
    } else {
        let len = inner + 2;
        d.push(0x40 | (len & 0x0f) as u8);
        d.push((len >> 4) as u8);
    }
    d.extend_from_slice(name);
    d.extend_from_slice(body);
    d
}

fn hid(id: &[u8]) -> Vec<u8> {
    let mut h = std::vec![0x08, b'_', b'H', b'I', b'D', 0x0d];
    h.extend_from_slice(id);
    h.push(0);
    h
}

/// `Name (_CRS, Buffer {...})` around `resources`, an end tag appended.
fn crs(resources: &[u8]) -> Vec<u8> {
    let mut data = resources.to_vec();
    data.extend_from_slice(&[0x79, 0x00]);
    let mut c = std::vec![0x08, b'_', b'C', b'R', b'S', 0x11, (3 + data.len()) as u8, 0x0a, data.len() as u8];
    c.extend_from_slice(&data);
    c
}

fn mem32(base: u32, size: u32) -> Vec<u8> {
    let mut m = std::vec![0x86, 0x09, 0x00, 0x01];
    m.extend_from_slice(&base.to_le_bytes());
    m.extend_from_slice(&size.to_le_bytes());
    m
}

#[test]
fn qemus_own_declarations_are_found_with_their_interrupts() {
    let found = scan(&table(&[VR00, VR01]));
    assert_eq!(
        found.as_slice(),
        &[
            Transport { base: 0x0a00_0000, size: 0x200, intid: Some(0x30) },
            Transport { base: 0x0a00_0200, size: 0x200, intid: Some(0x31) },
        ]
    );
    assert_eq!((found.dropped, found.unreadable), (0, 0));
}

#[test]
fn qemus_package_length_covers_the_whole_device() {
    // The two-byte PkgLength 41 04 is 0x41, counted from itself: the
    // device ends exactly where the bytes copied from QEMU end.
    assert_eq!(pkg_length(VR00, 2), Some((0x41, 2)));
    assert_eq!(2 + 0x41, VR00.len());
}

#[test]
fn a_table_with_no_declaration_gives_none() {
    assert!(scan(&table(&[])).is_empty());
    assert!(scan(&[0u8; HEADER_LEN]).is_empty());
    assert!(scan(&[]).is_empty());
    let other = device(b"L000", &[hid(b"PNP0C0F").as_slice(), crs(&mem32(0x0a00_0000, 0x200)).as_slice()].concat());
    assert!(scan(&table(&[&other])).is_empty());
}

#[test]
fn a_longer_id_starting_with_lnro0005_is_not_it() {
    let d = device(b"VR00", &[hid(b"LNRO00051").as_slice(), crs(&mem32(0x0a00_0000, 0x200)).as_slice()].concat());
    assert!(scan(&table(&[&d])).is_empty());
}

#[test]
fn a_crs_is_read_only_inside_its_own_device() {
    // A declaration with no _CRS, then another device that has one: the
    // second's address must not be taken for the first's.
    let bare = device(b"VR00", &hid(b"LNRO0005"));
    let next = device(b"L000", &[hid(b"PNP0C0F").as_slice(), crs(&mem32(0x0a00_0000, 0x200)).as_slice()].concat());
    let found = scan(&table(&[&bare, &next]));
    assert!(found.is_empty());
    assert_eq!(found.unreadable, 1);
}

#[test]
fn a_crs_without_memory_is_unreadable_not_a_transport() {
    let irq_only = [0x89, 0x06, 0x00, 0x01, 0x01, 0x30, 0x00, 0x00, 0x00];
    let d = device(b"VR00", &[hid(b"LNRO0005").as_slice(), crs(&irq_only).as_slice()].concat());
    let found = scan(&table(&[&d]));
    assert!(found.is_empty());
    assert_eq!(found.unreadable, 1);
}

#[test]
fn a_crs_with_no_interrupt_still_names_the_registers() {
    let d = device(b"VR00", &[hid(b"LNRO0005").as_slice(), crs(&mem32(0x0a00_0000, 0x200)).as_slice()].concat());
    assert_eq!(scan(&table(&[&d])).as_slice(), &[Transport { base: 0x0a00_0000, size: 0x200, intid: None }]);
}

#[test]
fn a_zero_length_register_block_is_not_a_transport() {
    let d = device(b"VR00", &[hid(b"LNRO0005").as_slice(), crs(&mem32(0x0a00_0000, 0)).as_slice()].concat());
    assert_eq!(scan(&table(&[&d])).unreadable, 1);
}

#[test]
fn a_word_sized_buffer_and_a_long_device_are_read() {
    let mut data = mem32(0x1000_0000, 0x1000);
    data.extend_from_slice(&[0x89, 0x06, 0x00, 0x01, 0x01, 0x40, 0x00, 0x00, 0x00, 0x79, 0x00]);
    let mut c = std::vec![0x08, b'_', b'C', b'R', b'S', 0x11, (4 + data.len()) as u8, 0x0b];
    c.extend_from_slice(&(data.len() as u16).to_le_bytes());
    c.extend_from_slice(&data);
    // Padding pushes the device past a one-byte PkgLength.
    let pad = [0xa3u8; 64];
    let d = device(b"VR00", &[hid(b"LNRO0005").as_slice(), &pad, c.as_slice()].concat());
    assert!(d[2] & 0xc0 != 0, "the device needs a two-byte PkgLength for this test");
    assert_eq!(scan(&table(&[&d])).as_slice(), &[Transport { base: 0x1000_0000, size: 0x1000, intid: Some(0x40) }]);
}

#[test]
fn more_than_the_maximum_are_counted_not_kept() {
    let devs: Vec<Vec<u8>> = (0..MAX_TRANSPORTS as u32 + 3)
        .map(|i| device(b"VRxx", &[hid(b"LNRO0005").as_slice(), crs(&mem32(0x0a00_0000 + i * 0x200, 0x200)).as_slice()].concat()))
        .collect();
    let parts: Vec<&[u8]> = devs.iter().map(|d| d.as_slice()).collect();
    let found = scan(&table(&parts));
    assert_eq!(found.as_slice().len(), MAX_TRANSPORTS);
    assert_eq!(found.dropped, 3);
}

#[test]
fn the_same_declaration_in_two_tables_is_kept_once() {
    let mut found = Transports::new();
    virtio_mmio(&table(&[VR00]), &mut found);
    virtio_mmio(&table(&[VR00]), &mut found);
    assert_eq!(found.as_slice().len(), 1);
}

#[test]
fn every_truncation_and_every_corrupted_byte_is_read_without_a_panic() {
    let whole = table(&[VR00, VR01]);
    for cut in 0..whole.len() {
        let _ = scan(&whole[..cut]);
    }
    for at in HEADER_LEN..whole.len() {
        for value in [0x00, 0x0a, 0x0b, 0x0c, 0x3f, 0x40, 0x79, 0x86, 0x89, 0xc0, 0xff] {
            let mut t = whole.clone();
            t[at] = value;
            let found = scan(&t);
            assert!(found.as_slice().len() <= 2);
        }
    }
}

#[test]
fn a_truncated_declaration_is_not_a_transport() {
    // The table ends inside VR00's _CRS: its device's length reaches past
    // the end, so no range holds it.
    let whole = table(&[VR00]);
    let cut = &whole[..whole.len() - 8];
    assert!(scan(cut).is_empty());
}

/// QEMU's whole reference DSDT, when `AMLSCAN_QEMU_DSDT` names a copy (not
/// committed: it is QEMU's file). All 32 slots, `0xa000000` up by 0x200,
/// each on SPI 16 + slot.
#[test]
fn qemus_whole_reference_dsdt_when_given() {
    let Ok(path) = std::env::var("AMLSCAN_QEMU_DSDT") else { return };
    let t = std::fs::read(path).expect("AMLSCAN_QEMU_DSDT names a readable file");
    let found = scan(&t);
    assert_eq!(found.as_slice().len(), 32);
    for (i, tr) in found.as_slice().iter().enumerate() {
        let i = i as u64;
        assert_eq!(*tr, Transport { base: 0x0a00_0000 + i * 0x200, size: 0x200, intid: Some(48 + i as u32) });
    }
    assert_eq!((found.dropped, found.unreadable), (0, 0));
}
