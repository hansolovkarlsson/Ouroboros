//! `stageprobe` - the owner of the kernel's spawn staging, for
//! `make test-spawn-owner` (`scripts/test-spawn-owner.py`). A probe, not a
//! tool: it claims the staging the way a spawner does (one `SPAWN_STAGE`
//! chunk) and then does NOT spawn, so the rig can see what the kernel does
//! for everyone else while a spawn is in progress, and when it ends.
//!
//! ```text
//! stageprobe hold <ms>            claim, hold for <ms>, exit without a SPAWN
//!                                 (the release is the task's end)
//! stageprobe hold <ms> abandon    claim, hold for <ms>, give the staging up
//!                                 with a SPAWN of total 0, then linger ten
//!                                 times as long before exiting (the release
//!                                 is the abandon, with the task still alive)
//! stageprobe over                 claim one 16-byte chunk, then SPAWN a total
//!                                 of 4096 with argv and cwd lengths of 512 and
//!                                 128: the kernel must answer SPAWN_ERROR (a
//!                                 total past what this owner staged), never
//!                                 read the previous stager's bytes as a program
//! stageprobe sparse <total>      one 16-byte chunk ending at <total>, with
//!                                 nothing staged below it, three seconds'
//!                                 wait (the refusal must have claimed
//!                                 nothing, so another task spawns
//!                                 meanwhile), then a SPAWN of <total>: the
//!                                 kernel must refuse the chunk and the
//!                                 SPAWN with SPAWN_ERROR. Given the
//!                                 size of /bin/STAGEPROBE, the buffer below
//!                                 the chunk holds the shell's copy of this
//!                                 program, so a kernel that took the chunk
//!                                 starts that copy, which prints its usage
//!                                 (the high review of #303)
//! stageprobe check <ms>           wait <ms>, then try to claim every 20 ms:
//!                                 report the first refusal and the claim that
//!                                 follows it, give the claim up, exit 0; exit
//!                                 3 if never refused, 2 if never claimed
//! ```
//!
//! Every line names the probe's own task slot, so the rig can `kill` a
//! holder and tell two probes' lines apart. Since 2026-10-11.

#![no_std]
#![no_main]

/// What `check` sleeps between tries, and how long it tries for.
const POLL_US: u64 = 20_000;
const GIVE_UP_US: u64 = 40_000_000;

/// The chunk a claim stages: any bytes, at offset 0. Never spawned.
static CHUNK: [u8; 16] = *b"stageprobe-chunk";

fn usage(target: u64) -> ! {
    ulib::write_out(target, b"usage: stageprobe hold <ms> [abandon] | check <ms> | over | sparse <total>\r\n");
    ulib::end_of_stream(target);
    ulib::exit(2);
}

/// One `SPAWN_STAGE` of [`CHUNK`] at offset 0: `0` when this task now owns
/// the staging, `SPAWN_ERR_STAGING_BUSY` while another does.
fn claim() -> u64 {
    ulib::syscall4(syscall_abi::SPAWN_STAGE, 0, CHUNK.as_ptr() as u64, CHUNK.len() as u64, 0)
}

/// A `SPAWN` with a total of `0`: refused as empty, and the staging released.
fn abandon() -> u64 {
    ulib::syscall4(syscall_abi::SPAWN, 0, syscall_abi::CON_TASK, 0, 0)
}

fn sleep_ms(ms: u64) {
    ulib::syscall(syscall_abi::SLEEP_UNTIL, ulib::monotonic_us() + ms * 1000);
}

fn say(target: u64, me: u64, text: &[u8], value: Option<u64>) {
    let mut out = [0u8; 128];
    let mut n = 0;
    ulib::emit(&mut out, &mut n, b"stageprobe ");
    ulib::emit_dec(&mut out, &mut n, me);
    ulib::emit(&mut out, &mut n, b": ");
    ulib::emit(&mut out, &mut n, text);
    if let Some(v) = value {
        ulib::emit_dec(&mut out, &mut n, v);
    }
    ulib::emit(&mut out, &mut n, b"\r\n");
    ulib::write_out(target, &out[..n]);
}

