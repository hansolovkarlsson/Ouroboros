//! `wc` - externalized line/word/byte counter (a pipeline filter). Reads its
//! stdin (`ulib::pipe_recv`) to end-of-stream, counting newlines, whitespace-
//! delimited words, and bytes, then writes `<lines> <words> <bytes>` to its
//! stdout target. The reducing end of the classic `cat FILE | grep x | wc`.
//! Byte-streaming - no line buffer needed, since the counts don't depend on
//! line boundaries beyond counting `\n`.

#![no_std]
#![no_main]

#[no_mangle]
#[link_section = ".text.start"]
pub extern "C" fn _start() -> ! {
    let target = ulib::stdout_target();

    let mut lines: u64 = 0;
    let mut words: u64 = 0;
    let mut bytes: u64 = 0;
    let mut in_word = false;

    let mut buf = [0u8; syscall_abi::MSG_MAX_LEN as usize];
    loop {
        let n = ulib::pipe_recv(&mut buf);
        if n == 0 {
            break;
        }
        for &b in &buf[..n] {
            bytes += 1;
            if b == b'\n' {
                lines += 1;
            }
            let ws = b == b' ' || b == b'\t' || b == b'\n' || b == b'\r';
            if ws {
                in_word = false;
            } else if !in_word {
                in_word = true;
                words += 1;
            }
        }
    }

    let mut out = [0u8; 64];
    let mut len = 0usize;
    ulib::emit_dec(&mut out, &mut len, lines);
    ulib::emit(&mut out, &mut len, b" ");
    ulib::emit_dec(&mut out, &mut len, words);
    ulib::emit(&mut out, &mut len, b" ");
    ulib::emit_dec(&mut out, &mut len, bytes);
    ulib::emit(&mut out, &mut len, b"\r\n");
    ulib::write_out(target, &out[..len]);
    ulib::end_of_stream(target);
    ulib::exit(0);
}
