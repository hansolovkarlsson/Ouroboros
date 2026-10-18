//! `cursorprobe` - the check that fsd's chain cursors never go stale, for
//! `make test-stale-cursor` (`scripts/test-stale-cursor.py`). A probe, not a
//! tool.
//!
//! FAT32 and exFAT each keep a read cursor and a write cursor (where the last
//! read or write ended in a file's cluster chain), keyed by the file's first
//! cluster, and drop both when a FAT write rewrites a link (`write_fat_entry`;
//! at every FAT write until 2026-10-18). Without the drop, a cursor outlives
//! its file: once the file is removed and a new file
//! is given the same first cluster on a different chain, the next read or
//! write of the new file at or past the cursor resumes on the old chain. A
//! read returns another file's bytes; a write puts the new file's bytes into
//! another file's cluster. No `cp` makes that sequence, since each read or
//! write sets its cursor again at its end, so `test-write-walk` and
//! `test-spawn-reads` pass with the drop removed. This program makes it.
//!
//! ```text
//! cursorprobe <dir> <cluster bytes>
//! ```
//!
//! In `<dir>`, which it creates, on a volume whose clusters are `<cluster
//! bytes>` (the rig reads it off the disk), three sequences, each in a
//! directory of its own (`R`, `I`, `P`) whose files it keeps. Every byte
//! written is a pattern of its file's tag and its offset, so a read of the
//! wrong cluster cannot pass. Each file is read back whole at the end of its
//! sequence.
//!
//! A sequence tests a stale cursor only if the allocator laid it out as below:
//! B on A's first cluster, and A's cursor cluster somewhere else in B's chain.
//! The probe cannot see clusters, so each sequence writes a one-cluster witness
//! just before A (`W1`) and one just after A's last cluster (`W2`); the rig
//! reads the kept files' chains off the disk after the boot and checks that A
//! lay between them, B starts where A did, and the cursor cluster is G's or
//! D's. Without that check a change in allocation would let every sequence
//! pass with nothing tested.
//!
//! - `read`: A of four clusters written and read whole (the read cursor at its
//!   last cluster), A removed, B and G written a cluster each in turn (B takes
//!   A's first cluster, on a chain interleaved with G's), then B read first at
//!   its last cluster: the stale cursor gives G's bytes.
//! - `write-in-place`: A and F written a cluster each in turn, A last (the
//!   write cursor at A's last cluster), both removed, B and then D written
//!   whole (`NP_WRITE`, no cursor: B takes A's first cluster, D the clusters
//!   after B, A's last among them), then B overwritten at its last cluster:
//!   the stale cursor writes into D.
//! - `write-append`: the same, then B appended to at its end: the stale cursor
//!   extends D's chain, and B's new bytes are not in B's.
//!
//! The write sequences need a whole-file write of at least two clusters, so
//! clusters of at most `SAFECOPY_MAX / 2` bytes; on larger ones (exFAT's 4 KiB)
//! they print `skip` and the rig gives that volume only `read`. A line a
//! sequence (`cursorprobe: ok read`, `cursorprobe: FAIL write-append: ...`),
//! then `cursorprobe: ok` or `cursorprobe: FAIL` on every exit, a refused
//! argument included, and exit 0 or 1. Since 2026-10-18.

#![no_std]
#![no_main]

use syscall_abi::SAFECOPY_MAX;

const PIECE: usize = SAFECOPY_MAX as usize;
/// The clusters of A, B and G in `read`.
const READ_CLUSTERS: usize = 4;
const PATH_MAX: usize = 128;
/// The largest cluster exFAT allows, 32 MiB; the arithmetic stays far from overflow.
const CLUSTER_MAX: u64 = 32 << 20;

/// The summary line the rig waits for, then the exit: every way out ends here.
fn finish(target: u64, failed: bool) -> ! {
    ulib::write_out(target, if failed { b"cursorprobe: FAIL\r\n" } else { b"cursorprobe: ok\r\n" });
    ulib::end_of_stream(target);
    ulib::exit(if failed { 1 } else { 0 });
}

