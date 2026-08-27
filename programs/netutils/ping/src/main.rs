//! `ping` - externalized ICMP echo (standalone-binaries Stage 4, first netd
//! command). `ping <a.b.c.d>` asks the network server to ARP-resolve the host
//! and send it an ICMP echo request, reporting whether a reply came back. The
//! whole protocol stack lives in netd; this just packs the target and reads the
//! status over `ulib::net_call`. Reaching netd needs `TO_NET`, which the shell
//! delegates to this task at spawn (spawnable slots don't hold it statically).
//! Ported from the shell's `cmd_ping` - success goes to the stdout target (so
//! `ping x > file` captures), errors to the console (the stderr split).

#![no_std]
#![no_main]

/// Parse a dotted-quad IPv4 (`10.0.2.2`) into its four octets. Hand-rolled
/// byte-by-byte (no `str::split`) - relocation-safe, from the shell's original.
fn parse_ipv4(s: &str) -> Option<[u8; 4]> {
    let mut out = [0u8; 4];
    let mut idx = 0;
    let mut val: u32 = 0;
    let mut digits = 0;
    for &c in s.as_bytes() {
        if c == b'.' {
            if digits == 0 || idx >= 3 {
                return None;
            }
            out[idx] = val as u8;
            idx += 1;
            val = 0;
            digits = 0;
        } else if c.is_ascii_digit() {
            val = val * 10 + (c - b'0') as u32;
            if val > 255 {
                return None;
            }
            digits += 1;
        } else {
            return None;
        }
    }
    if digits == 0 || idx != 3 {
        return None;
    }
    out[3] = val as u8;
    Some(out)
}

/// Print an error to the console (stderr), close the stdout stream (a no-op for
/// the console, an empty end-of-stream for a capture), and exit non-zero.
fn fail(msg: &[u8], target: u64) -> ! {
    ulib::con_write(msg);
    ulib::end_of_stream(target);
    ulib::exit(1);
}

#[no_mangle]
#[link_section = ".text.start"]
pub extern "C" fn _start() -> ! {
    ulib::usage_if_requested(b"usage: ping <host>  (ICMP echo an IPv4 address or hostname)\r\n");
    let target = ulib::stdout_target();

    let mut argbuf = [0u8; 32];
    let ip = match ulib::arg(1, &mut argbuf) {
        Some(len) if len > 0 => core::str::from_utf8(&argbuf[..len]).unwrap_or(""),
        _ => fail(b"ping: usage: ping <a.b.c.d>\r\n", target),
    };
    let Some(target_ip) = parse_ipv4(ip) else {
        fail(b"ping: usage: ping <a.b.c.d>\r\n", target);
    };

    // Request: [NETOP_PING][target IPv4 packed LE]; reply: [status].
    let packed_target = target_ip[0] as u64
        | (target_ip[1] as u64) << 8
        | (target_ip[2] as u64) << 16
        | (target_ip[3] as u64) << 24;
    let mut req = [0u8; 16];
    req[0..8].copy_from_slice(&syscall_abi::NETOP_PING.to_le_bytes());
    req[8..16].copy_from_slice(&packed_target.to_le_bytes());
    let mut reply = [0u8; syscall_abi::MSG_MAX_LEN as usize];

    let packed = ulib::net_call(&req, &mut reply);
    if packed == syscall_abi::TASK_ERR_NO_SUCH_TASK {
        fail(b"ping: no network server this boot\r\n", target);
    }
    if packed >= syscall_abi::FS_ERR_MIN {
        fail(b"ping: request failed\r\n", target);
    }
    let status = u64::from_le_bytes([
        reply[0], reply[1], reply[2], reply[3], reply[4], reply[5], reply[6], reply[7],
    ]);
    match status {
        syscall_abi::NET_PING_OK => {
            ulib::write_out(target, b"reply from ");
            ulib::write_out(target, ip.as_bytes());
            ulib::write_out(target, b"\r\n");
            ulib::end_of_stream(target);
            ulib::exit(0);
        }
        syscall_abi::NET_PING_TIMEOUT => {
            ulib::con_write(b"no reply from ");
            ulib::con_write(ip.as_bytes());
            fail(b" (timeout)\r\n", target);
        }
        syscall_abi::NET_PING_NO_ARP => {
            ulib::con_write(ip.as_bytes());
            fail(b" is unreachable (no ARP reply)\r\n", target);
        }
        syscall_abi::NET_PING_NO_NIC => fail(b"ping: no network interface this boot\r\n", target),
        _ => fail(b"ping: unexpected result\r\n", target),
    }
}
