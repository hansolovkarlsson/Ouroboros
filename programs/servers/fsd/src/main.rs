//! The filesystem server - the fifth userland program, and the first
//! real component moved out of the EL1 kernel (driver isolation part 2,
//! MINIX-style): it owns the FAT32 engine (`fat32.rs`, the kernel's old
//! module ported onto `disk.rs`'s `BLOCK_*` syscall shim), speaks the
//! `FSOP_*` request protocol to clients (see `syscall-abi`'s protocol
//! section - requests arrive as messages, normally via `MSG_CALL`), and
//! is the only task the kernel's `BLOCK_*` syscalls accept.
//!
//! Boot-loaded by the kernel (`loader::load_fsd`, `\EFI\ORBS\FSD.BIN`)
//! into task slot 2 (`syscall_abi::FSD_TASK`), which is
//! exit/kill-protected and never used by `spawn`. Same build shape as
//! `pong/`/`hello/`/`shell/`: `aarch64-unknown-none`, release-only,
//! shared linker script, constants from `syscall-abi`.
//!
//! **Its tables live in `main`'s own stack frame** - the mounted `Fs`
//! instances and the fid table, which works because this server is one
//! infinite loop that never returns. A few flags are statics (atomics, only
//! the spelling of a mutable static in a task of one thread):
//! `ROOT_READ_ONLY` here, `disk.rs`'s `CLAIMED`. The linker no longer
//! asserts `.data`/`.bss` empty (programs/linker.ld says why).
//!
//! **v2 protocol - fully self-contained, no client pointers.** The
//! original protocol passed raw pointers into the caller's memory,
//! which per-task page tables made impossible for this server to
//! dereference; requests now carry their payloads inline (header +
//! path/data bytes) and replies carry results inline (status + data),
//! all copied task-to-task by the kernel's message machinery - this
//! server only ever touches its own two buffers. The 512-byte
//! per-operation payload cap (`syscall_abi::FS_DATA_MAX`, the old
//! kernel `MAX_USER_LEN`) survives as the protocol's own.

#![no_std]
#![no_main]

mod disk;
mod exfat;
mod ext2;
mod fat32;
mod partition;
mod proc;
mod utf16;
mod vfs;

use core::arch::asm;
use core::ops::ControlFlow;
use core::sync::atomic::{AtomicBool, Ordering};
use core::panic::PanicInfo;

#[no_mangle]
#[link_section = ".text.start"]
pub extern "C" fn _start() -> ! {
    main()
}

fn main() -> ! {
    // The mount table (cluster Phase 0 multi-mount): several filesystems held at
    // once, indexed by the ninep-abi `tree` selector. Tree 0 is the boot
    // auto-mount; FSOP_MOUNT_AT fills the rest. Filesystem isn't Copy, so build
    // the array of `None`s with a const block rather than `[None; N]`.
    let mut mounts: [Option<vfs::Filesystem>; MAX_MOUNTS] = [const { None }; MAX_MOUNTS];
    // The open-file (fid) table - see the fid verbs. Lives here in main's frame
    // like `mounts`.
    let mut fids = [Fid::empty(); MAX_FIDS];
    // The block device, claimed here and nowhere else: every volume and disk
    // tool gets its handle from it, so no filesystem arm can make a disk
    // handle of its own (disk.rs's `Device`). Nothing runs before this line
    // to claim it first. If something ever did, fsd says so and spins, as it
    // does after a broken receive below, and the supervisor's wedge check
    // restarts it from its image. Whatever claimed first would claim first
    // again, so that ends when the restarts run out: the line says why.
    let Some(device) = disk::Device::claim() else {
        print("fsd: the block device was claimed before main - not serving\r\n");
        loop {
            core::hint::spin_loop();
        }
    };
    // Auto-mount if the kernel already holds a device (QEMU's boot-time
    // virtio path; on Parallels the device arrives later, via the
    // `mount` command -> MOUNT syscall -> FSOP_MOUNT request).
    try_mount(&device, &mut mounts);
    // The synthetic /proc filesystem (cluster Phase 3) always exists, at the
    // reserved PROC_TREE index - no disk, so it's just constructed here. The
    // shell's `mount -p` binds /proc to it; netd's export routes /proc paths to
    // it, so another machine can read this one's process table as files.
    mounts[ninep_abi::NS_PROC_TREE as usize] = Some(vfs::Filesystem::proc());
    print("fsd: filesystem server ready\r\n");

    // v2 protocol: requests arrive fully self-contained (header +
    // inline payload) and replies leave the same way (status + inline
    // result) - this server never dereferences a client pointer, which
    // is what makes it work at all under per-task page tables. Both
    // buffers live in this frame; the request is borrowed while the
    // reply is built, hence two separate arrays.
    let mut req = [0u8; syscall_abi::MSG_MAX_LEN as usize];
    let mut reply = [0u8; syscall_abi::MSG_MAX_LEN as usize];
    loop {
        let packed = syscall4::<{ syscall_abi::MSG_RECV }>(req.as_mut_ptr() as u64, req.len() as u64, 0, 0);
        if packed >= syscall_abi::FS_ERR_MIN {
            // RECV_INTERRUPTED can't reach a non-keyboard-owner task,
            // and no other error is expected - park rather than spin
            // on a broken call.
            break;
        }
        let sender = packed >> 32;
        // The call this request opened, read once, here, before anything
        // else can receive: the reply and every SAFECOPY name it.
        let call = syscall4::<{ syscall_abi::SENDER_CALL }>(0, 0, 0, 0);
        let len = ((packed & 0xffff_ffff) as usize).min(req.len());
        let reply_len = handle(&device, &mut mounts, &mut fids, sender, call, &req[..len], &mut reply);
        // Delivered only while the caller is still in the call: an answer to
        // a call that is over (cut short, or its caller dead and the slot
        // refilled) goes nowhere, REPLY_STALE, and a fid it carried is freed.
        let r = syscall4::<{ syscall_abi::MSG_REPLY }>(call, reply.as_ptr() as u64, reply_len as u64, 0);
        if r == syscall_abi::REPLY_STALE {
            forget_unheard_fid(&mut fids, &req[..len], &reply[..reply_len]);
        }
    }
    loop {
        core::hint::spin_loop();
    }
}

/// Whether tree 0 is the boot partition mounted read-only because the root
/// beside it did not mount (decision D5, docs/roadmap/roadmap-one-disk.md):
/// the system comes up to be looked at and repaired, and nothing is written
/// to the ESP, by root either. Set by [`try_mount`] when it falls back,
/// cleared by every other mount of tree 0 and by `FSOP_UNMOUNT`. fsd is one
/// task, so an atomic is only the spelling of a mutable static.
static ROOT_READ_ONLY: AtomicBool = AtomicBool::new(false);

/// Whether `tree`, mounted as `fs`, is read-only: tree 0 as D5's fallback, or
/// any tree whose arm answers [`vfs::Filesystem::read_only`] (exFAT's
/// read-only volume, `/proc`). `fs` is a required parameter so no caller can
/// ask without the volume (the high review of #361).
fn tree_read_only(tree: usize, fs: &vfs::Filesystem) -> bool {
    (tree == 0 && ROOT_READ_ONLY.load(Ordering::Relaxed)) || fs.read_only().is_some()
}

/// An `NP_OPEN`'s path and what its one stat of it found: `Ok(true)` the file
/// exists, `Ok(false)` it is missing, `Err` the status of any other failure (an
/// I/O error, a path through a file), and `dir` whether what exists is a
/// directory (false when nothing does). Made once in `handle_ninep`, before
/// [`check_access`], and read by it, by [`refused_read_only`] and by the
/// handler, so the three cannot disagree on whether the open creates: a stat
/// each let a transient I/O error authorize a create on the parent and the
/// handler touch a file that existed, and cost every open a second walk (the
/// second medium review of #364). It carries the path it was made from, so
/// the request's path is parsed once too (the third medium review of #364
/// found four copies of the parse).
#[derive(Clone, Copy)]
struct Found<'a> {
    path: &'a str,
    kind: Result<Kind, u64>,
}

/// What a [`Found`]'s stat found at its path, or that nothing is there. One
/// value, so "a directory" cannot be written beside "missing" or a failed
/// stat (the medium review of #370: a `dir: bool` beside the stat's result
/// could be).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Missing,
    File,
    Dir,
}

impl Kind {
    fn exists(self) -> bool {
        self != Kind::Missing
    }
}

/// The [`Found`] for `path` on `fs`.
fn found<'a>(fs: &mut vfs::Filesystem, path: &'a str) -> Found<'a> {
    let kind = match fs.stat(path) {
        Ok(st) if st.is_dir => Ok(Kind::Dir),
        Ok(_) => Ok(Kind::File),
        Err(fat32::Error::NotFound) => Ok(Kind::Missing),
        Err(e) => Err(error_code(&e)),
    };
    Found { path, kind }
}

/// The status `verb` on `tree`, mounted as `fs`, is refused with because that
/// tree is read-only ([`tree_read_only`]), or `None` when it goes on: the one
/// spelling of the gate, for `handle_ninep` and the fid path alike (the second
/// high review of #291 found the predicate written out twice). A change is
/// refused here before it reaches the arm, whose own refusal (exFAT's disk
/// gate, `proc.rs`'s `ReadOnly`) stays as the second line.
///
/// DEFAULT-DENY: the verbs that only read are listed, and every other verb,
/// one added later included, counts as a change, so a new verb cannot reach a
/// read-only tree unrefused for want of an arm here (the review of #291, the
/// unspellable-postmortem's rule). The verb is asked first, so a read pays
/// nothing for the tree's state. An `NP_OPEN` is a change by
/// [`ninep_abi::open_changes`], which needs to know whether the file exists
/// when the open would create it: `open` is the request's [`Found`] (`None` on
/// the fid path, where no `NP_OPEN` comes, and refused if one did). A missing file whose directory
/// exists answers EROFS; a missing directory answers its own error, ENOENT,
/// as on a writable tree, and a "directory" that is a file ENOTDIR; any other
/// failure answers its own status (an I/O error is not "read-only", the
/// second and third medium reviews of #364).
fn refused_read_only(tree: usize, fs: &mut vfs::Filesystem, verb: u64, p: &[u64; 4], open: Option<Found>) -> Option<u64> {
    const READ_ONLY: Option<u64> = Some(syscall_abi::FS_ERR_READ_ONLY);
    match verb {
        ninep_abi::NP_READDIR
        | ninep_abi::NP_READ_FILE
        | ninep_abi::NP_READ
        | ninep_abi::NP_READ_AT
        | ninep_abi::NP_STAT
        | ninep_abi::NP_PREAD
        | ninep_abi::NP_FSTAT
        | ninep_abi::NP_CLUNK => None,
        _ if !tree_read_only(tree, fs) => None,
        ninep_abi::NP_OPEN => {
            let flags = p[0];
            if !ninep_abi::open_changes(flags, false) {
                return None; // changes nothing, missing or not
            }
            if ninep_abi::open_changes(flags, true) {
                return READ_ONLY; // changes the file even where it exists
            }
            // Creates, and only when the file is missing.
            let Some(Found { path, kind }) = open else {
                return READ_ONLY;
            };
            match kind.map(Kind::exists) {
                Ok(true) => None,
                Ok(false) => match fs.stat(parent_of(path)) {
                    Ok(st) if st.is_dir => READ_ONLY,
                    Ok(_) => Some(syscall_abi::FS_ERR_NOT_A_DIRECTORY),
                    Err(e) => Some(error_code(&e)),
                },
                Err(code) => Some(code),
            }
        }
        _ => READ_ONLY,
    }
}

