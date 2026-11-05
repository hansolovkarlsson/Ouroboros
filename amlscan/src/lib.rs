//! The virtio-mmio transports the ACPI tables declare present, found by
//! walking their AML namespace. The kernel probes a virtio-mmio address only when the
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
//! is taken only from tables walked whole, and then when it has no `_STA`
//! at all, or a constant `_STA` with both the present and enabled bits set
//! that no method body anywhere names (a `Name` can be stored to, by
//! `\_SB._INI` before any `_STA` is read, say: the fourth review of #407);
//! a `_STA` this scan cannot evaluate, a method, is counted in
//! [`Transports::status_unknown`] and not taken.
//!
//! **A walk of the namespace, not an interpreter.** A `_STA` need not be
//! inside its device: `Scope (\_SB.VR00) { Name (_STA, Zero) }` later in the
//! DSDT, or in an SSDT, or `Name (\_SB.VR00._STA, Zero)` anywhere, defines it
//! too (the third review of #407). So every table is walked from its top
//! level, all of them in one [`virtio_mmio`] call, as a parser would load
//! them: a `Scope`, `Device`, `Processor`, `PowerResource` or `ThermalZone`
//! by recursing into its body at its own path, each name resolved to an
//! absolute path (`\`, `^` and multi-segment names included), each `Name`
//! by its value's encoding, a `Method` skipped whole, each unit of a
//! `Field`, `IndexField` or `BankField` taken as a name. Pass 1
//! finds the path of every device given `Name (_HID, "LNRO0005")`; pass 2
//! gathers every `_STA` and `_CRS` defined at those paths, wherever they
//! are. Bytes inside a value, a method or another device are never read as
//! a name. Until the reviews of #407 this was a search of one device's
//! bytes, and each review found a way for it to read an absent device as
//! present.
//!
//! **What makes the scan uncertain, and what that costs.** Anything the
//! walk cannot see through means the rest of that scope is unseen, and it
//! could define any name or store to one: an `If`, `Else` or `While`
//! outside a method (executed at load), an op the walk does not know (an
//! `OperationRegion` whose bounds are not constants, a `Store`), a length
//! running past its parent, a path deeper than 8 segments, scopes nested
//! more than 32 deep (bounded on its own, since a name from the root keeps
//! the path short however deep, and the recursion must not run the
//! kernel's stack out), a method whose body holds `Load` or `LoadTable`.
//! Where the thing has a PkgLength the walk goes on after it, so what
//! follows is still found and counted. Then no device counts as present,
//! a constant `_STA` included. QEMU's DSDT is walked whole (the test of its
//! reference copy says so), and none of its methods names `_STA`.
//!
//! Not taken, stated because the answer then is "no transport", which is
//! the safe one: a `_HID` given as an EISA id, or a device naming `LNRO0005`
//! only as its `_CID` (Parallels' balloon and NICs); a `_STA`, `_HID` or
//! `_CRS` defined twice or by a method or an `Alias`; a `_CRS` with no
//! `Memory32Fixed`, or only 64-bit or address-space descriptors; a `_HID`
//! named at a path no `Device` defines. Each transport found is still
//! checked by the kernel against the virtio-mmio magic value before it is
//! used.
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
    /// `LNRO0005` declarations this scan could not read as one device: a
    /// path no `Device` defines, a second `_HID`, or no single `_CRS`
    /// buffer it can read. Never probed.
    pub unreadable: usize,
    /// `LNRO0005` devices whose `Name (_STA, ...)` says not present or not
    /// enabled: not probed.
    pub absent: usize,
    /// `LNRO0005` devices with a `_STA` this scan cannot evaluate (a
    /// method, a value that is not a constant, two definitions), or with no
    /// `_STA` seen in a scan that was uncertain: not probed, since only an
    /// interpreter could say whether the device is there.
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
    let mut seen = Seen::default();
    for t in tables {
        walk_table(t, &mut find, &mut seen);
    }
    into.dropped += find.dropped;
    // Pass 2: everything defined at each of those paths, in any table.
    let mut facts = [Facts::default(); MAX_CANDIDATES];
    let mut gather = Gather { paths: &find.paths[..find.count], facts: &mut facts[..find.count] };
    for t in tables {
        walk_table(t, &mut gather, &mut seen);
    }
    for f in &facts[..find.count] {
        if !f.device || f.crs_bad || f.hids != 1 {
            into.unreadable += 1;
            continue;
        }
        // Present only from tables seen whole: then no load-time code ran
        // (a top-level `Store` is itself unseeable), and either the device
        // has no `_STA`, which nothing can create later, or a constant one
        // no method could store to. Bit 0 present, bit 1 enabled (ACPI 6.5,
        // 6.3.7).
        match f.sta {
            Sta::Value(v) if v & 0x3 != 0x3 => into.absent += 1,
            _ if seen.uncertain => into.status_unknown += 1,
            Sta::Unknown => into.status_unknown += 1,
            Sta::Value(_) if seen.sta_in_a_method => into.status_unknown += 1,
            Sta::None | Sta::Value(_) => match f.crs {
                Some(t) => into.push(t),
                None => into.unreadable += 1,
            },
        }
    }
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

