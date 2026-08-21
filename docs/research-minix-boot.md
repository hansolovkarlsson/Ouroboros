# Research: MINIX's boot process, compared to Ouroboros's

A comparative research note, not reference documentation for our own
system (see [`architecture.md`](architecture.md) for that) — MINIX is one
of this project's stated influences (`notes.txt`: "draw ideas from Linux,
Minix, and Plan 9"), so this looks at how a real, decades-mature
microkernel actually boots and starts its first processes, and where
Ouroboros's current boot sequence already resembles or diverges from that,
deliberately or not. Sourced from MINIX's own documentation and source
tree (linked throughout, not recalled from memory), not to be treated as
exhaustive MINIX internals documentation.

## MINIX's boot sequence (x86, the canonical platform)

1. **BIOS reads the first sector of the boot device** and executes it —
   ordinary MBR-style boot, no UEFI involved. That bootstrap code loads
   `/boot/boot`, **the MINIX Boot Monitor** — MINIX's own small
   boot-loading program, not a third-party bootloader.
2. **The Boot Monitor loads `/boot/image`** (or the newest file, if it's a
   directory) — a **multi-part file containing several separate programs**
   packed together: the kernel, and the initial set of user-space servers
   (see below). The monitor also reads boot environment variables
   (colon/comma-separated) that configure driver behavior at boot time —
   e.g. `DPETH0 = 300:10` for an Ethernet driver's I/O address and IRQ.
3. **The kernel starts**, initializing itself and its own kernel-resident
   tasks — **CLOCK** and **SYSTEM**, plus IPC — before anything in user
   space runs. These kernel tasks are invisible to normal processes; they
   exist to keep certain latency- or privilege-sensitive work inside the
   kernel without making the kernel itself do everything.
4. **The kernel starts PM (Process Manager) and VFS (Virtual File
   System)** — the first two user-space processes. **PM gets PID 0** and
   is genuinely the first thing running outside the kernel. Both
   initialize themselves as far as they can before the rest of the boot
   image exists to talk to.
5. **RS (the Reincarnation Server) becomes parent of every other boot-image
   process**, including **init**, and is what gives MINIX its
   fault-tolerance story: RS can restart a crashed driver or server
   without restarting the whole system, because drivers/servers were
   never part of the trusted kernel to begin with.
6. **init runs `/etc/rc`**, a shell script that starts everything that
   *wasn't* in the boot image — PCI enumeration, disk controllers, the
   keyboard driver, networking (`inet`/`lwip`), `procfs`, logging, and so
   on — before finally starting login on the configured terminals.

So the boot image is deliberately minimal (kernel + CLOCK + SYSTEM + IPC,
then PM + VFS + RS + a RAM disk driver + VM), and *everything else* —
every real device driver, the TCP/IP stack, procfs — is a separate,
independently-restartable user-space process started later by `/etc/rc`,
not compiled into the kernel at all.

## MINIX's ARM port: a genuinely different boot chain, worth noting given this project's target

MINIX's canonical platform is x86; its ARM port (BeagleBoard-xM,
BeagleBone/BeagleBone Black, TI Cortex-A8 SoCs) boots very differently
from the story above, and closer to what a lot of embedded ARM Linux
boards do: **U-Boot**, a third-party bootloader, loads the MINIX kernel at
a **fixed, hardcoded memory address** (`0x80200000`, per MINIX's own ARM
port documentation) and jumps to it directly — there is no MINIX-authored
boot monitor on ARM the way there is on x86.

Two points from MINIX's own project mailing list are worth recording
directly, since they're a real admission from the MINIX developers, not
speculation:

- **"There is no UEFI support whatsoever; MINIX 3 for now expects an IBM
  PC compatible computer on i386. On ARM everything is pretty much
  hard-coded right now."**
- Some EFI-adjacent tooling exists only on the x86 side (an optional GRUB2
  EFI FAT32 partition in the disk-image build scripts), unrelated to the
  ARM port.

## Ouroboros's boot sequence, for direct comparison