/// Attempts the auto-mount into tree 0 if the kernel has a device
/// installed. Quiet when there's simply no device (the ordinary
/// Parallels-before-`mount` case); logs the outcome whenever a device is
/// actually present. Returns `0` (mounted), [`MOUNT_ALREADY`] (tree 0 is
/// full, or the partition it found is already another tree's, see
/// [`tree_of`]), [`FS_ERR_IO`] (the partition table could not be read: the
/// disk is there and failing, which is not "no filesystem"), or [`NO_FS`].
///
/// [`MOUNT_ALREADY`]: syscall_abi::MOUNT_ALREADY
/// [`FS_ERR_IO`]: syscall_abi::FS_ERR_IO
/// [`NO_FS`]: syscall_abi::NO_FS
fn try_mount(device: &disk::Device, mounts: &mut [Option<vfs::Filesystem>; MAX_MOUNTS]) -> u64 {
    if mounts[0].is_some() {
        return syscall_abi::MOUNT_ALREADY;
    }
    if device.disk().capacity_sectors().is_err() {
        return syscall_abi::NO_FS;
    }
    match vfs::Filesystem::mount(device) {
        Ok((mounted, ..)) if tree_of(mounts, &mounted).is_some() => syscall_abi::MOUNT_ALREADY,
        Ok((mounted, fallback, unread)) => {
            // One write ([`con_write_parts`]), the read-only reason in it.
            // When the disk has a partition beside the boot one, the root on one
            // disk (one-disk step 4), and it did not mount, the line says so
            // first rather than run on the ESP's tree without a word.
            // D5: the fallback is read-only, so nothing is written to the ESP.
            ROOT_READ_ONLY.store(fallback.is_some(), Ordering::Relaxed);
            // Which rule the root was looked for by, and what came up instead:
            // with a typed root, the untyped partitions beside it were never
            // tried, so "none beside the boot partition mounted" would send
            // whoever reads it after the wrong partition (the review of #295).
            let fallback = match fallback {
                None => "",
                Some(vfs::Fallback { typed_root: true, boot_partition: true }) => {
                    "fsd: the partition typed as the root did not mount - the boot partition is `/`, read-only\r\n"
                }
                Some(vfs::Fallback { typed_root: true, boot_partition: false }) => {
                    "fsd: the partition typed as the root did not mount - an untyped partition is `/`, read-only\r\n"
                }
                Some(vfs::Fallback { typed_root: false, .. }) => {
                    "fsd: no partition beside the boot partition mounted - the boot partition is `/`, read-only\r\n"
                }
            };
            // A partition tried before it would not read: said, since what
            // it held is not known (a failing disk, not an empty partition).
            let unread = if unread { "fsd: a partition before it would not read (disk I/O error)\r\n" } else { "" };
            let (ro, why, end) = read_only_line(&mounted);
            con_write_parts(&[unread, fallback, "fsd: ", mounted.name(), " mounted, disk commands available\r\n", ro, why, end]);
            mounts[0] = Some(mounted);
            0
        }
        Err(e) => {
            // One write, as the line above, so no other task's line lands
            // inside it (accountd's did, in `make test-probe-ioerr`).
            // A disk with no partition at all, a blank one the kernel installed
            // since 2026-10-23 among them, is said with what prepares it:
            // "disk commands won't work" and `mount -a`'s question about a
            // stick both point away from `partition` (the max review).
            let mut parts = [partition::Partition::default(); partition::MAX_PARTITIONS];
            let empty = matches!(e, fat32::Error::NoFat32Partition)
                && matches!(partition::discover(&mut device.disk(), &mut parts), Ok(0));
            let tail = if empty {
                ") - the disk has no partition: `partition` then `format` prepare it\r\n"
            } else {
                ") - disk commands won't work\r\n"
            };
            con_write_parts(&["fsd: mount failed (", error_name(&e), tail]);
            match e {
                fat32::Error::Io(_) => syscall_abi::FS_ERR_IO,
                _ => syscall_abi::NO_FS,
            }
        }
    }
}

/// `mount -a`: mount tree 0 from the block device the kernel holds, and when
/// that finds no filesystem, have the kernel rescan the USB ports and install a
/// stick in its place (the `MOUNT` syscall, replace set), then try again. The
/// replace is passed only with no disk filesystem mounted, here where that is
/// known: a swap under a mounted one would hand its cached geometry another
/// disk. Since 2026-10-19 the kernel answers `MOUNT` to this server alone
/// (`CAP_BLOCK`); until then the shell called it on fsd's earlier word, and
/// so could any task, on nobody's.
///
/// Not on an I/O error: a disk whose partition table would not read is there
/// and failing, and swapping in whatever stick is attached would put the stick
/// at `/` with the real disk out of reach until a reboot. The same for a read
/// error inside a partition's probe when no partition mounts
/// (`vfs::Filesystem::first_mounting`, since 2026-10-20; until then the probe
/// read it as "no filesystem"). Answers `try_mount`'s codes,
/// `MOUNT_NO_DEVICE` when the rescan found no stick, or the kernel's own
/// refusal of the rescan as it came (`BLOCK_ERR_DENIED`: this task without
/// `CAP_BLOCK`, a policy fault the shell names).
fn mount_or_rescan(device: &disk::Device, mounts: &mut [Option<vfs::Filesystem>; MAX_MOUNTS]) -> u64 {
    let first = try_mount(device, mounts);
    if first != syscall_abi::NO_FS {
        return first;
    }
    match device.replace(mounts) {
        Some(0) => try_mount(device, mounts),
        Some(other) => other,
        None => first,
    }
}

/// The tree that already holds `candidate`'s partition, if any. A partition
/// is mounted at most once (since 2026-10-05): two trees over one partition
/// would be two filesystems over one set of on-disk structures, each with its
/// own memory of them (FAT32's FAT-sector cache and read cursor), and a write
/// through one would leave the other's stale. The synthetic `/proc` has no
/// partition and never matches.
fn tree_of(mounts: &[Option<vfs::Filesystem>; MAX_MOUNTS], candidate: &vfs::Filesystem) -> Option<usize> {
    if !candidate.is_disk() {
        return None;
    }
    mounts.iter().position(|m| {
        m.as_ref().is_some_and(|m| m.is_disk() && m.partition_lba() == candidate.partition_lba())
    })
}

/// Zero the disk's first `sectors` 512-byte sectors (`0` -> the default
/// [`ERASE_DEFAULT_SECTORS`]), clamped to the disk's capacity. The
/// destructive first step of preparing a blank disk: it removes the
/// partition table and any filesystem metadata near the start, so a
/// subsequent partition/format starts clean. Returns `0`,
/// [`MOUNT_NO_DEVICE`] (no block device), or [`FS_ERR_IO`].
fn erase_disk(device: &disk::Device, sectors: u64) -> u64 {
    let Ok(capacity) = device.disk().capacity_sectors() else {
        return syscall_abi::MOUNT_NO_DEVICE;
    };
    let want = if sectors == 0 {
        syscall_abi::ERASE_DEFAULT_SECTORS
    } else {
        sectors
    };
    let count = want.min(capacity);
    device.table_rewritten();
    let mut disk = device.disk();
    let zero = [0u8; 512];
    let mut lba = 0u64;
    while lba < count {
        if disk.write_sector(lba, &zero).is_err() {
            return syscall_abi::FS_ERR_IO;
        }
        lba += 1;
    }
    0
}

/// Write a fresh MBR with a single primary partition of type `type_byte`
/// (`0` -> `0x0C`, FAT32-LBA) spanning the disk from
/// [`PARTITION_START_LBA`] to the end. Only LBA 0 is written; the
/// partition's contents are left for a later format. Returns `0`,
/// [`MOUNT_NO_DEVICE`], [`FS_ERR_DISK_FULL`] (disk smaller than the 1 MiB
/// alignment start), or [`FS_ERR_IO`].
fn partition_disk(device: &disk::Device, type_byte: u8) -> u64 {
    let Ok(capacity) = device.disk().capacity_sectors() else {
        return syscall_abi::MOUNT_NO_DEVICE;
    };
    let start = syscall_abi::PARTITION_START_LBA;
    if capacity <= start {
        return syscall_abi::FS_ERR_DISK_FULL;
    }
    // MBR partitioning is a 32-bit LBA scheme; a partition past 2 TiB
    // can't be expressed here (that's what GPT, a later step, is for).
    let part_sectors = (capacity - start).min(u32::MAX as u64) as u32;
    device.table_rewritten();
    let ptype = if type_byte == 0 { 0x0C } else { type_byte };

    let mut mbr = [0u8; 512];
    // One partition entry at the classic offset 0x1BE (16 bytes):
    //   [0]      boot flag (0x00, not bootable)
    //   [1..4]   CHS start - 0xFE/0xFF/0xFF, the "use LBA, CHS invalid" marker
    //   [4]      partition type byte
    //   [5..8]   CHS end - same LBA marker
    //   [8..12]  LBA start (LE)
    //   [12..16] sector count (LE)
    let e = 0x1BE;
    mbr[e] = 0x00;
    mbr[e + 1] = 0xFE;
    mbr[e + 2] = 0xFF;
    mbr[e + 3] = 0xFF;
    mbr[e + 4] = ptype;
    mbr[e + 5] = 0xFE;
    mbr[e + 6] = 0xFF;
    mbr[e + 7] = 0xFF;
    mbr[e + 8..e + 12].copy_from_slice(&(start as u32).to_le_bytes());
    mbr[e + 12..e + 16].copy_from_slice(&part_sectors.to_le_bytes());
    // Boot signature.
    mbr[510] = 0x55;
    mbr[511] = 0xAA;

    if device.disk().write_sector(0, &mbr).is_err() {
        return syscall_abi::FS_ERR_IO;
    }
    0
}

/// The partition `format` lays a root on (start LBA, sector count): the
/// root by `vfs::mount`'s own order ([`vfs::root_order`]), from the one
/// partition parser ([`partition::discover`]), sized by the bound `mount`
/// checks a volume against ([`vfs::volume_bound`]: the table's length cut to
/// the next partition, the disk and sector 2^32), so the volume laid is one
/// `mount` takes. `None` on a GPT disk, which this MBR-only formatter
/// refuses, when the disk has no partition, or when the table gives the
/// partition no size;
/// [`fat32::Error::Io`] when the table, or the probe of a candidate before
/// the one chosen, would not read: a partition that could not be read is not
/// shown to be empty, and neither it nor a later candidate is formatted on
/// that word (the review of #329: the next candidate, an ESP among them when
/// the boot partition is not known, was formatted in its place).
fn find_partition(device: &disk::Device) -> Result<Option<(u32, u32)>, fat32::Error> {
    // A GPT disk, by the test `mount` reads it with: this formatter writes
    // MBR partitions only, and on a GPT disk it would lay the filesystem from
    // LBA 1 over the GPT and every partition in it, the ESP included (the
    // medium review of #288). Refused.
    if partition::is_gpt(&mut device.disk())? {
        return Ok(None);
    }
    // The partitions `mount` sees, from the one parser it uses, so `format`
    // never picks an entry `mount` would not list (one starting at LBA 0,
    // the MBR itself), each with its size.
    let mut parts = [partition::Partition::default(); partition::MAX_PARTITIONS];
    let count = partition::discover(&mut device.disk(), &mut parts)?;
    // The root by `vfs::mount`'s rule, so `format` writes over what `/`
    // showed: among the candidates beside the one the system booted from
    // (a partition typed as a root, else every other one), the first holding
    // a filesystem, else the first of them (one just made, or a typed root
    // too damaged to probe);
    // the boot partition only when the disk has no other. Never a fallback
    // while a candidate is there, even when the candidate holds no
    // filesystem: a damaged root beside the ESP is formatted, not the ESP
    // (the third review of #288 found the probe reaching it), and a damaged
    // typed root is formatted, not a data partition or an ESP beside it,
    // whether or not the boot partition is known (the review of #295).
    let boot = device.boot_partition_start();
    let roots = vfs::root_order(&parts, count, boot, device.disk().capacity_sectors().ok());
    let candidates = roots.format_targets();
    let probed = vfs::Filesystem::first_mounting(device, candidates);
    if let Some(e) = probed.failed {
        return Err(e);
    }
    let probing = probed.found.map(|(i, _)| candidates[i]);
    // `target` is a vfs::Candidate: the partition and its bound.
    let Some(target) = probing.or(candidates.first().copied()) else {
        return Ok(None);
    };
    // The size from the same parse that chose the partition, not a second
    // read of the table (the review of #296), as the bound `mount` will check
    // the volume against (the review of #330). A table that gives no size is
    // refused, as it always was.
    let sectors = (target.part.sectors != 0).then_some(target.bound).and_then(|s| u32::try_from(s).ok());
    Ok(u32::try_from(target.part.start).ok().zip(sectors))
}

/// Lay a fresh filesystem of type `fstype` into the disk's root partition
/// (mkfs), found by `mount`'s own rule ([`find_partition`]); a GPT disk is
/// refused as having no partition this can format. FAT32, exFAT, and ext2 are all supported (milestone 3);
/// any other `fstype` returns [`FS_ERROR`]. Returns `0`, [`MOUNT_NO_DEVICE`],
/// [`FS_ERR_NOT_FOUND`] (no partition - run `partition` first),
/// [`FS_ERR_DISK_FULL`] (partition too small), or [`FS_ERR_IO`] (also when
/// the partition table or the probe would not read).
fn format_disk(device: &disk::Device, fstype: u64) -> u64 {
    if device.disk().capacity_sectors().is_err() {
        return syscall_abi::MOUNT_NO_DEVICE;
    }
    let (start, sectors) = match find_partition(device) {
        Ok(Some(found)) => found,
        Ok(None) => return syscall_abi::FS_ERR_NOT_FOUND,
        Err(_) => return syscall_abi::FS_ERR_IO,
    };
    let result = match fstype {
        syscall_abi::FMT_FAT32 => fat32::Fs::format(device.disk(), start, sectors),
        syscall_abi::FMT_EXFAT => exfat::Fs::format(device.disk(), start, sectors),
        syscall_abi::FMT_EXT2 => ext2::Fs::format(device.disk(), start, sectors),
        _ => return syscall_abi::FS_ERROR,
    };
    match result {
        Ok(()) => 0,
        Err(fat32::Error::DiskFull) => syscall_abi::FS_ERR_DISK_FULL,
        Err(_) => syscall_abi::FS_ERR_IO,
    }
}

