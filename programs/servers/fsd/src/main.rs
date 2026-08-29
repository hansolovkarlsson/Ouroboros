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
//! **No static mutable state** (the linker asserts `.data`/`.bss`
//! empty, same as every userland program here) - the mounted `Fs`
//! lives in `main`'s own stack frame, which works because this server
//! is one infinite loop that never returns.
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
mod vfs;

use core::arch::asm;
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
    // like `mounts` (fsd keeps no static state).
    let mut fids = [Fid::empty(); MAX_FIDS];
    // Auto-mount if the kernel already holds a device (QEMU's boot-time
    // virtio path; on Parallels the device arrives later, via the
    // `mount` command -> MOUNT syscall -> FSOP_MOUNT request).
    try_mount(&mut mounts[0]);
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
        let packed = syscall4(syscall_abi::MSG_RECV, req.as_mut_ptr() as u64, req.len() as u64, 0, 0);
        if packed >= syscall_abi::FS_ERR_MIN {
            // RECV_INTERRUPTED can't reach a non-keyboard-owner task,
            // and no other error is expected - park rather than spin
            // on a broken call.
            break;
        }
        let sender = packed >> 32;
        let len = ((packed & 0xffff_ffff) as usize).min(req.len());
        let reply_len = handle(&mut mounts, &mut fids, sender, &req[..len], &mut reply);
        // A full/unreachable sender mailbox drops the reply - the
        // caller's MSG_CALL stays blocked until Ctrl+C; nothing better
        // exists to do with an undeliverable reply.
        syscall4(syscall_abi::MSG_SEND, sender, reply.as_ptr() as u64, reply_len as u64, 0);
    }
    loop {
        core::hint::spin_loop();
    }
}

/// Attempts the mount if the kernel has a device installed. Quiet when
/// there's simply no device (the ordinary Parallels-before-`mount`
/// case); logs the outcome whenever a device is actually present.
fn try_mount(fs: &mut Option<vfs::Filesystem>) {
    if fs.is_some() {
        return;
    }
    if disk::Disk.capacity_sectors().is_err() {
        return;
    }
    match vfs::Filesystem::mount(disk::Disk) {
        Ok(mounted) => {
            print("fsd: ");
            print(mounted.name());
            print(" mounted, disk commands available\r\n");
            *fs = Some(mounted);
        }
        Err(e) => {
            print("fsd: mount failed (");
            print(error_name(&e));
            print(") - disk commands won't work\r\n");
        }
    }
}

