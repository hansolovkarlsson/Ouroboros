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

Each line says where it was read. The board's registers were first read on
2026-11-13, by step 2's scan and step 5's bring-up in the same three boots;
what they said is the block "From the board's registers" below, and it
outranks every other line here where the two disagree.

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
**What the board said of these on 2026-11-13:** the PHY's address and the
MAC address were right; "idle unless it network-booted" was **wrong**; the
two interrupts' roles were then unread, the driver being polled. Step 9
read the first on 2026-11-16: INTID 189 carries the default queue's
receive done, and the driver takes it (the second, 190, is still unread).
The block below has the values.

**From the board's registers** (a Pi 4, three boots on 2026-11-13 of build
`e9cd5de57bfc`, debug, from an SD card with `\NETDUMP`: two with the cable
in, then one with it out; serial in `screenlog.0`, which is not in the
tree, so the values are copied here):

- **One GENET is declared present**, at `0xfd580000`, `0x10000` bytes,
  INTID 189 (unreadable 0, absent 0, status unknown 0, dropped 0).
- **It is v5.** `SYS_REV_CTRL` reads `0x06000000`: major 6, minor 0.
- **The firmware leaves the board's MAC address in the registers.**
  `UMAC_MAC0` `0xd83add85`, `UMAC_MAC1` `0x00002422`:
  `d8:3a:dd:85:24:22`, the same in all three boots, and the address
  the firmware's setup screen shows (compared by Hans, 2026-11-13).
- **The firmware leaves the device running, on a boot from the card.**
  Before the exit `UMAC_CMD` reads `0x0000001b` with the cable in and
  `0x00000013` with it out (transmit and receive enabled in both), and
  both DMA control registers read `0x00020001` (on) in all three boots. So
  the prediction that its driver is idle unless it network-booted was
  wrong, and the code that stops the device (`halt`) runs against a live
  device on every boot.
- **The other registers before the exit:** `SYS_PORT_CTRL` `0x00000003`;
  `EXT_RGMII_OOB_CTRL` `0x00f00050` with the cable in and `0x00f00000`
  with it out (so the firmware has RGMII mode and the link bit set when
  there is a link, and `ID_MODE_DIS`, bit 16, clear either way); the
  INTRL2_0 mask status `0x07fedfff`.
- **One PHY, at MDIO address 1, id `0x600d84a2`.** The other 31 addresses
  were flagged no answer; none read all ones or zero, none answered in
  part. The driver takes this id for the one U-Boot's BCM54210E set-up
  matches. Nothing on the board names the part.
- **The PHY's link follows the cable.** Status `0x796d`, link up,
  autonegotiation done, in both cable-in boots; `0x7949`, link down,
  autonegotiation not done, with the cable out.
- **After the driver's set-up and reset the PHY's delays read: receive
  clock skew on, transmit clock delay on** (shadows `0x71e7` and
  `0x0200`), the same in all three boots. The set-up wrote the transmit
  clock delay off, so the line ends `NOT as written`. The read-back alone
  did not say whether the reset put the delay back or the write never
  took; the next bullet does.
