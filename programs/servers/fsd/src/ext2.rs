//! ext2, read-write - the third on-disk format `fsd` understands, and the one
//! that actually tests the [`Filesystem`](crate::vfs::Filesystem) abstraction.
//! FAT32 and exFAT are the *same shape* (a partition, a FAT, a heap of clusters,
//! directory entries in a flat list), so a thin enum sufficed. ext2 is a
//! genuinely different model - and driving it through the unchanged `FSOP_*`
//! protocol is the proof the abstraction is real, not FAT-shaped in disguise.
//! Landed read-only first, then read-write in four staged commits (allocation +
//! `touch`; `write_file`/`write_at`; `mkdir`/`rm`/`rmdir`; `mv`).
//!
//! Same constraints as everything in `fsd`: hand-rolled, fixed-buffer, no
//! `alloc`, on [`Disk`]'s `BLOCK_*` syscall shim.
//!
//! **Write model.** Allocation is bitmap-based: each block group has a block
//! bitmap and an inode bitmap, and every allocation keeps the free counts in the
//! group descriptor *and* the superblock consistent (e2fsck checks all three).
//! Files use direct + single/double indirect block pointers ([`Fs::ensure_block`]);
//! directories track link counts (`mkdir` bumps the parent, `rmdir` decrements
//! it) and `bg_used_dirs_count`. A freed inode gets links 0 + a plausible
//! `i_dtime` (a small value is misread by e2fsck as an orphan-list pointer).
//! Same claim-before-use / write-new-before-free discipline as the FAT arcs;
//! validated end to end with e2fsck (clean) and debugfs (byte-identical reads).
//!
//! # The ext2 model, versus FAT
//!
//! - **Inodes, not directory entries, own a file's metadata.** A directory
//!   entry is just `(name -> inode number)`; the inode (in a fixed inode table)
//!   holds the mode, size, and block pointers. So resolving a path is: read the
//!   directory's inode, walk its data blocks for the name, get an inode number,
//!   read *that* inode, repeat. [`Fs::read_inode`] / [`Fs::find`].
//! - **Block groups.** The volume is split into groups; a block group
//!   descriptor table (right after the superblock) records each group's inode
//!   table location. Inode *N* lives in group `(N-1)/inodes_per_group` at index
//!   `(N-1) % inodes_per_group`. [`Fs::inode_pos`].
//! - **Direct + indirect block pointers.** An inode has 15 block pointers: 12
//!   direct, then single / double / triple indirect (pointer blocks of pointer
//!   blocks). [`Fs::block_for`] maps a file's logical block index to a physical
//!   block, following one or two levels of indirection (triple is not needed
//!   for the file sizes here and is treated as EOF). A pointer of `0` is a
//!   sparse hole - read as zeros.
//! - **Case-sensitive names** (Unix), unlike FAT/exFAT's case-fold - [`Fs::find`]
//!   matches exactly. Directory entries are a linked list of variable-length
//!   records (`rec_len`) within the directory's data blocks.
//!
//! **Scope.** The `FSOP_*` protocol is FAT-shaped (no permissions, owners, or
//! symlinks), so this presents files and directories and ignores the Unix
//! metadata it can't model - created entries get fixed 0644/0755 modes, and
//! symlinks are reported but not followed. Root is always inode 2.

use crate::disk::Disk;
use crate::fat32::{split_parent, Error};

const SECTOR_SIZE: usize = 512;
/// The ext2 superblock always starts 1024 bytes into the volume, whatever the
/// block size.
const SUPERBLOCK_OFFSET: u64 = 1024;
const EXT2_MAGIC: u16 = 0xef53;
const ROOT_INO: u32 = 2;

/// `i_mode` top-nibble file-type bits.
const S_IFMT: u16 = 0xf000;
const S_IFREG: u16 = 0x8000; // regular file
const S_IFDIR: u16 = 0x4000; // directory

/// Default modes for entries this driver creates: a 0644 regular file, a 0755
/// directory (the file-type bits ORed with rwx bits).
const NEW_FILE_MODE: u16 = S_IFREG | 0o644;
#[allow(dead_code)] // Stage C (mkdir)
const NEW_DIR_MODE: u16 = S_IFDIR | 0o755;

/// Directory-entry `file_type` values (when the `filetype` feature is present).
const FT_REG: u8 = 1;
#[allow(dead_code)] // Stage C (mkdir)
const FT_DIR: u8 = 2;

/// `s_feature_incompat` bit for the `filetype` feature.
const INCOMPAT_FILETYPE: u32 = 0x0002;

/// Direct block pointers in an inode (indices 0..12); [12]/[13]/[14] are the
/// single / double / triple indirect pointers.
const DIRECT_BLOCKS: usize = 12;
const INODE_BLOCK_PTRS: usize = 15;

/// Largest block size we handle (a 4 KiB working buffer covers 1024/2048/4096).
const MAX_BLOCK_SIZE: usize = 4096;

/// Longest name we keep (ext2 allows 255).
const NAME_MAX: usize = 255;

/// Cap on a single `write_at` gap zero-fill past EOF (same as the FAT arcs), so
/// a fat-fingered huge offset can't try to fill the whole volume.
const MAX_GAP_FILL: u64 = 1 << 20;

/// `i_dtime` written on a freed inode. It must be a plausible *timestamp*, not a
/// small integer: e2fsck treats a links-0 inode whose `i_dtime` is `< s_inodes_
/// count` as sitting on the orphan list (with `i_dtime` as the next-orphan inode
/// pointer), so a small sentinel like `1` is misread as "next orphan = inode 1".
/// We have no RTC, so a fixed constant well above any inode count is used
/// (0x4000_0000 ~= a 2004 Unix time).
const DELETION_TIME: u32 = 0x4000_0000;

/// A decoded inode - only the fields a read-only driver needs.
#[derive(Clone, Copy)]
struct Inode {
    mode: u16,
    /// Owning user id (`i_uid`) - read for the stat mode/owner surface; the
    /// write path leaves created inodes root-owned (`0`), which `write_inode`'s
    /// slot-zeroing already guarantees.
    uid: u16,
    /// Owning group id (`i_gid`), same treatment as [`uid`](Self::uid).
    gid: u16,
    /// Hard-link count (`i_links_count`) - needed by the write path (`rm`
    /// decrements it and frees the inode at zero; `mkdir` bumps the parent's).
    links: u16,
    size: u32,
    /// `i_blocks`, in 512-byte units (not filesystem blocks) - the write path
    /// keeps it consistent as it allocates/frees blocks.
    i_blocks: u32,
    /// The 15 block pointers, verbatim.
    block: [u32; INODE_BLOCK_PTRS],
}

impl Inode {
    fn is_dir(&self) -> bool {
        self.mode & S_IFMT == S_IFDIR
    }
    fn is_reg(&self) -> bool {
        self.mode & S_IFMT == S_IFREG
    }
}

/// One directory entry, decoded into a self-contained form.
#[derive(Clone, Copy)]
struct DirEntry {
    name: [u8; NAME_MAX],
    name_len: u8,
    inode: u32,
}

impl DirEntry {
    fn name(&self) -> &str {
        core::str::from_utf8(&self.name[..self.name_len as usize]).unwrap_or("")
    }
}

pub struct Fs {
    disk: Disk,
    /// Absolute LBA of the partition's first sector (byte 0 of the volume).
    part_lba: u32,
    /// Block size in bytes (1024 << s_log_block_size).
    block_size: u32,
    inodes_per_group: u32,
    inode_size: u32,
    /// Block number of the block group descriptor table.
    bgdt_block: u32,
    // ---- fields the write path needs (Stage A onward) --------------------
    /// Blocks per group (for locating a group's block bitmap coverage).
    blocks_per_group: u32,
    /// The superblock's block number (`s_first_data_block`): 1 for 1 KiB
    /// blocks, 0 otherwise. Block bitmaps count from here.
    first_data_block: u32,
    /// First non-reserved inode - new files/dirs allocate at or above it.
    #[allow(dead_code)]
    first_ino: u32,
    /// Number of block groups (`ceil(inodes_count / inodes_per_group)`).
    groups_count: u32,
    /// Whether the volume has the `filetype` incompat feature - if so, byte 7 of
    /// a directory entry is `file_type`; if not, it's the high byte of a u16
    /// `name_len` (so writes must leave it `0`). mke2fs sets this by default.
    has_filetype: bool,
    /// The `(uid, gid)` new inodes are created with, set per-op by
    /// [`set_creator`](Self::set_creator) from the calling task's identity so a
    /// file/dir is owned by whoever creates it (the permission model needs this -
    /// a user must own what it makes in its home). Defaults to root `(0, 0)` for
    /// boot/format-time creation and any caller fsd doesn't set it for.
    creator_uid: u16,
    creator_gid: u16,
}