const REQ_PAYLOAD: usize = syscall_abi::FS_REQ_PAYLOAD as usize;
const REPLY_PAYLOAD: usize = syscall_abi::FS_REPLY_PAYLOAD as usize;
const DATA_MAX: usize = syscall_abi::FS_DATA_MAX as usize;

/// How many filesystems fsd can hold mounted at once (cluster Phase 0
/// multi-mount), indexed by the ninep-abi `tree` selector. Tree 0 is the boot
/// auto-mount; `FSOP_MOUNT_AT` fills the middle; the reserved top slot
/// (`NS_PROC_TREE`) is the synthetic `/proc` (cluster Phase 3), so the table is
/// sized to include it. Bounded like every fixed table here (netd's
/// `MAX_CONNS`, the VFS's `MAX_PARTITIONS`).
const MAX_MOUNTS: usize = ninep_abi::NS_PROC_TREE as usize + 1;

// --- fids: server-side open-file handles (see ninep-abi's NP_OPEN..NP_CLUNK) -
/// How many files can be open at once, across all clients. Small and fixed - the
/// table lives on `main`'s stack frame, and fsd's
/// stack is guard-page-bounded, so this stays modest. If it fills, `NP_OPEN`
/// first reaps fids whose owner is gone (`reap_dead_fids`), then fails. The
/// value is `ninep-abi`'s since 2026-09-12: it is a budget the export's
/// sessions share and the host peers observe, so it is pinned there.
const MAX_FIDS: usize = ninep_abi::MAX_FIDS;
/// Longest path a fid remembers (bounded, no heap). Enough for the paths a C
/// program opens on the boot disk; a longer path fails `NP_OPEN`.
const FID_PATH_MAX: usize = 96;
/// Fids are numbered from 3 so they never collide with a C program's reserved
/// stdin/stdout/stderr (0/1/2) and can be used directly as its fd.
const FID_BASE: u64 = 3;

/// One open file: which client owns it, which mount, and the path (fsd is
/// path-based internally, so the fid re-resolves the path each op).
#[derive(Clone, Copy)]
struct Fid {
    used: bool,
    /// The PACKED TASK IDENTITY (`SENDER_TASK`: slot and generation) of the
    /// task that opened the fid, never the bare slot. A slot is a position and
    /// is recycled the moment its task is reaped: the shell reuses the same one
    /// for every foreground command, and the supervisor reinstalls a crashed
    /// `netd` into the same protected slot. With a slot here, a fid leaked by a
    /// task that exited without `NP_CLUNK` matched the next occupant's requests
    /// and could not be reaped while that occupant lived (`reap_dead_fids`
    /// asked `TASK_STATE`, and the new occupant is alive), and a restarted
    /// `netd` could neither be told apart from the old one nor reaped. The
    /// generation makes both visible: a later occupant's request fails the
    /// ownership test, and the reaper frees any fid whose slot now answers a
    /// different identity. The kernel captures this at send time, exactly as it
    /// captures the credential `owner_uid` is compared against.
    owner: u64,
    /// The access the fid was OPENED for (`OPEN_READ`/`OPEN_WRITE`/...).
    ///
    /// Load-bearing. The fid ops skip the per-op permission check because the
    /// fid was authorized once at open - which is only sound if the fid also
    /// remembers *what it was authorized for*. Without this a read-only open of
    /// a root-owned 0644 file (`/etc/passwd`) returned a fid that `NP_PWRITE`
    /// would write through, since the client picks the verb and nothing
    /// re-checked.
    flags: u64,
    /// The uid the fid was opened AS: the resolved caller's, which for `netd`
    /// is the remote user it proxied, not `netd`'s own.
    ///
    /// Part of the ownership test, with `owner`, for every fid verb including
    /// `NP_CLUNK`, but ONLY when the owner is `NET_TASK`. It is what separates
    /// two remote users behind that one identity: `netd` multiplexes every
    /// session through one mailbox, so every remote fid has the same `owner`,
    /// and without this a remote user could stat, read or clunk another's fid
    /// by naming it. A mismatch is refused and the fid is KEPT. It used to be
    /// dropped, on the reading that a uid change meant a recycled slot, which
    /// was the only way a slot-keyed `owner` could notice one; the generation
    /// in `owner` now notices that directly, and on a session the drop let a
    /// co-tenant destroy a handle by naming it. For any other owner the
    /// identity is already unique, and POSIX keeps a descriptor usable across
    /// a privilege drop (`SET_ID`), so comparing the uid there would strand
    /// the fid for the task's lifetime (review of #130).
    owner_uid: u32,
    tree: usize,
    /// Its filesystem was unmounted under it (`FSOP_UNMOUNT`). The slot stays
    /// `used`, so its number is not handed to the next opener while the
    /// owner may still use it: freeing it let a remote session's cached fid,
    /// or a C program's old descriptor, reach whoever opened next (the
    /// review of #214). Every op on it answers `NO_FS`; `NP_CLUNK` frees it,
    /// as does the reaper once its owner is gone.
    dead: bool,
    path_len: usize,
    path: [u8; FID_PATH_MAX],
}

impl Fid {
    const fn empty() -> Self {
        Fid { used: false, owner: 0, flags: 0, owner_uid: 0, tree: 0, dead: false, path_len: 0, path: [0u8; FID_PATH_MAX] }
    }
    fn path_str(&self) -> Option<&str> {
        core::str::from_utf8(&self.path[..self.path_len]).ok()
    }
}

/// Decodes and executes one request from this server's own receive buffer,
/// building the reply (status + inline result) in its own reply buffer. Returns
/// the reply's total length. Every slice below is into `req`/`reply` -
/// server-owned memory only. `sender` is the calling task, needed by the bulk
/// ops (`NP_READ`/`NP_WRITE`/`NP_WRITE_AT`) to `SAFECOPY` against the client's
/// grant (they move their data directly between task regions rather than inline
/// in the reply).
fn handle(device: &disk::Device, mounts: &mut [Option<vfs::Filesystem>; MAX_MOUNTS], fids: &mut [Fid; MAX_FIDS], sender: u64, call: u64, req: &[u8], reply: &mut [u8]) -> usize {
    if req.len() < REQ_PAYLOAD {
        return status_reply(reply, syscall_abi::FS_ERROR);
    }
    let op = read_u64(req, 0);

    // WHO this request is authorized as, resolved ONCE here so that every check
    // in it sees one identity. For an ordinary client that is the credential the
    // kernel bound at send; for `netd` it is the remote user stated in the
    // request's `a3` word. `None` means "no identity to authorize against", and
    // every consumer fails closed on it. See `effective_caller`.
    let who = effective_caller(op, sender, req);

    // File operations travel over the uniform verb set (ninep-abi, the Phase 0
    // cluster protocol), carrying a `tree` selector that picks which mount -
    // dispatched to handle_ninep. What remains below is the FSOP_* disk-
    // management control ops (mount/unmount/erase/partition/format, plus
    // FSOP_MOUNT_AT - fsd-specific, not uniform file verbs) plus the SYSOP_PING
    // fall-through.
    if (ninep_abi::NP_BASE..ninep_abi::NP_LIMIT).contains(&op) {
        // WHICH TASK sent it, as the kernel's packed identity (slot and
        // generation), captured at send time like `who`. The file verbs take
        // only this and derive the slot from it where a grant needs one, so a
        // slot can never be passed where an identity belongs. Read here rather
        // than beside `who`: the FSOP_* control ops below never need it.
        let owner = syscall4::<{ syscall_abi::SENDER_TASK }>(0, 0, 0, 0);
        return handle_ninep(mounts, fids, owner, call, who, op, req, reply);
    }
    let p = [read_u64(req, 8), read_u64(req, 16), read_u64(req, 24), read_u64(req, 32)];

    if op == syscall_abi::FSOP_MOUNT {
        // ROOT ONLY. check_access guards the NP_* file verbs; these FSOP_*
        // control ops are dispatched here and were guarded by nothing at all -
        // so any user could attach or detach a volume. Attaching one matters as
        // much as detaching: a user could mount a partition deliberately left
        // unmounted (unenforced entirely on FAT/exFAT), or an attacker-authored
        // ext2 image carrying chosen uid/gid/mode metadata.
        if !is_root(&who) {
            return status_reply(reply, syscall_abi::FS_ERR_PERM);
        }
        // The auto-mount at tree 0 (the default the shell's `mount -a`
        // triggers); `try_mount` answers MOUNT_ALREADY when tree 0 is full.
        return status_reply(reply, mount_or_rescan(device, mounts));
    }

    if op == syscall_abi::FSOP_MOUNT_AT {
        // ROOT ONLY - same reasoning as FSOP_MOUNT above.
        if !is_root(&who) {
            return status_reply(reply, syscall_abi::FS_ERR_PERM);
        }
        // Multi-mount: mount the p[0]-th partition into a fresh tree slot and
        // return the tree id, so a client can bind a namespace prefix to it.
        // A partition that is already a tree is not mounted again: its tree
        // id is returned, so every shell that mounts it binds the same tree
        // (`tree_of`). A refusal here would shut every other shell out of a
        // partition one shell has mounted, until the next `unmount`.
        //
        // Tree 0 is the root mount's alone, and must be there first: a
        // partition mount while `/` is unmounted could take slot 0 (and so
        // become every task's `/`), or hold the partition the next `mount -a`
        // picks (the review of #213). So refused while tree 0 is empty, and
        // a new tree never goes in slot 0.
        if mounts[0].is_none() {
            return status_reply(reply, syscall_abi::NO_FS);
        }
        let index = p[0] as usize;
        let fs = match vfs::Filesystem::mount_partition(device, index) {
            Ok(fs) => fs,
            // A partition that would not read is a failing disk, not "no
            // filesystem" (the review of #329).
            Err(fat32::Error::Io(_)) => return status_reply(reply, syscall_abi::FS_ERR_IO),
            Err(_) => return status_reply(reply, syscall_abi::NO_FS),
        };
        if let Some(existing) = tree_of(mounts, &fs) {
            return status_reply(reply, existing as u64);
        }
        let Some(tree) = (1..MAX_MOUNTS).find(|&t| mounts[t].is_none()) else {
            return status_reply(reply, syscall_abi::MOUNT_ALREADY); // no free slot
        };
        // `mount N` prints nothing when it mounts; a read-only one says why,
        // or its first write's refusal would be the first word of it.
        if fs.read_only().is_some() {
            let (ro, why, end) = read_only_line(&fs);
            con_write_parts(&[ro, why, end]);
        }
        mounts[tree] = Some(fs);
        status_reply(reply, tree as u64)
    } else if op == syscall_abi::FSOP_MOUNT_INFO {
        // Report tree 0 (the primary mount), as ever. Handles the not-mounted
        // case itself (NO_FS).
        let Some(mounted) = mounts[0].as_mut() else {
            return status_reply(reply, syscall_abi::NO_FS);
        };
        let part_lba = mounted.partition_lba() as u64;
        // Whether this filesystem can enforce permissions is derived HERE, from
        // the root's stat - the server is the only party that can answer it.
        // The shell used to answer it by string-matching the format name, which
        // is a security decision made by string comparison.
        let mut flags = 0u64;
        if models_modes_or_unknown(mounted) == Some(true) {
            flags |= syscall_abi::MOUNT_FLAG_ENFORCES_MODES;
        }
        let capacity = device.disk().capacity_sectors().unwrap_or(0);
        let name = mounted.name().as_bytes();
        reply[0..8].copy_from_slice(&0u64.to_le_bytes());
        reply[8..16].copy_from_slice(&part_lba.to_le_bytes());
        reply[16..24].copy_from_slice(&capacity.to_le_bytes());
        reply[24..32].copy_from_slice(&flags.to_le_bytes());
        reply[32..32 + name.len()].copy_from_slice(name);
        32 + name.len()
    } else if op == syscall_abi::FSOP_UNMOUNT {
        // ROOT ONLY - taking the disk away is at least as privileged as writing
        // to it. Confirmed reachable: `unmount` as an ordinary user succeeded.
        if !is_root(&who) {
            return status_reply(reply, syscall_abi::FS_ERR_PERM);
        }
        // Unmount EVERY disk tree, not just tree 0 (since 2026-10-05). Trees
        // above 0 have no unmount of their own, so clearing only tree 0 left
        // them for the boot: the disk tools stayed refused, and a `mount -a`
        // that replaced the device left them describing the old disk over
        // the new one, a write through them landing at the old disk's
        // offsets. The synthetic /proc tree stays. A fid on a now-empty tree
        // is marked dead, not freed (see `Fid::dead`): it holds a tree number
        // and a path, and the slot may later hold another partition.
        let mut any = false;
        ROOT_READ_ONLY.store(false, Ordering::Relaxed);
        for m in mounts.iter_mut() {
            if m.as_ref().is_some_and(|fs| fs.is_disk()) {
                *m = None;
                any = true;
            }
        }
        if !any {
            return status_reply(reply, syscall_abi::NO_FS);
        }
        for fid in fids.iter_mut() {
            if fid.used && mounts[fid.tree].is_none() {
                fid.dead = true;
            }
        }
        status_reply(reply, 0)
    } else if op == syscall_abi::FSOP_ERASE
        || op == syscall_abi::FSOP_PARTITION
        || op == syscall_abi::FSOP_FORMAT
    {
        // ROOT ONLY - these rewrite the raw disk.
        if !is_root(&who) {
            return status_reply(reply, syscall_abi::FS_ERR_PERM);
        }
        // Raw-disk ops (erase/partition/format) rewrite disk structures, so they
        // refuse while *any* tree is mounted - unmount everything first.
        // Disk trees only: the synthetic /proc tree is always there, and
        // counting it refused these tools every time since /proc landed (the
        // review of #213).
        if mounts.iter().flatten().any(|m| m.is_disk()) {
            return status_reply(reply, syscall_abi::MOUNT_ALREADY);
        }
        let status = match op {
            syscall_abi::FSOP_ERASE => erase_disk(device, p[0]),
            syscall_abi::FSOP_PARTITION => partition_disk(device, p[0] as u8),
            _ => format_disk(device, p[0]),
        };
        status_reply(reply, status)
    } else {
        // Anything else - a stray/legacy op, or the supervisor's SYSOP_PING. A
        // bare status reply is right for all of them (for the ping, that reply,
        // addressed back to the kernel sender, is itself the liveness ack).
        status_reply(reply, syscall_abi::FS_ERROR)
    }
}