See [`architecture.md`](architecture.md#boot-flow) for the authoritative
version; summarized here in the same shape as MINIX's sequence above for
side-by-side reading:

1. **UEFI firmware loads the kernel directly** as a UEFI application
   (`\EFI\BOOT\BOOTAA64.EFI`) — no MBR, no boot sector, no separate
   boot-monitor stage. This is a deliberate choice specific to this
   project's primary hardware target (Parallels on Apple Silicon), which
   boots ARM VMs exclusively through UEFI firmware with no direct-kernel
   shortcut — see `CLAUDE.md`'s "Boot architecture" section.
2. **The kernel does its own console discovery** (devicetree → ACPI/SPCR →
   PCI) and **loads its one userland program** (the shell, per a config
   file) — both still using UEFI boot services, since this is the same
   pre-`exit_boot_services` window MINIX's boot monitor would occupy, just
   without a separate program doing it.
3. **`exit_boot_services`** — the one-way transition out of the
   UEFI/boot-services world. Nothing before this point resembles a
   "kernel"; everything after it does.
4. **The kernel installs its own exception vectors, then its own MMU
   identity map**, replacing firmware's page tables — there is no
   equivalent step in MINIX's boot sequence above, because x86 real/
   protected-mode boot and UEFI's already-paged environment are different
   starting points; this is specific to arriving at EL1 with firmware's
   MMU already active.
5. **GIC + timer init**, then **the kernel builds its task contexts and
   drops to EL0** — one loaded shell process, one idle task. No process
   manager, no VFS, no reincarnation server, no boot image of multiple
   programs — one program.

## Side-by-side

| | MINIX (x86) | MINIX (ARM) | Ouroboros |
|---|---|---|---|
| Firmware/bootloader | BIOS → MBR → MINIX's own Boot Monitor | U-Boot (third-party) | UEFI firmware directly, no separate bootloader stage |
| Kernel image format | One multi-part `/boot/image` file (kernel + initial servers packed together) | Single kernel image at a fixed load address | Single UEFI PE/COFF application |
| First code to run | Boot Monitor (MINIX-authored) | U-Boot (not MINIX-authored) | The kernel's own `#[entry]`, immediately |
| Load address | Determined by the Boot Monitor | Hardcoded (`0x80200000`) | Wherever UEFI firmware places it (queried, not assumed) |
| First user-space process | PM (Process Manager), PID 0 | same | The loaded shell (task 0) |
| Initial process count | Kernel + CLOCK + SYSTEM + IPC, then PM + VFS + RS + more (a whole boot image) | same design, x86 boot image | Kernel (EL1) + one loaded program + one idle task |
| Where drivers/FS live | User-space servers, supervised and independently restartable by RS | same | Inside the EL1 kernel (console drivers, MMU, scheduler all kernel-resident) |
| IPC model | Synchronous message passing (`SEND`/`RECEIVE`/`SENDREC`, fixed 64-byte messages, `_syscall()` is implemented as `sendrec` to a server) | same | Direct `svc`-based syscalls, number in `x8`, one kernel-side dispatch table |
| Fault isolation of "OS" components | Strong — a crashed driver is a crashed *process*, restarted by RS | same | None yet — a bug in a console driver or the scheduler takes down the whole kernel |

## What this says about Ouroboros's current shape

**Update (2026-08-21): this section describes an earlier, more monolithic
Ouroboros — kept for history, corrected here.** Since it was written, the
FAT32 filesystem (`fsd`) and the console (`cond`) have moved out of the EL1
kernel into supervised, MMU-isolated EL0 servers reached over
`sendrec`-shaped IPC (`MSG_CALL`) — a real process/server boundary now
exists, with `grant`/`safecopy` capability copies (MINIX's `sys_safecopy`),
a capability send-mask, and a reincarnation-server-style restart layer. The
gap versus MINIX is now *breadth* (two servers, not a full fleet; no VFS/PM;
no process trees), not the absence of the model. See
[`research-directions.md`](research-directions.md) for the current
synthesis and where the influences point next. The original text follows.

Worth being honest about, not just cataloguing differences: `notes.txt`
states a microkernel goal, but the *current* implementation is
considerably more monolithic than either MINIX platform. MMU management,
exception handling, console drivers, and scheduling all run as
undifferentiated kernel code at EL1 — there is no process/server boundary
inside the kernel at all yet, because there's only ever been one thing to
schedule that wasn't a kernel-internal demo. MINIX's boot sequence is a
concrete, working existence proof of what "more in user space, less in
the kernel" actually looks like operationally: a small typed set of
programs (PM, VFS, RS, drivers) started in a specific dependency order,
each independently replaceable and restartable, rather than one process
with everything else still inside the privileged kernel.

That gap is expected at this stage, not a defect — Ouroboros is still
proving out its boot chain, MMU, and scheduler, and pushing drivers to
user space presupposes things this project doesn't have yet: more than
two tasks, dynamic task creation, and some IPC primitive richer than "call
a fixed kernel function by number." It's a real reference point for later,
though, in the same way this project already treats Linux/Plan 9 as
influences without imitating them outright.

## Concrete patterns worth revisiting once the prerequisites exist

Not commitments, just noted parallels between where MINIX's design landed
and where this project's own [`roadmap.md`](roadmap.md) is already headed:

- **A "boot image" of more than one program**, the way MINIX packs
  kernel+PM+VFS+RS together, is a natural generalization of `loader.rs`
  once dynamic task creation exists (currently: exactly one loaded
  program, hardcoded task count of two) — config could name a *list* of
  programs to start at boot instead of just one shell.
- **A supervisor/restart process (MINIX's RS)** is a reasonable shape for
  whatever eventually manages more than one loaded process — restart a
  crashed task instead of taking the whole system down, which today isn't
  possible even in principle (there's no fault isolation between EL1
  kernel bugs and anything else).
- **Message-passing IPC vs. direct syscalls** is a real, larger design
  fork worth deliberately deciding rather than drifting into — MINIX's
  choice buys stronger isolation (a server only ever sees messages, never
  another process's memory) at the cost of a heavier call path than a
  direct `svc`. Not a phase-3 concern (disk commands need file-read
  syscalls, not a new IPC model), but worth flagging before this project
  has enough userland processes that the syscall dispatch table's shape
  becomes load-bearing rather than incidental.
- **MINIX's ARM boot chain (U-Boot, fixed load address, "everything
  hardcoded") is the cautionary comparison, not the aspirational one** —
  it's a reminder that this project's UEFI-native, address-discovered
  approach (no hardcoded PL011 address, no hardcoded load address, RAM
  span read from the real UEFI memory map) is already ahead of what
  MINIX's own ARM port does, for exactly the reasons `CLAUDE.md`
  documents repeatedly (a hardcoded QEMU address once hard-crashed real
  Parallels hardware). Worth remembering the next time "just hardcode it
  for now" looks tempting.

## Sources

- [MINIX 3 `boot(8)` manual page](https://man.minix3.org/cgi-bin/man.cgi?query=boot&sektion=8&apropos=0&manpath=Minix+3.1.5) — Boot Monitor behavior, `/boot/image` loading, boot environment variables.
- [MINIX 3 wiki: Overview of MINIX3 servers and drivers](https://wiki.minix3.org/doku.php?id=developersguide:overviewofminixservers) — boot image contents, server start order, `/etc/rc`.
- [MINIX 3 wiki: Message Passing](https://wiki.minix3.org/doku.php?id=developersguide:messagepassing) — `SEND`/`RECEIVE`/`SENDREC`/`NOTIFY` primitives, message format, `_syscall()`.
- [MINIX 3 wiki: MINIX on ARM](https://wiki.minix3.org/doku.php?id=developersguide:minixonarm) — U-Boot-based ARM boot chain, fixed load address.
- [`Stichting-MINIX-Research-Foundation/minix`, `servers/pm/main.c`](https://github.com/Stichting-MINIX-Research-Foundation/minix/blob/master/minix/servers/pm/main.c) — PM's own startup code.
- [minix3 Google Group: "Minix and UEFI, ACPI"](https://groups.google.com/g/minix3/c/wJTzqSaa-3c) — direct developer statement on the absence of UEFI support and ARM's hardcoded boot configuration.
- [minix3 Google Group: "Porting Minix to new embedded ARM target"](https://groups.google.com/g/minix3/c/jXidVQjRmAE/m/NPxbg5MsQssJ) — ARM port target hardware (BeagleBoard-xM/BeagleBone, Cortex-A8).