impl Fs {
    /// Mount the ext2 volume at `partition_lba`, reading and validating the
    /// superblock. Returns [`Error::NotExt2`] if the `0xEF53` magic is absent,
    /// so `vfs::mount` can try the next partition/format.
    pub fn mount_at(mut disk: Disk, partition_lba: u32) -> Result<Self, Error> {
        // The superblock sits 1024 bytes in; with 512-byte sectors that's the
        // second sector of the partition. Read it.
        let mut sb = [0u8; SECTOR_SIZE];
        let sb_lba = partition_lba as u64 + SUPERBLOCK_OFFSET / SECTOR_SIZE as u64;
        disk.read_sector(sb_lba, &mut sb)?;

        // s_magic is at offset 56 within the superblock (== +56 in this sector,
        // since the superblock starts exactly at a sector boundary here).
        let magic = u16::from_le_bytes([sb[56], sb[57]]);
        if magic != EXT2_MAGIC {
            return Err(Error::NotExt2);
        }

        let log_block_size = u32::from_le_bytes([sb[24], sb[25], sb[26], sb[27]]);
        let block_size = 1024u32 << log_block_size;
        if block_size as usize > MAX_BLOCK_SIZE {
            return Err(Error::NotExt2); // unsupported (>4 KiB) block size
        }
        let first_data_block = u32::from_le_bytes([sb[20], sb[21], sb[22], sb[23]]);
        let inodes_per_group = u32::from_le_bytes([sb[40], sb[41], sb[42], sb[43]]);
        // s_rev_level at 76: rev 0 has a fixed 128-byte inode; rev 1 stores
        // s_inode_size at offset 88.
        let rev_level = u32::from_le_bytes([sb[76], sb[77], sb[78], sb[79]]);
        let inode_size = if rev_level >= 1 {
            u16::from_le_bytes([sb[88], sb[89]]) as u32
        } else {
            128
        };
        if inode_size == 0 || inodes_per_group == 0 {
            return Err(Error::NotExt2);
        }
        let inodes_count = u32::from_le_bytes([sb[0], sb[1], sb[2], sb[3]]);
        let blocks_per_group = u32::from_le_bytes([sb[32], sb[33], sb[34], sb[35]]);
        let first_ino = if rev_level >= 1 {
            u32::from_le_bytes([sb[84], sb[85], sb[86], sb[87]])
        } else {
            11
        };
        if blocks_per_group == 0 {
            return Err(Error::NotExt2);
        }

        Ok(Fs {
            disk,
            part_lba: partition_lba,
            block_size,
            inodes_per_group,
            inode_size,
            // The block group descriptor table is the block right after the one
            // holding the superblock (first_data_block).
            bgdt_block: first_data_block + 1,
            blocks_per_group,
            first_data_block,
            first_ino,
            groups_count: inodes_count.div_ceil(inodes_per_group),
            has_filetype: u32::from_le_bytes([sb[96], sb[97], sb[98], sb[99]]) & INCOMPAT_FILETYPE
                != 0,
            creator_uid: 0,
            creator_gid: 0,
        })
    }

    /// Set the owner new inodes are created with (the calling task's uid/gid).
    /// fsd calls this before each op so `touch`/`write_file`/`mkdir` produce
    /// files owned by their creator, not root - the permission model requires it.
    pub fn set_creator(&mut self, uid: u16, gid: u16) {
        self.creator_uid = uid;
        self.creator_gid = gid;
    }

    /// The volume's first sector - for `mount`-info reporting only.
    pub fn partition_lba(&self) -> u32 {
        self.part_lba
    }

    /// Create a fresh ext2 filesystem (mkfs) in the partition
    /// `[start_lba, start_lba + total_sectors)` - the inverse of
    /// [`mount_at`](Self::mount_at). The disk-management arc, milestone 3's
    /// final step (ext2).
    ///
    /// Deliberately minimal but e2fsck-clean: **one block group**, 4 KiB
    /// blocks (so `s_first_data_block` is `0` - no 1 KiB boot-block special
    /// case), a 128-byte inode, and the `filetype` incompat feature - exactly
    /// the shape [`mount_at`](Self::mount_at) re-derives. It writes the
    /// superblock, the single block-group descriptor, the block + inode
    /// bitmaps, a zeroed inode table carrying the root (inode 2) and
    /// `lost+found` (inode 11) directories, and those two directory data
    /// blocks. No backup superblock is needed (single group), and
    /// `sparse_super`/`resize`/`large_file` are all off - the plain old-style
    /// ext2 layout `e2fsck` still fully validates.
    ///
    /// Being single-group caps the filesystem at one group's worth of 4 KiB
    /// blocks (`8 * 4096 = 32768` blocks = 128 MiB); a larger partition is
    /// formatted to 128 MiB with the remainder unused - fine for the modest
    /// QEMU volumes this targets (multi-group mkfs is future work). Returns
    /// [`Error::DiskFull`] if the partition is too small for the fixed
    /// structures, or [`Error::Io`] on a write error. **Cost is O(inode-table
    /// size) single-sector writes**, like the other arms' `format`.
    pub fn format(mut disk: Disk, start_lba: u32, total_sectors: u32) -> Result<(), Error> {
        const BLK: u32 = FMT_BLOCK as u32; // 4096
        const LOG_BLK: u32 = 2; // 1024 << 2 == 4096
        const INODE_SZ: u32 = 128;
        const INODES_PER_BLK: u32 = BLK / INODE_SZ; // 32
        const BLOCKS_PER_GROUP: u32 = 8 * BLK; // one block bitmap covers this
        const FIRST_INO: u32 = 11; // inodes 1..10 reserved; 11 == lost+found

        // Whole 4 KiB blocks the partition holds, capped to a single group.
        let part_blocks = (total_sectors as u64 * SECTOR_SIZE as u64 / BLK as u64) as u32;
        let blocks_count = part_blocks.min(BLOCKS_PER_GROUP);

        // Fixed single-group layout (first_data_block == 0 for >= 2 KiB blocks):
        //   0 boot+superblock | 1 GDT | 2 block bitmap | 3 inode bitmap
        //   4.. inode table   | then the root dir, then lost+found data block.
        let inodes_count = {
            let target = (blocks_count / 4).clamp(16, 8192);
            target.div_ceil(INODES_PER_BLK) * INODES_PER_BLK // whole inode-table blocks
        };
        let inode_table_blocks = inodes_count / INODES_PER_BLK;
        let block_bitmap_block = 2u32;
        let inode_bitmap_block = 3u32;
        let inode_table_block = 4u32;
        let root_block = inode_table_block + inode_table_blocks;
        let lf_block = root_block + 1;
        if blocks_count <= lf_block + 1 {
            return Err(Error::DiskFull); // no room for the structures + two dirs
        }

        let free_blocks = blocks_count - (lf_block + 1); // blocks 0..=lf_block used
        let free_inodes = inodes_count - FIRST_INO; // inodes 1..=11 used

        // One reused 4 KiB scratch block - fsd's stack can't hold a separate
        // buffer per structure (eight would overflow its guard page), so each
        // step clears `buf`, fills it, and writes it.
        let mut buf = [0u8; FMT_BLOCK];

        // ---- block 0: boot area (zeroed) + the 1024-byte superblock ----------
        {
            let sb = &mut buf[1024..2048];
            put32(sb, 0, inodes_count);
            put32(sb, 4, blocks_count);
            put32(sb, 8, 0); // s_r_blocks_count (no reservation)
            put32(sb, 12, free_blocks);
            put32(sb, 16, free_inodes);
            put32(sb, 20, 0); // s_first_data_block (0 for >= 2 KiB blocks)
            put32(sb, 24, LOG_BLK);
            put32(sb, 28, LOG_BLK); // s_log_frag_size
            put32(sb, 32, BLOCKS_PER_GROUP);
            put32(sb, 36, BLOCKS_PER_GROUP); // s_frags_per_group
            put32(sb, 40, inodes_count); // s_inodes_per_group (single group)
            put32(sb, 48, MKFS_TIME); // s_wtime
            put16(sb, 54, 0xFFFF); // s_max_mnt_count = -1 (unlimited)
            put16(sb, 56, EXT2_MAGIC);
            put16(sb, 58, 1); // s_state = EXT2_VALID_FS
            put16(sb, 60, 1); // s_errors = continue
            put32(sb, 64, MKFS_TIME); // s_lastcheck
            put32(sb, 76, 1); // s_rev_level = DYNAMIC_REV
            put32(sb, 84, FIRST_INO); // s_first_ino
            put16(sb, 88, INODE_SZ as u16); // s_inode_size
            put32(sb, 96, INCOMPAT_FILETYPE); // s_feature_incompat
            sb[104..120].copy_from_slice(&FS_UUID); // nonzero so e2fsck won't offer one
            let name = b"OUROBOROS";
            sb[120..120 + name.len()].copy_from_slice(name); // s_volume_name
        }
        write_fmt_block(&mut disk, start_lba, 0, &buf)?;

        // ---- block 1: the single block-group descriptor ----------------------
        buf.fill(0);
        put32(&mut buf, 0, block_bitmap_block); // bg_block_bitmap
        put32(&mut buf, 4, inode_bitmap_block); // bg_inode_bitmap
        put32(&mut buf, 8, inode_table_block); // bg_inode_table
        put16(&mut buf, 12, free_blocks as u16); // bg_free_blocks_count
        put16(&mut buf, 14, free_inodes as u16); // bg_free_inodes_count
        put16(&mut buf, 16, 2); // bg_used_dirs_count (root + lost+found)
        write_fmt_block(&mut disk, start_lba, 1, &buf)?;

        // ---- block 2: block bitmap -------------------------------------------
        // Bit i == block i (first_data_block is 0). Used: 0..=lf_block. Every
        // bit for a block that doesn't exist (>= blocks_count) is set too.
        buf.fill(0);
        for b in 0..=lf_block {
            set_bit(&mut buf, b);
        }
        for b in blocks_count..BLOCKS_PER_GROUP {
            set_bit(&mut buf, b);
        }
        write_fmt_block(&mut disk, start_lba, block_bitmap_block, &buf)?;

        // ---- block 3: inode bitmap -------------------------------------------
        // Bit i == inode (i+1). Used: inodes 1..=11 (bits 0..=10). Bits for
        // inodes >= inodes_count are set (they don't exist).
        buf.fill(0);
        for i in 0..FIRST_INO {
            set_bit(&mut buf, i); // bits 0..11 -> inodes 1..11
        }
        for i in inodes_count..BLOCKS_PER_GROUP {
            set_bit(&mut buf, i);
        }
        write_fmt_block(&mut disk, start_lba, inode_bitmap_block, &buf)?;

        // ---- inode table: root (2) + lost+found (11), then zeroed remainder --
        // Both fall in the first table block (32 inodes/block).
        let dir_mode = S_IFDIR | 0o755;
        let ib = BLK / SECTOR_SIZE as u32; // i_blocks (512-byte units) for one 4 KiB block
        buf.fill(0);
        write_inode_slot(&mut buf, ROOT_INO, dir_mode, BLK, 3, ib, root_block);
        write_inode_slot(&mut buf, FIRST_INO, dir_mode, BLK, 2, ib, lf_block);
        write_fmt_block(&mut disk, start_lba, inode_table_block, &buf)?;
        buf.fill(0);
        for b in 1..inode_table_blocks {
            write_fmt_block(&mut disk, start_lba, inode_table_block + b, &buf)?;
        }

        // ---- root directory data block ---------------------------------------
        buf.fill(0);
        write_dirent(&mut buf[0..], ROOT_INO, 12, ".", FT_DIR);
        write_dirent(&mut buf[12..], ROOT_INO, 12, "..", FT_DIR);
        write_dirent(&mut buf[24..], FIRST_INO, FMT_BLOCK - 24, "lost+found", FT_DIR);
        write_fmt_block(&mut disk, start_lba, root_block, &buf)?;

        // ---- lost+found directory data block ---------------------------------
        buf.fill(0);
        write_dirent(&mut buf[0..], FIRST_INO, 12, ".", FT_DIR);
        write_dirent(&mut buf[12..], ROOT_INO, FMT_BLOCK - 12, "..", FT_DIR);
        write_fmt_block(&mut disk, start_lba, lf_block, &buf)?;

        Ok(())
    }