/// The deepest nesting of scopes followed. A name given from the root
/// (`Scope (\_SB) { Scope (\_SB) { ... } }`) keeps the path short however
/// deep the nesting, so the nesting is bounded on its own: deeper is
/// uncertain, and the recursion cannot run the kernel's stack out (the
/// fourth review of #407).
const MAX_NEST: usize = 32;

/// What a walk learns about the tables as a whole.
#[derive(Default)]
struct Seen {
    /// Something could not be seen through, and the rest of its scope may
    /// define any name: see [`term_list`].
    uncertain: bool,
    /// A method body somewhere holds the bytes `_STA`: it may store to a
    /// `Name (_STA, ...)` before the OS reads it, so no constant `_STA` is
    /// trusted (the fourth review of #407). A false match only keeps a
    /// device out.
    sta_in_a_method: bool,
}

/// Walks the table `table`'s whole TermList from its top level.
fn walk_table<V: Visit>(table: &[u8], v: &mut V, seen: &mut Seen) {
    if table.len() > HEADER_LEN {
        term_list(table, HEADER_LEN, table.len(), &Path::ROOT, 0, v, seen);
    }
}

/// Walks the objects of `table[p..end]` in the scope `scope`, as an AML
/// parser would: a `Name` by its value's encoding, a `Scope`, `Device`,
/// `Processor`, `PowerResource` or `ThermalZone` by recursing into its
/// body at its own path, a `Method` skipped whole by its PkgLength (its
/// body searched only for the bytes `_STA`, `Load` and `LoadTable`), a
/// `Field`, `IndexField` or `BankField` by the names of its units.
///
/// What it cannot see through sets [`Seen::uncertain`]: an `If`, `Else` or
/// `While` outside a method (executed at load, it may define any name or
/// store to one), an op it does not know, a length past its end, nesting
/// past [`MAX_NEST`], a method that loads a table. When the thing has a
/// PkgLength the walk goes on after it; otherwise the rest of the scope is
/// unseen and the walk of this scope stops.
fn term_list<V: Visit>(table: &[u8], mut p: usize, end: usize, scope: &Path, depth: usize, v: &mut V, seen: &mut Seen) {
    // An op's PkgLength at `at`: (where its package ends, where the
    // PkgLength ends). `None` when the package runs past `end`.
    let pkg = |at: usize| -> Option<(usize, usize)> {
        let (len, bytes) = pkg_length(table, at)?;
        let pkg_end = at.checked_add(len)?;
        (pkg_end <= end && len >= bytes).then_some((pkg_end, at + bytes))
    };
    while p < end {
        let op = (table[p], table.get(p + 1).copied());
        let next = match op {
            // NameOp: the name, then its value, recorded only once the value
            // is known to end inside this scope.
            (0x08, _) => name_string(table, p + 1, scope).and_then(|(path, len)| {
                let (value, next) = data(table, p + 1 + len)?;
                (next <= end).then(|| v.name(table, &path, &value))?;
                Some(next)
            }),
            // MethodOp: skipped whole; what it defines is unknowable here.
            (0x14, _) => pkg(p + 1).map(|(pkg_end, body)| {
                match name_string(table, body, scope) {
                    Some((path, len)) => {
                        v.opaque(&path);
                        // The body, after the name and the MethodFlags byte.
                        let code = table.get(body + len + 1..pkg_end).unwrap_or(&[]);
                        if code.windows(4).any(|w| w == b"_STA") {
                            seen.sta_in_a_method = true;
                        }
                        // Load and LoadTable add a table at run time.
                        if code.windows(2).any(|w| w == [0x5b, 0x20] || w == [0x5b, 0x1f]) {
                            seen.uncertain = true;
                        }
                    }
                    None => seen.uncertain = true,
                }
                pkg_end
            }),
            // ScopeOp, and the ExtOp scopes with a fixed header after
            // their name: Device (none), Processor (6 bytes), PowerResource
            // (3 bytes), ThermalZone (none). Walked into at their own path;
            // the walk of this scope goes on after them whatever they held.
            (0x10, _) | (0x5b, Some(0x82..=0x85)) => {
                let (pkg_at, header) = match op {
                    (0x10, _) => (p + 1, 0),
                    (_, Some(0x83)) => (p + 2, 6),
                    (_, Some(0x84)) => (p + 2, 3),
                    _ => (p + 2, 0),
                };
                pkg(pkg_at).map(|(pkg_end, body)| {
                    match name_string(table, body, scope) {
                        Some((path, len)) if body + len + header <= pkg_end && depth < MAX_NEST => {
                            if op == (0x5b, Some(0x82)) {
                                v.device(&path);
                            }
                            term_list(table, body + len + header, pkg_end, &path, depth + 1, v, seen);
                        }
                        _ => seen.uncertain = true,
                    }
                    pkg_end
                })
            }
            // ExternalOp: a name, an ObjectType and an ArgumentCount.
            (0x15, _) => name_string(table, p + 1, scope).map(|(_, len)| p + 1 + len + 2),
            // AliasOp: the new name is defined to something unknowable.
            (0x06, _) => name_string(table, p + 1, scope).and_then(|(_, len)| {
                let (alias, len2) = name_string(table, p + 1 + len, scope)?;
                v.opaque(&alias);
                Some(p + 1 + len + len2)
            }),
            // NoopOp.
            (0xa3, _) => Some(p + 1),
            // Field (a region, flags), IndexField (two names, flags),
            // BankField (two names, a constant bank value, flags): each unit
            // of the field list is a name defined in this scope.
            (0x5b, Some(0x81 | 0x86 | 0x87)) => pkg(p + 2).map(|(pkg_end, body)| {
                let units = (|| {
                    let (_, l1) = name_string(table, body, scope)?;
                    let mut q = body + l1;
                    if op.1 != Some(0x81) {
                        q += name_string(table, q, scope)?.1;
                    }
                    if op.1 == Some(0x87) {
                        let (Data::Int(_), after) = data(table, q)? else { return None };
                        q = after;
                    }
                    Some(q + 1)
                })();
                if !units.is_some_and(|q| field_units(table, q, pkg_end, scope, v)) {
                    seen.uncertain = true;
                }
                pkg_end
            }),
            // OperationRegion: a name, a RegionSpace byte, and an offset
            // and a length taken only as integer constants.
            (0x5b, Some(0x80)) => name_string(table, p + 2, scope).and_then(|(path, len)| {
                v.opaque(&path);
                let (Data::Int(_), q) = data(table, p + 2 + len + 1)? else { return None };
                let (Data::Int(_), q) = data(table, q)? else { return None };
                Some(q)
            }),
            // Mutex (a name and a SyncFlags byte), Event (a name).
            (0x5b, Some(0x01)) => name_string(table, p + 2, scope).map(|(_, len)| p + 2 + len + 1),
            (0x5b, Some(0x02)) => name_string(table, p + 2, scope).map(|(_, len)| p + 2 + len),
            // If, Else, While: their package is skipped, unseen.
            (0xa0..=0xa2, _) => {
                seen.uncertain = true;
                pkg(p + 1).map(|(pkg_end, _)| pkg_end)
            }
            // Anything else: not seen through, and of no known length.
            _ => None,
        };
        match next {
            Some(n) if n > p && n <= end => p = n,
            _ => {
                seen.uncertain = true;
                return;
            }
        }
    }
}

