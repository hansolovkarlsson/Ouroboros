# Research: Redox OS and the Rust Pi-4 bring-up tutorials — what to borrow

Two outside resources, flagged for two different reasons:

- **[Redox OS](https://www.redox-os.org/)** — the mature Rust microkernel. A
  *design* reference: it has already shipped, in a hardened form, almost
  exactly the architecture Ouroboros is building toward, so the useful question
  is **which of its decisions resolve open questions already on our roadmap**.
- **[`rust-embedded/rust-raspberrypi-OS-tutorials`](https://github.com/rust-embedded/rust-raspberrypi-OS-tutorials)**
  — bare-metal Rust OS-dev tutorials for the Pi 3 / Pi 4. Not a design
  influence but a **bring-up cookbook**: when the Raspberry Pi 4 boards (on
  order — see [[project-physical-hardware-target]]) don't behave like QEMU
  `virt`, this is where the concrete BCM2711 register-level answers live.

This note is a companion to [`research-directions.md`](research-directions.md)
(the MINIX/Linux/Plan 9/Helix synthesis) — same forward-looking lens, two
systems that note didn't cover. Ouroboros's own state is drawn from
`CLAUDE.md`/`CHANGELOG.md`; the outside claims are sourced at the end.

**Sourcing caveat, up front (Redox):** the authoritative pages — the Redox Book
and `redox-os.org` release notes — block automated fetch (HTTP 403). The Redox
material below leans on search snippets of those same pages plus the
[DeepWiki mirror](https://deepwiki.com/redox-os/book) of the book, Wikipedia,
and the GitLab/GitHub repos. It's accurate in shape, but **double-check exact
API names** (`setrens`, the scheme packet formats, the current scheme list)
against the live book and the `redox_syscall` crate before building against
them. Wikipedia's snapshot knows 0.9.0 (Sept 2024) as the last tagged release;
the 2026 "This Month in Redox" posts show continued development past it.

---

## Part 1 — Redox OS: the closest cousin

### The surprising finding: Ouroboros already *is* a small Redox

The headline isn't a long list of things to copy — it's that the two
architectures **already agree** on nearly every load-bearing decision, arrived
at independently. That convergence is itself the most useful result: it says
the Ouroboros design is on a path a much larger project validated by shipping.

| Design decision | Redox | Ouroboros |
|---|---|---|
| Kernel model | True microkernel (~40k LOC); "anything that *can* be userspace *should* be" | Same philosophy; kernel keeps scheduling/IPC/MMU/block-device cell only |
| Filesystem | `redoxfs` **daemon** serving `file:` | `fsd` **server** (task 2), FSOP/9P over IPC |
| Network stack | `smolnetd` **daemon** (smoltcp) serving `tcp:`/`udp:` | `netd` **server** (task 4), hand-rolled ARP/IP/ICMP/TCP |
| Display/console | `vesad`/Orbital **userspace** | `cond` console **server** (task 3) |
| Driver crash | Component panic ≠ kernel panic; supervised restart | `supervisor.rs`: restart fsd/cond on crash **or** wedge |
| Uniform resource protocol | **Schemes** (`scheme:path`), one open/read/write/close ABI | **9P verb set** + per-task namespaces + `bind` |
| Per-process view | Each process has its own namespace + fd table | Each task has its own namespace (NS_SET/bind) |
| Kernel POSIX-ness | Kernel ABI "intentionally unstable and minimal"; **POSIX lives in userspace (relibc)** | Exactly the conclusion of [`posix-divergence-postmortem.md`](posix-divergence-postmortem.md): POSIX is a userland-libc personality, not a kernel property |
| Capability idea | Namespace visibility = what a process may name; `fd`s carry permissions | Per-slot IPC send-mask + runtime `delegate` |
| Driver ↔ kernel | Syscalls + `irq:`/`event:`/`memory:` schemes | Gated syscalls (BLOCK_*, NET_*, CON_*/FB_*) to one privileged task each |

Read down that table: the divergences are *flavor*, not *structure*. Redox
went x86-first and URL-flavored; Ouroboros went aarch64-native and
Plan-9-flavored. The genuinely different bet is the next section.

### Where Redox is ahead — the real adoption candidates

Five things Redox has built that Ouroboros hasn't, ranked by how much they'd
actually move the project (and mapped to where they already sit on the
roadmap):

**1. `relibc` — a userland C library (the biggest one).** relibc is a real C
standard library + POSIX layer *written in Rust*, and it's the mechanism that
lets Redox run genuine C/C++ programs **and** Rust `std` programs. Two lessons
specifically transferable to us:
- It's the **existence proof for the plan we already wrote** — the
  posix-divergence postmortem and `ROADMAP.md`'s portability section both say
  "C portability comes back as a userland libc, not a POSIX kernel." Redox
  proves that's not hand-waving: a non-POSIX Rust microkernel really does run C
  via a compat libc.
- **relibc targets both Redox and Linux.** On Linux it's a thin syscall
  wrapper; on Redox it calls `libredox`. That dual-target trick means you can
  develop and unit-test the libc on the host before the OS backend exists — a
  concrete de-risking move for whenever we start ours.
- Rust `std` on Redox has a dedicated `sys` backend and **statically links
  relibc** — i.e. the libc is the seam that unlocks the whole Rust ecosystem,
  not just C.

  → Roadmap home: the **POSIX / C-program portability** north-star. This is a
  multi-month arc, not a session; Redox is the reference implementation to read
  when we start it.

**2. Namespace *as* the capability boundary (the one clean idea to steal
now).** In Redox, sandboxing *is* restricting which schemes a process can name;
a daemon calls `setrens(0,0)` to drop into a **null namespace** after init,
leaving it only its pre-opened fds. Ouroboros has both halves already — per-task
namespaces (`bind`/NS_SET) **and** a capability send-mask — but they're separate
mechanisms, and an empty namespace today means "unchanged," not "no access."
Redox shows the unification: **make the namespace the capability set.** That
directly serves the security north-star (#1 in the recent batch:
login/permissions/sandboxing) and unifies two things we built piecemeal, which
is exactly the kind of move `research-directions.md` argued Plan 9 namespaces
were for.

  → Roadmap home: fold into the **security/permissions** north-star as its
  enforcement mechanism (namespace visibility = the sandbox), and note it as
  the "namespaces become security boundaries, not just convenience" step.

**3. RedoxFS's shape — CoW + checksums + transparent encryption, as a small
daemon.** RedoxFS is ZFS-*inspired* but written from scratch precisely because
porting ZFS fought the microkernel model (Redox tried a read-only ZFS driver
and abandoned it). It gives copy-on-write, **data *and* metadata checksums**,
atomic updates, snapshots, and AES full-disk encryption — and the bootloader
can load the kernel off an encrypted partition. Two of our north-stars point
straight at this:
- **Data redundancy / failsafe** (#7): checksums + CoW are the substrate a
  redundancy scheme needs; RedoxFS is the "don't port ZFS, write a small Rust
  CoW+checksum FS" model to follow rather than reaching for a giant on-disk
  format.
- **Login/encryption** (#1): "bootloader loads the kernel off an encrypted
  partition" is a concrete target for at-rest security.

  → Roadmap home: reference note under the **cluster-redundancy** and
  **security** north-stars. (Our existing fsd already proves the "FS as a
  daemon behind a uniform protocol" half.)

**4. fork/exec/signals pushed *into* userspace.** Redox moved `fork()` and
`execve()` **out of the kernel** into relibc/`redox-rt`: `fork` is `clone`
without `CLONE_VM`, threads are `clone` with it. Ouroboros deliberately chose
`spawn` over `fork` (posix-divergence: fork is the primitive a microkernel
can't cheaply honor). Redox doesn't refute that — it *confirms* it, and then
shows the escape hatch: if C portability eventually **demands** fork semantics,
they can be synthesized in the libc over a lower-level clone/spawn primitive
without putting fork back in the kernel. Worth recording as the answer to "but
C programs call fork()" when the libc arc starts.

  → Roadmap home: a design note attached to the portability north-star (not its
  own arc).

**5. The `irq:` + `event:` pattern (minor, but tidy).** Redox delivers both
hardware interrupts and fd-readiness as events through one epoll-like `event:`
scheme. Ouroboros already has the seed of this in `NET_WAIT` (blocks on
frames-*or*-messages, with an optional timeout) plus IRQ-driven RX. If we ever
generalize server event-loops, Redox's "one wait primitive for IRQs and
readiness alike" is the shape to copy rather than growing a second mechanism.

**Explicitly *not* adopting:**
- **smoltcp** (Redox's netstack crate). Our hand-rolled `netd` is a deliberate
  learning goal, and pulling a large crate into a PIE `aarch64-unknown-none`
  binary runs straight into the `R_AARCH64_ABS64` link ceiling
  ([[reference-str-slice-pie-trap]] is the small version of that same wall).
- **URL/scheme *syntax*.** We already committed to 9P/Plan 9 paths; switching to
  `scheme:path` spelling buys nothing. The transferable part of schemes is the
  *namespace-as-capability* idea (#2), not the punctuation.
- **Orbital/COSMIC GUI.** Far beyond current scope; noted only as where Redox's
  userland has gone (COSMIC Files/Editor/Terminal are now core to the Redox
  desktop as of 2026).

### Schemes vs. namespaces — the one real design comparison

Both systems answer the same question — *how does a small kernel expose every
resource (files, sockets, devices, IPC) without a bespoke syscall per resource?*
— and both answer it the Plan 9 way: a uniform namespace of named resources
behind one open/read/write/close verb set. Redox spells a resource
`scheme:path` (a URL) and calls the provider a "scheme"; Ouroboros spells it a
9P path and calls the provider a server. **These are the same architecture.**

The single place Redox went further, and the thing actually worth taking, is
that **it made the per-process namespace a security boundary**: what you can
open is exactly what your namespace lets you name, and you sandbox a component
by handing it a smaller namespace (down to the null namespace). Ouroboros has
the namespace and has a capability model but hasn't yet *joined* them. That
join is adoption candidate #2, and it's the most concrete, near-term thing this
whole comparison surfaces.

### The plan (Redox → roadmap)

Nothing here is a "do it this session" item; it's a set of confirmations and
references to attach to arcs the roadmap already names:

1. **When the C/portability arc starts** → read relibc first; steal its
   dual-target (Redox + Linux) structure so the libc is host-testable, and its
   userspace fork-over-clone technique. *(portability north-star)*
2. **When the security arc starts** → make the per-task namespace the capability
   boundary (Redox's null-namespace sandbox is the model), and look at RedoxFS's
   encrypted-partition boot for at-rest security. *(security north-star)*
3. **When the cluster-redundancy arc starts** → RedoxFS (CoW + data/metadata
   checksums, written small as a daemon) is the design reference, not ZFS.
   *(redundancy north-star)*
4. **Confirmation, no action** → our microkernel/userspace-driver/supervised-
   restart/uniform-protocol/non-POSIX-kernel decisions all match Redox. The
   architecture is sound; the gap to Redox is *maturity and userland breadth*
   (relibc, a package manager, hundreds of ported programs, a GUI), not
   structure.

A note on aarch64: **Redox treats aarch64 as a first-class target** (alongside
x86-64 and RISC-V), with an active 2026 ARM64 effort — April 2026 fixed FP
register corruption, PTE shareability, DeviceTree panics, and enabled the
BCM2835 storage driver in the ARM64 image, targeting the **Pi 3B+**. So Redox
is *also* a real reference for ARM-specific kernel bugs — if we hit FP-save or
page-table-shareability weirdness on real hardware, their commit log is worth a
search.

---

## Part 2 — Raspberry Pi 4 bring-up (the tutorials repo, for when we're stuck)

The repo runs on **both Pi 3 and Pi 4** (`BSP=rpi4 make`), and tutorials 1–5 are
QEMU-only groundwork with real hardware starting at tutorial 05. But the most
important framing for *us* is this: **Ouroboros boots via UEFI, and the
tutorials boot a raw `kernel8.img`.** Those are two different worlds, so read
the repo as a hardware-facts reference, not a boot-path to copy wholesale.

### Two boot routes onto a Pi 4 — and why we should try UEFI first

- **Raw firmware boot (what the tutorials do):** the SoC ROM → `start4.elf`
  firmware reads `config.txt`; with `arm_64bit=1` it loads **`kernel8.img`** to
  **`0x8_0000`** and jumps in at EL2/EL1. No UEFI, no boot services — you'd be
  giving up the entire existing Ouroboros boot path (UEFI GOP, ACPI discovery,
  the memory map from `exit_boot_services`).
- **UEFI boot (the [pftf/RPi4](https://github.com/pftf/RPi4) EDK2 port):** flash
  `RPI_EFI.fd` to a FAT32 (type 0xEF) SD/USB, and the Pi 4 exposes **UEFI +
  ACPI** — "ServerReady," the same environment Fedora/Windows-on-ARM use.
  **Our existing UEFI loader should run largely unchanged.**

  → **Recommendation: try the pftf/RPi4 UEFI route first.** It reuses the whole
  boot/discovery stack we already debugged on QEMU and Parallels. Fall back to
  raw `kernel8.img` (and the tutorials' boot chapters) only if UEFI proves
  unworkable. Known pftf caveats: a default **3 GB RAM cap** (BCM2711 DMA-bug
  workaround, raisable in the firmware menu) and **ACPI-not-devicetree** (fine
  for us — our discovery is already ACPI-first).

### What Ouroboros already has that should transfer to a Pi 4

This is the encouraging part — several subsystems we built for QEMU/Parallels
map straight onto the Pi 4 *if* we go UEFI+ACPI:

- **GIC discovery.** The Pi 4's interrupt controller is a **GIC-400 = GICv2**
  (GICD `0xFF84_1000`, GICC `0xFF84_2000`). We already have a **GICv2 backend
  (`gicv2.rs`) selected by a real ACPI MADT parse (`madt.rs`)** — so under
  pftf's ACPI, the MADT should describe the GIC-400 and our existing path may
  Just Work. (This is exactly the win the MADT/GICv3 refactor was for: not
  hardcoding QEMU addresses.)
- **Console.** Our discovery ladder is ACPI SPCR → GOP framebuffer. A Pi 4 on
  HDMI under UEFI should give a **GOP framebuffer**, which drops us onto the
  `fbconsole`/`cond` path we already use on Parallels. A PL011 may also be
  described via SPCR.
- **The no-fallback-address discipline** (`CLAUDE.md`: "there is no fallback
  UART address, and there should not be one again") is exactly right here — the
  Pi 4 peripheral base is **`0xFE00_0000`**, *not* QEMU's or Pi 3's
  `0x3F00_0000`; a hardcoded guess would fault silently.

### The raw-hardware cookbook (facts to keep at hand)

Independent of boot route, these are the BCM2711 specifics the tutorials and
Pi-4 bare-metal community pin down — the answers to "why is there no output":

- **Peripheral MMIO base:** `0xFE00_0000` (Pi 4 / BCM2711). Pi 3 was
  `0x3F00_0000`. Keep it parameterized, never hardcoded.
- **GIC-400 (GICv2):** Distributor `0xFF84_1000`, CPU interface `0xFF84_2000`.
  The GIC must be enabled by `start4.elf`; `enable_gic=0` in `config.txt` forces
  the legacy Broadcom controller (only useful for Pi-3-style code).
- **Serial: use PL011, not the mini-UART.** PL011's baud is independent of the
  core clock (stable); the mini-UART's baud tracks the VPU/CPU clock (drifts).
  The tutorial's `bcm2xxx_pl011_uart.rs` is directly reusable if we ever need a
  raw driver.
- **GPIO mux:** on the 40-pin header, **GPIO14 (TXD) / GPIO15 (RXD)** must be set
  to **ALT0 (FSEL `0b100`)**. Trap: by default the firmware wires PL011 to the
  on-board **Bluetooth** and puts the mini-UART on the header;
  `dtoverlay=disable-bt` (or setting the alt-function yourself in bare metal)
  reassigns PL011 to the pins.
- **The physical serial rig (first thing to wire up):** USB-serial adapter to
  **GPIO14 (TX), GPIO15 (RX), GND — do NOT connect VCC/power** — power the Pi
  from its own supply. 115200 baud, host `/dev/ttyUSB0` (varies).

### Bring-up order and the classic QEMU→real-Pi-4 traps

- **QEMU ≠ Pi 4**, especially for IRQs. The repo itself says tutorials 1–5 "only
  make sense in QEMU"; interrupt behavior and the GIC are where emulation and
  silicon diverge most. (We already know this shape from the Parallels work.)
- **Interrupt-controller mismatch is *the* Pi-4 trap:** Pi-3 legacy-controller
  code won't drive the GIC-400. We're fine here — we have the GICv2 path — but
  it's the #1 thing that bites people.
- **MMU/cache ordering:** the repo spends four chapters (14–16 + groundwork) on
  translation-table setup and cache/MMU enable *order*; doing it out of order is
  a common silent hang on real hardware. Our `mmu.rs` already learned this lesson
  the hard way (the L0-vs-L1 starting-level bug), so we have the scar tissue.
- **Chainboot (raw-boot only):** if we ever go raw `kernel8.img`, the repo's
  **MiniLoad** chainloader (tutorial 06) pushes each new kernel over serial to
  `0x8_0000` so you stop swapping SD cards — and it demonstrates the
  "relocate yourself out of the load address" trick (loads at `0x8_0000`,
  relocates to `0x200_0000`, receives the new kernel, jumps). Under UEFI we
  don't need this (boot from USB/SD via firmware), but it's the reference if we
  do.

### The tutorials as a "when stuck, read chapter N" map

| If stuck on… | Read tutorial |
|---|---|
| First serial output on real HW (PL011 + GPIO) | **05** `drivers_gpio_uart` |
| Fast iterate without SD swaps (raw boot) | **06** `uart_chainloader` |
| Generic timer / timestamps | **07** `timestamps` |
| EL2→EL1 drop, privilege modes | **09** `privilege_level` |
| Exception vector table groundwork | **11** `exceptions_part1` |
| **Peripheral IRQs — the Pi 3 vs Pi 4 GIC split** | **13** `exceptions_part2_peripheral_IRQs` |
| MMU + MMIO remap / page-table setup | **14–16** `virtual_mem_part2/3/4` |
| Heap allocator | **19** `kernel_heap` |
| Timer-interrupt callbacks | **20** `timer_callbacks` |

---

## Sources

**Redox OS** — [Redox Book](https://doc.redox-os.org/book/) ·
[Microkernels](https://doc.redox-os.org/book/microkernels.html) ·
[How Redox Compares](https://doc.redox-os.org/book/how-redox-compares.html) ·
[DeepWiki: Microkernel Design](https://deepwiki.com/redox-os/book/3.1-microkernel-design) ·
[DeepWiki: Architecture Overview](https://deepwiki.com/redox-os/book/1.2-architecture-overview) ·
[aarch64 build page](https://doc.redox-os.org/book/aarch64.html) ·
[kernel ARM64 port outline](https://github.com/redox-os/kernel/blob/master/ARM-AARCH64-PORT-OUTLINE.md) ·
[RSoC: Porting to AArch64](https://www.redox-os.org/news/rsoc-arm64-0x01/) ·
[This Month in Redox — Apr 2026](https://www.redox-os.org/news/this-month-260430/) ·
[relibc README](https://github.com/redox-os/relibc/blob/master/README.md) ·
[RedoxFS](https://doc.redox-os.org/book/redoxfs.html) ·
[netstack repo](https://github.com/redox-os/netstack) ·
[drivers repo](https://github.com/redox-os/drivers) ·
[Boot Process](https://doc.redox-os.org/book/boot-process.html) ·
[Release 0.9.0](https://www.redox-os.org/news/release-0.9.0/) ·
[Wikipedia: Redox OS](https://en.wikipedia.org/wiki/RedoxOS) ·
[LWN: Redox in Rust](https://lwn.net/Articles/979524/)

**Raspberry Pi 4 bring-up** —
[rust-raspberrypi-OS-tutorials](https://github.com/rust-embedded/rust-raspberrypi-OS-tutorials) ·
[chainloader chapter](https://github.com/rust-embedded/rust-raspberrypi-OS-tutorials/tree/master/06_uart_chainloader) ·
[PL011 driver source](https://github.com/rust-embedded/rust-raspberrypi-OS-tutorials/blob/master/05_drivers_gpio_uart/src/bsp/device_driver/bcm/bcm2xxx_pl011_uart.rs) ·
[UART-on-Pi4 issue](https://github.com/rust-embedded/rust-raspberrypi-OS-tutorials/issues/36) ·
[GIC-400 on Pi 4 (forum)](https://forums.raspberrypi.com/viewtopic.php?t=264096) ·
[mini-UART vs PL011](https://www.rpi4os.com/part4-miniuart/) ·
[config.txt / kernel8.img / arm_64bit](https://www.raspberrypi.com/documentation/computers/config_txt.html) ·
[TF-A on Pi 4](https://trustedfirmware-a.readthedocs.io/en/latest/plat/rpi4.html) ·
[pftf/RPi4 UEFI](https://github.com/pftf/RPi4) · [rpi4-uefi.dev](https://rpi4-uefi.dev/)
