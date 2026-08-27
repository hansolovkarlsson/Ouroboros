# Ouroboros shell commands

Reference for the builtin commands in `shell/` — Ouroboros's default
userland shell, loaded from disk at boot (see
[`processes.md`](processes.md)). This document describes *what the
commands do and how to use them*; for the syscalls backing them see
[`architecture.md`](architecture.md)'s syscall table, and for the
debugging history behind specific design choices (why `cd`'s path
resolution works the way it does, why some errors read "no filesystem
mounted", etc.) see `CLAUDE.md`'s phase write-ups. Since the shell is
just a loaded program like any other (`docs/processes.md`), this
document describes the *default* one — a replacement program is free to
implement a completely different command set. For the **cluster commands**
(`mount -r`, remote `/proc`/`/net`/`/dev/cons`, `cpu` remote execution, and
`dial`/`serve` for using another machine's network) shown here,
[`manual.md`](manual.md)'s cluster section is the task-oriented walkthrough with
two-machine examples. The 9P export those commands ride is **authenticated with
a shared cluster secret** (`\CLUSTER.KEY`, mutually authenticated since v0.13.0);
machines that don't share the key are refused.

## General behavior

- **Line editing.** Backspace and DEL both erase the previous character
  (standard destructive-backspace: `\b`, space, `\b`). Enter (`\r` or
  `\n`) submits the line. A full 128-byte input buffer silently drops
  further typed characters rather than erroring.
- **Tokenization: whitespace-split, no quoting.** `echo "a b"` sees the
  literal words `"a` and `b"`, not one quoted argument — there is no
  quote-stripping. Every command except `echo`/`write` only ever looks
  at its *first one or two* words after the command name (`cp`/`mv`
  take exactly two paths; every other command just one); any further
  words are ignored (`mkdir a b c` creates `a` and says nothing about
  `b`/`c`).
- **Output redirection (`> file` / `>> file`) and multi-stage pipes
  (`a | b | c`) work** — see the dedicated sections below (pipelines are
  N-stage now, with arguments and `$PATH` lookup on every stage), plus
  `exec prog > file`. **No input redirection (`<`), globbing, `;`/`&&`
  chaining, command history, or tab completion.** One command (or one
  pipeline) per line, typed in full, every time.
- **Path resolution** (`ls`, `cat`, `cd`, `mkdir`, `rmdir`, `touch`,
  `rm`): a leading `/` is an absolute path; anything else is resolved
  against the current working directory. `.` and `..` are collapsed
  client-side before the path ever reaches the kernel (`..` past root
  simply stays at root, same as a real shell) — see `resolve_path`/
  `normalize_path` in `shell/src/main.rs`. An empty argument to a
  path-taking command means "the current directory" (so `ls` with no
  argument lists `pwd`'s output; `cd` with no argument re-resolves to
  the same directory rather than jumping to root, unlike a real shell's
  bare `cd`).
- **Name matching is case-insensitive**, same as FAT itself (`ls`/`cat`/
  `cd` on `boot`, `Boot`, or `BOOT` all find the same entry).
- **No filesystem available this boot** (e.g. `make run`'s fast
  dev-loop disk is FAT16, which the server can't mount — or no FSD.BIN
  was on the ESP at all, so there's no filesystem server) makes every
  disk command print one shared message rather than a command-specific
  error, since no path could ever resolve in that state. Use `make
  run-image` (or a Parallels boot plus `mount` with a USB stick) for
  disk commands to do anything.
- **Long filenames are read, not created.** The FAT32 reader now
  reconstructs long filenames (LFN) written by a real formatter, so a
  file like `index.html` shows up in `ls`, opens with `cat`, and can be
  navigated — by its real name. But *creating* one from the shell still
  isn't supported: `mkdir`/`touch`/`write` accept only 8.3 names — ASCII
  alphanumerics, `_`, and `-`, one optional `.` splitting an
  up-to-8-character base from an up-to-3-character extension — so a file
  the shell itself creates can't have a long name (yet). Deleting a
  long-named file works but leaves its LFN entries behind (a harmless
  space leak).

## Commands

| Command | Syntax | Description | Notes |
|---|---|---|---|
| `help` | `help` | Lists the shell **builtins** only, and points to `/bin` (via `ls /bin`) for the externalized programs. | Static text, no syscall. Deliberately doesn't enumerate `/bin` — those change as programs are added, and `ls /bin` is the live list. |
| `echo` | `echo [words...]` | Prints all words after `echo`, space-separated. | The one command that uses more than its first argument. |
| `uptime` | `uptime` | Prints the preemption tick count since boot. | Backed by `get_ticks` — real kernel state, not a demo. |
| `clear` | `clear` | Clears the screen (ANSI `\x1b[2J\x1b[H`). | A raw escape sequence the shell sends itself, not a syscall — the console has no notion of a screen. |
| `pwd` | `pwd` | Prints the current working directory. | Shell-local state only, no syscall. |
| `ls` | `ls [-l] [-a] [path]` | Lists a directory, **sorted by name** (case-insensitive), defaulting to the current directory. Default layout is multi-column (files plain, subdirectories suffixed `/`); **`-l`** is the long form — one entry per line with its type (`d`/`-`), size, and modified `YYYY-MM-DD HH:MM`. **`-a`** includes dotfiles (and `.`/`..`), hidden otherwise. Flags combine (`-la`). | The long form's size and time come from a `stat` of each entry (`NP_STAT`); the time shows only where the filesystem records one — **FAT32** does, so `ls -l` on the disk shows real dates, while exFAT/ext2/`/proc` currently show `-` (size and type are always real). Assumes an 80-column width. Truncates rather than erroring if a listing exceeds the 512-byte buffer. |
| `tree` | `tree [path]` | Recursively lists a directory as an indented tree (ASCII branches `\|-- `/`` `-- ``, since the framebuffer font is ASCII-only), ending with a `N directories, M files` summary. Defaults to the current directory. Entries are sorted alphabetically (case-insensitive), directories and files interleaved. | Depth-capped at 16 levels (the ~32 KB spawn stack bounds recursion — a deeper subtree isn't descended). `.`/`..` are skipped. Each directory's listing is bounded to 512 bytes like `ls`, and up to 64 entries per directory are sorted (a larger directory drops the overflow). |
| `cat` | `cat <file>` | Prints a file's contents. | Streams the file in chunks via the grant/safecopy bulk path, so it prints a file of *any* size (no truncation) without ever holding the whole thing; a file argument is required. |
| `more` | `more <file>` or `<command> \| more` (alias `less`) | Pages output a screen at a time. At each `--More--` pause: **space** shows the next screen, **Enter** shows one more line, **q** quits. | A **builtin**, not a `/bin` program — a pager must read the keyboard while running, and only the boot shell owns the keyboard (a spawned program can't), so paging has to happen in the shell. Content comes from a file or a captured command; it's held in the shell's heap buffer, so output larger than that (256 KB) is refused rather than paged. `\| more` accepts a single command (`ls -l \| more`, `cat big \| more`), not yet a multi-stage pipeline. `less` is an alias — same forward-only pager. Assumes ~24 rows. |
| `cd` | `cd [path]` | Changes the current working directory. | Validates the target exists and is a directory first (via a listing call — there's no dedicated "does this exist" syscall). |
| `bind` | `bind <newpath> <oldpath>` | Maps `newpath` onto the existing subtree `oldpath` in this shell's **namespace**, so any path under `newpath` resolves as if it were under `oldpath` (Plan 9 `bind`; the cluster's per-task namespace, Phase 0). E.g. `bind /mnt /EFI` makes `ls /mnt` list `/EFI`. | **Per-task**: only this shell and the commands it spawns (which inherit the namespace) see it — no other task's view changes. Both paths are resolved against the cwd. A bare `bind` remaps *within* the current mount; to point `newpath` at a *different* disk partition use `mount <n> <path>`, and at *another machine* use `mount -r` (both below). |
| `mkdir` | `mkdir <dir>` | Creates an empty subdirectory. | Fails with a specific message for each reason (already exists, invalid name, parent missing, disk full — the kernel returns distinct `FS_ERR_*` codes now). Grows a full parent directory by a cluster automatically (as do `touch`/`write`/`cp`/`mv` when creating entries). |
| `rmdir` | `rmdir <dir>` | Removes an *empty* subdirectory. | Fails if it doesn't exist, isn't empty, or is root. |
| `touch` | `touch <file>` | Creates an empty (zero-byte) file, or succeeds silently if one already exists there. | There's no RTC on this kernel, so unlike real `touch`, an existing file's "timestamp" isn't updated — nothing happens, successfully. Fails if the target is a directory. |
| `rm` | `rm <file>` | Removes a file. | Fails if it doesn't exist or is a directory — use `rmdir` for those. |
| `write` | `write <file> [words...]` | Joins every word after the filename with a single space (same style as `echo`) and writes the result as the file's *entire* contents, replacing whatever was there. Creates the file if it doesn't exist. | `write <file>` with no words truncates the file to empty (a real, valid case, not an error). Fails if the target is an existing directory or the parent is missing. |
| `writeat` | `writeat <file> <offset> <text...>` | A **random-access write**: writes the text at byte `offset`, overwriting bytes *in place* and leaving everything outside the written window intact (unlike `write`, which replaces the whole file). If `offset` is past the end of the file, the gap is **zero-filled** on disk. | The file must **already exist** — `writeat` does not create it (use `write`/`touch` first). The text is bounded by the input line. A past-EOF gap is capped at 1 MiB (a larger offset reports a device I/O error). Fails on a missing file, or if the target is a directory. |
| `cp` | `cp <src> <dst>` | Copies `src`'s contents to `dst`, creating `dst` if it doesn't exist or replacing it if it does. | **Streams the copy one chunk at a time via the FAT32 offset-write primitive, so it handles a file of any size** (bounded by disk space, not a shell buffer) — the old 2048-byte ceiling is gone. `cp x x` (a file onto itself, however the two paths are spelled) is **refused**: streaming truncates `dst` first, which would destroy the source. A missing source leaves `dst` untouched. Non-atomic: an interrupted copy leaves `dst` truncated (a partial copy is a wrong copy). No recursive directory copy. |
| `ps` | `ps` | One line per scheduler slot: the state (`unused`, `runnable`, `blocked (waiting)`, or `` exited (code N) - `wait` to collect `` for a zombie, where `N` is the exit code) followed by the task's **name**. Boot tasks are named `shell`/`idle`/`fsd`/`cond`/`netd`; a spawned task shows its `argv[0]` (the path `exec`/a pipeline launched it with); unused slots and reaped-pending zombies (whose argv was cleared on exit) show no name. | The name comes from the `TASK_NAME` syscall (`argv[0]` of the named slot); the zombie exit code from `TASK_EXIT_CODE` (peeked without reaping — `wait` is still what collects the status and frees the slot). The caller can't distinguish "running right now" from "runnable" — it is, by definition, the one running when it asks. Output is redirectable like any other command's. |
| `kill` | `kill <n>` | Destroys task `n` (see `ps` for numbers). | Tasks 0 (this shell), 1 (idle), and 2 (the filesystem server) are protected. A killed task's slot becomes spawnable again and its memory is reclaimed when allocation order allows. |
| `fg` | `fg <n>` | Hands the keyboard to task `n` — e.g. `exec /EFI/ORBS/SH.BIN` then `fg 2` gives a real nested shell session; its `exit` hands the keyboard back. | **Ctrl+C is the escape hatch**: typed while another task owns the keyboard, the kernel reclaims it for this shell (the foregrounded task keeps running in the background — Ctrl+C is keyboard reclamation, not a signal; `kill` the task if it should die too). Ownership also reverts automatically when the foregrounded task exits or is killed. While this shell owns the keyboard, Ctrl+C (like every unhandled control byte) is ignored by the line editor. |
| `send` | `send <n> <words...>` | Sends the words (space-joined, like `write`) as one IPC message (≤64 bytes) to task `n`'s mailbox. | Fails distinctly for a missing task, an over-long message, or a full mailbox (4 pending max). A dead task's queued mail dies with it. |
| `recv` | `recv` | Blocks until a message arrives and prints `task N: <message>`. | Ctrl+C interrupts, like `wait`; typing during a blocked `recv` is otherwise discarded. |
| `wait` | `wait <n>` | Blocks until task `n` dies, then reports its exit status — which is also what *reaps* it (an exited task holds its slot as a zombie, shown by `ps`, until waited). | Ctrl+C interrupts the wait (the task keeps running); any other typing during a wait is discarded, like typing at a busy foreground job in `sh`. Waiting on tasks 0-3 (shell, idle, fsd, cond) or yourself is refused (they never die). **Behavior change from earlier builds:** an un-waited exited task holds its slot — `exec` something three times without waiting and the third fails with "no free task slot" until you `wait` (or the task is `kill`ed, which reaps immediately). |
| `exit` | `exit` | Asks the kernel to destroy this task (`exit` syscall). | Always refused for the boot shell itself (it's the sole keyboard owner - the kernel returns `EXIT_DENIED` and the shell prints why); exists as the reference for how a replacement/spawned program ends itself. `hello/` (`exec /EFI/ORBS/HELLO.BIN`) demonstrates a successful exit. |
| `shutdown` | `shutdown` (alias `poweroff`) | Powers the machine off (the `POWER` syscall → PSCI `SYSTEM_OFF`; under QEMU the VM exits). | A **builtin**, not a `/bin` program — you must be able to power off even with no disk mounted (the same reasoning as `erase`/`partition`/`format`). The PSCI conduit (`hvc`/`smc`) is read from ACPI's FADT at boot; if PSCI is unavailable, it falls back to a halt. Does not return. |
| `halt` | `halt` | Halts the CPU: masks interrupts and parks the core forever, so the whole machine stops (but power is not cut — under QEMU the VM keeps running with the CPU halted). | A builtin, like `shutdown`. Nothing runs after it — not even the timer tick. Use `shutdown` to actually power off. |
| `exec` | `exec <path> [args...]` | Loads the program at `path` and starts it as a new, independent task alongside this shell (see `ps`) — spawn semantics, not exec-replaces-current-process. | Any words after the path become the program's **argv** (`argv[0]` is the path), read via the `GET_ARGC`/`GET_ARG` syscalls. Reads the program via the filesystem server in 512-byte chunks, stages them into the kernel, then spawns (see `architecture.md`'s "Dynamic task creation"). Fire-and-forget (not waited); fails with the specific reason: no such file, is a directory, not a loadable program (bad ELF), too large, or no free task slot. |
| `mount` | `mount` | With no argument, **reports what's mounted**: the filesystem format, the first sector (LBA) of its partition, and the disk's capacity — e.g. `exFAT mounted at partition LBA 2048 (disk 182272 sectors, 89 MiB)`, or `nothing mounted` if none is. | Asks the filesystem server `FSOP_MOUNT_INFO`. Redirectable like any command's output. The disk-management arc (milestone 1) repurposed bare `mount` to *list*, Unix-style; the mounting action moved to `mount -a`. |
| `mount -a` | `mount -a` | Makes a USB storage stick's filesystem available — the Parallels workflow: passthrough USB attaches a few seconds *after* boot, so boot, wait a moment, then `mount -a`. | Two halves under the hood: the filesystem server first retries mounting whatever block device the kernel already holds; only if that yields nothing does the kernel rescan the USB ports and install a found stick as a *replacement* device (safe exactly because nothing is mounted), then the server mounts it. An unmountable boot-time disk (e.g. `make run`'s FAT16) therefore never blocks a later stick. |
| `mount <n> <path>` | `mount 1 /mnt/f` | **Mounts the disk's `n`-th partition and makes it visible at `<path>`** — a *second* filesystem alongside the boot mount at `/` (cluster Phase 0 multi-mount). `fsd` mounts the partition (`FSOP_MOUNT_AT`) into a fresh "tree" and returns its id; the shell `bind`s `<path>` onto it. So `ls /mnt/f` lists the mounted partition while `ls /` still lists the boot filesystem — two different on-disk filesystems at once. | **Per-task** (it's a `bind`): only this shell and the commands it spawns see it. The partition index is the MBR/GPT discovery order (partition 0 is usually the boot auto-mount at tree 0). Up to 4 mounts. |
| `mount -r <host:port> <path>` | `mount -r 10.0.2.10:564 /mnt/a` | **Remote-mount another machine's 9P export** into this shell's namespace at `<path>` (cluster Phase 1c). A path under `<path>` then resolves to a `NETOP_RMOUNT` round trip through `netd` to `host:port`, so `ls /mnt/a` / `cat /mnt/a/file` read (and write) the *remote* machine's disk over TCP. | **Per-task**, inherited by spawned commands. Read+write (single-writer, no distributed lock). **Authenticated** with the shared cluster key (`\CLUSTER.KEY`) — a peer without it is refused; mutually authenticated since v0.13.0 (integrity, not confidentiality — bytes cross in cleartext). `host` is a dotted-quad IPv4; `port` defaults to 564. A disconnect fails the next op cleanly rather than hanging. See `manual.md`'s cluster section. |
| `mount -p <path>` | `mount -p /proc` | Binds the synthetic **`/proc`** filesystem — the process table as files (cluster Phase 3) — at `<path>`. Then `ls <path>` lists one dir per task slot and `cat <path>/<n>/state` reads its scheduler state (`runnable`/`blocked`/`zombie`/`unused`). | Read-only. A *remote* machine's `/proc` needs no bind — read it at `<remote-mount>/proc` (netd's export routes it). |
| `mount -c <path>` | `mount -c /dev/cons` | Binds the **console** as a *writable* file (cluster Phase 3 `/dev/cons`) at `<path>`. A write to `<path>` renders on the console; reads are refused. So `echo hi > /dev/cons` prints on screen, and `write <remote-mount>/dev/cons msg` prints on *another machine's* screen. | Write-only; a write routes to the console server (`cond`). |
| `mount -n <path>` | `mount -n /net` | Binds the network server's synthetic **`/net`** (this machine's network identity as read-only files, cluster Phase 3) at `<path>`. `cat <path>/ip` reads the IPv4, `cat <path>/mac` the MAC. | Read-only. `cat <remote-mount>/net/ip` reads *another* machine's address. |
| `unmount` | `unmount` | Drops the mounted filesystem (`FSOP_UNMOUNT`) so the disk can be reformatted or a different volume mounted; prints `unmounted`, or `nothing was mounted`. | The kernel's block device is untouched — `mount -a` re-probes and remounts it. The disk-management arc, milestone 1. |
| `erase` | `erase disk` | **Destructive.** Zeroes the disk's first 2048 sectors (1 MiB) — the partition table plus any filesystem metadata near the start — so the disk can be freshly partitioned (`FSOP_ERASE`). | Requires the literal `disk` argument as a guard against an accidental bare `erase`. Refused while a filesystem is mounted (`run unmount first`) and if there's no disk device. A **builtin**, not a `/bin` program: it runs when nothing is mounted, which is exactly when `/bin` can't be read to load a program. Disk-management arc, milestone 2. |
| `partition` | `partition [fat32\|exfat\|ext2]` | **Destructive.** Writes a fresh MBR with one primary partition spanning the disk from LBA 2048 to the end, tagged with the given type (default `fat32` → 0x0C; `exfat` → 0x07; `ext2`/`linux` → 0x83) (`FSOP_PARTITION`). | Only LBA 0 (the partition table) is written; the partition is left **unformatted** — `format` lays a filesystem into it, and `mount` won't succeed until then. Refused while mounted. GPT is a later step. A builtin, same reason as `erase`. Disk-management arc, milestone 2. |
| `format` | `format [fat32\|exfat\|ext2]` | **Destructive.** Lays a fresh filesystem into the disk's first partition (`FSOP_FORMAT`) — mkfs. **FAT32, exFAT, and ext2** are all supported. | The partition must already exist (`partition` first); refused while mounted. After it succeeds, `mount -a` then `ls` shows an empty root (or `lost+found`, on ext2). Validated against macOS `fsck_msdos` / `fsck_exfat` and, for ext2, `e2fsck` + `debugfs`. The ext2 formatter is single-block-group (128 MiB cap). A builtin, same reason as `erase`/`partition`. Disk-management arc, milestone 3 (complete). The typical prep flow is `unmount` → `erase disk` → `partition ext2` → `format ext2` → `mount -a`. |
| `ping` | `ping <a.b.c.d>` | Sends an ICMP echo request to a host and reports whether a reply came back (`reply from …`, `no reply … (timeout)`, `… is unreachable (no ARP reply)`, or `no network interface this boot`). | Asks the network server (`netd`) to ARP-resolve the host and ICMP-echo it — the whole protocol stack lives there; the shell just packs the target and reads the status. **Guest-initiated only** (the guest can't yet *answer* a host's ping — needs async receive). QEMU only for now (Parallels' virtio-net is PCI, unsupported); the source IP `10.0.2.15` and /24 assumption are the QEMU user-net convention. |
| `resolve` | `resolve <hostname>` | Looks up a hostname's IPv4 via DNS and prints `<host> is a.b.c.d`, or `could not resolve …` / `no response for …`. | Asks `netd` to send a DNS A-record query over **UDP** to the QEMU user-net DNS server (`10.0.2.3`), which forwards to the host's resolver — so it resolves *real* names (`resolve example.com`). A records only; QEMU only (same reasons as `ping`). |
| `fetch` | `fetch <hostname>` | Opens a **client TCP** connection to the host on port 80, sends a minimal HTTP `GET /`, and prints the response (e.g. `fetch example.com` → `HTTP/1.1 200 OK …`). | Chains the whole stack in `netd`: resolve → route (via the gateway for off-subnet hosts) → TCP handshake → HTTP → clean teardown. A client command; one connection, no retransmission; the response is capped at what fits one IPC reply (the full length is reported). (`netd` itself *does* serve TCP — see `dial`/`serve` below and its HTTP/export listeners.) QEMU only (same reasons as `ping`). |
| `dial` | `dial /net 1.2.3.4 80 GET / HTTP/1.0` | **Dial out of a machine's NIC** (`/net/tcp` connection files): opens a raw TCP connection to `<ip> <port>` and prints the reply, optionally sending the trailing words as a request first. `<base>` picks *whose* network dials: `dial /net …` uses this machine; `dial /mnt/a/net …` (a remote-mounted export) dials out of **machine A's** NIC — "use another machine's network." | A raw connection you drive yourself (unlike `cpu A fetch`, which runs a *program* on A). Stop-and-wait, small transactions. Needs `<base>/tcp` reachable — bind local with `mount -n /net`, or a remote export with `mount -r`. See `manual.md`'s cluster section. |
| `serve` | `serve /net 9000 hi there` | **Accept an inbound connection on a machine's NIC** (`/net/tcp` dial-in, the mirror of `dial`): announces `<port>`, accepts one client, sends the trailing words as a response, and closes. `serve /mnt/a/net 9000 …` announces on **machine A's** network, so a client that connects to A's address is answered here — A lends its ingress, this machine owns the service. | Inverse of `cpu A <server>` (which runs the server *on* A): here the service's state lives where `serve` runs. Small fan-out, TCP, one accept-then-exit (a persistent loop is a straightforward extension). See `manual.md`'s cluster section. |
| `cpu` | `cpu <host:port> <command...>` | **Remote execution** (cluster Phase 4, the Plan 9 `cpu` model): runs `<command>` on the machine at `host:port` and prints its output here. The command runs on the *remote's* CPU using its `/bin`, but reads **this** machine's files through the caller's namespace imported at **`/host`** — so within the command, `/` is the machine it runs on and `/host` is the machine you launched from. E.g. `cpu 10.0.2.10:564 ls /host` lists *your* root; `cpu 10.0.2.10:564 cat /host/x` runs `cat` on the remote but reads *your* `x`. | **Authenticated** with the shared cluster key (a peer without it is refused); the export also refuses `cpu` entirely if the remote has a `\NOEXEC` flag file (disk-share allowed, remote-exec off). The command's program is loaded from the *remote's* `/bin`, so both machines should share it. Output up to ~2 KB comes back whole — the shell pulls it in chunks (`NETOP_RUN_MORE`); larger output is still bounded (truly unbounded streaming is a documented later refinement). Under the hood: `netd` frames an `NP_RUN` to the remote export, which spawns the command with stdout piped back and `/host` mounted to the caller; the caller's `netd` stays responsive to serve the command's `/host` reads *during* the run. See `manual.md`'s cluster section and `roadmap-cluster-phase4.md`. |
| `env` | `env` | Lists every environment variable as `NAME=VALUE`, one per line. | Shell-local variables (`PATH` is preset to `/bin`). Redirectable like any command's output. |
| `set` / `export` | `set NAME=VALUE` | Sets (or replaces) an environment variable. `export` is an alias. | The value is a **single token** — no quoting, so `set X=a b` sets `X` to `a` (same limitation as `echo`). Fails if the name/value is too long or the table is full (16 vars). Variables are **shell-local** — not yet passed into spawned programs. |
| `unset` | `unset NAME` | Removes an environment variable. | Silent if it wasn't set. |
| `selftest` | `selftest` | Exercises the two historically-crashing code patterns (`write!`/`core::fmt` formatting, slice/str-vs-literal comparison) and prints pass/fail lines. | The relocating loader's permanent acceptance test — see `processes.md`'s "Binary format". |
| `mv` | `mv <src> <dst>` | Renames or moves a file or directory to `dst` — and if `dst` is an existing directory, moves `src` *into* it keeping its basename (`mv notes.txt backup/`), like real `mv`. | A `dst` that exists and isn't a directory still fails rather than being overwritten. `mv x x` is refused ("source and destination are the same"). Moving a directory to a different parent correctly updates its own `..` entry, so `cd ..` inside it still resolves to the *new* parent afterward. No cycle detection beyond the trivial self-move guard. |

Any other input names a **program to run** in the foreground, with the whole
line as its argv. How the program is found follows the standard shell rule:

- A **bare name** (no `/`) is looked up on **`$PATH`** — each `:`-separated
  directory in turn, e.g. `/bin/<name>` — so a command binary is invoked by
  name, no `exec` or leading path needed (`echo hello`).
- A word **containing `/`** is a **pathname**, resolved against the current
  directory (or used as-is if absolute) and **not** searched on `$PATH`:
  `/bin/echo hello`, `bin/echo hello` from `/`, `../bin/echo hello` from a
  subdirectory all run `/bin/echo`.

Only if the resolved program can't be found does it print `unknown command:
<word>`. `$VAR` references anywhere in a line are expanded from the environment
before dispatch. A blank line (just Enter, or only whitespace) does nothing.

## Output redirection

Append `> file` (create or fully replace) or `>> file` (append) to any
command to send its output to a file instead of the screen:

```
echo hello > greeting.txt
uptime >> boot.log
ls > listing.txt
cat a.txt > b.txt
```

Semantics, deliberately close to `sh` where the architecture allows:

- **Only a command's output is redirected; error messages always stay
  on the screen** — the POSIX stdout/stderr split, without a separate
  `2>` mechanism to reroute the error half.
- **The target file is created (or, for `>`, truncated) even if the
  command produced nothing** — `> f.txt` on its own, with no command at
  all, legitimately creates an empty file, same as `sh`. `>> f.txt` to
  a missing file creates it.
- **The operator must be its own whitespace-separated word.**
  `echo hi>f` is a single token, not a redirect — the same no-quoting
  tokenization rule as everywhere else. Exactly one word (the target
  path) may follow the operator.
- `cat`'s display-only trailing newline stays off the captured bytes,
  so `cat a > b` copies `a`'s bytes exactly. `cat big > b` now captures
  and writes the whole thing (`cat` streams any size, and the capture is
  heap-backed — see below), no longer refusing.

Limits, all refuse-outright rather than write-something-wrong:

- A command's redirected output is captured in the shell's **256KB heap
  region** (the userland-heap milestone — a raw buffer far larger than the
  stack, so `cat big > file` fits and is written to disk in `SAFECOPY_MAX`
  chunks); a command that emits more than 256KB prints
  `output too large to capture` and **nothing is written at all**.
- `>>` **appends at the file's end via the FAT32 offset-write
  primitive** (`write_at`) — no read-back of the existing content, so it
  works on a target file of **any existing size** (the old
  read-concatenate-rewrite capped both halves at a combined buffer and
  refused a large existing file; that refusal is gone). The *new* output
  is still bounded by the 1024-byte capture above — the input side — but
  what you append it to is not.

## Pipelines

Chain commands with `|` — a full N-stage pipeline (`a | b | c | …`). Each
stage is resolved like any command (bare name via `$PATH`, or a path), and
**stages take arguments**:

```
echo hello world | upper                ->  HELLO WORLD
echo chained pipe | upper | upper        ->  CHAINED PIPE   (three stages)
cat /notes.txt | upper                   ->  the file, uppercased
ps | upper                               ->  the process list, uppercased
```

- The **first stage** may be a **builtin** or a **program**. A builtin runs
  with its output captured (the same capture the redirection machinery uses -
  a bigger output refuses rather than truncating), then streamed to stage 2.
  A program first stage streams its own output onward directly.
- **Every later stage must be a program** (it reads its predecessor's output
  as stdin); a builtin or unknown name there reports "not found (a pipeline
  stage must be a program)".
- Programs stream **directly** to the next stage - the shell is not in the
  byte path (except for the one hop out of a builtin first stage). The shell
  spawns the stages, aims each one's stdout at the next (the last at the
  console), and hands each producer a runtime capability to reach its
  consumer (`DELEGATE`); that sibling-to-sibling send is otherwise forbidden
  by the IPC send-mask, and the shell alone can grant it. A linear chain needs
  only one delegated target per task, so no special "general delegation" is
  involved. The shell then waits for every stage to exit.

A pipeline stage is a standard filter program (see `upper/src/main.rs` for
the shape to copy: stdin is `msg_recv`, output goes to its **stdout target**
so it can be a *middle* stage, EOF-in is the empty message, EOF-out is an
empty message to the next stage, finishing is `exit`). Ctrl+C interrupts and
kills them.

**`exec prog > file`** captures a program's output to a file (the redirection
machinery, accumulating rather than forwarding):

```
exec /EFI/ORBS/HELLO.BIN > out.txt
cat out.txt                          ->  Hello from a second program! ...
```

Limits: a pipeline is at most five program stages (the spawnable task slots -
a longer chain fails with "no free task slot"); combining `|` with `>`/`>>`
is refused (the last stage writes straight to the console, so there's no
capture of its output to redirect); `exec > file` capture is bounded (512
bytes, refuse-not-truncate - pipes themselves stream unbounded). A consumer
that stops reading: a program producer gives up after its own ~3-second
timeout and exits, and the shell's wait on the still-live consumer blocks
until you press **Ctrl+C** (leaving it running in the background - `kill` it
via `ps`); a builtin first stage the shell feeds directly gets its consumer
killed after the same timeout.

## Known limitations

- **Every filesystem command (`ls`, `tree`, `cat`, `mkdir`, `rmdir`, `touch`,
  `rm`, `cp`, `mv`, `writeat`), the network commands (`ping`, `resolve`, `fetch`,
  `dial`, `serve`), and `echo`/`uptime`/`clear` are `/bin` programs, not
  builtins.** They were
  externalized (they run as `/bin/ECHO`, `/bin/LS`, `/bin/PING`, etc., found via
  `$PATH`) — behaviour is identical from a user's view, but they're
  **unavailable on a boot without a mounted filesystem** (e.g. `make run`'s
  FAT16, or real hardware with no USB stick), where a bare `echo` reports
  "unknown command" instead. (The filesystem ones need a mounted disk to do
  anything regardless.) A spawned command receives the shell's current directory
  (via the `CWD_STAGE`/`GET_CWD` ABI), so a bare `ls` or a relative `cp a b`
  resolves against the cwd just as the old builtins did, and reports errors with
  a non-zero exit code. The network commands reach the network server via the
  `TO_NET` capability the shell delegates to them at spawn (a spawned program
  can't reach netd on its own). Remaining builtins: `write`, `cd`, `pwd`,
  `exec`, `exit`, the job-control commands (`ps`, `kill`, `wait`, `fg`), and
  `mount`/`selftest`/`help`.
- **Pipeline filters live in `/bin` too:** `upper` (uppercases its input),
  `grep <pattern>` (lines containing a substring), `wc` (line/word/byte
  counts), `head [N]` (first N lines, default 10), `tail [N]` (last N lines,
  default 10 — the complement of `head`; capped at 64 retained lines), `nl`
  (numbers every line, right-aligned 6-column count + tab, like `cat -n`),
  `rev` (reverses the character order of each line), and `uniq` (collapses
  runs of *adjacent* identical lines). Each reads stdin and writes to its
  stdout target, so they chain: `cat FILE | grep x | wc`,
  `ls /bin | tail 5`, `cat FILE | uniq | nl`, `echo hi | rev`. They only do
  anything as a pipeline stage (they read piped input); run bare, they just
  wait for input that never comes (Ctrl+C to abort). Each is bounded to a
  256-byte line buffer with no heap — a longer line is handled in pieces
  (`tail` truncates it), the shared caveat of these fixed-buffer filters. See
  the Pipelines section.
- **Write granularity: `write` full-replaces; `writeat`/`>>`/`cp` do
  offset writes.** The FAT32 layer has a real random-access offset-write
  primitive (`write_at`): `writeat` writes in place at any offset
  (zero-filling a past-EOF gap, capped at 1 MiB), `>>` appends at the
  file's current end, and `cp` streams a copy of any size through it.
  `write` still replaces a file's *entire* contents (bounded by the
  128-byte input line). What's still missing: no way to *shrink* a file
  except by full-replacing it (no truncate-to-length), and `writeat`
  won't create a missing file.
- **`mv` has no cycle detection** beyond the trivial self-move guard
  (moving a directory into its own descendant isn't prevented).
- **Every filesystem failure prints its specific reason** ("already
  exists", "no such file or directory", "disk full", ...) — the
  filesystem server returns one named `FS_ERR_*` code per real cause
  (see `architecture.md`'s protocol notes), decoded by one shared
  message table in the shell.
- **`cd ..` from `/` stays at `/`** rather than erroring — a minor,
  deliberate rough edge, not a bug.
- **Tasks are MMU-isolated from each other** — each runs under its own
  page-table view where only its own memory is accessible, so a buggy
  or hostile program faults (and dies alone) rather than corrupting the
  shell or the filesystem server (see `processes.md`'s "known rough
  edges" and `architecture.md`'s memory layout).