    /// Absolute LBA of the first sector of filesystem block `block`.
    fn block_lba(&self, block: u32) -> u64 {
        self.part_lba as u64 + block as u64 * (self.block_size as u64 / SECTOR_SIZE as u64)
    }

    /// Read one filesystem block into `buf` (`buf` must be >= block_size).
    fn read_block(&mut self, block: u32, buf: &mut [u8]) -> Result<(), Error> {
        let lba = self.block_lba(block);
        let sectors = self.block_size as usize / SECTOR_SIZE;
        for s in 0..sectors {
            let mut sec = [0u8; SECTOR_SIZE];
            self.disk.read_sector(lba + s as u64, &mut sec)?;
            buf[s * SECTOR_SIZE..(s + 1) * SECTOR_SIZE].copy_from_slice(&sec);
        }
        Ok(())
    }

    /// Read a `u32` at absolute byte `offset` (used for a single pointer inside
    /// an indirect block, without buffering the whole block).
    fn read_u32_at(&mut self, byte_offset: u64) -> Result<u32, Error> {
        let lba = self.part_lba as u64 + byte_offset / SECTOR_SIZE as u64;
        let within = (byte_offset % SECTOR_SIZE as u64) as usize;
        let mut sec = [0u8; SECTOR_SIZE];
        self.disk.read_sector(lba, &mut sec)?;
        Ok(u32::from_le_bytes([
            sec[within],
            sec[within + 1],
            sec[within + 2],
            sec[within + 3],
        ]))
    }

    /// One pointer out of a pointer block: entry `index` of the block `block`.
    fn indirect_lookup(&mut self, block: u32, index: u32) -> Result<u32, Error> {
        if block == 0 {
            return Ok(0);
        }
        let byte = block as u64 * self.block_size as u64 + index as u64 * 4;
        self.read_u32_at(byte)
    }

    /// Map a file's logical block index to its physical block number (`0` = a
    /// sparse hole, read as zeros). Direct, then single/double indirect; triple
    /// indirect is beyond the sizes here and returns `0` (EOF).
    fn block_for(&mut self, inode: &Inode, logical: u32) -> Result<u32, Error> {
        let ptrs_per_block = self.block_size / 4; // pointers in an indirect block
        let l = logical as u64;

        if logical < DIRECT_BLOCKS as u32 {
            return Ok(inode.block[logical as usize]);
        }
        let mut idx = l - DIRECT_BLOCKS as u64;
        let k = ptrs_per_block as u64;

        // Single indirect.
        if idx < k {
            return self.indirect_lookup(inode.block[12], idx as u32);
        }
        idx -= k;

        // Double indirect.
        if idx < k * k {
            let first = self.indirect_lookup(inode.block[13], (idx / k) as u32)?;
            return self.indirect_lookup(first, (idx % k) as u32);
        }

        // Triple indirect: not needed for the file sizes here.
        Ok(0)
    }

    /// Read inode number `ino` (1-based) from its block group's inode table.
    fn read_inode(&mut self, ino: u32) -> Result<Inode, Error> {
        if ino == 0 {
            return Err(Error::NotFound);
        }
        let group = (ino - 1) / self.inodes_per_group;
        let index = (ino - 1) % self.inodes_per_group;

        // Block group descriptor: 32 bytes each, in the BGDT; bg_inode_table is
        // at offset 8 within the descriptor.
        let desc_byte = self.bgdt_block as u64 * self.block_size as u64 + group as u64 * 32;
        let inode_table = self.read_u32_at(desc_byte + 8)?;

        // The inode itself.
        let inode_byte =
            inode_table as u64 * self.block_size as u64 + index as u64 * self.inode_size as u64;
        let lba = self.part_lba as u64 + inode_byte / SECTOR_SIZE as u64;
        let within = (inode_byte % SECTOR_SIZE as u64) as usize;

        // An inode (128 bytes of the fields we read) can straddle two sectors,
        // so read two consecutive sectors into a window and index from `within`.
        let mut win = [0u8; SECTOR_SIZE * 2];
        let mut sec = [0u8; SECTOR_SIZE];
        self.disk.read_sector(lba, &mut sec)?;
        win[..SECTOR_SIZE].copy_from_slice(&sec);
        self.disk.read_sector(lba + 1, &mut sec)?;
        win[SECTOR_SIZE..].copy_from_slice(&sec);

        let i = &win[within..within + 128];
        let mode = u16::from_le_bytes([i[0], i[1]]);
        let uid = u16::from_le_bytes([i[2], i[3]]);
        let size = u32::from_le_bytes([i[4], i[5], i[6], i[7]]);
        let gid = u16::from_le_bytes([i[24], i[25]]);
        let links = u16::from_le_bytes([i[26], i[27]]);
        let i_blocks = u32::from_le_bytes([i[28], i[29], i[30], i[31]]);
        let mut block = [0u32; INODE_BLOCK_PTRS];
        for (b, slot) in block.iter_mut().enumerate() {
            let o = 40 + b * 4;
            *slot = u32::from_le_bytes([i[o], i[o + 1], i[o + 2], i[o + 3]]);
        }
        Ok(Inode {
            mode,
            uid,
            gid,
            links,
            size,
            i_blocks,
            block,
        })
    }

    /// Call `f` for each entry in directory `inode`, stopping early when it
    /// returns `true`. Walks the directory's data blocks; each block holds a
    /// linked list of `(inode, rec_len, name_len, file_type, name)` records.
    fn walk_dir(
        &mut self,
        inode: &Inode,
        mut f: impl FnMut(&DirEntry) -> bool,
    ) -> Result<(), Error> {
        let total = inode.size as u64;
        let bs = self.block_size as usize;
        let mut buf = [0u8; MAX_BLOCK_SIZE];
        let mut pos = 0u64; // byte position within the directory
        let mut logical = 0u32;

        while pos < total {
            let phys = self.block_for(inode, logical)?;
            if phys != 0 {
                self.read_block(phys, &mut buf[..bs])?;
                let mut off = 0usize;
                while off + 8 <= bs {
                    let e = &buf[off..];
                    let entry_ino = u32::from_le_bytes([e[0], e[1], e[2], e[3]]);
                    let rec_len = u16::from_le_bytes([e[4], e[5]]) as usize;
                    let name_len = e[6] as usize;
                    if rec_len < 8 || off + rec_len > bs {
                        break; // malformed - stop scanning this block
                    }
                    if entry_ino != 0 && name_len > 0 && off + 8 + name_len <= bs {
                        let mut de = DirEntry {
                            name: [0; NAME_MAX],
                            name_len: name_len.min(NAME_MAX) as u8,
                            inode: entry_ino,
                        };
                        let n = name_len.min(NAME_MAX);
                        de.name[..n].copy_from_slice(&buf[off + 8..off + 8 + n]);
                        if f(&de) {
                            return Ok(());
                        }
                    }
                    off += rec_len;
                }
            }
            pos += bs as u64;
            logical += 1;
        }
        Ok(())
    }

    /// Resolve an absolute, `/`-separated path to its inode. Case-sensitive
    /// (Unix), unlike FAT/exFAT. `.`/`..` resolve naturally (they're real
    /// directory entries in ext2).
    fn find(&mut self, path: &str) -> Result<Inode, Error> {
        Ok(self.resolve(path)?.1)
    }

    /// Like [`find`](Self::find), but also returns the inode *number* - the
    /// write path needs it to write an inode back after modifying it.
    fn resolve(&mut self, path: &str) -> Result<(u32, Inode), Error> {
        let mut ino = ROOT_INO;
        let mut inode = self.read_inode(ino)?;
        for component in path.split('/').filter(|c| !c.is_empty()) {
            if !inode.is_dir() {
                return Err(Error::NotADirectory);
            }
            let mut found: Option<u32> = None;
            self.walk_dir(&inode, |entry| {
                if entry.name() == component {
                    found = Some(entry.inode);
                    true
                } else {
                    false
                }
            })?;
            ino = found.ok_or(Error::NotFound)?;
            inode = self.read_inode(ino)?;
        }
        Ok((ino, inode))
    }