/// The units of a field list, `table[q..end]`: each named one reported as
/// defined at its path. `false` for an element this walk does not read.
fn field_units<V: Visit>(table: &[u8], mut q: usize, end: usize, scope: &Path, v: &mut V) -> bool {
    while q < end {
        q = match table[q] {
            // ReservedField: a PkgLength (a width in bits).
            0x00 => match pkg_length(table, q + 1) {
                Some((_, bytes)) => q + 1 + bytes,
                None => return false,
            },
            // AccessField: AccessType, AccessAttrib.
            0x01 => q + 3,
            // ExtendedAccessField: AccessType, ExtendedAccessAttrib, AccessLength.
            0x03 => q + 4,
            // NamedField: a NameSeg, then its width as a PkgLength.
            b'A'..=b'Z' | b'_' => {
                let Some((path, 4)) = name_string(table, q, scope) else { return false };
                let Some((_, bytes)) = pkg_length(table, q + 4) else { return false };
                v.opaque(&path);
                q + 4 + bytes
            }
            // ConnectField and anything else.
            _ => return false,
        };
    }
    q == end
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
    // BufferSize: an integer, by the one decoder `Name` values use.
    let (Data::Int(size), after) = data(table, p)? else { return None };
    p = after;
    let size = usize::try_from(size).ok()?;
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