#[no_mangle]
#[link_section = ".text.start"]
pub extern "C" fn _start() -> ! {
    let target = ulib::stdout_target();
    let me = ulib::self_task();
    let mut word = [0u8; 16];
    let Some(len) = ulib::arg(1, &mut word) else { usage(target) };
    let mode = match &word[..len.min(word.len())] {
        b"hold" => 0,
        b"check" => 1,
        b"over" => 2,
        b"sparse" => 3,
        _ => usage(target),
    };
    if mode == 2 {
        let r = claim();
        if r != 0 {
            say(target, me, b"could not claim the staging, code ", Some(r));
            ulib::end_of_stream(target);
            ulib::exit(1);
        }
        let r = ulib::syscall4(syscall_abi::SPAWN, 4096, syscall_abi::CON_TASK, 512, 128);
        say(target, me, b"spawn of a total past the staged chunk answered code ", Some(r));
        ulib::end_of_stream(target);
        ulib::exit(if r == syscall_abi::SPAWN_ERROR { 0 } else { 5 });
    }
    let mut num = [0u8; 16];
    let Some(len) = ulib::arg(2, &mut num) else { usage(target) };
    let Some(ms) = core::str::from_utf8(&num[..len.min(num.len())]).ok().and_then(ulib::parse_u64) else {
        usage(target)
    };
    if mode == 3 {
        let total = ms;
        if total < CHUNK.len() as u64 {
            usage(target);
        }
        let offset = total - CHUNK.len() as u64;
        let r = ulib::syscall4(syscall_abi::SPAWN_STAGE, offset, CHUNK.as_ptr() as u64, CHUNK.len() as u64, 0);
        say(target, me, b"a chunk with nothing staged below it answered code ", Some(r));
        sleep_ms(3000);
        let s = ulib::syscall4(syscall_abi::SPAWN, total, syscall_abi::CON_TASK, 0, 0);
        say(target, me, b"spawn of the sparse total answered code ", Some(s));
        ulib::end_of_stream(target);
        ulib::exit(if r == syscall_abi::SPAWN_ERROR && s == syscall_abi::SPAWN_ERROR { 0 } else { 5 });
    }
    let mut abandon_it = false;
    if let Some(len) = ulib::arg(3, &mut word) {
        if &word[..len.min(word.len())] != b"abandon" {
            usage(target);
        }
        abandon_it = true;
    }

    if mode == 0 {
        let r = claim();
        if r != 0 {
            say(target, me, b"could not claim the staging, code ", Some(r));
            ulib::end_of_stream(target);
            ulib::exit(1);
        }
        say(target, me, b"holding the spawn staging for ms ", Some(ms));
        sleep_ms(ms);
        if abandon_it {
            // Refused as empty (SPAWN_ERR_TOO_LARGE), the staging released.
            let r = abandon();
            say(target, me, b"abandoned the staging, spawn answered code ", Some(r));
            sleep_ms(ms * 10);
        }
        say(target, me, b"exiting", None);
        ulib::end_of_stream(target);
        ulib::exit(0);
    }

    // check: wait, then try until refused and then until claimed.
    say(target, me, b"checking after ms ", Some(ms));
    sleep_ms(ms);
    let start = ulib::monotonic_us();
    let mut refusals = 0u64;
    loop {
        let r = claim();
        if r == 0 {
            break;
        }
        if r == syscall_abi::SPAWN_ERR_STAGING_BUSY {
            if refusals == 0 {
                say(target, me, b"refused, the staging is held, code ", Some(r));
            }
            refusals += 1;
        } else {
            say(target, me, b"claim failed with an unexpected code ", Some(r));
            ulib::end_of_stream(target);
            ulib::exit(4);
        }
        if ulib::monotonic_us() - start > GIVE_UP_US {
            say(target, me, b"never claimed, refusals ", Some(refusals));
            ulib::end_of_stream(target);
            ulib::exit(2);
        }
        ulib::syscall(syscall_abi::SLEEP_UNTIL, ulib::monotonic_us() + POLL_US);
    }
    say(target, me, b"claimed after refusals ", Some(refusals));
    abandon();
    ulib::end_of_stream(target);
    ulib::exit(if refusals > 0 { 0 } else { 3 });
}
