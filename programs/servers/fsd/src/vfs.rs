//! The filesystem-multiplexing layer inside `fsd` (the VFS refactor).
//!
//! Clients already speak a filesystem-agnostic protocol (`FSOP_*`) over IPC -
//! they never know it's FAT32. What was missing was *internal* multiplexing so
//! `fsd` can drive more than one on-disk format: detect the type at mount time
//! and dispatch each op to the right driver. `Filesystem` is that dispatch.
//!
//! An **enum** (not `dyn Trait`) because `fsd` is `no_std` with no heap - the
//! same enum-over-`dyn` pattern `block::BlockDevice` and `console::Console`
//! already use for the same reason. Today FAT32 is the only arm; a second
//! filesystem (exFAT, ext2) is a new arm plus a branch in [`Filesystem::mount`]'s
//! type detection. The per-op methods forward to the arm, so this is a pure
//! refactor: `main.rs` calls `Filesystem` exactly as it called `fat32::Fs`, and
//! behaviour is byte-identical while FAT32 is the only format.

use core::ops::ControlFlow;

use crate::disk::Disk;
use crate::exfat;
use crate::ext2;
use crate::fat32;
use crate::partition;
use crate::proc;

/// Failure reasons, shared across every filesystem arm (defined in `fat32.rs`,
/// which held the only arm originally; now shared by the exFAT arm too).
pub use crate::fat32::Error;

/// A broken-down calendar timestamp (local time as the filesystem stores it) -
/// deliberately not an epoch, so no filesystem's differing epoch leaks into the
/// stat ABI and the client formats it with no date math.
#[derive(Clone, Copy)]
pub struct CalTime {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub min: u8,
    pub sec: u8,
}

/// POSIX ownership + permission bits for a path (ext2's `i_mode`/`i_uid`/
/// `i_gid`). Only a filesystem that stores them surfaces this; FAT32/exFAT/
/// `/proc` return `None` from [`Filesystem::stat`] since they can't model an
/// owner or mode. Read-only for now - stored, not enforced, and no `chmod`
/// write path yet (see `docs/gap-analysis.md`).
#[derive(Clone, Copy)]
pub struct FileMode {
    /// The `S_IFMT` type nibble plus the 12 permission bits, verbatim.
    pub mode: u16,
    pub uid: u16,
    pub gid: u16,
}

/// A path's metadata, from [`Filesystem::stat`] - what `ls -l` reports. `time`
/// is `None` when the filesystem doesn't (yet) surface a modified time; `mode`
/// is `None` when it can't model an owner/permissions at all.
pub struct Stat {
    pub size: u64,
    pub is_dir: bool,
    pub time: Option<CalTime>,
    pub mode: Option<FileMode>,
}

/// A mounted filesystem, whatever its on-disk format.
// FAT32's arm carries a one-sector FAT cache (`fat32::Fs::fat_cache`), 512
// bytes the others lack. `fsd` has no heap to box it in, and the mount table
// (`MAX_MOUNTS`, five) lives on its stack: about 2.7 KB of the 56 KB the
// loader gives it, taken knowingly.
#[allow(clippy::large_enum_variant)]
pub enum Filesystem {
    Fat32(fat32::Fs),
    /// exFAT, read-write (see [`exfat`]).
    ExFat(exfat::Fs),
    /// ext2, read-write (see [`ext2`]).
    Ext2(ext2::Fs),
    /// `/proc` - a synthetic, read-only process-table filesystem (no disk;
    /// generated from `TASK_STATE`). The first non-disk arm, proving the enum is
    /// a genuine VFS and not just a format multiplexer. See [`proc`] and cluster
    /// Phase 3 (`docs/roadmap/roadmap-cluster-phase3.md`).
    Proc(proc::Fs),
}

impl Filesystem {
    /// The synthetic `/proc` filesystem - auto-mounted at the reserved `PROC_TREE`
    /// index at boot (no disk, so no probing).
    pub fn proc() -> Self {
        Filesystem::Proc(proc::Fs::new())
    }

