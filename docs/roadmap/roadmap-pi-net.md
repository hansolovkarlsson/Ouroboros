# Networking on the Pi 4: the plan

*Set by Hans on 2026-11-10 as the first new work after the outstanding
fixes, and on 2026-11-12 pointed at the on-board hardware: "go with the
on-board hardware first for the Pi. USB ethernet adapter is something we
should add as well but later on, not right now." This is `ROADMAP.md`'s
Raspberry Pi item 7, made an arc. The USB adapter is item 8 there and is
not part of this plan.*

## Why

A Pi 4 has no network today. The kernel's one NIC driver is `virtio_net.rs`,
the Pi has no virtio device of any kind, so `netd` reports no NIC and
`ping`, `resolve`, `fetch`, `mount -r`, `cpu`, the 9P export and cluster
auth are all QEMU-only (`testing-pi4.md` section 2). Two things wait on it:
the two-node cluster on real hardware, which is the north star's first proof
off an emulator and the reason a second board is a spare and not a peer; and
SSH (Pi item 5).

**Done when** a Pi 4 with a cable in its Ethernet port answers a `ping` from
the Mac and pings it back, `fetch`es a file over the LAN whole (its bytes
checked against the Mac's copy), and two Pi 4s on one switch run the
cluster's own proof between them: `mount -r` of one from the other and a
`cpu` run, with keys.

## What is known

Each line says where it was read. Nothing here has been read off the board's
registers yet; step 2 does that.

**From the board's own ACPI tables** (`amlscan/testdata/pi4-dsdt.aml`,
dumped off the board on 2026-11-05, read on 2026-11-12):

- The NIC is `Device (ETH0)` directly under `\_SB`, `_HID "BCM6E4E"` (and
  the same string as its `_CID`).
- `_STA` is a method, `Return (0x0F)`: present and enabled.
- `_CRS` is a method that names a buffer inside itself and returns it: one
  `Memory32Fixed` at `0xFD580000`, length `0x10000`, and one Extended
  Interrupt naming two INTIDs, 189 and 190. `amlscan` did not read this
  method shape before step 1.
