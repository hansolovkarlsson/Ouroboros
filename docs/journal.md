# Ouroboros development journal

A chronological dev-log — what was worked on each day and why, in narrative
form. For the condensed milestone record see [`CHANGELOG.md`](CHANGELOG.md); for
the deeper design-and-bugs retrospectives see the postmortems under `docs/`;
for the forward plan see [`roadmap.md`](roadmap.md).

---

## 2026-08-27 (cont.) — GPT CRC validation + backup fallback

Closed the "GPT parsed but CRCs not validated on read" open gap. `fsd`'s
`partition::discover` trusted the "EFI PART" signature and read the entry
array without checking either CRC32. Now `try_gpt` validates the header CRC
(over `HeaderSize` bytes, CRC field zeroed) and the entry-array CRC (a
table-free bitwise CRC-32, reflected `0xEDB88320`, folded incrementally so
the array is CRC'd sector-by-sector), and a corrupt *primary* falls back to
the *backup* GPT at the last LBA; both corrupt → no partitions. Also detect a
GPT by a `0xEE` protective-MBR entry, not just the header signature, so a
disk whose primary signature is itself smashed is still recovered from the
backup.

The interesting part was *testing* it, because two layers of firmware get in
the way. I flipped a bit in the primary header of `espgpt.img` and booted it:
`fsd` mounted — but when I checked the on-disk image afterward, the primary
header was *valid again*. EDK2/OVMF **auto-repairs a corrupt primary GPT from
the backup during boot**, so by the time `fsd` reads the disk there's nothing
wrong with the primary. And corrupting *both* copies just makes the firmware
refuse to boot at all (`CheckCrc32: Crc check failed` → it drops to the EFI
shell), so the kernel never runs. Either way a QEMU boot can't actually
exercise `fsd`'s fallback. So I wrote a small host harness that `#[path]`-includes
the **real `partition.rs`** and drives it through a mock `Disk` over the raw
image bytes — no firmware in the loop. Five cases pass: clean → mounts on
primary; corrupt-primary-header and corrupt-primary-array → both fall back to
the backup; corrupt-both → zero; plain-MBR → still one partition (no
regression). I also cross-checked the bitwise CRC against `zlib.crc32` and the
values `mkgpt.py` writes — byte-identical for both the primary and backup
headers. The clean `run-image-gpt` boot still mounts, so nothing regressed at
the real-boot level either.

Nice reminder that "test it on real hardware/firmware" isn't automatically the
strongest test — here the firmware's own robustness (repair + reject) actively
*hides* the code path under test, and a host harness driving the real module
was the honest way to see `fsd`'s fallback actually fire.

## 2026-08-27 (cont.) — FAT32 long-filename *write*

Closed the "FAT32 long-filename write" follow-up from the roadmap. LFN was
readable but not creatable: `fsd`'s FAT32 arm only ever wrote 8.3 short
entries, so `touch archive.tar.gz` or `write LongFileName.txt …` failed with
"invalid name". Now a name that doesn't fit 8.3 gets a generated `NAME~N`
short alias plus a contiguous run of LFN entries carrying the real name;
8.3-fitting names still take the old plain-short-entry path byte-for-byte, so
nothing existing changed.

The pieces, all self-contained in `fat32.rs`: `insert_named_entry` (the one
funnel `mkdir`/`touch`/`write_file`/`mv` route through, deciding 8.3-vs-LFN),
`generate_short_alias` (`~N` incremented until unique — the base trimmed so
even `~999999` fits 8 chars), `build_name_entries`/`put_lfn_chars` (the
on-disk entry run, checksum-stamped, written high-sequence-first), and
`write_entry_run`/`place_entries` (find a physically-contiguous run of free
slots, extend the directory by a zeroed cluster if needed). I also did the
bonus the roadmap flagged: `free_entry_with_lfn` frees a deleted file's LFN
run, so `rm`/`rmdir`/`mv` no longer strand orphaned LFN entries — it matches
the target by exact on-disk location (so an `mv` dst inserted into the same
directory isn't mistaken for it) and only frees a preceding run whose
checksum matches.

Testing leaned on the foreign-observer discipline this project keeps: drove
the guest shell unattended over `make run-image` (the FIFO recipe), creating
`longfilename1.txt`/`longfilename2.txt` (aliases collide →
`LONGFI~1.TXT`/`LONGFI~2.TXT`), a long-named directory, a nested long-named
file, then `rm` and `mv` of long names. The guest read every name back; then
**macOS's own FAT driver** mounted the guest-written image and showed all the
long names and contents; and **`fsck_msdos`** passed its directory phase with
no orphaned-LFN/checksum/duplicate-short-name complaints — the real proof, an
implementation that shares none of my code. Only wrinkle was a pre-existing,
advisory FSInfo free-count warning (`fsd` never maintains that hint on
writes), unrelated to LFN. No faults, no guard-page overflow. Left
case-preservation for lowercase-8.3 names (`File.txt` → `FILE.TXT`) alone on
purpose — the gap was `>8.3` names, and that's closed.

## 2026-08-27 (cont.) — reading the neighbours: Redox OS and the Pi-4 tutorials

Not code — a research pass Hans asked for, on two outside resources: Redox OS
(the mature Rust microkernel) and the `rust-raspberrypi-OS-tutorials` repo. I
ran two parallel research agents and wrote up `docs/research-redox-and-pi.md`.

The Redox half had a genuinely useful conclusion I didn't expect going in:
**Ouroboros already structurally *is* a small Redox.** Both arrived
independently at userspace fs/net/console daemons, supervised driver restart, a
uniform resource protocol (their URL "schemes" ≈ our 9P verbs + per-task
namespaces), and a non-POSIX kernel with POSIX pushed to a userland libc —
which is exactly the conclusion our own posix-divergence postmortem reached. So
the "what to adopt" list is short and specific rather than sprawling: `relibc`
(the existence proof for our libc-portability plan — and it targets *both* Redox
and Linux, so it's host-testable), making the namespace *itself* the capability
boundary (Redox's null-namespace sandbox unifies two things we built separately
— namespaces and the send-mask), and RedoxFS's CoW+checksums shape for the
cluster-redundancy direction. Schemes vs. 9P turned out to be the *same*
architecture in different spelling, so there's nothing to copy there except the
namespace-as-security-boundary idea. I threaded those three into the roadmap's
portability/security/redundancy north-stars where they'll actually be worked.

The Pi half is a bring-up cookbook for when the boards arrive. The load-bearing
call: we boot via UEFI, the tutorials boot a raw `kernel8.img` — two different
worlds — so the plan is to try the pftf/RPi4 EDK2 UEFI+ACPI firmware *first*,
because a Pi 4 under it exposes UEFI + ACPI + a GOP framebuffer, and our
existing stack (UEFI loader, ACPI MADT → `gicv2.rs` for the Pi 4's GIC-400,
GOP `fbconsole`) should carry over largely unchanged. The tutorials stay the
reference for the raw BCM2711 facts (peripheral base `0xFE00_0000`, the GIC
addresses, PL011-not-mini-UART, the serial rig) if UEFI proves unworkable.

## 2026-08-27 (cont.) — keeping the roadmap forward-looking

A cleanup Hans asked for: the roadmap had grown to 1700 lines, half of it
finished work. Split it — `roadmap.md` back down to ~530 forward-looking lines
(open frontier, remaining follow-ups, north-stars, open gaps), and the finished
arcs (microkernel bring-up, network stack, filesystems, disk management,
standalone binaries, pipelines, and the done parking-lot entries) moved verbatim
into a new `roadmap-completed.md` — the plan-shaped companion to CHANGELOG.md's
condensed milestone log. The moved text keeps its original "deferred" tails for
context, with a note that those open items now live back in the roadmap.

## 2026-08-27 (cont.) — shell wildcards and tab completion

Two classic interactive-shell features. Globbing was the straightforward one:
expand `*`/`?` tokens against the filesystem in `run_line` before dispatch, with
the standard iterative backtracking matcher and bash's "no match stays literal"
rule. Tab completion was a bit more fiddly — find the current word, list the
directory, and either complete a lone match (replacing the typed prefix with the
real filename case), extend to the common prefix of several, or list them and
redraw the line.

The interesting part was a link failure that cost a proper bisection: the shell
suddenly wouldn't link, `R_AARCH64_ABS64 ... referenced by core`. The project has
long known that `core::fmt` can't be PIE-linked, but I hadn't hit it in a while.
Stubbing pieces one by one showed it wasn't `fs_list_dir` or `resolve_path`
(already live) — it was the `&str` slicing itself. Slicing a `&str` by a runtime
index emits `str`'s char-boundary panic, which *formats* the offending string,
which drags in the unlinkable `core::fmt`. Byte slicing doesn't. So the glob and
completion code got rewritten to work entirely in `&[u8]`, converting back with
`from_utf8` only where a `&str` was actually needed. Worth a memory: never slice
a `&str` by a runtime index in a `/bin` program. After that, both features came
up clean — `echo *.txt` matched, `cat ba<Tab>` filled in `BANANA.TXT`.

## 2026-08-27 (cont.) — man pages

`man <command>`. The first question was how much formatting the console can do,
and the honest answer settled the design: `cond`'s ANSI parser handles clear and
home and *silently drops everything else*, including SGR — so bold and colour
would render as nothing on the framebuffer (they'd work over the QEMU serial
line, but that's not the real target). So: plain text, with UPPERCASE section
headers doing the visual structure the way real man pages always have. The
implementation is deliberately boring — pages are plain files under `/man/`, and
`man` is basically `cat` with a `/man/` prefix, a friendly "no manual entry"
message, and `\n`→`\r\n` translation for the console. Keeping the pages as files
(not baked into the binary) means adding one is just dropping a file in
`manpages/`. A nice incidental check: `man partition` exercises FAT long
filenames (9 chars, past 8.3), and it read back fine. Wrote pages for the whole
command set — the file commands, the filters, the net commands, the builtins —
so there's a real reference now, not just terse `-?` lines.

## 2026-08-27 (cont.) — `-?` usage help everywhere

A small usability sweep: every command that takes arguments now answers `-?` with
a one-line usage. The trick to keeping it from being tedious was a single ulib
helper, `usage_if_requested(b"usage: ...")`, that each `/bin` program calls as its
first line — one line per program — and a small `builtin_usage` table the shell
consults before dispatching a builtin. The judgment calls were about what to
*leave alone*: `echo` and `args` print their arguments, so intercepting `-?` would
be wrong, and the pure filters (`wc`/`rev`/`uniq`/…) have no options to describe.
Everything else — the file commands, the net commands, the filters that take an
argument, the arg-taking builtins — got it. Nice to type `mount -?` or `dial -?`
and get a reminder instead of guessing.

## 2026-08-27 (cont.) — shrinking the shell: more/less/send/recv/selftest to /bin

With the keyboard arc in place, the obvious follow-up: pull everything out of the
shell that doesn't need to be there, and leave only the parts that genuinely are
the shell — cwd, namespace, environment, job control, the disk/power commands
that must run with no disk, and remote exec. The flagship extraction was the
pager. It had been a builtin purely because it reads the keyboard; now a `/bin`
program can, so out it went. `more <file>` was trivial. `cmd | more` took one
insight: the pager, as a pipeline's *last* stage, needs the keyboard too, so the
shell now fg's the last stage of every pipeline (harmless for `grep`/`wc`, which
own it and never read). Two small bugs surfaced doing it — a piped `more` handed
`MSG_RECV` its whole 256 KB heap as the receive buffer (rejected, since messages
cap at 768), and I'd forgotten the last-stage fg at first — both quick fixes.
`send`/`recv`/`selftest` came along for the ride; the nice moment was `selftest`
running as a `/bin` binary and proving `core::fmt` links under PIE from a spawned
program, not just the shell. The shell's builtin list is now short and every
entry earns its place.

## 2026-08-27 (cont.) — the keyboard arc: interactive programs can be `/bin` now

The big one, and it came straight out of a question: does a program that reads the
keyboard (a BASIC interpreter, an editor) have to be built into the shell? The
answer was "only because of one missing piece," so I built the piece. Keyboard
input goes to a single owner, and nothing ever handed that ownership to a
foreground program — so the shell now `FG`s a command before it `WAIT`s on it, and
the kernel already reverts ownership when the command dies. That's the whole
unlock: `readkey`, a throwaway echo program, ran as an ordinary `/bin` binary and
printed the keys I typed. First time a spawned program in this OS has read the
keyboard.

The subtle half was Ctrl+C. It used to just steal the keyboard back and leave the
program running (fine when nothing owned the keyboard but the shell; a deadlock
once a foreground program reads input). So I made Ctrl+C *terminate* the
foreground program — the escape hatch marks it, and `on_tick` does the kill using
the exact teardown the supervisor's crash-restart already uses (current-task vs
not). The one genuinely hard case was a *compute-bound* program that never reads:
nobody polls the keyboard for it, so I added a once-per-tick poll of a running
foreground owner. That can eat type-ahead, but only while a program is actively
running rather than blocked on a read — and a program waiting for input is
blocked, so in practice it's a non-issue. Terminate, not a signal, is the honest
limitation; an editor that wants to catch Ctrl+C needs real signals later. But the
door's open: the next editor or REPL is just a program in `/bin`.

## 2026-08-27 (cont.) — moving `pwd` and `write` out to `/bin`

A cleanup prompted by a good question: which builtins are builtins by *necessity*
and which are just leftovers? Going through them honestly, most have a real
reason — `cd`/`bind` change the shell's own cwd/namespace, `more`/`less` need the
keyboard only the shell owns, `erase`/`partition`/`format` have to run when
nothing is mounted (so `/bin` can't be loaded). But two didn't: `pwd` and
`write`. `pwd` just prints the cwd, which the shell already hands every spawned
program; `write` just writes a file, like every other `/bin` file command. The
nice discovery was that the old `write` builtin *already* word-split and rejoined
with single spaces — the "raw command line" reason in its doc comment was never
actually true of the code — so an argv-based `/bin/write` is a byte-for-byte
behavioural match, not an approximation. Both moved out cleanly and verified
(`cd /d; pwd` → `/d`, `write` + `cat` round-trip). Notably I *didn't* move the
two that were suggested first, `more`/`less` and `partition`/`format` — they're
the two with the strongest reasons to stay (keyboard ownership; no-disk
bootstrap), which was worth saying out loud rather than mechanically obliging.

## 2026-08-27 (cont.) — a pager: `more` (and `less`)

Hans asked for `more` or `less` — "pick one". They're forward-vs-backward
cousins; I made `more` the pager and `less` an alias for it (backward scrolling
would need a lot more, and our content is small). The design was decided for me
by one fact: a pager has to read the keyboard *while it's running*, and this
kernel gives keyboard input to exactly one owner — the boot shell, task 0. A
spawned `/bin` program never becomes that owner (there's a whole doc comment in
`tasks.rs` about why, born from a bug where two programs split keystrokes letter
by letter). So a pager simply cannot be a `/bin` program; it has to be a builtin
that runs inside the shell, which already holds the keyboard. That settled it.

The rest fell out cleanly by reusing what was there: the shell's 256 KB heap
buffer and its `Output::Capture` sink already exist for `>` redirects and
pipeline heads, so `<cmd> | more` just intercepts the trailing `| more`,
dispatches the producer with a capturing sink, and pages the buffer; `more <file>`
reads the file into the same buffer. The only fiddly bit was erasing the
`--More--` prompt, and `cond` turned out to already handle `\r`, so a
carriage-return-and-spaces wipe did it. Space pages, Enter line-steps, q quits —
all confirmed against a QEMU guest driven through piped stdin.

## 2026-08-27 (cont.) — `ls` grows up: columns, sort, and `-l`

The `ls` that just dumped names one per line always felt like a placeholder. This
turned it into a real one: sorted by name, multi-column by default (column-major,
like a terminal), dotfiles hidden unless `-a`, and a proper `-l` long form. The
columns and sort are pure client formatting (the `tree` playbook — collect
offset/length records, insertion-sort, lay out). The interesting part was `-l`,
because size and date/time are *metadata the filesystem protocol didn't expose*.

That's the keystone the gap-analysis kept pointing at: there was no `stat`. So I
added one — `NP_STAT`, a fixed 20-byte record (size, dir flag, modified time).
The design choice that made it clean was returning a **broken-down calendar**
rather than an epoch: FAT stores calendar fields, ext2 stores unix seconds, and
if the wire carried epochs I'd have to pick one and convert on both sides. A
calendar means each filesystem fills what it natively has and the client just
formats digits — no date math anywhere. I scoped the actual timestamp decode to
FAT32 (the tested disk) and left exFAT/ext2/proc returning size+type with the
time marked absent, shown as `-`. And crucially I made `ls -l` do readdir + a
stat per entry rather than fattening the readdir format — so tree, cd, the HTTP
directory index, none of the existing readdir callers had to change. `ls -l /bin`
lighting up with real sizes and `2026-08-27 08:54` timestamps was a good moment.

## 2026-08-27 (cont.) — `shutdown` and `halt`

The machine could boot but never stop itself — you killed QEMU from the host or
pulled the Parallels plug. So: `shutdown`/`halt`. The interesting part is that
powering off is genuinely privileged: it's a PSCI firmware call (`hvc`/`smc`),
which only EL1 can make, so it needs a real syscall (`POWER`, 56) rather than
anything a userland program could do alone. And PSCI has one gotcha — is the
conduit `hvc` or `smc`? Guessing wrong faults. The honest answer lives in ACPI's
FADT (`ARM_BOOT_ARCH` flags), which the project already knows how to walk (it
reads SPCR and MADT the same way), so `power.rs` reads the conduit at boot next to
the MADT parse and stashes it. QEMU's ACPI advertises `hvc`, and `SYSTEM_OFF` made
the VM exit cleanly on the first try. `halt` is the humble sibling — mask
interrupts, `wfi` forever, and confirmed the machine truly stops by watching a
follow-up `echo` produce nothing. Both are builtins, not `/bin`: you have to be
able to power off with no disk mounted, the same logic that keeps
`erase`/`partition`/`format` builtin.

## 2026-08-27 (cont.) — `tree` joins `/bin`

A satisfying little program to write because the constraints did the designing.
`tree` is inherently recursive, and two things immediately bound the shape: the
framebuffer font is ASCII-only (so the pretty Unicode box-drawing is out — ASCII
`|-- `/`` `-- `` it is), and a spawned program has a ~32 KB stack (so unbounded
recursion is a guard-page fault waiting to happen — depth-capped at 16, with each
frame's buffers kept small since the child borrows the parent's path/prefix
across the recursive call). The one real correctness trap was `.`/`..`: FAT hands
them back in every subdirectory listing, and descending `..` would loop forever —
so they're skipped explicitly. Came up clean on the first real boot: a built
`/t/sub/deep.txt` tree rendered with the right branches and `1 directory, 3
files`, and `tree /efi` drew the boot layout with correct `|   ` continuation
lines.

Then the obvious polish, added right after: **sorting**. Raw on-disk order is
whatever the directory happens to hold; alphabetical is what you want to read. No
heap means no `Vec` to sort, so each directory's entries become fixed
offset+length records into the listing buffer and get insertion-sorted in place
before emitting — the name bytes never move. Capped at 64 entries per directory,
since that array stays live across the recursion into each child (a stack cost).
`zebra`/`apple`/`mango` came out `APPLE BERRY MANGO ZEBRA`, and `tree /` rendered
the whole disk sorted top to bottom without tripping the guard page.

## 2026-08-27 (cont.) — a path-command bug: `bin/echo` from `/`

Hans hit a real one: `bin/echo hello` from `/` said `unknown command: bin/echo`,
but `../bin/echo` from a subfolder worked. The "subfolder fixes it" framing was a
red herring — the actual cause was that the top-level command dispatch fed
*anything* non-builtin through the `$PATH` search, prepending each PATH dir. So
`bin/echo` became `/bin/bin/echo` (gone), while `../bin/echo` became
`/bin/../bin/echo` which normalizes back to `/bin/echo` by pure luck, no matter
what the cwd was. The pipeline-stage resolver already got this right (a
`/`-containing token is a pathname, resolved against the cwd, PATH untouched) —
the top-level path just never learned the same rule. One `command.contains('/')`
branch brought them into line. Nice to see the fix confirmed across all five
shapes (bare, absolute, relative-from-root, relative-from-subdir, and a bogus
path still landing on `unknown command`).

## 2026-08-27 — `ps` shows process names

A small, satisfying observability win. `ps` had always printed a bare slot
number and state (`task 5: runnable`) — you could see *that* something ran, but
not *what*. The interesting part was that the data was already there: the kernel
keeps every spawned task's argv, and `argv[0]` is the command name. It just
couldn't get out — `GET_ARG` only reads the *calling* task's own argv, and the
boot-loaded servers (`fsd`/`cond`/`netd`, idle, the init shell) were never
`SPAWN`ed with an argv at all, so they had no name to read.

So two pieces: a `TASK_NAME` syscall (54) — the read-another-task's-argv[0]
mirror of `GET_ARG`, same bounds-checked copy-to-caller shape, the read-only
partner to the `TASK_STATE` that `ps` already probes — and naming the
boot-loaded tasks by synthesizing a one-argument argv blob for them (`set_name`),
with a `server_name` helper as the single source of truth so a supervised
*restart* re-applies the name after a crash wipes it. `ps` now appends the name;
`exec /bin/PONG` then `ps` confirmed a spawned task shows the path it was
launched with, and the servers show `fsd`/`cond`/`netd`. Zombies stay nameless
(their argv is cleared on exit — a zombie isn't running), which felt like the
right call rather than reaching to preserve it.

Then a natural follow-on the same day: if `ps` is going to list zombies, it
should say *why* they exited. The status was already there (it's what `wait`
returns), it just wasn't readable without reaping. A `TASK_EXIT_CODE` syscall
(55) that peeks a zombie's status without consuming it — the read-only sibling of
`wait` — and `ps` now prints `exited (code N)`. `exec /bin/CAT` with no argument
(exits 1) and `exec /bin/ARGS` (exits 0) both showed the right code, matching the
kernel's own exit log. Small, but it turns a zombie line from "something died
here" into "this exited cleanly / this failed", which is the actual question you
have when you see one.

## 2026-08-26 (cont.) — four more `/bin` filters (`tail`/`nl`/`rev`/`uniq`)

A quieter, additive session after the cluster work: filling out `/bin` with the
classic Unix line filters the pipeline machinery deserves. `head`/`grep`/`wc`
already proved the shape (stdin = `pipe_recv`, stdout = `write_out`, EOF out =
`end_of_stream`, finish = `exit`), so this was mostly writing four more programs
to that template — the interesting part was each filter's own bounded, no-heap
trick.

`tail` is the one with a twist: unlike `head` it can't stop early, because it
doesn't know which lines are the *last* ones until stdin ends. So it keeps the
newest lines in a fixed ring (cap 64, a larger N clamped down) and flushes at EOF.
`nl` numbers every line — I streamed it with a line-piece flush and a "mid-line"
flag so a long line split into pieces still gets its number exactly once, rather
than buffering whole lines. `rev` is an in-place two-pointer swap keeping the
newline last. `uniq` compares each completed line against the previously emitted
one (adjacent-only, real Unix semantics — it'll pair with a future `sort`).

No new mechanism, no kernel change — just four crates, the workspace membership,
and the Makefile's var/build/stage triples. I checked the binaries for stray
`R_AARCH64_ABS64` relocations out of habit (the PIE trap that keeps biting new
code) and they came back with *zero* relocations at all — fully position
independent, everything inlined. Verified the lot in one QEMU boot against a
multi-line `/TEST.TXT` fixture: `uniq` collapsed 6 fruit lines to 4, `rev` turned
`hi mom` into `mom ih`, `nl` numbered the listing, and `cat TEST | uniq | wc`
came back `4 4 25`. Workspace is 36 crates now.

## 2026-08-26 (cont.) — `cpu` output past one message (closing out the cluster)

The one real functional gap left in the cluster: `cpu`'s output was capped at one
IPC message (768 bytes) — `cat` a file or `ls` a big dir over `cpu` truncated. Now
the shell pulls the output in chunks, so a command's full output (up to ~2 KB)
comes back. The design was *forced* by the capability model: netd doesn't hold
`TO_SHELL`, so it can't push a stream — only reply once via the reply exemption.
So it's a pull loop: netd collects the run's output into a `PendingRun` buffer and
the shell pulls chunks with a new `NETOP_RUN_MORE` op (empty reply = end).

Two things worth noting. **The stack held** — I braced for the recurring guard-page
overflow (a ~2 KB buffer on netd's already-tight frame), but writing tcp_run's
output *into* the pending buffer instead of a local resp kept the peak neutral, and
it booted clean. **The test apparatus levelled up**: I confirmed QEMU `-nographic`
delivers piped stdin to the guest shell, then drove `cpu 10.0.2.2:5641 …` against a
host "fake export" that streamed 1500 bytes back — the guest printed all 33 lines,
past the old 768-byte cut. A single-guest end-to-end `cpu` test, no second VM.

Scoped honestly as *bounded* (~2 KB), which covers the small commands `cpu` is
really for; truly unbounded streaming (remote sends as it produces, resumable
caller) is documented in the roadmap as a later arc if the need arises. This was
the pragmatic close-out — the cluster's last obvious gap, filled without opening a
big new mechanism. **The cluster feels done.** Written up as
[`cpu-streaming-postmortem.md`](cpu-streaming-postmortem.md) (the 17th).

## 2026-08-26 (cont.) — reply authentication (auth tier 2, the cheap half)

v0.10.0 authenticated the export *request*; this makes the *reply* authenticated
too, so the exchange is mutually authenticated — an active injector can't feed a
client forged data. The framed reply gains a 32-byte MAC over `request_nonce ‖
[status][result]`; binding to the request nonce (which both sides already hold)
means no new wire field *and* ties each reply to its request. `netd`'s
`seal_reply` wraps every framed export reply; `handle_rmount` verifies before
trusting a byte. No round trip, no state — the existing `hmac.rs` applied to the
other direction.

Built as deliberate **defense-in-depth**, not to solve a current threat — Hans
isn't planning a non-trusted LAN, and even reply-auth only matters against an
active on-wire attacker. The reason to bank it now: the symmetric key + the
request-nonce trick made it nearly free. The rest of the hardening (replay
protection, per-peer identity, transport **encryption**, and reply-auth for the
`cpu`-run output stream) is now on the roadmap behind an explicit **"leaving a
trusted network" trigger**. The honest boundary I made sure to state everywhere:
this is *integrity, not confidentiality* — bytes still cross in cleartext; a
sniffer still reads every file. Verified cross-implementation (the Python
observer verifies the guest's sealed reply; a tampered reply is rejected), zero
faults. Written up as a tier-2 addendum to
[`cluster-auth-postmortem.md`](cluster-auth-postmortem.md), not a fresh
postmortem — it's a clean, small follow-on, not a debugging saga.

## 2026-08-26 (cont.) — dialing *in* through another machine (`/net/tcp` accept)

The mirror of dial-out landed the same day: a machine now **accepts inbound** TCP
connections on another's NIC (`serve /mnt/a/net 9000 …` announces on A's network;
a client that connects to A:9000 is answered by a program on B). Passive open on
A, relay to B, B owns the service. Verified end-to-end: the guest announced 9000,
a host socket connected to it, the guest accepted (passive open), and the
request/response relayed through the export were byte-exact; zero faults.

Mostly reuse — an accepted connection *is* a `DialConn`, so buffers/retransmit/
inbound handling were unchanged; the new bits (a `Listening` + `Accepting` state,
`dial_accept`, a `listen` read) were a new arm on an existing state machine, a day
not a month. Three things worth remembering. **Ordering is correctness**:
`dial_accept` runs *after* `dial_on_segment` so a retransmitted SYN matches the
already-accepted conn and never double-accepts. **The close-flush bug**: `Closing`
originally sent only the FIN, stranding a response written just before `close` —
fixed by flushing send data in both `Established` and `Closing`, FIN only once
drained (TCP's FIN means "no *more* data," not "drop what's queued"). And **the
stack overflow returned a fifth time**: `MAX_DIAL=4` blew netd's 32 KB stack;
capped to 3 + a smaller rbuf. On a fixed stack, per-connection array growth is a
stack question first. Full account in
[`dial-in-postmortem.md`](dial-in-postmortem.md). Honest caveat carried from the
design: dial-in is more speculative on consumers than dial-out — it completes the
`/net/tcp` model symmetrically rather than unblocking a waiting consumer.

## 2026-08-26 (cont.) — dialing out of another machine's NIC (`/net/tcp`)

The last unshared cluster resource fell today: a machine can now open a TCP
connection **out of another machine's network card**, exposed as Plan 9's
`/net/tcp` connection files. `dial /mnt/a/net <ip> <port> …` connects from A's
network — read `/net/tcp/clone` for a connection number, write `connect ip!port`
to its `ctl`, read/write its `data`. Verified end-to-end: from the host I drove
the guest's `/net/tcp` over the export to dial back out to a host TCP server,
which saw the connection arrive from the guest's NIC and got the guest-forwarded
request; the reply streamed back. Zero faults.

Three things worth remembering. **The handle is the path** (`/net/tcp/N/…`), so a
stateful, long-lived connection needed *no fids* — the Phase-0 decision to keep
verbs path-based, which looked like a mere simplification then, is what let this
fit now. **`net_op` never blocks; the event loop does the TCP** — `connect` is
async and the client polls `status`, the same "netd must never block its own
loop" rule from the network and remote-exec arcs, third time applied. And **the
stack trap came back**: 2 KB+4 KB buffers per connection × 4 blew netd's
guard-paged stack on first boot (the network arc bumped that stack three times;
still not learned) — fixed by right-sizing to the actual small-transaction
workload. Scoped honestly: `cpu A fetch` already dials out for the
run-a-program case, so `/net/tcp`'s real value is a *raw* connection B drives
with no program on A. Full account in
[`dial-out-postmortem.md`](dial-out-postmortem.md).

## 2026-08-26 (cont.) — locking the cluster door: export authentication

Phases 1–4 all shipped the same asterisk: *trusted-LAN, no auth*. Any host that
could reach a machine's 9P export (TCP 564) could read/write its disk and run
arbitrary `/bin` programs on it. Today that got a lock — the first real
hardening phase, cut as **v0.10.0**.

**The design fork, settled first.** Three ways to carry a credential over the
wire; all need a keyed hash. Picked the **client-nonce MAC** (`mac = HMAC-SHA256(
key, nonce ‖ np)`) over a server-nonce challenge-response, specifically because
the export uses one connection per request — a challenge-response would tax every
`ls`/`cat` with an extra round trip, while the client-nonce version folds into the
existing single request. Weaker on paper (a sniffer can replay an observed
request), stronger in *this* system. The secret is a **shared cluster key** —
which made the scary case (the `cpu` `/host` reverse callback, where the remote
becomes a client of the caller) authenticate in both directions for free.

**Built in one gate.** SHA-256 + HMAC hand-rolled (`hmac.rs`, checked against
NIST/RFC 4231 vectors); an auth header prepended to each framed request;
`netd` reads the key from `\CLUSTER.KEY` at boot (fail-closed if absent); the key
threads as `&Auth` through the whole event loop (no mutable statics — the `.bss`
ceiling again). Inbound verify in `handle_9p`; outbound sign in `handle_rmount`/
`handle_run`. A `\NOEXEC` flag shares the disk but refuses remote `cpu`.

**Two bugs, both from foreign observers.** The host-only Python↔Python test
passed while *both* scripts had the same transposed magic-byte constant — a
mirror confirms your bug as happily as your correctness. Only pointing the Python
client at the real Rust guest caught it. And `rustc`'s `unreachable_patterns`
caught an `FS_ERR_AUTH` sentinel colliding with `SPAWN_ERR_BAD_ELF`. Verified
end-to-end against the guest export (correct key serves the real disk, wrong key
refused, zero faults). Full account in
[`cluster-auth-postmortem.md`](cluster-auth-postmortem.md).

## 2026-08-26 (cont.) — v0.9.0, and admitting the syscalls aren't POSIX

A release-and-documentation day, no code. Two things.

**Cut v0.9.0.** Phase 4 (the full cpu model) plus the namespace-aware export
refactor and the new user docs, folded to `main` in one fast-forward — the two
stacked branches (`cluster-ns-aware-export` → `cluster-phase4-cpu`) were linear,
so merging the child brought the parent. VERSION → 0.9.0, release notes, artifacts
built, tagged, pushed, GitHub release live, both branches deleted. Phases 0–4 of
the cluster are all released now — the achievable Plan 9 vision (resource-sharing
+ explicit remote execution), with only the shared-memory mirage out of scope.

**Then a good question from Hans: are the syscalls POSIX, or something else?**
The honest answer is *neither*, and pulling on that thread was the more valuable
half of the day. The original `notes.txt` goal said "POSIX-ish system calls," but
what actually got built is a message-passing microkernel ABI: a tiny trap surface
(spawn/exit/kill/wait, the IPC primitives, raw console, and the three block_*
calls gated to fsd) and *everything else* — files, console, network — as messages
to userland servers. Only the register-shape calling convention is borrowed from
Linux; the numbers match nothing, there's no `fork` (spawn instead), and none of
`open`/`read`/`stat`/`socket` exist as syscalls (the fs_* calls that once did are
the gravestone gaps at 7–14).

The realization worth recording: this divergence wasn't a decision so much as a
*consequence*. The microkernel + enforced-isolation work forced the filesystem out
of the kernel (a driver the kernel depends on is a split, not a driver), which made
"a file operation" necessarily a message to a server; Plan 9 then arrived and
*rationalized* that into one uniform protocol + namespaces + the cluster. Hans's
call: keep the design, don't force POSIX back in — but plan for eventual C-program
portability. Which has a clean answer, because POSIX is a libc, not a kernel: port
newlib/picolibc whose ~20 stubs translate to the existing server messages (the
Fuchsia/MINIX3/APE way), implement `posix_spawn` natively for the fork gap, and
build the fd table in userland. The nice twist: a POSIX fd is essentially a Plan 9
fid, and we *deferred* fids in Phase 0 — so adding them someday pays off twice.

Wrote it all down: a "Philosophy — not POSIX, not Linux" subsection in
`architecture.md`, a parked "POSIX portability via a userland libc personality"
entry in `roadmap.md`, and a fresh `comparison.md` — a user-facing "what you gain,
what you give up" pro/con vs MINIX/Linux/Unix/Plan 9/Helix (the older
`research-directions.md` had gone stale as a user-facing view).

**No bug to postmortem today** — the release was mechanical and clean, the docs
were prose. The only "problem" was conceptual (intended POSIX, built something
else). At first I left it documented across those three files without a
standalone retrospective, reasoning there was no debugging story to tell; Hans
then asked for the postmortem anyway, and writing it proved the instinct wrong —
the drift *is* the story, and it's a genuinely different postmortem shape from the
twelve before it (no bug, no single day, triggered by a question, not a crash).
It's now [`posix-divergence-postmortem.md`](posix-divergence-postmortem.md), the
thirteenth: how "POSIX-ish syscalls" stopped being true, why isolation forced it
and Plan 9 rationalized it, and how portability returns as a userland libc — with
the through-line that an architecture can drift from its stated goals with no test
ever going red, and only attention catches it.

## 2026-08-26 (cont.) — Phase 4b: the full cpu model, importing the caller's namespace

The completion of remote execution: a command runs on the remote's CPU but reads
*your* files. `cpu 10.0.2.10:564 cat /host/BONLY.TXT` runs cat on machine A and
prints `hello-from-B-imported` — a file that only exists on B, the caller. On the
same command, `ls /` shows A's disk and `ls /host` shows B's. Data on B, compute
on A. Plan 9's cpu, whole.

I'd stopped here last round because wiring it hit a deadlock, and this time I
built the fix. The import itself is small: the cpu frame carries the caller's
endpoint, and the remote netd binds /host -> remote(caller) on its own namespace
before SPAWN, which the child inherits. The two hard parts were both foreseen by
the last session's design finding. First, the child now talks to netd for two
things — stdout (a send) and its /host reads (a NETOP_RMOUNT call) — so netd
demuxes by op field; a fs call always carries NETOP_RMOUNT, so it's never mistaken
for output, which would deadlock the child. Second, the real blocker: the caller's
netd serviced the run with a blocking tcp_get, so it couldn't serve the child's
/host callbacks arriving at its own export — it dropped their frames. That's a
mutual deadlock (A waits for output; A must serve the reads that produce it).

The fix is tcp_run: a client connection that pumps the event loop while it waits.
Each pass it accumulates the run's output, feeds every non-run frame to on_frame
(so the child's /host reads get served during the run), pumps the server
connections so those replies go out, and acks the health-ping. It's the exact same
"netd must never block" lesson that shaped 4a's capture — I just hadn't seen it
would bite the caller too until I traced the deadlock. Once tcp_run was in, it
worked on the first two-VM boot: A ran the command, saw both its own / and the
imported /host, and read B's file through it. Zero aborts. Phase 4 — the honest
distributed processing — is done.

## 2026-08-26 — Phase 4a: running a program on another machine

The compute half of the cluster: `cpu <host> <command>` runs a program on another
machine and streams its output back. The demo that makes it undeniable: I create a
`/RANHERE` directory on machine A, then on machine B run `cpu 10.0.2.10:564 ls /` —
and B prints `RANHERE/ BIN/ EFI/`. B's own disk has no RANHERE, so the `ls`
*ran on A*. B's CPU never touched A's disk; A's did.

I designed this carefully first (the Phase 4 doc), and the design paid off twice.
First, it settled that netd is the spawner — because a spawned child's output
arrives as ordinary messages to NET_TASK, which netd's event loop already drains
every wake, so the capture is *non-blocking* (netd is supervised and can't block
in a recv the way the shell's capture does). The child runs on its own slot; netd
just relays its messages to the run connection, and its end-of-stream reaps it and
releases the accumulated output to stream out. Second, the design flagged a
capability gap ahead of time: the child's output pipe needs it to send to netd,
but DELEGATE only hands out a cap the caller statically holds, and a /bin slot
doesn't hold TO_NET. Fixed minimally — NET_TASK holds a self-send TO_NET bit *only*
to delegate the reply cap to a child it spawns.

Two bugs, both quick because the design had mapped the terrain. The serve loop
held a `&mut conns[i]` across the per-segment mailbox drain, which now needs
`&mut conns` to route cpu output — so I re-shaped it to iterate by index and end
the borrow before draining. And the first run failed with "stage fail": I'd read
and staged the ELF in 2048-byte chunks, but the kernel's per-syscall pointer cap
(MAX_USER_LEN) is 512, which the shell's spawn_path already respects — dropped to
512-byte chunks and it spawned. A console diagnostic in netd found it in one run.

Scope kept tight: 4a runs with the *remote's* namespace (B's ls lists B's disk),
so it's "run there with the remote's resources" — the ssh-like core. Importing the
caller's namespace (so the remote command reads *my* files, the true Plan 9 cpu
model) is 4b, and the namespace-aware export left exactly the hook it needs. The
serve-loop restructure got a full export-matrix regression pass, zero aborts.
Ouroboros can now run a computation on another machine — a compute cluster, not
just a storage one.

## 2026-08-26 — the namespace-aware export: paying down the three prefix hacks

Not a feature day — a cleanup I'd been deferring on purpose. Each Phase 3 file
server (/proc, /dev/cons, /net) rode the export by an explicit path-prefix
special-case, and I kept writing down "the general namespace-aware export is
getting closer to worth building." Three consumers is the threshold I'd named, so
today I built it.

The move: the export gateway should serve *its own namespace* through the same
resolver a local client uses — Plan 9's "a server exports a namespace." So the
resolution logic (which had been duplicated in ulib and the shell) became one
`resolve_ns` in ninep-abi, returning a task-neutral `NsTarget` the callers map to
their own server ids. ulib and the shell deleted their copies and delegate; netd's
export resolves incoming paths against a tiny `EXPORT_NS` binding blob and
dispatches on the `NsTarget`. `route_export` and its three prefix checks: gone. A
fourth exported resource is now a fourth binding, not a branch.

The satisfying part is that there's nothing to demo — every path resolves exactly
as before. The whole value is that three implementations that had to agree became
one. That's the kind of change that's invisible until the day someone adds a
resource and it Just Works without touching netd.

Two process notes worth keeping. First: I verified `resolve_ns` in isolation (a
throwaway `rustc` of the pure function against the export blob) the moment a remote
`/net` read flaked in the big matrix test — the isolated test said the resolver was
correct, which saved me from "fixing" the resolver, and a focused re-run confirmed
remote /net works fine; the flake was connection churn (the 8th rapid back-to-back
op), a pre-existing chattiness of one-connection-per-op, not a refactor bug.
Second: deferring this until the third consumer was right. Built at the first
consumer it would have been machinery guessing at a shape; built now, the shape was
obvious because three real callers had already drawn it.

## 2026-08-25 (cont.) — Phase 3, step 3: /net, and Phase 3 complete

The last Phase 3 file server: `/net`, the machine's network identity as read-only
files (`/net/ip`, `/net/mac`). Locally `cat /net/ip`; remotely `cat /mnt/a/net/ip`
reads *another* machine's address. That completes the trio — `/proc`,
`/dev/cons`, `/net` — three resources, three file servers, each remotely readable.

`/net` is the first served by netd itself (netd owns the NIC, so it knows the
IP/MAC). The remote half was easy — the export already routes prefixes, and netd
is the export, so it just serves `/net` from its own state. The interesting part
was the *local* half: a local `cat /net/ip` has to reach netd, but netd's client
handler only spoke NETOP_*. So it now also answers NP read verbs addressed to
NET_TASK. And the routing had a wrinkle: a `/net` binding and a *remote* mount
both resolve to `server = NET_TASK`, so I needed to tell them apart — the
discriminator is the endpoint (a remote mount always carries a real ip:port; local
`/net` carries zeros). `is_local_net` checks it, and a local read goes to a direct
`np_netlocal` NP call instead of the NETOP_RMOUNT remote wrap. Writes to `/net`
are refused.

Worked on the first boot of both: local ip/mac read true (10.0.2.15 + the default
MAC), write refused; two VMs, B read A's 10.0.2.10 and its ...:0a MAC. Zero aborts.

That's three prefix special-cases in the export now (/proc, /dev/cons, /net) —
which is finally the honest signal that the namespace-aware export I've been
deferring has earned its place. Each of these was a few lines because it rode the
export's existing routing; a fourth resource is when I'd stop special-casing and
make the export resolve through a real composed namespace. But Phase 3's stated
scope — resources as files, remotely mountable — is done. The /net slice is
network *identity*; using another machine's NIC to actually dial out (Plan 9's
/net/tcp connection files) is a bigger surface for later. Cut v0.8.0.

## 2026-08-25 (cont.) — Phase 3, step 2: /dev/cons, writing another machine's screen

Second Phase 3 file server: `/dev/cons`, the console as a writable file. The demo
that makes it click: `write /mnt/a/dev/cons hi` on one machine, and "hi" appears
on *another* machine's screen. Remote console.

Where `/proc` was easy because it lived inside fsd (reusing the disk path whole),
the console is the interesting case — it's a *different server* (`cond`,
CON_TASK), so this is the first time the namespace and the export route somewhere
other than fsd. I did it with a console sentinel (NS_CON_TREE), the same shape as
the remote sentinel: locally `mount -c /dev/cons` binds the path, `resolve_ns`
returns server = CON_TASK, and the write helpers just `con_write` the bytes (an
NP_WRITE_FILE to the console) — which every task already does to print, so there
was no new plumbing, just a new *destination*. Reads are refused (write-only).
Remotely, the export recognizes `/dev/cons` and emits the inline bytes to
CON_TASK — and netd already logs to the console, so it could reach it.

Worked on the first boot of both halves: locally `write`/`echo >` both render and
`cat /dev/cons` fails as it should; between two VMs, B's `write /mnt/a/dev/cons
>>>HELLO-ON-A-FROM-B<<<` printed exactly that on A's console. Zero aborts. It's
another explicit prefix/sentinel special-case rather than a general
namespace-aware export — but /dev/cons is now the *second* such consumer, which is
the signal that the generalization (an export that resolves incoming paths through
a composed namespace) is getting closer to earning its keep. Still deferred; two
special-cases is cheaper than the general mechanism, and honest about it. /net is
the remaining big Phase 3 piece.

## 2026-08-25 (cont.) — Phase 3 begins: /proc, and reading another machine's processes

With the shared disk working, the next Plan 9 idea: *everything* is a file, not
just the disk. Phase 3's first step is `/proc` — the kernel's task table as a file
tree — and, because it rides the same export, **a remote machine's `/proc` too**.
The payoff line: `ls /mnt/a/proc` on one machine lists *another* machine's live
tasks, and `cat /mnt/a/proc/2/state` reads its filesystem server's state.

The satisfying part was how little it took. `fsd`'s `Filesystem` enum was already
a per-op dispatch over a `tree`-indexed mount table; `/proc` is just a fourth arm
— the *first non-disk* one, which is what turns the enum from a format
multiplexer into a real VFS. It holds nothing; every listing and file is generated
from the `TASK_STATE` syscall on demand (`/` → a dir per slot, `/<n>/state` →
runnable/blocked/zombie/unused). Auto-mounted at a reserved tree at boot, so it
always exists. Local access is a one-line namespace bind (`mount -p /proc`), the
same mechanism `mount <n>` and `mount -r` already use.

The remote half was the interesting design call. The export gateway sent every
path to fsd tree 0 (the disk); to serve `/proc` it has to pick a different tree. I
took the scoped route — the export *prefix-routes*: a `/proc/…` wire path goes to
the proc tree (prefix stripped), everything else to the disk. That's a deliberate
two-things-exposed hack, not the fully namespace-aware export Plan 9 ultimately
wants (resolve the incoming path through a composed per-export namespace) — but
that generalization has no second consumer yet, so it waits. Threading a `tree`
through the export's four fsd-client calls (HTTP passes 0) was the only real
plumbing.

It worked first try on both halves. Locally the states read true — fsd runnable,
netd blocked in NET_WAIT, slot 9 unused, the shell itself blocked waiting on the
`cat` it spawned. Remotely, B lists and reads A's slots over TCP, disk access
still working alongside, zero aborts on both nodes. One honest limit: only per-slot
*state* is exposed — `GET_ARG*` returns the caller's own argv, so there's no
cross-task name/cmd to show without a new kernel accessor. Enough for a real,
demonstrable "a remote resource is a remote file." More of Phase 3 (`/dev/cons`,
`/net`) later.

## 2026-08-25 (cont.) — Phase 2: a machine writes another's disk (shared-disk cluster)

Phase 1 ended with B *reading* A's disk. Phase 2 makes it a genuine shared disk:
B *writes* A's disk. And it turned out small — the transport, framing, remote
binding, and client routing all existed; Phase 2 was really just teaching the
export gateway the write verbs and chunking a large write client-side.

The export side relays each mutate verb to the local fsd, reusing netd's
fsd-client calls: path-only ops and `mv` go straight through, a full `NP_WRITE`
maps to fsd's inline `NP_WRITE_FILE` (no grant), and `NP_WRITE_AT` bridges the
wire's inline bytes to fsd's grant-based offset write — the exact mirror of the
Phase-1 read bridge, the other way. The client loops a large write into ≤512-byte
`NP_WRITE_AT` chunks, so `cp`/`writeat`/`>>` are all unchanged above the fs
helper. Semantics: single-writer, clean-disconnect — documented, not coordinated
(fsd already serializes disk access through one task, so a single writer never
tears; the only thing we don't claim is *concurrent* writers).

Two VMs, and it worked: from B, `mkdir /mnt/a/CL`, `write /mnt/a/CL/NOTE.TXT …`,
`cat` it back through A, then `cp /BIN/LS /mnt/a/LSCOPY` — a 17 KB file over 34
chunked round trips. Then A, reading its *own* disk, sees `CL/`, `LSCOPY`, and the
note's exact text: B genuinely wrote A's disk. The clincher was the foreign
observer — mounting A's disk image on macOS, `LSCOPY` is `cmp`-identical to
`/BIN/LS`, byte for byte. That's the years-long question answered with a yes you
can act on: two machines, one reading *and writing* the other's disk over a
protocol written from scratch.

The two-VM link also surfaced a real robustness gap the single-VM tests never
could: the client's `tcp_get` sent a *single* SYN with no retransmit, so a dropped
first packet on a freshly-connected QEMU socket hub failed the whole op (an
intermittent first-`ls`). Added a SYN retransmit (a few tries within the op) — it
helps HTTP fetch and remote reads too. And the disconnect test did its job:
`SIGKILL` A mid-session, and B's next remote op fails cleanly (a distinct error,
no hang) and B stays responsive locally — clean-disconnect, as designed. Zero
aborts on both nodes throughout. Phase 2 done → v0.7.0.

## 2026-08-25 (cont.) — Phase 1d: two Ouroboros machines, one reads the other's disk

The "aha," and it landed the same day. 1c proved the remote-mount client against a
host python server; 1d makes the peer a *second Ouroboros VM* on a shared L2 link
(QEMU `-netdev socket,listen=`/`connect=` — a virtual hub, no SLIRP, no gateway).
Machine A exports (its port-564 listener is always on); machine B runs `mount -r
10.0.2.10:564 /mnt/a` and reads A's actual disk — `ls /mnt/a` → `BIN/ EFI/`, `cat
/mnt/a/EFI/ORBS/INIT.CFG` → A's file, `ls /mnt/a/BIN` → A's whole `/bin`.

The one real design piece was the per-guest IP. netd's `OUR_IP` was the hardcoded
SLIRP lease `10.0.2.15`; two guests need distinct addresses. I made it `our_ip()`,
deriving the last octet from the NIC's MAC — the cleanest config channel that
already exists (no boot arg, no new syscall). The trick that kept it zero-risk:
map the QEMU-default MAC `…:56` back to `.15`, so every existing SLIRP path
(ping/resolve/fetch, and the export gateway's hostfwd, which all target `.15`) is
untouched, while the two-VM target hands out `…:0a`/`…:0b` → `.10`/`.11`. And no
mutable global was needed — userland has no `.bss`, so `our_ip()` just reads
`NET_MAC` each call. `next_hop` was already subnet-based, so two on-link guests
route directly; the existing ARP responder answers for the derived IP. Only eight
`OUR_IP` sites to touch.

It worked on the first boot of the pair. The shared-link pcap tells the whole
story: B ARPs for 10.0.2.10, A answers with `…:0a`, B opens TCP to `:564`, sends
the 53-byte framed readdir, A replies 22 bytes and FINs — rotating source ports
per connection (the TIME_WAIT fix from 1c earning its keep), SACK-permitted on A's
SYN-ACK, no SLIRP anywhere. Zero Data/Prefetch aborts on *both* VMs under `-d int`.

That's **Phase 1 complete** — the years-long question ("can several computers
share resources as one system?") has its first real yes: two Ouroboros machines,
one mounting and reading the other's disk over a protocol written from scratch.
Read-only for now; read+write is Phase 2. Ready to cut v0.6.0.

## 2026-08-25 (cont.) — Phase 1c: a machine reads another's disk over TCP

Picked up where the cluster day left off: the export gateway (1a) let the host
read the *guest's* disk; today's work is the mirror — the **remote-mount
client**, so a guest reads someone else's disk. The shape is the whole thesis
made real: `mount -r 10.0.2.2:5641 /mnt/a`, then `ls /mnt/a` and `cat
/mnt/a/HELLO.TXT` — and `ls`/`cat` are the *unchanged* `/bin` programs. Only the
namespace resolver decides local-vs-remote; everything above it is untouched.

The pieces: a `NETOP_RMOUNT(endpoint, NP-request)` op in netd that frames the
verb onto a TCP round trip (reusing `tcp_get`) and returns the reply; a remote
namespace binding (`tree` sentinel `0xFF`, target = `[ip][port][root]`); a
resolver that now returns a `Resolved { server, tree, endpoint }` instead of a
bare `(tree, path)`; the fs helpers routing a remote resolution through netd
instead of fsd — in both `ulib` and the shell's duplicate layer; and the `mount
-r` builtin. Bulk reads, which use grant/safecopy locally, fall back to inline
512-byte chunks over the wire, so `cat` streams a remote file the same way it
streams a local one.

It compiled clean and then the trace did its job — twice. First, `mount -r`
succeeded but every remote op returned "no filesystem": the pcap showed the
guest sending SYN, the host replying SYN-ACK, and the guest never ACKing.
`parse_tcp` — the client-side parser, shared with HTTP fetch — hardwired the
peer's source port to **80**, so it dropped the SYN-ACK coming from 5641. One
line. Then a subtler one: readdir worked but the *next* read failed, then a
readdir worked again — intermittent. The pcap: some SYNs got no reply at all.
The remote-mount client opens a fresh connection per verb, back to back, and the
fixed ephemeral source port `0xc000` meant each new SYN reused a 4-tuple the
peer still held in TIME_WAIT — silently dropped until it expired. Fixed with a
rotating `next_src_port` (and a derived ISN). A `.bss` snag along the way: the
obvious counter is a `static`, but a zero-init static needs `.bss` the userland
loader doesn't support, so the port comes from the microsecond clock instead —
successive connections are a round trip apart, so it's always advanced.

And one more for the relocation-trap collection: `mount -r`'s `host:port` split
first used `&hostport[..c]`, and str range-indexing pulls in a UTF-8
char-boundary panic path whose formatting tables break the PIE link
(`R_AARCH64_ABS64`). Byte slices + `from_utf8` fixed it — the same class the
shell-and-filesystem postmortem is about, still lurking.

Verified against a foreign observer, a ~120-line host python 9P *server*
(mirroring the 1a client): the guest lists and cats its tree, including a
multi-chunk `cat` over four round trips. Local ls/cat unchanged, the export
gateway still serves, zero Data/Prefetch aborts. A small honest note on staging:
the export shipped first (labeled 1a), so this is the design's 1a+1c together —
the outbound half is no longer ahead of us. Next is 1d, the two-VM integration
(a shared QEMU socket link + per-guest IPs), which turns "host as server" into
"machine A ↔ machine B" and cuts **v0.6.0**.

## 2026-08-25 — the cluster day: Phase 0 done (v0.5.0), Phase 1 begun

The biggest single day of the project. It started with a small fix and ended
with the filesystem servers speaking a single protocol over the network.

**First, a patch: the large-read `fsd` restart (v0.4.1).** The real-hardware pass
had left one open bug — a multi-MB `cat` got `fsd` supervisor-restarted mid-read.
The roadmap had guessed the fix was "ack the health-ping during long reads," but
reading the code told a different story: `fat32::read_at` re-walked the cluster
chain *from the file's start on every call*, with no FAT cache, so a chunked
read was O(n²) and a single late-offset request issued enough uncached FAT reads
to blow past the *runnable*-wedge threshold (2.56 s) — which no ping touches. The
fix was a sequential-read cursor that resumes the walk instead of restarting it:
each request O(chunk), the read O(n). Proven on QEMU with a decisive A/B (a 1 MiB
read: 0.99 s with the cursor vs. did-not-finish-in-120 s without). Shipped as
v0.4.1 — the first *patch* release, exercising that half of the version scheme.

**Then the whole of cluster Phase 0 — the arc.** The goal from
[`roadmap-cluster.md`](roadmap-cluster.md): stop having three bespoke server
protocols (`FSOP_*`, `DSPOP_*`, `NETOP_*`) and give every server *one* uniform,
Plan 9-style verb set, with each task composing its own namespace. Built in
sub-steps, each verified byte-identical and shippable:

- **0a+0b — the `ninep-abi` verb set, in use end to end.** Merged into one step
  because "fsd speaks the verbs" is untestable without a client — the minimal
  client *is* the `ulib` re-point, so they went together. Every `/bin` filesystem
  command reached `fsd` over the new protocol, byte-identical, with no `/bin`
  source change (ulib absorbed it). The load-bearing addition was the `tree`
  selector in the wire header — the future multi-mount key.
- **The FSOP retirement, client by client.** The shell keeps its *own* fs helpers
  (separate from ulib — a surprise found in the build), and `netd` is an fsd
  client too, so retiring `FSOP_*` was a client census, not a server edit: migrate
  the shell, then `netd`, and only then delete `fsd`'s twelve FSOP file-op arms
  (~210 lines). `FSOP_*` is now admin-only.
- **0c — per-task namespaces + `bind` (the first kernel change).** Modeled on the
  existing per-task CWD store. The plan was CWD-style spawn-time *staging*; while
  building it, the shell's own `cd` (which validates via `fs_list_dir`) showed it
  must resolve too — which made **direct `NS_SET` + automatic parent→child
  inheritance** the simpler design. An empty namespace is the identity, so
  unbound behavior stayed byte-identical. `bind /mnt /EFI` → `ls /mnt` == `ls
  /EFI`, per-task, inherited by spawned commands.
- **0d — multi-mount, the payoff.** `fsd`'s single mount became a table indexed
  by `tree`; `mount <partition> <path>` mounts a second filesystem and binds it.
  A nice simplification: the existing two-partition `run-image-ext2` disk was
  already the test rig — `ls /` (ext2) and `ls /mnt/f` (FAT32) show two different
  on-disk filesystems at once, which the single-mount model physically couldn't
  do.
- **0e — `cond` on the verbs.** Console writes became `NP_WRITE_FILE` to the
  console "file"; `DSPOP_*` deleted. The last bespoke protocol gone.

Cut **v0.5.0** — the first *per-arc minor* release (Phase 0).

**Then Phase 1 began — 9P-over-TCP.** Wrote the design
([`roadmap-cluster-phase1.md`](roadmap-cluster-phase1.md)) and built **step 1a:
the export gateway.** The reframing: locally, bulk data moves by grant/safecopy;
over TCP there is no grant, so a verb travels as a length-delimited frame with
data inline. `netd` grew a second inbound listener on port 564 (alongside HTTP's
80) — the connection remembers its local port so replies leave the right source
port, and the first-data handler dispatches by port to a new `handle_9p` that
decodes the frame, runs the verb against local `fsd`, and frames the reply. A
host-side python 9P client read the guest's disk over TCP (`readdir /` →
`BIN/ EFI/`; `read /EFI/ORBS/INIT.CFG` → its contents), HTTP unregressed, zero
aborts. A property worth naming fell out: because Phase 0 deferred fids, the
verbs are *path-based* — one round trip per op regardless of path depth — so real
9P's per-component-walk chattiness simply doesn't arise. The client half (1b/1c)
and the two-VM "aha" (1d) are next.

Design retrospective:
[the cluster Phase 0 postmortem](cluster-phase0-postmortem.md).

## 2026-08-24 — real-hardware xHCI, the north star, and the first releases

Three threads: closed the last real-hardware bug, wrote down where the project is
ultimately going, and started actually cutting releases.

**The xHCI keyboard↔USB-storage contention, fixed on real Parallels hardware.**
The bug wore one symptom over two boot configs, and it was *two* bugs. **Mode A**
(keyboard works, storage degrades to I/O errors) was a missing BOT error
recovery in `usb_msd.rs` — a single contention-induced bulk-endpoint stall
stayed halted forever; fixed with `xhci::reset_storage_endpoint` (controller
commands Parallels forwards, not the class request it doesn't) + a bounded
retry. **Mode B** (storage works, keyboard never addressed) was a port-scan race
— the scan broke on the first connected port, so the fast SuperSpeed stick won
and the slower synthetic keyboard was missed; fixed with a minimum-settle +
debounce scan. QEMU hid both (its keyboard is synthetic, its storage virtio-blk
— they never share the xHCI bus). Both confirmed on hardware.

**The north star, written down.** [`roadmap-cluster.md`](roadmap-cluster.md): a
Plan 9-style distributed resource-sharing cluster — machines exporting resources
as file trees, each composing a private namespace of the whole. Stated honestly:
sharing resources is doable (Phases 0–4); transparent shared memory / single
system image is the mirage, out of scope by design. The key insight the whole
plan rests on: *"remote" is just "the same protocol over TCP instead of local
IPC"* — which is why the local Plan 9 work (Phase 0) is step 1 of the cluster,
not a detour.

**Releases began.** A `VERSION` file, `scripts/release.sh` (a deliberate
two-phase `build`/`publish` split), and `docs/RELEASING.md` with the version
scheme: `0.MINOR.PATCH`, a completed *arc* → minor bump, an isolated *fix* →
patch. First cut was **v0.4.0** — bundling everything built to date (four arcs).
A real quirk surfaced: `prl_disk_tool`'s `.hdd` bundle references its `.dmg` by
absolute path, so a zipped `.hdd` is useless off the build machine; the portable
Parallels artifact is the self-contained `.dmg`, wrapped into a `.hdd` locally.

## 2026-08-23 — the filesystems day: exFAT read+write, a cleanup, then ext2

Yesterday's filesystem arc left a `Filesystem` enum inside `fsd` with exactly
one arm (FAT32) — a refactor that *claimed* to make a second format cheap.
Today made good on it: `fsd/src/exfat.rs`, a read-only exFAT driver, the first
real second arm. Nothing above `fsd` changed — clients speak `FSOP_*` and never
learn the on-disk format — which is the whole point the refactor was for.

**Read-only first, deliberately.** The roadmap's own scoping lever: FAT32's
write support was phases 4–8, where every corruption bug lived. So a new format
lands read-only as one milestone; read-write is a separate one. Every write op
in `exfat.rs` returns a new shared `Error::ReadOnly` → a new `FS_ERR_READ_ONLY`
ABI code → `ulib`'s "read-only filesystem" message.

**What was actually new versus FAT32.** exFAT is still a cluster filesystem, so
the read machinery (cluster→LBA, chain walking, windowed `read_at`) is the same
shape as `fat32.rs`. The genuinely different parts: a boot sector described by
`log2` shifts; **contiguous files that skip the FAT entirely** (a `NoFatChain`
flag — `advance()` either walks the FAT or just returns cluster+1); directory
**entry sets** (a File entry + a Stream-Extension entry + File-Name entries)
reassembled into one `DirEntry`, structurally the same job FAT32's LFN
reconstruction does; and UTF-16 names. Two structures a read-only driver gets to
*ignore*: the allocation bitmap (that's how you find free clusters — a write
concern) and the up-case table (ASCII case-fold suffices for the names this
system uses, same shortcut `fat32.rs` already takes).

**The testing wrinkle was the interesting part.** To prove the reader, `fsd`
has to *mount* exFAT — but UEFI can only boot from FAT, and `fsd` mounts the
first partition it can. So the test disk (`scripts/mkexfat.py`) is a
two-partition MBR: **exFAT first** (so `fsd` mounts it — the FAT32 probe fails,
the exFAT probe succeeds, exercising the real enum fallthrough) and the **FAT32
ESP second** (UEFI ignores the exFAT partition it can't read and boots from the
FAT32 one). The exFAT filesystem itself is built with macOS's `newfs_exfat`
(`hdiutil` can't make exFAT) and carries `/bin` plus test files — so the shell
runs its commands *off* exFAT. One block device, no slot ambiguity. It worked
first boot: `ls`/`cat`/a `cat | grep | wc` pipeline all reading from exFAT,
long names and subdirs resolved, writes refused read-only, zero aborts. FAT32
via `run-image` unregressed.

The recurring lesson, again: a parser is only tested if you can *build* it a
valid input — the harness that produces the artifact is part of the feature,
same as `mkgpt.py` was for the GPT parser yesterday.

**Then, read-write — the harder half, in four staged commits.** With the reader
proven, the write surface followed the same discipline the FAT32 write arc used:
narrowest-useful-first, one tested-and-committed stage at a time. Stage A built
the machinery (allocation bitmap + entry-set construction with both checksums)
and exercised it with `touch`, which allocates nothing — the lowest-risk first
cut. Stage B added `write_file`/`write_at` (cluster allocation + data). Stage C
added `mkdir`/`rm`/`rmdir`. Stage D added `mv`.

What made exFAT writes genuinely harder than FAT32's: free space is an
allocation *bitmap*, not a scan-the-FAT-for-zeros; and creating a file means
building a whole *entry set* (a File entry + a Stream-Extension entry + N
File-Name entries) with two checksums the format requires — a `SetChecksum` over
the entire set and a `NameHash` over the up-cased name. Get either wrong and a
real driver rejects the file. What made it *easier* than expected: exFAT has no
8.3/LFN mangling (names are just UTF-16, so no `make_short_name`), and no
`.`/`..` directory entries (an empty dir is a zeroed cluster, and moving a
directory needs no `..` fixup — the one hard step FAT32's `mv` couldn't skip).

The decision that kept it simple: created files are always FAT-*chained*
(`NoFatChain = 0`), never pure-contiguous, so allocation is the direct parallel
of FAT32's `write_chain` (set the bitmap bit, link the FAT) and the reader's
existing `advance()` walks them. A macOS-created *contiguous* file being appended
to is the one case needing a convert-to-chain step first.

The validation this time was better than any log line: macOS's own exFAT driver
mounts the volume `fsd` wrote, a binary copied through `cp` reads back
byte-identical, and `fsck_exfat` — a real filesystem checker — pronounces the
bitmap and directory hierarchy clean after a full churn of creates, writes,
deletes, and renames. When another vendor's checker signs off on your on-disk
structures, the checksums are right.

**Then two housekeeping passes, before the tree got unwieldy.** First, the ~26
userland crates (which had piled up at the repo root) moved under a `programs/`
tree grouped by role (`fileutils`, `textutils`, `netutils`, `shellutils`,
`servers`, `demos`, plus `shell`) — a pure `git mv` (history preserved), and
because crates build by package name, the Makefile needed *zero* edits; only the
`path = "../..."` deps deepened. Second, every generated artifact (the `esp/`
staging tree, the disk images, `net.pcap`, logs) moved under a single `build/`
dir driven by one Makefile variable, so the repo root is source/docs/config
only. We considered moving `kernel/` and the shared libs too, but left them —
three top-level dirs isn't crowding, and `kernel/` at the root is the
conventional, legible shape.

**Then ext2, read-only — the arm that actually tests the abstraction.** FAT32
and exFAT are the same shape (clusters, a FAT, a flat directory-entry list), so
the `Filesystem` enum barely had to stretch. ext2 is a different world: inodes
own the metadata and a directory entry is just `name -> inode number`, so path
resolution bounces between directory blocks and the inode table; files are found
through block-group descriptors; data blocks are reached through 12 direct
pointers then single/double indirect pointer-blocks; and names are
*case-sensitive*. Driving all of that through the **unchanged** `FSOP_*`
protocol — nothing above `fsd` touched — is the proof the abstraction was real
and not FAT-shaped in disguise.

Two nice moments in bring-up. First, the reader worked on the first boot for
`exec` and `cd`, but bare commands (`ls`) failed — which turned out *not* to be
a reader bug: the shell probes `/bin/<command>` as typed (lowercase), and it had
always relied on FAT/exFAT matching case-insensitively. ext2, correctly
case-sensitive, wouldn't match lowercase `ls` against the uppercase `LS` I'd
copied in. The fix wasn't in the reader at all — it was to give the ext2 image a
*lowercase* `/bin`, which is the Unix convention anyway. The abstraction was
faithfully surfacing ext2's semantics; my test image was the thing being
un-Unix. Second, forcing the image's block size to 1024 meant the >12 KiB `/bin`
binaries spilled past the 12 direct pointers into single-indirect blocks — so
`exec /bin/CAT` was quietly exercising the indirection path from the first run.

**And finally ext2 read-write — the finale of the whole filesystems arc.** Four
staged commits again (allocation + `touch`; `write_file`/`write_at`;
`mkdir`/`rm`/`rmdir`; `mv`). ext2 writing is the fiddliest of the three formats,
because its consistency is spread across more structures than FAT's single
table: allocation flips a block/inode *bitmap* bit **and** decrements free
counts in both the group descriptor and the superblock; directories carry link
counts that `mkdir`/`rmdir` must bump and drop; a cross-directory directory move
has to repoint the moved dir's `..` and shift its parent-link contribution. The
independent checker this time was `e2fsck` (macOS can't mount ext2), and it kept
me honest.

The best bug of the arc surfaced here. After `rm`/`rmdir`, `e2fsck` reported a
"corrupted orphan linked list" naming exactly the inodes I'd deleted. Everything
*looked* right — links 0, bitmap freed, deletion time set — and `s_last_orphan`
was 0, so there shouldn't have been an orphan list at all. The cause was subtle:
e2fsck treats a links-0 inode whose `i_dtime` is *less than the inode count* as
sitting on the orphan list, using `i_dtime` as the next-orphan inode pointer. I'd
written a sentinel deletion time of `1`, which e2fsck read as "next orphan =
inode 1", fabricating a bogus chain. The fix was to write a plausible *timestamp*
(a fixed large constant, since there's no RTC) instead of a small sentinel —
after which `e2fsck -fn` passed all five passes completely clean, including the
directory-connectivity and reference-count passes that validate `..` and link
counts after directory moves. A copied binary also came back byte-identical
through `debugfs`.

That closes the "more filesystems" arc: GPT/MBR discovery, a VFS refactor, and
FAT32 + exFAT + ext2 — `fsd` reads and writes all three through one unchanged
`FSOP_*` protocol. The whole point of the VFS refactor, proven: a genuinely
different (inode-based, case-sensitive) filesystem slotted in as a third enum arm
with zero changes above `fsd`.

**And then the real-hardware pass — which did exactly what it's for: found a bug
QEMU can't show.** After a session-long pile of QEMU-only work (exFAT, ext2,
`/bin`, pipelines, the housekeeping), we took it to a real Parallels VM via
`prlctl` (start / send-key-event / capture, reading the screenshots back).
**Part 1 was clean:** boots to a shell, all servers supervised, `ps` correct,
`selftest` (the relocation self-test) passes — none of the churn regressed the
hardware boot path.

**Part 2 got interesting.** Parallels exposes no disk to the kernel except USB
mass storage, so testing the filesystem work meant a real USB stick (an
`espexfat.img` written to it). First attempt — boot from the `.hdd`, stick
passed through — showed `fsd: exFAT mounted` but then every read failed with
"device I/O error", even reads of the *same sectors* the mount had just read
successfully. Then Hans ran his own experiment: detach the `.hdd`/CD entirely so
the VM boots *from the stick's* FAT32 ESP — and exFAT auto-mounted and read/wrote
fine. But when I tried to drive that config, the keyboard was completely dead —
not even a builtin registered.

The two configs form an inverse correlation: `.hdd`-boot → keyboard works,
storage reads degrade; USB-boot → storage works, keyboard dead. That's the
diagnosis: the xHCI/USB stack can't reliably keep a USB keyboard *and* a USB
mass-storage device live at the same time on this hardware (the "up to 4
concurrently addressed devices" limit + enumeration order in `xhci.rs`, and/or
mass-storage endpoint recovery in `usb_msd.rs`). It's a USB-subsystem robustness
bug, not a filesystem bug — the FS drivers read and wrote correctly whenever the
block layer actually served them. Recorded in the roadmap; it's the next real
debugging target, and postmortem-worthy. The satisfying part: the whole reason
to run on real hardware is to find precisely this kind of thing, and it did.

## 2026-08-22 — the userland day: /bin, pipelines, and the filesystem arc begins

A long single day that took the shell from "every command is a builtin compiled
into the shell binary" to a real `/bin` of standalone programs, gave it genuine
multi-stage pipelines with a filter set, raised the task-slot ceiling to make
that concurrency possible, and then started the "more filesystems" arc with a
VFS refactor and GPT support. Roughly in order:

**Finished the network stack's maturity work (Stages 4k–4o).** IRQ-driven NIC
receive (the RX queue's GIC SPI wakes `netd` instead of polling), an
RTT-estimated RTO (RFC 6298 SRTT/RTTVAR over a new microsecond clock), an HTTP
405 for unsupported methods, TCP congestion control (Reno cwnd/ssthresh), and
sender-side SACK. These closed out the network arc — the stack now has flow
control, loss recovery, congestion control, and selective retransmit.

**Trimmed `CLAUDE.md`** from a 6500-line milestone narrative to ~600 lines of
durable "read this before touching the code" guidance, moving the history into
`CHANGELOG.md` and the postmortems, and stripping the now-dangling back-pointers.

**The standalone-binaries arc — the day's spine.** The goal: commands become
real programs in `/bin`, found via `$PATH`, with arguments and a shell
environment. Built in stages: an argv ABI (`ARGS_STAGE`/`GET_ARGC`/`GET_ARG`); a
`/bin` + PATH lookup for unknown commands; a shell environment (`set`/`env`/
`unset`, `$VAR`, `PATH` a real variable); then a shared `ulib` support crate and
the externalization itself — `echo`/`uptime`/`clear`, then a cwd-delivery ABI
(`CWD_STAGE`/`GET_CWD`) so `ls`/`cat` resolve relative paths, then the path-only
write commands (`mkdir`/`rmdir`/`touch`/`rm`), then the bulk/multi-arg ones
(`cp`/`mv`/`writeat`). The **whole filesystem command surface** left the shell.

Then the network commands (`ping`/`resolve`/`fetch`) — the first `/bin` programs
to reach a server a spawnable slot can't statically talk to. Rather than widen
the capability policy, the shell **delegates** its `TO_NET` capability to the
child at spawn (the same `DELEGATE` mechanism the program-to-program pipe uses).

One batch was **attempted and reverted**: `ps`/`kill`/`wait`. Testing showed an
externalized job-control command runs in a spawnable slot, so it lists itself
and a task number goes stale between commands — which is exactly why bash makes
them builtins. That's a design finding, recorded, not a failure.

**Stage 0: raised `NUM_TASKS` 7 → 10** (five spawnable slots, up from two) — the
headroom real pipelines need. Mechanical, except the capability `u32` packed
resource caps at bits 8/9/10 and the send-mask now reached bit 9: a collision
caught before it shipped, fixed by moving the caps to bit 16+.

**The multi-stage-pipeline arc.** Turned the two-stage pipe into a real
N-stage `a | b | c`: a chainable filter shape (`upper` rewritten to write to its
stdout target, not a hardcoded console), N-stage parsing, argv and PATH on every
stage, and the spawn/delegate/wait plumbing. A satisfying confirmation: a linear
chain needs only the *existing* one-target-per-task delegation — the roadmap had
parked general delegation for want of a consumer, and this was that consumer,
without needing the general version. Then the payoff: real `/bin` filters
`grep`/`wc`/`head`, so `cat FILE | grep x | wc` works.

**The filesystems arc began.** A pure VFS refactor extracted `fsd`'s hardcoded
FAT32 into a `Filesystem` enum (FAT32 the only arm, proven byte-identical), then
GPT + multi-partition discovery: a new `partition.rs` enumerates partitions on
GPT *or* MBR disks, and `fsd` mounts a FAT32 partition wherever it sits. Testing
the GPT path meant hand-building a bootable GPT disk (macOS has no GPT tooling) —
`scripts/mkgpt.py` with correct CRC32s so UEFI boots it.

Everything QEMU-verified with zero `-d int` aborts throughout; a real-hardware
pass over the whole new surface is still outstanding. Design retrospective:
[the userland & pipelines postmortem](userland-and-pipelines-postmortem.md).