    /// Discover the disk's partitions (MBR or GPT - see `partition::discover`)
    /// and mount one that holds a filesystem we recognize: each is probed as
    /// FAT32 ([`fat32::Fs::mount_at`]), exFAT ([`exfat::Fs::mount_at`]) and
    /// ext2 ([`ext2::Fs::mount_at`]), in table order, except as below.
    /// Returns [`Error::NoFat32Partition`] if no partition mounts as any
    /// known format, or the [`Error::Io`] of a probe whose read failed when
    /// none mounts ([`first_mounting`](Self::first_mounting)). The
    /// [`Fallback`] is set when what mounted is not the root's candidate,
    /// every one of which failed to mount: the caller says
    /// so and mounts it read-only, since a root that did not probe then
    /// leaves the ESP or another partition as `/` (the high review of #288,
    /// the review of #295). The last of the three is whether a partition
    /// tried before the one that mounted would not read, for the caller to
    /// say: a disk failing a read is not a partition with nothing on it.
    ///
    /// The partition the system booted from, when the kernel knows it is on
    /// this disk ([`Disk::boot_partition_start`]), is passed over while any
    /// other partition mounts: on one disk carrying the ESP and a root beside
    /// it, the root is `/`, whatever their order. A disk whose only
    /// filesystem is the boot partition mounts it as before (one-disk step 4,
    /// docs/roadmap/roadmap-one-disk.md). A partition typed as a root is
    /// tried before, and in place of, any untyped one ([`root_order`]).
    pub fn mount(mut disk: Disk) -> Result<(Self, Option<Fallback>, bool), Error> {
        let mut parts = [partition::Partition::default(); partition::MAX_PARTITIONS];
        let count = partition::discover(&mut disk, &mut parts)?;
        let boot = disk.boot_partition_start();
        let roots = root_order(&parts, count, boot, disk.capacity_sectors().ok());
        let probed = Self::first_mounting(&roots.order[..roots.len]);
        let Some((i, fs)) = probed.found else {
            return Err(probed.failed.unwrap_or(Error::NoFat32Partition));
        };
        let fallback = roots.is_fallback(i).then_some(Fallback {
            typed_root: roots.typed,
            boot_partition: Some(roots.order[i].part.start) == boot,
        });
        Ok((fs, fallback, probed.failed.is_some()))
    }

    /// The first of `parts` that [`probe`](Self::probe)
    /// mounts, with its index in `parts`, and the [`Error::Io`] of a probe
    /// before it whose read failed (of any, when none mounts). A partition
    /// that would not read is passed over, so a root that will not read still
    /// leaves the ESP beside it to come up read-only, but it is never taken
    /// for one with nothing on it: when nothing mounts [`mount`](Self::mount)
    /// answers the read's error, so `mount -a` does not swap a stick in for a
    /// disk that is there and failing, and `format` refuses whenever a read
    /// failed, rather than laying a filesystem over a partition it could not
    /// read or over a later one that did. The one rule `mount` and `format`
    /// both pick by. A start LBA past 32 bits is not tried (the layout
    /// arithmetic is 32-bit; partitions past 2 TB aren't a concern here).
    pub fn first_mounting(parts: &[Candidate]) -> Probed {
        let mut failed = None;
        for (i, c) in parts.iter().enumerate() {
            let Ok(lba32) = u32::try_from(c.part.start) else {
                continue;
            };
            match Self::probe(lba32, c.bound) {
                Ok(Some(fs)) => return Probed { found: Some((i, fs)), failed },
                Ok(None) => {}
                Err(e) => failed = failed.or(Some(e)),
            }
        }
        Probed { found: None, failed }
    }