/// Decodes and executes one uniform-verb request (`ninep-abi`, the Phase 0
/// cluster protocol) - the sibling of [`handle`]'s FSOP dispatch. The wire is
/// the same shape with the `tree` selector at offset 8, so params sit at
/// 16/24/32/40 and the payload at [`NP_REQ_PAYLOAD`], one word later than
/// FSOP's. Each arm calls the **same** `vfs` method (and the same
/// grant/safecopy bulk path) as the FSOP op it replaces, so results are
/// byte-identical. `tree` must be `0` for now (single mount); the per-task
/// namespace resolves it to a real mount in a later step.
///
/// [`NP_REQ_PAYLOAD`]: ninep_abi::NP_REQ_PAYLOAD
#[allow(clippy::too_many_arguments)]
fn handle_ninep(mounts: &mut [Option<vfs::Filesystem>; MAX_MOUNTS], fids: &mut [Fid; MAX_FIDS], owner: u64, call: u64, who: Option<Caller>, verb: u64, req: &[u8], reply: &mut [u8]) -> usize {
    const NP_HDR: usize = ninep_abi::NP_REQ_PAYLOAD as usize;
    if req.len() < NP_HDR {
        return status_reply(reply, syscall_abi::FS_ERROR);
    }
    let tree = read_u64(req, 8) as usize;
    let p = [read_u64(req, 16), read_u64(req, 24), read_u64(req, 32), read_u64(req, 40)];
    let payload = &req[NP_HDR..];
    // Fid ops (PREAD/PWRITE/FSTAT/CLUNK) resolve their mount from the fid (opened
    // earlier), not the request's tree, and skip the per-op permission check -
    // the fid was authorized once at NP_OPEN (POSIX semantics). NP_OPEN itself
    // falls through to the normal path below (it has a tree + path + perm check).
    if matches!(
        verb,
        ninep_abi::NP_PREAD | ninep_abi::NP_PWRITE | ninep_abi::NP_FSTAT | ninep_abi::NP_CLUNK
    ) {
        return handle_fid_op(mounts, fids, owner, call, who, verb, &p, reply);
    }
    // The `tree` selector picks which mount (cluster Phase 0 multi-mount); an
    // out-of-range tree is a client bug. An unmounted tree replies NO_FS.
    if tree >= MAX_MOUNTS {
        return status_reply(reply, syscall_abi::FS_ERROR);
    }
    let Some(fs) = mounts[tree].as_mut() else {
        return status_reply(reply, syscall_abi::NO_FS);
    };
    // An NP_OPEN's path and its one stat ([`Found`]), read by `check_access`,
    // the gate and the handler below. A malformed path, and an open asking
    // neither to read nor to write (POSIX's access mode, which libc always
    // sends), are refused here, first, so the answer is the same for every
    // caller and every tree, whether the file exists or not: refused in the
    // handler, `OPEN_CREATE` alone answered FS_ERROR for an existing name
    // and `check_access`'s FS_ERR_PERM for a missing one, which told a user
    // who may not search a directory what is in it (the third medium review
    // of #364). Root too: one with neither created a missing file and was
    // refused for an existing one.
    let open = match verb {
        ninep_abi::NP_OPEN => {
            let Some(path) = path_from(payload, 0, p[1]) else {
                return status_reply(reply, syscall_abi::FS_ERROR);
            };
            if p[0] & (ninep_abi::OPEN_READ | ninep_abi::OPEN_WRITE) == 0 {
                return status_reply(reply, syscall_abi::FS_ERROR);
            }
            // `OPEN_EXCL` means something only beside `OPEN_CREATE`; alone
            // it is a malformed request, refused here for the same reason, as
            // is a flag fsd does not know (`OPEN_FLAGS`), so it fails closed.
            if p[0] & (ninep_abi::OPEN_EXCL | ninep_abi::OPEN_CREATE) == ninep_abi::OPEN_EXCL
                || p[0] & !ninep_abi::OPEN_FLAGS != 0
            {
                return status_reply(reply, syscall_abi::FS_ERROR);
            }
            Some(found(fs, path))
        }
        _ => None,
    };
    // `O_CREAT|O_EXCL` of a name that exists answers here, whole, and
    // nothing acts: see [`exists_answer`]. Only the malformed requests above
    // have answered before it, and none of them by the name's existence. So
    // does a directory opened to write, truncate or create: it opens for
    // reading alone, as POSIX has it (until 2026-10-29 such an open got a
    // fid, and only its first write failed).
    // `O_EXCL` is asked first, so `O_CREAT|O_EXCL` of a directory is EEXIST,
    // as on Linux, whose `do_open` answers it before EISDIR.
    if let Some(Found { kind: Ok(kind), .. }) = open {
        if kind.exists() && p[0] & ninep_abi::OPEN_EXCL != 0 {
            return status_reply(reply, exists_answer(fs, who, &p, payload, syscall_abi::FS_ERR_ALREADY_EXISTS));
        }
        if kind == Kind::Dir && p[0] & ninep_abi::OPEN_DIR_REFUSED != 0 {
            return status_reply(reply, exists_answer(fs, who, &p, payload, syscall_abi::FS_ERR_NOT_A_FILE));
        }
    }
    // Permission enforcement (users/permissions arc, step 3): check the caller's
    // uid/gid against the target's owner+mode before running the op. ext2-only
    // (others return mode:None, which `check_access` treats as unrestricted);
    // root bypasses. See `check_access`.
    if !check_access(fs, who, verb, &p, payload, open) {
        return status_reply(reply, syscall_abi::FS_ERR_PERM);
    }
    // A read-only tree (D5's fallback, or a volume its arm mounted read-only)
    // refuses every change, root's included: the gate has no rule for root.
    // It comes AFTER `check_access`, so a caller who may not search a
    // directory learns nothing of the names in it from a create's EROFS
    // (the second medium review of #364), and before everything that acts.
    if let Some(status) = refused_read_only(tree, fs, verb, &p, open) {
        return status_reply(reply, status);
    }
    // New files/dirs are owned by their creator, not root: stamp the caller's
    // identity onto the fs so touch/write/mkdir/open-create use it (ext2 only;
    // other arms ignore it). Root -> (0,0), the default, so nothing changes for
    // boot/format-time creation. Without this a user couldn't write in its own
    // home - the created file would be root-owned and the follow-up write denied.
    // No identity (a dead sender) must not stamp new inodes as root; the verb
    // itself is denied by check_access below, this is just the safe default.
    let (cuid, cgid) = who.map_or((u32::MAX, u32::MAX), |c| (c.uid, c.gid));
    fs.set_creator(cuid as u16, cgid as u16);
    match verb {
        ninep_abi::NP_READDIR => {
            let Some(path) = path_from(payload, 0, p[0]) else {
                return status_reply(reply, syscall_abi::FS_ERROR);
            };
            let Some(want) = want_len(p[1]) else {
                return status_reply(reply, syscall_abi::FS_ERROR);
            };
            // One page of the listing: entries from index `p[2]` (0 = the
            // first) in the filesystem's walk order, whole, until the next
            // would not fit, and then no more. Until 2026-10-16 a name that did
            // not fit was skipped and the walk went on, so a shorter one
            // further on still got in: a listing with holes, and no word of
            // them. Now a page is a run with no gaps, the client's next start
            // is this one's plus the lines it got, and an empty page is the
            // end. An entry that would not fit an empty page is refused
            // FS_ERROR, since a page of 0 bytes would read as the end. The
            // rule is ninep_abi::ReaddirFill, which netd's listings share; the
            // walk stops where the page ends (since 2026-10-17; it read the
            // rest of the directory for nothing). So a read error past a
            // page's end fails the page that reaches it, not every page: a
            // listing can give its first pages and then fail, as one cut
            // short by Ctrl+C always could.
            let (status_slot, result) = reply[..REPLY_PAYLOAD + want].split_at_mut(REPLY_PAYLOAD);
            let mut fill = ninep_abi::ReaddirFill::new(result, p[2]);
            let outcome = fs.list_dir(path, |name, is_dir| {
                fill.push(name.as_bytes(), if is_dir { b"/\n" } else { b"\n" })
            });
            match (outcome, fill.finish()) {
                (Err(e), _) => status_reply(reply, error_code(&e)),
                (Ok(()), None) => status_reply(reply, syscall_abi::FS_ERROR),
                (Ok(()), Some(written)) => {
                    status_slot[..8].copy_from_slice(&(written as u64).to_le_bytes());
                    REPLY_PAYLOAD + written
                }
            }
        }
        ninep_abi::NP_READ_FILE => {
            let Some(path) = path_from(payload, 0, p[0]) else {
                return status_reply(reply, syscall_abi::FS_ERROR);
            };
            let Some(want) = want_len(p[1]) else {
                return status_reply(reply, syscall_abi::FS_ERROR);
            };
            let (status_slot, result) = reply[..REPLY_PAYLOAD + want].split_at_mut(REPLY_PAYLOAD);
            match fs.read_file(path, result) {
                Ok(size) => {
                    status_slot[..8].copy_from_slice(&(size as u64).to_le_bytes());
                    REPLY_PAYLOAD + (size as usize).min(want)
                }
                Err(e) => status_reply(reply, error_code(&e)),
            }
        }
        ninep_abi::NP_STAT => {
            let Some(path) = path_from(payload, 0, p[0]) else {
                return status_reply(reply, syscall_abi::FS_ERROR);
            };
            match fs.stat(path) {
                Ok(st) => write_stat_reply(reply, &st),
                Err(e) => status_reply(reply, error_code(&e)),
            }
        }
        ninep_abi::NP_READ => {
            let Some(path) = path_from(payload, 0, p[0]) else {
                return status_reply(reply, syscall_abi::FS_ERROR);
            };
            // The path resolved once, then the file copied to the client's
            // grant a SAFECOPY_MAX piece at a time, at the grant offset each
            // piece lands at, up to NP_READ_MAX (since 2026-10-17; one piece
            // a request before, each resolving the path again).
            // One NP_READ fills at most one grant: the most fsd delivers a
            // request is the largest buffer a client may grant.
            const _: () = assert!(ninep_abi::NP_READ_MAX == syscall_abi::GRANT_MAX);
            let offset = p[1];
            let want = p[2].min(ninep_abi::NP_READ_MAX);
            let mut chunk = [0u8; syscall_abi::SAFECOPY_MAX as usize];
            let mut at = 0u64;
            let read = fs.read_each(path, offset, want, &mut chunk, |piece| {
                let r = syscall5::<{ syscall_abi::SAFECOPY }>(
                    call,
                    at,
                    piece.as_ptr() as u64,
                    piece.len() as u64,
                    syscall_abi::GRANT_WRITE,
                );
                if r >= syscall_abi::FS_ERR_MIN {
                    return ControlFlow::Break(());
                }
                at += piece.len() as u64;
                ControlFlow::Continue(())
            });
            match read {
                Ok((copied, false)) => status_reply(reply, copied),
                Ok((_, true)) => status_reply(reply, syscall_abi::FS_ERROR),
                Err(e) => status_reply(reply, error_code(&e)),
            }
        }
        ninep_abi::NP_WRITE => {
            let Some(path) = path_from(payload, 0, p[0]) else {
                return status_reply(reply, syscall_abi::FS_ERROR);
            };
            let data_len = p[1] as usize;
            if data_len > syscall_abi::SAFECOPY_MAX as usize {
                return status_reply(reply, syscall_abi::FS_ERROR);
            }
            let mut databuf = [0u8; syscall_abi::SAFECOPY_MAX as usize];
            if data_len > 0 {
                let r = syscall5::<{ syscall_abi::SAFECOPY }>(
                    call,
                    0,
                    databuf.as_mut_ptr() as u64,
                    data_len as u64,
                    syscall_abi::GRANT_READ,
                );
                if r >= syscall_abi::FS_ERR_MIN {
                    return status_reply(reply, syscall_abi::FS_ERROR);
                }
            }
            match fs.write_file(path, &databuf[..data_len]) {
                Ok(()) => status_reply(reply, 0),
                Err(e) => status_reply(reply, error_code(&e)),
            }
        }
        ninep_abi::NP_WRITE_AT => {
            let Some(path) = path_from(payload, 0, p[0]) else {
                return status_reply(reply, syscall_abi::FS_ERROR);
            };
            let offset = p[1];
            let data_len = p[2] as usize;
            if data_len > syscall_abi::SAFECOPY_MAX as usize {
                return status_reply(reply, syscall_abi::FS_ERROR);
            }
            let mut databuf = [0u8; syscall_abi::SAFECOPY_MAX as usize];
            if data_len > 0 {
                let r = syscall5::<{ syscall_abi::SAFECOPY }>(
                    call,
                    0,
                    databuf.as_mut_ptr() as u64,
                    data_len as u64,
                    syscall_abi::GRANT_READ,
                );
                if r >= syscall_abi::FS_ERR_MIN {
                    return status_reply(reply, syscall_abi::FS_ERROR);
                }
            }
            match fs.write_at(path, offset, &databuf[..data_len]) {
                Ok(()) => status_reply(reply, 0),
                Err(e) => status_reply(reply, error_code(&e)),
            }
        }
        ninep_abi::NP_TOUCH | ninep_abi::NP_MKDIR | ninep_abi::NP_RMDIR | ninep_abi::NP_RM => {
            let Some(path) = path_from(payload, 0, p[0]) else {
                return status_reply(reply, syscall_abi::FS_ERROR);
            };
            let outcome = match verb {
                ninep_abi::NP_MKDIR => fs.mkdir(path),
                ninep_abi::NP_RMDIR => fs.rmdir(path),
                ninep_abi::NP_TOUCH => fs.touch(path),
                _ => fs.rm(path),
            };
            match outcome {
                Ok(()) => status_reply(reply, 0),
                Err(e) => status_reply(reply, error_code(&e)),
            }
        }
        ninep_abi::NP_MV => {
            let Some(src) = path_from(payload, 0, p[0]) else {
                return status_reply(reply, syscall_abi::FS_ERROR);
            };
            let Some(dst) = path_from(payload, p[0] as usize, p[1]) else {
                return status_reply(reply, syscall_abi::FS_ERROR);
            };
            // A directory moved inside itself (`/d` to `/d/sub/d`) would be
            // unlinked from its parent and linked under its own descendant,
            // a cycle no path reaches: silent loss of the subtree. No format
            // checks it, so it is refused here, for every client (POSIX
            // `rename` answers EINVAL). As the filesystem resolves the two
            // paths, not by their bytes: `//`, `.` and `..` resolved (libc and
            // ulib collapse them, a raw NP_MV from a 9P peer need not), and
            // names compared by the filesystem's own rule, so on FAT32 and
            // exFAT `/docs/Docs` is below `/Docs` (until 2026-10-19 all of
            // these got through, and the subtree was lost).
            if ninep_abi::is_within(src, dst, |a, b| fs.names_match(a, b)) {
                return status_reply(reply, syscall_abi::FS_ERR_INVALID_NAME);
            }
            match fs.mv(src, dst) {
                Ok(()) => status_reply(reply, 0),
                Err(e) => status_reply(reply, error_code(&e)),
            }
        }
        ninep_abi::NP_READ_AT => {
            let Some(path) = path_from(payload, 0, p[0]) else {
                return status_reply(reply, syscall_abi::FS_ERROR);
            };
            let Some(want) = want_len(p[2]) else {
                return status_reply(reply, syscall_abi::FS_ERROR);
            };
            let (status_slot, result) = reply[..REPLY_PAYLOAD + want].split_at_mut(REPLY_PAYLOAD);
            match fs.read_at(path, p[1], result) {
                Ok(copied) => {
                    status_slot[..8].copy_from_slice(&(copied as u64).to_le_bytes());
                    REPLY_PAYLOAD + copied as usize
                }
                Err(e) => status_reply(reply, error_code(&e)),
            }
        }
        ninep_abi::NP_WRITE_FILE => {
            let Some(path) = path_from(payload, 0, p[0]) else {
                return status_reply(reply, syscall_abi::FS_ERROR);
            };
            let data_len = p[1] as usize;
            let path_len = p[0] as usize;
            if data_len > DATA_MAX || payload.len() < path_len + data_len {
                return status_reply(reply, syscall_abi::FS_ERROR);
            }
            let data = &payload[path_len..path_len + data_len];
            match fs.write_file(path, data) {
                Ok(()) => status_reply(reply, 0),
                Err(e) => status_reply(reply, error_code(&e)),
            }
        }
        ninep_abi::NP_CHMOD => {
            let Some(path) = path_from(payload, 0, p[0]) else {
                return status_reply(reply, syscall_abi::FS_ERROR);
            };
            match fs.chmod(path, p[1] as u16) {
                Ok(()) => status_reply(reply, 0),
                Err(e) => status_reply(reply, error_code(&e)),
            }
        }
        ninep_abi::NP_CHOWN => {
            let Some(path) = path_from(payload, 0, p[0]) else {
                return status_reply(reply, syscall_abi::FS_ERROR);
            };
            // u64::MAX in a field means "leave unchanged" (so uid-only and
            // gid-only chowns share one verb).
            let uid = if p[1] == u64::MAX { None } else { Some(p[1] as u16) };
            let gid = if p[2] == u64::MAX { None } else { Some(p[2] as u16) };
            match fs.chown(path, uid, gid) {
                Ok(()) => status_reply(reply, 0),
                Err(e) => status_reply(reply, error_code(&e)),
            }
        }
        // Open a fid (permission already checked by check_access). `a0` = flags,
        // `a1` = path length. Create/truncate per the flags, then record the fid.
        ninep_abi::NP_OPEN => {
            // The path and the one stat made above ([`Found`]), not another:
            // what `check_access` and the gate decided on is what this acts on.
            let Some(Found { path, kind }) = open else {
                return status_reply(reply, syscall_abi::FS_ERROR);
            };
            // The user the fid is opened as. `check_access` above already
            // refused a request with no caller, so this cannot fail; it fails
            // closed rather than storing a sentinel uid.
            let Some(opener) = who else {
                return status_reply(reply, syscall_abi::FS_ERROR);
            };
            // A FID SLOT BEFORE THE SIDE EFFECTS. Truncate and create below
            // change the disk, and an open refused after them for a full
            // table (or a path too long to remember) has emptied or created a
            // file the caller then holds no handle to; the host peer refuses
            // first, and `cleak` can fill the table to show it (review of
            // #130). Nothing the refusals need is computed by the branch.
            let Some(slot) = free_fid_slot(fids) else {
                return status_reply(reply, syscall_abi::FS_ERROR); // table full
            };
            if path.len() > FID_PATH_MAX {
                return status_reply(reply, syscall_abi::FS_ERROR);
            }
            let flags = p[0];
            let exists = match kind {
                Ok(kind) => kind.exists(),
                Err(code) => return status_reply(reply, code),
            };
            if flags & ninep_abi::OPEN_TRUNC != 0 {
                // O_TRUNC WITHOUT O_CREAT MUST NOT CREATE. `write_file` creates
                // the file when it is absent, so this branch made
                // `open(path, O_WRONLY|O_TRUNC)` conjure a file POSIX says must
                // fail with ENOENT - the exact violation the `else` arm below
                // was added to fix, left standing in the sibling branch. Half a
                // fix reads as a whole one until someone tries the other case.
                if flags & ninep_abi::OPEN_CREATE == 0 && !exists {
                    return status_reply(reply, syscall_abi::FS_ERR_NOT_FOUND);
                }
                if let Err(e) = fs.write_file(path, &[]) {
                    return status_reply(reply, error_code(&e));
                }
            } else if flags & ninep_abi::OPEN_CREATE != 0 {
                if !exists {
                    if let Err(e) = fs.touch(path) {
                        return status_reply(reply, error_code(&e));
                    }
                }
            } else if !exists {
                // THE FILE MUST EXIST when the caller is not creating it.
                //
                // This check was missing: a read-only NP_OPEN of an absent path
                // allocated a fid and returned it, and the caller only learned
                // the truth at its first NP_PREAD/NP_FSTAT. So a C program got a
                // valid fd for a file that is not there, and reported the
                // failure against the WRONG operation ("fstat failed: no such
                // file or directory" for a path that never opened).
                //
                // Three things it also broke, worth naming so this is not
                // re-simplified away: POSIX says open(O_RDONLY) on a missing
                // file fails; ninep-abi's NP_OPEN doc says permission is checked
                // "here, once, per the flags", which cannot be true of a file
                // that does not exist; and it made fsd DISAGREE with
                // scripts/np9p_server.py, which answers FS_ERR_NOT_FOUND - the
                // divergence a foreign observer exists to find, found by one.
                return status_reply(reply, syscall_abi::FS_ERR_NOT_FOUND);
            }
            let fid = &mut fids[slot];
            *fid = Fid::empty();
            fid.used = true;
            fid.owner = owner;
            fid.owner_uid = opener.uid;
            fid.flags = flags;
            fid.tree = tree;
            fid.path_len = path.len();
            fid.path[..path.len()].copy_from_slice(path.as_bytes());
            status_reply(reply, slot as u64 + FID_BASE)
        }
        _ => status_reply(reply, syscall_abi::FS_ERROR),
    }
}

