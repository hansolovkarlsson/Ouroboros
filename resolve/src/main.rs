//! `resolve` - externalized DNS lookup (standalone-binaries Stage 4, netd
//! command). `resolve <hostname>` asks the network server to look up a
//! hostname's IPv4 via DNS-over-UDP and prints the result. The DNS logic lives
//! in netd; this packs the query and formats the reply over `ulib::net_call`.
//! Reaches netd via the `TO_NET` capability the shell delegates at spawn.
//! Ported from the shell's `cmd_resolve` - success to the stdout target, errors
//! to the console.

#![no_std]
#![no_main]

/// Print an error to the console, close the stdout stream, exit non-zero.
fn fail(msg: &[u8], target: u64) -> ! {
    ulib::con_write(msg);
    ulib::end_of_stream(target);
    ulib::exit(1);
}

#[no_mangle]
#[link_section = ".text.start"]
pub extern "C" fn _start() -> ! {
    let target = ulib::stdout_target();

    let mut argbuf = [0u8; ulib::PATH_MAX];
    let host = match ulib::arg(1, &mut argbuf) {
        Some(len) if len > 0 => core::str::from_utf8(&argbuf[..len]).unwrap_or(""),
        _ => fail(b"resolve: usage: resolve <hostname>\r\n", target),
    };
    let hb = host.as_bytes();
    if hb.is_empty() || 8 + hb.len() > syscall_abi::MSG_MAX_LEN as usize {
        fail(b"resolve: usage: resolve <hostname>\r\n", target);
    }

    // Request: [NETOP_RESOLVE][hostname bytes]; reply: [status][ipv4].
    let mut req = [0u8; syscall_abi::MSG_MAX_LEN as usize];
    req[0..8].copy_from_slice(&syscall_abi::NETOP_RESOLVE.to_le_bytes());
    req[8..8 + hb.len()].copy_from_slice(hb);
    let mut reply = [0u8; syscall_abi::MSG_MAX_LEN as usize];

    let packed = ulib::net_call(&req[..8 + hb.len()], &mut reply);
    if packed == syscall_abi::TASK_ERR_NO_SUCH_TASK {
        fail(b"resolve: no network server this boot\r\n", target);
    }
    if packed >= syscall_abi::FS_ERR_MIN {
        fail(b"resolve: request failed\r\n", target);
    }
    let status = u64::from_le_bytes([
        reply[0], reply[1], reply[2], reply[3], reply[4], reply[5], reply[6], reply[7],
    ]);
    let ip = u64::from_le_bytes([
        reply[8], reply[9], reply[10], reply[11], reply[12], reply[13], reply[14], reply[15],
    ]);
    match status {
        syscall_abi::NET_RESOLVE_OK => {
            let mut line = [0u8; 64];
            let mut n = 0usize;
            ulib::emit(&mut line, &mut n, host.as_bytes());
            ulib::emit(&mut line, &mut n, b" is ");
            ulib::emit_dec(&mut line, &mut n, ip & 0xff);
            ulib::emit(&mut line, &mut n, b".");
            ulib::emit_dec(&mut line, &mut n, (ip >> 8) & 0xff);
            ulib::emit(&mut line, &mut n, b".");
            ulib::emit_dec(&mut line, &mut n, (ip >> 16) & 0xff);
            ulib::emit(&mut line, &mut n, b".");
            ulib::emit_dec(&mut line, &mut n, (ip >> 24) & 0xff);
            ulib::emit(&mut line, &mut n, b"\r\n");
            ulib::write_out(target, &line[..n]);
            ulib::end_of_stream(target);
            ulib::exit(0);
        }
        syscall_abi::NET_RESOLVE_TIMEOUT => {
            ulib::con_write(b"resolve: no response for ");
            ulib::con_write(host.as_bytes());
            fail(b"\r\n", target);
        }
        syscall_abi::NET_RESOLVE_NXDOMAIN => {
            ulib::con_write(b"resolve: could not resolve ");
            ulib::con_write(host.as_bytes());
            fail(b"\r\n", target);
        }
        syscall_abi::NET_RESOLVE_NO_NIC => {
            fail(b"resolve: no network interface this boot\r\n", target)
        }
        _ => fail(b"resolve: unexpected result\r\n", target),
    }
}