    /// The filesystem at `lba`, as FAT32, exFAT or ext2, the first that
    /// validates; `None` when none does. Every arm is tried, since they read
    /// different sectors (ext2's superblock is the partition's third and
    /// fourth, FAT32's and exFAT's boot sector its first), so one arm's
    /// failed read does not hide another arm's volume; when none validates,
    /// a read that failed is [`Error::Io`], never `None`: a partition that
    /// could not be read has not been shown to hold no filesystem. What
    /// [`first_mounting`](Self::first_mounting) tries at each partition.
    /// `sectors` is the most the volume may span ([`volume_bound`]): each arm
    /// refuses a volume claiming more, so no cluster or block it names lies
    /// past the partition, in the next one or past the disk.
    pub fn probe(lba: u32, sectors: u64) -> Result<Option<Self>, Error> {
        let mut failed = None;
        match fat32::Fs::mount_at(Disk, lba, sectors) {
            Ok(fs) => return Ok(Some(Filesystem::Fat32(fs))),
            Err(e @ Error::Io(_)) => failed = failed.or(Some(e)),
            Err(_) => {}
        }
        match exfat::Fs::mount_at(Disk, lba, sectors) {
            Ok(fs) => return Ok(Some(Filesystem::ExFat(fs))),
            Err(e @ Error::Io(_)) => failed = failed.or(Some(e)),
            Err(_) => {}
        }
        match ext2::Fs::mount_at(Disk, lba, sectors) {
            Ok(fs) => return Ok(Some(Filesystem::Ext2(fs))),
            Err(e @ Error::Io(_)) => failed = failed.or(Some(e)),
            Err(_) => {}
        }
        failed.map_or(Ok(None), Err)
    }

    /// Mount the disk's `index`-th partition, counted in the MBR/GPT
    /// discovery order (the table's, not [`mount`](Self::mount)'s, which tries
    /// the boot partition last: on `disk.img` tree 0 is index 1). Probes
    /// FAT32-then-exFAT-then-ext2 at that one partition ([`probe`](Self::probe)).
    /// The multi-mount entry point (cluster Phase 0): a client mounts a
    /// *specific* partition into a tree rather than taking whatever validates
    /// first. Returns
    /// [`Error::NoFat32Partition`] if `index` is out of range or the partition
    /// mounts as no known format, [`Error::Io`] if a read failed.
    pub fn mount_partition(mut disk: Disk, index: usize) -> Result<Self, Error> {
        let mut parts = [partition::Partition::default(); partition::MAX_PARTITIONS];
        let count = partition::discover(&mut disk, &mut parts)?;
        if index >= count {
            return Err(Error::NoFat32Partition);
        }
        let Ok(lba32) = u32::try_from(parts[index].start) else {
            return Err(Error::NoFat32Partition);
        };
        let bound = volume_bound(&parts[index], &parts[..count], disk.capacity_sectors().ok());
        Self::probe(lba32, bound)?.ok_or(Error::NoFat32Partition)
    }