/// The answer to an `NP_OPEN` whose caller left the call (`MSG_REPLY`
/// answered `REPLY_STALE`: cut short by Ctrl+C, or its caller dead) carried
/// a fid nobody will learn, so nobody will clunk it: freed here. Every other
/// verb's answer leaves nothing behind.
fn forget_unheard_fid(fids: &mut [Fid; MAX_FIDS], req: &[u8], reply: &[u8]) {
    if req.len() < 8 || reply.len() < 8 || read_u64(req, 0) != ninep_abi::NP_OPEN {
        return;
    }
    let fid = read_u64(reply, 0);
    if (FID_BASE..syscall_abi::FS_ERR_MIN).contains(&fid) {
        if let Some(f) = fids.get_mut((fid - FID_BASE) as usize) {
            *f = Fid::empty();
        }
    }
}

/// A free index in the fid table, or `None` if it is full even after reaping
/// fids whose owner is gone. Only finds the slot; `NP_OPEN` fills it, after its
/// side effects, so that a refusal here happens before them.
fn free_fid_slot(fids: &mut [Fid; MAX_FIDS]) -> Option<usize> {
    let mut slot = fids.iter().position(|f| !f.used);
    if slot.is_none() {
        reap_dead_fids(fids);
        slot = fids.iter().position(|f| !f.used);
    }
    slot
}

/// Free any fid whose owner is gone (a client that crashed, exited without
/// `NP_CLUNK`, or a server the supervisor restarted) - the pragmatic answer to
/// fsd not being notified of a client's death. Called only when the small fid
/// table is full.
///
/// "Gone" is decided by IDENTITY, not by the slot's state: the slot's current
/// occupant (`TASK_IDENTITY`) is compared against the identity recorded at
/// open, so a recycled slot frees its predecessor's fids even though the slot
/// reads alive. Asking `TASK_STATE` could not: the shell runs every foreground
/// command in the same slot, so eight C programs that each leaked one fid left
/// the ninth, running in that very slot, unable to open anything, and a
/// restarted `netd` in its protected slot kept the old `netd`'s fids for the
/// boot. An unused slot answers `GET_ID_ERR`, which no owner is.
///
/// ONE read per fid, on purpose. A zombie still answers its own identity and
/// is kept until its parent reaps it, which is at most one more full table
/// away; asking `TASK_STATE` as well would free it now, but two reads have a
/// window between them (the owner exits and is reaped, or its slot respawned,
/// between the two), in which a stale-but-matching identity and a non-zombie
/// state keep a fid that was reapable. A single read has no such window
/// (review of #130).
fn reap_dead_fids(fids: &mut [Fid; MAX_FIDS]) {
    for f in fids.iter_mut() {
        if !f.used {
            continue;
        }
        let slot = syscall_abi::task_id_slot(f.owner);
        if syscall4::<{ syscall_abi::TASK_IDENTITY }>(slot, 0, 0, 0) != f.owner {
            f.used = false;
        }
    }
}

