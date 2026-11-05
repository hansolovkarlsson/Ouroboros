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
    /// `LNRO0005` `_HID`s this scan could not read a device for: no
    /// enclosing `Device`, a `_HID` that is not one of its own objects, or
    /// no `_CRS` buffer it can read. Never probed.
    pub unreadable: usize,
    /// `LNRO0005` devices whose `Name (_STA, ...)` says not present or not
    /// enabled: not probed.
    pub absent: usize,
    /// `LNRO0005` devices with a `_STA` this scan cannot evaluate (a
    /// method, or a value that is not a constant), or whose walk ended
    /// before any `_STA` was seen: not probed, since only an interpreter
    /// could say whether the device is there.
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

/// The most AML tables taken in one scan (the DSDT and the SSDTs).
pub const MAX_TABLES: usize = 32;

/// The deepest name path followed; a deeper one makes the scan uncertain.
const MAX_DEPTH: usize = 8;

/// The most `LNRO0005` devices tracked; past it they are counted as
/// dropped.
const MAX_CANDIDATES: usize = 2 * MAX_TRANSPORTS;

/// Adds to `into` every virtio-mmio transport the AML tables `tables` (the
/// DSDT and every SSDT, each with its header) declare present. One call
/// for all of them: a device's `_STA` or `_CRS` may be defined in another
/// table than the device, by a `Scope` of its path (the third review of
/// #407).
pub fn virtio_mmio(tables: &[&[u8]], into: &mut Transports) {
    // Pass 1: the path of every Device with `Name (_HID, "LNRO0005")`.
    let mut find = FindHids { paths: [Path::ROOT; MAX_CANDIDATES], count: 0, dropped: 0 };
    let mut uncertain = false;
    for t in tables {
        walk_table(t, &mut find, &mut uncertain);
    }
    into.dropped += find.dropped;
    // Pass 2: everything defined at each of those paths, in any table.
    let mut facts = [Facts::default(); MAX_CANDIDATES];
    let mut gather = Gather { paths: &find.paths[..find.count], facts: &mut facts[..find.count] };
    for t in tables {
        walk_table(t, &mut gather, &mut uncertain);
    }
    for f in &facts[..find.count] {
        if !f.device || f.crs_bad || f.hids != 1 {
            into.unreadable += 1;
            continue;
        }
        let status = match f.sta {
            Sta::None if uncertain => Status::Unknown,
            Sta::None => Status::Present,
            // Bit 0 present, bit 1 enabled (ACPI 6.5, 6.3.7).
            Sta::Value(v) if v & 0x3 == 0x3 => Status::Present,
            Sta::Value(_) => Status::Absent,
            Sta::Unknown => Status::Unknown,
        };
        match status {
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
        match f.crs {
            Some(t) => into.push(t),
            None => into.unreadable += 1,
        }
    }
}

/// What a device's `_STA` says.
#[derive(Debug, PartialEq, Eq)]
enum Status {
    Present,
    Absent,
    Unknown,
}

/// An absolute name path, NameSeg by NameSeg. Equal by the segments in
/// use only: `segs` past `len` may hold a segment [`Path::split_last`] left.
#[derive(Clone, Copy)]
struct Path {
    segs: [[u8; 4]; MAX_DEPTH],
    len: usize,
}

impl PartialEq for Path {
    fn eq(&self, other: &Path) -> bool {
        self.segs[..self.len] == other.segs[..other.len]
    }
}

impl Eq for Path {}

impl Path {
    const ROOT: Path = Path { segs: [[0; 4]; MAX_DEPTH], len: 0 };

    /// The parent of this path, and its last segment.
    fn split_last(&self) -> Option<(Path, [u8; 4])> {
        let last = *self.segs.get(self.len.checked_sub(1)?)?;
        let mut parent = *self;
        parent.len -= 1;
        Some((parent, last))
    }
}

/// A NameString at `at` resolved against `scope`: the absolute path it
/// names, and its length in bytes. `None` for an encoding this scan does
/// not read, or a path deeper than [`MAX_DEPTH`].
fn name_string(table: &[u8], at: usize, scope: &Path) -> Option<(Path, usize)> {
    let mut p = at;
    let mut path = *scope;
    if table.get(p) == Some(&b'\\') {
        path = Path::ROOT;
        p += 1;
    }
    while table.get(p) == Some(&b'^') {
        path.len = path.len.checked_sub(1)?;
        p += 1;
    }
    let segs = match *table.get(p)? {
        0x00 => return Some((path, p + 1 - at)),
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
    for _ in 0..segs {
        let seg = table.get(p..p + 4)?;
        if path.len == MAX_DEPTH || !seg.iter().all(|&c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == b'_') {
            return None;
        }
        path.segs[path.len] = [seg[0], seg[1], seg[2], seg[3]];
        path.len += 1;
        p += 4;
    }
    Some((path, p - at))
}

/// A Name's value.
enum Data {
    Int(u64),
    Str(usize, usize),
    /// A Buffer: where its BufferOp is.
    Buffer(usize),
    Other,
}

/// The DataRefObject at `at`: its kind, and where the next object starts.
/// `None` for any encoding this scan does not take apart.
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
        0x11 => (Data::Buffer(at), (at + 1).checked_add(pkg_length(table, at + 1)?.0)?),
        0x12 | 0x13 => (Data::Other, (at + 1).checked_add(pkg_length(table, at + 1)?.0)?),
        _ => return None,
    })
}