- **The firmware leaves the PHY with its receive clock skew on and its
  transmit clock delay off** (shadows `0x73e7`, `0x0000`), read before
  any write of ours, on a fourth boot the same day (build
  `fff04bf2e4a9`, step 6's). With its MAC's `ID_MODE_DIS` clear, that is
  Linux's pairing. **And the reset is what turns the delay on:** it was
  off when found, the set-up wrote it off, and after the reset it read on
  (`0x71e7`, `0x0200`, as in the three boots before).
  **What this means for a frame was worked out the same day** (step 4b,
  "The transmit clock delay, worked out"): it is the state U-Boot's
  pairing needs, and step 6 goes ahead on it.
- **The link the driver reads:** up, 1000 Mb/s, full duplex, with the
  cable in; down after the whole 5000 ms wait with it out.
- **The receive area walks as non-cacheable** in the running tables, 128
  of 128 pages, at `0x378f4000`.

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

**D1 to D6: settled by Hans on 2026-11-14, each as recommended.** D1, D2,
D3, D5 and D6 were already built on their recommendations (steps 1 to 6),
so for those the answer confirms what ran on the board. D4 is the one that
still decides work: step 8 is the file, and DHCP is a later item of its
own. The entries below stand as they were put.

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
   **Built 2026-11-12; run on the board 2026-11-13, and it passed** (the
   result is at the end of this step). The scan is the first
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
   **The check on the board:** the version's major reads 6;
   the MAC address is the one the board's own firmware setup screen shows
   (or the Mac's ARP table from the Pi's own operating system), and not
   zeros; the PHY id is a Broadcom one; the status register's link bit
   follows the cable, in two boots, cable in and cable out. This step
   settles the predictions under "What is known" and whether the firmware
   has DMA running before the exit, which is not yet what it leaves the
   kernel (design 4). `touch NETDUMP` at the card's boot
   partition root, boot with serial captured, and the lines start
   `Ouroboros kernel: net:` (`testing-pi4.md`, the boot flag table).
   **Run 2026-11-13, by Hans: passed.** Three boots of build
   `e9cd5de57bfc`, two with the cable in and one with it out. Version
   major 6; MAC address `d8:3a:dd:85:24:22`, not zeros; one PHY, at
   address 1 as predicted, id `0x600d84a2`; the link bit up with the cable
   (status `0x796d`, twice) and down without it (`0x7949`). Every value is
   under "What is known", "From the board's registers". The MAC address
   is the one the firmware's setup screen shows, compared by Hans the
   same day, so the check is whole. **The firmware has both DMAs and the MAC's
   transmitter and receiver on before the exit**, on a boot from the card.
   The card's kernel was `main`, which holds step 5 too, so these boots
   were also step 5's run; the scan's lines come before the exit and
   step 5's code runs after it.

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
   step):** `NET_RECV` returned the frame's own length, and `netd` indexed
   its 1600-byte buffer by it (`recv` then `&t.frame[..n]`,
   `programs/servers/netd/src/main.rs`). A frame of 1601 to 2036 bytes
   would panic `netd`. SLIRP sends none; a LAN can. Step 7 owes the clamp
   (a frame longer than the buffer dropped, not cut), before the board
   receives from a network. The ABI's doc for `NET_RECV` said so then.
   Both are done: the clamp in step 7, and since 2026-11-18 the kernel
   answers `NET_FRAME_CUT` for such a frame and never a length past the
   buffer (step 7's left-open items).
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
   **The board's answers, 2026-11-13.** The PHY's id is `0x600d84a2`,
   which `setup` takes as this one. After the reset the receive clock skew
   reads on and the transmit clock delay reads **on**, where `setup` wrote
   it off (shadows `0x71e7`, `0x0200`; `NOT as written`, in three boots of
   three). So what U-Boot's order leaves on this PHY is not what its
   delay function writes, at least for the transmit side. Whether these
   are the right delays is still open, and only a frame on the wire
   answers it (step 6's `tcpdump`).

   **The transmit clock delay, worked out (2026-11-13, from the two
   fetched sources and the reading above; no frame has been sent).**
   RGMII wants the transmit clock delayed about 2 ns once, by the MAC or
   by the PHY. The two drivers that run this board split it differently
   for `rgmii-rxid`:
   - *Linux* (`bcmmii.c`, `bcmgenet_mii_config`, and its
     `bcm54xx_config_clock_delay`): the MAC delays (`ID_MODE_DIS` clear),
     the PHY's transmit delay is written off, and the PHY is reset
     *before* the write, so the write stands.
   - *U-Boot* (`bcmgenet.c`, `bcmgenet_adjust_link`, and `broadcom.c`,
     `bcm54210e_config`): the MAC does not delay (`ID_MODE_DIS` set), the
     PHY's transmit delay is written off, and the PHY is reset *after*
     the write.
   Read as written, U-Boot's pairing has no delay anywhere, and U-Boot
   works on this board. The board's reading is what squares it: in
   U-Boot's order the PHY ends with its transmit delay **on**. So what
   U-Boot runs is the PHY's delay on and the MAC's off, one delay, and
   this driver ends in the same state: the PHY as the board printed it,
   and `Genet::start` setting `ID_MODE_DIS` as U-Boot does.
   **So `NOT as written` is the state to expect on this board**, and the
   line says `as U-Boot leaves it` for it since step 6. **It is not to be
   repaired:** the delay written off again after the reset, with
   `ID_MODE_DIS` still set, is no delay at all, the one pairing that is
   surely wrong. **The other pairing, if step 6's frames are bad, is
   Linux's whole:** reset first, the delay off after it, `ID_MODE_DIS`
   clear (which is also how the firmware had the MAC before the exit,
   `EXT_RGMII_OOB_CTRL` `0x00f00050`).
   **Not known from this, when it was written:** whether the reset put
   the delay back or the write never took, and what the firmware left in
   the PHY. `setup` reads both shadows before it writes them, and step 6
   logs those values, at no cost in MDIO commands.
   **Both answered by step 6's first boot (2026-11-13, build
   `fff04bf2e4a9`).** The line `found with its receive clock skew on and
   its transmit clock delay off (shadows 0x73e7, 0x0000)` is what the
   firmware left: with its MAC delaying (`ID_MODE_DIS` clear), Linux's
   pairing. The delay was off when found, was written off, and read on
   after the reset, so **the reset restores it**; the write took or did
   not, and it makes no difference. That makes "U-Boot ends where this
   driver does" sure whatever U-Boot's boot chain leaves in the PHY: its
   reset comes last too. So both pairings are seen on this board, the
   firmware in Linux's and this driver in U-Boot's. Which of them puts a
   good frame on the wire is still step 6's to show; the firmware's
   network boot working would speak for Linux's, and U-Boot's for the
   other, and neither has been watched here.

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

   **Built 2026-11-13 (#457); run on the board the same day, as #460 left
   it** (the result is at the end of this step).
   `kernel/src/genetnic.rs` is the kernel's half (named so because a
   module called `genet` would clash with the crate). Before the exit,
   `discover` takes the one GENET the tables declare, through the helper
   below, and touches no register. After the identity map is installed,
   where `init_net` runs and the kernel's own fault handler is live,
   `quiet` asks the MMU's walk whether the block is device memory at both
   ends and makes the device quiet (`genet::mac::quiet`, design 4), for
   every declared GENET, used or not; `init`, which takes `quiet`'s
   answer, runs the crate's `Genet::init`, scans MDIO for exactly one
   PHY, runs `phy::setup` and reads the link with `phy::link_resolved`. The device
   goes in `syscall.rs`'s cell as `Nic::Genet`, which reports no
   interrupt (design 7).
   **The MAC is left off, which is this step's one variable.**
   `Genet::start` is not called: a send is refused, no frame arrives,
   and `has_frame` says no without asking the device. Steps 6 and 7 turn
   it on, and write the two paths that copy a frame, which are not here.
   **"Quiet at the exit" is done after the map is installed, not at the
   exit itself.** At the exit on the Pi the kernel is at EL2 on the
   firmware's tables with no fault handler of its own, so a miss there
   would be a silent hang. Nothing between the exit and `init` uses
   memory the firmware owned: the kernel runs on its own image, its own
   statics and what the loader allocated before the exit.
   **The helper:** `acpi::declared(rsdp, hid)` collects the tables and
   asks `amlscan`, and `amlscan::Transports::one` is the rule for
   "exactly one": one kept and none dropped. The SD card's discovery,
   `sdscan.rs`, `netscan.rs`, virtio-mmio's transports and the driver all
   go through them. **The SD card's rule changed by this:** its own copy
   took one device kept whatever had been dropped beside it, and `one`
   does not. On a board that declares one EMMC2 the answer is the same;
   `make test-sd` passed, on QEMU's PCI path, which is not the Pi's.
   **The receive buffers** are one page-aligned static of 524,288 bytes,
   `genetnic::RX_AREA`, in `.bss` (the image file did not grow), passed
   to `mmu.rs` as a second non-cacheable range beside the xHCI's pool
   whenever the firmware declares DMA non-coherent, device or none.
   **The link's wait:** the PHY's reset drops the link, so `init` asks
   for it every 100 ms until it is up, for at most 5 s (D3's bounded
   wait; `LINK_WAIT_MS` in `genetnic.rs`). **A board with no cable waits
   the whole 5 s on every boot.** The first version skipped the wait when
   the link read down before the reset; the review pointed out that a
   link still coming up reads the same, so the skip was a guess and is
   gone.
   **Checked on QEMU, which has no GENET:**
   - `make test-netdump`, extended: on both boots the driver prints one
     line, "receive area ..., 0x80000 bytes: walks as cacheable memory
     (0 of 128 pages non-cacheable)", read from the MMU's own walk, and
     no other `genet:` line. That is the control the check above asks
     for.
   - Control, the non-cacheable mapping forced on QEMU: the same line
     said "non-cacheable (128 of 128 pages)", `mmu.rs`'s self-check
     logged the area as Normal Non-cacheable on every page in all 11
     views with its neighbours not, the boot reached the shell, and the
     rig failed, as it must.
   - Control, the Pi's address taken with nothing declared: the kernel
     faulted at `0xfd580000` under its own handler, and the rig failed.
   - Each exit 0 on the committed code, and again on the review's:
     `make test` (434 tests), `test-netdump`, `test-early-fault`,
     `test-el1-drop`, `test-sd`, `test-smp`, `test-usb-hub`,
     `test-no-identity`, `test-run-callback`. The two controls above were
     run on the first commit only.
   **Not checked, and not checkable here:** `init` past its first line.
   No machine this kernel boots on has the device, so the quiet, the
   reset, the MDIO scan, the PHY's set-up and the link's read have run
   only as the crate's host tests against our own model.
   **The board run, owed after step 2's:** a card from this tree, serial
   captured, two boots, cable in and cable out. The lines start
   `Ouroboros kernel: genet:`. A pass, cable in:
   - `ACPI declares GENET @ 0xfd580000` (before the exit);
   - `receive area ...: walks as non-cacheable memory (128 of 128 ...)`;
   - `GENET v5 (version major 6 ...) @ 0xfd580000, MAC ...` with the
     address step 2 read;
   - `PHY at MDIO address N, id 0x600d84a_: set up and reset; ...`, whose
     last words say whether the reset kept the delays. **Either answer
     is a finding and not a failure**: it is the question D7 left open;
   - `link up, 1000 Mb/s, full duplex` (or what the switch gives);
   - `the MAC's transmitter and receiver are left off`, then
     `NIC installed for the network server`, and a login.
   Cable out: `link down (waited 5000 ms)`, and a login 5 s later than
   before. Then `ping` fails for want of an answer and not for want of a
   NIC. A fault line names the register: `far_el1` less `0xfd580000` is
   its offset.

   **The review of #457** found no fault in what QEMU can reach, and
   these in what the first board run could:
   - *A frame nothing could take.* `has_frame` asked the device while
     `poll_frame` returned nothing. Had the producer index ever differed
     from the consumer, `netd` would have been woken on every tick for
     good. `has_frame` says no while the MAC is not started.
   - *The wait skipped on a guess* (above).
   - *Quiet only when installed.* With another NIC in the cell, a GENET
     the firmware left receiving would have gone on writing memory.
     `quiet` is its own function now and runs for every declared GENET;
     `init` takes its answer, so the device cannot be set up without
     having been stopped. A block that does not walk as device memory is
     still not touched, and the line says the device is left as the
     firmware left it.
   - *Cacheable buffers were a log line.* Where the firmware says DMA is
     not coherent and the receive area does not walk non-cacheable on
     every page, `init` stops with a warning.
   - *Mapped by coincidence.* `mmu::MAX_EXTRA_DEVICES` was 5, and the Pi
     fills five (console, framebuffer, xHCI, two for the GIC), so the SD
     controller and the Ethernet were mapped only because the console's
     1GB block holds them, the coincidence `mmu.rs` records fixing for
     the console itself on 2026-09-28. It is 7, one 4 KB table each, and
     each has its slot.
   - *The copies of "exactly one" did differ* (above): this record had
     said they could not.
   - `virtio_mmio_transports` was `acpi::declared` written out again; it
     calls it. Comments in `main.rs`, `mmu.rs`, `syscall.rs` and
     `acpi.rs` that this step had made untrue are corrected. The link's
     line prints the wait from the constant.
   Not acted on: each `_HID` asked for walks the AML tables again
   (virtio, EMMC2, GENET), milliseconds at boot.
   **Two of these reach past networking on the Pi:** the device list of
   seven, and the SD card's rule. Each should leave a working board as it
   was (the same 1GB block, one EMMC2 declared), and each is on the boot
   path of a machine this was not run on. **The first board run of this
   tree is therefore also a check of the SD card's boot**, before any
   `genet:` line is read.

   **A review of the reviews' fixes** (2026-11-13, after #457 merged).
   Each pull request of steps 4 to 5 was reviewed; what was changed in
   answer to each review was not, and a repair is a change. A fifth
   review read those four commits alone. It found the three changes that
   reach the Pi's boot beyond networking sound (the list of seven builds
   the same tables on the Pi, the SD card's rule differs only past 64
   candidates, the split writes the same registers), and ten things in
   the driver. Fixed:
   - *The device was stopped in an order that can leave it unable to say
     it stopped.* `quiet` turned the MAC's transmitter off and then the
     transmit DMA. Linux's `bcmgenet_netif_stop` turns off the receiver,
     tears down the DMAs, and only then the transmitter, with the comment
     "TX DMA disabled must be done before this" (read in the fetched
     source, `bcmgenet.c`): a transmit DMA with a frame still queued
     drains it through the MAC. Had the firmware left one queued, the
     DMA's status might never have read disabled, and the kernel would
     have said the device "did not say it had" stopped and left the
     Ethernet down. One helper, `halt`, now holds Linux's order, and
     `quiet`, `Genet::init` and `Genet::stop` all stop through it. `init`
     stops the device itself before its reset, where it reset first and
     asked the DMAs afterwards, and so does not rest on a caller having
     run `quiet`.
   - *The read-back of the PHY's delays took all ones as their value*,
     which reads as both on and "NOT as written". The board's log is read
     for exactly that line. All ones is no answer now, for both shadows.
   - *The link's wait asked a stuck MDIO bus again and again*, each time
     writing a command over one still busy. A bus stuck busy ends the
     wait at once; a PHY that only does not answer is still asked again.
   - *A stop and then a start left the device dead and saying it was
     started.* `Genet::stop` takes the `Genet`: the way back is `init`.
   - *"No frame while the MAC is off" was the kernel wrapper's rule
     alone.* It is `Genet::has_frame`'s, so `rx_next` has it too.
   - The safety comment on the kernel's register access named the
     function that no longer makes the check; the receive area's pages
     were counted in two places with two spellings; the two guards on the
     device list could no longer fail, and every region now goes through
     one push that names a region it has to leave out; `Genet::stop` was
     a copy of `quiet`; `amlscan::virtio_mmio` had no caller but its own
     tests, which now call what the kernel calls.
   **What the model says about the stop order is ours.** A transmit DMA
   that will not stop once the transmitter is off with a frame queued is
   our reading of Linux's comment, put in the model so a test can tell
   the two orders apart. No device has shown it.
   **Checked:** `make test` 437 tests; 127 mutations of the driver, each
   failed by a test (one was alive after the pass: the first refusal of a
   DMA that will not stop, which a later one covered, and which now has a
   test that the MAC is not reset under it); each exit 0 on the fixes:
   `test-netdump`, `test-early-fault`, `test-el1-drop`, `test-sd`,
   `test-smp`, `test-usb-hub`, `test-no-identity`, `test-run-callback`.
   Bring-up took about 14 ms more by this: the device stopped once by
   `quiet` and again, already stopped, at the head of `init`. The next
   round removed the cost.

   **And a review of that** (#459, read after it merged). Ten more, two
   of which bore on the board run and both of which the round before had
   put there:
   - *A transmit DMA that was only slow was called stuck.* `halt` took
     its answer after 5 ms of polls, before the 10 ms wait it is given to
     drain, never asked again, and then turned the transmitter off, which
     by the helper's own account can keep it from ever stopping. It is
     asked again after the wait.
   - *The all-ones guard on the read-back had made a look into a
     failure.* One unanswered read after the PHY's reset returned the
     set-up as failed, and the boot never read the link, of a PHY that
     had been set up. `setup` returns a `ReadBack` now: the delays, or
     that they did not read, and the caller goes on to the link.
   - **Which DMA is fatal changed.** Until here either DMA not saying it
     stopped ended the bring-up. Linux warns of either and goes on. Now
     the **receive** DMA ends it, at the stop and before any reset,
     because it is the one that writes memory; the **transmit** DMA,
     which only reads, is noted, asked once more after the MAC's reset
     and the flush, and ends the set-up only then. That second question
     was, before this, a guard no test could fail: nothing in the model
     could make a DMA stopped at the first say otherwise at the second.
   - `halt` leaves a MAC held in software reset alone, as Linux's
     `umac_enable_set` does, and stopping a device that is already
     stopped waits for nothing, so `init` stopping it again after
     `quiet` costs no time (the 14 ms above is gone).
   - The link's wait ended for good on one busy read and said "not
     waited for" after waiting; it takes three running, and says that.
     A device region left out of the list was said on a console that a
     framebuffer-only machine does not have yet; it is said again once
     every console is up. "All ones is a bus nobody drives" was written
     out in five places and is one function, `mdio_read_driven`. The
     source map still described `amlscan::virtio_mmio`, removed in the
     pull request under review.
   **The model has two rules of ours for the transmit DMA now**, that it
   cannot stop with a frame queued once the transmitter is off, and that
   a flush releases one that has not stopped. Both exist so a test can
   tell two orders, or two policies, apart. Neither is a device's.
   **Checked:** `make test` 440 tests; 137 mutations, each failed by a
   test (one alive after the pass, "either DMA on earns the drain's
   wait" weakened to "both", got a test; one did not compile and was
   rewritten); each exit 0: `test-netdump`, `test-early-fault`,
   `test-el1-drop`, `test-sd`, `test-smp`, `test-usb-hub`,
   `test-no-identity`, `test-run-callback`.
   **On the board, new lines to know:** `the transmit DMA was told to
   stop and has not yet said it had` is a note and not a refusal; `its
   delays did not read back` leaves the PHY's question unanswered for
   that boot, and the link is still read.

   **Run on the board 2026-11-13, by Hans: the check passed where it
   could be read.** The same three boots as step 2's (build
   `e9cd5de57bfc`, which is `main` after #460). In each, after the exit:
   - `genet: receive area 0x378f4000, 0x80000 bytes: walks as
     non-cacheable memory (128 of 128 pages non-cacheable)`, the line
     whose control on QEMU says cacheable.
   - `genet: GENET v5 (version major 6 minor 0) @ 0xfd580000, MAC
     d8:3a:dd:85:24:22, set up on ring 16, polled`: step 2's values.
   - The PHY's line, ending `NOT as written` (step 4b has the reading).
   - `link up, 1000 Mb/s, full duplex` in both cable-in boots; `link down
     (waited 5000 ms)` with the cable out, and that boot went on to a
     login.
   - `NIC installed for the network server`, and `netd` no longer prints
     `no NIC this boot`. With the cable in, `ping 192.168.0.1` said
     `unreachable (no ARP reply)`: for want of an answer, not for want of
     a NIC, as the check asks. (That ping could not have been answered
     with the MAC on either: `netd`'s address is still in SLIRP's
     range, `10.0.2.34` on this board, step 8.)
   - No `EXCEPTION` line in any of the three boots, each run to a login
     and a `shutdown`. None was left running long, so "a sustained boot
     shows no fault" is not yet shown. **Shown 2026-11-15,** for one
     boot of about 14 minutes, most of it idle (the run of #465 on the
     board, under step 6).
   **The stop ran against a live device and raised no line.** The firmware
   had both DMAs and the MAC on (step 2), and none of the boots printed
   the transmit DMA's note or a refusal. That is read from the absence of
   a line: the driver prints nothing when a DMA says it stopped, so the
   log does not show the status it read or how long the stop took.
   **Since 2026-11-14 `quiet` says both:** `genet: made quiet in N us:
   receive DMA stopped, transmit DMA stopped; status now: receive
   0x..., transmit 0x...`, on every boot that stops the device, and the
   refusal for a receive DMA that did not stop has its time and the two
   status registers too. The transmit DMA's note above is now part of
   that line (`transmit DMA told to stop and has not yet said it had`)
   and not a line of its own; it no longer says the DMA is asked again,
   since that is `Genet::init`'s to do and not every boot reaches it.
   The status is read by `quiet` after the crate's polls, so it is the
   state then and not the reading the crate judged by; that one would
   need the crate to return it. **Seen on the board 2026-11-15**
   (build `363b87ca05e0`): `genet: made quiet in 13 us: receive DMA
   stopped, transmit DMA stopped; status now: receive 0x0003ffff,
   transmit 0x0003ffff`. Bit 0, `DMA_DISABLED`, is set in both.
   **The SD path changed by #457 works on the board:** `sd: ACPI declares
   EMMC2 @ 0xfe340000`, the card brought up as the boot disk, its ext2
   root mounted, in all three boots. `acpi::declared` and
   `Transports::one` gave the SD card's discovery the answer its own
   copy did.
   **Left open by the run:** `LINK_WAIT_MS`, whose 5 s the cable-out
   boot paid in full. The transmit clock delay was worked out the same
   day (step 4b). **Kept at 5 s by Hans on 2026-11-14, for now:** only a
   boot with no cable pays it, and a shorter wait leans on the send-time
   read of the link (step 6), which no run on the board has yet shown
   with a cable plugged in late. To be looked at again once one has.
   (Since 2026-11-17 a late cable is found by `netd`'s wait within a
   second, shown on the board with a control (step 7's left-open item),
   so a shorter wait no longer leans on a send. The value is still 5 s:
   Hans's to look at again.)

6. **The board: transmit.** `netd` sends what it sends now. With the
   address file in place (step 8 may come first if that is simpler), a
   `ping` of the Mac makes an ARP request.
   **Check:** `tcpdump` on the Mac, the foreign observer, shows the ARP
   request with the board's MAC address as its source and a whole frame
   (the Mac's interface counts no CRC errors). A check that can fail: with
   the transmit enable left out, `tcpdump` shows nothing.

   **Built 2026-11-13, not yet run on the board.** The transmitter
   alone: this step turns on half of what U-Boot's start turns on.
   - *In the crate,* `Genet::start` takes a third argument,
     `Receiver::Off` or `Receiver::On`, with no default, so a receiver is
     never on because nobody said otherwise. With `Off` the writes are
     U-Boot's but the last, which sets the transmit enable alone;
     `has_frame` says no until a start with `On`, without asking the
     device. **Why the receiver waits for step 7:** once it is on the
     receive DMA writes frames to the addresses the descriptors hold, and
     that the device takes a CPU address as its own is still a prediction
     ("What is known"). The transmit DMA only reads: a wrong address
     there is a wrong frame on the wire, which `tcpdump` shows, and no
     memory written.
   - *In the kernel,* `genetnic.rs` starts the MAC when the boot finds a
     link, and `send_frame` sends: the length checked, `tx_idle` asked
     before the copy, the frame copied into `TX_AREA` (one page of its
     own, mapped non-cacheable by the rule the receive area is, refused
     at `init` where it does not walk so), a `dsb sy`, then the crate's
     `tx_send`. The barrier is the one `xhci.rs` puts before a doorbell:
     the copy is a Normal store that nothing else orders against the
     register write that tells the device to read it. A boot that found
     no link starts nothing, and the first send reads the link once, with
     no wait, and starts the MAC if it is up (D3, as recommended).
   - *The PHY's line is two now.* `phy::setup` returns the two shadows as
     it found them, before any write (the reads it already made, so no
     MDIO command more), and the kernel prints them: what the firmware
     left. The line after the reset ends `as U-Boot leaves it` for the
     state the board read, and says which clock has no delay otherwise.
   **Checked without the board:** `make test` 443 tests (3 new, of the
   start with the receiver off, and the found values asserted in two
   more); 11 mutations of the new lines in the crate, each failed by a
   test; `make test-netdump` exit 0, with the transmit buffer's line
   added to the two a machine with no GENET may print, cacheable on QEMU.
   **Not checked by anything here:** `send_frame`. No QEMU machine has
   the device, so the kernel's half of a send has run nowhere. It is
   about 60 lines, and the board is its first run.
   **On the board.** A card from this tree, no flag needed; the cable in
   before power. A direct cable from the Pi to the Mac's Ethernet is the
   best observer: a switch drops a frame with a bad checksum, so through
   one "nothing arrived" does not tell a bad frame from none.
   1. On the Mac, with its interface named (`en0` here):
      `sudo tcpdump -i en0 -e -n -xx ether src d8:3a:dd:85:24:22`.
   2. Boot, log in, `ping 192.168.0.1` (any address: `netd` still has
      SLIRP's, so it asks for its gateway, `10.0.2.2`).
   3. **A pass:** `tcpdump` shows an ARP request, broadcast, from
      `d8:3a:dd:85:24:22`, asking who has `10.0.2.2`, told by
      `10.0.2.34` (`netd` takes its last octet from the MAC's, `0x22`),
      60 bytes (the device pads the 42 `netd` sends); the
      serial log has `genet: the MAC's transmitter is on at 1000 Mb/s`
      and `genet: first frame sent: 42 bytes, taken by the device`; the
      Mac's `netstat -I en0` counts no new input errors. `ping` still
      says `no ARP reply`: nothing is received yet.
   4. **What a failure looks like, and where to look:** `handed to the
      device and not taken in 100 ms`, then `a send refused`, is the
      transmit DMA not consuming (the ring, or the transmitter); the
      first-frame line with nothing on the Mac and its error count
      rising is a bad frame, and the clock delay's other pairing is next
      (step 4b); the first-frame line with nothing and no errors, through
      a switch, says little, which is why the direct cable.
   The serial log's two PHY lines are the other reading this boot is
   for: `found with ...` is what the firmware left in the PHY.

   **First run on the board, 2026-11-13, by Hans (build
   `fff04bf2e4a9`): the kernel's half passed, the wire is not yet
   seen.** The transmit buffer walks non-cacheable; the link came up at
   1000 Mb/s full duplex; `the MAC's transmitter is on at 1000 Mb/s`;
   and at `ping 192.168.0.1`, `first frame sent: 42 bytes, taken by the
   device`, with no timeout and no busy refusal after it. So
   `send_frame` ran, and the transmit DMA consumed a descriptor. The PHY's
   two lines are in step 4b.
   **`tcpdump` showed nothing, and that says nothing:** the Mac was on
   wireless, behind the router. A frame with a bad checksum is dropped
   by the router's switch, and a good ARP request for an address outside
   the router's subnet may not be passed to the wireless side. The check
   is still owed, over a direct cable to a USB Ethernet adapter on the
   Mac, with the adapter's interface in the `tcpdump` and `netstat`
   commands above.

   **The adapter run, 2026-11-14, by Hans (build `660aebe3de30`, the
   code #463 merged): passed. The frame is on the wire, whole.** One
   cable from the Pi to a USB Ethernet adapter on the Mac (`en9`),
   `tcpdump -i en9 -e -n -xx` with no filter, started before power.
   - *The observer was shown to see first.* From the moment the link
     came up the capture printed the Mac's own frames on that interface
     (DHCP, ARP, mDNS), 211 of the capture's 212 frames, so a silence from the Pi
     would have meant something.
   - *The frame:* at `ping 192.168.0.1`, one frame from
     `d8:3a:dd:85:24:22` to broadcast, ARP, `Request who-has 192.168.0.1
     tell 10.0.2.34`, 60 bytes: the 42 `netd` built, byte for byte in
     the capture's hex, and 18 zeros after them, the device's padding.
     It was the only frame from the Pi in the run.
   - *The serial log:* the link up at 1000 Mb/s full duplex, `the MAC's
     transmitter is on at 1000 Mb/s`, `first frame sent: 42 bytes, taken
     by the device` at the `ping`, then `192.168.0.1 is unreachable (no
     ARP reply)` as expected with the receiver off. No timeout line, no
     busy refusal, no `EXCEPTION`. The PHY's two lines read as on
     2026-11-13: found with its transmit clock delay off, on after the
     reset (`0x71e7`, `0x0200`).
   - *The adapter's error count:* `netstat -I en9` read `Ierrs` 0 after
     the run, so none was counted during it.
   So the transmit clock delay as U-Boot pairs it (step 4b) is right on
   this board, the device does pad a short frame, and the DMA took the
   CPU's address of the transmit buffer as its own.
   **One prediction above was wrong, and it was mine:** the frame asks
   for `192.168.0.1`, the address pinged, and not for the gateway
   `10.0.2.2`. `netd`'s `ping` hands its target straight to
   `arp_resolve`; only the TCP and UDP paths go through `next_hop`. The
   frame is what the code sends. Whether `ping` should route by
   `next_hop` is a question for step 8, when `netd` has an address of
   its own.
   **What one frame does not show:** a second send (the buffer reused,
   `tx_idle` saying yes), a frame longer than the minimum, and anything
   received. `ping` sends one ARP request and gives up after 500 ms.

   **The run of #465 on the board, 2026-11-15, by Hans (build
   `363b87ca05e0`, `main` after #465 and a closeout of documents alone):
   passed. Nothing differs from the adapter run but what #465 added.**
   The same cable, adapter and capture (`tcpdump -i en9 -e -n -xx`, no
   filter, started before power; the wire is in `scratch/boot-465.txt`,
   the serial in `screenlog.0`).
   - *The observer was shown to see first.* The Mac's own frames start
     at 08:10:21, the Pi's first at 08:11:06.
   - *The stop line,* new with #465: `made quiet in 13 us`, both DMAs
     stopped, both status registers `0x0003ffff` (quoted in step 5).
     So the barrier before the set-up's writes and the two status reads
     after the stop, the two things new on the device, ran without a
     fault.
   - *The PHY, the link and the transmitter* read as on 2026-11-14, word
     for word, the shadows included (`0x73e7`, `0x0000` found; `0x71e7`,
     `0x0200` after the reset). The receive area and the transmit buffer
     sit one page lower (`0x378f1000`, `0x37971000`): the image is a
     different size.
   - *The frame:* the same ARP request, `who-has 192.168.0.1 tell
     10.0.2.34`, 60 bytes, the capture's hex equal to 2026-11-14's byte
     for byte.
   - *A second send, and two more.* `ping 192.168.0.1` four times
     (08:11:06, 08:13:21, 08:24:30, 08:24:55) put four frames from
     `d8:3a:dd:85:24:22` on the wire, all four equal in the hex, and no
     other frame from the Pi. So the transmit buffer is reused and
     `tx_idle` says yes after a send. The kernel's `first frame sent`
     line is printed once, so the wire is the one witness of the three
     after it.
   - *The adapter's counts:* `netstat -I en9` read 749285 packets in
     before the boot and 749289 after, the four frames and no more, and
     `Ierrs` 0 both times.
   - *A sustained boot:* `uptime` read 42503 ticks, about 14 minutes at
     20 ms a tick, which fits the capture's span. The third and fourth
     sends came after the idle time, and `shutdown` powered the board
     off. No `EXCEPTION`, no `SError`, no timeout line in the boot.
   **Still not shown:** a frame longer than the minimum, anything
   received, and a cable plugged in after the boot (`LINK_WAIT_MS`,
   step 5). The 14 minutes were idle but for four sends, with the
   receiver off, so they say nothing of a boot under traffic.

   **The review of the step (#463, read after the first boot, nothing
   run by it).** Nine findings, none in the path a frame takes. Fixed,
   the ones Hans chose:
   - *A send checked its length after it had read the link*, and on a
     boot with no link could start the MAC for a frame it then refused.
     The old code matched its doc's order, the link first; what it
     broke was the doc's "the length is checked before anything is
     written", since starting the MAC writes registers. The length is
     checked first now, and the doc's order is the code's.
   - *A link that did not read at a send was returned as "not
     started"*, the same as a link that is down. A PHY that does not
     answer, or a stuck bus, has a line now, once.
   - *The PHY's line named one missing delay where two were missing*
     (both off said only the transmit clock), and the `no link` line was
     also printed under "link up at a speed the PHY does not name". Four
     cases worded, and the line says `no link read as up at a speed`.
   - *`Delays::as_written` had no caller but the tests* once the kernel
     printed the other question, and its doc called false "the fact the
     board run is asked for". It is the tests' alone now
     (`#[cfg(test)]`), so nothing can take the board's right state for a
     fault through it.
   - Stale lines: `mmu.rs` named two non-cacheable ranges where there
     are three of four, `nic.rs` said a send that times out leaves no
     line, and the source map and `ROADMAP.md` said step 6 had not run.
   **Not fixed, and open:**
   - *A link that changes after the MAC is started is never seen.* A
     send asks the PHY only while the MAC was never started, so a cable
     moved from a gigabit port to a 100 Mb one leaves `UMAC_CMD` at the
     old speed, every frame clocked wrong and still counted as taken.
     D3's "again whenever a send finds the link down" is built for the
     boot that found none and no further.
   - *With no link, every send reads the PHY under the kernel lock*, and
     on a stuck MDIO bus that is 20 ms with interrupts masked, per send,
     for the rest of the boot. Nothing remembers that the last read
     failed.
   - *Nothing guards the `dsb sy`.* The xHCI's barrier is a fixed
     function `make check-xhci-barriers` finds in the built image; this
     one is an inline instruction a later edit can drop with every check
     green. **Fixed 2026-11-14:** the barrier is in `genet_write32`, a
     naked function every register write of the driver goes through
     (`Mmio::write`, its one caller), and `make check-xhci-barriers`
     finds it in the image. The check failed with the barrier deleted,
     with a `nop` before the store, and with `Mmio::write` routed around
     the function. So the barrier is now before every register write,
     the set-up's too, where it was before a send's alone: not yet run
     on the board. **Run on the board 2026-11-15, passed** (the run of
     #465, under step 6's adapter run).
     **Its review the same day** (no fault in the path; eight findings,
     seven fixed): the "one caller" the check rests on was a comment,
     and is now privacy, the bus and its store in `mod bus` so a second
     caller outside that module does not compile; "every GENET register
     write" was false, since `netscan.rs`'s scan sends its MDIO read
     commands through a store of its own, and the Makefile now reads
     "every register write the driver's bus makes"; the entry in
     `make-targets.md` names the file and the function. **Left open:**
     the check tells functions apart by their instructions, so this
     one's operands are shaped by the check; finding them by symbol
     would lift that, and two drivers have not needed it.
     **A second review, of those fixes alone:** no fault in the code,
     and eight statements of mine that did not match it, all eight
     corrected. The stop's line promised a transmit DMA "is asked again
     after the MAC's reset", which `Genet::init` does only on a boot
     that reaches it, and the line now promises nothing. "A second
     caller does not compile" stood in four records without "outside
     the module". The plan said printing the stop's status needed a
     change to the crate, when both registers are public and `quiet`
     holds the bus: it reads them now. A duration of 0 was printed
     where the counter's frequency reads 0, and is now said as not
     known (`timer::us_since`). **Left open, outside this driver:**
     `xhci.rs`'s `publish_cycle_word` sits outside `mod reg`, its one
     caller held by convention, the thing `mod bus` was made to end
     here; and `timer::monotonic_us` divides by the counter's frequency
     unchecked, so a firmware that left it 0 ends the boot at the first
     caller. Neither is reachable on the Pi 4 as it runs.
     **The second fixed 2026-11-18** (`timer/zero-frequency`, #488):
     `timer::init` reads the boot core's frequency once, at the top of
     `kernel_main`, logs it, and keeps it as the one value every core
     uses (`timer::FREQUENCY_HZ`); below `timer::MIN_FREQUENCY_HZ`, 1 MHz,
     the boot parks with a line while the firmware's console still
     prints, its watchdog off. The floor is the kernel's own: every
     millisecond deadline is `frequency_hz() / 1000 * ms`, truncated
     first. **The review of #488** changed three things in the first
     version: the stop was a `panic!`, and uefi's handler stalls on the
     firmware's timer and then shuts the machine down, so the first
     run's "stopped" at 0 was the line and then edk2's `ASSERT
     [MetronomeDxe] ArmArchTimerLib.c(44): TimerFreq != 0` inside that
     stall; the check read the boot core's register while every call
     read its own core's, and `CNTFRQ_EL0` is per core; and the floor was
     1 kHz, where a truncated millisecond can be half one. Checked on
     QEMU at EL2 (`make run-el2`'s machine), with a temporary write of
     `CNTFRQ_EL0` before `timer::init`: 0 and 999999 parked with the
     line as the console's last, 1000000 booted to the shell, and the
     unmodified build logged 62500000 Hz and booted. The control, 0 with
     no check, did not reach the kernel's division: edk2's debug firmware
     ended the boot first with the same `ASSERT`, in a delay of its own.
     **Run on the board 2026-11-19, passed** (`73c1fb944a3f`, debug,
     boot 48, by Hans): `the generic timer's counter runs at 54000000
     Hz`, the Pi 4's first recorded frequency, 54 times the floor; the
     three secondaries up, the link up at 1000 Mb/s, login, a ping with
     its reply, and `curl` from the Mac returned the page.
     **The first fixed 2026-11-19** (`xhci/cycle-word-in-reg`, #489):
     `publish_cycle_word` is private to a `mod ring` in `xhci.rs`, and
     `ring_publish`, its one caller, and `ring_push` take a `ring::Ring`,
     a handle only that module builds, from the ring statics with each
     array's own length, as only `reg::locate` builds `reg`'s. So from
     outside the module neither the store nor a publish can be pointed at
     a register, at any other address, or at a ring with another's
     length. It is memory and not a register, so a module of its own and
     not `mod reg`. **Its review** found the first version had made the
     store private and left `ring_publish` taking any pointer: the
     convention had moved up one layer, not ended. Code added inside
     `mod ring` is not held, the limit `mod bus` has too. **Run on the
     board 2026-11-19, passed** (`f4d9ad31a673`, debug, boot 49, by
     Hans), with a USB stick and a keyboard in: the command ring and EP0
     rings enumerated the controller's hub, the stick (port 3, mass
     storage, its bulk endpoints configured) and the keyboard behind the
     hub (port 1.4); `keyboard ready` posted the interrupt ring, and keys
     typed on it reached the shell. No assertion, fault, `wedged` or
     `restarted` line. **The bulk rings were not run on the board:** the
     kernel holds one block device, the SD card took it as the boot disk
     (`usb-msd not installed: the SD card is the boot disk`), and no
     transfer went to the stick; `make test-usb-hub` runs them on QEMU.
   - *The send-time lines are said through the console that goes quiet
     once `cond` owns a framebuffer* (the review's reading, not checked
     here), so on a board with no serial console the first-frame line
     and both failure lines may never show, with their said-once flags
     spent.
   - The on/off wording of a `Delays` is written out four times, and
     the receive area and the transmit buffer have a walker each.
   **Checked after the fixes:** `make test` exit 0, 443 tests;
   `make test-netdump` exit 0. `send_frame` changed (the order of its
   first two checks, and one line), so the board's one run of it was of
   the code before the review: the adapter run is on a card staged
   after it.
   **A second review, of the fix alone** (commit `dd245c3b`, by
   reading): no fault in the code, nothing on a frame's path, and three
   statements of mine corrected: the first fix's sentence above, which had
   the old doc saying what it did not; "the link is read once, with no wait" in
   `genetnic.rs`, which is up to three MDIO reads on every send while
   the MAC is not started; and three lists of the lines said once, each
   one short of the line the fix added.

7. **The board: receive, polled.** The ARP reply and the echo reply come
   back through `NET_RECV`.
   **Check:** `ping` of the Mac from the Pi answers; `ping` of the Pi from
   the Mac answers; `fetch` of a file of a few megabytes from the Mac's
   web server, its checksum equal to the Mac's (this is what catches the
   2-byte pad, a ring wrap and a stale buffer); the same `fetch` run
   enough times to wrap the 16-bit counters, with no fault.

   **Built 2026-11-15; run on the board the same day, three boots, and
   it works from the third** (the result is at the end of this step).
   - *In the kernel,* `genetnic.rs`'s start passes `Receiver::On`, at
     the boot's link or at the first send that finds one, so from there
     the device writes frames into the receive area. `poll_frame` asks
     the crate's `rx_next`, copies a usable frame out of its buffer by a
     raw pointer, cut to the caller's buffer, and gives the buffer back
     (`rx_release`); a frame the crate refuses is dropped there and the
     next looked at, at most 256 in one call. Nothing in the kernel
     writes a receive buffer. No register store was added: the release
     is the crate's write, through the bus, behind the barrier.
   - *Its lines:* the start's line now reads `the MAC's transmitter and
     receiver are on at N Mb/s (UMAC_CMD 0x..., UMAC_MDF_CTRL 0x...)`,
     the two registers read back after the start. `UMAC_MDF_CTRL` (Linux
     `bcmgenet.h` line 183) is the destination filters the firmware left
     on; this driver writes none, as U-Boot writes none, and the value
     is there for the day nothing is received. Then `first frame
     received: N bytes, from buffer B of the receive area`, once, and `a
     received frame was dropped (cause; N taken before it)`, once.
   - *In `netd`,* the clamp step 3 found owed: `recv` drops a frame
     longer than the buffer it was cut to and asks for the next, so its
     callers index their 1600 bytes by a length that fits. No rig sends
     such a frame (SLIRP sends none, and the GENET returns at most
     1536), so the drop is a path no check has failed.
   - *On QEMU,* where no GENET is declared, nothing of the kernel's half
     runs. `netd`'s `recv` does, on virtio-net: `make test` and the
     network rigs are the check that it still receives.
   **The check as written cannot all be run with what `netd` is today,
   read before the board and not found on it:**
   - *`ping` of the Pi from the Mac* gets no answer whatever the driver
     does: `on_frame` answers ARP requests and TCP, and has no arm for
     an ICMP echo request. What the Mac can show is its ARP table
     holding the board's address (`arp -n`), which needs the Pi to have
     received the request and sent the reply.
   - *`fetch` of a file from the Mac* is not available: `fetch` resolves
     its host through DNS at `10.0.2.3`, takes no address, returns one
     message's worth, and there is no checksum command on the board.
   - **In its place, the other direction:** `curl` on the Mac of a file
     from `netd`'s HTTP server on the board, its checksum against the
     staged file's (`build/esp/bin/CPP`, 108 KB). Each transfer is the
     Mac's SYN and some tens of ACKs received (78 segments of 1400
     bytes, so about 44 to 82 frames from the Mac), so six of them wrap
     the 256-buffer ring, and 800 to 1500 wrap the 16-bit index. The
     kernel says its count of frames taken at 256 and at each 65536, so
     the log shows whether either was reached. It
     catches a stale or misplaced received frame as a transfer that
     stalls or a checksum that differs, though less directly than a
     received file would: the bytes compared are ones the board sent.
     The 2-byte pad is checked by every frame `netd` understands at
     all.
   So on the board: `ping 10.0.2.2` from the Pi answers (an ARP reply
   and an echo reply received), with the Mac's `en9` set to `10.0.2.2`
   by hand since `netd`'s address is `10.0.2.34` until step 8; the `curl`
   above, repeated; and no fault. An echo reply from `netd` and a
   `fetch` by address are what the check as written would need, each a
   change to `netd` alone.

   **The review of the step (2026-11-15, before the board, nothing run
   by it).** No fault found in the path a frame takes. Fixed: the counts
   were never said after the first frame and the first drop, so a ring
   wrap could not be read from the log (the count line above, with the
   crate's drops by cause); a flagged frame's status was printed in
   decimal; "several hundred" transfers for the 16-bit wrap was wrong by
   the arithmetic; and `ROADMAP.md` still said the step was next.
   **Left open, each a decision and none in the way of the board's
   run:**
   - *A link that comes up after the boot's wait is seen only at a
     send* (decision D3, made for a transmitter). With the receiver the
     same rule leaves the board deaf until the Pi itself sends: `netd`
     sends nothing unprompted, so a Mac's ARP goes unanswered until
     someone runs `ping` on the board. A link that drops and returns is
     never read again either. Asking the link from the tick is not
     open to it, since `has_frame` may not write or wait.
     **The late cable fixed on 2026-11-17** (`genet/late-link`; Hans
     chose the wait over the tick and over a `netd` announcement):
     `NET_WAIT` asks the link at most once a second while the MAC is
     off (`Device::link_at_wait`, the one ask a send makes, now
     `Device::ask_link`), starts the MAC if it is up, and limits `netd`'s
     wait to the time left until the next ask; three asks running whose
     reads fail stop the wait's asking, with a line. That rule is
     `genet::linkask`, host-tested since the branch's review, which
     found the first version asking about every two seconds (the wait
     ends on a 20 ms tick and was told a whole second again) and
     stopping for good at one failed read. **Shown on the board with a
     control**, each boot with the cable out until after `login:` and
     nothing typed on the Pi, the Mac asking `curl
     http://192.168.77.2/` and `arp -n 192.168.77.2` about 10 s after the
     cable went in. `main` (`5e4c0980713b`, boot 37): `link down (waited
     5000 ms)`, no link line after, `curl` 000, the ARP entry
     `(incomplete)`. The fix (`a8aaaeef04b4`): `genet: link up at a
     wait, 1000 Mb/s, full duplex`, the MAC on, a frame received and a
     42-byte one sent, `curl` 200 and the ARP entry
     `d8:3a:dd:85:24:22`. The reviewed build, with `linkask`
     (`1aaedc0c0c78`, boot 38), booted the same way and with nobody
     logged in: `link up at a wait` while the board sat at `login:`, the
     MAC on, a frame in and a 42-byte one out, no restart, exception or
     warning; `curl` 200 and the ARP entry `d8:3a:dd:85:24:22`. The
     control above stands for both. **Still open:** a link that drops and
     returns is not read again once the MAC is on.
     **Built on 2026-11-18** (`genet/link-reread`; Hans chose refusing
     sends while the link is down over a line alone): the wait goes on
     asking once a second with the MAC on. `genet::linkask::change`
     (host-tested) says what a read means against the link held: up
     where none was, or at another speed, sets the MAC at it
     (`Genet::start` again, which rewrites the speed and keeps the
     enables), with the link-up and pause lines; up at the same speed
     and another duplex starts nothing (the MAC is not told the duplex)
     and says the pause line again; anything not up where one was is
     `link down at a wait (...)`, and every send is refused (the
     kernel's `SendError::NoLink`, `NET_ERROR` to `netd`) and asks the
     link itself until it is up again. The MAC and its receiver stay on
     through a drop. **The review of the branch** changed the asking's
     stop: three failed asks running now slow it to once a minute, and
     a read brings the second back (Hans's choice over letting the link
     go or keeping the stop), since a stop with the MAC on left a later
     cable pull unseen for good. Up, the asking costs three MDIO reads a
     second under the kernel lock, and wakes `netd` once a second for
     good. **Not seen, by design:** a drop shorter than a second that
     returns at the same speed and duplex (the link bit's latch is read
     past, as U-Boot and Linux read it); nothing in the MAC needs
     changing then. **Not yet on the board.** The check: a cable pulled
     for several seconds and put back with `netd` serving, then `curl`;
     a send during the pull, to see whether a frame the device held at
     the drop leaves the transmitter busy after it (the review asked;
     on RGMII the MAC drives its own transmit clock, so it is expected
     to drain, and only the board can say); and if a switch at 100 Mb/s
     is at hand, a cable moved to it.
     **Run on the board 2026-11-19, passed** (`289a4baf7afa`, debug,
     boot 46, by Hans): link up at 1000 Mb/s full duplex at boot, the
     pause line as boot 45's; the cable pulled, and at the next ask
     `link down at a wait (no link): sends are refused until it is up
     again; the MAC stays on`. A `ping 192.168.77.1` during the pull
     answered `unreachable (no ARP reply)`, the send refused with no
     busy or timeout line. The cable back: `link up at a wait, 1000 Mb/s,
     full duplex`, `the MAC, on since before the link dropped, is set
     again at 1000 Mb/s (UMAC_CMD 0x1000010b ...)` and the pause line;
     the same ping then had its reply, and the boot's first frame sent
     (42 bytes, the ARP request) was taken by the device. No `wedged`
     or `restarted` line. **Not shown by this run:** a frame in the
     transmitter at the drop, since nothing had been sent before the
     pull (the first frame sent was after it), so the review's question
     of a transmitter left busy stays open; the speed change, with no
     100 Mb/s switch in the run; and `curl` (the ping stands for the
     path through `netd`).
     **Its second review, 2026-11-19** (seven findings; five fixed, one
     left open, one declined): a send's asks did not count toward the
     back-off, so a send that read the link up left the wait asking once
     a minute, and after a drop a stuck bus was read on every send. Every
     ask now tells `linkask` whether it read, and a send does not ask
     while the asking is backed off (`LinkAsk::backed_off`, host-tested;
     three mutations of it each failed the tests). That closes step 6's
     item that nothing remembered a failed read. The restart line said
     the MAC was "on since before the link dropped" for a speed changed
     with no drop read between, and says "on at the speed before" there
     now. The line for a link that did not read at a send was said once
     a boot, and is once a link. Three docs still said a single run of
     `say_pause`, or that `Change::Up` covered a change of duplex.
     **Left open:** a link renegotiated to half duplex runs with the MAC
     at full duplex, since nothing sets `CMD_HD_EN` (Linux's
     `bcmgenet_mac_config` does; U-Boot does not). The boot's own start
     is the same, so it is older than this branch, and only a half-duplex
     partner on the board can show it. **Declined:** reading the status
     register once while a link is held, in place of three reads a
     second. The latched read would see every short drop as down and
     restart the MAC for it, the thing `phy::status` reads past on
     purpose. The kernel's half of the fixes is checked by reading and
     clippy, and its common path on the board (below).
     **Run on the board again 2026-11-19, passed** (`2cf07036941f`,
     debug, boot 47, by Hans, the branch's head after its second
     review): this time a ping before the pull, so the boot's first
     frame was sent and taken before the link dropped. Two pulls, each
     the same: `link down at a wait`, a ping during it `unreachable (no
     ARP reply)`, the cable back, `link up at a wait, 1000 Mb/s, full
     duplex`, `the MAC, on since before the link dropped, is set again`
     and the pause line, then a ping with its reply; and `curl` from the
     Mac returned the page after each. No line of a send refused as busy
     or of a link that did not read, and no `wedged` or `restarted`.
     **That answers the review's question:** with a frame sent before the
     drop, the transmitter was free after it, and the first send after
     each return was taken. Still not shown: the speed change and its
     line, and the back-off on a stuck bus, neither of which the board
     can be made to do with a cable alone.
   - *The receiver is never turned off.* With nobody draining it
     (before `netd` first runs, after `netd` is past its restart cap,
     after the kernel halts) the ring fills and the DMA sits at its
     flow-control threshold. Whether the MAC then sends PAUSE frames is
     not known here; U-Boot stops the MAC after each command and never
     leaves it on undrained.
     **It did, and it is fixed on 2026-11-17** (`genet/receiver-off`; Hans
     chose ignoring pause over turning the receiver off): every
     `Mac::start` sets `CMD_TX_PAUSE_IGNORE` and `CMD_RX_PAUSE_IGNORE`
     (`UMAC_CMD` bits 28 and 8, cited to Linux's `unimac.h`). Linux sets
     each bit when its direction was not negotiated
     (`bcmgenet_mac_config`); this driver reads no pause resolution and
     sets both. The receiver stays on by design: a full ring now
     drops frames and sends nothing. **Shown on the board with a
     control**, each boot with `netd` left out (`NETD.BIN` renamed),
     the Mac flooding `ping -f` at the Pi with a static ARP entry, and a
     temporary line every 5 s reading the MAC's own counters (never
     committed). `main` (`d2c12835f18b`, boot 39, `UMAC_CMD
     0x0000000b`): pause frames sent rose about 30 a second from the
     second reading, steadily, with or without the flood, and were
     still rising long after it, 10365 in six minutes; none received,
     no receive-buffer overflow. The fix (`34a4157799e2`, boot 41,
     `UMAC_CMD 0x1000010b`): pause frames sent 0 for the whole boot,
     while the MAC received 20081 frames and the ring's producer stopped
     at 266 (the firmware's 10 plus 256 descriptors) with 19813
     discarded. Boot 40, the same build with a diagnostic too narrow to
     see the frames arrive, proved nothing either way and is not
     counted. **Still open:** the frames the full ring drops before
     `netd` runs are lost, which is what a NIC with no reader should do;
     nothing tells `netd` how many. And whether pause is in fact
     negotiated has not been read: the PHY keeps its reset-default
     advertisement (`phy.rs`, `genphy_config_aneg` left out), and neither
     its `ADVERTISE` nor the partner's `LPA` pause bits (10 and 11) have
     been logged. If both offer it, the link expects pause honoured and
     the MAC ignores what the other end sends; a read of the two
     registers at the link line would settle it (the branch's review,
     2026-11-17). **The read is in on 2026-11-18** (`genet/pause-bits`):
     `genet::phy::pause` reads both registers whole (two MDIO reads,
     nothing written), `Pause::agreed` resolves their bits 10 and 11 by
     Linux's `mii_resolve_flowctrl_fdx` (host-tested against IEEE 802.3
     Table 28B-3, all sixteen rows) and agrees nothing when `LPA`'s
     bit 14 says no partner page was received, and
     `genet::mac::pause_in_cmd` reads what `UMAC_CMD` does. The kernel
     logs `genet: pause: ...` once, right after the MAC's start line on
     whichever path started it (the boot, a wait or a send), with both
     registers whole and the verdict "as agreed" or "NOT as agreed"
     taken from `UMAC_CMD` as read, not from what the start is believed
     to write (the branch's review). **The board's answer, 2026-11-18**
     (`ad5798c7067e`, debug, boot 45, the Mac's USB adapter at the other
     end): `this end advertises no pause, the partner symmetric and
     asymmetric pause (ADVERTISE 0x01e1, LPA 0xdde1); the link agrees no
     pause, the MAC does no pause: as agreed`. The PHY's reset-default
     advertisement offers no pause, so ignoring it both ways is what the
     link agreed, and the question is closed. It stays closed only while
     the advertisement does: a set-up that made the PHY offer pause
     would turn the line to "NOT as agreed" on its first boot. The same
     boot was #482's first on the board: no exception, fault or restart,
     `netd` up from `/etc/net`, frames received; no frame past 1600
     bytes was sent, so `NET_FRAME_CUT` itself is still unseen there. The firmware's own MAC, on with pause honoured until
     the kernel quiets it, is left as it is: a window of milliseconds in
     which the firmware ran the same way before (the review again,
     declined).
   - *`netd`'s drain loop has no bound and no heartbeat* (`while let
     Some(n) = recv(...)`, and `recv`'s own loop over oversize frames).
     Frames arriving faster than they are handled for 2.56 s would have
     the supervisor call `netd` wedged. SLIRP never did that; a wire
     can.
     **It did, and it is fixed on 2026-11-17** (`netd/drain-bound`; Hans
     chose a bound with progress at `NET_WAIT` over a heartbeat in
     `netd`'s loop and over a heartbeat alone). A pass of `serve`'s loop
     takes at most `DRAIN_BUDGET` frames (64, Linux's NAPI weight) before
     it serves its mailbox and timers, `recv` skips at most as many frames
     too long for its buffer (each `NET_RECV` counted against the pass's
     budget, `recv`'s own skip bounded apart), and the kernel's `NET_WAIT`
     is progress for the supervisor when the pass that ended there took a
     frame or a message (`supervisor::note_pass`, marked by `NET_RECV` and
     `MSG_TRY_RECV` through `note_input`; messages since the second
     review, which found a stream of them with no frame, a `cpu`
     command's output say, wedged `netd` just as a flood did, on `main`
     as well): under a flood it returns at once and `netd` is never
     seen blocked. A wedge inside a pass is still caught, since that pass
     never reaches the wait; and so is a spin through passes that take
     nothing, which the first version, noting progress at every
     `NET_WAIT`, would never have restarted (the branch's review; under
     it the supervisor's ping never fires either, since `netd` is never
     `Blocked`). **Seen on the board
     first, as the control** (`main`, `5f43c89044e3`, boot 42): `curl`
     answered 200, then `scratch/udp_flood.py` sent small UDP datagrams
     from the Mac for 15 s. About 65,000 frames in, `server slot 4 wedged
     - no progress (runnable) - restarting`, three restarts, each wedged
     again by the flood, then `failed more than 3 times this boot - giving
     up`; `curl` after hung, the board without a network server for the
     rest of the boot. **QEMU shows it too**, so a rig guards it: `make
     test-net-flood` floods a forwarded UDP port for 15 s and checks that
     no server is called wedged, that `netd` answers HTTP after, and that
     the pcap holds at least 50,000 of the datagrams (it held 1.7 to 2.0
     million). It boots `netd`'s restart-rig build, whose serve loop is
     the shipped one, and then pings 192.0.2.252 from the guest, on which
     that build loops on `NET_WAIT` reading nothing: the supervisor must
     call it wedged and restart it. Controlled: `main` failed it with 4
     wedges and 3 restarts and no HTTP after, and so did the fix with
     either half taken out, the drain unbounded or the note at `NET_WAIT`
     removed, and with `note_input` never marking a pass; with every
     `NET_WAIT` progress, the first version's rule, the flood passed and
     the spin was never restarted. **And the fix on the board**
     (`netd/drain-bound`, `fb7a12cdbeb1`, boot 43, 2026-11-18, by Hans):
     the same flood, the same 15 s, `curl` answered 200 before and
     after, and the serial log has no `wedged` and no `restarted` line.
     The driver's own count passed 720,896 frames received, none dropped
     by it, where `main` wedged about 65,000 in; the Mac's `en9` counted
     1.6 million packets out. **Still open:** under a flood `netd` serves its
     clients between passes but drops what the full ring drops, its own
     connections' frames among them; the message half of `note_input` is
     checked by reading and by the `cpu` rigs still passing, since no rig
     drives 2.56 s of messages with no frame; and the budget counts
     frames, not time, so 64 frames each costing 40 ms would still wedge
     `netd` (none measured near that; the second review, declined).
     Declined too: the registry looked up for every frame taken (four
     entries, under the lock `NET_RECV` already holds).
   - *`NET_RECV` returned a length longer than the buffer it filled*,
     and the clamp was in `netd`'s one `recv`, so a second caller would
     meet the trap again. **Fixed 2026-11-18:** a frame longer than the
     buffer is taken and dropped and answered `NET_FRAME_CUT`
     (`syscall_abi`, `u64::MAX - 2`), so every answer but a sentinel is
     at most the buffer's length. One place for both drivers,
     `Nic::poll_frame`, which turns a driver's length past the buffer
     into `Received::Cut`; `netd`'s `recv_one` reads the sentinel, and
     keeps a length past its buffer as a dropped frame, never an index.
     Chosen over the copied length (a cut frame handed on as whole) and
     over leaving the frame on the ring (a caller that never retries
     spins on it). **Checked:** `make test-net-flood` pings 192.0.2.249,
     on which the rig's `netd` reads the first frame after an ARP request
     into 16 bytes; the answer must be `NET_FRAME_CUT`, and `netd`'s own
     reading of it (`got`, the half of `recv_one` after the syscall) a
     frame too long. Controlled: with `Nic::poll_frame` answering every
     frame whole it answered 64, and with `got` reading the sentinel as
     nothing the check failed too. The review (`/code-review high`) had
     the probe log on one line and beat, the sentinels asserted apart and
     above `NET_MAX_FRAME`, and `got` checked by the rig, which until
     then ran only on a frame over 1600 bytes, never sent by SLIRP.
     Declined: the bound in each driver before its copy, which would
     spare a copy of a frame dropped anyway but put the bound in two
     places again; and one loop shared by the probe and `arp_resolve`,
     the probe being the rig's build alone.

   **The board, 2026-11-15, by Hans: three boots, one cable to the Mac's
   adapter, `en9` set to `10.0.2.2` by hand.** The wire of each is in
   `scratch/boot-step7.txt`, `-step7b.txt` and `-step7c.txt`, the serial
   in `screenlog.0`.
   - *First boot (build `b43b881ecfcb`): nothing received.* The wire was
     right: the Pi's ARP request for `10.0.2.2`, the Mac's reply within
     a millisecond, the Mac's SYNs and its own ARP requests for
     `10.0.2.34`, none answered. The serial log had one line for it: `a
     received frame was dropped (Oversize; 0 taken before it)`. No frame
     the Mac sent was over 1403 bytes.
   - *Second boot (build `c42393e2f713`), a diagnostic that changed no
     behaviour:* the receive ring's indexes, its pointers, and which of
     the 256 descriptors no longer held the value the set-up wrote. It
     read: producer index 10 at the set-up, as the firmware left it;
     after the first frame, the index 11, the write pointer 3 (one
     descriptor, in words), and exactly one descriptor written,
     **number 0**, holding `0x003e7fc0`: a whole clean frame of 60
     bytes.
   - **The fault: the crate looked for a frame in the descriptor named
     by the index's low byte, as U-Boot does, and the device fills the
     ring from where the write pointer was set, 0.** With the index at
     10, every frame was looked for ten descriptors from where it lay,
     in one that still held the set-up's 2048, which reads as oversize.
     Linux keeps a pointer of its own. Fixed in the crate: `rx_desc`
     counts from 0, apart from the consumer index.
   - **Why the host did not find it: the model was written from the
     driver.** It placed a frame by the index's low byte, the same
     mistake from the other side, so tests that started the index at 5,
     at `0x00f0` and at `0xfffe` all passed. The model now places frames
     from the write pointer, as the board was seen to; with that and the
     driver unfixed, four tests failed.
   - *Third boot (build `a9eeb46bcdaf`, the fix): it receives.* The
     index stood at 9 this time. `first frame received: 60 bytes, from
     buffer 0 of the receive area`; `ping 10.0.2.2` said `reply from
     10.0.2.2`; the Mac's ARP table held `10.0.2.34` at the board's
     address.
   - *Files from the board's HTTP server, by `curl` on the Mac:* all
     246 files under `include` and `man`; 241 equal to the staged
     copies by SHA-256, 827 KB in all. The other 5 are man pages edited
     since the card's root was written (`man/tree` read as its text
     from before 2026-11-11): `make sdcard` stages the boot partition
     alone, so the card's root, its binaries too, is an older build,
     and `/bin/cpp` could not be compared with `build/esp/bin/CPP`.
     Then `/bin/cpp` 1500 times against its own first checksum: 1500
     equal, in 1420 s.
   - *The counts:* `256 frames received so far`, `65536` and `131072`,
     each with no frame dropped for any cause. So the ring went round
     and the 16-bit index wrapped, twice. `en9` counted no input error.
     No `EXCEPTION`.
   - *Two mistakes of mine in the check, neither the board's:* the file
     is `/bin/cpp` on the card, not `/bin/CPP`, so the first twenty
     checksums Hans read were of a `404` page; and the staged binaries
     were not what the card held.
   **The transmit ring has the same shape and was left as it is.** It
   takes its descriptor from its index's low byte too. The diagnostic
   read that index as 0 at the set-up, which is why every send so far
   went where the device looked; a firmware that sends a frame before
   the exit (a network boot tried first) would leave it elsewhere. That
   the device reads the transmit ring from its read pointer, as it
   fills the receive ring from its write pointer, is likely and was not
   seen, since the index was 0. The kernel now says both indexes at the
   set-up (`the indexes the firmware left: receive N, transmit M`), with
   a warning when the transmit one is off a multiple of 256. Counting
   the transmit descriptor from 0 as well is the change to make; it is
   Hans's to decide, since no boot here can show it wrong or right.
   **A fourth boot, the same day (build `788ca3491741`, the diagnostic
   removed and that line added): as the third.** `the indexes the
   firmware left: receive 9, transmit 0`, `first frame received: 60
   bytes, from buffer 0`, `ping 10.0.2.2` answered, and one file fetched
   from the board equal to its staged copy.
   **Decided by Hans on 2026-11-16: count the transmit descriptor from
   0 (#472), the merge held for a board check, which passed the same
   day.** The crate's `tx_desc` counts from 0, where the set-up writes
   `TDMA_READ_PTR`, apart from the producer index. The model reads a
   sent frame from its read pointer; with that and the driver unfixed,
   three tests failed. #472 first cited Linux's `write_ptr` as the
   evidence, and its review found that Linux cannot tell the two
   readings apart: `bcmgenet_init_tx_ring` writes both indexes 0, so
   with ring 16 starting at descriptor `tx_queues * tx_bds_per_q` the
   read pointer and "the ring's start plus the index" name the same
   descriptor, and with our ring at 0 the second is U-Boot's low byte.
   So the board was asked.
   **The board, 2026-11-16, by Hans: two boots with a temporary
   diagnostic** (`df6fd2a7`, removed before the merge) that wrote 10 to
   `TDMA_CONS_INDEX` between `quiet` and the set-up, read it back, and
   gave the read pointer at the first send. The serial logs are
   `scratch/boot-472.txt` and `-control.txt`, the wire `boot-472.pcap`
   and `-control.pcap`.
   - **The index can be set,** whatever U-Boot's comment says: it read
     0, then `0xa` after the write, and the set-up's line said
     `transmit 10`, so it survived the MAC's reset.
   - *First boot (build `df6fd2a7d531`, counted from 0): it sends.*
     `first frame sent: 42 bytes, taken by the device; DIAGNOSTIC:
     transmit read pointer now 3 (words), consumer index 0xb`: the
     device took descriptor 0, with the index at 10. On the wire, the
     ARP request and the ICMP echo request whole, and `reply from
     192.168.77.1`. Then 200 fetches of `/etc/net` from the Mac, all
     equal to the file; 1004 frames from the board in the capture, so
     the ring went round about four times from that start. No timeout,
     no busy refusal, no frame dropped, no `EXCEPTION`. (The link was
     down at boot, a cable not fully in; it came up at a send.)
   - *The control (build `df6fd2a7d531+dirty`, the driver's transmit
     descriptor put back to the index's low byte): it does not.* Link
     up at 1000 Mb/s, then `a frame of 42 bytes was handed to the
     device and not taken in 100 ms (0 sent before it)`, then `a send
     refused: the device still has the frame before it`. Not one frame
     from the board in the 217 captured.
   - *A claim of mine the control corrected:* I had written that a
     wrong rule fails silently, the device sending a stale descriptor
     while its index moves, and the kernel warned on that ground. The
     device instead took nothing: the descriptor it read was never
     written. The warning is gone with the question it asked; the
     indexes line stays. The host model still sends a stale descriptor
     there, and says at `tx_at` that the board did not.
   **Still not shown:** a ping of the Pi from the Mac and a `fetch` by
   address (`netd` does neither), a frame addressed to the board longer
   than 165 bytes, the longest in the capture (the transfers' large
   frames all went out; whether the Mac's multicast frames of up to
   1403 bytes were taken or filtered by the MAC is not read), a cable
   plugged in after the boot.

8. **The address file.** `netd` reads its configuration from a file on the
   root (D4), absent meaning today's values.
   **Check:** on QEMU, every rig passes with no file; a new boot with a
   file naming another address in SLIRP's range answers at that address
   and not the old one; a malformed file is refused with a line and falls
   back.

   **Built 2026-11-15, on QEMU; run on the board the same day, and it
   works** (the result is at the end of this step).
   - *The file is `/etc/net`*, lines of `key value`: `address` and
     `mask` required, `gateway` and `dns` optional, `#` lines and blank
     lines skipped. With no gateway only the machine's own subnet is
     reached; with no `dns` no name resolves. **A file with anything
     not understood is refused whole,** with a line that says why and
     at which line of the file: a half-read file would give a machine
     an address its operator did not write.
   - *The rules are a pure crate, `netconf`,* host-tested (13 tests):
     the parse and each refusal, the values with no file
     (`Config::slirp`, what `netd` held as constants), and the next hop.
     The /24 that `next_hop` assumed is now the file's mask.
   - *In `netd`,* `load_netconf` reads the file once at start, called
     from `load_auth` after its reads have waited out the disk's mount
     and before its last lines, and sets four atomics
     that `our_ip`, `next_hop` and the DNS code read. One line says the
     configuration on every boot and where it came from: `netd: address
     A/N, gateway G, dns D (from /etc/net)`, or `(no /etc/net: the
     defaults)`, or `(nothing mounted to read /etc/net from: the
     defaults)`, or a warning that the file could not be read or was
     refused. Every way of having no usable file ends in the defaults.
   - **`ping` goes through the next hop now.** The adapter run
     (2026-11-14) found `handle_ping` asking the wire for the target's
     own address, so a ping off the subnet could not be answered. It
     asks for the gateway's as the TCP paths do. A DNS server off the
     subnet is reached the same way.
   - *The check, `make test-net-config`,* three boots of a copy of the
     ext2 image: a file naming `10.0.2.20` (reached there through a
     forwarded port, **not** at `10.0.2.15`, and `ping 10.0.2.2` from
     the guest answers); a file whose third line is not understood,
     after a good address line (refused with its line named, the
     defaults used, reached at `.15` and **not** at the `.20` its first
     line named); and no file, the control. The first boot also pings
     an address off the subnet, which must not be refused for want of
     an ARP reply. 21 checks, all passed. Controlled twice: with `netd`
     ignoring a good file, three checks of the first boot failed; with
     `handle_ping` put back to asking for the target's own address, the
     off-subnet check failed.
   - *Not checked by any rig:* a file that cannot be read (a disk
     error), a configuration with no gateway or no DNS server on the
     wire, and a mask other than /24 past the host tests.
   **The review of the step (2026-11-15, before the board).** Ten
   findings, no fault in the path. Fixed:
   - *The file was taken from one read.* A read may return fewer bytes
     than asked without being the end, and a file cut short can still
     parse, to an address nobody wrote. It is read until a read returns
     nothing, as the users registry is.
   - *A DNS server at our own subnet's own or broadcast address was
     accepted,* where the same mistake in the address or the gateway is
     refused.
   - *The address line was printed after `load_auth`'s last line,* which
     two rigs wait for before they type (`test-blank-disk.py`,
     `test-sd.py`). It is printed before those lines now.
   - *A boot with nothing mounted,* a supported one, was given a warning
     and a retry budget of its own, against `load_auth`'s rule of one
     budget. `NO_FS` is now taken at once and said without a warning.
   - *Nothing checked the ping through the next hop;* the rig's one ping
     was on the subnet. The rig pings off it now.
   - `test-net-config` was not in `.PHONY`; the rig left its image
     copies behind when its set-up failed; `resolve`'s and `fetch`'s
     rows, two comments and a sentence saying userland has no statics
     still described what this step changed.
   **Decided by Hans on 2026-11-16:** `/etc/net` keeps its name and
   format, and `netd` keeps its configuration in the four atomics. Both
   were made in this step without asking him.
   **Left open:**
   - *A disk that mounts after `load_auth` has given up leaves the
     machine on the defaults for the boot,* as it leaves it with no
     cluster identity. `NET_WAIT` is not a sleep (`testing-pi4.md`, risk
     4b): with a frame or a message waiting it returns at once, so the
     retries can be spent in a moment, and since step 7 a board on a
     busy wire always has a frame waiting. (Until #475, 2026-11-16, it
     returned at the next tick, not at once: it blocked without asking,
     so each retry cost up to 20 ms and the budget at most half its 2 s.
     Since #475 it returns at once, which makes this item worse.) Nothing reads either file
     again. Not seen on the board, whose card mounted in time on every
     boot; the cure is a wait that is a wait, for both files.
     **The wait fixed on 2026-11-17** (#477): the
     retries sleep (`SLEEP_UNTIL`), and the budget is 2 s of time slept
     between reads, whatever arrives and however long a read blocks in
     `fsd` (`LoadBudget`; `load_auth`'s
     three reads together; `load_netconf`'s own transient retries have
     200 ms, `NETCONF_WAIT_US`, since it waits for no mount). Shown on
     QEMU, not yet on the board (`testing-pi4.md`, risk 4b). What stays
     open is the rest of this item: a disk that mounts after 2 s still
     leaves the defaults, and nothing reads either file again.
     **Done 2026-11-22** (`netd/late-etc-net`). Reading `fsd` first
     changed the question: it mounts tree 0 before it takes its first
     request, and the kernel queues a call to a task not yet receiving, so
     at boot a load that answers `NO_FS` has found what the boot holds,
     and the 2 s waits for nothing then (it stays, by Hans's choice, for
     a change of its own). The disk that comes later comes by `mount -a`:
     a stick passed through to Parallels after the VM started, or plugged
     into the Pi after boot. `fsd` now sends `netd` a one-way notice,
     `NETOP_ROOT_MOUNTED`, each time tree 0 mounts (`TO_NET` in its send
     mask for it; Hans chose `fsd` telling `netd` over `netd` asking and
     over the shell's `mount -a` telling it). `netd` takes it from `fsd`'s
     slot alone and, when its last load found nothing mounted, runs
     `load_auth` again, `/etc/net` last, replacing what it held, which had
     no identity to lose. `make test-net-config`'s fourth boot is the
     Parallels workflow on QEMU: the ESP on a read-only vvfat disk `fsd`
     cannot mount, the root with `/etc/net` on a USB stick that `mount -a`
     brings in, reached at the file's address after it and not at the
     default; and the other three boots must not reload.
     **Its review** (high, eight findings, five fixed). The reload's
     gate was `/etc/net`'s `NO_FS` alone, so a load whose identity read
     went unanswered (`fsd` mid-restart) and whose `/etc/net` was then
     read kept no identity for the boot; any of the load's retried reads
     unanswered now counts (`Auth::unanswered`, which replaces two
     statics). The notice taken by the drain inside a connection's send
     loop waited for the next wake to act; both drains now answer whether
     it came, and the reload follows each. `netd` refuses anything else
     from `fsd`'s slot, since the send mask cannot narrow `TO_NET` to one
     op, and `fsd` says in a line when the notice could not be sent (a
     full mailbox). And the late boot's "before `mount -a`, no HTTP
     server" check could not fail: it fetched `/etc/passwd`, which with
     nothing mounted no address could serve. It asks for any HTTP answer
     now, an error too, and must find one at the default address and none
     at the file's. Declined, with the reason: a reload's 2 s budget
     stalling the serve loop (only if the root is gone again by the time
     the notice is read), and connections opened at the default address
     before `mount -a` left to their own timeouts (seconds of a boot, at
     an address nothing on a real network can reach).
   - *Three parsers of a dotted address serve `netd`:* `netconf`'s,
     `clusterkeys::parse_ip` with the same rules, and `netd`'s own
     `parse_ip4`. One crate holding one is the change. **Done
     2026-11-19** (`netd/one-ip-parser`, #490): a crate of its own,
     `netaddr`, holds the one parser (`parse_ip`) and its formatter
     (`format_ip`); `netconf`, `clusterkeys` and the `clusterkey`
     tool, `netd`, the shell and `ping` call it. There were five parsers
     with four sets of rules, not three: `netd`'s took any digits a part
     and wrapped (`18446744073709551617.0.0.1` was `1.0.0.1`, and a
     `connect` port of 18446744073709551696 was 80), and the shell's and
     `ping`'s any leading zeros, which `mount -r`, `cpu` and `ping` now
     refuse past three digits (`shell-commands.md`). **Its review** found
     the first version's tests could not fail (`netconf`'s parser had
     always refused the wrapping input; the wrap was `netd`'s, and
     untested), and that it put the one parser in `/etc/net`'s crate, tying
     the cluster's identity files, the shell and `ping` to it; `netaddr`
     is below them all, with no re-export. `netd`'s decimals are read by
     `accounts::parse_dec`, the checked reader it already linked (a second
     review found the first fix had copied it into `netaddr`), whose new
     test of the wrap fails with wrapping arithmetic put back, as
     `netaddr`'s fails without the three-digit limit. `netd`'s `/net/ip` and the shell's
     mount listing format through `format_ip`. The shell's and `ulib`'s
     `parse_u64` stay theirs: on `&str`, already checked, and for every
     number a command takes. `test-net-config` (`ping`),
     `test-mount-points` (`mount -r`), `test-run-silent` (`cpu`) and
     `test-no-identity` (`/etc/cluster/authorized`) passed. No rig sent
     a `/net/tcp` `connect` until `make test-net-dial` (2026-11-22): one
     good dial, seven connects refused with nothing on the wire, an
     unreachable one and a second good dial. Its first run found `dial`
     holding its connection slot after every failed dial, three of which
     refused the next at its clone; `dial` closes it now (`fail_closed`).
     The 20-digit port does not fit `dial`'s 8-byte argument and stays a
     host test.
   - *The ext2 root's place in the image* is written out in three rigs
     now (`test-no-identity.py`, `test-net-config.py` and
     `mkext2.py`, which decides it).
   **On the board** the file is written once at the Pi's shell, as
   root (`echo address 192.168.1.50 > /etc/net`, then a `>>` for each
   further line), and read at the next boot. `make sdcard` stages the
   boot partition alone, so a file on the card's root stays.

   **The board, 2026-11-15, by Hans: two boots of build `2e9d0dc7f73a`,
   the Mac's adapter moved to `192.168.77.1`.**
   - *First boot, no file:* `netd: address 10.0.2.34/24, gateway
     10.0.2.2, dns 10.0.2.3 (no /etc/net: the defaults)`. The file was
     then written at the shell, three `echo` lines (address
     `192.168.77.2`, mask `255.255.255.0`, gateway `192.168.77.1`, no
     `dns`), read back with `cat`, and the board shut down.
   - *Second boot:* `netd: address 192.168.77.2/24, gateway
     192.168.77.1, dns none (from /etc/net)`. `ping 192.168.77.1` said
     `reply from 192.168.77.1`. The Mac's ARP table held `192.168.77.2`
     at the board's address.
   - *From the Mac, at the new address:* the 246 files under `include`
     and `man` by `curl`; 241 equal to the staged copies by SHA-256,
     827 KB, and the same 5 man pages differing as in step 7 (the
     card's root is older than the tree). `/etc/net` itself came back
     as written.
   - No `EXCEPTION`, no frame dropped, `en9` counting no input error.
     The firmware's indexes read receive 10, transmit 0 on both boots.
   **Not shown on the board:** a refused file, a ping off the subnet
   (the Mac routes nothing on), and a name resolved (no `dns` line).
   The first two are the rig's on QEMU.

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

   **Built 2026-11-16, and run on the board the same day: it works,
   after one change the board asked for** (the result is at the end of
   this step). The two items below are the first build's: since the
   change the ack masks as well as clears, and the first-interrupt line
   reads `first receive interrupt taken (INTID 189), and masked at the
   device until the ring is drained`.
   - *The crate:* `init` writes the receive ring's `DMA_MBUF_DONE_THRESH`
     to 1, Linux's default, so receive done is raised after every frame
     (U-Boot takes no interrupt and leaves the firmware's value). `start`,
     when it turns the receiver on, clears a stale receive done and
     unmasks that bit alone (`IRQ_RX_DONE`, bit 13 of INTRL2_0, Linux's
     `UMAC_IRQ_RXDMA_MBDONE`), before the receiver's enable.
     `ack_rx_interrupt` clears it. The model has the interrupt block
     (status, mask, clear; the line is status and not mask) and raises
     receive done by the threshold: **Linux's reading, not seen.** Five
     tests (94 in the crate). Controlled: with the unmask left out four
     failed, with the ack clearing bit 14 one, with the threshold left out
     five.
   - *The kernel:* `Declared` carries the `_CRS`'s INTID through `quiet`
     to the `Device`, `nic.rs`'s GENET arm reports it and acks through
     the device, and `main.rs`'s existing NIC path enables it at the GIC
     and wakes `netd` on it. The ack counts the interrupts; the first has
     a line (`first receive interrupt taken (INTID 189), and cleared at
     the device`) and the count rides on the received-counts line at 256
     frames and each 65536 (`; N receive interrupts taken`).
   - *On QEMU,* which has no GENET: `make test`, `test-netdump` and the
     six boots of `test-smp` pass.
   **The board's two boots, to run:** this build, and the control with the
   unmask left out. In each, the boot's `NIC receive interrupt enabled
   (GIC INTID 189)` line; then a hundred pings from the Mac
   (`ping -c 100 192.168.77.2`), which `netd` does not answer (it answers
   no echo request yet) but which arrive as a hundred frames, and fetches
   from its HTTP server to pass 256 frames. This build: the
   first-interrupt line, and the count at 256 frames at least a hundred.
   The control: no such line, the count 0, and the fetches still served,
   by the tick's poll. (The step's check above says the pings are
   answered; they cannot be until `netd` answers echo, so the fetches are
   the served traffic.) Then the soak: repeated fetches with no fault and
   the board still answering.

   **The board, 2026-11-16, by Hans: five boots.** The Mac sent 100 pings
   (`netd` answers no echo, so none was answered) and fetched `/etc/net`
   from the board's HTTP server until the 256-frame line came.
   - *First boot (build `a0a3a7116a0a`, receive done cleared by the ack
     alone): it works, with too many interrupts.* `NIC receive interrupt
     enabled (GIC INTID 189)`, then `first receive interrupt taken (INTID
     189), and cleared at the device`, and the board went on: the line is
     cleared, or the first frame would have hung it. At 256 frames, **304
     interrupts**. A soak of 300 fetches, all equal, no fault.
   - *Second and third boots (a diagnostic, `ec1587bf`, `d4023736`,
     removed in `bf9d343d`): every interrupt real, and more than one a
     frame.* Each ack counted receive done reading clear on entry (0), no
     frame waiting (0), and receive done still set after the clear (0);
     the third also said what the device itself had taken, from its
     producer index: **302 interrupts for 257 frames**, none discarded.
     So the stale re-delivery first suspected (an end of interrupt before
     the clear landed) was ruled out by the second boot, and the third
     showed receive done set again for frames already in the ring and not
     yet taken.
   - *The change (`ee9befa2`, `10c90971`), Linux's shape:* the ack masks
     receive done as well as clearing it (`bcmgenet_isr0` with
     `rx_ring16_int_disable`), and a drain that finds the ring empty
     rearms it: clear, look again, unmask only if nothing waits
     (`rearm_rx_interrupt`, called from `poll_frame`). The model now
     raises receive done again while frames wait, inferred from the third
     boot. Controlled on the host: without the mask two tests failed,
     without the second look one, without the clear before the unmask two.
   - *Fourth boot (build `10c909712371`, the change, the diagnostic still
     in): **158 interrupts for 260 frames**,* the three diagnostic counts
     0. A soak of 400 fetches, all equal, no fault.
   - *Fifth boot, the control (build `bf9d343dbc0e+dirty`, the unmask
     left out of `start`):* the GIC's line as before, no first-interrupt
     line, and at 256 frames **`0 receive interrupts taken`**; 270
     fetches served, by the tick's poll. (Its 256th frame came only after
     about 270 fetches, where the interrupt boots reached it after about
     80; not explained by anything the boot logged, and not part of the
     check.)
   **The check, as it stands:** the count is one interrupt for a frame or
   a burst of them, so it is not at least the frames the device took, and
   is expected below them (158 for 260 here) though nothing bounds it
   there: a line whose fall lagged the ack at the GIC would be delivered
   and counted once more. With the mask, "at least a hundred after a
   hundred pings" is met (158) but is not the property; the property is
   that the count is not 0 with the unmask and is 0 without it.
   - *The review of #475 (`7e84d248`, `ea22730f`), after the boots:* the
     masked state moved from a kernel atomic into `Genet`, so the host
     tests cover it, and the rearm writes nothing when no ack masked
     receive done or the receiver is off; `Nic::ack_interrupt` takes
     `&mut`, so the kernel's count is a plain field, and saturates. The
     contract in `nic.rs` now says the ack takes the line down and leaves
     when the next interrupt comes to the arm. Controlled on the host:
     each of the flag's three writes and the rearm's two guards taken
     out, and three of the step's constants made wrong, each failed a
     test (the start's write passed at first, and got its own test). The
     register writes the board ran are the same.
   - *Sixth boot, confirming the review (build `0d920489063b`):* the GIC's
     line, the first-interrupt line, and at 256 frames **155 receive
     interrupts taken**, none dropped; a hundred pings and three hundred
     fetches from the Mac, then 50 more fetches all equal, and no fault,
     restart or warning in the boot.
   - *The second review of #475 (`7977067d`, and a test after it):* the
     rearm moved from `poll_frame`'s drain to empty into `NET_WAIT`, just
     before the reader blocks (`Nic::rearm_interrupt`), so a `netd`
     restarted between its last frame and its next `NET_RECV` no longer
     waits with receive done masked. The ack reads the mask back after
     its two posted writes, with a `dsb` after the read in the kernel, so
     the GIC's end of interrupt comes after the device took the mask (the
     third review: not proof that the line's fall reached the GIC). `start`
     unmasks only when it turns the receiver on. The model holds receive
     done up while frames wait, whatever the driver reads. Controlled on
     the host: twelve mutations, each failing a test (the read-back moved
     before the writes passed at first: the test compared reads and
     writes apart, and now records their order). On the board the
     rearm's check is the count: without it every interrupt after the
     first would be masked for good, and the count would stay at 1.
   - *Seventh boot, confirming the second review (build
     `2ec4ff0b8973`):* the GIC's line, the first-interrupt line, and at
     256 frames **133 receive interrupts taken**, none dropped: well above
     the 1 a missing rearm would leave, so `NET_WAIT` rearms it. A hundred
     pings and three hundred fetches from the Mac, then 50 more all equal,
     and no fault, restart or warning in the boot. Fewer interrupts than
     the sixth boot's 155, which fits the rearm at the wait rather than
     at every drain to empty, from one boot each.
   - *The third review of #475 (`5a853848`):* `NET_WAIT` blocked without
     asking its wake condition (`block_current_and_switch` does not
     look; only the tick does), so a frame that came in before the wait
     waited a tick, and on the GENET with receive done still masked by
     a rearm that had found it. It now returns at once when the rearm
     finds a frame or the condition already holds (`tasks::ready_now`).
     That predates step 9 and holds for virtio-net too: on QEMU, 100
     HTTP fetches took **0.25 s with the check and 2.78 s without it**
     (0.26 s with it again). And `gic::enable_declared_spi` sets the
     SPI level-triggered in `GICD_ICFGR` before the enable and says what
     the GIC held: the ack's mask and the wait's rearm hold only for a
     level line, and the reset value is the implementation's. On QEMU
     both GIC versions held level already (GICv2 in
     `test-server-children`, GICv3 in a scratch boot with virtio-net that
     served 20 fetches); the board's line says what its firmware left.
   - *Eighth boot, confirming the third review (build `8956597d9b0c`):*
     `NIC receive interrupt enabled (GIC INTID 189, level-triggered; the
     GIC held level)`, so the Pi's firmware leaves it level and the write
     changed nothing there; the first-interrupt line, and at 256 frames
     **194 receive interrupts taken**, none dropped. More than the
     seventh boot's 133, which fits a `NET_WAIT` that now returns for a
     frame already waiting and so waits, and rearms, more often (one boot
     each). A hundred pings and three hundred fetches from the Mac, then
     50 more all equal (1.65 s; earlier boots were not timed), and no
     fault, restart or warning in the boot.

10. **Two boards.** Two Pi 4s on one switch, each with its address file
    and cluster keys.
    **Check:** `mount -r` of one from the other reads a file whole; a `cpu`
    run returns its output; a board with no key in the other's `authorized`
    is refused. Then the same from the Mac with `scripts/np9p_client.py`,
    the foreign observer the QEMU rigs use.
    Since 2026-11-19 `netd` answers an ICMP echo request
    (`answer_echo`, `make test-echo-reply`), so `ping` of a board from
    the Mac or from the other board is the first check of the wire. Step
    7's and step 9's notes that the Mac's pings went unanswered were
    true when they were written.
    On the board 2026-11-20 (`d05972c1edf6`, debug, boot 50): the Mac's
    pings answered, 10 of 10 twice and then 100 of 100 at 0.2 s apart
    while seven `ping 10.0.2.98` on the Pi each held `netd` in a wait of
    about a second (#493); about 1.1 ms, TTL 64. The overlap rests on
    when the Pi's pings were typed, and no control was run on the board.
    The two boards' output is the boot log's to solve
    (`roadmap-boot-log.md`), since one serial port reads one board.

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
