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
        let reply_len = handle(&mut mounts, sender, &req[..len], &mut reply);
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

/// Decodes and executes one request from this server's own receive buffer,
/// building the reply (status + inline result) in its own reply buffer. Returns
/// the reply's total length. Every slice below is into `req`/`reply` -
/// server-owned memory only. `sender` is the calling task, needed by the bulk
/// ops (`NP_READ`/`NP_WRITE`/`NP_WRITE_AT`) to `SAFECOPY` against the client's
/// grant (they move their data directly between task regions rather than inline
/// in the reply).
fn handle(mounts: &mut [Option<vfs::Filesystem>; MAX_MOUNTS], sender: u64, req: &[u8], reply: &mut [u8]) -> usize {
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
        return handle_ninep(mounts, sender, op, req, reply);
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
fn handle_ninep(mounts: &mut [Option<vfs::Filesystem>; MAX_MOUNTS], sender: u64, verb: u64, req: &[u8], reply: &mut [u8]) -> usize {
    const NP_HDR: usize = ninep_abi::NP_REQ_PAYLOAD as usize;
    if req.len() < NP_HDR {
        return status_reply(reply, syscall_abi::FS_ERROR);
    }
    let tree = read_u64(req, 8) as usize;
    let p = [read_u64(req, 16), read_u64(req, 24), read_u64(req, 32), read_u64(req, 40)];
    let payload = &req[NP_HDR..];
    // The `tree` selector picks which mount (cluster Phase 0 multi-mount); an
    // out-of-range tree is a client bug. An unmounted tree replies NO_FS.
    if tree >= MAX_MOUNTS {
        return status_reply(reply, syscall_abi::FS_ERROR);
    }
    let Some(fs) = mounts[tree].as_mut() else {
        return status_reply(reply, syscall_abi::NO_FS);
    };
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
                Ok(st) => {
                    let (status_slot, result) =
                        reply[..REPLY_PAYLOAD + ninep_abi::STAT_INFO_LEN].split_at_mut(REPLY_PAYLOAD);
                    result.fill(0); // unset fields (time when absent) read as zero
                    let mut flags: u32 = 0;
                    if st.is_dir {
                        flags |= ninep_abi::STAT_FLAG_DIR;
                    }
                    result[ninep_abi::STAT_SIZE_OFF..ninep_abi::STAT_SIZE_OFF + 8]
                        .copy_from_slice(&st.size.to_le_bytes());
                    result[ninep_abi::STAT_FLAGS_OFF..ninep_abi::STAT_FLAGS_OFF + 4]
                        .copy_from_slice(&flags.to_le_bytes());
                    if let Some(t) = st.time {
                        result[ninep_abi::STAT_YEAR_OFF..ninep_abi::STAT_YEAR_OFF + 2]
                            .copy_from_slice(&t.year.to_le_bytes());
                        result[ninep_abi::STAT_MONTH_OFF] = t.month;
                        result[ninep_abi::STAT_DAY_OFF] = t.day;
                        result[ninep_abi::STAT_HOUR_OFF] = t.hour;
                        result[ninep_abi::STAT_MIN_OFF] = t.min;
                        result[ninep_abi::STAT_SEC_OFF] = t.sec;
                        result[ninep_abi::STAT_TIMEVALID_OFF] = 1;
                    }
                    status_slot[..8].copy_from_slice(&(ninep_abi::STAT_INFO_LEN as u64).to_le_bytes());
                    REPLY_PAYLOAD + ninep_abi::STAT_INFO_LEN
                }
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
        _ => status_reply(reply, syscall_abi::FS_ERROR),
    }
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
