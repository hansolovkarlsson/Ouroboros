//! `netd` - the network server, the eighth userland program and the fourth
//! protected server (task slot [`syscall_abi::NET_TASK`], 4). Stage 2a of the
//! network stack (`docs/roadmap.md`): it drives the kernel's virtio-net NIC
//! entirely from EL0 through the gated `NET_SEND`/`NET_RECV`/`NET_MAC`
//! syscalls - the DMA-owning driver stays in the kernel (no IOMMU), the
//! protocol stack lives here, the `fsd`/`BLOCK_*` pattern.
//!
//! This first cut proves the userland NIC path works: at startup it builds a
//! broadcast ARP request for the QEMU user-net gateway *from userland* and
//! confirms the reply comes back through `NET_RECV`. Then it becomes an
//! ordinary request server - blocking on `MSG_RECV` and replying to each
//! message - which is also what keeps the supervisor's health-ping acked
//! (an unknown message gets a status reply, and the reply addressed to the
//! kernel's ping sentinel is the ack). Real ARP resolution, IPv4, ICMP, and
//! a `ping` command are Stage 2b; the hardcoded QEMU user-net IPs here are
//! exactly what `init_net`'s old in-kernel probe used, a QEMU-shaped
//! convention to be replaced by real ARP/DHCP.
//!
//! Built like every userland program: `aarch64-unknown-none`, release-only,
//! the shared `shell/linker.ld`, staged as `\EFI\ORBS\NETD.BIN`.

#![no_std]
#![no_main]

use core::arch::asm;
use core::panic::PanicInfo;

#[no_mangle]
#[link_section = ".text.start"]
pub extern "C" fn _start() -> ! {
    main()
}

fn main() -> ! {
    log(b"netd: network server ready\r\n");

    // Our MAC (the Ethernet source of every frame we build).
    let packed = syscall(syscall_abi::NET_MAC, 0);
    if packed == syscall_abi::NET_ERROR {
        log(b"netd: no NIC this boot - network unavailable\r\n");
        serve();
    }
    let mac = [
        packed as u8,
        (packed >> 8) as u8,
        (packed >> 16) as u8,
        (packed >> 24) as u8,
        (packed >> 32) as u8,
        (packed >> 40) as u8,
    ];

    // Stage 2a liveness proof: a broadcast ARP request ("who has 10.0.2.2?
    // tell 10.0.2.15"), built and sent from userland, its reply received via
    // NET_RECV - proving the whole EL0 -> gated-syscall -> kernel-driver ->
    // wire path works.
    let mut arp = [0u8; 42];
    arp[0..6].copy_from_slice(&[0xff; 6]); // eth dst: broadcast
    arp[6..12].copy_from_slice(&mac); // eth src
    arp[12..14].copy_from_slice(&[0x08, 0x06]); // ethertype: ARP
    arp[14..16].copy_from_slice(&[0x00, 0x01]); // htype: Ethernet
    arp[16..18].copy_from_slice(&[0x08, 0x00]); // ptype: IPv4
    arp[18] = 6; // hlen
    arp[19] = 4; // plen
    arp[20..22].copy_from_slice(&[0x00, 0x01]); // oper: request
    arp[22..28].copy_from_slice(&mac); // sha: our MAC
    arp[28..32].copy_from_slice(&[10, 0, 2, 15]); // spa: our assumed IP
    arp[38..42].copy_from_slice(&[10, 0, 2, 2]); // tpa: the gateway

    if syscall4(syscall_abi::NET_SEND, arp.as_ptr() as u64, arp.len() as u64, 0, 0) != 0 {
        log(b"netd: ARP send failed\r\n");
        serve();
    }

    // Poll for the reply for ~1s (50 ticks of 20ms). GET_TICKS advances as
    // the timer preempts this busy poll.
    let deadline = syscall(syscall_abi::GET_TICKS, 0) + 50;
    let mut frame = [0u8; 1600];
    loop {
        let r = syscall4(syscall_abi::NET_RECV, frame.as_mut_ptr() as u64, frame.len() as u64, 0, 0);
        if r != syscall_abi::NET_NO_FRAME && r != syscall_abi::NET_ERROR {
            let len = r as usize;
            if len >= 42
                && frame[12] == 0x08
                && frame[13] == 0x06 // ethertype ARP
                && frame[20] == 0x00
                && frame[21] == 0x02 // oper: reply
                && frame[28..32] == [10, 0, 2, 2]
            // sender: the gateway
            {
                log(b"netd: ARP reply received from userland - network is up\r\n");
                break;
            }
        }
        if syscall(syscall_abi::GET_TICKS, 0) > deadline {
            log(b"netd: no ARP reply within 1s\r\n");
            break;
        }
    }

    serve();
}