fn usage(target: u64) -> ! {
    ulib::write_out(target, b"usage: cursorprobe <dir> <cluster bytes>\r\n");
    finish(target, true)
}

/// The byte at `offset` of a file tagged `tag`: each aligned four bytes are the
/// tag and the low 24 bits of the word's index, so two files, or two offsets
/// of one file, never agree on a word.
fn pattern(tag: u8, offset: usize) -> u8 {
    let word = offset / 4;
    match offset % 4 {
        0 => tag,
        1 => (word >> 16) as u8,
        2 => (word >> 8) as u8,
        _ => word as u8,
    }
}

/// What a file should hold: `[start, end)` ranges in order from 0, each of one tag.
type Model<'a> = &'a [(usize, usize, u8)];

struct Probe {
    target: u64,
    dir: [u8; PATH_MAX],
    dir_len: usize,
    cluster: usize,
    /// The running sequence's directory under `dir`.
    sub: core::cell::Cell<u8>,
}

impl Probe {
    fn say(&self, parts: &[&[u8]], value: Option<u64>) {
        let mut out = [0u8; 192];
        let mut n = 0;
        ulib::emit(&mut out, &mut n, b"cursorprobe: ");
        for p in parts {
            ulib::emit(&mut out, &mut n, p);
        }
        if let Some(v) = value {
            ulib::emit_dec(&mut out, &mut n, v);
        }
        ulib::emit(&mut out, &mut n, b"\r\n");
        ulib::write_out(self.target, &out[..n]);
    }

