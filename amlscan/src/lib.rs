//! The virtio-mmio transports an ACPI table declares, found by a byte scan
//! of its AML. The kernel probes a virtio-mmio address only when the
//! firmware has said a transport is there: until 2026-11-04 it scanned
//! QEMU `virt`'s 32 slots from `0xa000000` whenever a serial console had
//! been found before the exit, and Parallels 27.1, which gave its VM a
//! serial console that day, took a synchronous external abort on the first
//! slot (`docs/testing/testing-parallels.md`, "Parallels 27.1 and the
//! one-disk stick"). A platform that declares no transport is now never
//! scanned.
//!
//! QEMU declares each transport as a `Device` holding `_HID "LNRO0005"` and
//! a `_CRS` buffer of a `Memory32Fixed` descriptor (the registers) and an
//! Extended Interrupt descriptor (the GIC INTID), as `hw/arm/virt-acpi-build.c`
//! writes them and its reference DSDT (`tests/data/acpi/aarch64/virt/DSDT`)
//! shows:
//!
//! ```text
//! 5B 82 41 04 "VR00"                        Device (VR00), PkgLength 0x41
//!   08 "_HID" 0D "LNRO0005" 00              Name (_HID, "LNRO0005")
//!   08 "_UID" 00                            Name (_UID, Zero)
//!   08 "_CCA" 01                            Name (_CCA, One)
//!   08 "_CRS" 11 1A 0A 17                   Name (_CRS, Buffer (0x17) {
//!     86 09 00 01 00000A00 00020000           Memory32Fixed (ReadWrite, 0x0A000000, 0x200)
//!     89 06 00 01 01 30000000                 Interrupt (..., Exclusive) { 0x30 }
//!     79 00                                   EndTag })
//! ```
//!
//! **A declaration is only one if `_STA` says the device is there.**
//! Parallels 27.1's DSDT declares a virtio-mmio GPU (`_HID "LNRO0005"` at
//! `0x2180000`) whose `_STA` is a method reading a hardware field; in a VM
//! without that GPU it answers "not present", and a read of its registers
//! is a synchronous external abort (seen 2026-11-04). ACPI says a device
//! with no `_STA` is present and enabled, which is QEMU's case. So a device
//! is taken when it has no `_STA`, or a `Name (_STA, value)` with both the
//! present and enabled bits set; a `_STA` this scan cannot evaluate, a
//! method, is counted in [`Transports::status_unknown`] and not taken.
//!
//! **A walk of one level, not an interpreter.** The `_HID` is found by its
//! fixed encoding; then the `Device` that holds it, the nearest `DeviceOp`
//! before it whose package reaches past it, has its own objects walked in
//! order, as a parser would at that level: each `Name` by its value's
//! encoding (an integer, a string, a buffer, a package), and every op that
//! carries a PkgLength (a `Method`, a child `Device`, `PowerResource`,
//! `Processor` or `ThermalZone`, a `Scope`, `If`, `Else`, `While`, a
//! `Field`) skipped whole. So bytes inside a value or a child are never read
//! as the device's own objects: the `_HID`, `_STA` and `_CRS` used are the
//! device's. Until the second review of #407 this was a byte search for each
//! name, and a `5B 82` inside a constant, skipped as a child, hid the
//! device's own `_STA`, which then read as present.
//!
//! What it cannot see, stated because the answer then is "no transport",
//! which is the safe one: an op the walk does not know (an `OperationRegion`
//! among them) ends the walk, and a device whose walk ends before its end
//! with no `_STA` seen counts its status unknown; a `_HID` given as an EISA
//! id, or a device naming `LNRO0005` only as its `_CID` (Parallels'
//! balloon and NICs), is not taken; a `_CRS` built by a method, a 64-bit or
//! address-space memory descriptor, any `_STA` but a constant. Each
//! transport found is still checked by the kernel against the virtio-mmio
//! magic value before it is used.
//!
//! Every length read from the table is bounded: a table only being read
//! must never panic the kernel. Pure: no I/O, no heap, so it builds and is
//! tested on the host (`make test`).

#![no_std]

/// The most transports kept, QEMU `virt`'s count. Any past it are counted
/// in [`Transports::dropped`], not kept.
pub const MAX_TRANSPORTS: usize = 32;

/// The `_HID` virtio-mmio is declared by (Linaro's id, which Linux's
/// `virtio_mmio` driver matches).
pub const VIRTIO_MMIO_HID: &[u8; 8] = b"LNRO0005";

/// One declared transport.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Transport {
    /// The register block's physical address.
    pub base: u64,
    /// Its length in bytes.
    pub size: u32,
    /// The GIC INTID its interrupt is wired to, when the `_CRS` names one.
    pub intid: Option<u32>,
}

