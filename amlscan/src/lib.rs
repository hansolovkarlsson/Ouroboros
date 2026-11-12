//! The virtio-mmio transports the ACPI tables declare present, found by
//! walking their AML namespace. The kernel probes a virtio-mmio address only when the
//! firmware has said a transport is there: until 2026-11-04 it scanned
//! QEMU `virt`'s 32 slots from `0xa000000` whenever a serial console had
//! been found before the exit, and Parallels 27.1, which gave its VM a
//! serial console that day, took a synchronous external abort on the first
//! slot (`docs/testing/testing-parallels.md`, "Parallels 27.1 and the
//! one-disk stick"). A platform that declares no transport is now never
//! scanned. The same walk finds any device by its `_HID` string
//! ([`declared`]): the Raspberry Pi 4's SD card controller, EMMC2
//! ([`EMMC2_HID`]), is the second user (`docs/roadmap/roadmap-sd-card.md`).
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
//! finds the path of every device given `Name (_HID, "<the id>")`; pass 2
//! gathers every `_STA` and `_CRS` defined at those paths, wherever they
//! are. Bytes inside a value, a method or another device are never read as
//! a name. Until the reviews of #407 this was a search of one device's
//! bytes, and each review found a way for it to read an absent device as
//! present.
//!
//! **Three method bodies are read, each in one exact shape**
//! ([`MethodShape`], the first two since 2026-11-05 and the third since
//! 2026-11-12, as the Pi's firmware writes them): a `_STA` that is
//! only `Return (<constant>)`, which nothing can store to, so it counts as
//! a constant `_STA`; a `_CRS` that is only `Name (<buffer>, Buffer
//! {...}); Return (<buffer>)`, the buffer named inside the method itself,
//! which is read as a `_CRS` buffer is (the name lives only while the
//! method runs, and the method does nothing between making it and
//! returning it, so nothing can change its bytes); and a `_CRS` that is
//! only `CreateDWordField
//! (^<buffer>, <offset>, <field>); Store (<constant>, <field>);
//! Return (^<buffer>)`, where `<buffer>` is a `Name` in the same device. A
//! third pass finds that buffer's definitions in every table; with exactly
//! one, a Buffer kept whole, a copy is patched and read as a `_CRS` buffer
//! is. Any other method body is skipped whole, as before. The base the
//! patching shape gives cannot be moved by another method: the `_CRS` stores its constant
//! into the buffer each time it is evaluated, right before returning it, so
//! a store by `_INI` or any other method to the same bytes is overwritten.
//! What another method could change unseen is the rest of the buffer, its
//! length or interrupt, which is the residual: a method body is not read for
//! stores into a buffer, as it is read for the bytes `_STA`, because the
//! Pi's container `GDV1` has a `_CRS` patching a buffer of the same name
//! (its own `RBUF`), and telling the two apart takes the name resolution
//! only an interpreter has (the review of step 1, 2026-11-06).
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
//! `_CRS` defined twice, by an `Alias`, or by a method of any other shape; a `_CRS` with no
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
    /// Declarations of the `_HID` this scan could not read as one device: a
    /// path no `Device` defines, a second `_HID`, or no single `_CRS`
    /// buffer it can read. Never probed.
    pub unreadable: usize,
    /// Devices whose constant `_STA` says not present or not
    /// enabled: not probed.
    pub absent: usize,
    /// Devices with a `_STA` this scan cannot evaluate (a
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

/// The most devices of one `_HID` tracked; past it they are counted as
/// dropped.
const MAX_CANDIDATES: usize = 2 * MAX_TRANSPORTS;

/// Adds to `into` every virtio-mmio transport the AML tables `tables` (the
/// DSDT and every SSDT, each with its header) declare present. One call
/// for all of them: a device's `_STA` or `_CRS` may be defined in another
/// table than the device, by a `Scope` of its path (the third review of
/// #407).
pub fn virtio_mmio(tables: &[&[u8]], into: &mut Transports) {
    declared(tables, VIRTIO_MMIO_HID, into);
}

/// The `_HID` the BCM2711's EMMC2 controller is declared by, the one the
/// Raspberry Pi 4's SD card sits on (edk2-platforms `Emmc.asl`; Linux's
/// `sdhci-iproc` matches it too).
pub const EMMC2_HID: &[u8; 8] = b"BRCME88C";

/// The `_HID` the BCM2711's on-board Ethernet, GENET v5, is declared by on
/// the Raspberry Pi 4 (`Device (ETH0)` in the board's DSDT; Linux's
/// `bcmgenet` matches it too). Its `_CRS` names two interrupts, and the
/// first, the one kept, is the default queue's and the link's
/// (`docs/roadmap/roadmap-pi-net.md`).
pub const GENET_HID: &[u8; 7] = b"BCM6E4E";

