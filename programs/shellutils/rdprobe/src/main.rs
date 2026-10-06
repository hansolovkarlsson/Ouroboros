//! `rdprobe` - the check that the kernel's five per-task store reads
//! (`GET_ARG`, `GET_ENV`, `GET_CWD`, `GET_NS`, `TASK_NAME`) accept an out
//! buffer larger than the store they read. Every other reader passes 512 bytes
//! or less, where the kernel's old blanket refusal and its copy-out helper
//! (`copy_out` in `kernel/src/syscall.rs`, 2026-10-06) give the same answer, so
//! without this program no rig could tell them apart.
//!
//! Each read goes through a 4 KiB buffer, larger than every store, and one
//! `GET_ENV` passes a capacity of `u64::MAX`. A refused read prints `refused`,
//! which is what the old kernel answered to all of them. Run by
//! `scripts/test-cenv.py`, which checks each line against what it set up:
//!
//! ```text
//! arg1=<argument 1>
//! env=<NAME=VALUE>          one per entry
//! envmax=<entry 0>          read with capacity u64::MAX
//! cwd=<working directory>
//! ns=<namespace blob>       bytes outside printable ASCII as \xNN
//! name=<TASK_NAME of this task>
//! ```

#![no_std]
#![no_main]

use syscall_abi::{GET_ARG, GET_CWD, GET_ENV, GET_NS, NO_ARG, SELF, TASK_NAME};

/// Larger than every store (ENV_MAX, the largest, is 2048).
const BIG: usize = 4096;

#[no_mangle]
#[link_section = ".text.start"]
pub extern "C" fn _start() -> ! {
    ulib::usage_if_requested(
        b"usage: rdprobe <arg>   (read the per-task stores through a 4 KiB buffer)\r\n",
    );
    let target = ulib::stdout_target();
    let mut buf = [0u8; BIG];

    let r = read(&mut buf, |p, c| ulib::syscall4(GET_ARG, 1, p, c, 0), NO_ARG);
    line(target, b"arg1", &buf, r, false);

    let mut i = 0;
    loop {
        let r = read(&mut buf, |p, c| ulib::syscall4(GET_ENV, i, p, c, 0), NO_ARG);
        if r.is_none() {
            if i == 0 {
                line(target, b"env", &buf, None, false);
            }
            break;
        }
        line(target, b"env", &buf, r, false);
        i += 1;
    }

    let n = ulib::syscall4(GET_ENV, 0, buf.as_mut_ptr() as u64, u64::MAX, 0);
    let r = if n == NO_ARG { None } else { Some(n as usize) };
    line(target, b"envmax", &buf, r, false);

    let r = read(&mut buf, |p, c| ulib::syscall4(GET_CWD, p, c, 0, 0), 0);
    line(target, b"cwd", &buf, r, false);

    let r = read(&mut buf, |p, c| ulib::syscall4(GET_NS, p, c, 0, 0), 0);
    line(target, b"ns", &buf, r, true);

    let me = ulib::syscall(SELF, 0);
    let r = read(&mut buf, |p, c| ulib::syscall4(TASK_NAME, me, p, c, 0), 0);
    line(target, b"name", &buf, r, false);

    ulib::end_of_stream(target);
    ulib::exit(0);
}

/// One read into the whole of `buf`: the length it reports, or `None` when the
/// kernel answered `refused` (the call's own sentinel for "nothing").
fn read(buf: &mut [u8; BIG], call: impl Fn(u64, u64) -> u64, refused: u64) -> Option<usize> {
    let n = call(buf.as_mut_ptr() as u64, BIG as u64);
    if n == refused {
        None
    } else {
        Some(n as usize)
    }
}

/// `<key>=<bytes>` or `<key> refused`, then CRLF. `escape` writes bytes outside
/// printable ASCII as `\xNN` (the namespace blob carries binary lengths).
fn line(target: u64, key: &[u8], buf: &[u8; BIG], r: Option<usize>, escape: bool) {
    let mut out = Out { target, chunk: [0; 256], n: 0 };
    out.put(key);
    match r {
        None => out.put(b" refused"),
        Some(len) => {
            out.put(b"=");
            // A length past the buffer would be a kernel answering more than
            // it holds; print what the buffer has.
            let bytes = buf.get(..len).unwrap_or(&buf[..]);
            for &b in bytes {
                if escape && !(0x20..0x7f).contains(&b) {
                    const HEX: &[u8; 16] = b"0123456789abcdef";
                    out.put(&[b'\\', b'x', HEX[(b >> 4) as usize], HEX[(b & 15) as usize]]);
                } else {
                    out.put(&[b]);
                }
            }
        }
    }
    out.put(b"\r\n");
    out.flush();
}

/// Output gathered into chunks, so a line is a few messages, not one a byte.
struct Out {
    target: u64,
    chunk: [u8; 256],
    n: usize,
}

impl Out {
    fn put(&mut self, bytes: &[u8]) {
        for &b in bytes {
            if self.n == self.chunk.len() {
                self.flush();
            }
            self.chunk[self.n] = b;
            self.n += 1;
        }
    }

    fn flush(&mut self) {
        if self.n > 0 {
            ulib::write_out(self.target, &self.chunk[..self.n]);
            self.n = 0;
        }
    }
}