    /// The mounted format's name, for the startup/mount log line.
    pub fn name(&self) -> &'static str {
        match self {
            Filesystem::Fat32(_) => "FAT32",
            Filesystem::ExFat(_) => "exFAT",
            Filesystem::Ext2(_) => "ext2",
            Filesystem::Proc(_) => "proc",
        }
    }

    /// Why this filesystem was mounted read-only, for fsd's mount lines:
    /// exFAT's reason ([`exfat::Fs::read_only`]); `None` for every other arm,
    /// which mounts writable or not at all.
    pub fn read_only(&self) -> Option<&'static str> {
        match self {
            Filesystem::ExFat(fs) => fs.read_only(),
            _ => None,
        }
    }

    /// Whether this filesystem lives on a disk partition (every arm but the
    /// synthetic `/proc`), so that `partition_lba` names a real place.
    pub fn is_disk(&self) -> bool {
        !matches!(self, Filesystem::Proc(_))
    }

    /// Whether directory entry name `entry` answers to `name` here, the rule
    /// every lookup of this filesystem uses (`fat32::names_match` and
    /// `exfat::names_match` fold ASCII case, exFAT's by the `upcase` its
    /// NameHash uses; `ext2::names_match` matches bytes; `/proc`'s names are its
    /// own bytes). `NP_MV`'s guard against a move into the
    /// mover's own subtree compares paths by it, so it cannot state a different
    /// rule from the lookups it guards.
    pub fn names_match(&self, entry: &str, name: &str) -> bool {
        match self {
            Filesystem::Fat32(_) => fat32::names_match(entry, name),
            Filesystem::ExFat(_) => exfat::names_match(entry, name),
            Filesystem::Ext2(_) => ext2::names_match(entry, name),
            Filesystem::Proc(_) => entry == name,
        }
    }

    /// The first sector of the mounted volume - `mount`-info reporting only
    /// (disk-tools milestone 1).
    pub fn partition_lba(&self) -> u32 {
        match self {
            Filesystem::Fat32(fs) => fs.partition_lba(),
            Filesystem::ExFat(fs) => fs.partition_lba(),
            Filesystem::Ext2(fs) => fs.partition_lba(),
            Filesystem::Proc(fs) => fs.partition_lba(),
        }
    }

    /// Set the owner (uid, gid) new inodes are created with, from the calling
    /// task's identity. Only ext2 records owners; the other arms ignore it.
    /// fsd calls this before each op so a created file/dir is owned by its
    /// creator rather than root (the permission model needs it).
    pub fn set_creator(&mut self, uid: u16, gid: u16) {
        if let Filesystem::Ext2(fs) = self {
            fs.set_creator(uid, gid);
        }
    }

    /// Calls `f(name, is_dir)` for each entry of the directory at `path`
    /// until it answers `Break` (a full `NP_READDIR` page). No size: nothing
    /// listing needs one, and on ext2 it would cost an inode read an entry.
    pub fn list_dir(
        &mut self,
        path: &str,
        f: impl FnMut(&str, bool) -> ControlFlow<()>,
    ) -> Result<(), Error> {
        match self {
            Filesystem::Fat32(fs) => fs.list_dir(path, f),
            Filesystem::ExFat(fs) => fs.list_dir(path, f),
            Filesystem::Ext2(fs) => fs.list_dir(path, f),
            Filesystem::Proc(fs) => fs.list_dir(path, f),
        }
    }

    pub fn read_file(&mut self, path: &str, buf: &mut [u8]) -> Result<u32, Error> {
        match self {
            Filesystem::Fat32(fs) => fs.read_file(path, buf),
            Filesystem::ExFat(fs) => fs.read_file(path, buf),
            Filesystem::Ext2(fs) => fs.read_file(path, buf),
            Filesystem::Proc(fs) => fs.read_file(path, buf),
        }
    }

    /// Metadata for a single path (`ls -l`): size, whether it's a directory,
    /// and the modified time when the filesystem surfaces one. Only FAT32
    /// currently decodes a timestamp; the others return `time: None` (size and
    /// type are always real).
    pub fn stat(&mut self, path: &str) -> Result<Stat, Error> {
        match self {
            Filesystem::Fat32(fs) => fs.stat(path),
            Filesystem::ExFat(fs) => fs.stat(path),
            Filesystem::Ext2(fs) => fs.stat(path),
            Filesystem::Proc(fs) => fs.stat(path),
        }
    }

    pub fn read_at(&mut self, path: &str, offset: u64, buf: &mut [u8]) -> Result<u32, Error> {
        match self {
            Filesystem::Fat32(fs) => fs.read_at(path, offset, buf),
            Filesystem::ExFat(fs) => fs.read_at(path, offset, buf),
            Filesystem::Ext2(fs) => fs.read_at(path, offset, buf),
            Filesystem::Proc(fs) => fs.read_at(path, offset, buf),
        }
    }

    /// Read up to `want` bytes of `path` from `offset`, a piece of at most
    /// `buf.len()` at a time, each handed to `sink`, with the path resolved
    /// once: see [`read_pieces`]. `NP_READ` copies each piece to its client.
    pub fn read_each(
        &mut self,
        path: &str,
        offset: u64,
        want: u64,
        buf: &mut [u8],
        sink: impl FnMut(&[u8]) -> ControlFlow<()>,
    ) -> Result<(u64, bool), Error> {
        match self {
            Filesystem::Fat32(fs) => fs.read_each(path, offset, want, buf, sink),
            Filesystem::ExFat(fs) => fs.read_each(path, offset, want, buf, sink),
            Filesystem::Ext2(fs) => fs.read_each(path, offset, want, buf, sink),
            Filesystem::Proc(fs) => fs.read_each(path, offset, want, buf, sink),
        }
    }

    pub fn write_file(&mut self, path: &str, data: &[u8]) -> Result<(), Error> {
        match self {
            Filesystem::Fat32(fs) => fs.write_file(path, data),
            Filesystem::ExFat(fs) => fs.write_file(path, data),
            Filesystem::Ext2(fs) => fs.write_file(path, data),
            Filesystem::Proc(fs) => fs.write_file(path, data),
        }
    }

    pub fn write_at(&mut self, path: &str, offset: u64, data: &[u8]) -> Result<(), Error> {
        match self {
            Filesystem::Fat32(fs) => fs.write_at(path, offset, data),
            Filesystem::ExFat(fs) => fs.write_at(path, offset, data),
            Filesystem::Ext2(fs) => fs.write_at(path, offset, data),
            Filesystem::Proc(fs) => fs.write_at(path, offset, data),
        }
    }

    pub fn mkdir(&mut self, path: &str) -> Result<(), Error> {
        match self {
            Filesystem::Fat32(fs) => fs.mkdir(path),
            Filesystem::ExFat(fs) => fs.mkdir(path),
            Filesystem::Ext2(fs) => fs.mkdir(path),
            Filesystem::Proc(fs) => fs.mkdir(path),
        }
    }

    pub fn rmdir(&mut self, path: &str) -> Result<(), Error> {
        match self {
            Filesystem::Fat32(fs) => fs.rmdir(path),
            Filesystem::ExFat(fs) => fs.rmdir(path),
            Filesystem::Ext2(fs) => fs.rmdir(path),
            Filesystem::Proc(fs) => fs.rmdir(path),
        }
    }

    pub fn touch(&mut self, path: &str) -> Result<(), Error> {
        match self {
            Filesystem::Fat32(fs) => fs.touch(path),
            Filesystem::ExFat(fs) => fs.touch(path),
            Filesystem::Ext2(fs) => fs.touch(path),
            Filesystem::Proc(fs) => fs.touch(path),
        }
    }

    pub fn rm(&mut self, path: &str) -> Result<(), Error> {
        match self {
            Filesystem::Fat32(fs) => fs.rm(path),
            Filesystem::ExFat(fs) => fs.rm(path),
            Filesystem::Ext2(fs) => fs.rm(path),
            Filesystem::Proc(fs) => fs.rm(path),
        }
    }

    pub fn mv(&mut self, src: &str, dst: &str) -> Result<(), Error> {
        match self {
            Filesystem::Fat32(fs) => fs.mv(src, dst),
            Filesystem::ExFat(fs) => fs.mv(src, dst),
            Filesystem::Ext2(fs) => fs.mv(src, dst),
            Filesystem::Proc(fs) => fs.mv(src, dst),
        }
    }

    /// Set a path's permission bits (the low 12 of the mode; the type nibble is
    /// preserved). The write twin of [`stat`](Self::stat)'s mode field — ext2
    /// only, since it's the one arm that stores a mode. FAT32/exFAT/`/proc`
    /// return [`Error::Unsupported`] rather than pretend to succeed.
    pub fn chmod(&mut self, path: &str, mode: u16) -> Result<(), Error> {
        match self {
            Filesystem::Ext2(fs) => fs.chmod(path, mode),
            _ => Err(Error::Unsupported),
        }
    }

    /// Set a path's owner uid and/or gid (`None` leaves that field unchanged).
    /// ext2 only, same per-filesystem degradation as [`chmod`](Self::chmod).
    pub fn chown(&mut self, path: &str, uid: Option<u16>, gid: Option<u16>) -> Result<(), Error> {
        match self {
            Filesystem::Ext2(fs) => fs.chown(path, uid, gid),
            _ => Err(Error::Unsupported),
        }
    }
}

