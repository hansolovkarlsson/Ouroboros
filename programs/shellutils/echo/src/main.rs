//! `echo` - the first externalized command (standalone-binaries Stage 4).
//! Prints its arguments (argv[1..]) separated by single spaces, then a
//! newline. Formerly a shell builtin; now a real `/bin/echo` program, found
//! by PATH and run with the line as its argv. Needs neither the filesystem
//! nor the shell's cwd, which is why it's in the first externalized batch.

#![no_std]
#![no_main]

#[no_mangle]
#[link_section = ".text.start"]
pub extern "C" fn _start() -> ! {
    let target = ulib::stdout_target();
    let n = ulib::argc();
    let mut abuf = [0u8; 512];
    let mut i = 1u64; // argv[0] is the program name ("echo")
    let mut first = true;
    while i < n {
        if !first {
            ulib::write_out(target, b" ");
        }
        if let Some(len) = ulib::arg(i, &mut abuf) {
            ulib::write_out(target, &abuf[..len]);
        }
        first = false;
        i += 1;
    }
    ulib::write_out(target, b"\r\n");
    ulib::end_of_stream(target);
    ulib::exit(0);
}