/// The request loop: block on `MSG_RECV`, reply to each message with a
/// status. Stage 2a has no real operations yet (Stage 2b adds a `NETOP_PING`
/// handler), but replying is what acks the supervisor's health-ping (the
/// reply addressed to the kernel's sentinel is intercepted as the ack) and
/// keeps this task blocked-and-healthy rather than a busy wedge.
fn serve() -> ! {
    let mut buf = [0u8; syscall_abi::MSG_MAX_LEN as usize];
    loop {
        let packed = syscall4(syscall_abi::MSG_RECV, buf.as_mut_ptr() as u64, buf.len() as u64, 0, 0);
        if packed >= syscall_abi::FS_ERR_MIN {
            continue; // interrupted or error - just wait again
        }
        let sender = packed >> 32;
        // Reply with a single-u64 status (0). For the supervisor's ping this
        // is the ack; for a stray message a harmless status; real ops in 2b.
        let status = 0u64;
        syscall4(syscall_abi::MSG_SEND, sender, &status as *const u64 as u64, 8, 0);
    }
}

/// Route a log line through the console server (task `CON_TASK`) as a
/// batched `DSPOP_WRITE` message, falling back to the kernel console (`PUTC`)
/// if there's no server this boot - the same shape as every other program's
/// `con_write`.
fn log(bytes: &[u8]) {
    let payload_off = syscall_abi::FS_REQ_PAYLOAD as usize;
    let mut off = 0;
    while off < bytes.len() {
        let n = (bytes.len() - off).min(syscall_abi::FS_DATA_MAX as usize);
        let mut req = [0u8; syscall_abi::FS_REQ_PAYLOAD as usize + syscall_abi::FS_DATA_MAX as usize];
        req[0..8].copy_from_slice(&syscall_abi::DSPOP_WRITE.to_le_bytes());
        req[8..16].copy_from_slice(&(n as u64).to_le_bytes());
        req[payload_off..payload_off + n].copy_from_slice(&bytes[off..off + n]);
        let mut reply = [0u8; syscall_abi::MSG_MAX_LEN as usize];
        let r = syscall4(
            syscall_abi::MSG_CALL,
            syscall_abi::CON_TASK,
            req.as_ptr() as u64,
            (payload_off + n) as u64,
            reply.as_mut_ptr() as u64,
        );
        if r >= syscall_abi::FS_ERR_MIN {
            for &b in &bytes[off..off + n] {
                syscall(syscall_abi::PUTC, b as u64);
            }
        }
        off += n;
    }
}

#[inline(always)]
fn syscall4(number: u64, arg0: u64, arg1: u64, arg2: u64, arg3: u64) -> u64 {
    let ret: u64;
    unsafe {
        asm!(
            "svc #0",
            inout("x0") arg0 => ret,
            in("x1") arg1,
            in("x2") arg2,
            in("x3") arg3,
            in("x8") number,
            options(nostack),
        );
    }
    ret
}

#[inline(always)]
fn syscall(number: u64, arg0: u64) -> u64 {
    let ret: u64;
    unsafe {
        asm!(
            "svc #0",
            inout("x0") arg0 => ret,
            in("x1") 0u64,
            in("x2") 0u64,
            in("x3") 0u64,
            in("x8") number,
            options(nostack),
        );
    }
    ret
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
