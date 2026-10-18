//! `dirprobe` - the check that a directory grown past its first cluster
//! still lists and finds every entry, for `make test-grow-dir`
//! (`scripts/test-grow-dir.py`). A probe, not a tool.
//!
//! ```text
//! dirprobe <dir> <count>
//! ```
//!
//! Makes `<dir>` and writes `<count>` files in it, `F000`, `F001`, ..., each
//! holding its own name, so the directory outgrows its first cluster and fsd
//! links more to it (`grow_dir`). Then, in turn:
//!
//! - `create`: every write succeeded;
//! - `find`: every file opens by its path and holds its own name;
//! - `list`: the directory's listing names every file once and nothing else;
//! - `retouch`: a `touch` of every file (a no-op on a file that exists), then
//!   the listing again, the same: a name fsd cannot find is created again.
//!
//! The probe sees only what fsd answers; the rig reads the directory off the
//! disk after the boot, its size and its chain, and runs the host's fsck on
//! it. A line a step (`dirprobe: ok find`, `dirprobe: FAIL find: F042: not
//! found`), then `dirprobe: ok` or `dirprobe: FAIL` on every exit, a refused
//! argument included, and exit 0 or 1. Since 2026-10-18.

#![no_std]
#![no_main]

const PATH_MAX: usize = 128;
const COUNT_MAX: usize = 999;
/// The listing of `COUNT_MAX` names of five bytes (`F000\n`), with room to spare.
const LIST_MAX: usize = 8192;

/// The summary line the rig waits for, then the exit: every way out ends here.
fn finish(target: u64, failed: bool) -> ! {
    ulib::write_out(target, if failed { b"dirprobe: FAIL\r\n" } else { b"dirprobe: ok\r\n" });
    ulib::end_of_stream(target);
    ulib::exit(if failed { 1 } else { 0 });
}

fn usage(target: u64) -> ! {
    ulib::write_out(target, b"usage: dirprobe <dir> <count>\r\n");
    finish(target, true)
}

/// `F` and `i` in three digits.
fn name(i: usize) -> [u8; 4] {
    [b'F', b'0' + (i / 100) as u8, b'0' + (i / 10 % 10) as u8, b'0' + (i % 10) as u8]
}

struct Probe {
    target: u64,
    dir: [u8; PATH_MAX],
    dir_len: usize,
    count: usize,
}

impl Probe {
    fn say(&self, parts: &[&[u8]]) {
        let mut out = [0u8; 192];
        let mut n = 0;
        ulib::emit(&mut out, &mut n, b"dirprobe: ");
        for p in parts {
            ulib::emit(&mut out, &mut n, p);
        }
        ulib::emit(&mut out, &mut n, b"\r\n");
        ulib::write_out(self.target, &out[..n]);
    }

    fn dir(&self) -> &str {
        // Checked as UTF-8 when it was read.
        core::str::from_utf8(&self.dir[..self.dir_len]).unwrap_or("")
    }

