//! ext2, read-only - the third on-disk format `fsd` understands, and the one
//! that actually tests the [`Filesystem`](crate::vfs::Filesystem) abstraction.
//! FAT32 and exFAT are the *same shape* (a partition, a FAT, a heap of clusters,
//! directory entries in a flat list), so a thin enum sufficed. ext2 is a
//! genuinely different model - and driving it through the unchanged `FSOP_*`
//! protocol is the proof the abstraction is real, not FAT-shaped in disguise.
//!
//! Same constraints as everything in `fsd`: hand-rolled, fixed-buffer, no
//! `alloc`, on [`Disk`]'s `BLOCK_*` syscall shim. Read-only first, the arc's
//! discipline (write is a separate, higher-risk milestone).
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
//! **Scope of this read-only cut.** The `FSOP_*` protocol is FAT-shaped (no
//! permissions, owners, or symlinks), so this presents files and directories
//! and ignores the Unix metadata it can't model. Symlinks (mode `0xA000`) are
//! reported as entries but not followed. Every write op returns
//! [`Error::ReadOnly`]. Root is always inode 2.

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

/// A decoded inode - only the fields a read-only driver needs.
#[derive(Clone, Copy)]
struct Inode {
    mode: u16,
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
        })
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
        let size = u32::from_le_bytes([i[4], i[5], i[6], i[7]]);
        let links = u16::from_le_bytes([i[26], i[27]]);
        let i_blocks = u32::from_le_bytes([i[28], i[29], i[30], i[31]]);
        let mut block = [0u32; INODE_BLOCK_PTRS];
        for (b, slot) in block.iter_mut().enumerate() {
            let o = 40 + b * 4;
            *slot = u32::from_le_bytes([i[o], i[o + 1], i[o + 2], i[o + 3]]);
        }
        Ok(Inode {
            mode,
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
            links: 1,
            size: data.len() as u32,
            i_blocks: 0,
            block: [0; INODE_BLOCK_PTRS],
        };
        self.write_file_data(&mut node, data)?;

        match existing {
            Some((ino, _)) => {
                let old = self.read_inode(ino)?;
                node.mode = old.mode;
                node.links = old.links;
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

    // ---- write surface: still read-only (later stages) ----------------------

    pub fn mkdir(&mut self, _path: &str) -> Result<(), Error> {
        Err(Error::ReadOnly)
    }
    pub fn rmdir(&mut self, _path: &str) -> Result<(), Error> {
        Err(Error::ReadOnly)
    }
    pub fn rm(&mut self, _path: &str) -> Result<(), Error> {
        Err(Error::ReadOnly)
    }
    pub fn mv(&mut self, _src: &str, _dst: &str) -> Result<(), Error> {
        Err(Error::ReadOnly)
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
