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
    scan_all(&[t])
}

fn scan_all(tables: &[&[u8]]) -> Transports {
    let mut found = Transports::new();
    declared(tables, VIRTIO_MMIO_HID, &mut found);
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
        .map(|i| {
            let name = [b'V', b'0' + (i / 100) as u8, b'0' + (i / 10 % 10) as u8, b'0' + (i % 10) as u8];
            device(&name, &[hid(b"LNRO0005").as_slice(), crs(&mem32(0x0a00_0000 + i * 0x200, 0x200)).as_slice()].concat())
        })
        .collect();
    let parts: Vec<&[u8]> = devs.iter().map(|d| d.as_slice()).collect();
    let found = scan(&table(&parts));
    assert_eq!(found.as_slice().len(), MAX_TRANSPORTS);
    assert_eq!(found.dropped, 3);
}

#[test]
fn the_same_device_defined_in_two_tables_is_not_taken() {
    // Two _HIDs and two _CRSs at one path: not a declaration this scan can
    // read as one device.
    let (a, b) = (table(&[VR00]), table(&[VR00]));
    let found = scan_all(&[&a, &b]);
    assert!(found.is_empty());
    assert_eq!(found.unreadable, 1);
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

/// Parallels 27.1's virtio-mmio GPU, byte for byte from its DSDT (read off
/// a boot on 2026-11-04): `_HID "LNRO0005"`, and a `_STA` method that
/// answers present only when the VM has that GPU (`If (GPU_ & 0x10)
/// Return (0x0F)`). A VM without it faults on the registers.
const PARALLELS_GPU1: &[u8] = &[
    0x5b, 0x82, 0x45, 0x06, b'G', b'P', b'U', b'1', 0x08, b'_', b'H', b'I', b'D', 0x0d, b'L', b'N', b'R', b'O', b'0',
    b'0', b'0', b'5', 0x00, 0x08, b'_', b'U', b'I', b'D', 0x0a, 0x04, 0x08, b'_', b'C', b'C', b'A', 0x01, 0x14, 0x15,
    b'_', b'S', b'T', b'A', 0x00, 0xa0, 0x0c, 0x7b, b'G', b'P', b'U', b'_', 0x0a, 0x10, 0x00, 0xa4, 0x0a, 0x0f, 0xa4,
    0x00, 0x08, b'_', b'C', b'L', b'S', 0x12, 0x07, 0x03, 0x0a, 0x03, 0x0a, 0x02, 0x00, 0x08, b'_', b'C', b'R', b'S',
    0x11, 0x1a, 0x0a, 0x17, 0x86, 0x09, 0x00, 0x01, 0x00, 0x00, 0x18, 0x02, 0x00, 0x40, 0x00, 0x00, 0x89, 0x06, 0x00,
    0x01, 0x01, 0x27, 0x00, 0x00, 0x00, 0x79, 0x00,
];

/// Parallels' first NIC, the same way: an EISA `_HID`, `LNRO0005` only as
/// its `_CID`, and a `_STA` method.
const PARALLELS_ETH0: &[u8] = &[
    0x5b, 0x82, 0x4c, 0x06, b'E', b'T', b'H', b'0', 0x08, b'_', b'H', b'I', b'D', 0x0c, 0x42, 0x4c, 0x40, 0x09, 0x08,
    b'_', b'C', b'I', b'D', 0x0d, b'L', b'N', b'R', b'O', b'0', b'0', b'0', b'5', 0x00, 0x08, b'_', b'U', b'I', b'D',
    0x00, 0x08, b'_', b'C', b'C', b'A', 0x01, 0x14, 0x14, b'_', b'S', b'T', b'A', 0x00, 0xa0, 0x0b, 0x7b, b'N', b'E',
    b'T', b'_', 0x01, 0x00, 0xa4, 0x0a, 0x0f, 0xa4, 0x00, 0x08, b'_', b'C', b'L', b'S', 0x12, 0x06, 0x03, 0x0a, 0x02,
    0x00, 0x00, 0x08, b'_', b'C', b'R', b'S', 0x11, 0x1a, 0x0a, 0x17, 0x86, 0x09, 0x00, 0x01, 0x00, 0x00, 0x1c, 0x02,
    0x00, 0x40, 0x00, 0x00, 0x89, 0x06, 0x00, 0x03, 0x01, 0x2c, 0x00, 0x00, 0x00, 0x79, 0x00,
];

#[test]
fn parallels_package_lengths_cover_their_devices() {
    assert_eq!(pkg_length(PARALLELS_GPU1, 2).map(|(n, _)| 2 + n), Some(PARALLELS_GPU1.len()));
    assert_eq!(pkg_length(PARALLELS_ETH0, 2).map(|(n, _)| 2 + n), Some(PARALLELS_ETH0.len()));
}

#[test]
fn a_status_method_keeps_parallels_gpu_out() {
    let found = scan(&table(&[PARALLELS_GPU1, PARALLELS_ETH0]));
    assert!(found.is_empty());
    assert_eq!((found.status_unknown, found.absent, found.unreadable), (1, 0, 0));
}

#[test]
fn a_device_naming_lnro0005_only_as_its_cid_is_not_taken() {
    let found = scan(&table(&[PARALLELS_ETH0]));
    assert!(found.is_empty());
    assert_eq!((found.status_unknown, found.absent, found.unreadable), (0, 0, 0));
}

fn with_status(sta: &[u8]) -> Vec<u8> {
    let mut name = std::vec![0x08, b'_', b'S', b'T', b'A'];
    name.extend_from_slice(sta);
    device(b"VR00", &[hid(b"LNRO0005").as_slice(), &name, crs(&mem32(0x0a00_0000, 0x200)).as_slice()].concat())
}

#[test]
fn a_named_status_is_read_by_its_present_and_enabled_bits() {
    for (sta, present) in [
        (&[0x0a, 0x0f][..], true),
        (&[0x0a, 0x03][..], true),
        (&[0xff][..], true),
        (&[0x0c, 0x0f, 0x00, 0x00, 0x00][..], true),
        (&[0x00][..], false),
        (&[0x01][..], false),
        (&[0x0a, 0x0d][..], false),
        (&[0x0b, 0x0e, 0x00][..], false),
    ] {
        let found = scan(&table(&[&with_status(sta)]));
        assert_eq!(!found.is_empty(), present, "_STA {sta:02x?}");
        assert_eq!(found.absent, usize::from(!present), "_STA {sta:02x?}");
    }
}

#[test]
fn a_status_that_is_not_a_constant_is_unknown() {
    // A NameOp, then something that is no integer constant.
    let found = scan(&table(&[&with_status(&[0x70])]));
    assert!(found.is_empty());
    assert_eq!(found.status_unknown, 1);
}

/// A child `Device` with its own `_CRS` and `_STA`, to nest inside a
/// declaring one.
fn child(sta: &[u8]) -> Vec<u8> {
    let mut name = std::vec![0x08, b'_', b'S', b'T', b'A'];
    name.extend_from_slice(sta);
    device(b"CH00", &[name.as_slice(), crs(&mem32(0x0219_0000, 0x200)).as_slice()].concat())
}

#[test]
fn a_childs_crs_is_never_the_declaring_devices() {
    // The declaring device has no _CRS of its own: nothing is taken, the
    // child's address above all.
    let d = device(b"VR00", &[hid(b"LNRO0005").as_slice(), child(&[0x0a, 0x0f]).as_slice()].concat());
    let found = scan(&table(&[&d]));
    assert!(found.is_empty());
    assert_eq!(found.unreadable, 1);
    // With its own _CRS after the child, its own is the one read.
    let d = device(
        b"VR00",
        &[hid(b"LNRO0005").as_slice(), child(&[0x0a, 0x0f]).as_slice(), crs(&mem32(0x0a00_0000, 0x200)).as_slice()]
            .concat(),
    );
    assert_eq!(scan(&table(&[&d])).as_slice(), &[Transport { base: 0x0a00_0000, size: 0x200, intid: None }]);
}

#[test]
fn a_childs_status_is_never_the_declaring_devices() {
    // A child saying present, before the declaring device's own _STA method
    // (Parallels' GPU shape): the method decides, so not taken.
    let method = [0x14, 0x0a, b'_', b'S', b'T', b'A', 0x00, 0xa4, 0x0a, 0x0f, 0xa4, 0x00];
    let d = device(
        b"GPU1",
        &[hid(b"LNRO0005").as_slice(), child(&[0x0a, 0x0f]).as_slice(), &method, crs(&mem32(0x0218_0000, 0x4000)).as_slice()]
            .concat(),
    );
    let found = scan(&table(&[&d]));
    assert!(found.is_empty());
    assert_eq!(found.status_unknown, 1);
    // A child saying absent does not keep a device with no _STA out.
    let d = device(
        b"VR00",
        &[hid(b"LNRO0005").as_slice(), child(&[0x00]).as_slice(), crs(&mem32(0x0a00_0000, 0x200)).as_slice()].concat(),
    );
    assert_eq!(scan(&table(&[&d])).as_slice().len(), 1);
}

#[test]
fn an_eight_byte_status_method_is_not_a_name() {
    // `Method (_STA) { Return (One) }`: 14 08 _STA 00 A4 01, whose
    // PkgLength byte is 0x08, NameOp's value. Read as the method it is, a
    // constant `_STA` since 2026-11-05 (`MethodShape::Sta`): One is present
    // but not enabled, so absent, not a Name and not a transport.
    let method = [0x14, 0x08, b'_', b'S', b'T', b'A', 0x00, 0xa4, 0x01];
    let d = device(b"VR00", &[hid(b"LNRO0005").as_slice(), &method, crs(&mem32(0x0a00_0000, 0x200)).as_slice()].concat());
    let found = scan(&table(&[&d]));
    assert!(found.is_empty());
    assert_eq!((found.status_unknown, found.absent), (0, 1));
    // The same with a two-byte PkgLength ending in 0x08.
    let method = [0x14, 0x48, 0x08, b'_', b'S', b'T', b'A', 0x00, 0xa4, 0x01];
    let d = device(b"VR00", &[hid(b"LNRO0005").as_slice(), &method, crs(&mem32(0x0a00_0000, 0x200)).as_slice()].concat());
    assert_eq!(scan(&table(&[&d])).status_unknown, 1);
}

fn name_int(seg: &[u8; 4], value: &[u8]) -> Vec<u8> {
    let mut n = std::vec![0x08];
    n.extend_from_slice(seg);
    n.extend_from_slice(value);
    n
}

#[test]
fn device_op_bytes_inside_a_constant_do_not_hide_the_status() {
    // The second review of #407, reproduced: Name (_UID, 0x825B) holds the
    // bytes 5B 82, which the byte search took for a child Device and
    // skipped, hiding Name (_STA, Zero), so the device read as present.
    let d = device(
        b"VR00",
        &[
            hid(b"LNRO0005").as_slice(),
            &name_int(b"_UID", &[0x0b, 0x5b, 0x82]),
            &name_int(b"_STA", &[0x00]),
            &name_int(b"_ADR", &[0x00]),
            crs(&mem32(0x0a00_0000, 0x200)).as_slice(),
        ]
        .concat(),
    );
    let found = scan(&table(&[&d]));
    assert!(found.is_empty());
    assert_eq!(found.absent, 1);
}

#[test]
fn a_constant_ending_in_a_method_op_byte_does_not_make_status_a_method() {
    // Name (_UID, 0x14), then Name (_STA, 0x0F): 0A 14 08 _STA looked like
    // MethodOp 14, PkgLength 08.
    let d = device(
        b"VR00",
        &[
            hid(b"LNRO0005").as_slice(),
            &name_int(b"_UID", &[0x0a, 0x14]),
            &name_int(b"_STA", &[0x0a, 0x0f]),
            crs(&mem32(0x0a00_0000, 0x200)).as_slice(),
        ]
        .concat(),
    );
    assert_eq!(scan(&table(&[&d])).as_slice().len(), 1);
}

#[test]
fn a_power_resources_status_is_not_the_devices() {
    // PowerResource (PR00, 0, 0) { Name (_STA, Zero) } inside the device.
    let inner = name_int(b"_STA", &[0x00]);
    let mut pr = std::vec![0x5b, 0x84, (1 + 4 + 3 + inner.len()) as u8, b'P', b'R', b'0', b'0', 0x00, 0x00, 0x00];
    pr.extend_from_slice(&inner);
    let d = device(b"VR00", &[hid(b"LNRO0005").as_slice(), &pr, crs(&mem32(0x0a00_0000, 0x200)).as_slice()].concat());
    let found = scan(&table(&[&d]));
    assert_eq!(found.as_slice().len(), 1, "absent {} unknown {}", found.absent, found.status_unknown);
}

#[test]
fn a_status_method_after_a_buffer_holding_device_op_bytes_is_still_seen() {
    let mut buf_name = std::vec![0x08, b'B', b'U', b'F', b'0', 0x11, 0x06, 0x0a, 0x03, 0x5b, 0x82, 0x08];
    let method = [0x14, 0x0a, b'_', b'S', b'T', b'A', 0x00, 0xa4, 0x0a, 0x0f, 0xa4, 0x00];
    buf_name.extend_from_slice(&method);
    let d = device(b"GPU1", &[hid(b"LNRO0005").as_slice(), &buf_name, crs(&mem32(0x0218_0000, 0x4000)).as_slice()].concat());
    let found = scan(&table(&[&d]));
    assert!(found.is_empty());
    assert_eq!(found.status_unknown, 1);
}

#[test]
fn a_child_running_past_its_parent_ends_the_walk_and_is_not_taken() {
    let mut ch = child(&[0x0a, 0x0f]);
    ch[2] += 4; // the child's PkgLength now reaches past the parent's end
    let d = device(b"VR00", &[hid(b"LNRO0005").as_slice(), ch.as_slice()].concat());
    let found = scan(&table(&[&d]));
    assert!(found.is_empty());
    assert_eq!(found.status_unknown, 1);
}

#[test]
fn an_op_the_walk_does_not_know_makes_a_device_without_a_constant_status_unknown() {
    // Store (One, Local0) at device level: executed at load, not walked.
    let store = [0x70, 0x01, 0x60];
    let d = device(b"VR00", &[hid(b"LNRO0005").as_slice(), crs(&mem32(0x0a00_0000, 0x200)).as_slice(), &store].concat());
    let found = scan(&table(&[&d]));
    assert!(found.is_empty());
    assert_eq!(found.status_unknown, 1);
    // Not even a constant _STA saying present: load-time code such as this
    // Store may overwrite it before the OS reads it (the fourth review of
    // #407).
    let d = device(
        b"VR00",
        &[hid(b"LNRO0005").as_slice(), &name_int(b"_STA", &[0x0a, 0x0f]), crs(&mem32(0x0a00_0000, 0x200)).as_slice(), &store]
            .concat(),
    );
    let found = scan(&table(&[&d]));
    assert!(found.is_empty());
    assert_eq!(found.status_unknown, 1);
}

#[test]
fn an_operation_region_with_constant_bounds_is_walked_past() {
    // OperationRegion (REG0, SystemMemory, 0x1000, 0x10), then the _STA.
    let opregion = [0x5b, 0x80, b'R', b'E', b'G', b'0', 0x00, 0x0b, 0x00, 0x10, 0x0a, 0x10];
    let d = device(
        b"VR00",
        &[hid(b"LNRO0005").as_slice(), &opregion, &name_int(b"_STA", &[0x00]), crs(&mem32(0x0a00_0000, 0x200)).as_slice()]
            .concat(),
    );
    let found = scan(&table(&[&d]));
    assert!(found.is_empty());
    assert_eq!(found.absent, 1);
}

#[test]
fn a_hid_inside_a_value_is_not_the_devices() {
    // The _HID's bytes inside a buffer of an ordinary device: the walk of
    // that device never meets them as a Name of its own.
    let mut inner = hid(b"LNRO0005");
    inner.extend_from_slice(&crs(&mem32(0x0a00_0000, 0x200)));
    let mut buf = std::vec![0x08, b'B', b'U', b'F', b'0', 0x11, (3 + inner.len()) as u8, 0x0a, inner.len() as u8];
    buf.extend_from_slice(&inner);
    let d = device(b"L000", &[hid(b"PNP0C0F").as_slice(), buf.as_slice()].concat());
    let found = scan(&table(&[&d]));
    assert!(found.is_empty());
    // Not a declaration at all, so not a line in the boot log either (the
    // third review of #407).
    assert_eq!((found.unreadable, found.status_unknown, found.absent), (0, 0, 0));
}

#[test]
fn device_op_bytes_in_a_value_before_the_hid_are_not_the_device() {
    // The third review of #407, reproduced: Name (_UID, QWord) whose bytes
    // hold 5B 82 LL 00, a "Device" with a null name whose package ends
    // before the real device's Name (_STA, Zero).
    let tail_len = (hid(b"LNRO0005").len() + crs(&mem32(0x0a00_0000, 0x200)).len()) as u8;
    let uid = [0x08, b'_', b'U', b'I', b'D', 0x0e, 0x01, 0x02, 0x03, 0x04, 0x5b, 0x82, 2 + tail_len, 0x00];
    let d = device(
        b"VR00",
        &[uid.as_slice(), hid(b"LNRO0005").as_slice(), crs(&mem32(0x0a00_0000, 0x200)).as_slice(), &name_int(b"_STA", &[0x00])]
            .concat(),
    );
    let found = scan(&table(&[&d]));
    assert!(found.is_empty(), "taken although its _STA says absent");
}

#[test]
fn a_status_inside_an_if_at_device_level_is_not_missed() {
    // The third review of #407, reproduced: If (One) { Name (_STA, Zero) }.
    let inner = name_int(b"_STA", &[0x00]);
    let mut iff = std::vec![0xa0, (2 + inner.len()) as u8, 0x01];
    iff.extend_from_slice(&inner);
    let d = device(b"VR00", &[hid(b"LNRO0005").as_slice(), &iff, crs(&mem32(0x0a00_0000, 0x200)).as_slice()].concat());
    let found = scan(&table(&[&d]));
    assert!(found.is_empty(), "taken although a _STA inside its If may say absent");
}


/// A Scope of `path` (a NameString's bytes) around `body`.
fn scope(path: &[u8], body: &[u8]) -> Vec<u8> {
    let mut s = std::vec![0x10];
    s.extend_from_slice(&pkg_len(path.len() + body.len()));
    s.extend_from_slice(path);
    s.extend_from_slice(body);
    s
}

/// The PkgLength encoding for a package of `inner` bytes after it, in the
/// fewest bytes (1 to 4) that hold it.
fn pkg_len(inner: usize) -> Vec<u8> {
    if inner + 1 < 0x40 {
        return std::vec![(inner + 1) as u8];
    }
    for follow in 1..=3usize {
        let len = inner + 1 + follow;
        if len < 1 << (4 + 8 * follow) {
            let mut v = std::vec![((follow as u8) << 6) | (len & 0x0f) as u8];
            for i in 0..follow {
                v.push((len >> (4 + 8 * i)) as u8);
            }
            return v;
        }
    }
    panic!("a package too long for a PkgLength");
}

#[test]
fn the_test_pkg_len_is_read_back_by_the_scans() {
    for inner in [0, 0x3e, 0x3f, 0x100, 0xffd, 0xffe, 0x10_0000, 0xff_fff0] {
        let enc = pkg_len(inner);
        assert_eq!(pkg_length(&enc, 0), Some((inner + enc.len(), enc.len())), "inner {inner:#x}");
    }
}

/// QEMU's declaration with no _STA, under \_SB_ as QEMU puts it.
fn qemu_under_sb() -> Vec<u8> {
    scope(b"\\_SB_", VR00)
}

#[test]
fn qemus_declaration_under_sb_is_found() {
    assert_eq!(scan(&table(&[&qemu_under_sb()])).as_slice().len(), 1);
}

#[test]
fn a_status_added_by_a_later_scope_of_the_device_is_seen() {
    // The third review of #407: Scope (\_SB.VR00) { Name (_STA, Zero) }
    // later in the same table.
    let later = scope(b"\\\x2e_SB_VR00", &name_int(b"_STA", &[0x00]));
    let found = scan(&table(&[&qemu_under_sb(), &later]));
    assert!(found.is_empty());
    assert_eq!(found.absent, 1);
}

#[test]
fn a_status_added_in_another_table_is_seen() {
    // The same Scope in an SSDT.
    let dsdt = table(&[&qemu_under_sb()]);
    let ssdt = table(&[&scope(b"\\\x2e_SB_VR00", &name_int(b"_STA", &[0x00]))]);
    let found = scan_all(&[&dsdt, &ssdt]);
    assert!(found.is_empty());
    assert_eq!(found.absent, 1);
    // And a method there.
    let method = [0x14, 0x0a, b'_', b'S', b'T', b'A', 0x00, 0xa4, 0x0a, 0x0f, 0xa4, 0x00];
    let ssdt = table(&[&scope(b"\\\x2e_SB_VR00", &method)]);
    assert_eq!(scan_all(&[&dsdt, &ssdt]).status_unknown, 1);
}

#[test]
fn a_status_named_by_its_full_path_is_seen() {
    // Name (\_SB.VR00._STA, Zero), at the table's top level.
    let mut name = std::vec![0x08, b'\\', 0x2f, 0x03];
    name.extend_from_slice(b"_SB_VR00_STA");
    name.push(0x00);
    let found = scan(&table(&[&qemu_under_sb(), &name]));
    assert!(found.is_empty());
    assert_eq!(found.absent, 1);
}

#[test]
fn a_status_named_from_a_parent_prefix_is_seen() {
    // Inside \_SB_.L000: Name (^VR00._STA, Zero).
    let mut name = std::vec![0x08, b'^', 0x2e];
    name.extend_from_slice(b"VR00_STA");
    name.push(0x00);
    let l000 = device(b"L000", &[hid(b"PNP0C0F").as_slice(), &name].concat());
    let sb = scope(b"\\_SB_", &[VR00, l000.as_slice()].concat());
    let found = scan(&table(&[&sb]));
    assert!(found.is_empty());
    assert_eq!(found.absent, 1);
}

#[test]
fn an_if_anywhere_outside_a_method_makes_a_device_without_a_constant_status_unknown() {
    // If (One) { } in a different device altogether: it could define any
    // name, \_SB.VR00._STA among them.
    let iff = [0xa0, 0x02, 0x01];
    let other = device(b"L000", &[hid(b"PNP0C0F").as_slice(), &iff].concat());
    let found = scan(&table(&[&scope(b"\\_SB_", &[VR00, other.as_slice()].concat())]));
    assert!(found.is_empty());
    assert_eq!(found.status_unknown, 1);
}

#[test]
fn a_status_on_a_power_resource_or_processor_inside_is_theirs() {
    // Processor (CPU0, 0, 0, 0) { Name (_STA, Zero) } inside the device:
    // its _STA is \VR00.CPU0's.
    let inner = name_int(b"_STA", &[0x00]);
    let mut cpu = std::vec![0x5b, 0x83, (1 + 4 + 6 + inner.len()) as u8, b'C', b'P', b'U', b'0', 0, 0, 0, 0, 0, 0];
    cpu.extend_from_slice(&inner);
    let d = device(b"VR00", &[hid(b"LNRO0005").as_slice(), &cpu, crs(&mem32(0x0a00_0000, 0x200)).as_slice()].concat());
    assert_eq!(scan(&table(&[&d])).as_slice().len(), 1);
}

#[test]
fn a_device_named_but_never_defined_as_a_device_is_not_taken() {
    // Name (\VR00._HID, "LNRO0005") and its _CRS with no Device (VR00).
    let mut hid_name = std::vec![0x08, b'\\', 0x2e];
    hid_name.extend_from_slice(b"VR00_HID");
    hid_name.push(0x0d);
    hid_name.extend_from_slice(b"LNRO0005\0");
    let mut crs_name = std::vec![0x08, b'\\', 0x2e];
    crs_name.extend_from_slice(b"VR00");
    crs_name.extend_from_slice(&crs(&mem32(0x0a00_0000, 0x200))[1..]);
    let found = scan(&table(&[&hid_name, &crs_name]));
    assert!(found.is_empty());
    assert_eq!(found.unreadable, 1);
}

#[test]
fn nesting_past_the_limit_is_uncertain_not_a_stack_overflow() {
    // Scope (\) { Scope (\) { ... } } ten thousand deep: the path stays at
    // the root, so only the nesting bound stops it (the fourth review of
    // #407).
    let mut body = qemu_under_sb();
    for _ in 0..10_000 {
        body = scope(b"\\\0", &body);
    }
    // Well formed all the way down: the root scope's package covers it all.
    assert_eq!(pkg_length(&body, 1).map(|(n, _)| 1 + n), Some(body.len()));
    // The device past the bound is never reached: nothing found, nothing
    // probed, and the walk returned instead of running the stack out.
    assert!(scan(&table(&[&body])).is_empty());
}

#[test]
fn a_method_naming_sta_keeps_a_constant_status_out() {
    // Method (\_SB._INI) { Store (Zero, \_SB.VR00._STA) }: ACPI runs
    // \_SB._INI before any device's _STA, so the constant may not be what
    // the OS reads.
    let mut ini = std::vec![0x14, 0x00, b'_', b'I', b'N', b'I', 0x00, 0x70, 0x00, b'\\', 0x2f, 0x03];
    ini.extend_from_slice(b"_SB_VR00_STA");
    ini[1] = (ini.len() - 1) as u8;
    let vr = device(
        b"VR00",
        &[hid(b"LNRO0005").as_slice(), &name_int(b"_STA", &[0x0a, 0x0f]), crs(&mem32(0x0a00_0000, 0x200)).as_slice()].concat(),
    );
    let sb = scope(b"\\_SB_", &[vr.as_slice(), &ini].concat());
    let found = scan(&table(&[&sb]));
    assert!(found.is_empty());
    assert_eq!(found.status_unknown, 1);
    // Without the method, the constant is trusted.
    let sb = scope(b"\\_SB_", &vr);
    assert_eq!(scan(&table(&[&sb])).as_slice().len(), 1);
}

#[test]
fn a_method_that_loads_a_table_makes_the_scan_uncertain() {
    // Method (LDT0) { LoadTable (...) }: the 5B 1F bytes in its body.
    let method = [0x14, 0x08, b'L', b'D', b'T', b'0', 0x00, 0x5b, 0x1f, 0x00];
    let found = scan(&table(&[&qemu_under_sb(), &method]));
    assert!(found.is_empty());
    assert_eq!(found.status_unknown, 1);
}

#[test]
fn a_field_unit_named_sta_is_a_definition() {
    // OperationRegion (R000, SystemMemory, 0x1000, 4)
    // Field (R000, ByteAcc, NoLock, Preserve) { _STA, 8 }
    let opregion = [0x5b, 0x80, b'R', b'0', b'0', b'0', 0x00, 0x0b, 0x00, 0x10, 0x0a, 0x04];
    let field_body = [b'R', b'0', b'0', b'0', 0x01, b'_', b'S', b'T', b'A', 0x08];
    let mut field = std::vec![0x5b, 0x81, (1 + field_body.len()) as u8];
    field.extend_from_slice(&field_body);
    let d = device(b"VR00", &[hid(b"LNRO0005").as_slice(), &opregion, &field, crs(&mem32(0x0a00_0000, 0x200)).as_slice()].concat());
    let found = scan(&table(&[&d]));
    assert!(found.is_empty());
    assert_eq!(found.status_unknown, 1);
}

#[test]
fn a_crs_value_running_past_its_device_is_not_read() {
    // The device's PkgLength ends inside its _CRS buffer: the value is not
    // recorded, and the scan is uncertain.
    let mut d = device(b"VR00", &[hid(b"LNRO0005").as_slice(), &name_int(b"_STA", &[0x0a, 0x0f]), crs(&mem32(0x0a00_0000, 0x200)).as_slice()].concat());
    d[2] -= 6;
    let found = scan(&table(&[&d, &[0xa3; 6]]));
    assert!(found.is_empty());
}

#[test]
fn a_child_that_cannot_be_read_does_not_hide_the_devices_after_it() {
    // A device holding an unknown op, then QEMU's declaration: both are
    // counted, the second as unknown since the scan is uncertain (the
    // fourth review of #407: it went uncounted).
    let bad = device(b"L000", &[hid(b"PNP0C0F").as_slice(), &[0x70, 0x01, 0x60]].concat());
    let found = scan(&table(&[&scope(b"\\_SB_", &[bad.as_slice(), VR00].concat())]));
    assert!(found.is_empty());
    assert_eq!(found.status_unknown, 1);
}

#[test]
fn a_value_running_past_its_scope_is_never_recorded() {
    // White-box: what the walk hands pass 2. Through the public scan the
    // device is already kept out, as uncertain; this pins the bound itself.
    let mut d = device(b"VR00", &[hid(b"LNRO0005").as_slice(), crs(&mem32(0x0a00_0000, 0x200)).as_slice()].concat());
    d[2] -= 6;
    let t = table(&[&d, &[0xa3; 6]]);
    let mut seen = Seen::default();
    let mut find = FindHids { hid: VIRTIO_MMIO_HID, paths: [Path::ROOT; MAX_CANDIDATES], count: 0, dropped: 0 };
    walk_table(&t, &mut find, &mut seen);
    assert_eq!(find.count, 1);
    let mut facts = [Facts::default(); MAX_CANDIDATES];
    let mut g = Gather { paths: &find.paths[..1], facts: &mut facts[..1] };
    walk_table(&t, &mut g, &mut seen);
    assert!(facts[0].crs.is_none() && !facts[0].crs_bad, "the overrunning _CRS was recorded");
    assert!(seen.uncertain);
}

/// `hid_string_at`: an SSDT shaped as edk2-platforms' `Emmc.asl` declares
/// EMMC2, `Device (SDC3)` under `Scope (\_SB)`.
#[test]
fn hid_string_found_where_written() {
    let dev = device(b"SDC3", &hid(b"BRCME88C"));
    let t = table(&[&[0x10, (1 + 5 + dev.len()) as u8, 0x5c, b'_', b'S', b'B', b'_'], &dev]);
    let at = hid_string_at(&t, b"BRCME88C").expect("declared");
    assert_eq!(&t[at..at + 6], &[0x08, b'_', b'H', b'I', b'D', 0x0d]);
    assert_eq!(at, HEADER_LEN + 7 + 2 + 1 + 4);
}

#[test]
fn hid_string_must_end_where_the_id_does() {
    // A longer string that starts with the id, a shorter id, another id.
    let t = table(&[&device(b"SDC3", &hid(b"BRCME88CX"))]);
    assert_eq!(hid_string_at(&t, b"BRCME88C"), None);
    let t = table(&[&device(b"SDC1", &hid(b"BCM2847"))]);
    assert_eq!(hid_string_at(&t, b"BRCME88C"), None);
    assert!(hid_string_at(&t, b"BCM2847").is_some());
    assert_eq!(hid_string_at(&t, b""), None);
}

#[test]
fn hid_string_is_only_a_hid() {
    // The same string as a `_CID`, or as a bare string, is not a `_HID`.
    let mut cid = hid(b"BRCME88C");
    cid[3] = b'C';
    cid[4] = b'I';
    let t = table(&[&device(b"SDC3", &cid), b"\x0dBRCME88C\x00"]);
    assert_eq!(hid_string_at(&t, b"BRCME88C"), None);
}

/// The Raspberry Pi 4's own tables, dumped off the board by the kernel's
/// `\SDDUMP` on 2026-11-05 (pftf v1.53, `docs/roadmap/roadmap-sd-card.md`
/// step 0), each with its ACPI checksum whole: the SSDT `RPI4EMMC` that
/// declares EMMC2, and the DSDT beside it.
const PI4_SSDT_EMMC: &[u8] = include_bytes!("../testdata/pi4-ssdt-rpi4emmc.aml");
const PI4_DSDT: &[u8] = include_bytes!("../testdata/pi4-dsdt.aml");

fn emmc2(tables: &[&[u8]]) -> Transports {
    let mut found = Transports::new();
    declared(tables, EMMC2_HID, &mut found);
    found
}

const PI4_EMMC2: Transport = Transport { base: 0xfe34_0000, size: 0x100, intid: Some(158) };

#[test]
fn the_pi4_tables_are_the_ones_dumped() {
    for t in [PI4_SSDT_EMMC, PI4_DSDT] {
        assert_eq!(u32::from_le_bytes(t[4..8].try_into().unwrap()) as usize, t.len());
        assert_eq!(t.iter().fold(0u8, |a, &b| a.wrapping_add(b)), 0, "checksum");
    }
}

#[test]
fn the_pi4s_emmc2_is_read_from_its_methods() {
    // `Method (_STA) { Return (0x0F) }`, and a `_CRS` method storing
    // 0xFE340000 into `RBUF` at byte 4 (`MethodShape`).
    let found = emmc2(&[PI4_SSDT_EMMC]);
    assert_eq!(found.as_slice(), &[PI4_EMMC2]);
    assert_eq!((found.unreadable, found.absent, found.status_unknown), (0, 0, 0));
}

#[test]
fn the_pi4s_emmc2_is_read_beside_its_dsdt() {
    // As the kernel scans them, every table at once: nothing in the DSDT
    // makes the walk uncertain or names `_STA` inside a method.
    let found = emmc2(&[PI4_DSDT, PI4_SSDT_EMMC]);
    assert_eq!(found.as_slice(), &[PI4_EMMC2]);
    let mut virtio = Transports::new();
    declared(&[PI4_DSDT, PI4_SSDT_EMMC], VIRTIO_MMIO_HID, &mut virtio);
    assert!(virtio.is_empty(), "the Pi declares no virtio-mmio");
}

/// The SSDT with `from` replaced by `to` once, at the only place `from`
/// occurs inside `SDC3` (the container `GDV1` before it has a `_CRS` of the
/// same shape), its length kept (the walk does not read the checksum).
fn pi4_ssdt_with(from: &[u8], to: &[u8]) -> Vec<u8> {
    assert_eq!(from.len(), to.len());
    let sdc3 = hid_string_at(PI4_SSDT_EMMC, EMMC2_HID).unwrap();
    let at: Vec<usize> = (sdc3..PI4_SSDT_EMMC.len() - from.len()).filter(|&i| &PI4_SSDT_EMMC[i..i + from.len()] == from).collect();
    assert_eq!(at.len(), 1, "the bytes to replace occur once");
    let mut t = PI4_SSDT_EMMC.to_vec();
    t[at[0]..at[0] + to.len()].copy_from_slice(to);
    t
}

#[test]
fn a_status_method_returning_zero_is_absent() {
    let t = pi4_ssdt_with(&[b'_', b'S', b'T', b'A', 0x00, 0xa4, 0x0a, 0x0f], &[b'_', b'S', b'T', b'A', 0x00, 0xa4, 0x0a, 0x00]);
    let found = emmc2(&[&t]);
    assert!(found.is_empty());
    assert_eq!(found.absent, 1);
}

#[test]
fn a_status_method_with_an_argument_is_unknown() {
    let t = pi4_ssdt_with(&[b'_', b'S', b'T', b'A', 0x00, 0xa4], &[b'_', b'S', b'T', b'A', 0x01, 0xa4]);
    let found = emmc2(&[&t]);
    assert!(found.is_empty());
    assert_eq!(found.status_unknown, 1);
}

#[test]
fn the_patch_lands_at_its_own_offset() {
    // CreateDWordField (^RBUF, 8, RBAS): the base is stored over the
    // length, so the registers read are not EMMC2's.
    let crs_create = [0x8a, 0x5e, b'R', b'B', b'U', b'F', 0x0a, 0x04, b'R', b'B', b'A', b'S', 0x70];
    let mut moved = crs_create;
    moved[7] = 0x08;
    let found = emmc2(&[&pi4_ssdt_with(&crs_create, &moved)]);
    assert_ne!(found.as_slice(), &[PI4_EMMC2]);
    // At 0x15, the DWord would end past the 23-byte buffer.
    let mut past = crs_create;
    past[7] = 0x15;
    let found = emmc2(&[&pi4_ssdt_with(&crs_create, &past)]);
    assert!(found.is_empty());
    assert_eq!(found.unreadable, 1);
}

#[test]
fn a_crs_method_with_another_statement_is_unreadable() {
    // The Return replaced by a second Store of the same length: not the
    // shape, so nothing is taken from it.
    let ret = [0xa4, 0x5e, b'R', b'B', b'U', b'F', 0x08, b'D', b'S', b'D', b'1'];
    let store = [0x70, 0x00, b'R', b'B', b'A', b'S', 0x08, b'D', b'S', b'D', b'1'];
    let found = emmc2(&[&pi4_ssdt_with(&ret, &store)]);
    assert!(found.is_empty());
    assert_eq!(found.unreadable, 1);
}

#[test]
fn a_crs_method_storing_into_another_field_is_unreadable() {
    let store = [0x70, 0x0c, 0x00, 0x00, 0x34, 0xfe, b'R', b'B', b'A', b'S'];
    let other = [0x70, 0x0c, 0x00, 0x00, 0x34, 0xfe, b'R', b'B', b'A', b'T'];
    let found = emmc2(&[&pi4_ssdt_with(&store, &other)]);
    assert!(found.is_empty());
    assert_eq!(found.unreadable, 1);
}

#[test]
fn a_crs_method_returning_another_buffer_is_unreadable() {
    let ret = [0xa4, 0x5e, b'R', b'B', b'U', b'F', 0x08, b'D', b'S', b'D', b'1'];
    let other = [0xa4, 0x5e, b'R', b'B', b'U', b'G', 0x08, b'D', b'S', b'D', b'1'];
    let found = emmc2(&[&pi4_ssdt_with(&ret, &other)]);
    assert!(found.is_empty());
    assert_eq!(found.unreadable, 1);
}

#[test]
fn the_buffer_redefined_elsewhere_is_unreadable() {
    // A second `RBUF` in the device (an Alias would do the same): which one
    // the method patches is not knowable here.
    let dup = [0x08, b'R', b'B', b'U', b'F', 0x11, 0x03, 0x0a, 0x00, 0xa3];
    let mut t = PI4_SSDT_EMMC.to_vec();
    // Into SDC3's body, right after its _HID, its PkgLength grown to match.
    let hid_end = hid_string_at(&t, EMMC2_HID).unwrap() + 15;
    t.splice(hid_end..hid_end, dup[..9].iter().copied());
    let dev = (0..t.len()).find(|&i| t[i..].starts_with(&[0x5b, 0x82]) && &t[i + 4..i + 8] == b"SDC3").unwrap();
    let (len, bytes) = pkg_length(&t, dev + 2).unwrap();
    assert_eq!(bytes, 2);
    let len = len + 9;
    t[dev + 2] = 0x40 | (len & 0x0f) as u8;
    t[dev + 3] = (len >> 4) as u8;
    // The enclosing GDV1 and \_SB scopes grow too.
    for name in [&b"GDV1"[..], &b"\\_SB_"[..]] {
        let at = (0..t.len()).find(|&i| t[i..].starts_with(name)).unwrap();
        // Both PkgLengths are two bytes, right before the name.
        let pkg_at = at - 2;
        let (l, b) = pkg_length(&t, pkg_at).unwrap();
        assert_eq!(b, 2);
        let l = l + 9;
        t[pkg_at] = 0x40 | (l & 0x0f) as u8;
        t[pkg_at + 1] = (l >> 4) as u8;
    }
    let total = t.len() as u32;
    t[4..8].copy_from_slice(&total.to_le_bytes());
    let found = emmc2(&[&t]);
    assert!(found.is_empty(), "{:?}", found.as_slice());
    assert_eq!(found.unreadable, 1);
}


/// A device `SDC3` of `_HID "BRCME88C"` with `Name (RBUF, ...)` over the
/// Pi's resources and a `_CRS` method of the patching shape, `extra` after
/// its Return.
fn patching_device(extra: &[u8]) -> Vec<u8> {
    let rbuf_data = [0x86, 0x09, 0x00, 0x01, 0, 0, 0, 0, 0x00, 0x01, 0x00, 0x00, 0x89, 0x06, 0x00, 0x09, 0x01, 0x9e, 0, 0, 0, 0x79, 0x00];
    let mut rbuf = std::vec![0x08, b'R', b'B', b'U', b'F', 0x11, (3 + rbuf_data.len()) as u8, 0x0a, rbuf_data.len() as u8];
    rbuf.extend_from_slice(&rbuf_data);
    let mut code = std::vec![0x08, 0x8a, 0x5e, b'R', b'B', b'U', b'F', 0x0a, 0x04, b'R', b'B', b'A', b'S'];
    code.extend_from_slice(&[0x70, 0x0c, 0x00, 0x00, 0x34, 0xfe, b'R', b'B', b'A', b'S', 0xa4, 0x5e, b'R', b'B', b'U', b'F']);
    code.extend_from_slice(extra);
    let mut method = std::vec![0x14, (1 + 4 + code.len()) as u8, b'_', b'C', b'R', b'S'];
    method.extend_from_slice(&code);
    device(b"SDC3", &[hid(b"BRCME88C").as_slice(), &rbuf, &method].concat())
}

#[test]
fn a_patching_crs_method_is_read_exactly() {
    assert_eq!(emmc2(&[&table(&[&patching_device(&[])])]).as_slice(), &[PI4_EMMC2]);
    // A Noop after the Return: not the shape.
    let found = emmc2(&[&table(&[&patching_device(&[0xa3])])]);
    assert!(found.is_empty());
    assert_eq!(found.unreadable, 1);
}

/// The Pi 4's on-board Ethernet, GENET v5, as its DSDT declares it
/// (`Device (ETH0)` under `\_SB`, read off the dumped table on 2026-11-12):
/// 64 KB of registers and two interrupts, of which the first is kept.
const PI4_GENET: Transport = Transport { base: 0xfd58_0000, size: 0x1_0000, intid: Some(189) };

fn genet(tables: &[&[u8]]) -> Transports {
    let mut found = Transports::new();
    declared(tables, GENET_HID, &mut found);
    found
}

#[test]
fn the_pi4s_genet_is_read_from_its_crs_method() {
    // `Method (_STA) { Return (0x0F) }`, and a `_CRS` method that names
    // its buffer inside itself and returns it (`MethodShape::CrsLocal`).
    let found = genet(&[PI4_DSDT]);
    assert_eq!(found.as_slice(), &[PI4_GENET]);
    assert_eq!((found.unreadable, found.absent, found.status_unknown, found.dropped), (0, 0, 0, 0));
    // As the kernel scans them, with the SSDT beside it, in either order.
    assert_eq!(genet(&[PI4_DSDT, PI4_SSDT_EMMC]).as_slice(), &[PI4_GENET]);
    assert_eq!(genet(&[PI4_SSDT_EMMC, PI4_DSDT]).as_slice(), &[PI4_GENET]);
    // And the SSDT alone declares none.
    assert!(genet(&[PI4_SSDT_EMMC]).is_empty());
}

/// The DSDT with `from` replaced by `to` once, at the only place `from`
/// occurs inside `ETH0`, its length kept. The device ends where its
/// PkgLength says, and the PkgLength is 6 bytes before the `_HID` (two
/// bytes of it, then the name `ETH0`).
fn pi4_dsdt_with(from: &[u8], to: &[u8]) -> Vec<u8> {
    assert_eq!(from.len(), to.len());
    let eth0 = hid_string_at(PI4_DSDT, GENET_HID).unwrap();
    assert_eq!(&PI4_DSDT[eth0 - 8..eth0 - 6], &[0x5b, 0x82], "ETH0's DeviceOp");
    assert_eq!(&PI4_DSDT[eth0 - 4..eth0], b"ETH0");
    let end = eth0 - 6 + pkg_length(PI4_DSDT, eth0 - 6).unwrap().0;
    let at: Vec<usize> = (eth0..=end - from.len()).filter(|&i| &PI4_DSDT[i..i + from.len()] == from).collect();
    assert_eq!(at.len(), 1, "the bytes to replace occur once");
    let mut t = PI4_DSDT.to_vec();
    t[at[0]..at[0] + to.len()].copy_from_slice(to);
    t
}

#[test]
fn the_genets_status_method_returning_zero_is_absent() {
    let t = pi4_dsdt_with(&[b'_', b'S', b'T', b'A', 0x00, 0xa4, 0x0a, 0x0f], &[b'_', b'S', b'T', b'A', 0x00, 0xa4, 0x0a, 0x00]);
    let found = genet(&[&t]);
    assert!(found.is_empty());
    assert_eq!(found.absent, 1);
}

#[test]
fn a_local_crs_method_with_an_argument_is_unreadable() {
    let t = pi4_dsdt_with(&[b'_', b'C', b'R', b'S', 0x08, 0x08], &[b'_', b'C', b'R', b'S', 0x09, 0x08]);
    let found = genet(&[&t]);
    assert!(found.is_empty());
    assert_eq!(found.unreadable, 1);
}

#[test]
fn a_local_crs_method_returning_another_name_is_unreadable() {
    // The Return names `RBUG`, which the method does not define: it could
    // be anything, anywhere.
    let ret = [0xa4, b'R', b'B', b'U', b'F', 0x08, b'_', b'D', b'S', b'D'];
    let mut other = ret;
    other[4] = b'G';
    let found = genet(&[&pi4_dsdt_with(&ret, &other)]);
    assert!(found.is_empty());
    assert_eq!(found.unreadable, 1);
    // And the same when it is the Name that differs.
    let name = [0x08, b'R', b'B', b'U', b'F', 0x11];
    let mut other = name;
    other[4] = b'G';
    let found = genet(&[&pi4_dsdt_with(&name, &other)]);
    assert!(found.is_empty());
    assert_eq!(found.unreadable, 1);
}

#[test]
fn a_local_crs_method_that_does_not_return_is_unreadable() {
    // The Return replaced by five Noops: the method returns nothing.
    let ret = [0xa4, b'R', b'B', b'U', b'F', 0x08, b'_', b'D', b'S', b'D'];
    let mut noops = ret;
    noops[..5].copy_from_slice(&[0xa3; 5]);
    let found = genet(&[&pi4_dsdt_with(&ret, &noops)]);
    assert!(found.is_empty());
    assert_eq!(found.unreadable, 1);
}

#[test]
fn a_local_crs_buffer_is_read_by_its_own_bytes() {
    // The base moved: the registers read are the buffer's, not a constant.
    let mem = [0x86, 0x09, 0x00, 0x01, 0x00, 0x00, 0x58, 0xfd];
    let mut moved = mem;
    moved[6] = 0x59;
    let found = genet(&[&pi4_dsdt_with(&mem, &moved)]);
    assert_eq!(found.as_slice(), &[Transport { base: 0xfd59_0000, ..PI4_GENET }]);
    // A zero length is no register block.
    let len = [0x58, 0xfd, 0x00, 0x00, 0x01, 0x00, 0x89];
    let mut zero = len;
    zero[4] = 0x00;
    let found = genet(&[&pi4_dsdt_with(&len, &zero)]);
    assert!(found.is_empty());
    assert_eq!(found.unreadable, 1);
}

/// The body of a device of `_HID "BCM6E4E"` whose `_CRS` method names
/// `RBUF` inside itself over the Pi's resources and returns it, `extra`
/// after the Return.
fn local_crs_body(extra: &[u8]) -> Vec<u8> {
    local_crs_body_named(&[], extra)
}

/// The same, the buffer named with `prefix` before `RBUF` in both the Name
/// and the Return (`^` names it in the device, outside the method).
fn local_crs_body_named(prefix: &[u8], extra: &[u8]) -> Vec<u8> {
    let rbuf_data = [0x86, 0x09, 0x00, 0x01, 0, 0, 0x58, 0xfd, 0x00, 0x00, 0x01, 0x00, 0x89, 0x06, 0x00, 0x01, 0x01, 0xbd, 0, 0, 0, 0x79, 0x00];
    let mut code = std::vec![0x08, 0x08];
    code.extend_from_slice(prefix);
    code.extend_from_slice(&[b'R', b'B', b'U', b'F', 0x11, (3 + rbuf_data.len()) as u8, 0x0a, rbuf_data.len() as u8]);
    code.extend_from_slice(&rbuf_data);
    code.push(0xa4);
    code.extend_from_slice(prefix);
    code.extend_from_slice(b"RBUF");
    code.extend_from_slice(extra);
    let mut method = std::vec![0x14, (1 + 4 + code.len()) as u8, b'_', b'C', b'R', b'S'];
    method.extend_from_slice(&code);
    [hid(b"BCM6E4E").as_slice(), &method].concat()
}

fn local_crs_device(extra: &[u8]) -> Vec<u8> {
    device(b"ETH0", &local_crs_body(extra))
}

#[test]
fn a_local_crs_method_is_read_exactly() {
    assert_eq!(genet(&[&table(&[&local_crs_device(&[])])]).as_slice(), &[PI4_GENET]);
    // A Noop after the Return: not the shape.
    let found = genet(&[&table(&[&local_crs_device(&[0xa3])])]);
    assert!(found.is_empty());
    assert_eq!(found.unreadable, 1);
}

#[test]
fn a_second_crs_beside_a_local_one_is_unreadable() {
    // A `Name (_CRS, ...)` in the same device as the method: two
    // definitions, neither taken.
    let crs = crs(&[0x86, 0x09, 0x00, 0x01, 0, 0, 0x58, 0xfd, 0x00, 0x00, 0x01, 0x00]);
    let body = [local_crs_body(&[]).as_slice(), crs.as_slice()].concat();
    let found = genet(&[&table(&[&device(b"ETH0", &body)])]);
    assert!(found.is_empty(), "{:?}", found.as_slice());
    assert_eq!(found.unreadable, 1);
    // And with the Name first, where it is the method that is the second.
    let body = [crs.as_slice(), local_crs_body(&[]).as_slice()].concat();
    let found = genet(&[&table(&[&device(b"ETH0", &body)])]);
    assert!(found.is_empty(), "{:?}", found.as_slice());
    assert_eq!(found.unreadable, 1);
}

#[test]
fn a_crs_method_naming_its_buffer_outside_itself_is_unreadable() {
    // `Name (^RBUF, ...)` makes a name in the device, which outlives the
    // method and which anything may store to: not the local shape.
    let found = genet(&[&table(&[&device(b"ETH0", &local_crs_body_named(b"^", &[]))])]);
    assert!(found.is_empty(), "{:?}", found.as_slice());
    assert_eq!(found.unreadable, 1);
}

#[test]
fn a_local_crs_method_ending_in_another_op_is_unreadable() {
    // The Return's one byte made an Increment, the name after it kept:
    // `Name (RBUF, ...); Increment (RBUF)` returns nothing.
    let ret = [0xa4, b'R', b'B', b'U', b'F', 0x08, b'_', b'D', b'S', b'D'];
    let mut inc = ret;
    inc[0] = 0x75;
    let found = genet(&[&pi4_dsdt_with(&ret, &inc)]);
    assert!(found.is_empty(), "{:?}", found.as_slice());
    assert_eq!(found.unreadable, 1);
}

/// The `_CRS` method of the patching shape and the `RBUF` it patches, as
/// `patching_device` holds them, without the device or its `_HID`.
fn patching_crs() -> Vec<u8> {
    let dev = patching_device(&[]);
    let hid = hid(b"BRCME88C");
    let at = dev.windows(hid.len()).position(|w| w == hid.as_slice()).unwrap();
    dev[at + hid.len()..].to_vec()
}

#[test]
fn a_patching_crs_beside_a_local_one_is_unreadable() {
    // Two `_CRS` methods, each of a shape this scan reads: two
    // definitions, neither taken, in either order.
    let local = local_crs_body(&[]);
    let patching = patching_crs();
    for body in [[local.as_slice(), &patching].concat(), [patching.as_slice(), &local].concat()] {
        let found = genet(&[&table(&[&device(b"ETH0", &body)])]);
        assert!(found.is_empty(), "{:?}", found.as_slice());
        assert_eq!(found.unreadable, 1);
    }
    // The control: the patching `_CRS` alone under this `_HID` is read.
    let alone = [hid(b"BCM6E4E").as_slice(), &patching].concat();
    assert_eq!(genet(&[&table(&[&device(b"ETH0", &alone)])]).as_slice(), &[PI4_EMMC2]);
}

#[test]
fn one_is_exactly_one_kept_and_none_dropped() {
    let t = Transport { base: 0x1000, size: 0x100, intid: Some(48) };
    let mut found = Transports::new();
    assert_eq!(found.one(), None);
    found.push(t);
    assert_eq!(found.one(), Some(&t));
    // Declarations not read, absent or of unknown status beside it do not
    // count against the one that is present.
    found.unreadable = 2;
    found.absent = 1;
    found.status_unknown = 1;
    assert_eq!(found.one(), Some(&t));
    // One past the list's room is a second device: which is meant is a guess.
    let mut dropped = found;
    dropped.dropped = 1;
    assert_eq!(dropped.one(), None);
    found.push(Transport { base: 0x2000, ..t });
    assert_eq!(found.one(), None);
}