    /// Lists a directory, calling `f(name, is_dir, size)` for each entry. `""`
    /// or `"/"` lists the root. Skips the `.`/`..` self/parent links (FAT/exFAT
    /// don't surface those, so this matches their `list_dir`). The scan is
    /// inlined rather than reusing [`walk_dir`](Self::walk_dir) so each entry's
    /// inode can be read *during* the walk for its kind and size (a closure
    /// can't, since it can't also borrow `self`); it needs no per-entry `alloc`.
    pub fn list_dir(
        &mut self,
        path: &str,
        mut f: impl FnMut(&str, bool, u32),
    ) -> Result<(), Error> {
        let dir = self.find(path)?;
        if !dir.is_dir() {
            return Err(Error::NotADirectory);
        }
        let bs = self.block_size as usize;
        let total = dir.size as u64;
        let mut buf = [0u8; MAX_BLOCK_SIZE];
        let mut pos = 0u64;
        let mut logical = 0u32;
        while pos < total {
            let phys = self.block_for(&dir, logical)?;
            if phys != 0 {
                self.read_block(phys, &mut buf[..bs])?;
                let mut off = 0usize;
                while off + 8 <= bs {
                    let e = &buf[off..];
                    let entry_ino = u32::from_le_bytes([e[0], e[1], e[2], e[3]]);
                    let rec_len = u16::from_le_bytes([e[4], e[5]]) as usize;
                    let name_len = e[6] as usize;
                    if rec_len < 8 || off + rec_len > bs {
                        break; // malformed - stop scanning this block
                    }
                    if entry_ino != 0 && name_len > 0 && off + 8 + name_len <= bs {
                        let n = name_len.min(NAME_MAX);
                        let mut namebuf = [0u8; NAME_MAX];
                        namebuf[..n].copy_from_slice(&buf[off + 8..off + 8 + n]);
                        let name = core::str::from_utf8(&namebuf[..n]).unwrap_or("");
                        if name != "." && name != ".." {
                            let child = self.read_inode(entry_ino)?;
                            f(name, child.is_dir(), child.size);
                        }
                    }
                    off += rec_len;
                }
            }
            pos += bs as u64;
            logical += 1;
        }
        Ok(())
    }

    /// Reads a file into `buf` (up to `buf.len()` bytes), returning its real
    /// size - same truncation-detection contract as the other arms.
    pub fn read_file(&mut self, path: &str, buf: &mut [u8]) -> Result<u32, Error> {
        let inode = self.find(path)?;
        if !inode.is_reg() {
            return Err(Error::NotAFile); // directory / symlink / device / etc
        }
        self.read_range(&inode, 0, buf)?;
        Ok(inode.size)
    }

    /// Metadata for one path: size, directory flag, and the real POSIX
    /// mode/owner from the inode (ext2 is the one arm that stores them). The
    /// inode's `i_mtime` isn't parsed yet, so `time` is `None`. Backs `ls -l`.
    pub fn stat(&mut self, path: &str) -> Result<crate::vfs::Stat, Error> {
        let inode = self.find(path)?;
        Ok(crate::vfs::Stat {
            size: inode.size as u64,
            is_dir: inode.is_dir(),
            time: None,
            mode: Some(crate::vfs::FileMode {
                mode: inode.mode,
                uid: inode.uid,
                gid: inode.gid,
            }),
        })
    }

    /// Set a path's permission bits (the write twin of [`stat`](Self::stat)'s
    /// mode). Only the low 12 bits change; the `S_IFMT` type nibble is
    /// preserved from the existing inode, so `chmod` can never turn a directory
    /// into a file. Backs `/bin/chmod`.
    pub fn chmod(&mut self, path: &str, mode: u16) -> Result<(), Error> {
        let (ino, inode) = self.resolve(path)?;
        let new_mode = (inode.mode & S_IFMT) | (mode & 0o7777);
        self.patch_inode(ino, |slot| {
            slot[0..2].copy_from_slice(&new_mode.to_le_bytes());
        })
    }

    /// Set a path's owner uid and/or gid (`None` leaves that field unchanged, so
    /// one op covers `chown user`, `chown :group`, and `chown user:group`).
    /// Backs `/bin/chown`.
    pub fn chown(&mut self, path: &str, uid: Option<u16>, gid: Option<u16>) -> Result<(), Error> {
        let (ino, _inode) = self.resolve(path)?;
        self.patch_inode(ino, |slot| {
            if let Some(u) = uid {
                slot[2..4].copy_from_slice(&u.to_le_bytes());
            }
            if let Some(g) = gid {
                slot[24..26].copy_from_slice(&g.to_le_bytes());
            }
        })
    }

    /// Read-modify-write just the caller-patched bytes of an existing inode's
    /// on-disk slot, preserving every other field. `chmod`/`chown` can't reuse
    /// [`write_inode`](Self::write_inode) - that zeroes the whole slot for a
    /// freshly-allocated inode, which would wipe a pre-existing file's
    /// timestamps and flags. `f` receives the inode's 128-byte slice; the
    /// mode/uid/gid fields it touches all live within it.
    fn patch_inode<F: FnOnce(&mut [u8])>(&mut self, ino: u32, f: F) -> Result<(), Error> {
        let group = (ino - 1) / self.inodes_per_group;
        let index = (ino - 1) % self.inodes_per_group;
        let inode_table = self.read_u32_at(self.desc_byte(group) + 8)?;
        let inode_byte =
            inode_table as u64 * self.block_size as u64 + index as u64 * self.inode_size as u64;
        let lba = self.part_lba as u64 + inode_byte / SECTOR_SIZE as u64;
        let within = (inode_byte % SECTOR_SIZE as u64) as usize;

        let mut win = [0u8; SECTOR_SIZE * 2];
        let mut sec = [0u8; SECTOR_SIZE];
        self.disk.read_sector(lba, &mut sec)?;
        win[..SECTOR_SIZE].copy_from_slice(&sec);
        self.disk.read_sector(lba + 1, &mut sec)?;
        win[SECTOR_SIZE..].copy_from_slice(&sec);

        f(&mut win[within..within + 128]);

        let mut out = [0u8; SECTOR_SIZE];
        out.copy_from_slice(&win[..SECTOR_SIZE]);
        self.disk.write_sector(lba, &out)?;
        out.copy_from_slice(&win[SECTOR_SIZE..]);
        self.disk.write_sector(lba + 1, &out)?;
        Ok(())
    }

    /// Reads up to `buf.len()` bytes from byte `offset`, returning how many were
    /// copied (`0` at/past EOF). The windowed primitive behind
    /// `FSOP_READ_AT`/`READ_BULK`.
    pub fn read_at(&mut self, path: &str, offset: u64, buf: &mut [u8]) -> Result<u32, Error> {
        let inode = self.find(path)?;
        if !inode.is_reg() {
            return Err(Error::NotAFile);
        }
        Ok(self.read_range(&inode, offset, buf)? as u32)
    }

    /// Copy the file's bytes starting at `start`, up to `buf.len()` and clamped
    /// to EOF, into `buf`; returns bytes copied. Walks logical blocks through
    /// [`block_for`](Self::block_for); a sparse hole (pointer `0`) reads as
    /// zeros.
    fn read_range(&mut self, inode: &Inode, start: u64, buf: &mut [u8]) -> Result<usize, Error> {
        let bs = self.block_size as usize;
        let file_size = inode.size as u64;
        if start >= file_size || buf.is_empty() {
            return Ok(0);
        }
        let want = ((file_size - start) as usize).min(buf.len());
        let mut block = [0u8; MAX_BLOCK_SIZE];
        let mut done = 0usize;
        while done < want {
            let abs = start + done as u64;
            let logical = (abs / bs as u64) as u32;
            let in_block = (abs % bs as u64) as usize;
            let phys = self.block_for(inode, logical)?;
            let n = (bs - in_block).min(want - done);
            if phys == 0 {
                buf[done..done + n].fill(0);
            } else {
                self.read_block(phys, &mut block[..bs])?;
                buf[done..done + n].copy_from_slice(&block[in_block..in_block + n]);
            }
            done += n;
        }
        Ok(done)
    }

    // ---- write infrastructure (Stage A) -------------------------------------
    //
    // ext2 allocation is bitmap-based: each block group has a block bitmap and
    // an inode bitmap. Allocating flips a bitmap bit AND decrements the free
    // counts in both the group descriptor and the superblock (e2fsck checks all
    // three agree). Same claim-before-use discipline as the FAT arcs.

    /// Write one filesystem block from `buf` (`buf.len() >= block_size`).
    fn write_block(&mut self, block: u32, buf: &[u8]) -> Result<(), Error> {
        let lba = self.block_lba(block);
        for s in 0..self.block_size as usize / SECTOR_SIZE {
            let mut sec = [0u8; SECTOR_SIZE];
            sec.copy_from_slice(&buf[s * SECTOR_SIZE..(s + 1) * SECTOR_SIZE]);
            self.disk.write_sector(lba + s as u64, &sec)?;
        }
        Ok(())
    }

    /// Byte offset of block group `group`'s 32-byte descriptor.
    fn desc_byte(&self, group: u32) -> u64 {
        self.bgdt_block as u64 * self.block_size as u64 + group as u64 * 32
    }

    /// Read a `u16` at a partition-relative byte offset (fields here are aligned,
    /// never straddling a sector).
    fn read_u16_at(&mut self, byte_offset: u64) -> Result<u16, Error> {
        let lba = self.part_lba as u64 + byte_offset / SECTOR_SIZE as u64;
        let w = (byte_offset % SECTOR_SIZE as u64) as usize;
        let mut sec = [0u8; SECTOR_SIZE];
        self.disk.read_sector(lba, &mut sec)?;
        Ok(u16::from_le_bytes([sec[w], sec[w + 1]]))
    }

    /// RMW `len` bytes (<= 4) of a little-endian integer at a partition-relative
    /// byte offset.
    fn write_int_at(&mut self, byte_offset: u64, value: u32, len: usize) -> Result<(), Error> {
        let lba = self.part_lba as u64 + byte_offset / SECTOR_SIZE as u64;
        let w = (byte_offset % SECTOR_SIZE as u64) as usize;
        let mut sec = [0u8; SECTOR_SIZE];
        self.disk.read_sector(lba, &mut sec)?;
        sec[w..w + len].copy_from_slice(&value.to_le_bytes()[..len]);
        self.disk.write_sector(lba, &sec)?;
        Ok(())
    }

