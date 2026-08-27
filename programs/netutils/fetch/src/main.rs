//! `fetch` - externalized HTTP GET (standalone-binaries Stage 4, netd command).
//! `fetch <hostname>` asks the network server to open a client TCP connection to
//! the host on port 80, send a minimal HTTP GET, and return the response, which
//! is printed. All the connection logic lives in netd; this packs the request
//! and prints the reply over `ulib::net_call`. Reaches netd via the `TO_NET`
//! capability the shell delegates at spawn. Ported from the shell's `cmd_fetch`:
//! the body goes to the stdout target (so `fetch host > file` captures), errors
//! to the console. netd returns one bounded reply (the response is truncated to
//! what fits a message); this prints what came back and notes the total.

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
    ulib::usage_if_requested(b"usage: fetch <url>  (HTTP GET a URL and print the response)\r\n");
    let target = ulib::stdout_target();

    let mut argbuf = [0u8; ulib::PATH_MAX];
    let host = match ulib::arg(1, &mut argbuf) {
        Some(len) if len > 0 => core::str::from_utf8(&argbuf[..len]).unwrap_or(""),
        _ => fail(b"fetch: usage: fetch <hostname>\r\n", target),
    };
    let hb = host.as_bytes();
    if hb.is_empty() || 8 + hb.len() > syscall_abi::MSG_MAX_LEN as usize {
        fail(b"fetch: usage: fetch <hostname>\r\n", target);
    }

    // Request: [NETOP_FETCH][hostname bytes]; reply: [status][total][body...].
    let mut req = [0u8; syscall_abi::MSG_MAX_LEN as usize];
    req[0..8].copy_from_slice(&syscall_abi::NETOP_FETCH.to_le_bytes());
    req[8..8 + hb.len()].copy_from_slice(hb);
    let mut reply = [0u8; syscall_abi::MSG_MAX_LEN as usize];

    let packed = ulib::net_call(&req[..8 + hb.len()], &mut reply);
    if packed == syscall_abi::TASK_ERR_NO_SUCH_TASK {
        fail(b"fetch: no network server this boot\r\n", target);
    }
    if packed >= syscall_abi::FS_ERR_MIN {
        fail(b"fetch: request failed\r\n", target);
    }
    let reply_len = (packed & 0xffff_ffff) as usize;
    let status = u64::from_le_bytes([
        reply[0], reply[1], reply[2], reply[3], reply[4], reply[5], reply[6], reply[7],
    ]);
    let total = u64::from_le_bytes([
        reply[8], reply[9], reply[10], reply[11], reply[12], reply[13], reply[14], reply[15],
    ]);
    match status {
        syscall_abi::NET_FETCH_OK => {
            let end = reply_len.min(reply.len());
            let body = if end > 16 { &reply[16..end] } else { &[][..] };
            ulib::write_out(target, body);
            if (total as usize) > body.len() {
                let mut note = [0u8; 64];
                let mut n = 0usize;
                ulib::emit(&mut note, &mut n, b"\r\n[fetch: response truncated - ");
                ulib::emit_dec(&mut note, &mut n, total);
                ulib::emit(&mut note, &mut n, b" bytes total]\r\n");
                // The truncation note is diagnostic - console (stderr), so a
                // captured body stays byte-exact.
                ulib::con_write(&note[..n]);
            }
            ulib::end_of_stream(target);
            ulib::exit(0);
        }
        syscall_abi::NET_FETCH_TIMEOUT => {
            ulib::con_write(b"fetch: no response from ");
            ulib::con_write(host.as_bytes());
            fail(b"\r\n", target);
        }
        syscall_abi::NET_FETCH_REFUSED => {
            ulib::con_write(b"fetch: connection refused by ");
            ulib::con_write(host.as_bytes());
            fail(b"\r\n", target);
        }
        syscall_abi::NET_FETCH_NO_ROUTE => {
            ulib::con_write(b"fetch: could not reach ");
            ulib::con_write(host.as_bytes());
            fail(b"\r\n", target);
        }
        syscall_abi::NET_FETCH_NO_NIC => fail(b"fetch: no network interface this boot\r\n", target),
        _ => fail(b"fetch: unexpected result\r\n", target),
    }
}
