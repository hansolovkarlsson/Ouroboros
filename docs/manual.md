# Ouroboros manual

The one-stop reference for using Ouroboros: building it, running it,
using the shell and the `/bin` commands, **sharing resources across a
cluster of machines**, and the syscall ABI. Each section links to the
deeper reference where one exists — [`testing-qemu.md`](testing-qemu.md)
for the full QEMU run/test guide (single machine *and* the two-node
cluster), [`architecture.md`](architecture.md) for how the kernel works,
[`processes.md`](processes.md) for the userland program model,
[`shell-commands.md`](shell-commands.md) for the builtin command
reference, [`roadmap.md`](roadmap.md) / [`roadmap-cluster.md`](roadmap-cluster.md)
for what's next, and `CLAUDE.md` at the repository root for the debugging
history behind every design decision. To build an OS like this yourself,
[`tutorial.md`](tutorial.md) is the staged from-scratch guide.

**What Ouroboros is, in one line:** an ARM64 microkernel OS whose
filesystem, console, and network are userland servers reached over a
uniform file protocol — which is exactly what lets several machines share
their disks, processes, consoles, and even *run programs on each other*
over the network (the [cluster](#cluster-sharing-resources-across-machines)
section below).

## Prerequisites

- **Rust** — pinned by `rust-toolchain.toml` (stable channel; the
  `aarch64-unknown-uefi` and `aarch64-unknown-none` targets and the
  `llvm-tools` component install automatically on first build). No
  nightly, no `-Z build-std`.
- **QEMU** — `brew install qemu`, which also provides the aarch64 OVMF
  firmware the run targets point at.
- **macOS's `hdiutil`** — used by `make image` to build the FAT32 disk
  image.
- **Parallels Desktop** (optional, for real-hardware runs) — provides
  `prl_disk_tool` (used by `make parallels-hdd`) and `prlctl` (used by
  `make test-parallels`).

## Building

```sh
make build                  # kernel only (debug profile)
make build PROFILE=release  # kernel, release profile
make shell-bin              # the default shell (userland)
make hello-bin              # the demo second program (userland)
make esp                    # stage the full ESP directory layout
```

The workspace has four crates: `kernel` (a UEFI application,
`aarch64-unknown-uefi`), `shell` and `hello` (userland programs,
`aarch64-unknown-none`), and `syscall-abi` (the shared constants both
sides import, so syscall numbers can never silently drift). Userland
programs **always build `--release`** regardless of `PROFILE` — a hard
toolchain constraint of the position-independent build, not a
preference (a debug build fails to link; see `processes.md`'s "Binary
format").

The staged ESP layout:

```
\EFI\BOOT\BOOTAA64.EFI   the kernel (UEFI auto-boots this path)
\EFI\ORBS\SH.BIN         the default shell
\EFI\ORBS\HELLO.BIN      the demo second program
\EFI\ORBS\INIT.CFG       one line: the path of the program to boot into
```

Swap the boot program by editing `INIT.CFG` — no kernel rebuild needed.
(`ORBS`, not the full project name: FAT's 8.3 short-name limit, which
the runtime FAT32 reader doesn't parse long-filename entries around.)

## Running on QEMU

The everyday commands:

```sh
make run             # fastest loop - vvfat FAT16 disk (no real filesystem)
make run-image       # boots build/esp.img - real FAT32; disk commands work
make run-image-net   # real FAT32 + a NIC - the fullest single-machine run
```

The one everyday gotcha: **`make run`'s disk is FAT16** (a QEMU vvfat
artifact), which the FAT32-only filesystem server can't mount — every disk
command prints "no filesystem mounted" there. Use **`make run-image`**
whenever you want `ls`/`cat`/`write`/`exec` to actually work. Exit QEMU
with **`Ctrl+a x`**.

That's enough to get a working shell. The **full run/test guide** — every
`make run-*` target, the exFAT/ext2/GPT test disks, the network runs, the
9P host peers, and the **two-node cluster** setup — is
[`testing-qemu.md`](testing-qemu.md). You'll want it for the
[cluster](#cluster-sharing-resources-across-machines) section below.

## Running on Parallels (real hardware)

```sh
make image           # build/esp.img - raw MBR+FAT32 disk image
make parallels-hdd   # wraps it into build/esp.hdd via prl_disk_tool
```

Attach `build/esp.hdd` as the VM's **Hard Disk** device. Two confirmed traps
(see `CLAUDE.md`'s "Parallels disk attachment"): Parallels rejects raw
`.img` files on the Hard Disk device, and attaching the image to the
CD/DVD device *looks* like it works but doesn't (the optical driver
wants ISO9660). Also: `build/esp.hdd` stores a *pointer* to `build/esp.dmg`'s
absolute path, so the two files must stay together.

What works on real Parallels hardware today: the GOP framebuffer
console, a real USB keyboard (including Shift and Ctrl chords),
preemptive multitasking, the whole shell — **and real disk access via
a passed-through USB stick** (Parallels exposes no storage controller
this kernel can drive, but USB mass storage over the xHCI driver
works: pass a USB 3.x stick through to the VM, boot, wait a few
seconds for it to attach, and type `mount` — a FAT32-formatted stick
then serves the full disk-command surface, `exec` included; USB 2.0
sticks land on the EHCI controller instead and can't be used).

### Scripted real-hardware testing

```sh
make test-parallels CMDS="help;ls;uptime" [BOOT_WAIT=12] [VM_NAME=Ouroboros]
```

Rebuilds `build/esp.hdd`, boots the registered VM headlessly, types each
`;`-separated command via `prlctl send-key-event`, and saves a
screenshot after each into a `parallels-test-<timestamp>/` directory
(gitignored — delete it when done). Supports `>` (a held-Shift chord)
and a `CTRL-C` pseudo-command (a held-Ctrl chord, no Enter). Requires
the VM registered in Parallels with its Hard Disk pointed at this
repo's `build/esp.hdd`.

## Using the shell

Full builtin reference: [`shell-commands.md`](shell-commands.md). The
short tour (boot `make run-image` so the disk is mountable):

```
$ help                          # list the builtins
$ ls /                          # BIN/  EFI/
$ ls /bin                       # the standalone commands (see below)
$ write notes.txt hello world   # create/replace a file's contents
$ cat notes.txt
$ echo backup >> log.txt        # output redirection: > replace, >> append
$ cat log.txt | grep back | wc  # multi-stage pipelines
$ mkdir docs ; mv notes.txt docs
$ cd docs ; pwd
$ ps                            # one line per task slot: state + name
$ set PATH=/bin ; env           # a real environment; $VAR expansion
```

### Commands come from two places

- **Builtins** run inside the shell itself — a deliberately minimal set:
  `help  cd  bind  mount  unmount  erase  partition  format
  exec  exit  shutdown  halt  ps  kill  fg  wait  env  set  unset  cpu`.
  Job control (`ps`/`kill`/`fg`/`wait`/`exec`), the disk-management trio
  (`erase`/`partition`/`format` — they must run when *nothing* is mounted,
  exactly when `/bin` can't be read), power control (`shutdown`/`halt` — same
  no-disk reasoning), the environment (`env`/`set`/`unset`) and
  cwd/namespace (`cd`/`bind`) that *are* the shell's state, the mount commands,
  and `cpu` (remote execution) are builtins for reasons the cluster section and
  [`shell-commands.md`](shell-commands.md) explain. Everything else lives in
  `/bin`.

- **`/bin` programs** are real standalone binaries loaded from disk, found
  on `$PATH` (default `/bin`), spawned with arguments, and reaped:
  `ls  tree  cat  cp  mv  mkdir  rmdir  touch  rm  write  writeat  more` (files;
  `more`/`less` is the pager),
  `echo  pwd  uptime  clear  args  send  recv  selftest` (basics/diagnostics),
  `grep  wc  head  tail  nl  rev  uniq  upper` (pipeline filters),
  `ping  resolve  fetch` (network). You type them the same way (`ls`,
  `cat x`); the shell finds `/bin/LS` on PATH (FAT is case-insensitive).
  A `/bin` command resolves relative paths against the shell's working
  directory, delivered to it at spawn.

**Usage help:** append `-?` to any command that takes arguments — builtin or
`/bin` — for a one-line usage reminder (`ls -?`, `mount -?`, `dial -?`).

**Output redirection & pipelines** work across both:
`echo hi > f.txt` (replace), `>> f.txt` (append), and `a | b | c`
(each stage a separate program; the filters
`grep`/`wc`/`head`/`tail`/`nl`/`rev`/`uniq`/`upper` are built to chain).
See [`shell-commands.md`](shell-commands.md).

### Spawning, job control, and IPC demos

```
$ exec /bin/LS /             # exec spawns a program explicitly (like typing it)
$ exec /EFI/ORBS/SH.BIN      # spawn a second shell...
$ fg 5                       # ...and hand it the keyboard (a nested session)
$ exit                       # (in the nested shell) hand it back
$ kill 5                     # destroy a task
$ exec /EFI/ORBS/PONG.BIN    # the IPC echo-server demo...
$ send 5 hello ; recv        # ...send it a message and read its echo
```

Job-control notes: one task owns the keyboard at a time (`fg` moves it;
it returns to the boot shell when the owner dies, or on **Ctrl+C**, which
reclaims the keyboard without killing anything). An exited task holds its
slot as a zombie until `wait`ed (`ps` shows it); `kill` reaps immediately.
Ctrl+C also interrupts a stuck `wait`.

### Disks and mounts

```
$ mount                      # show what's mounted (format, partition, capacity)
$ mount -a                   # mount the kernel's block device (or rescan USB)
$ mount 1 /mnt/f             # mount the disk's 2nd partition at /mnt/f (multi-mount)
$ mount -p /proc             # the process table as files (see the cluster section)
$ mount -n /net              # this machine's network identity (ip, mac)
$ mount -c /dev/cons         # the console as a writable file
$ bind /work /EFI/ORBS       # /work now resolves to /EFI/ORBS, for this shell
$ unmount                    # drop the mounted filesystem
```

Every mount/`bind` changes **only this shell's** namespace (and the
commands it spawns inherit it) — a per-task view, never a global one.
Preparing a blank disk from inside the guest: `erase disk`, then
`partition fat32` (or `exfat`/`ext2`), then `format fat32`, then
`mount -a`. `mount -r`, and remote `/proc`/`/net`/`/dev/cons`, are the
[cluster](#cluster-sharing-resources-across-machines) section.

## Cluster: sharing resources across machines

This is what Ouroboros is ultimately about: **several machines sharing
their resources as one system** — one machine reads another's disk,
inspects its processes, writes its screen, or *runs a program on it* — over
a from-scratch 9P-style protocol carried on the network. It's the Plan 9
model: *"remote" is just the same file protocol over TCP instead of local
IPC*, so the commands you already know (`ls`, `cat`, `mkdir`, `cpu`) work
across machines with no new syntax — only *where a path points* changes.

**Trust, stated plainly:** the export is authenticated with a **shared cluster
secret** (since v0.10.0 — the export-hardening phase). Every machine reads the
same key from `\CLUSTER.KEY` on its boot disk; a request is signed with an HMAC
so the secret never crosses the wire, and a peer without the key gets nothing
(fail-closed). So `mount -r` and `cpu` work between machines that share the key,
and are refused between machines that don't. Since v0.13.0 the exchange is
**mutually authenticated** — replies are MAC'd too, so a client rejects a forged
reply — but this is **integrity, not secrecy**: bytes still cross the wire in
cleartext. This is **cluster-membership** auth, not per-peer identity, and it
assumes a **trusted LAN** for the parts still deferred (a passive sniffer reads
your files and can replay an observed request; encryption and per-peer identity
are gated behind a "leaving a trusted network" trigger on the roadmap). Two knobs: no `\CLUSTER.KEY` = the export is closed
entirely; a `\NOEXEC` flag file = the machine shares its disk but refuses remote
`cpu` execution. Don't expose an Ouroboros export to a genuinely hostile network
yet — per-peer auth, encryption, and replay protection are named next steps.

**Config files (both optional, on the boot disk root), read by `netd` at boot:**

| File | Effect |
| --- | --- |
| `\CLUSTER.KEY` | The shared cluster secret. All machines in a cluster need the **same** contents. Absent ⇒ the export is closed to every remote peer (fail-closed). |
| `\NOEXEC` | Presence-only flag: authenticated peers may still `mount -r` the disk, but every `cpu` (remote-exec) is refused. |

In the QEMU images a dev `\CLUSTER.KEY` is staged automatically (the Makefile's
`CLUSTER_KEY`, default `ouroboros-dev-cluster-key-v1`), so the two-VM targets
authenticate out of the box. To set your own on a running machine, just write the
file — `write /CLUSTER.KEY my-secret` (then reboot so `netd` re-reads it), and
`touch /NOEXEC` to enable the no-exec lever.

### Setting up two machines

Any two Ouroboros machines on the same network can do this. The dev setup
is two QEMU guests on a shared virtual link — **see
[`testing-qemu.md`](testing-qemu.md) §5** for the exact commands
(`make run-image-2vm-a` in one terminal, `make run-image-2vm-b` in
another). In that setup machine **A is 10.0.2.10** and **B is 10.0.2.11**;
each `netd` runs a 9P **export** on TCP port **564** automatically, so every
machine is ready to share the moment it boots. A machine reads its own
address with `mount -n /net ; cat /net/ip`.

Everything below is typed at one machine's shell and reaches the other over
the network. The examples run on **machine B**, reaching **machine A**
(`10.0.2.10`).

### Sharing a disk — `mount -r`

Mount another machine's exported filesystem into your namespace, then read
(and write) it like any local path:

```
$ mount -r 10.0.2.10:564 /mnt/a     # bind A's export at /mnt/a (needs the shared cluster key)
$ ls /mnt/a                         # A's disk root
$ cat /mnt/a/EFI/ORBS/INIT.CFG      # read a file on A
$ mkdir /mnt/a/reports              # create on A's disk
$ write /mnt/a/reports/note.txt hi  # write to A's disk
$ cp /BIN/LS /mnt/a/ls.copy         # stream a local file onto A (chunked)
```

The mount is **per-shell** and inherited by spawned commands. Read/write
both work (single-writer: one machine writes a given tree at a time, by
convention — no distributed lock yet). If A disconnects, the next operation
on `/mnt/a` fails cleanly rather than hanging.

### Another machine's processes — remote `/proc`

`/proc` is a synthetic view of a machine's task table as files. Mount your
own with `mount -p /proc`; read *another* machine's straight off its remote
mount (no bind needed — `netd`'s export serves `/proc` too):

```
$ mount -p /proc                    # your own process table
$ ls /proc                          # 0/ 1/ 2/ ... one dir per task slot
$ cat /proc/2/state                 # runnable | blocked | zombie | unused
$ ls /mnt/a/proc                    # MACHINE A's task slots
$ cat /mnt/a/proc/2/state           # A's filesystem-server state
```

### Writing another machine's screen — remote `/dev/cons`

`/dev/cons` is the console as a **writable** file. Write to your own with
`mount -c /dev/cons`; write *another* machine's screen over its remote
mount:

```
$ mount -c /dev/cons                # your console as a file
$ echo hello > /dev/cons            # ...prints on this screen
$ write /mnt/a/dev/cons "ping from B"   # prints on MACHINE A's screen
```

### Another machine's network identity — remote `/net`

`/net` exposes a machine's IPv4 and MAC as read-only files:

```
$ mount -n /net ; cat /net/ip       # your own address (e.g. 10.0.2.11)
$ cat /mnt/a/net/ip                 # MACHINE A's address -> 10.0.2.10
$ cat /mnt/a/net/mac                # MACHINE A's MAC
```

### Running a program on another machine — `cpu`

`cpu <host:port> <command>` runs `<command>` on the remote machine's CPU and
streams its output back to you — and, crucially, the command reads **your**
files, through your namespace **imported at `/host`**. That's the Plan 9
`cpu` model: ship the computation to another machine while its data stays
yours.

```
$ cpu 10.0.2.10:564 ls /            # run `ls` ON machine A; see A's disk root
$ cpu 10.0.2.10:564 uptime          # run `uptime` on A; A's uptime
$ cpu 10.0.2.10:564 ls /host        # /host is THIS machine (the caller) — YOUR root
$ write local.txt hi
$ cpu 10.0.2.10:564 cat /host/local.txt   # cat runs on A, but reads YOUR local.txt
```

So within one remote command, **`/`** is the machine it runs on and
**`/host`** is the machine you launched it from. `cpu B ls /` shows B's
disk; `cpu B ls /host` shows yours. The command's `/bin` program is loaded
from the remote's disk, so both machines should have the same `/bin`.
(Output up to ~2 KB comes back whole — the shell pulls it in chunks; larger
output is still bounded, with truly unbounded streaming a documented later
refinement. The imported namespace covers filesystem access, not the remote's
`/proc`/`/net` unless you reach them under `/host`.)

### Dialing out of another machine's network — `/net/tcp`

A machine can open a TCP connection **out of another machine's NIC** — use A's
network to reach the outside. It's exposed as Plan 9's `/net/tcp` connection
files: read `clone` for a connection number, write `connect <ip>!<port>` to its
`ctl`, then read/write its `data`. The `dial` command drives the whole sequence:

```
$ mount -n /net                              # bind THIS machine's /net
$ dial /net 93.184.216.34 80 GET / HTTP/1.0  # dial out of OUR nic
$ mount -r 10.0.2.10:564 /mnt/a              # mount machine A's export
$ dial /mnt/a/net 93.184.216.34 80 GET / HTTP/1.0   # dial out of A's nic
```

The only thing that changes is the base path: `/net` is your network,
`/mnt/a/net` is A's — so the last line reaches the web *through A's connection*,
authenticated by the same cluster key as any export access. Unlike `cpu A fetch`
(which runs a program on A), `dial` gives you a raw connection you drive
yourself, with no matching program needed on A. Scoped to a TCP client
(stop-and-wait, small transactions); UDP is later work.

### Accepting connections on another machine's network — `/net/tcp` dial-in

The mirror of `dial`: **accept inbound** connections on another machine's network
presence. `announce` a port, then `listen` hands out each accepted connection; a
client that connects to that machine's address is answered by the program that
announced. The `serve` command drives it (announce, accept one, respond, close):

```
$ mount -n /net                 # bind THIS machine's /net
$ serve /net 9000 hi there      # answer clients that connect to US on :9000
$ mount -r 10.0.2.10:564 /mnt/a # mount machine A's export
$ serve /mnt/a/net 9000 hi there   # answer clients that connect to A on :9000
```

The last line makes a program *here* answer clients that connect to **machine A's
address** — A lends its ingress, this machine owns the service. It's the inverse
of `cpu A <server>` (which runs the server on A): here the server's state lives
where `serve` runs. Scoped to a small fan-out (a listener plus a couple of
concurrent connections), TCP only; a persistent multi-client server loop is a
straightforward extension of `serve`.

### Under the hood (pointers)

Every machine's `netd` is both a client and a 9P **export gateway**; paths
resolve through a per-task **namespace** whose bindings can point at a local
`fsd` mount, the console, `/net`, or a **remote** endpoint. The full design
and its build history: [`roadmap-cluster.md`](roadmap-cluster.md) and the
per-phase design docs (`roadmap-cluster-phase{1,2,3,4}.md`), with the bug
retrospectives in `cluster-distributed-postmortem.md` and
`cluster-phase0-postmortem.md`.

## Syscall ABI

Full, current per-syscall detail: [`architecture.md`](architecture.md)'s
syscall table; the authoritative constants live in
`syscall-abi/src/lib.rs` and `ninep-abi/src/lib.rs`, imported by both the
kernel and every userland program. (The table below covers the core
syscalls; the filesystem protocol has since moved to the uniform
`ninep-abi` verb set — `NP_READDIR`/`NP_READ`/`NP_WRITE`/… — and the
network/cluster ops to the `NETOP_*` protocol, both documented in those
crates.)

**Calling convention** (Linux-shaped, not Linux-compatible): syscall
number in `x8`, up to four arguments in `x0`–`x3`, return value in
`x0`, via `svc #0`. Pointer/length arguments are bounded at **512
bytes per buffer** (`MAX_USER_LEN`) — longer buffers are rejected, not
truncated.

**Error convention:** all failure codes live in a reserved top band of
`u64` — **any return value `>= FS_ERR_MIN` (`u64::MAX - 31`) is an
error**; everything below is a real result (byte counts, sizes, exit
statuses). `NO_FS` (`MAX-1`) means no filesystem is mounted this boot.

| # | Name | Arguments | Purpose |
|---|---|---|---|
| 0 | `print` | value | Demo: log a value through the kernel console |
| 1 | `double` | value | Demo: returns `value * 2` |
| 2 | `report` | task id | Demo: per-task counter |
| 3 | `try_read_char` | — | Non-blocking read; `NO_CHAR` if nothing waiting |
| 4 | `putc` | byte | Raw console byte write |
| 5 | — | | *Deliberate gap (removed `shell_input`; ABI stability over density)* |
| 6 | `get_ticks` | — | Preemption tick count since boot |
| 7–14 | — | | *Deliberate gaps: the old `fs_*` syscalls — the filesystem lives in userland now (the fsd server); their contracts survive as the `FSOP_*` request protocol below* |
| 15 | `read_char` | — | Blocking read: the task is suspended until a byte arrives |
| 16 | `spawn` | staged total len | Start a program image previously fed in via `spawn_stage` as a new task alongside the caller; returns the new task's slot index |
| 17 | `exit` | code | Destroy the calling task; status kept (masked to 0–255) until `wait`ed. Tasks 0–2 refused (`EXIT_DENIED`) |
| 18 | `task_state` | index | `UNUSED`/`RUNNABLE`/`BLOCKED`/`ZOMBIE`, or `TASK_STATE_INVALID` past the last slot |
| 19 | `kill` | index | Destroy another task (reaps immediately). Tasks 0–2 protected |
| 20 | `fg` | index | Hand keyboard ownership to a task (auto-reverts to task 0 on the owner's death, or on Ctrl+C) |
| 21 | `wait` | index | Block until the task dies; returns its status (0–255), `TASK_KILLED_STATUS` (0x100), or `WAIT_INTERRUPTED` (Ctrl+C). Collecting the status reaps the slot |
| 22 | `mount` | replace flag | Rescan the USB ports and install a storage device as the kernel's block device (`0`, `MOUNT_ALREADY`, or `MOUNT_NO_DEVICE`) — the device half; the FS half is the server's `FSOP_MOUNT` |
| 23 | `msg_send` | dest, buf ptr/len | IPC: deliver a message (≤64 bytes) — straight into a matching blocked receiver's buffer (direct delivery), or into the task's bounded mailbox. Zero length is legal: the pipeline end-of-stream marker |
| 24 | `msg_recv` | buf ptr/len | Block until a message arrives; returns `(sender << 32) \| len`, or `RECV_INTERRUPTED` on Ctrl+C |
| 25 | `msg_try_recv` | buf ptr/len | Non-blocking receive; `NO_MSG` when empty |
| 26 | `block_info` | — | Block-device capacity in sectors — **task 2 (the fsd server) only**, like all three `block_*` syscalls |
| 27 | `block_read` | LBA, buf ptr | Read one 512-byte sector (fsd only) |
| 28 | `block_write` | LBA, buf ptr | Write one 512-byte sector (fsd only) |
| 29 | `msg_call` | dest, req ptr/len, reply ptr | Synchronous request/response: send + block for a reply *from `dest` specifically*; sub-tick round trips via direct delivery. Reply buffer is a fixed 64 bytes |
| 30 | `spawn_stage` | offset, chunk ptr/len | Feed one chunk of a program image into the kernel's 128KB staging buffer for `spawn` |

**File operations** are `FSOP_*` requests to the filesystem server
(task 2), sent via `msg_call`: a 56-byte message (op + six LE u64
args — pointers into your own buffers), one u64 reply with the old
syscalls' exact return semantics. Ops: `LIST_DIR`, `READ_FILE`,
`READ_AT` (windowed read at an offset), `WRITE_FILE`, `MKDIR`,
`RMDIR`, `TOUCH`, `RM`, `MV`, `MOUNT`. The shell's `fs_call` wrapper
(`shell/src/main.rs`) is the reference client.

Filesystem failures return a specific `FS_ERR_*` code (`NOT_FOUND`,
`NOT_A_FILE`, `NOT_A_DIRECTORY`, `INVALID_NAME`, `ALREADY_EXISTS`,
`NOT_EMPTY`, `IS_ROOT`, `DISK_FULL`, `IO`); `spawn` adds
`SPAWN_ERR_BAD_ELF`/`SPAWN_ERR_TOO_LARGE`/`SPAWN_ERR_NO_FREE_SLOT`;
the task syscalls add `TASK_ERR_NO_SUCH_TASK`/`TASK_ERR_PROTECTED`;
the block syscalls add `BLOCK_ERR_NO_DEVICE`/`BLOCK_ERR_IO`/
`BLOCK_ERR_DENIED`.

## Writing your own program

The full guide is [`processes.md`](processes.md); `hello/` is the
working ~70-line template. The essentials: `#![no_std]`/`#![no_main]`,
`_start` placed first in `.text` via `.text.start`, syscalls via inline
`svc #0`, **release builds only**, and **no static mutable state**
(the linker script asserts `.data`/`.bss` empty — keep state in
`main`'s stack frame). Stage the stripped ELF on the ESP and either
point `INIT.CFG` at it (boot into it) or `exec` it from the shell.