    /// Add `delta` to a `u16` group-descriptor field at `desc + field_off`.
    fn adjust_group_u16(&mut self, group: u32, field_off: u64, delta: i32) -> Result<(), Error> {
        let at = self.desc_byte(group) + field_off;
        let v = self.read_u16_at(at)? as i32 + delta;
        self.write_int_at(at, v as u32, 2)
    }

    /// Add `delta` to a `u32` superblock free-count field at `SUPERBLOCK + off`.
    fn adjust_super_u32(&mut self, field_off: u64, delta: i32) -> Result<(), Error> {
        let at = SUPERBLOCK_OFFSET + field_off;
        let v = self.read_u32_at(at)? as i32 + delta;
        self.write_int_at(at, v as u32, 4)
    }

    /// Find the first free bit (`< max_bits`) in the bitmap at `bitmap_block`,
    /// set it, write the bitmap back, and return the bit index. `None` if full.
    fn bitmap_alloc(&mut self, bitmap_block: u32, max_bits: u32) -> Result<Option<u32>, Error> {
        let bs = self.block_size as usize;
        let mut buf = [0u8; MAX_BLOCK_SIZE];
        self.read_block(bitmap_block, &mut buf[..bs])?;
        let max_bytes = (max_bits as usize).div_ceil(8).min(bs);
        for byte in 0..max_bytes {
            if buf[byte] != 0xff {
                let bit = buf[byte].trailing_ones();
                let global = byte as u32 * 8 + bit;
                if global >= max_bits {
                    break;
                }
                buf[byte] |= 1 << bit;
                self.write_block(bitmap_block, &buf[..bs])?;
                return Ok(Some(global));
            }
        }
        Ok(None)
    }

    /// Clear bit `bit` in the bitmap at `bitmap_block` (free it).
    #[allow(dead_code)] // Stage C (rm/rmdir)
    fn bitmap_free(&mut self, bitmap_block: u32, bit: u32) -> Result<(), Error> {
        let bs = self.block_size as usize;
        let mut buf = [0u8; MAX_BLOCK_SIZE];
        self.read_block(bitmap_block, &mut buf[..bs])?;
        buf[(bit / 8) as usize] &= !(1u8 << (bit % 8));
        self.write_block(bitmap_block, &buf[..bs])
    }

    /// Allocate one data block: find a free bit in some group's block bitmap,
    /// set it, decrement the group + superblock free-block counts, return the
    /// block number. Optionally zeroes it.
    fn alloc_block(&mut self, zero: bool) -> Result<u32, Error> {
        for group in 0..self.groups_count {
            let bmp = self.read_u32_at(self.desc_byte(group))?;
            if let Some(bit) = self.bitmap_alloc(bmp, self.blocks_per_group)? {
                let block = self.first_data_block + group * self.blocks_per_group + bit;
                self.adjust_group_u16(group, 12, -1)?; // bg_free_blocks_count
                self.adjust_super_u32(12, -1)?; // s_free_blocks_count
                if zero {
                    self.zero_block(block)?;
                }
                return Ok(block);
            }
        }
        Err(Error::DiskFull)
    }

    /// Free a data block: clear its block-bitmap bit and bump the free counts.
    #[allow(dead_code)] // Stage B overwrite / Stage C rm
    fn free_block(&mut self, block: u32) -> Result<(), Error> {
        if block < self.first_data_block {
            return Ok(());
        }
        let rel = block - self.first_data_block;
        let group = rel / self.blocks_per_group;
        let bit = rel % self.blocks_per_group;
        let bmp = self.read_u32_at(self.desc_byte(group))?;
        self.bitmap_free(bmp, bit)?;
        self.adjust_group_u16(group, 12, 1)?;
        self.adjust_super_u32(12, 1)
    }

    /// Zero a whole filesystem block.
    fn zero_block(&mut self, block: u32) -> Result<(), Error> {
        let z = [0u8; MAX_BLOCK_SIZE];
        self.write_block(block, &z[..self.block_size as usize])
    }

    /// Allocate an inode: find a free bit in some group's inode bitmap (reserved
    /// inodes are already marked used), set it, decrement free-inode counts, and
    /// for a directory bump `bg_used_dirs_count`. Returns the inode number.
    fn alloc_inode(&mut self, is_dir: bool) -> Result<u32, Error> {
        for group in 0..self.groups_count {
            let bmp = self.read_u32_at(self.desc_byte(group) + 4)?;
            if let Some(bit) = self.bitmap_alloc(bmp, self.inodes_per_group)? {
                let ino = group * self.inodes_per_group + bit + 1;
                self.adjust_group_u16(group, 14, -1)?; // bg_free_inodes_count
                self.adjust_super_u32(16, -1)?; // s_free_inodes_count
                if is_dir {
                    self.adjust_group_u16(group, 16, 1)?; // bg_used_dirs_count
                }
                return Ok(ino);
            }
        }
        Err(Error::DiskFull)
    }

    /// Free an inode: clear its inode-bitmap bit, bump free-inode counts, and for
    /// a directory decrement `bg_used_dirs_count`.
    #[allow(dead_code)] // Stage C (rm/rmdir)
    fn free_inode(&mut self, ino: u32, is_dir: bool) -> Result<(), Error> {
        let group = (ino - 1) / self.inodes_per_group;
        let bit = (ino - 1) % self.inodes_per_group;
        let bmp = self.read_u32_at(self.desc_byte(group) + 4)?;
        self.bitmap_free(bmp, bit)?;
        self.adjust_group_u16(group, 14, 1)?;
        self.adjust_super_u32(16, 1)?;
        if is_dir {
            self.adjust_group_u16(group, 16, -1)?;
        }
        Ok(())
    }

    /// Write inode `ino`'s managed fields (mode, links, size, i_blocks, the 15
    /// block pointers) from `node`, zeroing the rest of the slot for a freshly
    /// allocated inode. Assumes `inode_size <= 256` (mke2fs's default) so the
    /// slot fits the 2-sector window.
    fn write_inode(&mut self, ino: u32, node: &Inode) -> Result<(), Error> {
        let group = (ino - 1) / self.inodes_per_group;
        let index = (ino - 1) % self.inodes_per_group;
        let inode_table = self.read_u32_at(self.desc_byte(group) + 8)?;
        let inode_byte =
            inode_table as u64 * self.block_size as u64 + index as u64 * self.inode_size as u64;
        let lba = self.part_lba as u64 + inode_byte / SECTOR_SIZE as u64;
        let within = (inode_byte % SECTOR_SIZE as u64) as usize;

        let mut win = [0u8; SECTOR_SIZE * 2];
        let mut sec = [0u8; SECTOR_SIZE];
        self.disk.read_sector(lba, &mut sec)?;
        win[..SECTOR_SIZE].copy_from_slice(&sec);
        self.disk.read_sector(lba + 1, &mut sec)?;
        win[SECTOR_SIZE..].copy_from_slice(&sec);

        let sz = (self.inode_size as usize).min(256);
        let slot = &mut win[within..within + sz];
        slot.fill(0);
        slot[0..2].copy_from_slice(&node.mode.to_le_bytes());
        slot[2..4].copy_from_slice(&node.uid.to_le_bytes());
        slot[24..26].copy_from_slice(&node.gid.to_le_bytes());
        slot[4..8].copy_from_slice(&node.size.to_le_bytes());
        slot[26..28].copy_from_slice(&node.links.to_le_bytes());
        slot[28..32].copy_from_slice(&node.i_blocks.to_le_bytes());
        for (b, ptr) in node.block.iter().enumerate() {
            let o = 40 + b * 4;
            slot[o..o + 4].copy_from_slice(&ptr.to_le_bytes());
        }

        let mut out = [0u8; SECTOR_SIZE];
        out.copy_from_slice(&win[..SECTOR_SIZE]);
        self.disk.write_sector(lba, &out)?;
        out.copy_from_slice(&win[SECTOR_SIZE..]);
        self.disk.write_sector(lba + 1, &out)?;
        Ok(())
    }

    /// Insert a `(name -> ino)` record into directory `dir` (inode number
    /// `dir_ino`). Uses the classic ext2 slack-split: find an entry whose
    /// `rec_len` has room to spare after its real length and carve the new entry
    /// out of the slack; if no block has room, grow the directory by one block.
    fn insert_dirent(
        &mut self,
        dir_ino: u32,
        dir: &mut Inode,
        name: &str,
        ino: u32,
        ftype: u8,
    ) -> Result<(), Error> {
        let name_len = name.len();
        if name_len == 0 || name_len > 255 {
            return Err(Error::InvalidName);
        }
        let needed = round4(8 + name_len);
        let bs = self.block_size as usize;
        let ftype_byte = if self.has_filetype { ftype } else { 0 };
        let nblocks = dir.size as usize / bs;
        let mut buf = [0u8; MAX_BLOCK_SIZE];

        for li in 0..nblocks {
            let phys = self.block_for(dir, li as u32)?;
            if phys == 0 {
                continue;
            }
            self.read_block(phys, &mut buf[..bs])?;
            let mut off = 0usize;
            while off + 8 <= bs {
                let e = &buf[off..];
                let e_ino = u32::from_le_bytes([e[0], e[1], e[2], e[3]]);
                let rec_len = u16::from_le_bytes([e[4], e[5]]) as usize;
                let e_name_len = e[6] as usize;
                if rec_len < 8 || off + rec_len > bs {
                    break;
                }
                let used = if e_ino == 0 { 0 } else { round4(8 + e_name_len) };
                if rec_len - used >= needed {
                    let new_off = off + used;
                    let new_rec = rec_len - used;
                    if e_ino != 0 {
                        buf[off + 4..off + 6].copy_from_slice(&(used as u16).to_le_bytes());
                    }
                    write_dirent(&mut buf[new_off..], ino, new_rec, name, ftype_byte);
                    self.write_block(phys, &buf[..bs])?;
                    return Ok(());
                }
                off += rec_len;
            }
        }

        // No slack anywhere - grow the directory by one block (direct pointers
        // only; a directory needing > 12 blocks isn't a case this system hits).
        if nblocks >= DIRECT_BLOCKS {
            return Err(Error::DiskFull);
        }
        let newblk = self.alloc_block(true)?;
        let mut nb = [0u8; MAX_BLOCK_SIZE];
        write_dirent(&mut nb[..bs], ino, bs, name, ftype_byte);
        self.write_block(newblk, &nb[..bs])?;
        dir.block[nblocks] = newblk;
        dir.size += bs as u32;
        dir.i_blocks += (bs / SECTOR_SIZE) as u32;
        self.write_inode(dir_ino, dir)
    }