/// Handle a fid op (`a0` = fid): resolve the fid to its owner-checked path +
/// mount, then read/write/stat/clunk. No per-op permission check - authorized at
/// open. `owner` is the sender's packed identity, what a fid is owned by;
/// `call` is the handle of the call being answered (`SENDER_CALL`, read on
/// receipt), what a SAFECOPY names - not a slot, which SAFECOPY no longer
/// takes.
#[allow(clippy::too_many_arguments)]
fn handle_fid_op(
    mounts: &mut [Option<vfs::Filesystem>; MAX_MOUNTS],
    fids: &mut [Fid; MAX_FIDS],
    owner: u64,
    call: u64,
    who: Option<Caller>,
    verb: u64,
    p: &[u64; 4],
    reply: &mut [u8],
) -> usize {
    let fid = p[0];
    if fid < FID_BASE {
        return status_reply(reply, syscall_abi::FS_ERROR);
    }
    let idx = (fid - FID_BASE) as usize;
    if idx >= MAX_FIDS || !fids[idx].used || fids[idx].owner != owner {
        return status_reply(reply, syscall_abi::FS_ERROR); // bad or not-yours
    }
    let sender = syscall_abi::task_id_slot(owner);
    // ...and, from `netd`, asked for by the same USER, for every verb with
    // NP_CLUNK included: see Fid::owner_uid. A mismatch refuses and KEEPS the
    // fid - the owner is still using it, and the asker was never entitled to
    // it. Only `netd`: it is the one sender whose identity fronts many users.
    if sender == syscall_abi::NET_TASK {
        match who {
            Some(c) if c.uid == fids[idx].owner_uid => {}
            _ => return status_reply(reply, syscall_abi::FS_ERR_PERM),
        }
    }
    if verb == ninep_abi::NP_CLUNK {
        fids[idx] = Fid::empty();
        return status_reply(reply, 0);
    }
    if fids[idx].dead {
        return status_reply(reply, syscall_abi::NO_FS);
    }
    // Honour what the fid was opened FOR. These ops skip check_access on the
    // grounds that the open was authorized - so the open's access mode IS the
    // authorization, and writing through a read-only fid was never authorized.
    let flags = fids[idx].flags;
    let allowed = match verb {
        ninep_abi::NP_PREAD => flags & ninep_abi::OPEN_READ != 0,
        // `OPEN_WRITE` alone: an open that created or truncated without it
        // (`O_RDONLY|O_CREAT`, `O_RDONLY|O_TRUNC`) holds a read-only fd, and
        // POSIX answers its write EBADF (the medium review of #364).
        ninep_abi::NP_PWRITE => flags & ninep_abi::OPEN_WRITE != 0,
        _ => true, // NP_FSTAT: metadata, like NP_STAT
    };
    if !allowed {
        return status_reply(reply, syscall_abi::FS_ERR_PERM);
    }
    let tree = fids[idx].tree;
    // The read-only gate for the fid path too, on every tree (D5's ESP, an
    // arm's read-only volume, `/proc`), so the refusal of a write-open is not
    // the only thing between a fid and a read-only volume: today a write fid
    // cannot outlive the unmount before the fallback, but nothing here should
    // depend on that staying true (the review of #291). The mount is looked
    // up first because the gate asks it.
    let Some(fs) = mounts[tree].as_mut() else {
        return status_reply(reply, syscall_abi::NO_FS);
    };
    if let Some(status) = refused_read_only(tree, fs, verb, p, None) {
        return status_reply(reply, status);
    }
    let Some(path) = fids[idx].path_str() else {
        return status_reply(reply, syscall_abi::FS_ERROR);
    };
    match verb {
        ninep_abi::NP_PREAD => {
            // `capped`, not `want_len`: a count of 0 is a legal read that
            // answers 0, the POSIX shape. It still goes through `read_at` so
            // the errors a real read would meet (not a file, gone) are met.
            let Some(want) = capped(p[2]) else {
                return status_reply(reply, syscall_abi::FS_ERROR);
            };
            let (status_slot, result) = reply[..REPLY_PAYLOAD + want].split_at_mut(REPLY_PAYLOAD);
            match fs.read_at(path, p[1], result) {
                Ok(copied) => {
                    status_slot[..8].copy_from_slice(&(copied as u64).to_le_bytes());
                    REPLY_PAYLOAD + copied as usize
                }
                Err(e) => status_reply(reply, error_code(&e)),
            }
        }
        ninep_abi::NP_PWRITE => {
            let data_len = p[2] as usize;
            if data_len > syscall_abi::SAFECOPY_MAX as usize {
                return status_reply(reply, syscall_abi::FS_ERROR);
            }
            let mut databuf = [0u8; syscall_abi::SAFECOPY_MAX as usize];
            if data_len > 0 {
                let r = syscall5::<{ syscall_abi::SAFECOPY }>(
                    call,
                    0,
                    databuf.as_mut_ptr() as u64,
                    data_len as u64,
                    syscall_abi::GRANT_READ,
                );
                if r >= syscall_abi::FS_ERR_MIN {
                    return status_reply(reply, syscall_abi::FS_ERROR);
                }
            }
            match fs.write_at(path, p[1], &databuf[..data_len]) {
                Ok(()) => status_reply(reply, data_len as u64),
                Err(e) => status_reply(reply, error_code(&e)),
            }
        }
        ninep_abi::NP_FSTAT => match fs.stat(path) {
            Ok(st) => write_stat_reply(reply, &st),
            Err(e) => status_reply(reply, error_code(&e)),
        },
        _ => status_reply(reply, syscall_abi::FS_ERROR),
    }
}

/// Build a stat reply (the [`ninep_abi::STAT_INFO_LEN`] record) from a `Stat`,
/// shared by `NP_STAT` (path) and `NP_FSTAT` (fid).
fn write_stat_reply(reply: &mut [u8], st: &vfs::Stat) -> usize {
    let (status_slot, result) =
        reply[..REPLY_PAYLOAD + ninep_abi::STAT_INFO_LEN].split_at_mut(REPLY_PAYLOAD);
    result.fill(0); // unset fields (time when absent) read as zero
    let mut flags: u32 = 0;
    if st.is_dir {
        flags |= ninep_abi::STAT_FLAG_DIR;
    }
    result[ninep_abi::STAT_SIZE_OFF..ninep_abi::STAT_SIZE_OFF + 8].copy_from_slice(&st.size.to_le_bytes());
    result[ninep_abi::STAT_FLAGS_OFF..ninep_abi::STAT_FLAGS_OFF + 4].copy_from_slice(&flags.to_le_bytes());
    if let Some(t) = st.time {
        result[ninep_abi::STAT_YEAR_OFF..ninep_abi::STAT_YEAR_OFF + 2].copy_from_slice(&t.year.to_le_bytes());
        result[ninep_abi::STAT_MONTH_OFF] = t.month;
        result[ninep_abi::STAT_DAY_OFF] = t.day;
        result[ninep_abi::STAT_HOUR_OFF] = t.hour;
        result[ninep_abi::STAT_MIN_OFF] = t.min;
        result[ninep_abi::STAT_SEC_OFF] = t.sec;
        result[ninep_abi::STAT_TIMEVALID_OFF] = 1;
    }
    if let Some(m) = st.mode {
        result[ninep_abi::STAT_MODE_OFF..ninep_abi::STAT_MODE_OFF + 2].copy_from_slice(&m.mode.to_le_bytes());
        result[ninep_abi::STAT_UID_OFF..ninep_abi::STAT_UID_OFF + 2].copy_from_slice(&m.uid.to_le_bytes());
        result[ninep_abi::STAT_GID_OFF..ninep_abi::STAT_GID_OFF + 2].copy_from_slice(&m.gid.to_le_bytes());
        result[ninep_abi::STAT_MODEVALID_OFF] = 1;
    }
    status_slot[..8].copy_from_slice(&(ninep_abi::STAT_INFO_LEN as u64).to_le_bytes());
    REPLY_PAYLOAD + ninep_abi::STAT_INFO_LEN
}

/// Writes a status-only reply and returns its length.
fn status_reply(reply: &mut [u8], status: u64) -> usize {
    reply[..8].copy_from_slice(&status.to_le_bytes());
    REPLY_PAYLOAD
}

/// A validated path slice out of the request payload: non-empty,
/// capped, in-bounds, UTF-8.
fn path_from(payload: &[u8], start: usize, len: u64) -> Option<&str> {
    let len = len as usize;
    if len == 0 || len > DATA_MAX || payload.len() < start + len {
        return None;
    }
    core::str::from_utf8(&payload[start..start + len]).ok()
}

/// A result-window size capped at the per-op payload max (which also always
/// fits the reply buffer). The one bound every read verb shares; whether a
/// verb also refuses 0 is that verb's choice, made by calling this or
/// [`want_len`].
fn capped(want: u64) -> Option<usize> {
    let want = want as usize;
    if want > DATA_MAX {
        return None;
    }
    Some(want)
}

/// [`capped`] plus a floor: a validated result-window size that is non-zero.
/// The path read verbs use it; `NP_PREAD` does not (a zero count answers 0).
fn want_len(want: u64) -> Option<usize> {
    capped(want).filter(|&w| w != 0)
}

// --- Permission enforcement (users/permissions arc, step 3) -----------------
// The classic Unix check: given a caller's uid/gid and a file's owner + mode,
// decide whether an operation is allowed. Enforced only on a filesystem that
// models mode/owner (ext2 - `stat` returns `Some(FileMode)`); FAT32/exFAT/`/proc`
// return `None` and stay unrestricted, the same honest per-FS degradation as
// `chmod`/`chown`. Root (uid 0) bypasses everything. Path traversal (the search
// bit on ancestor directories) is deliberately NOT checked yet - only the object
// of the operation, and the parent directory for create/delete/rename - a
// documented first-cut simplification.

const PERM_R: u16 = 4;
const PERM_W: u16 = 2;
/// Search permission on a directory - the right to *resolve a path through* it,
/// which is a different thing from reading it (`r` lists the names; `x` lets you
/// reach what they refer to).
const PERM_X: u16 = 1;
/// What modifying a directory's entries requires: write **and** search on that
/// directory. `PERM_W` alone is not enough - the ancestor walk below checks a
/// parent's *ancestors*, never the parent itself, so `chmod 0300 dir` (write, no
/// search) would still allow `touch dir/f`, `rm dir/f` and `mkdir dir/d` inside a
/// directory the caller cannot resolve a path through. POSIX requires both.
const PERM_WX: u16 = PERM_W | PERM_X;

/// The caller's identity for a permission decision: uid, primary gid, and the
/// supplementary groups the kernel carries for that task.
#[derive(Clone, Copy)]
struct Caller {
    uid: u32,
    gid: u32,
    groups: [u32; syscall_abi::MAX_SUPP_GROUPS],
    n_groups: usize,
}

impl Caller {
    /// Whether this caller is in `owner_gid` - as its primary group, or through
    /// any supplementary one. This is the whole of what supplementary groups
    /// buy: the group triad now applies to every group the user belongs to, not
    /// only the one packed into its identity word.
    fn in_group(&self, owner_gid: u32) -> bool {
        self.gid == owner_gid || self.groups[..self.n_groups].contains(&owner_gid)
    }
}

/// Whether `who` is allowed `need` (an `R`/`W`/`X` bit combination) on an object
/// owned by `owner_uid`/`owner_gid` with `mode`.
fn mode_allows(mode: u16, owner_uid: u32, owner_gid: u32, who: &Caller, need: u16) -> bool {
    if who.uid == 0 {
        return true; // root bypasses permission checks
    }
    let triad = if who.uid == owner_uid {
        (mode >> 6) & 7 // owner rwx
    } else if who.in_group(owner_gid) {
        (mode >> 3) & 7 // group rwx - primary or supplementary
    } else {
        mode & 7 // other rwx
    };
    triad & need == need
}

/// The requesting task's `(uid, gid)` and group list, as **captured by the
/// kernel when the request was sent** (`SENDER_ID`/`SENDER_GROUPS`). The
/// identity is a packed `(gid << 32) | uid`; widened to `u32` to compare
/// against the ext2 inode's `u16` owner fields.
///
/// Deliberately NOT `GET_ID(sender)`, which answers who occupies that slot
/// *now*. A caller can send a request (`MSG_SEND` does not block), exit, and
/// have its slot reaped and re-spawned before this server drains its mailbox -
/// and if the new occupant is root, the request would be authorized as root.
/// Refusing a *dead* slot never closed that: a recycled slot is alive, and the
/// message carries a bare slot number with nothing to tell the two apart. The
/// kernel binds the credential at send instead; this reads that.
///
/// `None` when the kernel has no captured credential to report, and every
/// caller fails closed on it.
fn caller_id() -> Option<Caller> {
    let packed = syscall4::<{ syscall_abi::SENDER_ID }>(0, 0, 0, 0);
    if packed == syscall_abi::GET_ID_ERR {
        return None;
    }
    let mut groups = [0u32; syscall_abi::MAX_SUPP_GROUPS];
    let n = syscall4::<{ syscall_abi::SENDER_GROUPS }>(
        groups.as_mut_ptr() as u64,
        groups.len() as u64,
        0,
        0,
    );
    let n_groups = if n == syscall_abi::GET_ID_ERR { 0 } else { (n as usize).min(groups.len()) };
    Some(Caller { uid: packed as u32, gid: (packed >> 32) as u32, groups, n_groups })
}

