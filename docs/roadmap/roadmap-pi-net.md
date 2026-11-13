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

**From the same sources read again on 2026-11-13, pinned** (U-Boot at tag
`v2024.10`: `drivers/net/bcmgenet.c`, `drivers/net/phy/phy.c` and
`broadcom.c`; Linux at `v6.6`: `bcmgenet.h`, `bcmgenet.c`, `bcmmii.c` and
`include/uapi/linux/mii.h`). Step 4 was written from these, and what they
added to the list above:

- **The two drivers disagree on the MAC's RGMII delay for `rgmii-rxid`.**
  U-Boot sets `ID_MODE_DIS` in `EXT_RGMII_OOB_CTRL` for `rgmii` and for
  `rgmii-rxid` (`bcmgenet_adjust_link`). Linux sets it for `rgmii` alone
  (`bcmmii.c`, `bcmgenet_mii_config`). Both run this board.
- **U-Boot's bring-up writes to the PHY, and this plan had it writing
  nothing.** Its driver for the board's PHY (`bcm54210e_config`) sets the
  PHY's own clock delays through two Broadcom shadow registers (for
  `rgmii-rxid`: the receive skew on, the transmit delay off), restarts
  autonegotiation, resets the PHY and sets its LEDs. It then reads the
  speed from a Broadcom status register, not the standard ones. Decision
  D7 is what to do about it.
- **The receive producer index is 16 bits; the register's high half counts
  discarded frames** (Linux's `DMA_P_INDEX_MASK` and
  `DMA_P_INDEX_DISCARD_CNT_SHIFT`). U-Boot compares the whole register.
- **U-Boot's wait for a send to complete never ends at the 65536th
  frame**: it compares the consumer index's low 16 bits against a producer
  it did not wrap.
- **Linux drops a received frame** that is longer than its buffer, that
  does not carry both the start and end flags, or that has any of five
  error flags. U-Boot checks none of these.
- **Linux masks and clears every interrupt of both blocks at set-up**
  (`bcmgenet_intr_disable`). U-Boot never touches them.
- **The device pads a short frame itself when it appends the checksum.**
  Linux's transmit path says software padding of runt packets would be
  needed only without `DMA_TX_APPEND_CRC` (`bcmgenet.c` line 2132), and
  neither driver pads. (This line first said the opposite, from memory,
  and the crate had a 60-byte minimum for an hour on the strength of it.)