    // ---- write surface ------------------------------------------------------

    /// Create an empty file, or succeed as a no-op if a file already exists
    /// there (no RTC to update). An empty file needs no data blocks - just an
    /// inode (links 1, size 0) and a directory entry.
    pub fn touch(&mut self, path: &str) -> Result<(), Error> {
        let (parent_path, name) = split_parent(path).ok_or(Error::InvalidName)?;
        let (parent_ino, mut parent) = self.resolve(parent_path)?;
        if !parent.is_dir() {
            return Err(Error::NotADirectory);
        }
        match self.lookup_in(&parent, name)? {
            Some((_, mode)) if mode & S_IFMT == S_IFDIR => return Err(Error::NotAFile),
            Some(_) => return Ok(()), // a file already exists - no-op
            None => {}
        }
        let ino = self.alloc_inode(false)?;
        let node = Inode {
            mode: NEW_FILE_MODE,
            uid: self.creator_uid,
            gid: self.creator_gid,
            links: 1,
            size: 0,
            i_blocks: 0,
            block: [0; INODE_BLOCK_PTRS],
        };
        self.write_inode(ino, &node)?;
        self.insert_dirent(parent_ino, &mut parent, name, ino, FT_REG)
    }

    /// Look up `name` in directory `dir`, returning `(inode number, i_mode)` if
    /// present. The kind check callers need without a second inode read for the
    /// name match itself.
    fn lookup_in(&mut self, dir: &Inode, name: &str) -> Result<Option<(u32, u16)>, Error> {
        let mut found: Option<u32> = None;
        self.walk_dir(dir, |entry| {
            if entry.name() == name {
                found = Some(entry.inode);
                true
            } else {
                false
            }
        })?;
        match found {
            Some(ino) => Ok(Some((ino, self.read_inode(ino)?.mode))),
            None => Ok(None),
        }
    }

    // ---- write infrastructure (Stage B: data blocks) ------------------------

    /// RMW one pointer (`u32` at `index`) inside an indirect (pointer) block.
    fn write_indirect_ptr(&mut self, block: u32, index: u32, value: u32) -> Result<(), Error> {
        self.write_int_at(block as u64 * self.block_size as u64 + index as u64 * 4, value, 4)
    }

    /// Ensure the file's logical block `logical` is allocated, returning its
    /// physical block; allocates the block (zeroed) and any indirect blocks
    /// needed to reach it, updating `node.block`/`node.i_blocks` in place. Only
    /// direct + single/double indirect (triple returns `DiskFull`).
    fn ensure_block(&mut self, node: &mut Inode, logical: u32) -> Result<u32, Error> {
        let existing = self.block_for(node, logical)?;
        if existing != 0 {
            return Ok(existing);
        }
        let per_blk = self.block_size / 4;
        let sectors_per_block = (self.block_size as usize / SECTOR_SIZE) as u32;
        let blk = self.alloc_block(true)?;
        node.i_blocks += sectors_per_block;

        if logical < DIRECT_BLOCKS as u32 {
            node.block[logical as usize] = blk;
        } else if logical < DIRECT_BLOCKS as u32 + per_blk {
            if node.block[12] == 0 {
                node.block[12] = self.alloc_block(true)?;
                node.i_blocks += sectors_per_block;
            }
            self.write_indirect_ptr(node.block[12], logical - DIRECT_BLOCKS as u32, blk)?;
        } else if logical < DIRECT_BLOCKS as u32 + per_blk + per_blk * per_blk {
            let dd = logical - DIRECT_BLOCKS as u32 - per_blk;
            let (l1, l2) = (dd / per_blk, dd % per_blk);
            if node.block[13] == 0 {
                node.block[13] = self.alloc_block(true)?;
                node.i_blocks += sectors_per_block;
            }
            let mut sib = self.indirect_lookup(node.block[13], l1)?;
            if sib == 0 {
                sib = self.alloc_block(true)?;
                node.i_blocks += sectors_per_block;
                self.write_indirect_ptr(node.block[13], l1, sib)?;
            }
            self.write_indirect_ptr(sib, l2, blk)?;
        } else {
            return Err(Error::DiskFull); // triple indirect unsupported
        }
        Ok(blk)
    }

    /// Free every data block of `node` (direct + indirect data blocks *and* the
    /// indirect pointer blocks themselves). Reads indirection while it's still
    /// intact, then frees the pointer blocks last.
    fn free_all_blocks(&mut self, node: &Inode) -> Result<(), Error> {
        let bs = self.block_size as usize;
        let per_blk = self.block_size / 4;
        let nblocks = (node.size as usize).div_ceil(bs) as u32;
        for d in 0..nblocks {
            let phys = self.block_for(node, d)?;
            if phys != 0 {
                self.free_block(phys)?;
            }
        }
        if node.block[12] != 0 {
            self.free_block(node.block[12])?;
        }
        if node.block[13] != 0 {
            for l1 in 0..per_blk {
                let sib = self.indirect_lookup(node.block[13], l1)?;
                if sib != 0 {
                    self.free_block(sib)?;
                }
            }
            self.free_block(node.block[13])?;
        }
        Ok(())
    }

    // ---- write surface ------------------------------------------------------

    /// Create a file with exactly `data`, or fully replace an existing file's
    /// contents. Allocates and writes the new blocks, points the inode at them,
    /// then frees the old blocks (write-new-before-free, the FAT arcs' ordering).
    pub fn write_file(&mut self, path: &str, data: &[u8]) -> Result<(), Error> {
        let (parent_path, name) = split_parent(path).ok_or(Error::InvalidName)?;
        let (parent_ino, mut parent) = self.resolve(parent_path)?;
        if !parent.is_dir() {
            return Err(Error::NotADirectory);
        }
        let existing = self.lookup_in(&parent, name)?;
        if let Some((_, mode)) = existing {
            if mode & S_IFMT == S_IFDIR {
                return Err(Error::NotAFile);
            }
        }

        // Allocate + write the new data blocks first (nothing existing touched).
        let mut node = Inode {
            mode: NEW_FILE_MODE,
            uid: self.creator_uid,
            gid: self.creator_gid,
            links: 1,
            size: data.len() as u32,
            i_blocks: 0,
            block: [0; INODE_BLOCK_PTRS],
        };
        self.write_file_data(&mut node, data)?;

        match existing {
            Some((ino, _)) => {
                // Overwriting existing content must NOT change the file's
                // identity: preserve the old owner and mode+links (creator_uid/
                // gid apply only to a freshly created inode, the None arm).
                // POSIX: writing a file never chowns it.
                let old = self.read_inode(ino)?;
                node.mode = old.mode;
                node.links = old.links;
                node.uid = old.uid;
                node.gid = old.gid;
                self.write_inode(ino, &node)?;
                self.free_all_blocks(&old)
            }
            None => {
                let ino = self.alloc_inode(false)?;
                self.write_inode(ino, &node)?;
                self.insert_dirent(parent_ino, &mut parent, name, ino, FT_REG)
            }
        }
    }

    /// Allocate and write every data block of `data` into `node` (block pointers
    /// + `i_blocks` filled in). Shared by `write_file`.
    fn write_file_data(&mut self, node: &mut Inode, data: &[u8]) -> Result<(), Error> {
        let bs = self.block_size as usize;
        let nblocks = data.len().div_ceil(bs);
        let mut buf = [0u8; MAX_BLOCK_SIZE];
        for d in 0..nblocks {
            let phys = self.ensure_block(node, d as u32)?;
            let start = d * bs;
            let end = ((d + 1) * bs).min(data.len());
            buf[..end - start].copy_from_slice(&data[start..end]);
            buf[end - start..bs].fill(0);
            self.write_block(phys, &buf[..bs])?;
        }
        Ok(())
    }

    /// Write `data` at byte `offset`, extending the file as needed without
    /// rewriting the bytes before `offset` - the streaming/append primitive
    /// (`cp`/`>>`/`writeat`). Grow-only size; a gap past EOF is zero-filled
    /// (bounded by `MAX_GAP_FILL`); blocks are allocated on demand.
    pub fn write_at(&mut self, path: &str, offset: u64, data: &[u8]) -> Result<(), Error> {
        if data.is_empty() {
            return Ok(());
        }
        let (ino, mut node) = self.resolve(path)?;
        if !node.is_reg() {
            return Err(Error::NotAFile);
        }
        let old_size = node.size as u64;
        if offset > old_size && offset - old_size > MAX_GAP_FILL {
            return Err(Error::InvalidOffset);
        }
        let bs = self.block_size as u64;
        let write_start = old_size.min(offset);
        let write_end = offset + data.len() as u64;

        let mut buf = [0u8; MAX_BLOCK_SIZE];
        let mut pos = write_start;
        while pos < write_end {
            let logical = (pos / bs) as u32;
            let in_block = (pos % bs) as usize;
            let phys = self.ensure_block(&mut node, logical)?;
            let n = (bs as usize - in_block).min((write_end - pos) as usize);
            self.read_block(phys, &mut buf[..bs as usize])?;
            for i in 0..n {
                let fp = pos + i as u64;
                buf[in_block + i] = if fp >= offset { data[(fp - offset) as usize] } else { 0 };
            }
            self.write_block(phys, &buf[..bs as usize])?;
            pos += n as u64;
        }

        node.size = old_size.max(write_end) as u32;
        self.write_inode(ino, &node)
    }