/// The most sectors a volume in `p` may span, the bound every arm's
/// `mount_at` checks its boot sector against: the table's length (none when
/// it gives 0: an MBR entry of size 0, a GPT entry ending before it starts,
/// which mounted from the boot sector alone before 2026-10-20), cut to the
/// start of the next partition in `table` (so no entry's volume, sized or
/// not, claims a partition after it; the second review of #330), to the
/// disk's `capacity` (an entry longer than the device, as on an image written
/// to a smaller stick), and to sector 2^32, since the layout arithmetic of
/// every arm is 32-bit: a volume reaching past it would wrap its sectors onto
/// the start of the disk. A capacity the kernel does not answer cuts nothing.
/// `format` sizes the volume it lays by it too, so a new volume never claims
/// past the disk or into another partition.
pub fn volume_bound(p: &partition::Partition, table: &[partition::Partition], capacity: Option<u64>) -> u64 {
    let length = if p.sectors == 0 { u64::MAX } else { p.sectors };
    let next = table.iter().filter(|q| q.start > p.start).map(|q| q.start - p.start).min();
    let disk = capacity.map(|c| c.saturating_sub(p.start));
    let wrap = (1u64 << 32).saturating_sub(p.start);
    length.min(next.unwrap_or(u64::MAX)).min(disk.unwrap_or(u64::MAX)).min(wrap)
}