    /// `<dir>/F<i>` in `buf`, as the `&str` ulib takes.
    fn path<'b>(&self, i: usize, buf: &'b mut [u8; PATH_MAX]) -> &'b str {
        let mut n = 0;
        ulib::emit(buf, &mut n, &self.dir[..self.dir_len]);
        ulib::emit(buf, &mut n, b"/");
        ulib::emit(buf, &mut n, &name(i));
        // Built from the argument and ASCII names only.
        core::str::from_utf8(&buf[..n]).unwrap_or("")
    }

    /// Runs `op` on every file, stopping at the first refusal, which it reports.
    fn each(&self, step: &[u8], op: impl Fn(&str) -> u64) -> bool {
        for i in 0..self.count {
            let mut buf = [0u8; PATH_MAX];
            let r = op(self.path(i, &mut buf));
            if ulib::is_fs_error(r) {
                self.say(&[b"FAIL ", step, b": ", &name(i), b": ", ulib::fs_error_msg(r)]);
                return false;
            }
        }
        true
    }

    fn create(&self) -> bool {
        self.each(b"create", |path| {
            let tag = &path.as_bytes()[path.len() - 4..];
            ulib::fs_write_bulk(path, tag)
        })
    }

    fn find(&self) -> bool {
        for i in 0..self.count {
            let mut buf = [0u8; PATH_MAX];
            let mut data = [0u8; 16];
            let r = ulib::fs_read_bulk(self.path(i, &mut buf), 0, &mut data);
            if ulib::is_fs_error(r) {
                self.say(&[b"FAIL find: ", &name(i), b": ", ulib::fs_error_msg(r)]);
                return false;
            }
            if data[..(r as usize).min(data.len())] != name(i) {
                self.say(&[b"FAIL find: ", &name(i), b" does not hold its name"]);
                return false;
            }
        }
        true
    }

    /// The listing names `F000` to the last file once each, and nothing else.
    fn list(&self, step: &[u8]) -> bool {
        let mut buf = [0u8; LIST_MAX];
        let (len, complete) = match ulib::fs_list_dir(self.dir(), &mut buf) {
            Ok(v) => v,
            Err(r) => {
                self.say(&[b"FAIL ", step, b": ", ulib::fs_error_msg(r)]);
                return false;
            }
        };
        if !complete {
            self.say(&[b"FAIL ", step, b": the listing did not end"]);
            return false;
        }
        let mut seen = [0u8; COUNT_MAX];
        for line in buf[..len].split(|&b| b == b'\n').filter(|l| !l.is_empty()) {
            let i = match line {
                [b'F', d @ ..] if d.len() == 3 && d.iter().all(u8::is_ascii_digit) => {
                    (d[0] - b'0') as usize * 100 + (d[1] - b'0') as usize * 10 + (d[2] - b'0') as usize
                }
                _ => COUNT_MAX,
            };
            if i >= self.count {
                self.say(&[b"FAIL ", step, b": a name no file was given: ", line]);
                return false;
            }
            seen[i] += 1;
        }
        let mut num = [0u8; 24];
        let mut n = 0;
        let listed = seen[..self.count].iter().filter(|&&s| s > 0).count();
        ulib::emit_dec(&mut num, &mut n, listed as u64);
        if let Some(i) = seen[..self.count].iter().position(|&s| s == 0) {
            self.say(&[b"FAIL ", step, b": ", &num[..n], b" names listed, ", &name(i), b" the first missing"]);
            return false;
        }
        if let Some(i) = seen[..self.count].iter().position(|&s| s > 1) {
            self.say(&[b"FAIL ", step, b": ", &name(i), b" listed twice"]);
            return false;
        }
        true
    }

    fn retouch(&self) -> bool {
        self.each(b"retouch", |path| ulib::fs_op_path(syscall_abi::FSOP_TOUCH, path))
            && self.list(b"retouch")
    }
}

#[no_mangle]
#[link_section = ".text.start"]
pub extern "C" fn _start() -> ! {
    ulib::usage_if_requested(b"usage: dirprobe <dir> <count>\r\n");
    let target = ulib::stdout_target();
    let mut probe = Probe { target, dir: [0; PATH_MAX], dir_len: 0, count: 0 };
    let Some(len) = ulib::arg(1, &mut probe.dir) else { usage(target) };
    // Room for "/F000" after it.
    if len == 0 || len > PATH_MAX - 8 || core::str::from_utf8(&probe.dir[..len]).is_err() {
        usage(target);
    }
    probe.dir_len = len;
    let mut num = [0u8; 16];
    let Some(len) = ulib::arg(2, &mut num) else { usage(target) };
    let Some(count) = core::str::from_utf8(&num[..len]).ok().and_then(ulib::parse_u64) else {
        usage(target)
    };
    if count == 0 || count > COUNT_MAX as u64 {
        usage(target);
    }
    probe.count = count as usize;

    let r = ulib::fs_op_path(syscall_abi::FSOP_MKDIR, probe.dir());
    if ulib::is_fs_error(r) {
        probe.say(&[b"FAIL mkdir: ", ulib::fs_error_msg(r)]);
        finish(target, true);
    }
    // Each step needs the one before it: a file that was never made cannot
    // be found, so the first failure ends the run.
    for (step, run) in [
        (&b"create"[..], Probe::create as fn(&Probe) -> bool),
        (&b"find"[..], Probe::find),
        (&b"list"[..], |p: &Probe| p.list(b"list")),
        (&b"retouch"[..], Probe::retouch),
    ] {
        if !run(&probe) {
            finish(target, true);
        }
        probe.say(&[b"ok ", step]);
    }
    finish(target, false)
}
