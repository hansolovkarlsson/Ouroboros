//! `uptime` - externalized command (standalone-binaries Stage 4). Prints the
//! preemption tick count since boot, via the `get_ticks` syscall. No
//! filesystem, no cwd - part of the first externalized batch.

#![no_std]
#![no_main]

#[no_mangle]
#[link_section = ".text.start"]
pub extern "C" fn _start() -> ! {
    let target = ulib::stdout_target();
    let mut buf = [0u8; 48];
    let mut n = 0;
    ulib::emit_dec(&mut buf, &mut n, ulib::get_ticks());
    ulib::emit(&mut buf, &mut n, b" ticks since boot\r\n");
    ulib::write_out(target, &buf[..n]);
    ulib::end_of_stream(target);
    ulib::exit(0);
}