/// A partition in [`root_order`], with the most its volume may span
/// ([`volume_bound`], from the whole table).
#[derive(Clone, Copy, Default)]
pub struct Candidate {
    pub part: partition::Partition,
    pub bound: u64,
}

/// What [`Filesystem::first_mounting`] found: the first partition that
/// mounted, with its index, and the error of a read that failed in a probe
/// before it (or in any probe, when none mounted).
pub struct Probed {
    pub found: Option<(usize, Filesystem)>,
    pub failed: Option<Error>,
}

/// What [`Filesystem::mount`] fell back to when the root did not mount: for
/// the line that says so, which names the rule the root was looked for by.
pub struct Fallback {
    /// The root was a partition typed as one.
    pub typed_root: bool,
    /// What mounted instead is the partition the system booted from (else
    /// one of the untyped partitions, the boot partition not being known).
    pub boot_partition: bool,
}

/// Where a root is looked for on one disk ([`root_order`]): the partitions
/// in order, each with the length its table gives it, how many there are, and how many of the first of them are the
/// root's candidates. A partition past the candidates is a fallback, mounted
/// read-only (decision D5) so that the system comes up to be repaired with
/// nothing written to it, and never a partition `format` takes.
pub struct RootOrder {
    pub order: [Candidate; partition::MAX_PARTITIONS],
    pub len: usize,
    pub candidates: usize,
    /// Whether the candidates are partitions typed as a root.
    pub typed: bool,
}

impl RootOrder {
    /// The partitions `format` may lay a root on: the candidates, or every
    /// partition when there are none (a disk whose only partition is the
    /// boot one, or a table that names no root and no boot partition).
    pub fn format_targets(&self) -> &[Candidate] {
        if self.candidates > 0 { &self.order[..self.candidates] } else { &self.order[..self.len] }
    }

