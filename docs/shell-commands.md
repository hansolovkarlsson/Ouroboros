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
- **Output redirection (`> file` / `>> file`) works** — see the
  dedicated section below. **No pipes, input redirection (`<`),
  globbing, `;`/`&&` chaining, environment variables, command history,
  or tab completion.** One command per line, typed in full, every time.
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
- **No mounted filesystem this boot** (e.g. `make run`'s fast dev-loop
  disk, which is FAT16 — see `fat32.rs`) makes every disk command print
  one shared message rather than a command-specific error, since no path
  could ever resolve in that state. Use `make run-image` (or a real
  Parallels/`esp.img` boot) for disk commands to do anything.
- **Only 8.3 short filenames.** No long-filename (LFN) support in the
  underlying FAT32 reader — `mkdir`/`touch` additionally only accept
  ASCII alphanumerics, `_`, and `-` in names they create (one optional
  `.` splitting an up-to-8-character base from an up-to-3-character
  extension). Existing on-disk names outside that set (created by a real
  formatter) can't be created by this shell, and LFN entries aren't
  parsed at all — they won't show up in `ls`.

## Commands

| Command | Syntax | Description | Notes |
|---|---|---|---|
| `help` | `help` | Lists every builtin command name. | Static text, no syscall. |
| `echo` | `echo [words...]` | Prints all words after `echo`, space-separated. | The one command that uses more than its first argument. |
| `uptime` | `uptime` | Prints the preemption tick count since boot. | Backed by `get_ticks` — real kernel state, not a demo. |
| `clear` | `clear` | Clears the screen (ANSI `\x1b[2J\x1b[H`). | A raw escape sequence the shell sends itself, not a syscall — the console has no notion of a screen. |
| `pwd` | `pwd` | Prints the current working directory. | Shell-local state only, no syscall. |
| `ls` | `ls [path]` | Lists a directory's entries, `name` for files and `name/` for subdirectories. Defaults to the current directory. | Truncates rather than erroring if the listing doesn't fit a 512-byte buffer (the kernel's per-syscall cap). |
| `cat` | `cat <file>` | Prints a file's contents. | Truncates at 512 bytes (the kernel's per-syscall cap) with a notice if the file is larger; a file argument is required. |
| `cd` | `cd [path]` | Changes the current working directory. | Validates the target exists and is a directory first (via a listing call — there's no dedicated "does this exist" syscall). |
| `mkdir` | `mkdir <dir>` | Creates an empty subdirectory. | Fails with a specific message for each reason (already exists, invalid name, parent missing, disk full — the kernel returns distinct `FS_ERR_*` codes now). Grows a full parent directory by a cluster automatically (as do `touch`/`write`/`cp`/`mv` when creating entries). |
| `rmdir` | `rmdir <dir>` | Removes an *empty* subdirectory. | Fails if it doesn't exist, isn't empty, or is root. |
| `touch` | `touch <file>` | Creates an empty (zero-byte) file, or succeeds silently if one already exists there. | There's no RTC on this kernel, so unlike real `touch`, an existing file's "timestamp" isn't updated — nothing happens, successfully. Fails if the target is a directory. |
| `rm` | `rm <file>` | Removes a file. | Fails if it doesn't exist or is a directory — use `rmdir` for those. |
| `write` | `write <file> [words...]` | Joins every word after the filename with a single space (same style as `echo`) and writes the result as the file's *entire* contents, replacing whatever was there. Creates the file if it doesn't exist. | `write <file>` with no words truncates the file to empty (a real, valid case, not an error). Fails if the target is an existing directory or the parent is missing. |
| `cp` | `cp <src> <dst>` | Copies `src`'s contents to `dst`, creating `dst` if it doesn't exist or replacing it if it does. | Reads `src` fully into a 512-byte buffer (the kernel's per-syscall cap) before writing anything to `dst`, so copying a file onto itself is safe. Refuses (rather than truncating) if `src` is larger than that buffer. No recursive directory copy. |
| `ps` | `ps` | One line per scheduler slot: `unused`, `runnable`, `blocked (waiting)`, or `` exited - `wait` to collect its status `` (a zombie holding its slot). | The caller can't distinguish "running right now" from "runnable" — it is, by definition, the one running when it asks. Output is redirectable like any other command's. |
| `kill` | `kill <n>` | Destroys task `n` (see `ps` for numbers). | Tasks 0 (this shell) and 1 (idle) are protected. A killed task's slot becomes spawnable again and its memory is reclaimed when allocation order allows. |
| `fg` | `fg <n>` | Hands the keyboard to task `n` — e.g. `exec /EFI/ORBS/SH.BIN` then `fg 2` gives a real nested shell session; its `exit` hands the keyboard back. | **Ctrl+C is the escape hatch**: typed while another task owns the keyboard, the kernel reclaims it for this shell (the foregrounded task keeps running in the background — Ctrl+C is keyboard reclamation, not a signal; `kill` the task if it should die too). Ownership also reverts automatically when the foregrounded task exits or is killed. While this shell owns the keyboard, Ctrl+C (like every unhandled control byte) is ignored by the line editor. |
| `send` | `send <n> <words...>` | Sends the words (space-joined, like `write`) as one IPC message (≤64 bytes) to task `n`'s mailbox. | Fails distinctly for a missing task, an over-long message, or a full mailbox (4 pending max). A dead task's queued mail dies with it. |
| `recv` | `recv` | Blocks until a message arrives and prints `task N: <message>`. | Ctrl+C interrupts, like `wait`; typing during a blocked `recv` is otherwise discarded. |
| `wait` | `wait <n>` | Blocks until task `n` dies, then reports its exit status — which is also what *reaps* it (an exited task holds its slot as a zombie, shown by `ps`, until waited). | Ctrl+C interrupts the wait (the task keeps running); any other typing during a wait is discarded, like typing at a busy foreground job in `sh`. Waiting on tasks 0/1 or yourself is refused. **Behavior change from earlier builds:** an un-waited exited task holds its slot — `exec` something three times without waiting and the third fails with "no free task slot" until you `wait` (or the task is `kill`ed, which reaps immediately). |
| `exit` | `exit` | Asks the kernel to destroy this task (`exit` syscall). | Always refused for the boot shell itself (it's the sole keyboard owner - the kernel returns `EXIT_DENIED` and the shell prints why); exists as the reference for how a replacement/spawned program ends itself. `hello/` (`exec /EFI/ORBS/HELLO.BIN`) demonstrates a successful exit. |
| `mv` | `mv <src> <dst>` | Renames or moves a file or directory to `dst` — and if `dst` is an existing directory, moves `src` *into* it keeping its basename (`mv notes.txt backup/`), like real `mv`. | A `dst` that exists and isn't a directory still fails rather than being overwritten. `mv x x` is refused ("source and destination are the same"). Moving a directory to a different parent correctly updates its own `..` entry, so `cd ..` inside it still resolves to the *new* parent afterward. No cycle detection beyond the trivial self-move guard. |

Any other input prints `unknown command: <word>`. A blank line (just
Enter, or only whitespace) does nothing.

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
- `cat`'s two display niceties stay off the captured bytes: the
  tidy-terminal trailing newline and the truncation notice both go to
  the screen only, so `cat a > b` copies `a`'s bytes exactly (bounded
  by the same 512-byte read buffer as `cp` — a larger file's copy is
  truncated the same way `cat`'s display is, notice on screen).

Limits, all refuse-outright rather than write-something-wrong:

- A command's redirected output is captured in a 512-byte buffer; a
  command that emits more than that prints `output too large to
  capture` and **nothing is written at all** (no current builtin can
  exceed it in normal use).
- `>>` is shell-side read-concatenate-rewrite (the kernel has no append
  primitive — every `fs_write_file` is a full replace), bounded by the
  kernel's own 512-byte per-syscall buffer cap (`MAX_USER_LEN`,
  `kernel/src/syscall.rs`): appending where existing content plus new
  output would exceed 512 bytes prints `file too large to append to`
  and leaves the file untouched.

## Known limitations

- **`write`/`cp` always fully replace a file's contents — no append or
  partial/offset writes at the syscall/FAT32 layer.** `>>` provides
  *bounded* append purely shell-side (see "Output redirection" above) —
  a real append primitive would need a new syscall and FAT32-layer
  support that don't exist yet. `write` content is also bounded by
  the 128-byte input line, so nothing typed at the shell can ever
  exceed one FAT32 cluster's worth of content; `cp` is bounded by its
  own 256-byte read buffer instead.
- **`mv` has no move-into-an-existing-directory-keeping-basename
  shortcut** (real `mv`'s most common everyday case beyond a plain
  rename) and no cycle detection (moving a directory into its own
  descendant isn't guarded against).
- **Every filesystem error beyond "no mounted filesystem" collapses to
  one generic message per command** (e.g. `mkdir`'s "already exists, bad
  name, parent missing, or disk full" covers four genuinely different
  failures) — the underlying syscalls report only two sentinel values,
  not a specific reason. See `architecture.md`'s syscall table.
- **`cd ..` from `/` stays at `/`** rather than erroring — a minor,
  deliberate rough edge, not a bug.
- **Disk-command pointer/length arguments are trusted by the kernel, not
  validated** against this program's actual mapped region — fine while
  this is the only, trusted userland program (see `processes.md`'s
  "known rough edges").