/// What a walk reports, object by object, at their absolute paths.
trait Visit {
    fn device(&mut self, _path: &Path) {}
    fn name(&mut self, _table: &[u8], _path: &Path, _value: &Data) {}
    /// A Method, an Alias or anything else that defines `path` with a
    /// value this scan cannot know.
    fn opaque(&mut self, _path: &Path) {}
}

/// Walks the table `table`'s whole TermList from its top level.
fn walk_table<V: Visit>(table: &[u8], v: &mut V, uncertain: &mut bool) {
    if table.len() <= HEADER_LEN {
        return;
    }
    if !term_list(table, HEADER_LEN, table.len(), &Path::ROOT, v) {
        *uncertain = true;
    }
}

/// Walks the objects of `table[p..end]` in the scope `scope`, as an AML
/// parser would: a `Name` by its value's encoding, a `Scope`, `Device`,
/// `Processor`, `PowerResource` or `ThermalZone` by recursing into its
/// body at its own path, a `Method` or a `Field` skipped whole by its
/// PkgLength. `false` when it met something it cannot see through: an
/// `If`, `Else` or `While` outside a method (which may define any name), an
/// op it does not know, or a length past its end; the rest of the scope is
/// then unseen, so the scan as a whole is uncertain.
fn term_list<V: Visit>(table: &[u8], mut p: usize, end: usize, scope: &Path, v: &mut V) -> bool {
    // An op's PkgLength at `at`: (where its package ends, where the
    // PkgLength ends). `None` when the package runs past `end`.
    let pkg = |at: usize| -> Option<(usize, usize)> {
        let (len, bytes) = pkg_length(table, at)?;
        let pkg_end = at.checked_add(len)?;
        (pkg_end <= end && len >= bytes).then_some((pkg_end, at + bytes))
    };
    while p < end {
        let next = match (table[p], table.get(p + 1).copied()) {
            // NameOp: the name, then its value.
            (0x08, _) => (|| {
                let (path, len) = name_string(table, p + 1, scope)?;
                let (value, next) = data(table, p + 1 + len)?;
                v.name(table, &path, &value);
                Some(next)
            })(),
            // MethodOp: skipped whole; what it defines is unknowable here.
            (0x14, _) => (|| {
                let (pkg_end, body) = pkg(p + 1)?;
                let (path, _) = name_string(table, body, scope)?;
                v.opaque(&path);
                Some(pkg_end)
            })(),
            // ScopeOp, and the ExtOp scopes with a fixed header after
            // their name: Device (none), Processor (6 bytes), PowerResource
            // (3 bytes), ThermalZone (none).
            (0x10, _) | (0x5b, Some(0x82..=0x85)) => (|| {
                let ext = usize::from(table[p] == 0x5b);
                let (pkg_end, body) = pkg(p + 1 + ext)?;
                let (path, len) = name_string(table, body, scope)?;
                let header = match table.get(p + 1).copied() {
                    Some(0x83) if ext == 1 => 6,
                    Some(0x84) if ext == 1 => 3,
                    _ => 0,
                };
                if ext == 1 && table[p + 1] == 0x82 {
                    v.device(&path);
                }
                let start = body + len + header;
                (start <= pkg_end && term_list(table, start, pkg_end, &path, v)).then_some(pkg_end)
            })(),
            // ExternalOp: a name, an ObjectType and an ArgumentCount.
            (0x15, _) => name_string(table, p + 1, scope).map(|(_, len)| p + 1 + len + 2),
            // AliasOp: the new name is defined to something unknowable.
            (0x06, _) => (|| {
                let (_, len) = name_string(table, p + 1, scope)?;
                let (alias, len2) = name_string(table, p + 1 + len, scope)?;
                v.opaque(&alias);
                Some(p + 1 + len + len2)
            })(),
            // NoopOp.
            (0xa3, _) => Some(p + 1),
            // Field, IndexField, BankField: skipped whole.
            (0x5b, Some(0x81 | 0x86 | 0x87)) => pkg(p + 2).map(|(pkg_end, _)| pkg_end),
            // OperationRegion: a name, a RegionSpace byte, and an offset
            // and a length taken only as integer constants.
            (0x5b, Some(0x80)) => (|| {
                let (path, len) = name_string(table, p + 2, scope)?;
                v.opaque(&path);
                let (Data::Int(_), q) = data(table, p + 2 + len + 1)? else { return None };
                let (Data::Int(_), q) = data(table, q)? else { return None };
                Some(q)
            })(),
            // Mutex (a name and a SyncFlags byte), Event (a name).
            (0x5b, Some(0x01)) => name_string(table, p + 2, scope).map(|(_, len)| p + 2 + len + 1),
            (0x5b, Some(0x02)) => name_string(table, p + 2, scope).map(|(_, len)| p + 2 + len),
            // If, Else, While, and anything else: not seen through.
            _ => None,
        };
        match next {
            Some(n) if n > p && n <= end => p = n,
            _ => return false,
        }
    }
    true
}

