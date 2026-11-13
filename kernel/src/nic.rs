//! The network device the `NET_*` syscalls sit on: an enum over the concrete
//! drivers, the idiom `block.rs` uses for the disks and `console.rs` for the
//! consoles (an enum and not a trait object, since `Box<dyn ...>` needs the
//! boot-services allocator that is gone after the exit).
//!
//! Two arms: virtio-net, and since the plan's step 5 the Pi 4's GENET
//! (`docs/roadmap/roadmap-pi-net.md`, design 6; `genetnic.rs`). The enum
//! came first, in step 3, so the driver arrived as an arm and
//! `syscall.rs` and `netd` did not change with it. **The GENET arm is
//! bring-up alone so far:** it answers `mac` and `has_frame`, refuses a
//! send and returns no frame, until steps 6 and 7.
//!
//! What every arm owes its callers, stated here because the callers are in
//! three files and cannot see a driver:
//!
//! - [`Nic::has_frame`] is called from the tick's IRQ path under the kernel
//!   lock (`tasks.rs`'s `NetInput` wake check, through
//!   `syscall::net_has_frame`). It reads and does nothing else: no side
//!   effect on the device, no wait.
//! - [`Nic::intid`] is an interrupt that means "a frame was received" and
//!   nothing else, and that [`Nic::ack_interrupt`] clears at the device.
//!   `main.rs` enables whatever INTID is reported, and `exceptions.rs` wakes
//!   the network server on it without asking the device what happened. An
//!   arm that cannot promise both reports `None` and is polled on the tick.
//! - Every arm takes a frame to send, and a buffer to receive into, of any
//!   length up to `syscall_abi::NET_MAX_FRAME`. That bound is the ABI's and
//!   is the same for every device, so `netd`'s buffers do not depend on the
//!   driver below. The syscalls refuse anything longer before a driver sees
//!   it, and the drivers rely on that: virtio-net's `poll_frame` indexes its
//!   own buffer by the caller's length. An arm whose device carries less
//!   refuses the frame in `send_frame` and never returns a longer one
//!   (GENET: 1532 to send, 1536 received). The assertions below hold each
//!   arm to the bound.

use crate::genetnic;
use crate::virtio_net;

/// One error type across the drivers. `NET_SEND` answers `NET_ERROR` for any
/// of them and nothing prints one today, so the `Display` below has no
/// caller yet: a send that times out leaves no line.
#[derive(Debug)]
pub enum Error {
    Virtio(virtio_net::Error),
    Genet(genet::mac::TxError),
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::Virtio(e) => write!(f, "virtio-net: {e}"),
            Error::Genet(e) => write!(f, "genet: {e:?}"),
        }
    }
}

pub enum Nic {
    Virtio(virtio_net::Device),
    /// The Pi 4's on-board Ethernet (`genetnic.rs`).
    Genet(genetnic::Device),
}

// The ABI's bound against the arm's own: a frame or buffer the syscalls let
// through is one virtio-net's buffers hold.
const _: () = assert!(syscall_abi::NET_MAX_FRAME as usize <= virtio_net::MAX_FRAME);
// GENET carries less than the bound in both directions, so it refuses a
// longer send itself and returns no frame longer than the bound.
const _: () = assert!(genet::mac::RX_MAX_FRAME <= syscall_abi::NET_MAX_FRAME as usize);

impl Nic {
    /// The device's MAC address, as `NET_MAC` returns it.
    pub fn mac(&self) -> [u8; 6] {
        match self {
            Nic::Virtio(d) => d.mac(),
            Nic::Genet(d) => d.mac(),
        }
    }

    /// The GIC INTID of the receive interrupt, or `None` for a device that
    /// is polled on the tick. See the module doc for what reporting one
    /// promises.
    pub fn intid(&self) -> Option<u32> {
        match self {
            Nic::Virtio(d) => d.intid(),
            // Polled, whatever its `_CRS` names (the plan's design 7): an
            // INTID reported is one `main.rs` enables, and nothing here
            // would clear the level-triggered line.
            Nic::Genet(_) => None,
        }
    }

    /// Clears the pending receive interrupt at the device, so it can raise
    /// the next one.
    ///
    /// # Safety
    /// The driver's own contract: the device initialized, called from the
    /// IRQ path.
    pub unsafe fn ack_interrupt(&self) {
        match self {
            Nic::Virtio(d) => unsafe { d.ack_interrupt() },
            // No interrupt is reported, so none is taken.
            Nic::Genet(_) => {}
        }
    }

    /// Sends one Ethernet frame, waiting at most `timeout_ticks` of the
    /// generic timer's counter for the device to take it.
    ///
    /// # Safety
    /// The driver's own contract: the device initialized, its MMIO and DMA
    /// ranges mapped under this kernel's tables. `frame` is no longer than
    /// `syscall_abi::NET_MAX_FRAME`.
    pub unsafe fn send_frame(&mut self, frame: &[u8], timeout_ticks: u64) -> Result<(), Error> {
        match self {
            Nic::Virtio(d) => unsafe { d.send_frame(frame, timeout_ticks) }.map_err(Error::Virtio),
            // Its own bound on the wait (`genet::mac::TX_POLLS`).
            Nic::Genet(d) => d.send_frame(frame).map_err(Error::Genet),
        }
    }

    /// Whether a received frame is waiting, without taking it. A read with
    /// no side effect and no wait (the module doc says who depends on that).
    ///
    /// # Safety
    /// As [`Self::send_frame`].
    pub unsafe fn has_frame(&self) -> bool {
        match self {
            Nic::Virtio(d) => unsafe { d.has_frame() },
            Nic::Genet(d) => d.has_frame(),
        }
    }

    /// Takes one received frame if there is one: copies it into `out`, cut
    /// to `out`'s length, and returns the frame's own length, which can be
    /// more than `out`'s (`NET_RECV` passes it on as it is).
    ///
    /// # Safety
    /// As [`Self::send_frame`], and `out` is no longer than
    /// `syscall_abi::NET_MAX_FRAME`: virtio-net indexes its own receive
    /// buffer by `out`'s length.
    pub unsafe fn poll_frame(&mut self, out: &mut [u8]) -> Option<usize> {
        match self {
            Nic::Virtio(d) => unsafe { d.poll_frame(out) },
            Nic::Genet(d) => d.poll_frame(out),
        }
    }
}
