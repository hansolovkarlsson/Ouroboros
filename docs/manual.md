# Ouroboros manual

The one-stop reference for using Ouroboros: building it, running it on
QEMU and on real Parallels hardware, testing it, using the shell, and
the complete syscall ABI. Each section links to the deeper reference
where one exists — [`architecture.md`](architecture.md) for how the
kernel works, [`processes.md`](processes.md) for the userland program
model, [`shell-commands.md`](shell-commands.md) for the full builtin
command reference, [`roadmap.md`](roadmap.md) for what's next, and
`CLAUDE.md` at the repository root for the debugging history behind
every design decision.

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

```sh
make run             # fast dev loop - vvfat-backed disk (FAT16!)
make run-image       # boots esp.img - real FAT32, disk commands work
make run-usb-kbd     # + xHCI controller & USB keyboard (monitor sendkey)
make run-usb-multi   # + USB tablet and storage stick (3-device xHCI rig)
make run-virtio-console  # + a virtio console device (see its Makefile note)
make run-gicv3       # forces GICv3 instead of QEMU's default GICv2
```

The one everyday gotcha: **`make run`'s disk is FAT16** (an artifact of
QEMU's vvfat driver), which the kernel's FAT32-only reader can't mount
— every disk command prints a shared "no filesystem mounted this boot"
message there. Use `make run-image` whenever you want `ls`/`cat`/
`exec`/etc. to actually work.

Exit QEMU with `Ctrl+a x`. On the USB targets, keystrokes can be
injected through the monitor socket:
`printf 'sendkey u\n' | nc -U qemu-monitor.sock`.

## Running on Parallels (real hardware)

```sh
make image           # esp.img - raw MBR+FAT32 disk image
make parallels-hdd   # wraps it into esp.hdd via prl_disk_tool
```

Attach `esp.hdd` as the VM's **Hard Disk** device. Two confirmed traps
(see `CLAUDE.md`'s "Parallels disk attachment"): Parallels rejects raw
`.img` files on the Hard Disk device, and attaching the image to the
CD/DVD device *looks* like it works but doesn't (the optical driver
wants ISO9660). Also: `esp.hdd` stores a *pointer* to `esp.dmg`'s
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

Rebuilds `esp.hdd`, boots the registered VM headlessly, types each
`;`-separated command via `prlctl send-key-event`, and saves a
screenshot after each into a `parallels-test-<timestamp>/` directory
(gitignored — delete it when done). Supports `>` (a held-Shift chord)
and a `CTRL-C` pseudo-command (a held-Ctrl chord, no Enter). Requires
the VM registered in Parallels with its Hard Disk pointed at this
repo's `esp.hdd`.

## Using the shell

Full reference: [`shell-commands.md`](shell-commands.md). The short
tour:

```
$ help                          # list every builtin
$ mount                         # (Parallels) mount a passed-through USB stick
$ ls /EFI/ORBS                  # disk commands (FAT32 boot only)
$ write notes.txt hello world   # create/replace a file's contents
$ cat notes.txt
$ echo backup >> log.txt        # output redirection: > replace, >> append
$ mv notes.txt /EFI/ORBS        # into an existing directory keeps the name
$ exec /EFI/ORBS/HELLO.BIN      # spawn a second program (runs alongside)
$ wait 2                        # block until it exits, collect its status
$ exec /EFI/ORBS/SH.BIN         # spawn a second shell...
$ fg 2                          # ...and hand it the keyboard (nested session)
$ exit                          # (in the nested shell) hand it back
$ ps                            # one line per task slot
$ kill 2                        # destroy a task
```

Job-control notes: only one task owns the keyboard at a time (`fg`
moves it; ownership returns to the boot shell automatically when the
owner dies, or on **Ctrl+C**, which reclaims the keyboard without
killing anything). An exited task holds its slot as a zombie until
`wait`ed (`ps` shows it); `kill` reaps immediately. Ctrl+C also
interrupts a stuck `wait`.

## Syscall ABI

Full per-syscall detail: [`architecture.md`](architecture.md)'s
syscall table; the authoritative constants live in
`syscall-abi/src/lib.rs`, imported by both the kernel and every
userland program.

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
| 7 | `fs_list_dir` | path ptr/len, buf ptr/len | Directory listing (`name\n`, `name/\n`) |
| 8 | `fs_read_file` | path ptr/len, buf ptr/len | Read a file; returns its *real* size (compare to detect truncation) |
| 9 | `fs_mkdir` | path ptr/len | Create a directory (grows a full parent automatically) |
| 10 | `fs_rmdir` | path ptr/len | Remove an empty directory |
| 11 | `fs_touch` | path ptr/len | Create an empty file (no-op if it exists) |
| 12 | `fs_rm` | path ptr/len | Remove a file |
| 13 | `fs_write_file` | path ptr/len, data ptr/len | Create/fully replace a file's contents (zero-length data = truncate) |
| 14 | `fs_mv` | src ptr/len, dst ptr/len | Rename/move (relinks the cluster chain; no content copy) |
| 15 | `read_char` | — | Blocking read: the task is suspended until a byte arrives |
| 16 | `spawn` | path ptr/len | Load a program from disk as a new task alongside the caller |
| 17 | `exit` | code | Destroy the calling task; status kept (masked to 0–255) until `wait`ed. Tasks 0/1 refused (`EXIT_DENIED`) |
| 18 | `task_state` | index | `UNUSED`/`RUNNABLE`/`BLOCKED`/`ZOMBIE`, or `TASK_STATE_INVALID` past the last slot |
| 19 | `kill` | index | Destroy another task (reaps immediately). Tasks 0/1 protected |
| 20 | `fg` | index | Hand keyboard ownership to a task (auto-reverts to task 0 on the owner's death, or on Ctrl+C) |
| 21 | `wait` | index | Block until the task dies; returns its status (0–255), `TASK_KILLED_STATUS` (0x100), or `WAIT_INTERRUPTED` (Ctrl+C). Collecting the status reaps the slot |
| 22 | `mount` | — | Rescan the USB ports and mount a storage device's FAT32 (`0`, `MOUNT_ALREADY`, or `MOUNT_NO_DEVICE`) — the Parallels disk path |

Filesystem failures return a specific `FS_ERR_*` code (`NOT_FOUND`,
`NOT_A_FILE`, `NOT_A_DIRECTORY`, `INVALID_NAME`, `ALREADY_EXISTS`,
`NOT_EMPTY`, `IS_ROOT`, `DISK_FULL`, `IO`); `spawn` adds
`SPAWN_ERR_BAD_ELF`/`SPAWN_ERR_TOO_LARGE`/`SPAWN_ERR_NO_FREE_SLOT`;
the task syscalls add `TASK_ERR_NO_SUCH_TASK`/`TASK_ERR_PROTECTED`.

## Writing your own program

The full guide is [`processes.md`](processes.md); `hello/` is the
working ~70-line template. The essentials: `#![no_std]`/`#![no_main]`,
`_start` placed first in `.text` via `.text.start`, syscalls via inline
`svc #0`, **release builds only**, and **no static mutable state**
(the linker script asserts `.data`/`.bss` empty — keep state in
`main`'s stack frame). Stage the stripped ELF on the ESP and either
point `INIT.CFG` at it (boot into it) or `exec` it from the shell.