- **Linux turns on a 64-byte status block in both directions**
  (`RBUF_64B_EN`, and `TBUF_64B_EN` at 0x0600 on v5); U-Boot leaves both
  off, sets only the bit beside the first and never names the second. A
  driver that follows U-Boot after a firmware that followed Linux would
  find 64 bytes it does not expect before each received frame, and would
  have the first 64 bytes of each frame it sends taken for a status
  block.

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
   follows. Step 2 reads whether the firmware has DMA running before
   the exit. That is not the state the kernel inherits (the firmware's
   driver may stop the device in its exit notification, which is how
   edk2's GENET driver is remembered, unread), so this design does not
   wait on the answer: the device is quieted whatever step 2 shows.
5. **One queue, its buffers in a non-cacheable range of their own.** Ring
   16 alone, 2048-byte buffers, no priority queues, no checksum offload,
   the second interrupt unused. The receive and transmit buffers are one
   page-aligned static in `genet.rs`, and `main.rs` passes its range as a
   second `uncached` entry beside `xhci::dma_region()` when the firmware
   says `_CCA 0`, so the driver does no cache maintenance of its own, as
   `xhci.rs` does none; frames are copied in and out at the syscall, as
   virtio-net's are. Without that entry the buffers are cacheable memory
   on a non-coherent device, and the fault would show only on the board:
   step 5 has the check. **Settled in step 4: 256 receive buffers of 2048
   bytes, half a megabyte, as U-Boot.** The ring could be given fewer
   descriptors, and no source read here sets a smaller one up on this
   device, so a smaller ring would be a reading of our own with nothing to
   check it against but the board.
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

- **D7. The PHY's set-up: settled by Hans on 2026-11-13, (b), do what
  U-Boot does.** Step 4b built it. The entry as it was put (new on 2026-11-13, from the sources read for
  step 4; it replaces what D3 assumed, that nothing need be written to the
  PHY). U-Boot writes the PHY's RGMII clock delays and resets it, and
  reads the speed the PHY settled on from a Broadcom status register; the
  `genet` crate as step 4 built it writes nothing to the PHY and reads the
  link from the standard registers, which give the best speed both ends
  advertise and so the wrong one when the PHY has dropped to a lower. What state the PHY is in when the
  kernel gets it is not known: the firmware may have set it up, or not.
  The choices: (a) write nothing, and learn from the board; (b) do what
  U-Boot's `bcm54210e_config` and `bcm54xx_parse_status` do, as a step 4b
  in the crate (an MDIO write, the two shadow registers, the resolved
  speed, all only for a PHY whose identifier is this one's, host-tested
  like the rest), before the board is tried. Recommended: (b). U-Boot is the one working driver for
  this exact board that this plan follows everywhere else, and its MAC
  setting (`ID_MODE_DIS`) is paired with its PHY setting; taking one
  without the other is a combination nobody runs. A wrong delay shows as
  frames with bad checksums or none at all, which on the board is hard to
  tell from five other faults. With (b), `ID_MODE_DIS` stays as U-Boot has
  it; if the board then disagrees, Linux's pairing is the one other thing
  to try. **Does not block step 5's bring-up**, which sends nothing; it
  should be settled before step 6.)

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
   then, only if it found exactly one device, `SYS_REV_CTRL`, and, only
   when that version reads v5, the rest: `SYS_PORT_CTRL`, `UMAC_CMD`,
   `UMAC_MAC0` and `UMAC_MAC1`, `EXT_RGMII_OOB_CTRL`, both DMA control
   registers, the INTRL2_0 mask status, and the id and status of every
   PHY that answers on MDIO, addresses 0 to 31. On any other version the
   log has one register line and no more, by design and not by a fault.
   The MDIO read commands are the scan's only writes, and the bus the
   kernel lends the scan refuses any other
   (`genet::scan_write_allowed`); everything else is a read.
   Without the flag nothing here runs, as with `\SDDUMP`.
   **Built 2026-11-12, not yet run on the board.** The scan is the first
   piece of design 2's crate, in two halves over a `Bus` trait (read,
   write, delay; a trait where the design said a closure, since there are
   three operations): `genet::registers`, which reads the version and
   only on v5 the other eight, handing each value to the caller as it is
   read, and `genet::phys`, the MDIO scan. `kernel/src/netscan.rs` is the
   thin half that lends them MMIO and the firmware's stall and logs. Every
   MDIO address is scanned, not the predicted address 1 alone, so a wrong
   prediction still finds the PHY.
   **Checked without the board:** 21 host tests run the scan against a
   model device that records every read, write and delay: it writes
   `MDIO_CMD` alone and only read commands; on a version other than v5 it
   reads the version register and nothing else, and writes nothing; it
   gives up a start bit that never clears after 20 ms and stops the scan
   there (also when the bus sticks half way through a PHY); and it keeps
   the second reading of the status register. Every rule of the crate was
   taken out in turn, twenty-five mutations over the two passes, and each
   failed a test. `make test-netdump` boots QEMU with the flag: the scan
   says no GENET is declared and no register touched, and the boot
   reaches the shell with a clean trace; with the scan made to read the
   Pi's address regardless, the boot faulted before the exit and the rig
   failed. The model is ours (design 2): none of this says what the device
   answers.
   **The review of the step, before its first commit,** changed what a
   board's log can be trusted for. Each register is logged as it is read,
   the version first, so a later read that faults leaves the earlier
   values on serial (they were all logged at the end). The MDIO half runs
   at `TPL_NOTIFY`: the firmware's network stack may poll the PHY from a
   timer event (predicted), and a command of its own landing between ours
   and our poll would have been read as our answer. A PHY that answers its
   identifier's first half and then stops is counted as partial, where it
   was dropped without a word; addresses that read zero are counted apart
   from PHYs, so a bus held low does not read as thirty-two of them; and a
   declared base that is zero or unaligned is refused with a line
   (`genet::block_usable`). The rig checks that the staged kernel is this
   tree's, as `test-early-fault.py` does, and shares its list of what a
   clean boot must not show. **Left for step 5:** `netscan.rs` is the
   third place that collects the tables, asks `amlscan::declared` and
   requires exactly one device (`main.rs`'s `emmc2_from_acpi` and
   `sdscan.rs` are the others). The driver's discovery would be the
   fourth, so step 5 makes it one helper in `acpi.rs`; this step did not
   reach into the SD card's proven path to do it.
   **The review of #452** found no fault in the offsets or the MDIO
   sequence, and corrected what the scan claims. The enables are read
   before the exit, so they do not say what the kernel inherits (design
   4 now says so, and the log line does). "Read-only" is held by the bus
   the kernel lends, which refuses any write but an MDIO read command,
   where it rested on what the crate happened to write. The time at
   `TPL_NOTIFY` was stated as 20 ms at most and is bounded at about
   2.6 s (a bus whose commands complete slowly is never cut off); the
   doc and the log line say that now. A scan that returned nothing would
   have ended the log without a line. The rig names the two lines a
   machine with no GENET may print, where it listed the ones it may not,
   and takes its identity grades from `test-early-fault.py`
   (`identity_checks`) where it had a copy. It is no longer listed as a
   check for the `genet` crate, none of whose code runs on QEMU. **For
   step 5's helper:** the three copies already differ. `netscan.rs`
   requires that no device was dropped; `emmc2_from_acpi` and
   `sdscan.rs` take the one device whatever the counts beside it. The
   helper has to choose one rule.
   **The check on the board, still owed:** the version's major reads 6;
   the MAC address is the one the board's own firmware setup screen shows
   (or the Mac's ARP table from the Pi's own operating system), and not
   zeros; the PHY id is a Broadcom one; the status register's link bit
   follows the cable, in two boots, cable in and cable out. This step
   settles the predictions under "What is known" and whether the firmware
   has DMA running before the exit, which is not yet what it leaves the
   kernel (design 4). `touch NETDUMP` at the card's boot
   partition root, boot with serial captured, and the lines start
   `Ouroboros kernel: net:` (`testing-pi4.md`, the boot flag table).