/// Adds to `into` the register block and interrupt of every device the
/// tables declare present with `Name (_HID, "<hid>")`, by the same rules as
/// [`virtio_mmio`]: the walk, `_STA` and `_CRS` as the module doc says,
/// each a `Name` or one of the method shapes it reads.
pub fn declared(tables: &[&[u8]], hid: &[u8], into: &mut Transports) {
    if hid.is_empty() {
        return;
    }
    // Pass 1: the path of every Device with `Name (_HID, "<hid>")`.
    let mut find = FindHids { hid, paths: [Path::ROOT; MAX_CANDIDATES], count: 0, dropped: 0 };
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
    // Pass 3: the buffer each patching `_CRS` method names, in any table.
    let mut targets = [None; MAX_CANDIDATES];
    for (t, f) in targets.iter_mut().zip(&facts[..find.count]) {
        *t = f.crs_patch.map(|(seg, _, _)| seg);
    }
    let mut bufs = [Buf::default(); MAX_CANDIDATES];
    if targets.iter().any(Option::is_some) {
        let mut pass3 = FindBufs { paths: &find.paths[..find.count], targets: &targets[..find.count], bufs: &mut bufs[..find.count] };
        for t in tables {
            walk_table(t, &mut pass3, &mut seen);
        }
    }
    for (f, buf) in facts[..find.count].iter().zip(&bufs) {
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
            Sta::None | Sta::Value(_) => match f.crs.or_else(|| f.crs_patch.and_then(|p| patched_crs(p, buf))) {
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
    /// A Method of one of the shapes [`method_shape`] reads exactly. By
    /// default as unknowable as any other method.
    fn method(&mut self, path: &Path, _shape: &MethodShape) {
        self.opaque(path);
    }
}

/// The three method bodies this scan reads, each in exactly one shape, as
/// edk2-platforms writes them for the Raspberry Pi 4. Two are `Emmc.asl`'s
/// (read off the board on 2026-11-05):
///
/// ```text
/// 14 09 "_STA" 00 A4 0A 0F               Method (_STA) { Return (0x0F) }
/// 14 22 "_CRS" 08                        Method (_CRS, 0, Serialized) {
///   8A 5E "RBUF" 0A 04 "RBAS"              CreateDWordField (^RBUF, 4, RBAS)
///   70 0C 0000 34FE "RBAS"                 Store (0xFE340000, RBAS)
///   A4 5E "RBUF"                           Return (^RBUF) }
/// ```
///
/// The third is the `_CRS` of the board's Ethernet, `ETH0` in its DSDT
/// (read in the dumped table on 2026-11-12), its buffer a name of the
/// method's own:
///
/// ```text
/// 14 2F "_CRS" 08                        Method (_CRS, 0, Serialized) {
///   08 "RBUF" 11 1E 0A 1B ...              Name (RBUF, Buffer (0x1B) {...})
///   A4 "RBUF"                              Return (RBUF) }
/// ```
///
/// Anything else in a body, another statement, an argument, a
/// different field op, leaves the method unknowable, as every other is.
enum MethodShape {
    /// `Return (<constant>)`.
    Sta(u64),
    /// A buffer at `buffer`, a DWord at byte `offset` of it set to `value`,
    /// the buffer returned.
    CrsPatch { buffer: Path, offset: u64, value: u32 },
    /// A buffer named inside the method and returned: what its bytes
    /// describe, `None` when they are no register block (no
    /// `Memory32Fixed`, a zero length, a length past the buffer).
    CrsLocal(Option<Transport>),
}

/// The shape of the method at `path`, its MethodFlags byte at `flags`, its
/// body ending at `end`; `None` unless it is exactly one of [`MethodShape`]'s.
/// Names in the body resolve against the method's own path, as AML's do: a
/// `^` names its device.
fn method_shape(table: &[u8], path: &Path, flags: usize, end: usize) -> Option<MethodShape> {
    // No arguments.
    if *table.get(flags)? & 0x07 != 0 {
        return None;
    }
    let code = flags + 1;
    match path.split_last()?.1 {
        [b'_', b'S', b'T', b'A'] => {
            if *table.get(code)? != 0xa4 {
                return None;
            }
            let (Data::Int(v), next) = data(table, code + 1)? else { return None };
            (next == end).then_some(MethodShape::Sta(v))
        }
        [b'_', b'C', b'R', b'S'] if table.get(code) == Some(&0x08) => {
            // Name (<a name of the method's own>, Buffer {...}).
            let (buffer, l1) = name_string(table, code + 1, path)?;
            if buffer.split_last()?.0 != *path {
                return None;
            }
            let (Data::Buffer(at), q) = data(table, code + 1 + l1)? else { return None };
            // Return (<that buffer>), and nothing after it.
            if *table.get(q)? != 0xa4 {
                return None;
            }
            let (returned, l2) = name_string(table, q + 1, path)?;
            (returned == buffer && q + 1 + l2 == end)
                .then(|| MethodShape::CrsLocal(buffer_bytes(table, at).and_then(resource_bytes)))
        }
        [b'_', b'C', b'R', b'S'] => {
            // CreateDWordField (SourceBuff, ByteIndex, NameString).
            if *table.get(code)? != 0x8a {
                return None;
            }
            let (buffer, l1) = name_string(table, code + 1, path)?;
            let (Data::Int(offset), q) = data(table, code + 1 + l1)? else { return None };
            let (field, l2) = name_string(table, q, path)?;
            // Store (<constant>, <that field>).
            let q = q + l2;
            if *table.get(q)? != 0x70 {
                return None;
            }
            let (Data::Int(value), q) = data(table, q + 1)? else { return None };
            let (target, l3) = name_string(table, q, path)?;
            // Return (<that buffer>), and nothing after it.
            let q = q + l3;
            if *table.get(q)? != 0xa4 {
                return None;
            }
            let (returned, l4) = name_string(table, q + 1, path)?;
            let value = u32::try_from(value).ok()?;
            (target == field && returned == buffer && q + 1 + l4 == end)
                .then_some(MethodShape::CrsPatch { buffer, offset, value })
        }
        _ => None,
    }
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
                        match method_shape(table, &path, body + len, pkg_end) {
                            Some(shape) => v.method(&path, &shape),
                            None => v.opaque(&path),
                        }
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

/// Pass 1: every device path given `Name (_HID, "<hid>")`.
struct FindHids<'a> {
    hid: &'a [u8],
    paths: [Path; MAX_CANDIDATES],
    count: usize,
    dropped: usize,
}

impl Visit for FindHids<'_> {
    fn name(&mut self, table: &[u8], path: &Path, value: &Data) {
        let (Some((parent, b"_HID")), Data::Str(a, b)) = (path.split_last().as_ref().map(|(p, s)| (*p, s)), value) else {
            return;
        };
        if &table[*a..*b] != self.hid || self.paths[..self.count].contains(&parent) {
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

/// The longest named buffer kept for a `_CRS` method to patch: the Pi's
/// is 23 bytes (a `Memory32Fixed`, an Extended Interrupt, the end tag).
const MAX_BUFFER: usize = 32;

/// The one buffer a patching `_CRS` method names, as pass 3 finds it: how
/// many things define its path, and its bytes when the one definition is a
/// `Name` of a Buffer no longer than [`MAX_BUFFER`].
#[derive(Clone, Copy)]
struct Buf {
    defs: u8,
    len: usize,
    bytes: [u8; MAX_BUFFER],
}

impl Default for Buf {
    fn default() -> Self {
        Buf { defs: 0, len: 0, bytes: [0; MAX_BUFFER] }
    }
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
    /// A `_CRS` method of the patching shape: the buffer's segment (in the
    /// device), the byte offset and the value.
    crs_patch: Option<([u8; 4], usize, u32)>,
}

/// The `_CRS` a patching method returns: its buffer, copied, the DWord at
/// its offset set, read as any `_CRS` buffer is. `None` when the buffer is
/// defined other than once by a `Name` kept whole, or the DWord does not
/// fit inside it.
fn patched_crs(patch: ([u8; 4], usize, u32), buf: &Buf) -> Option<Transport> {
    let (_, offset, value) = patch;
    if buf.defs != 1 || buf.len == 0 || offset.checked_add(4)? > buf.len {
        return None;
    }
    let mut bytes = buf.bytes;
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    resource_bytes(&bytes[..buf.len])
}

/// Pass 3: for each device whose `_CRS` is a patching method, every
/// definition of the buffer it names, in any table.
struct FindBufs<'a> {
    paths: &'a [Path],
    targets: &'a [Option<[u8; 4]>],
    bufs: &'a mut [Buf],
}

impl FindBufs<'_> {
    fn at(&mut self, path: &Path) -> Option<&mut Buf> {
        let (parent, seg) = path.split_last()?;
        let i = self.paths.iter().zip(self.targets).position(|(p, t)| *p == parent && *t == Some(seg))?;
        Some(&mut self.bufs[i])
    }
}

impl Visit for FindBufs<'_> {
    fn device(&mut self, path: &Path) {
        if let Some(b) = self.at(path) {
            b.defs = b.defs.saturating_add(1);
        }
    }

    fn name(&mut self, table: &[u8], path: &Path, value: &Data) {
        let Some(b) = self.at(path) else { return };
        b.defs = b.defs.saturating_add(1);
        let bytes = match value {
            Data::Buffer(at) => buffer_bytes(table, *at),
            _ => None,
        };
        match bytes {
            Some(bytes) if bytes.len() <= MAX_BUFFER => {
                b.bytes[..bytes.len()].copy_from_slice(bytes);
                b.len = bytes.len();
            }
            _ => b.len = 0,
        }
    }

    fn opaque(&mut self, path: &Path) {
        if let Some(b) = self.at(path) {
            b.defs = b.defs.saturating_add(1);
        }
    }
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
            b"_CRS" => match (f.crs.is_some() || f.crs_bad || f.crs_patch.is_some(), value) {
                (false, Data::Buffer(at)) => match buffer_bytes(table, *at).and_then(resource_bytes) {
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

    fn method(&mut self, path: &Path, shape: &MethodShape) {
        let Some((f, seg)) = self.at(path) else { return };
        match (&seg, shape) {
            (b"_STA", MethodShape::Sta(v)) => {
                f.sta = match f.sta {
                    Sta::None => Sta::Value(*v),
                    _ => Sta::Unknown,
                }
            }
            (b"_CRS", MethodShape::CrsPatch { buffer, offset, value }) => {
                // The buffer must be the device's own, beside the method.
                let device = path.split_last().map(|(d, _)| d);
                let own = buffer.split_last().filter(|(d, _)| Some(*d) == device).map(|(_, s)| s);
                match (f.crs.is_some() || f.crs_bad || f.crs_patch.is_some(), own, usize::try_from(*offset)) {
                    (false, Some(s), Ok(offset)) => f.crs_patch = Some((s, offset, *value)),
                    _ => {
                        f.crs = None;
                        f.crs_patch = None;
                        f.crs_bad = true;
                    }
                }
            }
            (b"_CRS", MethodShape::CrsLocal(t)) => match (f.crs.is_some() || f.crs_bad || f.crs_patch.is_some(), t) {
                (false, Some(t)) => f.crs = Some(*t),
                _ => {
                    f.crs = None;
                    f.crs_patch = None;
                    f.crs_bad = true;
                }
            },
            _ => self.opaque(path),
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

/// The bytes of the Buffer whose BufferOp is at `at`: its BufferSize, cut
/// at its PkgLength and at the table's end. `None` for a length this scan
/// cannot read.
fn buffer_bytes(table: &[u8], at: usize) -> Option<&[u8]> {
    let (pkg, pkg_bytes) = pkg_length(table, at + 1)?;
    let buf_end = (at + 1).checked_add(pkg)?.min(table.len());
    // BufferSize: an integer, by the one decoder `Name` values use.
    let (Data::Int(size), p) = data(table, at + 1 + pkg_bytes)? else { return None };
    let size = usize::try_from(size).ok()?;
    let data_end = p.checked_add(size)?.min(buf_end);
    table.get(p..data_end)
}

/// The transport a `_CRS` buffer's bytes describe: its `Memory32Fixed` and
/// the first INTID of its Extended Interrupt. `None` when it has no
/// `Memory32Fixed`, or when a length points past the buffer.
fn resource_bytes(table: &[u8]) -> Option<Transport> {
    let mut p = 0;
    let data_end = table.len();
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

/// The byte offset of the first `Name (_HID, "<hid>")` in `table`, a
/// string `_HID` written as NameOp, the name, a string prefix, the string
/// and its zero. A byte search, not the namespace walk above: it answers
/// "does this table hold the declaration", for a report (the kernel's
/// `sdscan.rs`), and says nothing of where the device is or whether its
/// `_STA` lets it be touched. An empty `hid` is never found.
pub fn hid_string_at(table: &[u8], hid: &[u8]) -> Option<usize> {
    if hid.is_empty() {
        return None;
    }
    let len = 6 + hid.len() + 1;
    table
        .windows(len)
        .position(|w| w[..6] == [0x08, b'_', b'H', b'I', b'D', 0x0d] && &w[6..len - 1] == hid && w[len - 1] == 0)
}

#[cfg(test)]
mod tests;