/// The transports found, in table order.
#[derive(Debug, Clone, Copy)]
pub struct Transports {
    list: [Transport; MAX_TRANSPORTS],
    count: usize,
    /// Declared past [`MAX_TRANSPORTS`], so not kept.
    pub dropped: usize,
    /// `LNRO0005` devices whose `_CRS` this scan could not read: declared,
    /// so worth a line in the boot log, but never probed.
    pub unreadable: usize,
    /// `LNRO0005` devices whose `Name (_STA, ...)` says not present or not
    /// enabled: not probed.
    pub absent: usize,
    /// `LNRO0005` devices with a `_STA` this scan cannot evaluate (a
    /// method): not probed, since only running it says whether the device
    /// is there.
    pub status_unknown: usize,
}

impl Default for Transports {
    fn default() -> Self {
        Self::new()
    }
}

impl Transports {
    pub const fn new() -> Self {
        Self {
            list: [Transport { base: 0, size: 0, intid: None }; MAX_TRANSPORTS],
            count: 0,
            dropped: 0,
            unreadable: 0,
            absent: 0,
            status_unknown: 0,
        }
    }

    /// The transports kept.
    pub fn as_slice(&self) -> &[Transport] {
        &self.list[..self.count]
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    fn push(&mut self, t: Transport) {
        if self.list[..self.count].contains(&t) {
            return;
        }
        if self.count < MAX_TRANSPORTS {
            self.list[self.count] = t;
            self.count += 1;
        } else {
            self.dropped += 1;
        }
    }
}

/// The size of an ACPI table header: AML starts after it.
pub const HEADER_LEN: usize = 36;

const DEVICE_OP: [u8; 2] = [0x5b, 0x82];
const NAME_HID: [u8; 6] = [0x08, b'_', b'H', b'I', b'D', 0x0d];

/// Adds to `into` every virtio-mmio transport the AML table `table` (a
/// DSDT or SSDT, its header included) declares. Called once a table, so
/// the DSDT and every SSDT add to one list.
pub fn virtio_mmio(table: &[u8], into: &mut Transports) {
    if table.len() <= HEADER_LEN {
        return;
    }
    let mut at = HEADER_LEN;
    while let Some(found) = find(table, at, table.len(), &NAME_HID) {
        let name = found + NAME_HID.len();
        at = name;
        // The string, then its NUL: "LNRO0005" and not a longer id that
        // starts with it.
        if table.get(name..name + 9).is_none_or(|s| &s[..8] != VIRTIO_MMIO_HID || s[8] != 0) {
            continue;
        }
        let Some((start, end)) = enclosing_device(table, found) else {
            into.unreadable += 1;
            continue;
        };
        let body = walk_device(table, start, end);
        // The `_HID` must be one of the device's own objects, seen by the
        // walk, not bytes inside one of them.
        if body.hid_at != Some(found) {
            into.unreadable += 1;
            continue;
        }
        match body.status() {
            Status::Present => {}
            Status::Absent => {
                into.absent += 1;
                continue;
            }
            Status::Unknown => {
                into.status_unknown += 1;
                continue;
            }
        }
        match body.crs_buffer.and_then(|buf| resources(table, buf + 1, end)) {
            Some(t) => into.push(t),
            None => into.unreadable += 1,
        }
    }
}

/// What a device's `_STA` says.
#[derive(Debug, PartialEq, Eq)]
enum Status {
    /// No `_STA` in a device walked whole, or a constant with the present
    /// and enabled bits.
    Present,
    /// A constant `_STA` without both bits.
    Absent,
    /// A `_STA` method or a value that is not a constant, or a device the
    /// walk could not finish before seeing one.
    Unknown,
}

/// A device's own objects, as [`walk_device`] found them.
#[derive(Default)]
struct Body {
    /// Where its `Name (_HID, "LNRO0005")` starts.
    hid_at: Option<usize>,
    /// `Some(Some(v))` for `Name (_STA, constant v)`, `Some(None)` for a
    /// `_STA` method or any other value.
    sta: Option<Option<u64>>,
    /// Where its `Name (_CRS, Buffer ...)`'s BufferOp is.
    crs_buffer: Option<usize>,
    /// Whether the walk reached the device's end.
    complete: bool,
}

impl Body {
    fn status(&self) -> Status {
        match self.sta {
            // Bit 0 present, bit 1 enabled (ACPI 6.5, 6.3.7).
            Some(Some(v)) if v & 0x3 == 0x3 => Status::Present,
            Some(Some(_)) => Status::Absent,
            Some(None) => Status::Unknown,
            None if self.complete => Status::Present,
            None => Status::Unknown,
        }
    }
}

/// A NameString's length at `at`, and its single NameSeg when it is one
/// (no root, parent or multi-segment prefix).
fn name_string(table: &[u8], at: usize) -> Option<(usize, Option<[u8; 4]>)> {
    let mut p = at;
    while matches!(table.get(p), Some(b'\\' | b'^')) {
        p += 1;
    }
    let segs = match *table.get(p)? {
        0x00 => return Some((p + 1 - at, None)),
        0x2e => {
            p += 1;
            2
        }
        0x2f => {
            p += 2;
            usize::from(*table.get(p - 1)?)
        }
        b'A'..=b'Z' | b'_' => 1,
        _ => return None,
    };
    let len = segs.checked_mul(4)?;
    let seg = table.get(p..p.checked_add(len)?)?;
    let single = (segs == 1 && p == at).then(|| [seg[0], seg[1], seg[2], seg[3]]);
    Some((p + len - at, single))
}

/// A Name's value.
enum Data {
    Int(u64),
    Str(usize, usize),
    Buffer,
    Other,
}

/// The DataRefObject at `at`: its kind, and where the next object starts.
/// `None` for any encoding this walk does not take apart.
fn data(table: &[u8], at: usize) -> Option<(Data, usize)> {
    let le = |n: usize| -> Option<u64> {
        let b = table.get(at + 1..at + 1 + n)?;
        Some(b.iter().rev().fold(0u64, |v, &x| (v << 8) | u64::from(x)))
    };
    Some(match *table.get(at)? {
        0x00 => (Data::Int(0), at + 1),
        0x01 => (Data::Int(1), at + 1),
        0xff => (Data::Int(u64::MAX), at + 1),
        0x0a => (Data::Int(le(1)?), at + 2),
        0x0b => (Data::Int(le(2)?), at + 3),
        0x0c => (Data::Int(le(4)?), at + 5),
        0x0e => (Data::Int(le(8)?), at + 9),
        0x0d => {
            let nul = at + 1 + table.get(at + 1..)?.iter().position(|&b| b == 0)?;
            (Data::Str(at + 1, nul), nul + 1)
        }
        0x11 => (Data::Buffer, at + 1 + pkg_length(table, at + 1)?.0),
        0x12 | 0x13 => (Data::Other, at + 1 + pkg_length(table, at + 1)?.0),
        _ => return None,
    })
}

/// Walks the device `table[start..end]`'s own objects in order, the way an
/// AML parser would at that level: a `Name` by its value's encoding, and
/// every op with a PkgLength (a `Method`, a child `Device`, `Processor`,
/// `PowerResource` or `ThermalZone`, a `Scope`, `If`, `Else`, `While`, a
/// `Field`) skipped whole, so nothing inside one of them is read as the
/// device's (the second review of #407: a byte search took data bytes for a
/// child and hid the device's own `_STA`). An op it does not know ends the
/// walk with [`Body::complete`] false.
fn walk_device(table: &[u8], start: usize, end: usize) -> Body {
    let mut body = Body::default();
    let Some((_, pkg_bytes)) = pkg_length(table, start + DEVICE_OP.len()) else { return body };
    let Some((name_len, _)) = name_string(table, start + DEVICE_OP.len() + pkg_bytes) else { return body };
    let mut p = start + DEVICE_OP.len() + pkg_bytes + name_len;
    // Skips an op whose PkgLength starts at `p + op_len`.
    let skip = |p: usize, op_len: usize| pkg_length(table, p + op_len).and_then(|(len, _)| (p + op_len).checked_add(len));
    while p < end {
        let next = match table[p] {
            // NameOp: the name, then its value.
            0x08 => (|| {
                let (len, seg) = name_string(table, p + 1)?;
                let value = p + 1 + len;
                let (d, next) = data(table, value)?;
                match (seg.as_ref(), &d) {
                    (Some(b"_HID"), Data::Str(a, b)) if &table[*a..*b] == VIRTIO_MMIO_HID => body.hid_at = Some(p),
                    (Some(b"_STA"), Data::Int(v)) => body.sta = Some(Some(*v)),
                    (Some(b"_STA"), _) => body.sta = Some(None),
                    (Some(b"_CRS"), Data::Buffer) => body.crs_buffer = Some(value),
                    _ => {}
                }
                Some(next)
            })(),
            // MethodOp: its name is after its PkgLength.
            0x14 => (|| {
                let (len, bytes) = pkg_length(table, p + 1)?;
                if let Some((_, Some(seg))) = name_string(table, p + 1 + bytes) {
                    if &seg == b"_STA" {
                        body.sta = Some(None);
                    }
                }
                (p + 1).checked_add(len)
            })(),
            // ScopeOp, IfOp, ElseOp, WhileOp.
            0x10 | 0xa0 | 0xa1 | 0xa2 => skip(p, 1),
            // NoopOp.
            0xa3 => Some(p + 1),
            0x5b => match table.get(p + 1) {
                // Field, Device, Processor, PowerResource, ThermalZone,
                // IndexField, BankField.
                Some(0x81..=0x87) => skip(p, 2),
                // Mutex: a name and a SyncFlags byte. Event: a name.
                Some(0x01) => name_string(table, p + 2).map(|(len, _)| p + 2 + len + 1),
                Some(0x02) => name_string(table, p + 2).map(|(len, _)| p + 2 + len),
                _ => None,
            },
            _ => None,
        };
        match next {
            Some(n) if n > p && n <= end => p = n,
            _ => return body,
        }
    }
    body.complete = p == end;
    body
}

/// The first offset of `pat` in `table[from..to]`.
fn find(table: &[u8], from: usize, to: usize, pat: &[u8]) -> Option<usize> {
    let to = to.min(table.len());
    if from >= to || to - from < pat.len() {
        return None;
    }
    table[from..to].windows(pat.len()).position(|w| w == pat).map(|p| from + p)
}

/// An AML PkgLength at `at`: (its value, the bytes it takes). The value
/// counts from `at` itself.
fn pkg_length(table: &[u8], at: usize) -> Option<(usize, usize)> {
    let lead = *table.get(at)?;
    let follow = usize::from(lead >> 6);
    if follow == 0 {
        return Some((usize::from(lead & 0x3f), 1));
    }
    let mut value = usize::from(lead & 0x0f);
    for i in 0..follow {
        value |= usize::from(*table.get(at + 1 + i)?) << (4 + 8 * i);
    }
    Some((value, 1 + follow))
}

/// The byte range of the innermost `Device` whose package holds `inside`:
/// the nearest `DeviceOp` before it whose length reaches past it.
fn enclosing_device(table: &[u8], inside: usize) -> Option<(usize, usize)> {
    let mut at = inside;
    while at > HEADER_LEN {
        at -= 1;
        if table[at..].starts_with(&DEVICE_OP) {
            let pkg = at + DEVICE_OP.len();
            if let Some((len, _)) = pkg_length(table, pkg) {
                let end = pkg.checked_add(len)?;
                if end > inside && end <= table.len() {
                    return Some((at, end));
                }
            }
        }
    }
    None
}

/// The transport a `_CRS` buffer describes, the buffer's PkgLength at
/// `at`, ending by `end`: its `Memory32Fixed` and the first INTID of its
/// Extended Interrupt. `None` when it has no `Memory32Fixed`, or when a
/// length points past the buffer.
fn resources(table: &[u8], at: usize, end: usize) -> Option<Transport> {
    let (pkg, pkg_bytes) = pkg_length(table, at)?;
    let buf_end = at.checked_add(pkg)?.min(end);
    let mut p = at + pkg_bytes;
    // BufferSize: ByteConst, WordConst or DWordConst.
    let size = match *table.get(p)? {
        0x0a => {
            p += 2;
            usize::from(*table.get(p - 1)?)
        }
        0x0b => {
            p += 3;
            usize::from(u16::from_le_bytes(table.get(p - 2..p)?.try_into().ok()?))
        }
        0x0c => {
            p += 5;
            u32::from_le_bytes(table.get(p - 4..p)?.try_into().ok()?) as usize
        }
        _ => return None,
    };
    let data_end = p.checked_add(size)?.min(buf_end);
    let mut memory = None;
    let mut intid = None;
    while p < data_end {
        let tag = table[p];
        if tag & 0x80 == 0 {
            // A small item: the end tag stops the walk.
            if tag & 0x78 == 0x78 {
                break;
            }
            p += 1 + usize::from(tag & 0x07);
            continue;
        }
        let len = usize::from(u16::from_le_bytes(table.get(p + 1..p + 3)?.try_into().ok()?));
        let body = p + 3;
        let next = body.checked_add(len)?;
        if next > data_end {
            return None;
        }
        match tag {
            // Memory32Fixed: information byte, base, length.
            0x86 if len >= 9 => {
                let base = u32::from_le_bytes(table[body + 1..body + 5].try_into().ok()?);
                let size = u32::from_le_bytes(table[body + 5..body + 9].try_into().ok()?);
                memory.get_or_insert((u64::from(base), size));
            }
            // Extended Interrupt: flags, count, then the INTIDs.
            0x89 if len >= 6 && table[body + 1] >= 1 => {
                intid.get_or_insert(u32::from_le_bytes(table[body + 2..body + 6].try_into().ok()?));
            }
            _ => {}
        }
        p = next;
    }
    let (base, size) = memory?;
    (size != 0).then_some(Transport { base, size, intid })
}

#[cfg(test)]
mod tests;