    /// `<dir>/<sequence>/<name>` in `buf`, as the `&str` ulib takes.
    fn path<'b>(&self, name: &[u8], buf: &'b mut [u8; PATH_MAX]) -> &'b str {
        let mut n = 0;
        ulib::emit(buf, &mut n, &self.dir[..self.dir_len]);
        ulib::emit(buf, &mut n, b"/");
        ulib::emit(buf, &mut n, &[self.sub.get()]);
        if !name.is_empty() {
            ulib::emit(buf, &mut n, b"/");
            ulib::emit(buf, &mut n, name);
        }
        // Built from the argument and ASCII names only.
        core::str::from_utf8(&buf[..n]).unwrap_or("")
    }

    /// The sequence's directory, `sub` under `dir`.
    fn enter(&self, seq: &[u8], sub: u8) -> Result<(), ()> {
        self.sub.set(sub);
        let mut buf = [0u8; PATH_MAX];
        let r = ulib::fs_op_path(syscall_abi::FSOP_MKDIR, self.path(b"", &mut buf));
        self.check_status(seq, b"mkdir ", &[sub], r)
    }

    /// An empty file `name`, or the failure.
    fn create(&self, seq: &[u8], name: &[u8]) -> Result<(), ()> {
        let mut buf = [0u8; PATH_MAX];
        let r = ulib::fs_write_bulk(self.path(name, &mut buf), &[]);
        self.check_status(seq, b"create ", name, r)
    }

    fn remove(&self, seq: &[u8], name: &[u8]) -> Result<(), ()> {
        let mut buf = [0u8; PATH_MAX];
        let r = ulib::fs_op_path(syscall_abi::FSOP_RM, self.path(name, &mut buf));
        self.check_status(seq, b"rm ", name, r)
    }

    fn check_status(&self, seq: &[u8], what: &[u8], name: &[u8], r: u64) -> Result<(), ()> {
        if ulib::is_fs_error(r) {
            self.say(&[b"FAIL ", seq, b": ", what, name, b": ", ulib::fs_error_msg(r)], None);
            return Err(());
        }
        Ok(())
    }

    /// `len` bytes of `tag`'s pattern at `offset` of `name`, a cursor-moving
    /// `NP_WRITE_AT` a piece.
    fn write_at(&self, seq: &[u8], name: &[u8], offset: usize, len: usize, tag: u8) -> Result<(), ()> {
        let mut buf = [0u8; PATH_MAX];
        let path = self.path(name, &mut buf);
        let mut data = [0u8; PIECE];
        let mut at = offset;
        while at < offset + len {
            let n = (offset + len - at).min(PIECE);
            for (i, b) in data[..n].iter_mut().enumerate() {
                *b = pattern(tag, at + i);
            }
            let r = ulib::fs_write_at(path, at as u64, &data[..n]);
            self.check_status(seq, b"write ", name, r)?;
            at += n;
        }
        Ok(())
    }

    /// `name` created or replaced whole with `len` bytes of `tag`'s pattern,
    /// one `NP_WRITE`: no cursor is set by it.
    fn write_whole(&self, seq: &[u8], name: &[u8], len: usize, tag: u8) -> Result<(), ()> {
        let mut buf = [0u8; PATH_MAX];
        let mut data = [0u8; PIECE];
        for (i, b) in data[..len].iter_mut().enumerate() {
            *b = pattern(tag, i);
        }
        let r = ulib::fs_write_bulk(self.path(name, &mut buf), &data[..len]);
        self.check_status(seq, b"write whole ", name, r)
    }

    /// `name` read from `offset` to the model's end, a piece a request, and
    /// compared with `model`; the first difference is reported with the tag
    /// the wrong word carries.
    fn verify(&self, seq: &[u8], name: &[u8], offset: usize, model: Model) -> Result<(), ()> {
        let mut buf = [0u8; PATH_MAX];
        let path = self.path(name, &mut buf);
        let end = model.last().map_or(0, |m| m.1);
        let mut data = [0u8; PIECE];
        let mut at = offset;
        while at < end {
            let want = (end - at).min(PIECE);
            let r = ulib::fs_read_bulk(path, at as u64, &mut data[..want]);
            self.check_status(seq, b"read ", name, r)?;
            let got = r as usize;
            if got != want {
                self.say(&[b"FAIL ", seq, b": ", name, b" ends short, at "], Some((at + got) as u64));
                return Err(());
            }
            for (i, &b) in data[..got].iter().enumerate() {
                let o = at + i;
                let tag = model.iter().find(|m| m.0 <= o && o < m.1).map_or(0, |m| m.2);
                if b != pattern(tag, o) {
                    let word = o - o % 4;
                    let found = if word >= at { data[word - at] } else { b'?' };
                    self.say(
                        &[b"FAIL ", seq, b": ", name, b" holds a word of ", &[found], b" at "],
                        Some(o as u64),
                    );
                    return Err(());
                }
            }
            at += got;
        }
        Ok(())
    }

    /// `read`, in the module doc. `Ok(true)`: it ran and passed.
    fn read(&self) -> Result<bool, ()> {
        let seq = b"read";
        let c = self.cluster;
        let size = READ_CLUSTERS * c;
        self.enter(seq, b'R')?;
        self.write_whole(seq, b"W1", 1, b'W')?;
        self.create(seq, b"A")?;
        self.write_at(seq, b"A", 0, size, b'A')?;
        self.write_whole(seq, b"W2", 1, b'W')?;
        // The read cursor at A's last cluster.
        self.verify(seq, b"A", 0, &[(0, size, b'A')])?;
        self.remove(seq, b"A")?;
        self.create(seq, b"B")?;
        self.create(seq, b"G")?;
        for i in 0..READ_CLUSTERS {
            self.write_at(seq, b"B", i * c, c, b'B')?;
            self.write_at(seq, b"G", i * c, c, b'G')?;
        }
        // B's first read, at its last cluster: where A's cursor was.
        self.verify(seq, b"B", size - c, &[(0, size, b'B')])?;
        self.verify(seq, b"B", 0, &[(0, size, b'B')])?;
        self.verify(seq, b"G", 0, &[(0, size, b'G')])?;
        Ok(true)
    }

    /// `write-in-place` (`append` false) and `write-append`, in the module doc.
    /// `Ok(false)`: skipped, the clusters too large to build it.
    fn write(&self, append: bool) -> Result<bool, ()> {
        let seq: &[u8] = if append { b"write-append" } else { b"write-in-place" };
        let c = self.cluster;
        let k = PIECE / c;
        if k < 2 {
            self.say(&[b"skip ", seq, b": a whole-file write is one cluster of "], Some(c as u64));
            return Ok(false);
        }
        let whole = k * c;
        self.enter(seq, if append { b'P' } else { b'I' })?;
        self.write_whole(seq, b"W1", 1, b'W')?;
        self.create(seq, b"A")?;
        self.create(seq, b"F")?;
        for i in 0..k - 1 {
            self.write_at(seq, b"A", i * c, c, b'A')?;
            self.write_at(seq, b"F", i * c, c, b'F')?;
        }
        // F's last cluster first, so A's is the last allocated, past B's
        // whole-file chain and in D's.
        self.write_at(seq, b"F", (k - 1) * c, c, b'F')?;
        self.write_at(seq, b"A", (k - 1) * c, c, b'A')?;
        self.write_whole(seq, b"W2", 1, b'W')?;
        self.remove(seq, b"A")?;
        self.remove(seq, b"F")?;
        self.write_whole(seq, b"B", whole, b'B')?;
        self.write_whole(seq, b"D", whole, b'D')?;
        if append {
            self.write_at(seq, b"B", whole, c, b'X')?;
            self.verify(seq, b"B", 0, &[(0, whole, b'B'), (whole, whole + c, b'X')])?;
        } else {
            self.write_at(seq, b"B", whole - c, c, b'X')?;
            self.verify(seq, b"B", 0, &[(0, whole - c, b'B'), (whole - c, whole, b'X')])?;
        }
        self.verify(seq, b"D", 0, &[(0, whole, b'D')])?;
        Ok(true)
    }
}