/// Zero the disk's first `sectors` 512-byte sectors (`0` -> the default
/// [`ERASE_DEFAULT_SECTORS`]), clamped to the disk's capacity. The
/// destructive first step of preparing a blank disk: it removes the
/// partition table and any filesystem metadata near the start, so a
/// subsequent partition/format starts clean. Returns `0`,
/// [`MOUNT_NO_DEVICE`] (no block device), or [`FS_ERR_IO`].
fn erase_disk(sectors: u64) -> u64 {
    let Ok(capacity) = disk::Disk.capacity_sectors() else {
        return syscall_abi::MOUNT_NO_DEVICE;
    };
    let want = if sectors == 0 {
        syscall_abi::ERASE_DEFAULT_SECTORS
    } else {
        sectors
    };
    let count = want.min(capacity);
    let zero = [0u8; 512];
    let mut lba = 0u64;
    while lba < count {
        if disk::Disk.write_sector(lba, &zero).is_err() {
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
fn partition_disk(type_byte: u8) -> u64 {
    let Ok(capacity) = disk::Disk.capacity_sectors() else {
        return syscall_abi::MOUNT_NO_DEVICE;
    };
    let start = syscall_abi::PARTITION_START_LBA;
    if capacity <= start {
        return syscall_abi::FS_ERR_DISK_FULL;
    }
    // MBR partitioning is a 32-bit LBA scheme; a partition past 2 TiB
    // can't be expressed here (that's what GPT, a later step, is for).
    let part_sectors = (capacity - start).min(u32::MAX as u64) as u32;
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

    if disk::Disk.write_sector(0, &mbr).is_err() {
        return syscall_abi::FS_ERR_IO;
    }
    0
}

/// The disk's first MBR partition (start LBA, sector count), or `None` if
/// the disk has no valid MBR (bad boot signature) or no non-empty partition
/// entry. Reads LBA 0's four 16-byte entries at the classic 0x1BE offset -
/// the pairing for [`partition_disk`]'s writer. GPT disks aren't handled here
/// yet (a later step); [`FSOP_FORMAT`] targets the MBR partition table.
fn find_partition() -> Option<(u32, u32)> {
    let mut mbr = [0u8; 512];
    if disk::Disk.read_sector(0, &mut mbr).is_err() {
        return None;
    }
    if mbr[510] != 0x55 || mbr[511] != 0xAA {
        return None;
    }
    for i in 0..4 {
        let e = 0x1BE + i * 16;
        let ptype = mbr[e + 4];
        let start = u32::from_le_bytes([mbr[e + 8], mbr[e + 9], mbr[e + 10], mbr[e + 11]]);
        let size = u32::from_le_bytes([mbr[e + 12], mbr[e + 13], mbr[e + 14], mbr[e + 15]]);
        if ptype != 0 && size != 0 {
            return Some((start, size));
        }
    }
    None
}

/// Lay a fresh filesystem of type `fstype` into the disk's first MBR
/// partition (mkfs). FAT32, exFAT, and ext2 are all supported (milestone 3);
/// any other `fstype` returns [`FS_ERROR`]. Returns `0`, [`MOUNT_NO_DEVICE`],
/// [`FS_ERR_NOT_FOUND`] (no partition - run `partition` first),
/// [`FS_ERR_DISK_FULL`] (partition too small), or [`FS_ERR_IO`].
fn format_disk(fstype: u64) -> u64 {
    if disk::Disk.capacity_sectors().is_err() {
        return syscall_abi::MOUNT_NO_DEVICE;
    }
    let Some((start, sectors)) = find_partition() else {
        return syscall_abi::FS_ERR_NOT_FOUND;
    };
    let result = match fstype {
        syscall_abi::FMT_FAT32 => fat32::Fs::format(disk::Disk, start, sectors),
        syscall_abi::FMT_EXFAT => exfat::Fs::format(disk::Disk, start, sectors),
        syscall_abi::FMT_EXT2 => ext2::Fs::format(disk::Disk, start, sectors),
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
/// table lives on `main`'s stack frame (fsd keeps no static state), and fsd's
/// stack is guard-page-bounded, so this stays modest. If it fills, `NP_OPEN`
/// first reaps fids whose owner task has died (`TASK_STATE`), then fails.
const MAX_FIDS: usize = 8;
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
    owner: u64,
    tree: usize,
    path_len: usize,
    path: [u8; FID_PATH_MAX],
}

impl Fid {
    const fn empty() -> Self {
        Fid { used: false, owner: 0, tree: 0, path_len: 0, path: [0u8; FID_PATH_MAX] }
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
fn handle(mounts: &mut [Option<vfs::Filesystem>; MAX_MOUNTS], fids: &mut [Fid; MAX_FIDS], sender: u64, req: &[u8], reply: &mut [u8]) -> usize {
    if req.len() < REQ_PAYLOAD {
        return status_reply(reply, syscall_abi::FS_ERROR);
    }
    let op = read_u64(req, 0);
    // File operations travel over the uniform verb set (ninep-abi, the Phase 0
    // cluster protocol), carrying a `tree` selector that picks which mount -
    // dispatched to handle_ninep. What remains below is the FSOP_* disk-
    // management control ops (mount/unmount/erase/partition/format, plus
    // FSOP_MOUNT_AT - fsd-specific, not uniform file verbs) plus the SYSOP_PING
    // fall-through.
    if (ninep_abi::NP_BASE..ninep_abi::NP_LIMIT).contains(&op) {
        return handle_ninep(mounts, fids, sender, op, req, reply);
    }
    let p = [read_u64(req, 8), read_u64(req, 16), read_u64(req, 24), read_u64(req, 32)];

    if op == syscall_abi::FSOP_MOUNT {
        // The auto-mount at tree 0 (the default the shell's `mount -a` triggers).
        if mounts[0].is_some() {
            return status_reply(reply, syscall_abi::MOUNT_ALREADY);
        }
        try_mount(&mut mounts[0]);
        let status = if mounts[0].is_some() { 0 } else { syscall_abi::NO_FS };
        return status_reply(reply, status);
    }

    if op == syscall_abi::FSOP_MOUNT_AT {
        // Multi-mount: mount the p[0]-th partition into a fresh tree slot and
        // return the tree id, so a client can bind a namespace prefix to it.
        let index = p[0] as usize;
        let Some(tree) = mounts.iter().position(|m| m.is_none()) else {
            return status_reply(reply, syscall_abi::MOUNT_ALREADY); // no free slot
        };
        match vfs::Filesystem::mount_partition(disk::Disk, index) {
            Ok(fs) => {
                mounts[tree] = Some(fs);
                status_reply(reply, tree as u64)
            }
            Err(_) => status_reply(reply, syscall_abi::NO_FS),
        }
    } else if op == syscall_abi::FSOP_MOUNT_INFO {
        // Report tree 0 (the primary mount), as ever. Handles the not-mounted
        // case itself (NO_FS).
        let Some(mounted) = mounts[0].as_ref() else {
            return status_reply(reply, syscall_abi::NO_FS);
        };
        let part_lba = mounted.partition_lba() as u64;
        let capacity = disk::Disk.capacity_sectors().unwrap_or(0);
        let name = mounted.name().as_bytes();
        reply[0..8].copy_from_slice(&0u64.to_le_bytes());
        reply[8..16].copy_from_slice(&part_lba.to_le_bytes());
        reply[16..24].copy_from_slice(&capacity.to_le_bytes());
        reply[24..24 + name.len()].copy_from_slice(name);
        24 + name.len()
    } else if op == syscall_abi::FSOP_UNMOUNT {
        // Unmount tree 0 (the primary mount).
        if mounts[0].is_none() {
            return status_reply(reply, syscall_abi::NO_FS);
        }
        mounts[0] = None;
        status_reply(reply, 0)
    } else if op == syscall_abi::FSOP_ERASE
        || op == syscall_abi::FSOP_PARTITION
        || op == syscall_abi::FSOP_FORMAT
    {
        // Raw-disk ops (erase/partition/format) rewrite disk structures, so they
        // refuse while *any* tree is mounted - unmount everything first.
        if mounts.iter().any(|m| m.is_some()) {
            return status_reply(reply, syscall_abi::MOUNT_ALREADY);
        }
        let status = match op {
            syscall_abi::FSOP_ERASE => erase_disk(p[0]),
            syscall_abi::FSOP_PARTITION => partition_disk(p[0] as u8),
            _ => format_disk(p[0]),
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
fn handle_ninep(mounts: &mut [Option<vfs::Filesystem>; MAX_MOUNTS], fids: &mut [Fid; MAX_FIDS], sender: u64, verb: u64, req: &[u8], reply: &mut [u8]) -> usize {
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
        return handle_fid_op(mounts, fids, sender, verb, &p, reply);
    }
    // The `tree` selector picks which mount (cluster Phase 0 multi-mount); an
    // out-of-range tree is a client bug. An unmounted tree replies NO_FS.
    if tree >= MAX_MOUNTS {
        return status_reply(reply, syscall_abi::FS_ERROR);
    }
    let Some(fs) = mounts[tree].as_mut() else {
        return status_reply(reply, syscall_abi::NO_FS);
    };
    // Permission enforcement (users/permissions arc, step 3): check the caller's
    // uid/gid against the target's owner+mode before running the op. ext2-only
    // (others return mode:None, which `check_access` treats as unrestricted);
    // root bypasses. See `check_access`.
    if !check_access(fs, sender, verb, &p, payload) {
        return status_reply(reply, syscall_abi::FS_ERR_PERM);
    }
    // New files/dirs are owned by their creator, not root: stamp the caller's
    // identity onto the fs so touch/write/mkdir/open-create use it (ext2 only;
    // other arms ignore it). Root -> (0,0), the default, so nothing changes for
    // boot/format-time creation. Without this a user couldn't write in its own
    // home - the created file would be root-owned and the follow-up write denied.
    let creator = caller_id(sender);
    fs.set_creator(creator.uid as u16, creator.gid as u16);
    match verb {
        ninep_abi::NP_READDIR => {
            let Some(path) = path_from(payload, 0, p[0]) else {
                return status_reply(reply, syscall_abi::FS_ERROR);
            };
            let Some(want) = want_len(p[1]) else {
                return status_reply(reply, syscall_abi::FS_ERROR);
            };
            let mut written = 0usize;
            let (status_slot, result) = reply[..REPLY_PAYLOAD + want].split_at_mut(REPLY_PAYLOAD);
            let outcome = fs.list_dir(path, |name, is_dir, _size| {
                let suffix: &[u8] = if is_dir { b"/\n" } else { b"\n" };
                let entry_len = name.len() + suffix.len();
                if written + entry_len > result.len() {
                    return;
                }
                result[written..written + name.len()].copy_from_slice(name.as_bytes());
                written += name.len();
                result[written..written + suffix.len()].copy_from_slice(suffix);
                written += suffix.len();
            });
            match outcome {
                Ok(()) => {
                    status_slot[..8].copy_from_slice(&(written as u64).to_le_bytes());
                    REPLY_PAYLOAD + written
                }
                Err(e) => status_reply(reply, error_code(&e)),
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
            let offset = p[1];
            let want = (p[2] as usize).min(syscall_abi::SAFECOPY_MAX as usize);
            let mut chunk = [0u8; syscall_abi::SAFECOPY_MAX as usize];
            match fs.read_at(path, offset, &mut chunk[..want]) {
                Ok(copied) => {
                    let copied = copied as usize;
                    if copied > 0 {
                        let r = syscall5(
                            syscall_abi::SAFECOPY,
                            sender,
                            0,
                            chunk.as_ptr() as u64,
                            copied as u64,
                            syscall_abi::GRANT_WRITE,
                        );
                        if r >= syscall_abi::FS_ERR_MIN {
                            return status_reply(reply, syscall_abi::FS_ERROR);
                        }
                    }
                    status_reply(reply, copied as u64)
                }
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
                let r = syscall5(
                    syscall_abi::SAFECOPY,
                    sender,
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
                let r = syscall5(
                    syscall_abi::SAFECOPY,
                    sender,
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
            let Some(path) = path_from(payload, 0, p[1]) else {
                return status_reply(reply, syscall_abi::FS_ERROR);
            };
            let flags = p[0];
            if flags & ninep_abi::OPEN_TRUNC != 0 {
                if let Err(e) = fs.write_file(path, &[]) {
                    return status_reply(reply, error_code(&e));
                }
            } else if flags & ninep_abi::OPEN_CREATE != 0 && fs.stat(path).is_err() {
                if let Err(e) = fs.touch(path) {
                    return status_reply(reply, error_code(&e));
                }
            }
            match alloc_fid(fids, sender, tree, path) {
                Some(fid) => status_reply(reply, fid),
                None => status_reply(reply, syscall_abi::FS_ERROR), // table full
            }
        }
        _ => status_reply(reply, syscall_abi::FS_ERROR),
    }
}

/// Reserve a fid for `owner` referring to `path` on `tree`. Returns the fid
/// number (>= FID_BASE) or `None` if the table is full even after reaping fids
/// whose owner task has died.
fn alloc_fid(fids: &mut [Fid; MAX_FIDS], owner: u64, tree: usize, path: &str) -> Option<u64> {
    let pb = path.as_bytes();
    if pb.len() > FID_PATH_MAX {
        return None;
    }
    let mut slot = fids.iter().position(|f| !f.used);
    if slot.is_none() {
        reap_dead_fids(fids);
        slot = fids.iter().position(|f| !f.used);
    }
    let i = slot?;
    fids[i].used = true;
    fids[i].owner = owner;
    fids[i].tree = tree;
    fids[i].path_len = pb.len();
    fids[i].path[..pb.len()].copy_from_slice(pb);
    Some(i as u64 + FID_BASE)
}

/// Free any fid whose owner task no longer exists (a client that crashed or
/// exited without `NP_CLUNK`) - the pragmatic answer to fsd not being notified
/// of a client's death. Called only when the small fid table is full.
fn reap_dead_fids(fids: &mut [Fid; MAX_FIDS]) {
    for f in fids.iter_mut() {
        if f.used {
            let state = syscall4(syscall_abi::TASK_STATE, f.owner, 0, 0, 0);
            if state == syscall_abi::TASK_STATE_UNUSED || state == syscall_abi::TASK_STATE_ZOMBIE {
                f.used = false;
            }
        }
    }
}

/// Handle a fid op (`a0` = fid): resolve the fid to its owner-checked path +
/// mount, then read/write/stat/clunk. No per-op permission check - authorized at
/// open.
fn handle_fid_op(
    mounts: &mut [Option<vfs::Filesystem>; MAX_MOUNTS],
    fids: &mut [Fid; MAX_FIDS],
    sender: u64,
    verb: u64,
    p: &[u64; 4],
    reply: &mut [u8],
) -> usize {
    let fid = p[0];
    if fid < FID_BASE {
        return status_reply(reply, syscall_abi::FS_ERROR);
    }
    let idx = (fid - FID_BASE) as usize;
    if idx >= MAX_FIDS || !fids[idx].used || fids[idx].owner != sender {
        return status_reply(reply, syscall_abi::FS_ERROR); // bad or not-yours
    }
    if verb == ninep_abi::NP_CLUNK {
        fids[idx].used = false;
        return status_reply(reply, 0);
    }
    let tree = fids[idx].tree;
    let Some(path) = fids[idx].path_str() else {
        return status_reply(reply, syscall_abi::FS_ERROR);
    };
    let Some(fs) = mounts[tree].as_mut() else {
        return status_reply(reply, syscall_abi::NO_FS);
    };
    match verb {
        ninep_abi::NP_PREAD => {
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
        ninep_abi::NP_PWRITE => {
            let data_len = p[2] as usize;
            if data_len > syscall_abi::SAFECOPY_MAX as usize {
                return status_reply(reply, syscall_abi::FS_ERROR);
            }
            let mut databuf = [0u8; syscall_abi::SAFECOPY_MAX as usize];
            if data_len > 0 {
                let r = syscall5(
                    syscall_abi::SAFECOPY,
                    sender,
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

/// A validated result-window size: non-zero, capped at the per-op
/// payload max (which also always fits the reply buffer).
fn want_len(want: u64) -> Option<usize> {
    let want = want as usize;
    if want == 0 || want > DATA_MAX {
        return None;
    }
    Some(want)
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

/// The caller's identity for a permission decision: the uid, the primary gid,
/// and the supplementary groups the kernel carries for that task.
#[derive(Clone, Copy)]
struct Caller {
    uid: u32,
    gid: u32,
    groups: [u32; syscall_abi::MAX_SUPP_GROUPS],
    n_groups: usize,
}

impl Caller {
    /// Whether this caller is in `owner_gid` - as its primary group, or through
    /// any supplementary group. This is the whole of what supplementary groups
    /// buy: the group triad now applies to every group the user belongs to, not
    /// only the one packed into its identity word.
    fn in_group(&self, owner_gid: u32) -> bool {
        self.gid == owner_gid || self.groups[..self.n_groups].contains(&owner_gid)
    }
}

/// Whether `caller` is allowed `need` (an `R`/`W`/`X` bit combination) on an
/// object owned by `owner_uid`/`owner_gid` with `mode`.
fn mode_allows(mode: u16, owner_uid: u32, owner_gid: u32, caller: &Caller, need: u16) -> bool {
    if caller.uid == 0 {
        return true; // root bypasses permission checks
    }
    let triad = if caller.uid == owner_uid {
        (mode >> 6) & 7 // owner rwx
    } else if caller.in_group(owner_gid) {
        (mode >> 3) & 7 // group rwx - primary or supplementary
    } else {
        mode & 7 // other rwx
    };
    triad & need == need
}

/// The calling task's identity from the kernel: the packed `(gid << 32) | uid`
/// (`GET_ID`) plus its supplementary group list (`GET_GROUPS`). Widened to `u32`
/// to compare against the ext2 inode's `u16` owner fields.
fn caller_id(sender: u64) -> Caller {
    let packed = syscall4(syscall_abi::GET_ID, sender, 0, 0, 0);
    let mut groups = [0u32; syscall_abi::MAX_SUPP_GROUPS];
    let n = syscall4(
        syscall_abi::GET_GROUPS,
        sender,
        groups.as_mut_ptr() as u64,
        groups.len() as u64,
        0,
    );
    let n_groups = if n == syscall_abi::GET_ID_ERR {
        0
    } else {
        (n as usize).min(groups.len())
    };
    Caller { uid: packed as u32, gid: (packed >> 32) as u32, groups, n_groups }
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
/// no modes passes, same rule as [`path_allows`].
fn dir_searchable(fs: &mut vfs::Filesystem, dir: &str, who: &Caller) -> bool {
    match fs.stat(dir) {
        Ok(st) => match st.mode {
            Some(m) => mode_allows(m.mode, m.uid as u32, m.gid as u32, who, PERM_X),
            None => true,
        },
        Err(_) => true,
    }
}

/// Whether every **ancestor directory** of `path` grants the caller search
/// (`x`) permission - POSIX path resolution.
///
/// This is what makes `chmod 700 ~` mean something. Without it, a mode on a
/// *directory* only protected the directory's own listing: a caller who already
/// knew the name of a world-readable file inside could still open it by full
/// path, because only the object and its immediate parent were ever checked.
/// Closing that was the deliberate "first cut" the users arc left open.
///
/// Walks `/` first (everything resolves through it), then each `/`-terminated
/// prefix, which yields exactly the ancestors and never the final component.
/// Byte-only: slicing a `&str` by a runtime index pulls in `core::fmt` and an
/// `R_AARCH64_ABS64` the `-pie` link rejects (the recurring trap), so this
/// slices the byte view and re-wraps.
///
/// Cost: one `stat` per ancestor level, on every path op by a non-root caller.
/// Paths here are shallow (`/etc/passwd` is two levels) and root skips the whole
/// check, so this is a small constant, not a walk of the disk.
///
/// A non-directory in the middle of a path passes if its `x` bit happens to be
/// set (an executable, say). POSIX would answer `ENOTDIR`; here the op then
/// fails on its own merits when the filesystem can't walk through a file, which
/// is the same outcome by a different route.
fn ancestors_searchable(fs: &mut vfs::Filesystem, path: &str, who: &Caller) -> bool {
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
///
/// Every ancestor directory must also be searchable: an object you cannot reach
/// is one you cannot use, whatever its own mode says. Checking it here rather
/// than in each verb's arm means every path that goes through this function -
/// which is every path operand except `stat`'s - gets the traversal for free.
fn path_allows(fs: &mut vfs::Filesystem, path: &str, who: &Caller, need: u16) -> bool {
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

/// Enforce the caller's permission for `verb`. Returns `true` if allowed. Reads
/// the object's owner+mode via `stat` and compares against the caller's uid/gid.
fn check_access(fs: &mut vfs::Filesystem, sender: u64, verb: u64, p: &[u64; 4], payload: &[u8]) -> bool {
    let who = caller_id(sender);
    let uid = who.uid;
    if uid == 0 {
        return true; // root: skip the per-op work entirely
    }
    match verb {
        // Reads: R on the target file/dir. Stat is left open (metadata is needed
        // by `ls -l` for entries a readable directory already exposes).
        ninep_abi::NP_STAT => true,
        // open: the fid is authorized here, once. read-open needs R on the file;
        // write/create-open needs W (on the file if it exists, else on the parent
        // dir for a create). a0 = flags, a1 = path length.
        ninep_abi::NP_OPEN => match path_from(payload, 0, p[1]) {
            Some(path) => {
                let flags = p[0];
                if flags & ninep_abi::OPEN_READ != 0 && !path_allows(fs, path, &who, PERM_R) {
                    return false;
                }
                if flags & (ninep_abi::OPEN_WRITE | ninep_abi::OPEN_CREATE) != 0 {
                    let ok = if fs.stat(path).is_ok() {
                        path_allows(fs, path, &who, PERM_W)
                    } else {
                        path_allows(fs, parent_of(path), &who, PERM_W)
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
                        path_allows(fs, parent_of(path), &who, PERM_W)
                    }
                }
                None => true,
            }
        }
        // Namespace changes: W on the parent directory.
        ninep_abi::NP_TOUCH | ninep_abi::NP_MKDIR | ninep_abi::NP_RM | ninep_abi::NP_RMDIR => {
            match path_from(payload, 0, p[0]) {
                Some(path) => path_allows(fs, parent_of(path), &who, PERM_W),
                None => true,
            }
        }
        // Rename: W on both the source and destination parent directories.
        ninep_abi::NP_MV => {
            match (path_from(payload, 0, p[0]), path_from(payload, p[0] as usize, p[1])) {
                (Some(src), Some(dst)) => {
                    path_allows(fs, parent_of(src), &who, PERM_W)
                        && path_allows(fs, parent_of(dst), &who, PERM_W)
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
        _ => true,
    }
}

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
        let r = syscall4(
            syscall_abi::MSG_CALL,
            syscall_abi::CON_TASK,
            req.as_ptr() as u64,
            (payload_off + n) as u64,
            reply.as_mut_ptr() as u64,
        );
        if r >= syscall_abi::FS_ERR_MIN {
            for &b in &bytes[off..off + n] {
                syscall4(syscall_abi::PUTC, b as u64, 0, 0, 0);
            }
        }
        off += n;
    }
}

#[inline(always)]
pub(crate) fn syscall4(number: u64, arg0: u64, arg1: u64, arg2: u64, arg3: u64) -> u64 {
    let ret: u64;
    unsafe {
        asm!(
            "svc #0",
            inout("x0") arg0 => ret,
            in("x1") arg1,
            in("x2") arg2,
            in("x3") arg3,
            in("x8") number,
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
pub(crate) fn syscall5(number: u64, a0: u64, a1: u64, a2: u64, a3: u64, a4: u64) -> u64 {
    let ret: u64;
    unsafe {
        asm!(
            "svc #0",
            inout("x0") a0 => ret,
            in("x1") a1,
            in("x2") a2,
            in("x3") a3,
            in("x4") a4,
            in("x8") number,
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
