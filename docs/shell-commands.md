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
implement a completely different command set.

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
- **Output redirection (`> file` / `>> file`) and pipes (`|`) work** — see
  the dedicated sections below (pipes now include `program | program` and
  `exec prog > file`). **No input redirection (`<`), globbing, `;`/`&&`
  chaining, environment variables, command history, or tab completion.**
  One command per line, typed in full, every time.
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
| `help` | `help` | Lists every builtin command name. | Static text, no syscall. |
| `echo` | `echo [words...]` | Prints all words after `echo`, space-separated. | The one command that uses more than its first argument. |
| `uptime` | `uptime` | Prints the preemption tick count since boot. | Backed by `get_ticks` — real kernel state, not a demo. |
| `clear` | `clear` | Clears the screen (ANSI `\x1b[2J\x1b[H`). | A raw escape sequence the shell sends itself, not a syscall — the console has no notion of a screen. |
| `pwd` | `pwd` | Prints the current working directory. | Shell-local state only, no syscall. |
| `ls` | `ls [path]` | Lists a directory's entries, `name` for files and `name/` for subdirectories. Defaults to the current directory. | Truncates rather than erroring if the listing doesn't fit a 512-byte buffer (the ABI's per-buffer cap). |
| `cat` | `cat <file>` | Prints a file's contents. | Streams the file in chunks via the grant/safecopy bulk path, so it prints a file of *any* size (no truncation) without ever holding the whole thing; a file argument is required. |
| `cd` | `cd [path]` | Changes the current working directory. | Validates the target exists and is a directory first (via a listing call — there's no dedicated "does this exist" syscall). |
| `mkdir` | `mkdir <dir>` | Creates an empty subdirectory. | Fails with a specific message for each reason (already exists, invalid name, parent missing, disk full — the kernel returns distinct `FS_ERR_*` codes now). Grows a full parent directory by a cluster automatically (as do `touch`/`write`/`cp`/`mv` when creating entries). |
| `rmdir` | `rmdir <dir>` | Removes an *empty* subdirectory. | Fails if it doesn't exist, isn't empty, or is root. |
| `touch` | `touch <file>` | Creates an empty (zero-byte) file, or succeeds silently if one already exists there. | There's no RTC on this kernel, so unlike real `touch`, an existing file's "timestamp" isn't updated — nothing happens, successfully. Fails if the target is a directory. |
| `rm` | `rm <file>` | Removes a file. | Fails if it doesn't exist or is a directory — use `rmdir` for those. |
| `write` | `write <file> [words...]` | Joins every word after the filename with a single space (same style as `echo`) and writes the result as the file's *entire* contents, replacing whatever was there. Creates the file if it doesn't exist. | `write <file>` with no words truncates the file to empty (a real, valid case, not an error). Fails if the target is an existing directory or the parent is missing. |
| `writeat` | `writeat <file> <offset> <text...>` | A **random-access write**: writes the text at byte `offset`, overwriting bytes *in place* and leaving everything outside the written window intact (unlike `write`, which replaces the whole file). If `offset` is past the end of the file, the gap is **zero-filled** on disk. | The file must **already exist** — `writeat` does not create it (use `write`/`touch` first). The text is bounded by the input line. A past-EOF gap is capped at 1 MiB (a larger offset reports a device I/O error). Fails on a missing file, or if the target is a directory. |
| `cp` | `cp <src> <dst>` | Copies `src`'s contents to `dst`, creating `dst` if it doesn't exist or replacing it if it does. | **Streams the copy one chunk at a time via the FAT32 offset-write primitive, so it handles a file of any size** (bounded by disk space, not a shell buffer) — the old 2048-byte ceiling is gone. `cp x x` (a file onto itself, however the two paths are spelled) is **refused**: streaming truncates `dst` first, which would destroy the source. A missing source leaves `dst` untouched. Non-atomic: an interrupted copy leaves `dst` truncated (a partial copy is a wrong copy). No recursive directory copy. |
| `ps` | `ps` | One line per scheduler slot: `unused`, `runnable`, `blocked (waiting)`, or `` exited - `wait` to collect its status `` (a zombie holding its slot). | The caller can't distinguish "running right now" from "runnable" — it is, by definition, the one running when it asks. Output is redirectable like any other command's. |
| `kill` | `kill <n>` | Destroys task `n` (see `ps` for numbers). | Tasks 0 (this shell), 1 (idle), and 2 (the filesystem server) are protected. A killed task's slot becomes spawnable again and its memory is reclaimed when allocation order allows. |
| `fg` | `fg <n>` | Hands the keyboard to task `n` — e.g. `exec /EFI/ORBS/SH.BIN` then `fg 2` gives a real nested shell session; its `exit` hands the keyboard back. | **Ctrl+C is the escape hatch**: typed while another task owns the keyboard, the kernel reclaims it for this shell (the foregrounded task keeps running in the background — Ctrl+C is keyboard reclamation, not a signal; `kill` the task if it should die too). Ownership also reverts automatically when the foregrounded task exits or is killed. While this shell owns the keyboard, Ctrl+C (like every unhandled control byte) is ignored by the line editor. |
| `send` | `send <n> <words...>` | Sends the words (space-joined, like `write`) as one IPC message (≤64 bytes) to task `n`'s mailbox. | Fails distinctly for a missing task, an over-long message, or a full mailbox (4 pending max). A dead task's queued mail dies with it. |
| `recv` | `recv` | Blocks until a message arrives and prints `task N: <message>`. | Ctrl+C interrupts, like `wait`; typing during a blocked `recv` is otherwise discarded. |
| `wait` | `wait <n>` | Blocks until task `n` dies, then reports its exit status — which is also what *reaps* it (an exited task holds its slot as a zombie, shown by `ps`, until waited). | Ctrl+C interrupts the wait (the task keeps running); any other typing during a wait is discarded, like typing at a busy foreground job in `sh`. Waiting on tasks 0-3 (shell, idle, fsd, cond) or yourself is refused (they never die). **Behavior change from earlier builds:** an un-waited exited task holds its slot — `exec` something three times without waiting and the third fails with "no free task slot" until you `wait` (or the task is `kill`ed, which reaps immediately). |
| `exit` | `exit` | Asks the kernel to destroy this task (`exit` syscall). | Always refused for the boot shell itself (it's the sole keyboard owner - the kernel returns `EXIT_DENIED` and the shell prints why); exists as the reference for how a replacement/spawned program ends itself. `hello/` (`exec /EFI/ORBS/HELLO.BIN`) demonstrates a successful exit. |
| `exec` | `exec <path> [args...]` | Loads the program at `path` and starts it as a new, independent task alongside this shell (see `ps`) — spawn semantics, not exec-replaces-current-process. | Any words after the path become the program's **argv** (`argv[0]` is the path), read via the `GET_ARGC`/`GET_ARG` syscalls. Reads the program via the filesystem server in 512-byte chunks, stages them into the kernel, then spawns (see `architecture.md`'s "Dynamic task creation"). Fire-and-forget (not waited); fails with the specific reason: no such file, is a directory, not a loadable program (bad ELF), too large, or no free task slot. |
| `mount` | `mount` | Makes a USB storage stick's FAT32 filesystem available — the Parallels workflow: passthrough USB attaches a few seconds *after* boot, so boot, wait a moment, then `mount`. | Two halves under the hood: the filesystem server first retries mounting whatever block device the kernel already holds; only if that yields nothing does the kernel rescan the USB ports and install a found stick as a *replacement* device (safe exactly because nothing is mounted), then the server mounts it. An unmountable boot-time disk (e.g. `make run`'s FAT16) therefore never blocks a later stick. |
| `ping` | `ping <a.b.c.d>` | Sends an ICMP echo request to a host and reports whether a reply came back (`reply from …`, `no reply … (timeout)`, `… is unreachable (no ARP reply)`, or `no network interface this boot`). | Asks the network server (`netd`) to ARP-resolve the host and ICMP-echo it — the whole protocol stack lives there; the shell just packs the target and reads the status. **Guest-initiated only** (the guest can't yet *answer* a host's ping — needs async receive). QEMU only for now (Parallels' virtio-net is PCI, unsupported); the source IP `10.0.2.15` and /24 assumption are the QEMU user-net convention. |
| `resolve` | `resolve <hostname>` | Looks up a hostname's IPv4 via DNS and prints `<host> is a.b.c.d`, or `could not resolve …` / `no response for …`. | Asks `netd` to send a DNS A-record query over **UDP** to the QEMU user-net DNS server (`10.0.2.3`), which forwards to the host's resolver — so it resolves *real* names (`resolve example.com`). A records only; QEMU only (same reasons as `ping`). |
| `fetch` | `fetch <hostname>` | Opens a **client TCP** connection to the host on port 80, sends a minimal HTTP `GET /`, and prints the response (e.g. `fetch example.com` → `HTTP/1.1 200 OK …`). | Chains the whole stack in `netd`: resolve → route (via the gateway for off-subnet hosts) → TCP handshake → HTTP → clean teardown. **Client only** (can't serve TCP yet), one connection, no retransmission; the response is capped at what fits one IPC reply (the full length is reported). QEMU only (same reasons as `ping`). |
| `env` | `env` | Lists every environment variable as `NAME=VALUE`, one per line. | Shell-local variables (`PATH` is preset to `/bin`). Redirectable like any command's output. |
| `set` / `export` | `set NAME=VALUE` | Sets (or replaces) an environment variable. `export` is an alias. | The value is a **single token** — no quoting, so `set X=a b` sets `X` to `a` (same limitation as `echo`). Fails if the name/value is too long or the table is full (16 vars). Variables are **shell-local** — not yet passed into spawned programs. |
| `unset` | `unset NAME` | Removes an environment variable. | Silent if it wasn't set. |
| `selftest` | `selftest` | Exercises the two historically-crashing code patterns (`write!`/`core::fmt` formatting, slice/str-vs-literal comparison) and prints pass/fail lines. | The relocating loader's permanent acceptance test — see `processes.md`'s "Binary format". |
| `mv` | `mv <src> <dst>` | Renames or moves a file or directory to `dst` — and if `dst` is an existing directory, moves `src` *into* it keeping its basename (`mv notes.txt backup/`), like real `mv`. | A `dst` that exists and isn't a directory still fails rather than being overwritten. `mv x x` is refused ("source and destination are the same"). Moving a directory to a different parent correctly updates its own `..` entry, so `cd ..` inside it still resolves to the *new* parent afterward. No cycle detection beyond the trivial self-move guard. |

Any other input is looked up as a **program on `$PATH`** (each `:`-separated
directory in turn, e.g. `/bin/<name>`) and run in the foreground with the
whole line as its argv — so a command binary is invoked by bare name, no
`exec` or leading path needed. Only if no PATH directory has it does it print
`unknown command: <word>`. `$VAR` references anywhere in a line are expanded
from the environment before dispatch. A blank line (just Enter, or only
whitespace) does nothing.

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

Pipe a command's output into a freshly spawned program with `|`. The left
side can be a **builtin** or a **program**:

```
echo hello | /EFI/ORBS/UPPER.BIN                  ->  HELLO
ls | /EFI/ORBS/UPPER.BIN
/EFI/ORBS/HELLO.BIN | /EFI/ORBS/UPPER.BIN          ->  HELLO FROM A SECOND PROGRAM! ...
```

- **Builtin left** (`echo … | prog`): the builtin runs with its output
  captured (the same capture the redirection machinery uses - a bigger
  output refuses rather than truncating), then streamed to the spawned
  program.
- **Program left** (`/prog_a | /prog_b`): both sides are spawned programs,
  and the producer streams its output **directly** to the consumer - the
  shell is not in the byte path. The shell spawns the consumer, spawns the
  producer with its stdout aimed at the consumer, and hands the producer a
  runtime capability to reach it (`DELEGATE`); that sibling-to-sibling send
  is otherwise forbidden by the IPC send-mask, and the shell alone can grant
  it (only it holds the spawnable slots' send-caps). The shell then just
  waits for both to exit.

Either way the right side is a standard filter program (see
`upper/src/main.rs` for the shape to copy: stdin is `msg_recv`, output is
its stdout target - the console by default - EOF is the empty message,
finishing is `exit`). The shell waits for the program(s) to exit before
prompting again; Ctrl+C interrupts and kills them.

**`exec prog > file`** captures a program's output to a file (the same
relay, but the shell accumulates the output and writes it rather than
forwarding it to another program):

```
exec /EFI/ORBS/HELLO.BIN > out.txt
cat out.txt                          ->  Hello from a second program! ...
```

Limits, deliberate for v1: one `|` per line (no `a | b | c` - that needs
shell-side chaining of relays); the right side is exactly one program path
(programs take no arguments); the only *producer* program shipped is
`HELLO.BIN` (any generator follows the same stdout-target pattern; `upper`
is a consumer/filter); `exec > file` capture is bounded (512 bytes,
refuse-not-truncate - pipes themselves stream unbounded). A consumer that
stops reading behaves differently in the two cases: for a **builtin-left**
pipe the shell is feeding it, so it kills the consumer after a ~3-second
real-tick timeout; for a **program-left** pipe the shell is out of the byte
path, so the producer gives up after its own ~3-second timeout and exits,
and the shell's wait on the still-live consumer blocks until you press
**Ctrl+C** (which leaves the consumer running in the background - `kill` it
via `ps`).

## Known limitations

- **`echo`, `uptime`, and `clear` are `/bin` programs, not builtins.** They
  were externalized (they run as `/bin/ECHO`/`/bin/UPTIME`/`/bin/CLEAR`, found
  via `$PATH`) — behaviour is identical from a user's view, but they're
  **unavailable on a boot without a mounted filesystem** (e.g. `make run`'s
  FAT16, or real hardware with no USB stick), where a bare `echo` reports
  "unknown command" instead. The other commands are still builtins.
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