#[no_mangle]
#[link_section = ".text.start"]
pub extern "C" fn _start() -> ! {
    ulib::usage_if_requested(b"usage: cursorprobe <dir> <cluster bytes>\r\n");
    let target = ulib::stdout_target();
    let mut probe =
        Probe { target, dir: [0; PATH_MAX], dir_len: 0, cluster: 0, sub: core::cell::Cell::new(0) };
    let Some(len) = ulib::arg(1, &mut probe.dir) else { usage(target) };
    // Room for "/<sequence>/<name>" after it.
    if len == 0 || len > PATH_MAX - 8 {
        usage(target);
    }
    probe.dir_len = len;
    let mut num = [0u8; 16];
    let Some(len) = ulib::arg(2, &mut num) else { usage(target) };
    let Some(cluster) = core::str::from_utf8(&num[..len]).ok().and_then(ulib::parse_u64) else {
        usage(target)
    };
    if cluster == 0 || cluster % 512 != 0 || cluster > CLUSTER_MAX {
        usage(target);
    }
    probe.cluster = cluster as usize;

    let dir = core::str::from_utf8(&probe.dir[..probe.dir_len]).unwrap_or("");
    let r = ulib::fs_op_path(syscall_abi::FSOP_MKDIR, dir);
    if ulib::is_fs_error(r) {
        probe.say(&[b"FAIL mkdir: ", ulib::fs_error_msg(r)], None);
        finish(target, true);
    }
    // Each sequence in its own directory, so one that fails leaves nothing in
    // another's way; the rig checks each one's layout on its own.
    let mut failed = false;
    for (seq, result) in [
        (&b"read"[..], probe.read()),
        (&b"write-in-place"[..], probe.write(false)),
        (&b"write-append"[..], probe.write(true)),
    ] {
        match result {
            Ok(true) => probe.say(&[b"ok ", seq], None),
            Ok(false) => {}
            Err(()) => failed = true,
        }
    }
    // Every file is kept: the rig reads their chains off the disk.
    finish(target, failed)
}