/// Pass 1: every device path given `_HID "LNRO0005"`.
struct FindHids {
    paths: [Path; MAX_CANDIDATES],
    count: usize,
    dropped: usize,
}

impl Visit for FindHids {
    fn name(&mut self, table: &[u8], path: &Path, value: &Data) {
        let (Some((parent, b"_HID")), Data::Str(a, b)) = (path.split_last().as_ref().map(|(p, s)| (*p, s)), value) else {
            return;
        };
        if &table[*a..*b] != VIRTIO_MMIO_HID || self.paths[..self.count].contains(&parent) {
            return;
        }
        if self.count < MAX_CANDIDATES {
            self.paths[self.count] = parent;
            self.count += 1;
        } else {
            self.dropped += 1;
        }
    }
}

/// A `_STA` as the tables define it.
#[derive(Clone, Copy, Default)]
enum Sta {
    #[default]
    None,
    Value(u64),
    /// A method, a value that is not a constant, or two definitions.
    Unknown,
}

/// Everything pass 2 found defined at one device path.
#[derive(Clone, Copy, Default)]
struct Facts {
    device: bool,
    hids: u8,
    sta: Sta,
    crs: Option<Transport>,
    /// A `_CRS` this scan cannot read, or two of them.
    crs_bad: bool,
}

/// Pass 2: the facts at each path pass 1 found.
struct Gather<'a> {
    paths: &'a [Path],
    facts: &'a mut [Facts],
}

impl Gather<'_> {
    fn at(&mut self, path: &Path) -> Option<(&mut Facts, [u8; 4])> {
        let (parent, seg) = path.split_last()?;
        let i = self.paths.iter().position(|p| *p == parent)?;
        Some((&mut self.facts[i], seg))
    }
}

impl Visit for Gather<'_> {
    fn device(&mut self, path: &Path) {
        if let Some(i) = self.paths.iter().position(|p| p == path) {
            self.facts[i].device = true;
        }
    }

    fn name(&mut self, table: &[u8], path: &Path, value: &Data) {
        let Some((f, seg)) = self.at(path) else { return };
        match &seg {
            b"_HID" => f.hids = f.hids.saturating_add(1),
            b"_STA" => {
                f.sta = match (f.sta, value) {
                    (Sta::None, Data::Int(v)) => Sta::Value(*v),
                    _ => Sta::Unknown,
                }
            }
            b"_CRS" => match (f.crs.is_some() || f.crs_bad, value) {
                (false, Data::Buffer(at)) => match resources(table, at + 1, table.len()) {
                    Some(t) => f.crs = Some(t),
                    None => f.crs_bad = true,
                },
                _ => {
                    f.crs = None;
                    f.crs_bad = true;
                }
            },
            _ => {}
        }
    }

    fn opaque(&mut self, path: &Path) {
        let Some((f, seg)) = self.at(path) else { return };
        match &seg {
            b"_STA" => f.sta = Sta::Unknown,
            b"_CRS" => {
                f.crs = None;
                f.crs_bad = true;
            }
            b"_HID" => f.hids = f.hids.saturating_add(1),
            _ => {}
        }
    }
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