/// Whether the resolved caller is root - the gate on the disk-management
/// control ops, which never pass through [`check_access`]. A request with no
/// identity to authorize against is not root.
fn is_root(who: &Option<Caller>) -> bool {
    matches!(who, Some(c) if c.uid == 0)
}

/// The identity a request is authorized against, resolved once per request.
///
/// Two sources, and which one applies is decided by the **sender's slot**:
///
/// - **Anyone but `NET_TASK`:** the credential the kernel bound when the message
///   was sent ([`caller_id`]). The request's `a3` word is ignored entirely, so a
///   task cannot claim an identity by writing one there.
/// - **`NET_TASK` (`netd`):** the identity stated in `a3`, because `netd`
///   forwards other machines' requests and its own credential is root - see
///   `ninep_abi`'s `NP_ID_*` docs for why the exception must exist. Either
///   [`NP_ID_FIRST_PARTY`](ninep_abi::NP_ID_FIRST_PARTY) (netd's own business,
///   authorized as netd) or a proxied `(uid, gid)`.
///
/// **A `NET_TASK` request that states nothing is refused**, not served as netd.
/// That fallback is the hole this exists to close: it is what an earlier
/// attempt did whenever its separate identity message went missing, and it
/// meant a forgotten call site escalated silently to root instead of failing.
/// Refusing costs nothing, because every path through `netd` states which it is.
///
/// Checking the SLOT rather than a credential is deliberate and sound here:
/// slots below `FIRST_SPAWNABLE` are protected boot servers, so `NET_TASK` is
/// `netd` for the life of the boot and can never be recycled to something else.
/// That is a different question from "who sent this", which is why it has a
/// different answer - see [`caller_id`] for the one that must not be asked this
/// way.
fn effective_caller(op: u64, sender: u64, req: &[u8]) -> Option<Caller> {
    if sender != syscall_abi::NET_TASK {
        return caller_id();
    }
    // `a3` lives at offset 40 of an NP request. A shorter message, or one of the
    // FSOP_* admin ops (which have no such field and which the export can never
    // emit), states nothing - and from netd that is a refusal.
    if !(ninep_abi::NP_BASE..ninep_abi::NP_LIMIT).contains(&op) || req.len() < ninep_abi::NP_REQ_PAYLOAD as usize {
        return None;
    }
    match read_u64(req, 40) {
        ninep_abi::NP_ID_FIRST_PARTY => caller_id(),
        w => {
            let (uid, gid) = ninep_abi::proxy_parts(w)?;
            // No supplementary groups cross the cluster: one word has no room,
            // and a group list arriving out of step with its identity is the
            // failure mode this design removes. Under-privileging only.
            Some(Caller { uid, gid, groups: [0; syscall_abi::MAX_SUPP_GROUPS], n_groups: 0 })
        }
    }
}

/// The parent directory of an absolute path (`/etc/passwd` -> `/etc`, `/foo` ->
/// `/`). Works in bytes: slicing a `&str` by a runtime index would pull in
/// `core::fmt` (the PIE relocation trap), so it slices the byte view and
/// re-wraps.
fn parent_of(path: &str) -> &str {
    let b = path.as_bytes();
    let mut i = b.len();
    let mut cut = 0;
    while i > 0 {
        i -= 1;
        if b[i] == b'/' {
            cut = i;
            break;
        }
    }
    if cut == 0 {
        "/"
    } else {
        core::str::from_utf8(&b[..cut]).unwrap_or("/")
    }
}

/// Whether one directory grants the caller search (`x`) permission. Shared by
/// the ancestor walk; a directory that can't be stat'd or whose filesystem has
/// no modes passes, the same rule [`path_allows`] uses.
fn dir_searchable(fs: &mut vfs::Filesystem, dir: &str, who: &Caller) -> bool {
    match fs.stat(dir) {
        Ok(st) => match st.mode {
            Some(m) => mode_allows(m.mode, m.uid as u32, m.gid as u32, who, PERM_X),
            None => true,
        },
        Err(_) => true,
    }
}

/// Whether `path` contains a `..` component (not merely the two characters - a
/// file named `..foo` is fine). Byte-only, like every path helper here.
fn has_dotdot(path: &str) -> bool {
    let b = path.as_bytes();
    let mut start = 0usize;
    for i in 0..=b.len() {
        if i == b.len() || b[i] == b'/' {
            if &b[start..i] == b".." {
                return true;
            }
            start = i + 1;
        }
    }
    false
}

/// Whether every **ancestor directory** of `path` grants the caller search
/// (`x`) permission - POSIX path resolution.
///
/// This is what makes `chmod 700 <dir>` mean something. Without it a mode on a
/// *directory* only protected the directory's own listing: a caller who already
/// knew the name of a world-readable file inside could still open it by full
/// path, because only the object and (for writes) its immediate parent were ever
/// checked. Demonstrated before fixing: as an ordinary user, `cat
/// /secret/inside` on a 0644 file inside a `drwx------` root-owned directory
/// printed its contents.
///
/// Walks `/` first (everything resolves through it), then each `/`-terminated
/// prefix, which yields exactly the ancestors and never the final component.
/// Byte-only: slicing a `&str` by a runtime index pulls in `core::fmt` and an
/// `R_AARCH64_ABS64` the `-pie` link rejects (the recurring trap).
///
/// A `..` component is REFUSED rather than resolved. This walk checks lexical
/// prefixes, while ext2 resolves `..` as a real directory entry - so
/// `/bin/../priv/x` would never consult `/priv`. The `/bin` tools normalize
/// client-side, but the C libc does not and a remote 9P client need not, so the
/// server cannot rely on that. No legitimate caller sends an unnormalized path.
///
/// Cost: one `stat` per ancestor level, on non-root callers only, and a
/// filesystem with no modes short-circuits before reaching here. Each `stat`
/// re-resolves from the root inode, so a deep path is proportionally dearer;
/// resolving once and collecting modes on the way down is the obvious
/// refinement if that ever shows up in a profile.
fn ancestors_searchable(fs: &mut vfs::Filesystem, path: &str, who: &Caller) -> bool {
    if has_dotdot(path) {
        return false;
    }
    if !dir_searchable(fs, "/", who) {
        return false;
    }
    let b = path.as_bytes();
    let mut i = 1usize;
    while i < b.len() {
        if b[i] == b'/' {
            match core::str::from_utf8(&b[..i]) {
                Ok(prefix) => {
                    if !dir_searchable(fs, prefix, who) {
                        return false;
                    }
                }
                Err(_) => return true, // not a path we can reason about
            }
        }
        i += 1;
    }
    true
}

/// Whether `path` (or, for namespace ops, the given directory) grants the caller
/// `need`. A path that can't be stat'd (e.g. doesn't exist) or a filesystem that
/// can't model permissions passes - the op then runs and fails on its own merits
/// (not-found, etc.) rather than masquerading as a permission error.
fn path_allows(fs: &mut vfs::Filesystem, path: &str, who: &Caller, need: u16) -> bool {
    // The "does this filesystem model permissions at all?" short-circuit used to
    // live here. It is now the first thing `check_access` asks, because three
    // verbs (NP_STAT/NP_CHMOD/NP_CHOWN) reach `ancestors_searchable` without
    // coming through this function and so never got it - see `models_modes`.
    if !ancestors_searchable(fs, path, who) {
        return false;
    }
    match fs.stat(path) {
        Ok(st) => match st.mode {
            Some(m) => mode_allows(m.mode, m.uid as u32, m.gid as u32, who, need),
            None => true, // filesystem doesn't model owner/mode -> unrestricted
        },
        Err(_) => true,
    }
}

/// Whether the mounted filesystem models owner/mode at all - `None` when the
/// root would not `stat`, so the question could not be answered.
///
/// The tri-state is not fussiness: the two callers resolve the unknown in
/// OPPOSITE directions, and a single `bool` here would have to pick one and
/// silently impose it on the other. `check_access` treats unknown as "modelled"
/// so the per-verb checks still run; `FSOP_MOUNT_INFO` treats it as "not
/// modelled" so the shell still warns. Both are "say so when unsure", which is
/// the same rule pointing two ways because one is a denial and the other is a
/// warning.
fn models_modes_or_unknown(fs: &mut vfs::Filesystem) -> Option<bool> {
    fs.stat("/").ok().map(|st| st.mode.is_some())
}

/// Whether to apply the per-verb permission checks. ext2 models owner/mode;
/// FAT32, exFAT and the synthetic `/proc` do not, and on those every permission
/// question has the same answer, so asking once is both the correct answer and
/// the cheap one.
///
/// Unknown counts as modelled: the per-verb checks each fail open on their own
/// `Err`, so an unreadable root must not ALSO skip the ancestor walk.
fn models_modes(fs: &mut vfs::Filesystem) -> bool {
    models_modes_or_unknown(fs).unwrap_or(true)
}

/// The answer to an open (`p`, `payload`) of a name that exists, refused for
/// what the name is: `known` to a caller who may stat it, which is what tells
/// anyone the name exists and what it is, and FS_ERR_PERM to one who may not,
/// or to no caller. `known` is FS_ERR_ALREADY_EXISTS (EEXIST) for
/// `O_CREAT|O_EXCL`, and FS_ERR_NOT_A_FILE (EISDIR) for a directory opened to
/// write, truncate or create (since 2026-10-29), each answered before
/// `check_access`'s mode check and the read-only gate, as Linux's `may_open`
/// answers EISDIR before both. "May stat it" is [`check_access`]'s own `NP_STAT` rule,
/// asked of it, not a copy of it (the high review of #367): root, a
/// filesystem without modes and the ancestor walk stay one rule. The name's own mode is not asked: the open will
/// neither read nor write it, and Linux answers both before it looks at the
/// mode (`do_open`'s EEXIST, then its EISDIR for `O_CREAT` and `may_open`'s
/// for a write or truncate, all before `inode_permission`), so EEXIST is given for a file the
/// caller may not read and EISDIR for a directory it may not write. Nor is the read-only gate: POSIX gives EROFS only for
/// a file the open would make. Made whole here, before [`check_access`], and
/// not as a skip inside it with the EEXIST answered after: the control
/// removing that later answer left the skip standing, and `user` opened
/// /etc/shadow for writing and wrote it (the first build of the O_EXCL fix,
/// 2026-10-28). Removing this one leaves `check_access` as it always was.
/// Until 2026-10-28 libc dropped `O_EXCL` and such an open succeeded.
fn exists_answer(fs: &mut vfs::Filesystem, who: Option<Caller>, p: &[u64; 4], payload: &[u8], known: u64) -> u64 {
    // NP_STAT's `a0` is the path length, NP_OPEN's `a1`; the payload is the
    // same path at offset 0 for both.
    let stat = [p[1], 0, 0, 0];
    if check_access(fs, who, ninep_abi::NP_STAT, &stat, payload, None) {
        known
    } else {
        syscall_abi::FS_ERR_PERM
    }
}