    // ---- write infrastructure (Stage C: directories) ------------------------

    /// Re-point the existing entry `name` in `dir` at inode `ino`, in place.
    ///
    /// ONE BLOCK WRITE, and it is the commit point of a replacing
    /// [`mv`](Self::mv): before it the name resolves to the old content, after
    /// it to the new, and at no instant does it resolve to nothing. That is the
    /// property a `rm` followed by a rename cannot offer, and it is available
    /// here only because an ext2 directory entry names an inode rather than
    /// holding the file's location itself.
    fn set_dirent_inode(&mut self, dir: &Inode, name: &str, ino: u32, ftype: u8) -> Result<(), Error> {
        let bs = self.block_size as usize;
        let nblocks = dir.size as usize / bs;
        let mut buf = [0u8; MAX_BLOCK_SIZE];
        for li in 0..nblocks {
            let phys = self.block_for(dir, li as u32)?;
            if phys == 0 {
                continue;
            }
            self.read_block(phys, &mut buf[..bs])?;
            let mut off = 0usize;
            while off + 8 <= bs {
                let e = &buf[off..];
                let e_ino = u32::from_le_bytes([e[0], e[1], e[2], e[3]]);
                let rec_len = u16::from_le_bytes([e[4], e[5]]) as usize;
                let nl = e[6] as usize;
                if rec_len < 8 || off + rec_len > bs {
                    break;
                }
                if e_ino != 0
                    && nl == name.len()
                    && off + 8 + nl <= bs
                    && &buf[off + 8..off + 8 + nl] == name.as_bytes()
                {
                    buf[off..off + 4].copy_from_slice(&ino.to_le_bytes());
                    buf[off + 7] = ftype;
                    return self.write_block(phys, &buf[..bs]);
                }
                off += rec_len;
            }
        }
        Err(Error::NotFound)
    }

    /// Unlink `name` from directory `dir`: extend the previous entry's `rec_len`
    /// to swallow the removed one (or, if it's first in its block, zero its inode
    /// field) - the standard ext2 removal. `NotFound` if absent.
    fn remove_dirent(&mut self, dir: &Inode, name: &str) -> Result<(), Error> {
        let bs = self.block_size as usize;
        let nblocks = dir.size as usize / bs;
        let mut buf = [0u8; MAX_BLOCK_SIZE];
        for li in 0..nblocks {
            let phys = self.block_for(dir, li as u32)?;
            if phys == 0 {
                continue;
            }
            self.read_block(phys, &mut buf[..bs])?;
            let mut off = 0usize;
            let mut prev: Option<usize> = None;
            while off + 8 <= bs {
                let e = &buf[off..];
                let e_ino = u32::from_le_bytes([e[0], e[1], e[2], e[3]]);
                let rec_len = u16::from_le_bytes([e[4], e[5]]) as usize;
                let nl = e[6] as usize;
                if rec_len < 8 || off + rec_len > bs {
                    break;
                }
                if e_ino != 0
                    && nl == name.len()
                    && off + 8 + nl <= bs
                    && &buf[off + 8..off + 8 + nl] == name.as_bytes()
                {
                    if let Some(p) = prev {
                        let pr = u16::from_le_bytes([buf[p + 4], buf[p + 5]]) as usize;
                        buf[p + 4..p + 6].copy_from_slice(&((pr + rec_len) as u16).to_le_bytes());
                    } else {
                        buf[off..off + 4].copy_from_slice(&0u32.to_le_bytes());
                    }
                    self.write_block(phys, &buf[..bs])?;
                    return Ok(());
                }
                prev = Some(off);
                off += rec_len;
            }
        }
        Err(Error::NotFound)
    }

    /// Whether directory `dir` holds any entry other than `.`/`..`.
    fn dir_is_empty(&mut self, dir: &Inode) -> Result<bool, Error> {
        let mut empty = true;
        self.walk_dir(dir, |entry| {
            let n = entry.name();
            if n != "." && n != ".." {
                empty = false;
                true
            } else {
                false
            }
        })?;
        Ok(empty)
    }

    // ---- write surface ------------------------------------------------------

    /// Create an empty subdirectory. Allocates an inode + one data block for its
    /// `.`/`..` entries, links it into the parent, and bumps the parent's link
    /// count (the new dir's `..` points at it). Fails if the name exists.
    pub fn mkdir(&mut self, path: &str) -> Result<(), Error> {
        let (parent_path, name) = split_parent(path).ok_or(Error::InvalidName)?;
        let (parent_ino, mut parent) = self.resolve(parent_path)?;
        if !parent.is_dir() {
            return Err(Error::NotADirectory);
        }
        if self.lookup_in(&parent, name)?.is_some() {
            return Err(Error::AlreadyExists);
        }

        let new_ino = self.alloc_inode(true)?;
        let dblk = self.alloc_block(true)?;
        let bs = self.block_size as usize;
        let ftype_dir = if self.has_filetype { FT_DIR } else { 0 };
        let mut buf = [0u8; MAX_BLOCK_SIZE];
        write_dirent(&mut buf[0..], new_ino, 12, ".", ftype_dir);
        write_dirent(&mut buf[12..], parent_ino, bs - 12, "..", ftype_dir);
        self.write_block(dblk, &buf[..bs])?;

        let mut node = Inode {
            mode: NEW_DIR_MODE,
            uid: self.creator_uid,
            gid: self.creator_gid,
            links: 2, // "." and the name in the parent
            size: bs as u32,
            i_blocks: (bs / SECTOR_SIZE) as u32,
            block: [0; INODE_BLOCK_PTRS],
        };
        node.block[0] = dblk;
        self.write_inode(new_ino, &node)?;

        self.insert_dirent(parent_ino, &mut parent, name, new_ino, FT_DIR)?;
        parent.links += 1; // the new dir's ".." references the parent
        self.write_inode(parent_ino, &parent)
    }

    /// Remove a file (rejects a directory with `NotAFile`). Unlinks the entry,
    /// then decrements the inode's link count and - at zero - frees its data
    /// blocks and the inode.
    pub fn rm(&mut self, path: &str) -> Result<(), Error> {
        let (parent_path, name) = split_parent(path).ok_or(Error::InvalidName)?;
        let (_parent_ino, parent) = self.resolve(parent_path)?;
        if !parent.is_dir() {
            return Err(Error::NotADirectory);
        }
        let (tino, mode) = self.lookup_in(&parent, name)?.ok_or(Error::NotFound)?;
        if mode & S_IFMT == S_IFDIR {
            return Err(Error::NotAFile);
        }
        let target = self.read_inode(tino)?;
        self.remove_dirent(&parent, name)?;
        self.drop_link(tino, &target)
    }

    /// Drop one link to `ino`, whose inode the caller has ALREADY read: free its
    /// blocks and the inode itself when that was the last one, otherwise just
    /// decrement the count.
    ///
    /// Factored out of [`rm`](Self::rm) when [`mv`](Self::mv) grew a second
    /// caller (replacing an existing destination unlinks it). The `i_dtime`
    /// that `mark_inode_deleted` writes is not decoration: a freed inode with a
    /// small `i_dtime` is misread by `e2fsck` as an orphan-list next-pointer,
    /// which is the bug the filesystems arc hit and would be easy to omit in a
    /// second hand-written copy of this sequence.
    ///
    /// It takes the `Inode` rather than reading it because the first version
    /// read it HERE, which moved the read after `rm`'s `remove_dirent` and
    /// turned a clean "I/O error, nothing changed" into an unlinked file whose
    /// blocks leak. Every caller now reads before it unlinks or commits.
    fn drop_link(&mut self, ino: u32, target: &Inode) -> Result<(), Error> {
        let target = *target;
        if target.links <= 1 {
            self.free_all_blocks(&target)?;
            self.free_inode(ino, false)?;
            self.mark_inode_deleted(ino, target.mode)?;
        } else {
            let mut t = target;
            t.links -= 1;
            self.write_inode(ino, &t)?;
        }
        Ok(())
    }

    /// Remove an empty subdirectory (rejects a non-empty one, a file, or the
    /// root). Unlinks it, frees its block(s) and inode, and decrements the
    /// parent's link count (the gone `..` no longer points at it).
    pub fn rmdir(&mut self, path: &str) -> Result<(), Error> {
        let (parent_path, name) = split_parent(path).ok_or(Error::CannotRemoveRoot)?;
        let (parent_ino, mut parent) = self.resolve(parent_path)?;
        if !parent.is_dir() {
            return Err(Error::NotADirectory);
        }
        let (tino, mode) = self.lookup_in(&parent, name)?.ok_or(Error::NotFound)?;
        if mode & S_IFMT != S_IFDIR {
            return Err(Error::NotADirectory);
        }
        let target = self.read_inode(tino)?;
        if !self.dir_is_empty(&target)? {
            return Err(Error::DirectoryNotEmpty);
        }
        self.remove_dirent(&parent, name)?;
        self.free_all_blocks(&target)?;
        self.free_inode(tino, true)?;
        self.mark_inode_deleted(tino, target.mode)?;
        parent.links -= 1; // the removed dir's ".." no longer references parent
        self.write_inode(parent_ino, &parent)
    }

    /// Mark a freed inode deleted: links 0, everything cleared, and a nonzero
    /// `i_dtime` (e2fsck expects a deleted inode to carry a deletion time).
    fn mark_inode_deleted(&mut self, ino: u32, mode: u16) -> Result<(), Error> {
        let dead = Inode {
            mode,
            uid: 0,
            gid: 0,
            links: 0,
            size: 0,
            i_blocks: 0,
            block: [0; INODE_BLOCK_PTRS],
        };
        self.write_inode(ino, &dead)?;
        // i_dtime lives at offset 20 within the inode.
        let group = (ino - 1) / self.inodes_per_group;
        let index = (ino - 1) % self.inodes_per_group;
        let inode_table = self.read_u32_at(self.desc_byte(group) + 8)?;
        let inode_byte =
            inode_table as u64 * self.block_size as u64 + index as u64 * self.inode_size as u64;
        self.write_int_at(inode_byte + 20, DELETION_TIME, 4)
    }

