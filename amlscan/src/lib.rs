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
//! **A scan, not an interpreter.** It finds the `_HID` by its fixed encoding,
//! bounds the search for the `_CRS` and `_STA` by the package length of the
//! `Device` that holds it, and reads only those two descriptor kinds. What
//! it cannot see, stated because the answer then is "no transport", which
//! is the safe one: a `_HID` given as an EISA id or returned by a method (a
//! device naming `LNRO0005` only as its `_CID`, as Parallels' balloon and
//! NICs do, is not taken), a `_CRS` built by a method, a 64-bit or
//! address-space memory descriptor, any `_STA` but a plain `Name`. A child
//! `Device` inside the declaring one is skipped whole by both searches, so
//! a child's `_CRS` or `_STA` is never read as the declaring device's (the
//! review of #407); bytes in a buffer that happen to look like a child
//! `Device` can only hide the device's own objects, which keeps it out.
//! Each transport found is still checked by the kernel against the
//! virtio-mmio magic value before it is used.
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
const NAME_CRS: [u8; 6] = [0x08, b'_', b'C', b'R', b'S', 0x11];
const STA: [u8; 4] = *b"_STA";

/// What a device's `_STA` says.
#[derive(Debug, PartialEq, Eq)]
enum Status {
    /// No `_STA`, or one whose value has the present and enabled bits.
    Present,
    /// A `Name (_STA, value)` without both bits.
    Absent,
    /// A `_STA` that is not a plain `Name` with a constant.
    Unknown,
}

/// The device `table[start..end]`'s own `_STA`, by the rule in the module
/// doc.
fn status(table: &[u8], start: usize, end: usize) -> Status {
    let Some(at) = find_own(table, start, end, &STA) else { return Status::Present };
    // `Name (_STA, constant)`: NameOp right before the name. A 0x08 there
    // may instead be the last byte of a `Method (_STA)`'s PkgLength (`14 08
    // _STA ...` is an 8-byte method), so it counts as a NameOp only when no
    // MethodOp's PkgLength ends there (the review of #407).
    if at == 0 || table[at - 1] != 0x08 || method_pkg_ends_at(table, at) {
        return Status::Unknown;
    }
    let v = at + STA.len();
    let value = match table.get(v) {
        Some(0x00) => Some(0),
        Some(0x01) => Some(1),
        Some(0xff) => Some(u32::MAX),
        Some(0x0a) => table.get(v + 1).map(|&b| u32::from(b)),
        Some(0x0b) => table.get(v + 1..v + 3).map(|b| u32::from(u16::from_le_bytes([b[0], b[1]]))),
        Some(0x0c) => table.get(v + 1..v + 5).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]])),
        _ => None,
    };
    match value {
        // Bit 0 present, bit 1 enabled (ACPI 6.5, 6.3.7).
        Some(x) if x & 0x3 == 0x3 => Status::Present,
        Some(_) => Status::Absent,
        None => Status::Unknown,
    }
}

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
        match status(table, start, end) {
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
        match find_own(table, start, end, &NAME_CRS).and_then(|crs| resources(table, crs + NAME_CRS.len(), end)) {
            Some(t) => into.push(t),
            None => into.unreadable += 1,
        }
    }
}

/// Whether a `MethodOp` (0x14) and a PkgLength of 1 to 4 bytes end just
/// before `at`.
fn method_pkg_ends_at(table: &[u8], at: usize) -> bool {
    (1..=4).any(|n| {
        at > n && table[at - n - 1] == 0x14 && pkg_length(table, at - n).is_some_and(|(_, bytes)| bytes == n)
    })
}

/// The first offset of `pat` among the device `table[start..end]`'s own
/// objects: after its header (DeviceOp, PkgLength, name), and skipping
/// every `Device` nested inside it whole.
fn find_own(table: &[u8], start: usize, end: usize, pat: &[u8]) -> Option<usize> {
    let (_, pkg_bytes) = pkg_length(table, start + DEVICE_OP.len())?;
    let mut p = start + DEVICE_OP.len() + pkg_bytes + 4;
    while p + pat.len() <= end {
        if table[p..].starts_with(&DEVICE_OP) {
            if let Some((len, _)) = pkg_length(table, p + DEVICE_OP.len()) {
                let child_end = p + DEVICE_OP.len() + len;
                if child_end <= end && len > 0 {
                    p = child_end;
                    continue;
                }
            }
        }
        if table[p..].starts_with(pat) {
            return Some(p);
        }
        p += 1;
    }
    None
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
