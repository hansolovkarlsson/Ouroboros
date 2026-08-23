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
use crate::fat32::Error;

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

/// Direct block pointers in an inode (indices 0..12); [12]/[13]/[14] are the
/// single / double / triple indirect pointers.
const DIRECT_BLOCKS: usize = 12;
const INODE_BLOCK_PTRS: usize = 15;

/// Largest block size we handle (a 4 KiB working buffer covers 1024/2048/4096).
const MAX_BLOCK_SIZE: usize = 4096;

/// Longest name we keep (ext2 allows 255).
const NAME_MAX: usize = 255;

/// A decoded inode - only the fields a read-only driver needs.
#[derive(Clone, Copy)]
struct Inode {
    mode: u16,
    size: u32,
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

        Ok(Fs {
            disk,
            part_lba: partition_lba,
            block_size,
            inodes_per_group,
            inode_size,
            // The block group descriptor table is the block right after the one
            // holding the superblock (first_data_block).
            bgdt_block: first_data_block + 1,
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
        let mut block = [0u32; INODE_BLOCK_PTRS];
        for (b, slot) in block.iter_mut().enumerate() {
            let o = 40 + b * 4;
            *slot = u32::from_le_bytes([i[o], i[o + 1], i[o + 2], i[o + 3]]);
        }
        Ok(Inode { mode, size, block })
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
        let mut inode = self.read_inode(ROOT_INO)?;
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
            let ino = found.ok_or(Error::NotFound)?;
            inode = self.read_inode(ino)?;
        }
        Ok(inode)
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

    // ---- write surface: read-only (write is a separate milestone) -----------

    pub fn write_file(&mut self, _path: &str, _data: &[u8]) -> Result<(), Error> {
        Err(Error::ReadOnly)
    }
    pub fn write_at(&mut self, _path: &str, _offset: u64, _data: &[u8]) -> Result<(), Error> {
        Err(Error::ReadOnly)
    }
    pub fn mkdir(&mut self, _path: &str) -> Result<(), Error> {
        Err(Error::ReadOnly)
    }
    pub fn rmdir(&mut self, _path: &str) -> Result<(), Error> {
        Err(Error::ReadOnly)
    }
    pub fn touch(&mut self, _path: &str) -> Result<(), Error> {
        Err(Error::ReadOnly)
    }
    pub fn rm(&mut self, _path: &str) -> Result<(), Error> {
        Err(Error::ReadOnly)
    }
    pub fn mv(&mut self, _src: &str, _dst: &str) -> Result<(), Error> {
        Err(Error::ReadOnly)
    }
}