- `_CCA` is 0: the device's DMA is not cache-coherent. The kernel already
  maps its DMA pool non-cacheable when any table says `_CCA 0`
  (`acpi::dma_noncoherent`, there for the Pi's xHCI).
- `ETH0` has no `_DMA` of its own and is not under a bus that has one
  (predicted from that: bus addresses are CPU addresses for it).
- Its `_DSD` gives `phy-mode` as `"rgmii-rxid"` and
  `brcm,max-dma-burst-size` as 8.
- The Pi publishes no devicetree (`roadmap-sd-card.md`, "What is known"),
  so ACPI is the only source.

**From U-Boot's `drivers/net/bcmgenet.c` and Linux's `bcmgenet.h`** (both
fetched and read on 2026-11-12; U-Boot's driver is the model, since it is
about 700 lines, polls, and uses one queue, which is what this arc wants
first):

- **Version.** `SYS_REV_CTRL` at 0x00; bits 27:24 read 6 on GENET v5, and
  U-Boot accepts nothing else.
- **Blocks.** SYS at 0x0000, EXT at 0x0080, INTRL2_0 at 0x0200, INTRL2_1 at
  0x0240, RBUF at 0x0300, UMAC at 0x0800, receive DMA at 0x2000, transmit
  DMA at 0x4000.
- **The descriptors are registers, not memory.** Each direction has 256
  descriptors of 12 bytes (length and status, address low, address high)
  inside the register block, then the ring registers after them (0x2C00,
  0x4C00). Only the packet buffers are DMA memory. The default queue is
  ring 16 (`DEFAULT_Q 0x10`), and it is the only one U-Boot sets up.
- **Receive.** 2048-byte buffers, a 2-byte pad before each frame
  (`RBUF_ALIGN_2B`), the device's producer index and the driver's consumer
  index as 16-bit counters over a 256-entry ring.
- **Transmit.** Address and length written to the descriptor with SOP, EOP
  and append-CRC, the producer index advanced, completion seen in the
  device's consumer index.
- **Reset.** RBUF flush, `UMAC_CMD` software reset with local loopback,
  MIB counters reset, max frame length, the 2-byte alignment, then the MAC
  address written to `UMAC_MAC0`/`UMAC_MAC1`, DMA off, rings set, DMA on,
  the PHY started, the speed written to `UMAC_CMD` and
  `EXT_RGMII_OOB_CTRL`, and last the transmit and receive enables.
- **MDIO** is one register, `MDIO_CMD` at 0x0E14: a command word with the
  PHY address and register, a start bit that reads back busy until done.
- **Interrupts.** U-Boot uses none. Linux's header gives the INTRL2 block's
  status, clear and mask registers, and the bits: receive done is bit 13,
  transmit done bit 16, link up and down bits 4 and 5.
- **Cache.** U-Boot flushes and invalidates around every buffer, which is
  what `_CCA 0` means for a driver with cacheable buffers.

**Predicted, from memory of the Pi's devicetree and firmware sources, not
read on 2026-11-12:** the PHY is a BCM54213PE at MDIO address 1; the first
interrupt (189) carries the default queue and the link, the second (190)
the priority queues this plan does not use; the firmware leaves the board's
MAC address in `UMAC_MAC0`/`UMAC_MAC1` for the operating system to read
(Linux reads it there under ACPI); the firmware's own driver is idle unless
it network-booted.

**From this tree:**

- `init_net()` (`kernel/src/main.rs`) calls only
  `virtio_net::Device::discover`. The device then sits in one `SyncCell` in
  `syscall.rs` behind `NET_SEND`, `NET_RECV`, `NET_MAC` and `NET_WAIT`, all
  gated to `netd`.
- **The kernel reaches the NIC in six places, not four.** Beside the four
  syscalls, `syscall::net_has_frame()` asks the device whether a frame
  waits, and `syscall::net_intid()` gives `main.rs` the interrupt it
  enables at the GIC whenever the device reports one.
- **An idle `netd` waits with no deadline** (`NET_WAIT` with timeout 0;
  200 ms, `RTO_POLL_MS`, only while TCP data is unacknowledged). What
  wakes it without an interrupt is the tick: every 20 ms `tasks.rs`'s
  `on_tick` calls `net_has_frame()` for a task parked in `NET_WAIT`. So a
  NIC with no interrupt is already polled, by the kernel, at the tick.
- **The non-cacheable DMA memory is the xHCI's own.** `xhci.rs`'s
  `DMA_POOL` is one static of that driver's fields, and it is the only
  range `main.rs` passes to `mmu::install_identity_map` as `uncached`.
  There is no general pool to take a NIC's buffers from.
- `netd` has no address configuration. `our_ip()` returns a `10.0.2.x`
  address, with the gateway and DNS server at QEMU SLIRP's fixed addresses
  (`programs/servers/netd/src/main.rs`, whose module doc says "DHCP/routing
  is future work"). On a real LAN those are wrong.

**No emulator can stand in** (read 2026-11-12 against QEMU 11.1.3):
`raspi4b` is the only machine near it, it has no GENET (the Ethernet range
is among its eleven `unimplemented-device` regions), no GENET device can be
added to `virt`, and `raspi4b` does not boot this kernel's UEFI path. The
SD arc wrote its driver on QEMU's `sdhci-pci`; this arc has no such stand-in,
and the design's point 2 is the answer to that.

## The design

1. **Found, never assumed.** `amlscan::declared` with `GENET_HID`, by the
   rules virtio-mmio and EMMC2 are found by: a table walked whole, a `_STA`
   that says present, a `_CRS` the scan reads exactly. No declaration means
   no NIC, never a guessed `0xFD580000`.
2. **The driver's logic is a pure crate, `genet`, over a register closure.**
   Every register read and write goes through a `Regs` the caller passes
   (as `parttable` takes a read-sector closure), so the whole bring-up, the
   MDIO exchanges, a transmit and a receive run on the host against a model
   of the device written from the two sources above. The kernel's
   `genet.rs` is the thin half: the MMIO `Regs`, the buffers in the DMA
   pool, the delay. **What this is and is not:** the model is ours, so the
   host tests check the driver against our reading of the device and catch
   arithmetic, ordering and bounds. They cannot catch a wrong reading. The
   board is the only foreign observer, and each board step names what on
   the wire or on the Mac must be seen.
3. **Reset from scratch, the MAC address kept.** The driver reads the MAC
   address first, then resets the device and sets it up as U-Boot does,
   trusting nothing else the firmware left, so the state it starts from is
   the same every boot. A MAC address of all zeros or all ones is no NIC:
   none is made up.
4. **Quiet at the exit.** Whatever the firmware's driver was doing, the
   device's DMA and its transmit and receive enables go off before the
   kernel uses memory the firmware owned, the same rule the xHCI takeover
   follows. Step 2 reads whether the firmware leaves DMA running.
5. **One queue, its buffers in a non-cacheable range of their own.** Ring
   16 alone, 2048-byte buffers, no priority queues, no checksum offload,
   the second interrupt unused. The receive and transmit buffers are one
   page-aligned static in `genet.rs`, and `main.rs` passes its range as a
   second `uncached` entry beside `xhci::dma_region()` when the firmware
   says `_CCA 0`, so the driver does no cache maintenance of its own, as
   `xhci.rs` does none; frames are copied in and out at the syscall, as
   virtio-net's are. Without that entry the buffers are cacheable memory
   on a non-coherent device, and the fault would show only on the board:
   step 5 has the check. How many receive buffers (U-Boot's 256 is half a
   megabyte) is step 4's to settle.
6. **A NIC enum behind the syscalls, `netd` unchanged.** `nic.rs` holds
   `enum Nic { Virtio, Genet }` the way `block.rs` holds the disks, and
   `syscall.rs`'s cell holds a `Nic`. `NET_SEND`, `NET_RECV`, `NET_MAC` and
   `NET_WAIT` keep their numbers and meanings. The enum also answers the
   kernel's two other questions, `has_frame` and `intid`. `has_frame` on
   GENET is one register read (the producer index against the consumer),
   made from the tick's IRQ path under the kernel lock, so it must be a
   read with no side effect and no wait.
7. **Polled first, then the interrupt.** The tick already asks the NIC
   for a waiting frame every 20 ms, so a driver with no interrupt works,
   a frame late by up to a tick. Until step 9 the GENET arm's `intid` is
   `None`, whatever the `_CRS` names: `main.rs` enables any INTID the
   device reports, and a level-triggered line enabled with nothing to
   clear it hangs the board at the first frame. The interrupt (INTID 189,
   receive done unmasked in INTRL2_0, cleared in the kernel's IRQ path) is
   its own step with its own check.
8. **An address from a file.** `netd` reads its address, mask, gateway and
   DNS server from a file on the root, and falls back to today's SLIRP
   values when the file is absent, so every QEMU rig runs unchanged.

## Decisions for Hans

Each has a recommendation; none blocks step 2.

- **D1. Discovery from ACPI alone.** Recommended, and step 1 built it: the
  board publishes no devicetree.
- **D2. Polled first, the interrupt as a later step** (design 7), or the
  interrupt from the start. Recommended: polled first. It is one variable
  fewer in the first round trips, the kernel's tick already does the
  polling, and U-Boot's driver is the proof the device works that way.
- **D3. The link.** Recommended: autonegotiation as the PHY does it out of
  reset, whatever speed and duplex result, read once at bring-up with a
  bounded wait and again whenever a send finds the link down. A cable
  plugged in after boot then works at the next send. Link-change
  interrupts are out of scope.
- **D4. The address.** A static file (design 8), or a DHCP client in
  `netd`. Recommended: the file now, DHCP as its own later item. Two
  boards on a switch with no router need static addresses anyway, and the
  cluster's `authorized` files name machines by address.
- **D5. A MAC address the firmware did not leave.** Recommended: no NIC,
  with a line saying so (design 3). The other way is asking the VideoCore
  mailbox for the board's address, a second new interface for one value.
- **D6. The pure crate** (design 2), or the driver written straight into
  the kernel as `sdhci.rs` was. Recommended: the crate. Without it nothing
  but the board can fail a check on the driver's logic.

## Steps, each with its check

0. **What can test it off the board.** Done 2026-11-12: nothing emulates
   it ("No emulator can stand in", above). The answer shaped design 2.

1. **Discovery, host-tested.** `amlscan` reads `ETH0` from the Pi's dumped
   DSDT. **Built 2026-11-12:** a third method shape,
   `MethodShape::CrsLocal`, for a `_CRS` that is only `Name (<buffer>,
   Buffer {...}); Return (<buffer>)` with the buffer named inside the
   method, and `GENET_HID`. The test on the board's own table gives
   `0xFD580000`, `0x10000`, INTID 189, with nothing unreadable, absent or
   unknown, alone and beside the EMMC2 SSDT in either order. Before the
   change the same test gave no device and `unreadable` 1. Eight more
   tests bend the shape (an argument, a Return of another name, a Name of
   another name, no Return, a statement after the Return, a buffer named
   outside the method with `^`, a second `_CRS` before or after, a moved
   base and a zero length), and the conditions of the new reader were
   taken out in turn to see one of them fail. One was found with no test
   that could fail for it (a second `_CRS` defined before the method),
   and got one; one was redundant (a bound the line after it implies) and
   was removed. **The review of #451 found that sweep incomplete:** the
   check that the method ends in a Return had no test that could fail for
   it (the test meant for it broke the name after the opcode too), and
   neither had a patching `_CRS` beside a local one. Both have a test now,
   each seen to fail with its condition out, and the rule "a second `_CRS`
   means none" is one function where it was three copies. `Transport`
   still carries one INTID: the second interrupt is not kept, by design 5.

2. **Read the board.** One boot under a new flag file, `\NETDUMP`, that
   logs before the exit: what `amlscan::declared` finds for `GENET_HID`,
   then, only if it found a device, `SYS_REV_CTRL`, `UMAC_CMD`,
   `UMAC_MAC0` and `UMAC_MAC1`, `EXT_RGMII_OOB_CTRL`, both DMA control
   registers, the INTRL2_0 mask status, and the PHY's two id registers and
   its status register over MDIO. The MDIO reads are the scan's only
   writes (one command word each); everything else is a read. Without the
   flag nothing here runs, as with `\SDDUMP`.
   **Check:** the version's major reads 6; the MAC address is the one the
   board's own firmware setup screen shows, and not zeros; the PHY id is a
   Broadcom one; the status register's link bit follows the cable, in two
   boots, cable in and cable out. On QEMU the same flag prints that no
   GENET is declared and touches no address (the control: `make
   test-early-fault`'s style of boot, graded on that line). This step
   settles the predictions under "What is known" and whether the firmware
   leaves DMA running (design 4).

3. **The NIC enum.** `nic.rs` with the virtio arm alone, `syscall.rs` and
   `init_net()` moved onto it. No behaviour changes.
   **Check:** every network rig on QEMU passes as before (`make
   test-async-rmount`, `test-run-callback`, `test-run-orphan`,
   `test-run-silent`, `test-cred-client`, `test-held-keys`,
   `test-call-interrupt`, `test-no-identity`, `test-server-children`), and
   `make-targets.md` is grepped for `virtio_net` and `NET_` for any other
   rig this owes.

4. **The `genet` crate and its model, on the host.** The register map as
   constants, the command words, the MAC address packing, the descriptor
   words, the index arithmetic (a 16-bit counter over a 256-entry ring),
   the bring-up sequence, the PHY's link and speed read, one transmit, one
   receive. A model device in the tests answers the registers.
   **Check:** `make test` runs it. Each constant is cited to its line in
   U-Boot's or Linux's source in a table the test file carries, and the
   tests include the counters' wrap at 65536 and the ring's at 256, a
   receive of a maximum-size frame, a frame longer than the buffer
   refused, and a busy MDIO that never clears ending in an error and not a
   hang. Every bound is mutated once.

5. **The board: bring-up and link.** The kernel's `genet.rs`, the device
   quiet at the exit, reset, the MAC address kept, the PHY read, its
   buffers' range mapped non-cacheable (design 5), `intid` `None` (design
   7). The boot log gains one line: the version, the MAC address, the
   link and its speed.
   **Check:** the line matches step 2's values; cable out reads link down
   and the boot goes on to a login; `NET_MAC` gives `netd` the address
   (`ping` then fails for want of an answer, not for want of a NIC). The
   buffers' mapping is read back from the translation tables by a walk,
   not from the list handed to `mmu.rs`, and logged; on QEMU, where
   nothing says `_CCA 0`, the same line must say cacheable (the control).
   A sustained boot shows no fault, with the tick calling `has_frame`
   throughout. One variable: nothing is sent yet.

6. **The board: transmit.** `netd` sends what it sends now. With the
   address file in place (step 8 may come first if that is simpler), a
   `ping` of the Mac makes an ARP request.
   **Check:** `tcpdump` on the Mac, the foreign observer, shows the ARP
   request with the board's MAC address as its source and a whole frame
   (the Mac's interface counts no CRC errors). A check that can fail: with
   the transmit enable left out, `tcpdump` shows nothing.

7. **The board: receive, polled.** The ARP reply and the echo reply come
   back through `NET_RECV`.
   **Check:** `ping` of the Mac from the Pi answers; `ping` of the Pi from
   the Mac answers; `fetch` of a file of a few megabytes from the Mac's
   web server, its checksum equal to the Mac's (this is what catches the
   2-byte pad, a ring wrap and a stale buffer); the same `fetch` run
   enough times to wrap the 16-bit counters, with no fault.

8. **The address file.** `netd` reads its configuration from a file on the
   root (D4), absent meaning today's values.
   **Check:** on QEMU, every rig passes with no file; a new boot with a
   file naming another address in SLIRP's range answers at that address
   and not the old one; a malformed file is refused with a line and falls
   back.

9. **The interrupt.** INTID 189 enabled in the GIC, receive done unmasked,
   cleared in the kernel's IRQ path, `NET_WAIT` woken by it.
   **Check:** the kernel counts the interrupts it takes on INTID 189 and
   shows the count; after a hundred pings from the Mac it is at least a
   hundred, and with the unmask left out it stays 0 while the pings are
   still answered by the tick's poll. That second run is why the count is
   the check and a ping's latency is not: a polled driver answers within
   a tick too. `make test-smp`'s concerns apply (the IRQ path runs under
   the kernel lock), and a soak of repeated `fetch`es shows no fault and
   no stuck interrupt (a level-triggered line left uncleared would hang
   the board at the first frame, which is the check that it is cleared).

10. **Two boards.** Two Pi 4s on one switch, each with its address file
    and cluster keys.
    **Check:** `mount -r` of one from the other reads a file whole; a `cpu`
    run returns its output; a board with no key in the other's `authorized`
    is refused. Then the same from the Mac with `scripts/np9p_client.py`,
    the foreign observer the QEMU rigs use.

11. **The records.** `testing-pi4.md` section 2 rewritten (its headline is
    "still no networking"), `gap-analysis.md`'s network rows,
    `make-targets.md` for any new rig, `source-map.md` and `CLAUDE.md` for
    the new files and crate, Pi item 7 on `ROADMAP.md` closed with a
    pointer here, and item 5 (SSH) unblocked.

## Out of scope

- The USB Ethernet adapter (`ROADMAP.md` Pi item 8), and with it a NIC for
  Parallels.
- DHCP (D4), IPv6, and routing beyond one gateway.
- The priority queues, the second interrupt, checksum and segmentation
  offload, jumbo frames, flow control, wake-on-LAN, and energy-efficient
  Ethernet.
- Link-change interrupts (D3).
- SSH (Pi item 5), which this arc unblocks and does not start.
- The Pi 400 and the Compute Module: the same GENET, predicted to need
  nothing more, not tried.
