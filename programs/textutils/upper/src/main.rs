//! The pipeline filter demo - the reference for a *chainable* filter (the
//! multi-stage-pipeline arc). Reads piped input (stdin = `MSG_RECV`, 1-64 bytes
//! per message, EOF = the empty message), uppercases each byte, and writes the
//! result to its **stdout target** (`ulib::write_out`) - the console when it's
//! the last stage, or the next program when it's a middle stage of a longer
//! pipeline. On EOF it signals end-of-stream downstream and exits, so the next
//! stage finishes too. `echo hello | /EFI/ORBS/UPPER.BIN` prints `HELLO`;
//! `… | upper | upper` chains because output goes to the target, not a
//! hardcoded console.
//!
//! The shape any pipeline filter has on this kernel: no argv, stdin is
//! `MSG_RECV`, stdout is `ulib::write_out(stdout_target, …)`, EOF in is the
//! empty message, EOF out is `ulib::end_of_stream`, and finishing is `exit`.

#![no_std]
#![no_main]

#[no_mangle]
#[link_section = ".text.start"]
pub extern "C" fn _start() -> ! {
    let target = ulib::stdout_target();
    let mut buf = [0u8; syscall_abi::MSG_MAX_LEN as usize];
    loop {
        let packed = ulib::syscall4(
            syscall_abi::MSG_RECV,
            buf.as_mut_ptr() as u64,
            buf.len() as u64,
            0,
            0,
        );
        if packed >= syscall_abi::FS_ERR_MIN {
            // Nothing here should error (RECV_INTERRUPTED can't reach a
            // non-keyboard-owner) - stop rather than spin on a bad status.
            break;
        }
        let len = ((packed & 0xffff_ffff) as usize).min(buf.len());
        if len == 0 {
            break; // end of stream - the pipeline convention's empty message
        }
        for b in &mut buf[..len] {
            *b = b.to_ascii_uppercase();
        }
        ulib::write_out(target, &buf[..len]);
    }
    // Propagate end-of-stream to the next stage (a no-op when the target is the
    // console), then exit so the shell (and any downstream stage) finishes.
    ulib::end_of_stream(target);
    ulib::exit(0);
}