3. **The NIC enum.** `nic.rs` with the virtio arm alone, `syscall.rs` and
   `init_net()` moved onto it. No behaviour changes.
   **Built 2026-11-13 (#454).** `kernel/src/nic.rs` holds
   `enum Nic { Virtio }` and answers `mac`, `intid`, `ack_interrupt`,
   `send_frame`, `has_frame` and `poll_frame`; `syscall.rs`'s cell holds a
   `Nic`, and `init_net()` moves the device it brought up into
   `Nic::Virtio`. The module doc states what an arm owes its callers,
   which step 5's GENET arm has to keep: `has_frame` is a read with no
   side effect, and an INTID reported is one that means receive and that
   `ack_interrupt` clears (design 7 is why GENET's is `None` until step 9).
   **Checked:** `make test` and the nine rigs below, each exit 0 on the
   first commit. No mutation was run. `make-targets.md` names no other rig
   for `virtio_net` or `NET_`.
   **The review of #454** found no fault in what runs today and four
   things the GENET arm would have walked into. The first commit made the
   frame bound of `NET_SEND` and `NET_RECV` the installed device's own. An
   arm with a bound below `netd`'s 1600-byte buffers would then have had
   every receive refused, read by `netd` as "no frame", with no line. The
   bound is now a number of the ABI, `syscall_abi::NET_MAX_FRAME` (2036,
   the value it always had), the same for every device; `nic.rs` asserts
   that virtio-net's buffers hold it, and says that a driver relies on it
   (virtio-net's `poll_frame` indexes its buffer by the caller's length,
   which nothing had stated). `on_net_irq`'s doc in `tasks.rs` gave a
   virtio detail as its reason for waking without a second look at the
   device; it now names the promise an arm makes by reporting an INTID.
   Two safety comments in `syscall.rs` still argued from a single core,
   and `nic::Error`'s doc claimed log lines nothing prints. This record
   had said nothing reaches the driver around the enum "since `syscall.rs`
   no longer names `virtio_net`", which does not follow: the reason is
   that `init_net()` moves the one `Device` into the enum and the
   driver's rings are private to its module. No rig sends a frame or a
   buffer over the bound, so that refusal is still a check nothing fails.
   **Found there, not fixed here (it is `netd`'s, and older than this
   step):** `NET_RECV` returns the frame's own length, and `netd` indexes
   its 1600-byte buffer by it (`recv` then `&t.frame[..n]`,
   `programs/servers/netd/src/main.rs`). A frame of 1601 to 2036 bytes
   would panic `netd`. SLIRP sends none; a LAN can. Step 7 owes the clamp
   (a frame longer than the buffer dropped, not cut), before the board
   receives from a network. The ABI's doc for `NET_RECV` says so now.
   **Check:** every network rig on QEMU passes as before (`make
   test-async-rmount`, `test-run-callback`, `test-run-orphan`,
   `test-run-silent`, `test-cred-client`, `test-held-keys`,
   `test-call-interrupt`, `test-no-identity`, `test-server-children`), and
   `make-targets.md` is grepped for `virtio_net` and `NET_` for any other
   rig this owes.

4. **The `genet` crate and its model, on the host.** Begun in step 2
   (the scan, MDIO reads, the version and the MAC address). The rest: the
   register map as
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
   **Built 2026-11-13, on the host (#455).** Two new modules in the crate, and
   nothing in them touches memory: the packet buffers are the caller's,
   and a frame is named by its buffer's index or its address.
   `genet::mac` is the driver: `quiet` (design 4: interrupts masked, the
   MAC's enables and both DMAs off, on v5 only), `Genet::init` (U-Boot's
   probe and start, write for write, ending with the DMAs on and the MAC
   off), `start` (the speed to the MAC and its enables, U-Boot's
   `bcmgenet_adjust_link`), `has_frame` (one register read), `rx_next`
   and `rx_release`, and `tx_send`. `genet::phy` is `link`: the status
   register twice, then what both ends advertise, gigabit first, by the
   standard registers and U-Boot's `genphy_parse_link` rule.
   **Where it departs from U-Boot,** each stated in `mac.rs`'s module doc
   and each with a test: the address is read before the reset and written
   back, and an unusable one ends `init` before any write (design 3, D5);
   every interrupt is masked first; both 64-byte status blocks are
   cleared and the receive DMA's rings 0 to 15 disabled, so a firmware
   that drove the device as Linux does leaves nothing behind (design 3);
   the indexes are 16 bits; a received frame must be whole, unflagged and
   inside its buffer; a buffer's address is computed, never read back
   from the device; a send waits for the consumer to equal the producer
   (U-Boot's comparison never ends at the 65536th frame), for at most
   100 ms; and a send is refused while an earlier frame is still with the
   device, because every send shares one buffer.
   **Checked** (the figures are the first commit's; the review's are
   below): 36 new host tests (34 of the driver, 2 of the citations) against a model device that is typed
   from the sources as numbers, not from the crate's constants: `init`'s
   writes and delays equal U-Boot's sequence, all 821 of them; a device
   left running ends in the state a fresh one does, with no ring set up
   under an enabled DMA; both counters across 65536 and the ring across
   256, in each direction; a frame that fills its buffer taken and one
   byte more dropped; each of the five error flags, and five bits that
   are not errors; a full ring; the send bounds; a send done on the last
   poll and one that is not; a frame the device never takes, across the
   wrap too; the link at each speed and duplex, the latched status, a PHY
   that does not answer and an MDIO bus that sticks (an error after one
   command's 20 ms, not a hang). 66 mutations, one a rule or bound, each
   failed a test. The first pass left one alive, U-Boot's own "consumer at
   or past the producer" in place of equality, and it got the wrap test
   that fails it. `cited_tests.rs` carries 108 constants, each with its
   line in U-Boot `v2024.10` or Linux `v6.6`; a script checked that each
   cited line defines that name (the sources are GPL and are not in this
   tree, so the test checks the values).
   **What none of this says:** the model is ours (design 2). The
   sequence is U-Boot's and the tests hold the code to it; whether the
   device does what the sources say is the board's to answer.
   **Not built, on purpose:** any write to the PHY (D7), and the duplex
   told to the MAC (U-Boot does not either).
   **The review of #455** found no wrong offset, and ten things in what
   the crate promises. Acted on:
   - *Turning a DMA off was not waited for.* `quiet` said "after this the
     device writes to no memory" and only cleared the enable bits, as
     U-Boot does. Linux polls each DMA's status register until it says
     disabled, and gives the MAC and the DMAs time to finish a frame.
     The crate does that now (`DMA_STATUS`, 5 ms of polls a DMA, 2 ms
     after the MAC's enables, 10 ms between the DMAs); `quiet` answers
     whether each stopped, and `init` sets up no ring of a DMA that did
     not (`InitError::DmaStuck`). The model's DMA winds down over a
     number of status reads, so a ring written too early is a violation
     a test sees.
   - *A status of all ones read as a gigabit link.* A bus nobody drives
     has every bit of "up, negotiated, 1000 full" set. `link` answers
     `NoAnswer` for it, as the scan already counted it apart.
   - *A release too many moved the consumer past the device.*
     `rx_release` acts only on a frame `rx_next` returned and that has
     not been released, and says whether it did.
   - *A frame of 1 to 13 bytes, or of up to 2046, could be returned.*
     The crate drops a frame under 14 bytes or over the 1536 it told the
     device.
   - *A second `start` turned the MAC off for one write.* The speed is
     now written with the enables held.
   - *Nothing could ask whether the transmit buffer was free.*
     `tx_idle` is public, and `tx_send`'s doc says to ask it before the
     copy. `Genet::stop` exists, so a device made quiet no longer says it
     is started.
   - Two models of the MDIO bus: the driver's gained the busy polls and
     the unflagged all-ones answer the scan's has. They are still two.
   **After the review:** 43 host tests of the driver and the link where
   there were 34 (`make test`: 419 tests), `init` 822 writes and delays,
   110 cited constants, and 81 mutations, each failed by a test. That
   pass left two alive at first, both on the bound "no longer than its
   buffer", which the new bound of 1536 had made unable to fail; it was
   removed, and there is one bound.
   Not acted on:
   - *The speed read is the best both ends advertise, not the one the PHY
     settled on.* A Broadcom PHY on a poor cable drops to 100 and goes on
     advertising 1000; the MAC would be set to 1000 and nothing would
     pass. U-Boot reads the resolved speed from a Broadcom register for
     this reason. It is one read, and it is Broadcom-specific, so it is
     part of D7 and of step 4b. `phy.rs` says so.
   - *`tx_send` waits for the device to take the frame, under the kernel
     lock.* Kept: it is the bound virtio-net's send has, and on the board
     a send that reports its own failure is worth more than the time.
     Returning at once and letting the next send wait is the change to
     make if step 6 measures the wait to matter.
   **For step 5, found here:**
   - A received frame is 14 to 1536 bytes (the review made the crate hold
     that itself), inside the ABI's 2036 and inside `netd`'s 1600-byte
     buffers, so step 7's clamp in `netd` is owed for virtio-net's sake
     and not for this device's.
   - A send takes 14 to 1532 bytes. `netd`'s largest frame is 1514.
   - The kernel's one transmit buffer may be written only when
     `Genet::tx_idle` says so. After a send that timed out, the frame is
     still the device's.
   - `quiet` answers whether each DMA said it stopped. Only when both did
     may the kernel take memory the firmware owned; the line it logs
     otherwise is step 5's to word.
   - The receive area is one static of 524,288 bytes, in the kernel's
     image on every machine, the ones with no GENET too. Its range is the
     second `uncached` entry of design 5.
   - `tx_send` waits up to 100 ms under the kernel lock, the bound
     virtio-net's send already has.

4b. **The Pi's PHY, as U-Boot sets it up** (decision D7).
   **Built 2026-11-13, on the host (#456).** In `genet::phy`, for a PHY whose
   identifier is U-Boot's BCM54210E (`0x600d84a0` under the mask
   `0xfffffff0`) and for no other:
   - `setup`: U-Boot's `bcm54xx_config_clock_delay` for `rgmii-rxid` (the
     receive clock skew on, in the auxiliary control register's
     miscellaneous shadow; the transmit clock delay off, in shadow 3 of
     register 0x1c), then its `phy_reset`, the reset bit polled for at
     most half a second of waits.
   - `link_resolved`: the status register twice, as `link`, then the
     speed and duplex the PHY settled on, from its auxiliary status
     register (U-Boot's `bcm54xx_parse_status`). For any other PHY it is
     `link`.
   - `genet::mdio_write`, U-Boot's `bcmgenet_mdio_write`, which the
     set-up needed.
   **The order is U-Boot's, delays first and then the reset, on
   purpose.** Linux resets first and sets the delays after. Whether the
   reset keeps the delay settings is not known here. In U-Boot's order
   the PHY ends as it does under U-Boot whichever is true, and that is
   the one state known to work with the MAC's `ID_MODE_DIS`, which
   step 4 took from the same driver.
   **Left out of U-Boot's set-up:** `genphy_config_aneg`, which it runs
   before the reset and the reset undoes by clause 22 (control and
   advertisement registers back to their defaults, autonegotiation on),
   and the LED register.
   **What it means for step 5:** the order there is `Genet::init`, the
   PHY found by `phys` (its address and identifier), `phy::setup`, then
   `phy::link_resolved` and, on a link, `Genet::start`. `setup`'s
   answer, the two delay shadows as the PHY holds them after its reset,
   goes in the boot log. The reset drops
   the link: it takes seconds to come back, so the first read after
   `setup` says down or negotiating, and D3's "again whenever a send
   finds the link down" is how the link is then found. A PHY that never
   leaves reset costs about 0.5 s; the bound, on a bus whose every read
   takes its whole 20 ms, is about 10.5 s.
   **Checked** (the first commit's figures; the review's are below): 11
   new host tests. The model's PHY has the two Broadcom
   windows as U-Boot's `broadcom.c` uses them, and a reset bit that stays
   set for a number of reads. The set-up's MDIO commands equal U-Boot's,
   one for one (eight on a PHY whose reset clears at once); the skew is
   set and the delay cleared whatever was in the shadows, other bits
   kept; another PHY's identifier writes nothing; the reset clear on the
   500th read is taken and one later is `ResetStuck`; a bus that sticks
   ends the set-up at its first command; the resolved link by each value
   of the mode field, with the advertisements saying otherwise. 28
   mutations of the new code, each failed by a test. One was alive
   after the first pass (the shadow's read-back not cut to its ten bits
   of data, which changes nothing while the PHY echoes the selector it
   was given) and got a test with a PHY that does not. 126 constants are
   cited now, the Broadcom ones to `broadcom.c`'s lines.
   **The review of #456** fetched the same sources itself and found the
   write sequence, the mode table, the identifier and every citation to
   match. What it found around them, and what was done:
   - *A PHY silent while it resets failed the set-up as "no PHY".* The
     reset poll took the controller's "no answer" flag as an error.
     U-Boot's MDIO read never looks at that flag, so it polls on. The
     poll here now does too, for a flagged read and for all ones; only a
     bus that sticks ends it early.
   - *`setup` said it had worked without looking.* This plan says the
     reset may or may not keep the delays, and the PHY starts where the
     UEFI firmware's driver left it, not where U-Boot finds it. `setup`
     now reads both shadows back after the reset and returns them
     (`Delays`, with `as_written`). **Step 5 logs them, and the first
     board run answers the question.** The model's reset can keep the
     shadows or wipe them, and the tests run both.
   - *An auxiliary status of all ones read as 1000 full.* It is
     `NoAnswer` now, as the status register's already was.
   - *The read that can be wrong on this PHY was still public.* `link`
     is private to the crate; `link_resolved` is the one way in.
   - A test asserted the shadows after the model's reset, which wrote
     down as fact what is not known. It asserts the writes before the
     reset and the returned state after it.
   - Stale lines: `mac.rs` still called the PHY's set-up an open
     question, the crate's module doc named neither the new functions nor
     `broadcom.c`, and `docs/ROADMAP.md` item 7 still said only steps 1
     and 2 were built. The PHY is named by what it is taken for: the
     board's is predicted a BCM54213PE, and U-Boot's BCM54210E driver is
     the one that matches its identifier.
   - The MDIO command word is packed in one place, and the link's status
     is an enum where it was an error type carrying an answer.
   After the review: 14 host tests of step 4b where there were 11, the
   set-up eleven MDIO commands on a PHY whose reset clears at once
   (the four of the read-back added), and 37 mutations of step 4b's
   code, each failed by a test, in a pass of 118 over the whole driver.
   Departures from U-Boot the review asked to be listed, now in
   `phy.rs`'s module doc: a reset that never ends is an error here
   (U-Boot throws `phy_reset`'s result away), and the read-back.
   **Still only the board's to say:** whether these are the right delays
   (a wrong one is frames with bad checksums, or none), whether the reset
   keeps them (`setup`'s answer), and whether the PHY is this one
   (step 2's scan reads its identifier).

5. **The board: bring-up and link.** One helper in `acpi.rs` for "the one
   device of this `_HID` declared present", used by this driver, the scan
   and the SD card's two callers (step 2's note). The kernel's `genet.rs`,
   the device
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