    /// Point directory `dir`'s `..` entry at `new_parent` (after a
    /// cross-directory move). The `..` is a real entry in the dir's first block.
    fn set_dotdot(&mut self, dir: &Inode, new_parent: u32) -> Result<(), Error> {
        let bs = self.block_size as usize;
        let phys = self.block_for(dir, 0)?;
        if phys == 0 {
            return Ok(());
        }
        let mut buf = [0u8; MAX_BLOCK_SIZE];
        self.read_block(phys, &mut buf[..bs])?;
        let mut off = 0usize;
        while off + 8 <= bs {
            let rec_len = u16::from_le_bytes([buf[off + 4], buf[off + 5]]) as usize;
            let nl = buf[off + 6] as usize;
            if rec_len < 8 || off + rec_len > bs {
                break;
            }
            if nl == 2 && &buf[off + 8..off + 10] == b".." {
                buf[off..off + 4].copy_from_slice(&new_parent.to_le_bytes());
                self.write_block(phys, &buf[..bs])?;
                return Ok(());
            }
            off += rec_len;
        }
        Ok(())
    }

    /// Rename or move a file/directory. Re-points a new directory entry at
    /// `src`'s inode (no data copy) then unlinks the old entry
    /// (write-new-before-delete). For a directory moved to a *different*
    /// parent, fixes its `..` and moves the parent-link-count contribution.
    ///
    /// An existing `dst` is REPLACED when both it and `src` are non-directories
    /// - the POSIX rename, and on ext2 it is very nearly atomic: the whole
    /// change is one write of `dst`'s directory entry, re-pointing it at
    /// `src`'s inode. The name never resolves to nothing. What follows that
    /// write is cleanup (unlink `src`'s name, drop the replaced inode), and a
    /// crash inside it leaks - a link count too high, blocks not returned,
    /// both `e2fsck` repairs - rather than losing either file.
    ///
    /// Anything involving a DIRECTORY as the destination is still refused with
    /// `AlreadyExists`, deliberately: POSIX also replaces an empty directory
    /// with a directory, which needs an emptiness check and the parent link
    /// counts moved, and nothing has asked for it. Refusing is safe and
    /// honest; the surface can grow when something wants it.
    pub fn mv(&mut self, src: &str, dst: &str) -> Result<(), Error> {
        let (sp_path, s_name) = split_parent(src).ok_or(Error::InvalidName)?;
        let (dp_path, d_name) = split_parent(dst).ok_or(Error::InvalidName)?;
        let (sp_ino, mut sp) = self.resolve(sp_path)?;
        if !sp.is_dir() {
            return Err(Error::NotADirectory);
        }
        let (dp_ino, mut dp) = self.resolve(dp_path)?;
        if !dp.is_dir() {
            return Err(Error::NotADirectory);
        }
        let (s_ino, s_mode) = self.lookup_in(&sp, s_name)?.ok_or(Error::NotFound)?;
        let is_dir = s_mode & S_IFMT == S_IFDIR;
        let ftype = if is_dir { FT_DIR } else { FT_REG };

        // A destination that already exists. `mv f f` MUST be a no-op and not
        // reach the replace path: the same-name case would unlink the entry it
        // had just re-pointed and destroy the file the caller asked to keep.
        // (The same self-destruct shape as `cp x x` in the isolation arc.)
        if let Some((d_ino, d_mode)) = self.lookup_in(&dp, d_name)? {
            // Same entry, or two names for the same inode. POSIX `rename`
            // requires both to succeed and do nothing, and here the second is
            // also a DATA-LOSS path: `set_dirent_inode` would re-point `dst` at
            // an inode `drop_link(d_ino)` then frees, taking the blocks out
            // from under the name that now claims them - whenever the link
            // count says 1, which a foreign or damaged image can present while
            // two names exist. This OS cannot yet create a hard link, but these
            // ext2 images are built by the host's `mke2fs -d`.
            if (sp_ino == dp_ino && s_name == d_name) || d_ino == s_ino {
                return Ok(());
            }
            let dst_is_dir = d_mode & S_IFMT == S_IFDIR;
            if is_dir || dst_is_dir {
                return Err(Error::AlreadyExists);
            }
            // Read the replaced inode BEFORE committing: if the disk cannot
            // be read we want to fail having changed nothing, not after the
            // destination name already points at the new file.
            let d_inode = self.read_inode(d_ino)?;
            // THE COMMIT: one block write, after which `dst` names the new
            // content. Everything below is cleanup.
            self.set_dirent_inode(&dp, d_name, s_ino, ftype)?;
            if sp_ino == dp_ino {
                self.remove_dirent(&dp, s_name)?;
            } else {
                self.remove_dirent(&sp, s_name)?;
            }
            return self.drop_link(d_ino, &d_inode);
        }

        if sp_ino == dp_ino {
            // Rename within one directory - operate on a single parent struct.
            self.insert_dirent(dp_ino, &mut dp, d_name, s_ino, ftype)?;
            self.remove_dirent(&dp, s_name)
        } else {
            self.insert_dirent(dp_ino, &mut dp, d_name, s_ino, ftype)?;
            self.remove_dirent(&sp, s_name)?;
            if is_dir {
                let moved = self.read_inode(s_ino)?;
                self.set_dotdot(&moved, dp_ino)?; // ".." now points at the new parent
                dp.links += 1;
                sp.links -= 1;
                self.write_inode(dp_ino, &dp)?;
                self.write_inode(sp_ino, &sp)?;
            }
            Ok(())
        }
    }
}

/// Round `n` up to a multiple of 4 - ext2 directory entries are 4-byte aligned.
fn round4(n: usize) -> usize {
    n.div_ceil(4) * 4
}

/// Write a directory entry `(ino, rec_len, name_len, file_type, name)` at the
/// start of `dst` (`dst.len() >= 8 + name.len()`).
fn write_dirent(dst: &mut [u8], ino: u32, rec_len: usize, name: &str, ftype: u8) {
    dst[0..4].copy_from_slice(&ino.to_le_bytes());
    dst[4..6].copy_from_slice(&(rec_len as u16).to_le_bytes());
    dst[6] = name.len() as u8;
    dst[7] = ftype;
    dst[8..8 + name.len()].copy_from_slice(name.as_bytes());
}

// ---- mkfs ([`Fs::format`]) helpers --------------------------------------------
//
// The formatter uses a fixed 4 KiB block, single-block-group layout, so these
// don't need an `Fs` instance (which doesn't exist yet at mkfs time).

/// The fixed filesystem block size mkfs lays down (see [`Fs::format`]).
const FMT_BLOCK: usize = 4096;
/// A plausible fixed timestamp for the mkfs-written superblock/inodes - this
/// system has no RTC, and e2fsck wants a nonzero, non-tiny time (~a 2004 date).
const MKFS_TIME: u32 = 0x4000_0000;
/// A fixed, nonzero volume UUID (ASCII "OUROBORO" tail) so e2fsck doesn't offer
/// to generate one. e2fsck only requires it be nonzero, not any given value.
const FS_UUID: [u8; 16] = [
    0x0b, 0xad, 0xc0, 0xde, 0x00, 0x55, 0x00, 0x55, 0x4f, 0x55, 0x52, 0x4f, 0x42, 0x4f, 0x52, 0x4f,
];

/// Little-endian `u32` at `off` (mkfs field writer).
fn put32(buf: &mut [u8], off: usize, v: u32) {
    buf[off..off + 4].copy_from_slice(&v.to_le_bytes());
}
/// Little-endian `u16` at `off` (mkfs field writer).
fn put16(buf: &mut [u8], off: usize, v: u16) {
    buf[off..off + 2].copy_from_slice(&v.to_le_bytes());
}
/// Set bit `bit` in a little-endian bitmap block (bit 0 == byte 0 bit 0).
fn set_bit(bitmap: &mut [u8], bit: u32) {
    bitmap[(bit / 8) as usize] |= 1u8 << (bit % 8);
}

/// Write one mkfs block (`data.len() == FMT_BLOCK`) at filesystem block number
/// `block` of the partition starting at `start_lba`, one sector at a time.
fn write_fmt_block(disk: &mut Disk, start_lba: u32, block: u32, data: &[u8]) -> Result<(), Error> {
    let spb = FMT_BLOCK / SECTOR_SIZE; // sectors per block (8)
    let lba = start_lba as u64 + block as u64 * spb as u64;
    for s in 0..spb {
        let mut sec = [0u8; SECTOR_SIZE];
        sec.copy_from_slice(&data[s * SECTOR_SIZE..(s + 1) * SECTOR_SIZE]);
        disk.write_sector(lba + s as u64, &sec)?;
    }
    Ok(())
}

/// Write inode `ino`'s 128-byte slot into `table_block` (the inode-table block
/// that contains it), setting only the fields this driver models - mode, size,
/// links, `i_blocks`, and `block[0]` - plus the three timestamps. `ino` must
/// live in this block (mkfs only writes inodes 2 and 11, both in the first).
fn write_inode_slot(
    table_block: &mut [u8],
    ino: u32,
    mode: u16,
    size: u32,
    links: u16,
    i_blocks: u32,
    block0: u32,
) {
    let inodes_per_block = FMT_BLOCK / 128;
    let off = ((ino - 1) as usize % inodes_per_block) * 128;
    let s = &mut table_block[off..off + 128];
    put16(s, 0, mode); // i_mode
    put32(s, 4, size); // i_size
    put32(s, 8, MKFS_TIME); // i_atime
    put32(s, 12, MKFS_TIME); // i_ctime
    put32(s, 16, MKFS_TIME); // i_mtime
    put16(s, 26, links); // i_links_count
    put32(s, 28, i_blocks); // i_blocks (512-byte units)
    put32(s, 40, block0); // i_block[0]
}