/// Enforce the caller's permission for `verb`. Returns `true` if allowed. Reads
/// the object's owner+mode via `stat` and compares against the caller's uid/gid.
///
/// `open` is an `NP_OPEN`'s path and its one stat ([`Found`]), `None` for
/// every other verb.
fn check_access(fs: &mut vfs::Filesystem, who: Option<Caller>, verb: u64, p: &[u64; 4], payload: &[u8], open: Option<Found>) -> bool {
    let Some(who) = who else {
        return false; // no identity to authorize against - refuse
    };
    let uid = who.uid;
    if uid == 0 {
        return true; // root: skip the per-op work entirely
    }
    // ONE stat answers "does this filesystem model permissions at all?", for
    // EVERY verb. It used to sit inside `path_allows`, which meant the three
    // verbs that call `ancestors_searchable` directly - NP_STAT, NP_CHMOD and
    // NP_CHOWN - never got it: on FAT32 a non-root `cat ../f` was allowed (it
    // goes through `path_allows`) while `ls -l ../f` was refused, and every
    // `ls -l` entry paid an ancestor walk that could only return `true`.
    if !models_modes(fs) {
        return true;
    }
    match verb {
        // Stat: the object's own mode is NOT checked - `ls -l` needs metadata for
        // entries a readable directory already exposes, and the mode is what it
        // is about to print. But the ancestor walk DOES apply, or `chmod 700
        // <dir>` would still leak the size, owner, mode and mtime of any path
        // inside it to someone who can guess the name. (A directory that is
        // readable but not searchable therefore lists names and refuses `-l`,
        // which is what POSIX does too.)
        ninep_abi::NP_STAT => match path_from(payload, 0, p[0]) {
            Some(path) => ancestors_searchable(fs, path, &who),
            None => true,
        },
        // open: the fid is authorized here, once. read-open needs R on the file;
        // write/create-open needs W (on the file if it exists, else on the parent
        // dir for a create). a0 = flags, a1 = path length.
        ninep_abi::NP_OPEN => match open {
            Some(Found { path, kind }) => {
                let flags = p[0];
                // OPEN_TRUNC is a WRITE: the handler acts on it by truncating
                // the file, so leaving it out of this mask let
                // `open(path, O_RDONLY | O_TRUNC)` destroy a root-owned file
                // with only read permission. OPEN_CREATE is one only when the
                // file is missing, so `O_RDONLY|O_CREAT` of a file the caller
                // may read opens it (the medium review of #364).
                //
                // A stat that failed (`Err`) authorizes nothing to act: the
                // handler answers that status from the same [`Found`] and
                // touches nothing. Its status still names the path's shape (a
                // path through a file), so the ancestors must be searchable to
                // hear it, as for NP_STAT. An open asking neither to read nor
                // to write never reaches here: `handle_ninep` refuses it first,
                // for every caller alike.
                let exists = match kind {
                    Ok(kind) => kind.exists(),
                    Err(_) => return ancestors_searchable(fs, path, &who),
                };
                let wants_write = ninep_abi::open_changes(flags, exists);
                let wants_read = flags & ninep_abi::OPEN_READ != 0;
                if wants_read && !path_allows(fs, path, &who, PERM_R) {
                    return false;
                }
                if wants_write {
                    let ok = if exists {
                        path_allows(fs, path, &who, PERM_W)
                    } else {
                        path_allows(fs, parent_of(path), &who, PERM_WX)
                    };
                    if !ok {
                        return false;
                    }
                }
                true
            }
            None => true,
        },
        ninep_abi::NP_READDIR | ninep_abi::NP_READ_FILE | ninep_abi::NP_READ | ninep_abi::NP_READ_AT => {
            match path_from(payload, 0, p[0]) {
                Some(path) => path_allows(fs, path, &who, PERM_R),
                None => true,
            }
        }
        // Writes: W on an existing file, else W on the parent dir (create).
        ninep_abi::NP_WRITE | ninep_abi::NP_WRITE_FILE | ninep_abi::NP_WRITE_AT => {
            match path_from(payload, 0, p[0]) {
                Some(path) => {
                    if fs.stat(path).is_ok() {
                        path_allows(fs, path, &who, PERM_W)
                    } else {
                        path_allows(fs, parent_of(path), &who, PERM_WX)
                    }
                }
                None => true,
            }
        }
        // Namespace changes: W on the parent directory.
        ninep_abi::NP_TOUCH | ninep_abi::NP_MKDIR | ninep_abi::NP_RM | ninep_abi::NP_RMDIR => {
            match path_from(payload, 0, p[0]) {
                Some(path) => path_allows(fs, parent_of(path), &who, PERM_WX),
                None => true,
            }
        }
        // Rename: W on both the source and destination parent directories.
        ninep_abi::NP_MV => {
            match (path_from(payload, 0, p[0]), path_from(payload, p[0] as usize, p[1])) {
                (Some(src), Some(dst)) => {
                    path_allows(fs, parent_of(src), &who, PERM_WX)
                        && path_allows(fs, parent_of(dst), &who, PERM_WX)
                }
                _ => true,
            }
        }
        // chmod: owner-only (root already returned above). These two stat the
        // object directly rather than going through path_allows, so they need
        // the ancestor walk spelled out.
        ninep_abi::NP_CHMOD => match path_from(payload, 0, p[0]) {
            Some(path) if !ancestors_searchable(fs, path, &who) => false,
            Some(path) => match fs.stat(path) {
                Ok(st) => match st.mode {
                    Some(m) => uid == m.uid as u32,
                    None => true,
                },
                Err(_) => true,
            },
            None => true,
        },
        // chown: root-only, so a non-root caller (we're past the root bypass) is
        // always refused when the filesystem models ownership.
        ninep_abi::NP_CHOWN => match path_from(payload, 0, p[0]) {
            Some(path) if !ancestors_searchable(fs, path, &who) => false,
            Some(path) => match fs.stat(path) {
                Ok(st) => st.mode.is_none(), // unrestricted only if not modelled
                Err(_) => true,
            },
            None => true,
        },
        // DEFAULT-DENY. Every verb that can reach here has an arm above, so
        // this is unreachable today - which is exactly why it must not be
        // `true`. It was, and a verb added to `ninep-abi` without an arm here
        // would have shipped UNAUTHORIZED: no compiler warning, no failing
        // test, and nothing in the diff adding the verb to point at this file.
        //
        // Refusing is the safe direction for the same reason the root bypass
        // above exists: an enforcement mistake can then only over-restrict a
        // non-root caller, never hand out access. See
        // docs/postmortems/users-and-permissions-postmortem.md.
        _ => false,
    }
}

// A verb added to `ninep-abi` must be given an arm in `check_access` above.
// The default arm now REFUSES, so forgetting is no longer a security hole -
// but a verb silently refused is still a bug, and nothing else would say so:
// the change that adds a verb touches `ninep-abi`, not this file, which is the
// shape that let the hole exist in the first place.
//
// So the compiler asks. If this stops compiling, add the arm (or add the verb
// to the fid-op early-return in `handle_ninep`, which bypasses this check by
// design), then bump the count.
const _: () = assert!(
    ninep_abi::NP_LIMIT == ninep_abi::NP_BASE + 20,
    "a NP_* verb was added or removed: give it an arm in check_access, then update this count"
);

/// The old kernel `fs_error_code`, unchanged: maps a `fat32::Error` to
/// its ABI code. The mount-shape variants map to `FS_ERR_IO` rather
/// than being omitted - an unreachable arm today isn't a proof it
/// stays unreachable.
fn error_code(e: &fat32::Error) -> u64 {
    match e {
        fat32::Error::NotFound => syscall_abi::FS_ERR_NOT_FOUND,
        fat32::Error::NotAFile => syscall_abi::FS_ERR_NOT_A_FILE,
        fat32::Error::NotADirectory => syscall_abi::FS_ERR_NOT_A_DIRECTORY,
        fat32::Error::InvalidName => syscall_abi::FS_ERR_INVALID_NAME,
        fat32::Error::AlreadyExists => syscall_abi::FS_ERR_ALREADY_EXISTS,
        fat32::Error::DirectoryNotEmpty => syscall_abi::FS_ERR_NOT_EMPTY,
        fat32::Error::CannotRemoveRoot => syscall_abi::FS_ERR_IS_ROOT,
        fat32::Error::DiskFull => syscall_abi::FS_ERR_DISK_FULL,
        fat32::Error::ReadOnly => syscall_abi::FS_ERR_READ_ONLY,
        fat32::Error::Unsupported => syscall_abi::FS_ERR_NOT_SUPPORTED,
        fat32::Error::Io(_)
        | fat32::Error::NoFat32Partition
        | fat32::Error::NotFat32
        | fat32::Error::NotExFat
        | fat32::Error::NotExt2
        | fat32::Error::InvalidOffset
        | fat32::Error::UnsupportedSectorSize(_) => syscall_abi::FS_ERR_IO,
    }
}

/// Fixed name per error for the mount log - replaces the old kernel
/// module's `Display` impl (dropped in the port; nothing here needs
/// `core::fmt`, and a static string table is leaner).
fn error_name(e: &fat32::Error) -> &'static str {
    match e {
        fat32::Error::NoFat32Partition => "no mountable partition on the disk",
        fat32::Error::NotFat32 => "partition is not FAT32",
        fat32::Error::NotExFat => "partition is not exFAT",
        fat32::Error::NotExt2 => "partition is not ext2",
        fat32::Error::ReadOnly => "read-only filesystem",
        fat32::Error::Unsupported => "operation not supported by this filesystem",
        fat32::Error::UnsupportedSectorSize(_) => "unsupported sector size",
        fat32::Error::Io(_) => "disk I/O error",
        fat32::Error::NotFound => "not found",
        fat32::Error::NotAFile => "not a file",
        fat32::Error::NotADirectory => "not a directory",
        fat32::Error::InvalidName => "invalid name",
        fat32::Error::AlreadyExists => "already exists",
        fat32::Error::DirectoryNotEmpty => "directory not empty",
        fat32::Error::CannotRemoveRoot => "cannot remove root",
        fat32::Error::DiskFull => "disk full",
        fat32::Error::InvalidOffset => "invalid offset",
    }
}

fn read_u64(buf: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes([
        buf[offset],
        buf[offset + 1],
        buf[offset + 2],
        buf[offset + 3],
        buf[offset + 4],
        buf[offset + 5],
        buf[offset + 6],
        buf[offset + 7],
    ])
}

fn print(s: &str) {
    con_write(s.as_bytes());
}

/// The line saying why `fs` was mounted read-only, in three pieces for
/// [`con_write_parts`]; empty pieces when it is writable. Until 2026-10-26
/// exFAT made a volume read-only with no word (the high review that day).
fn read_only_line(fs: &vfs::Filesystem) -> (&'static str, &'static str, &'static str) {
    match fs.read_only() {
        Some(why) => ("fsd: mounted read-only: ", why, "\r\n"),
        None => ("", "", ""),
    }
}

/// `parts` joined and written in one write, so no other task's line lands
/// inside them: three pieces let accountd's ready line split "fsd: FAT32
/// mounted" on a boot `make test-boot-partition` read (the review of #288).
/// Bounded, so a longer name or message is cut, never a panic in the
/// storage server (the third review of #288); 320 bytes stays under one
/// console message, [`FS_DATA_MAX`](syscall_abi::FS_DATA_MAX).
fn con_write_parts(parts: &[&str]) {
    let mut line = [0u8; 320];
    let mut len = 0;
    for part in parts {
        let take = part.len().min(line.len() - len);
        line[len..len + take].copy_from_slice(&part.as_bytes()[..take]);
        len += take;
    }
    con_write(&line[..len]);
}

/// Route output through the console server (task `CON_TASK`) as a batched
/// `DSPOP_WRITE` message, falling back to the kernel console (`PUTC`) if
/// there's no server this boot - same shape as the shell's `con_write`.
/// The filesystem server only prints at startup / on `mount`, so this is
/// never on a hot path; without it the server's few lines would render at
/// the kernel's own fbconsole cursor instead of `cond`'s, stranded on a
/// framebuffer-only platform (see CLAUDE.md's "Driver isolation, part 3").
fn con_write(bytes: &[u8]) {
    let payload_off = ninep_abi::NP_REQ_PAYLOAD as usize;
    let mut off = 0;
    while off < bytes.len() {
        let n = (bytes.len() - off).min(syscall_abi::FS_DATA_MAX as usize);
        let mut req = [0u8; ninep_abi::NP_REQ_PAYLOAD as usize + syscall_abi::FS_DATA_MAX as usize];
        req[0..8].copy_from_slice(&ninep_abi::NP_WRITE_FILE.to_le_bytes());
        // tree (a8) and path_len (a16) stay 0; data_len at a1 (offset 24).
        req[24..32].copy_from_slice(&(n as u64).to_le_bytes());
        req[payload_off..payload_off + n].copy_from_slice(&bytes[off..off + n]);
        let mut reply = [0u8; syscall_abi::MSG_MAX_LEN as usize];
        let r = syscall4::<{ syscall_abi::MSG_CALL }>(
            syscall_abi::CON_TASK,
            req.as_ptr() as u64,
            (payload_off + n) as u64,
            reply.as_mut_ptr() as u64,
        );
        if r >= syscall_abi::FS_ERR_MIN {
            for &b in &bytes[off..off + n] {
                syscall4::<{ syscall_abi::PUTC }>(b as u64, 0, 0, 0);
            }
        }
        off += n;
    }
}

/// A syscall, its number `N` a constant. The disk is reached through disk.rs
/// alone (its own `svc`), so a `BLOCK_*` or `MOUNT` number here, a file
/// writing to the disk around its `Disk` and exFAT's gate, does not compile
/// ([`disk::is_device_call`]; `make check-disk-handle` writes one).
#[inline(always)]
pub(crate) fn syscall4<const N: u64>(arg0: u64, arg1: u64, arg2: u64, arg3: u64) -> u64 {
    const { assert!(!disk::is_device_call(N), "a device call is disk.rs's alone") };
    let ret: u64;
    unsafe {
        asm!(
            "svc #0",
            inout("x0") arg0 => ret,
            in("x1") arg1,
            in("x2") arg2,
            in("x3") arg3,
            in("x8") N,
            options(nostack),
        );
    }
    ret
}

/// Five-argument variant for `SAFECOPY`, whose fifth argument
/// (direction) rides in `x4` - the kernel's dispatch reads it from the
/// saved frame, since the 4-argument dispatch signature is full (see
/// the syscall-abi doc on `SAFECOPY`).
#[inline(always)]
pub(crate) fn syscall5<const N: u64>(a0: u64, a1: u64, a2: u64, a3: u64, a4: u64) -> u64 {
    // As syscall4: no device call but disk.rs's.
    const { assert!(!disk::is_device_call(N), "a device call is disk.rs's alone") };
    let ret: u64;
    unsafe {
        asm!(
            "svc #0",
            inout("x0") a0 => ret,
            in("x1") a1,
            in("x2") a2,
            in("x3") a3,
            in("x4") a4,
            in("x8") N,
            options(nostack),
        );
    }
    ret
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