    /// Whether mounting the `i`-th partition of the order means the root
    /// did not mount.
    pub fn is_fallback(&self, i: usize) -> bool {
        self.candidates > 0 && i >= self.candidates
    }
}

/// `parts[..count]` in the order a root is looked for, each with its bound
/// ([`volume_bound`], the disk's `capacity` read once by the caller). One list, so
/// [`Filesystem::mount`] and `format` cannot order it differently (the
/// medium review of #288).
///
/// The partitions whose type marks them as a root ([`partition::Partition::
/// root`]: MBR 0x83, GPT's AArch64 root GUID) are the candidates, in table
/// order, when the disk has one: on a disk with an exFAT data partition
/// before the root, the root is `/`. Every other partition is then only a
/// fallback, read-only: the boot partition when the kernel names it, else
/// the untyped partitions in table order, since which of them is the ESP is
/// not known. So a typed root that does not mount never leaves a data
/// partition or the ESP as a writable `/`, and `format` takes the typed root,
/// whether the boot partition is known or not (the review of #295).
///
/// With no typed root, every partition but the boot one is a candidate, in
/// table order, as before the root was named by type (`espexfat.img`'s exFAT
/// root has a type, 0x07, that a data partition shares), and the boot
/// partition is the fallback. With no typed root and no boot partition named,
/// every partition is a candidate, as every one was then.
pub fn root_order(
    parts: &[partition::Partition; partition::MAX_PARTITIONS],
    count: usize,
    boot: Option<u64>,
    capacity: Option<u64>,
) -> RootOrder {
    let mut order = [Candidate::default(); partition::MAX_PARTITIONS];
    let candidate = |p: &partition::Partition| Candidate { part: *p, bound: volume_bound(p, &parts[..count], capacity) };
    let mut n = 0;
    let beside = |p: &&partition::Partition| Some(p.start) != boot;
    for p in parts[..count].iter().filter(beside).filter(|p| p.root) {
        order[n] = candidate(p);
        n += 1;
    }
    let typed = n > 0;
    if !typed || boot.is_none() {
        for p in parts[..count].iter().filter(beside).filter(|p| !p.root) {
            order[n] = candidate(p);
            n += 1;
        }
    }
    let candidates = if typed {
        parts[..count].iter().filter(beside).filter(|p| p.root).count()
    } else if boot.is_some() {
        n
    } else {
        0
    };
    for p in &parts[..count] {
        if Some(p.start) == boot {
            order[n] = candidate(p);
            n += 1;
        }
    }
    RootOrder { order, len: n, candidates, typed }
}

/// The piece loop of every `read_each`, once: `read(offset, piece)` reads the
/// file its caller resolved, and each piece goes to `sink` until `want` bytes
/// have, the file ends (a short or empty piece), or `sink` answers `Break`.
/// Returns the bytes read and whether `sink` stopped it. On the disk arms
/// nothing can change the file between pieces: fsd answers one request at a
/// time, the whole loop is one request, and only fsd writes its disks. A
/// `/proc` file is made anew each piece from kernel state that other cores
/// change meanwhile, so a read longer than one piece could mix two moments;
/// every `/proc` file is one line, far under a piece.
pub fn read_pieces(
    offset: u64,
    want: u64,
    buf: &mut [u8],
    mut read: impl FnMut(u64, &mut [u8]) -> Result<u32, Error>,
    mut sink: impl FnMut(&[u8]) -> ControlFlow<()>,
) -> Result<(u64, bool), Error> {
    let mut done = 0u64;
    while done < want && !buf.is_empty() {
        let len = ((want - done) as usize).min(buf.len());
        let n = read(offset + done, &mut buf[..len])? as usize;
        if n == 0 {
            break;
        }
        if sink(&buf[..n]).is_break() {
            return Ok((done, true));
        }
        done += n as u64;
        if n < len {
            break;
        }
    }
    Ok((done, false))
}
