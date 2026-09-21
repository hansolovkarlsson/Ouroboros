# Running and testing Ouroboros on QEMU

The practical guide to booting Ouroboros under QEMU — the fast dev loop the whole
project relies on — from a single machine up to a **two-node cluster** on a shared
virtual network. Companion to [`manual.md`](../manual.md) (which covers *using* the
OS once it's booted) and [`testing-exfat.md`](testing-exfat.md) (the exFAT disk
rig). Every command here is a `make` target defined in the repository `Makefile`.

Prerequisites: `brew install qemu` (which also provides the aarch64 OVMF firmware
the targets point at) and macOS's `hdiutil` (for `make image`). For the ext2 test
disk, also `brew install e2fsprogs`.

Quit any QEMU instance with **`Ctrl+a x`**.

---

## 1. The fast single-machine runs

```sh
make run          # fastest loop - a vvfat-backed disk (FAT16!), no real FS
make run-image    # boots build/esp.img - a real FAT32 disk; disk commands work
```

The everyday gotcha: **`make run`'s disk is FAT16** (an artifact of QEMU's vvfat
driver), which the FAT32-only filesystem server can't mount — so every disk command
prints "no filesystem mounted" there. Use **`make run-image`** whenever you want
`ls`/`cat`/`write`/`exec` and the rest to actually work; it builds and boots
`build/esp.img`, a genuine MBR+FAT32 disk.

`make image` (re)builds `build/esp.img` on its own; the `run-image*` targets depend
on it, so they rebuild it as needed.

---

## 1b. Driving the shell unattended (`scripts/drive-qemu.py`)

Every run above expects a human at the keyboard. `scripts/drive-qemu.py` removes
that: each argument is a `WAIT@@TYPE` step — wait for a regex to appear in *new*
guest output, then type a line — and it prints the transcript followed by the
abort count from QEMU's own `-d int` trace.

An empty `TYPE` waits without typing. A `TYPE` of **`<ENTER>`** types an *empty
line* — a bare Enter — which is how a prompt that must **refuse** an empty
answer gets tested. Without it every such rule was unreachable from this rig,
including `useradd`'s and `passwd`'s empty-password refusals.

```sh
python3 scripts/drive-qemu.py build/espext2.img \
    'login@@root' 'assword@@root' '# @@id' '# @@cat /etc/shadow'

# A prompt that must refuse an empty answer:
python3 scripts/drive-qemu.py build/espext2.img \
    'login@@root' 'assword@@root' '# @@useradd bob' \
    'New password@@<ENTER>' 'Retype@@<ENTER>'      # -> "password may not be empty"
```

This is what makes login, permission enforcement and `cpu` testable in a loop.

**Why it types one character at a time.** The PL011 the guest reads has **no RX
FIFO**, so a byte arriving while the guest is not polling is not queued — it is
gone. Piping a script straight into QEMU loses most of it. Two consequences,
both load-bearing:

1. Characters go out individually with a delay.
2. Each step matches only output produced **since the previous step**. Searching
   the whole buffer matches an *earlier* prompt and starts typing while the
   guest is still printing, which silently eats the first characters of the
   command — `cat /etc/shadow` arrives as `t /etc/shadow`, and the test fails
   for a reason that has nothing to do with the code under test.

A timeout prints which pattern it was waiting for, which is usually enough to
see whether the guest died or the prompt simply differs from the regex.

**The firmware hangs before its own `BdsDxe: loading Boot0001` and `BdsDxe:
starting Boot0001` lines about one boot in six on this host**, with no kernel output at all: the transcript ends
at the firmware's clear-screen and the first pattern waited for (`login:`)
times out. Measured 2026-09-20 (QEMU 11.1.1, edk2-stable202408): 1 hang in 6
bare boots, pauses between boots making no difference, and the same image
booting on every retry. Not attributed. Nothing of ours has run by then, so a
boot without that line is not evidence about the kernel: rerun it.
`scripts/test-keyboard-chain.sh` retries a boot whose transcript lacks the
`BdsDxe: starting` line, up to three times, and says so; a boot that reaches
it is never retried. This paragraph is the one statement of the hang; the
script and the Makefile point here.

**The nested shell keeps the keyboard.** The keyboard reverts, on its owner's
death, to the task that held it when `fg` handed it over (`PREVIOUS_OWNERS`
in `tasks.rs`), which is what lets a shell spawned from a shell run more than
one command. Misdiagnosed once (2026-09-06, "a builtin, no child") and fixed
on 2026-09-20. **`make test-keyboard-chain`** (`scripts/test-keyboard-chain.sh`)
runs the recipes below plus a fourth (four shells deep, `fg` back one level,
a kill, a Ctrl+C: the live shell two levels up must answer) and grades each
on the lines its negative control lacked; the target rebuilds the image
first (the Makefile is the authority on that). The first, by hand, on
`build/esp.img`:

```sh
python3 scripts/drive-qemu.py build/esp.img 'login:@@root' 'assword@@root' \
  '# @@exec /EFI/ORBS/SH.BIN' 'login:@@fg 6' '@@root' 'assword@@root' \
  '# @@cd /EFI' '# @@echo hi' '# @@pwd' '# @@ps' '# @@'
# expected: pwd prints /EFI (the nested shell's cwd) and ps shows
# "task 6: runnable" with task 0 blocked. The negative control, measured on
# the kernel before the fix: pwd prints / and ps shows task 0 runnable,
# task 6 blocked, because the boot shell answered.
```

The chain, three shells deep, with the middle one killed from below (a link
dying while NOT the owner): after Ctrl+C at the innermost prompt the keyboard
must reach the outer nested shell, not the boot shell. `$'\x03'` is zsh and
bash quoting for the raw Ctrl+C byte; under plain `sh` it is four literal
characters, the driver types those, and the run times out at the same step
as the negative control.

```sh
python3 scripts/drive-qemu.py build/esp.img 'login:@@root' 'assword@@root' \
  '# @@/EFI/ORBS/SH.BIN' 'login:@@root' 'assword@@root' '# @@cd /EFI' \
  '# @@/EFI/ORBS/SH.BIN' 'login:@@root' 'assword@@root' \
  '# @@exec /EFI/ORBS/SH.BIN' 'login:@@fg 8' '@@root' 'assword@@root' \
  '# @@kill 7' '# @@'$'\x03' '# @@pwd' '# @@ps' '# @@'
# expected: pwd prints /EFI (task 6 answered) and ps shows task 6 runnable
# with task 0 blocked. Negative control, measured with the splice loop in
# revert_input_owner_if removed: after "foreground task 8 terminated" no
# prompt ever comes back (the rig times out waiting for `# `), because the
# keyboard fell to the boot shell, blocked in wait on task 6.
```

Handing back down the chain: shell 7 (handed the keyboard by shell 6) hands
it back with `fg 6`, which pops 7 off the chain; 6 kills 7, then Ctrl+C at
6's prompt. The keyboard must reach the boot shell.

```sh
python3 scripts/drive-qemu.py build/esp.img 'login:@@root' 'assword@@root' \
  '# @@exec /EFI/ORBS/SH.BIN' 'login:@@fg 6' '@@root' 'assword@@root' \
  '# @@cd /EFI' '# @@exec /EFI/ORBS/SH.BIN' 'login:@@fg 7' '@@root' \
  'assword@@root' '# @@fg 6' '# @@kill 7' '# @@'$'\x03' '# @@pwd' '# @@'
# expected: pwd prints / (the boot shell answered). Negative control,
# measured on the kernel that pushed unconditionally (fg 6 from 7 made a
# cycle, and the kill spliced 6 onto itself): after "foreground task 6
# terminated" no prompt ever comes back, the keyboard stranded on the
# empty slot 6.
```

## 2. Disk-format test images

Ouroboros's filesystem server (`fsd`) mounts FAT32, exFAT, and ext2, and discovers
partitions on MBR or GPT disks. Each format has a purpose-built test disk:

```sh
make run-image         # FAT32 (the default boot disk)
make run-image-gpt     # the FAT32 disk wrapped in a bootable GPT (tests GPT discovery)
make run-image-exfat   # a two-partition MBR: exFAT first (fsd mounts it) + FAT32 ESP (UEFI boots it)
make run-image-ext2    # same two-partition trick with ext2 first  (needs `brew install e2fsprogs`)
```

The two-partition images (exFAT/ext2) are the trick that lets `fsd` mount a
non-FAT filesystem while UEFI still boots from a FAT32 ESP: partition 1 is the
filesystem under test, partition 2 is the bootable ESP. Inside the guest, `fsd`
probes FAT32-then-exFAT-then-ext2 and mounts the first that validates, so it lands
on partition 1. See [`testing-exfat.md`](testing-exfat.md) for the full exFAT
round-trip (including verifying against macOS's own `fsck_exfat`).

**Disk-management from inside the guest** (works on any of these, or a blank disk):
`erase disk`, `partition [fat32|exfat|ext2]`, then `format [fat32|exfat|ext2]`
lay down a fresh filesystem; `mount -a` mounts it. These are shell *builtins*
(they must run when nothing is mounted — see [`manual.md`](../manual.md)).

---

## 3. Networking (single machine)

```sh
make run-net          # a virtio-net NIC + QEMU user-net (SLIRP) + a net.pcap dump
make run-image-net    # real FAT32 *and* the NIC in one boot - the fullest single run
make run-image-server # + SLIRP hostfwd tcp::5555->:80 so the host can reach netd's HTTP server
```

- **Client ops:** boot `make run-image-net`, then at the shell `ping 10.0.2.2`,
  `resolve example.com`, or `fetch example.com`. SLIRP reaches the outside world.
- **Server:** boot `make run-image-server`, then on the host `curl
  http://localhost:5555/` — the guest's from-scratch TCP stack serves a page. Any
  path streams a file from `fsd` (`curl http://localhost:5555/EFI/ORBS/INIT.CFG`);
  a directory returns a browsable HTML index. Every frame is dumped to
  `build/net.pcap` for `tcpdump`/Wireshark inspection.

QEMU's user-mode networking assigns the guest **10.0.2.15**, gateway **10.0.2.2**,
DNS **10.0.2.3**. (Parallels' virtio-net is PCI, which this project's virtio-mmio
path doesn't drive — networking is QEMU-only.)

---

## 4. The 9P export, tested from the host

`netd` exports the guest's filesystem over 9P-over-TCP (port 564). Two host-side
python peers (no dependencies beyond python 3) act as the "foreign observer":

```sh
# The host reads the GUEST's disk over TCP:
make run-image-9p                                   # adds a hostfwd tcp::5640-:564
python3 scripts/np9p_client.py localhost 5640 readdir /
python3 scripts/np9p_client.py localhost 5640 read /EFI/ORBS/INIT.CFG
python3 scripts/np9p_client.py localhost 5640 stat /EFI/ORBS/INIT.CFG
python3 scripts/np9p_client.py localhost 5640 mv /A.TXT /B.TXT       # NP_MV, raw paths
python3 scripts/np9p_client.py localhost 5640 session /EFI/ORBS      # readdir+stat+read on ONE connection (NP_SESSION)
```

**The session gate** (step 4 of `docs/roadmap/roadmap-fid-verbs.md`, 2026-09-07):
`session-gate` runs the plan's checks and negative controls against the live
export and prints one PASS/FAIL line each (idle hold, the budget of three, a
one-shot served beside them, no eviction, a fifth connection refused not hung,
a peer's RST returning its slot, silent sessions reaped after 30 s, the slots
back). It takes about a minute, most of it the reap wait, and the guest
transcript is the other half of the first check: no `wedged`/`restarted`
line. The `cpu` run at the end is what the last guest step keys on:

```sh
scripts/run-guest.sh -- python3 scripts/np9p_client.py localhost 5640 session-gate 5
# expected: 11 PASS, "0 check(s) failed", then
#   run-guest: guest alive at the end - the run means what it says
#   run-guest: 0 restart line(s), 0 abort line(s)
```

**The fid gate** (step 5, 2026-09-12): `fid-gate` runs the plan's check and
controls for the three fid verbs the export serves on a session, one PASS/FAIL
line each: open→fstat→clunk with the `fstat` record byte-equal to `NP_STAT`'s;
the clunked fid refused; a never-opened fid refused; a fid from session A
refused on B and still served on A; an old number dead on a fresh session;
12 opens over three closed sessions against `fsd`'s table (`ninep_abi::MAX_FIDS`,
which the gate's product must exceed; the wire checker asserts it), the check
that fails when closing a session does not clunk; the fifth open on a session
`FS_ERR_BUSY`; a fid verb on a one-shot connection `FS_ERR_NO_SUCH_VERB`;
another user refused `FS_ERR_PERM` while the owner keeps the fid; another
user's `NP_CLUNK` refused `FS_ERR_PERM` while the owner still holds the fid and
then closes it (added by the review of #130: the freeing verb's ownership test
had no observer at all); and, since step 6 (2026-09-12), the data path: a
`pread` of a never-opened fid refused; another user's `pread` refused
`FS_ERR_PERM` with the owner's next read served; `/man/grep` (longer than one
`NP_REMOTE_CHUNK`) read to EOF through a fid in chunks and byte-compared
against the same file read path-based over `NP_READ_AT`, two independent
paths to the same bytes, the check FAILING on a file that fits one chunk (a
same-length compare that passes on a short read is the failure mode, and a
one-chunk file cannot show it); then EOF answering 0 and the session still in
phase. The one-shot refusal check sends `NP_PREAD` beside `NP_OPEN`. About
twenty seconds after boot. Against `main` before step 5 it fails at the first
open; against step 5's export the four step-6 checks fail with
`FS_ERR_NO_SUCH_VERB` and the rest pass (10 of 14, measured).

```sh
scripts/run-guest.sh -- python3 scripts/np9p_client.py localhost 5640 fid-gate
# expected: 14 PASS, "0 check(s) failed", then the two run-guest lines above
```

**Its two step-6 controls, both measured 2026-09-12**, each failing the byte
compare and nothing else. On the client, make the expected buffer one byte
short: in `do_fid_gate`, `via_path = via_path[:-1]` just before the `ok =`
line of the compare (4661 through the fid, 4660 path-based, DIFFER). On the
export, shift the read: in `build_9p_reply`'s `NP_PREAD` arm, forward `p1 + 1`
as the offset (4660 through the fid, 4661 path-based). A short-read mutation
(`DATA_INLINE - 1` as the cap) is NOT a control: a short read is legal and the
client continues from it, so the file still arrives whole and the gate
rightly passes. Revert each with `git checkout`, having committed first.

**The path gate** (step 6, 2026-09-12): the fold that put the fid verbs into
`build_9p_reply` rewrote the preamble every path verb runs through (the header
decode, which request word carries the path length, the namespace resolution,
the console / `/net` / remote refusals), and the fid gate exercises none of
`write`/`read_at`/`write_at`/`mv`/`chmod`/`touch`/`rm`/`mkdir`. `path-gate`
runs each end to end on a scratch file (`/PGATE.TXT`, `/PGATE2.TXT`,
`/PGATE3.TXT`, `/PGATED`, removed at the end), plus the console arm (an
`NP_WRITE` to `/dev/cons`, which prints on the guest console, and a read
refused as no arm), the `/net` arm (`/net/ip` reads as an address), and on a
session `NP_OPEN` of `/net/ip`, `/net/tcp/clone`, `/net/tcp` and `/dev/cons`
refused `FS_ERR_NO_SUCH_VERB` with a path verb still served after. The two
`/net/tcp` paths are there because the first fold answered them with a bare
`FS_ERROR` and `FS_ERR_NOT_A_FILE` (the `/net` arm hands that subtree to the
dial code before looking at the verb) while `/net/ip` was refused correctly,
so the check passed and saw nothing; measured on that build before the fix
(review of the step-6 PR). `NP_CHMOD` answers `FS_ERR_NOT_SUPPORTED`
on FAT32 and 0 on ext2; either means the arm reached `fsd`, and the check says
which it saw.

```sh
scripts/run-guest.sh -- python3 scripts/np9p_client.py localhost 5640 path-gate
# expected: 12 PASS, "0 check(s) failed", then the two run-guest lines above
```

Its control is the selector: in `path_len_word`, make the `_` arm answer
`Some(p1 as usize)` so every path verb reads its length from the wrong word.
Measured: 10 of 12 fail. The two that survive, `read_at` with `a1` = 100 and
`chmod` with `a1` = 0o600, survive because a too-long length is clamped to the
whole payload, which for a path-only request is the path; so a wrong word is
invisible whenever the wrong number is larger than the right one, and the
other ten are the checks that see it.

**The fid reaper check** (2026-09-12, the `fsd`-side follow-up to step 5):
`fsd` reaps a leaked fid, when its table is full, by comparing the owning
slot's current `TASK_IDENTITY` against the identity recorded at open. The
condition that distinguishes that from "is the slot dead" cannot be made from
the shell, so `/bin/CLEAK` (`libc/cleak.c`) exists: it opens a file and exits
through the raw `EXIT` syscall, past `_exit`'s close-all. The shell runs every
foreground command in the same slot, so `MAX_FIDS + 1` runs in a row leave
`MAX_FIDS` leaked fids under earlier generations of one slot, and the last
run's open succeeds only if the reaper frees by identity. `cfile` after it
shows the table is usable again.

```sh
python3 scripts/drive-qemu.py build/esp.img 'login:@@root' 'assword@@root' \
  '# @@cleak' '# @@cleak' '# @@cleak' '# @@cleak' '# @@cleak' \
  '# @@cleak' '# @@cleak' '# @@cleak' '# @@cleak' '# @@cfile' '# @@'
# expected: nine "cleak: opened fd 3 and leaked it", then cfile's
# "wrote and re-opened /CTEST.TXT". Nine because MAX_FIDS is 8; if that
# constant moves, the run count moves with it.
```

Against `fsd` as of `main` before the change (its reaper asked `TASK_STATE`,
and the ninth run is alive in the very slot the eight leaks name) the ninth
prints `cleak: open failed` and `cfile` prints `open (write) failed`: the table
is stuck for the boot. That is the negative control, measured. The same
mechanism is what reclaims a restarted `netd`'s fids (a supervisor restart is a
new generation in the same protected slot); that case is not exercised by a
rig, and rides on the identity comparison this run does exercise.

**The second control for the same change** is the fid gate's three co-tenant
checks (fstat, clunk, and since step 6 pread) with `netd`'s own per-user test
removed (`session_slot`, the `opener.uid != proxy.uid` refusal; it was in
`fid_verb_reply` until step 6 folded that function away). Before the change
that mutation made `fsd` drop the owner's fid, and the fstat check failed; now
`fsd` refuses the co-tenant `FS_ERR_PERM` and keeps the fid, and the gate stays
14 of 14 on `fsd`'s wall alone (re-measured after step 6; it was 11 of 11 at
step 5). The clunk check is the one that observes the freeing verb:
with the same `netd` mutation AND `fsd`'s `handle_fid_op` made to skip the uid
test for `NP_CLUNK`, it fails (the clunk as `user` answers 0 and the owner's
next fstat `FS_ERROR`) while the fstat check still passes, so it can fail, and
only for the reason it exists. Revert both mutations with `git checkout`
afterwards, and COMMIT FIRST: a checkout restores the committed file, and on
2026-09-12 it took uncommitted fixes with it.

**Use `run-guest.sh`, not `drive-qemu.py`, for any host-side client that runs
longer than a shell step.** `drive-qemu.py` types its steps, lingers four
seconds and kills QEMU; a client still running then sees connection refusals
that read exactly like the export refusing it. The session gate was measured
that way on 2026-09-07 and its reap check reported `3/3 reaped` against a guest
that had already been killed. `run-guest.sh` boots the guest for as long as the
client needs, waits for `export open` rather than sleeping, and **asserts the
guest is still alive at the end**, exiting 99 if not. See
[`blind-instruments-postmortem.md`](../postmortems/blind-instruments-postmortem.md).

**Forcing a retransmit.** SLIRP never drops a segment, so every loss-recovery
path in `netd` is otherwise argued rather than run. In `pump_send`'s file-body
arm, swallow exactly one new segment once (a `bool` on `TcpConn` and
`if !c.dropped_one && c.read_off == (SERVE_CHUNK as u64) * 2`, still advancing
`snd_nxt`/`read_off`), then fetch a file big enough to have several segments in
flight:

```sh
scripts/run-guest.sh --hostfwd=tcp::5555-:80 -- \
  curl -s --max-time 120 http://localhost:5555/EFI/ORBS/FSD.BIN -o /tmp/got.bin
shasum -a 256 /tmp/got.bin build/esp/EFI/ORBS/FSD.BIN   # must match
```

The peer dup-ACKs, netd fast-retransmits, and the hashes match. This is the
only rig in the tree that reaches a retransmit path, and the one measured
finding of the 2026-09-07 TCP arc came from it: with the incoming-ACK bound
placed on `snd_nxt` (a cursor `rewind_to` moves backwards) rather than the send
high-water mark, the same fetch truncated at 11,303 of 140,088 bytes. That fix
is NOT in the tree - the branch carrying it was abandoned, see `ROADMAP.md` -
so the rig is recorded here for whoever builds the next attempt. Remove the
mutation afterwards; nothing reaches this path without it.

Every line can fail: against an export without the verb the gate stops at the
first open with `FS_ERR_NO_SUCH_VERB`; with `SESSION_MAX` raised to `MAX_CONNS`
and `CONN_IDLE_TICKS` made huge it fails on the budget, on the one-shot beside a
full table, and on the reap (measured, 2026-09-07).

`stat` really does send `NP_STAT` (it sent `NP_READ_FILE` until 2026-09-02 and
printed the byte count as a "size", so it could not exercise the one verb that
reaches `fsd`'s ancestor walk without going through `path_allows`). `mv` exists
because the guest's own `/bin/mv` guards `mv f f` before `fsd` ever sees it, so
the server-side guard — the one protecting the path where paths arrive raw — had
no client that could reach it.

```sh
# The GUEST reads a file served by the HOST:
make run-image-9p-client                            # a NIC, no hostfwd needed
python3 scripts/np9p_server.py 5641                 # on the host; serves a small tree
#   ...then in the guest shell:
#   mount -r 10.0.2.2:5641 /mnt/a ; ls /mnt/a ; cat /mnt/a/HELLO.TXT
```

**Run at least one of these PIPED, and one under `exec`.** Unattended:

```sh
python3 scripts/drive-qemu.py --slirp build/esp.img \
  'login:@@root' 'assword@@root' \
  '# @@mount -r 10.0.2.2:5641 /mnt/a' \
  '# @@cat /mnt/a/HELLO.TXT | wc' \
  '# @@cat /mnt/a/BIG.TXT | wc' \
  '# @@ping 10.0.2.2 | wc' \
  '# @@exec /bin/cat /mnt/a/SUB/NOTE.TXT' \
  '# @@'
```

Expected `40 400 1960`, `200 2600 13600`, `1 3 21`, and the note's one line.

Not decoration: a spawned command reaches `netd` only through a capability the
shell delegates, and until 2026-09-06 **two of the shell's five spawn paths did
not delegate it** — so every network program was broken in a pipeline and under
`exec`, for months, while every recipe on this page ran unpiped commands and
passed. The bug was invisible to the tests that were supposed to cover it
because they all exercised the one spawn path that worked. `BIG.TXT` is also
the control that the failure is not a timeout: it is ~28 round trips over ~17 s,
far past the supervisor's 2.56 s wedge threshold, and it must **succeed** —
that is what distinguishes a capability denial (fails instantly, no packets)
from a server restart (`server slot 4 wedged` on the console).

**A client alive across a `netd` restart.** A spawned program reaches `netd`
only through the `TO_NET` grant the shell makes once, at spawn; until
2026-09-06 the server's crash teardown stripped that grant from every live
task and nothing made it again, so any program alive across a supervised
restart was netless for the rest of its life. The kernel now keeps grants
aimed at a protected slot across its teardown. Nothing in the tree can force
a restart on demand, so the check uses **two temporary mutations, applied for
the run and reverted after** (`git checkout` the two files):

- in `programs/servers/netd/src/main.rs`, at the top of the `NETOP_RESOLVE`
  arm: `if buf[8..end].starts_with(b"wedgeme") { loop { core::hint::spin_loop() } }`:
  a resolve of that name parks the server, and the supervisor's passive
  heartbeat restarts it after `WEDGE_TICKS` (about 2.6 s);
- in `programs/netutils/ping/src/main.rs`, replace the single request with
  eight, each followed by a 75-tick wait (`get_ticks` + `yield_now`), printing
  `reply from` or `ping: request failed` each time via `con_write`.

```sh
python3 scripts/drive-qemu.py --slirp build/esp.img \
  'login:@@root' 'assword@@root' \
  '# @@exec /bin/ping 10.0.2.2' \
  'reply from@@resolve wedgeme' \
  'wedged@@' \
  'reply from|request failed@@' 'reply from|request failed@@' \
  'reply from|request failed@@' 'reply from|request failed@@' \
  'exited@@ps' '# @@'
```

The trigger is keyed on ping's first reply, not the prompt: `exec` returns the
prompt before ping prints, and a step waiting for `# ` there matches nothing
new and times out. Expected: *"server slot 4 wedged … restarting"*, one
`ping: request failed` (the request in flight when the server died), then
**`reply from 10.0.2.2` resuming** for the remaining requests. Against the
pre-fix kernel the same run gives `ping: request failed` for every request
after *"restarted (attempt 1/3)"*, six of six, each after the client's
150-tick spin. `resolve` itself reports *"no network server this boot"*, which
is the mid-call death answer, not a boot condition; recorded, not fixed.

**A recycled slot must not inherit a server's memory of its predecessor.**
`netd` used to remember its remote-exec child by bare slot number; since
2026-09-06 it records the kernel-issued task identity (`TASK_IDENTITY` at
spawn, `SENDER_TASK` on every message). The check needs a host-side peer and
the guest shell at once, which is what `drive-qemu.py`'s `--hostfwd` is for
(it implies `--slirp`; macOS has no `timeout`, hence the perl alarm):

```sh
(sleep 40; perl -e 'alarm 100; exec @ARGV' \
   python3 scripts/np9p_client.py localhost 5640 run 'touch /X1' > /tmp/run.log 2>&1) &
python3 scripts/drive-qemu.py --hostfwd=tcp::5640-:564 build/esp.img \
  'login:@@root' 'assword@@root' \
  'exited \(code 0\)@@ps' \
  '# @@wait 6' \
  '# @@ping 10.0.2.2' \
  '# @@ps' '# @@'
```

`touch` never ends its stream, so the child exits into a zombie while the
connection still names its slot; the third step keys on the kernel's exit
line, `ps` shows slot 6 as *exited*, `wait 6` reaps it, and the `ping` lands
in slot 6. Expected: `reply from 10.0.2.2`. Against the pre-fix `netd` the
ping's request is captured as the child's output and the prompt never
returns (the harness times out on the last steps). The same `--hostfwd` rig
with `run 'ls /'` is the control: the host prints the guest's root listing,
and `ping 10.0.2.2 | wc` in the guest answers `1 3 21` beside it.

**A nested shell can wire its own pipelines and reach the network.** Until
2026-09-06 a spawned `SH.BIN` could authorize nothing (every `DELEGATE` it
issued was refused, since spawnable slots hold no spawnable slot statically);
the kernel now records each task's parent and lets a task pass a right it
holds to its own children. Both shells print the same `# ` prompt, so each
step is keyed on the previous command's output, not on the prompt. Until
2026-09-20 the keyboard went back to the boot shell after every nested command
and every step here had to be preceded by `fg 6` (see section 1b); the recipe
below is the one run after that fix, with no `fg` between commands. `ps` from
inside shows task 6 runnable and task 0 blocked, which is how to know which
shell answered.

```sh
python3 scripts/drive-qemu.py --slirp build/esp.img \
  'login:@@root' 'assword@@root' \
  '# @@exec /EFI/ORBS/SH.BIN' \
  'login:@@fg 6' 'fg 6@@root' 'assword@@root' \
  '# @@ls / | wc' \
  '1 5 40|authorize@@ping 10.0.2.2' \
  'reply from|failed@@ping 10.0.2.2 | wc' \
  '1 3 21|authorize|failed@@ps' \
  'task 10@@exit' 'logout@@'
```

Expected: `1 5 40`, `reply from 10.0.2.2`, `1 3 21`, and a `ps` with task 6
runnable. Against the pre-fix kernel the first answers `pipe: could not
authorize the stream: permission denied (...)` and the ping `ping: request
failed`; the later commands may then *look* fixed, because they ran in the
boot shell after the keyboard reverted, which is what the `ps` step is for.

**Authentication.** Every request is **signed** with a per-machine Ed25519 key,
and the exporter serves only a public key listed in its
`/etc/cluster/authorized`. Both python peers hold the dev "host" identity, which
`scripts/mkclusterkeys.py` puts in every image's `authorized`, so the commands
above work unchanged. Three ways to prove the gate:

```sh
# a key the guest does not authorize
python3 scripts/np9p_client.py localhost 5640 readdir / --sign=nobody     # -> AUTH FAILED
# the RETIRED shared-key MAC format, which nothing accepts any more
python3 scripts/np9p_client.py localhost 5640 readdir / --legacy-mac      # -> AUTH FAILED
# a reply signed by the wrong machine (we check the key for the address we dialled).
# node-b IS in the guest's `authorized`, which is what makes this control mean
# something: it is refused for being the wrong PEER, not for being an unknown key.
python3 scripts/np9p_client.py localhost 5640 readdir / --peer=node-b     # -> REPLY NOT VERIFIED
```

`run` drives the `cpu` path from the host, which is the only surface where the
export's refusals are *prose* rather than a status code — a remote-run reply is
an output stream, not a framed reply:

```sh
python3 scripts/np9p_client.py localhost 5640 run "uptime"                # the guest's output
python3 scripts/np9p_client.py localhost 5640 run "uptime" --sign=nobody  # cpu: authentication failed ...
```

**A REFUSAL MUST NOT REVEAL THIS MACHINE'S OWN STATE.** The gate that answers an
unusable frame runs *before* authentication, on nothing but the magic number, so
whatever it says is said to a stranger. To check that, run the same refused
command against a configured node and one with no `/etc/cluster/id`, and compare
the two lines — they must be byte-identical:

```sh
# a second image with no identity (nothing stages this; do it by hand)
make esp && rm build/esp/etc/cluster/id
hdiutil create -size 64m -fs FAT32 -volname OUROBOROS -srcfolder build/esp \
    -format UDTO -ov build/noid.cdr && mv build/noid.cdr build/noid.img
make image        # restores the identity and rebuilds the normal image

# boot both with their own hostfwd, then:
python3 scripts/np9p_client.py localhost 5640 run "uptime" --sign=nobody   # configured
python3 scripts/np9p_client.py localhost 5642 run "uptime" --sign=nobody   # no identity
```

Until 2026-09-01 these differed — the second said "the remote machine cannot
serve signed requests (no identity of its own, or no authorized peers)" — so
anyone who could reach port 564 could fingerprint every node's configuration
while holding no key at all. The distinction now lives only in each machine's own
boot log, where it is useful and where nobody else can ask for it.

`--legacy-mac` is refused on the `run` path: the export does not parse the
retired format, so it cannot tell a `cpu` request from an fs one and answers with
a framed status either way, which this path would print as binary. Use it with
`readdir`/`read`/`stat`, where the reply is decoded.

The shared `\CLUSTER.KEY` (v0.10.0–v0.15.0) authenticates nothing now and is no
longer staged onto any image; `--legacy-mac` exists only so its refusal is
demonstrable rather than assumed.

This host↔guest round trip is the real cross-implementation check: the python
peer's Ed25519 and the guest's hand-rolled `ed25519` crate must agree
byte-for-byte over the same signed bytes, or a correctly-keyed peer is rejected
too (that is how a magic-byte transposition in the python peers was caught under
the older MAC format — see `docs/postmortems/cluster-auth-postmortem.md`). A machine with a
`\NOEXEC` flag file authenticates mounts but refuses `cpu` remote-exec.

The guest reaches the host at **10.0.2.2** over SLIRP with no hostfwd (SLIRP routes
guest→host automatically), which is why `run-image-9p-client` needs only a NIC.

**Dial-out and dial-in (`/net/tcp`).** `run-image-9p` also forwards `tcp::5900-:9000`,
so the same host client can drive the guest's `/net/tcp` over the export:

```sh
# Dial-OUT: make the guest dial a host TCP server out of ITS nic (run a host
# server on :8000 first; the guest reaches it at 10.0.2.2:8000):
python3 scripts/np9p_client.py localhost 5640 dial 10.0.2.2 8000 GET / HTTP/1.0

# Dial-IN: make the guest ANNOUNCE :9000 and accept an inbound connection; a
# host socket connects to it via the :5900->:9000 hostfwd as the external client:
python3 scripts/np9p_client.py localhost 5640 serve 9000 5900 HELLO-SERVED-VIA-GUEST
```

Both are foreign-observer round trips: `dial` proves the guest opened a real
outbound connection (a host server sees it arrive from the guest's NIC); `serve`
proves the guest accepted a real inbound one (the external host socket gets the
served reply). See `docs/postmortems/dial-out-postmortem.md` / `docs/postmortems/dial-in-postmortem.md`.

---

## 5. The two-node cluster (the real thing)

Two Ouroboros guests on a **shared L2 link** — a QEMU socket "hub", no host in the
middle — let you exercise the whole distributed stack (Phases 1–4): remote disk
mount, remote `/proc`/`/dev/cons`/`/net`, and remote execution (`cpu`).

```sh
# Terminal 1 - machine A (start it FIRST; it listens):
make run-image-2vm-a

# Terminal 2 - machine B:
make run-image-2vm-b
```

**How the link works.** `run-image-2vm-a` runs a QEMU `-netdev
socket,listen=127.0.0.1:12340`; `-2vm-b` runs `connect=127.0.0.1:12340`. That pair
is a virtual Ethernet hub joining the two guests at layer 2 — no SLIRP, no
gateway, no DNS. Each guest gets its own disk copy (`build/esp-a.img` /
`esp-b.img`, since two QEMU write-locks can't share one file) and its own pcap
(`build/net-a.pcap` / `net-b.pcap`).

**FAT32 or ext2? This one matters.** The `run-image-2vm-a`/`-b` pair boots the
**FAT32** image, which is right for everything except permissions — FAT32
records no mode, so `fsd` has nothing to enforce and **every remote request
looks permitted there regardless of who sent it**. That is not a bug in the rig,
but it means a permission test on it passes *before* a fix and *after* it,
proving nothing either time. For anything about who may read what across the
cluster, use the ext2 pair:

```sh
make images-2vm-ext2           # build BOTH node images first - see below
make run-image-2vm-ext2-a      # terminal 1, listens (port 12341)
make run-image-2vm-ext2-b      # terminal 2
```

**The two ext2 node images are no longer one disk copied twice.** They were, for
as long as the cluster shared a single symmetric key. Since per-machine keypairs
(2026-08-31) each node carries its own `/etc/cluster/id`, so `-a` and `-b` are
separate builds (`CLUSTER_NODE=node-a` / `node-b`) and `make images-2vm-ext2`
produces the pair. Their `authorized` files are identical — every node accepts
the same peers — so the only difference is which private key each holds. The dev
identities come from fixed seeds, so rebuilding one of them reproduces the same
keys rather than desynchronising the pair.

It has its **own link port**, so it can run alongside the FAT32 pair rather than
colliding with it (a collision shows up as an unrelated guest-side timeout,
which is a miserable thing to debug). `make image-ext2` stages
`/etc/cluster/id` onto that disk — without it `netd`'s export is fail-closed and
the rig cannot come up — at **mode 0600**, because ext2 is the one image where
`fsd` enforces modes and a machine's private key is what its whole identity
rests on: anyone who can read it can impersonate the machine.

**Driving both nodes unattended.** `scripts/drive-2vm.py` starts A, runs its
steps, then starts B and runs its steps while A stays alive — printing both
transcripts and both health bars:

```sh
make images-2vm-ext2      # the two nodes hold different keys: build them together
python3 scripts/drive-2vm.py build/espext2-a.img build/espext2-b.img \
  --a 'login@@root' 'assword@@root' '# @@' \
  --b 'login@@user' 'assword@@user' \
     '\$ @@mount -r 10.0.2.10:564 /mnt/a' \
     '\$ @@cat /mnt/a/etc/shadow' \
     '\$ @@'
```

**Note the trailing `'\$ @@'`** — a wait with nothing typed. Without it only the
4-second linger separates the last keystroke from the transcript print, and a
remote mount plus read under TCG routinely takes longer; the empty tail then
looks exactly like the refusal you were trying to observe. Both `--a` and `--b`
are required and each needs at least one step, because a run that types nothing
and exits 0 is worse than no run at all.

It shares `drive-qemu.py`'s `Guest` class rather than copying the console rules
— the paced typing and the match-only-new-output high-water mark are the
load-bearing parts, and a second copy would drift from the first.

**What the permission test should show, and why one line is not enough.** Since
2026-08-31 a remote request carries the requesting user's name, so the run above
prints `cat: permission denied` where it once printed A's password hashes. A
refusal on its own is weak evidence, though — a broken export refuses too — so
run the matrix, which needs the *served* cases as well as the denied one:

```sh
python3 scripts/drive-2vm.py build/espext2-a.img build/espext2-b.img \
  --a 'login:@@root' 'assword:@@root' '# @@ls /' \
  --b 'login:@@user' 'assword:@@user' \
     '\$ @@mount -r 10.0.2.10:564 /mnt/a' \
     '\$ @@cat /mnt/a/HELLO.TXT' \
     '\$ @@cat /mnt/a/etc/shadow' \
     '\$ @@cpu 10.0.2.10:564 id' \
     '\$ @@'
```

Expected: `HELLO.TXT` served, `/etc/shadow` refused, and `cpu … id` reporting
`uid=1000(user) gid=1000(user)` — the last of these is what proves identity
reaches a *spawned* command and not just a file verb. Repeat the same steps after
`logout` and a `root` login and all three should succeed, which is what
distinguishes "the far side enforces permissions" from "the far side is broken".

**Run the negative control too.** `git stash` the change (or check out `main`),
rebuild the image, and run the identical script: it should print the hashes and
report `uid=0(root)`. A permission test that has never been seen to fail is a
test whose passing means nothing — and on the FAT32 rig it *cannot* fail, which
is the whole reason this section says to use ext2.

**The client-session witness** (Decision 4 of `roadmap-fid-verbs.md`, the
client half of step 5, 2026-09-12). A shell `cat` over a remote mount is
path-based (one `NETOP_RMOUNT` per chunk); only a **C program** exercises the
held client session, because only `libc`'s fd path uses fids. `/bin/CBIG`
(`libc/cbig.c`) reads `/man/grep` (4661 bytes, nine `NP_REMOTE_CHUNK`s) off A
over the mount and byte-compares it against B's own identical copy of the same
file, so it self-checks with no host oracle. The read succeeds only if `netd`
holds ONE connection to A's export across the C `open`→`read`(nine preads)
→`close` (the far fid dies with the connection otherwise).

```sh
python3 scripts/drive-2vm.py build/espext2-a.img build/espext2-b.img \
  --a 'login:@@root' 'assword:@@root' '# @@ls /man' \
  --b 'login:@@user' 'assword:@@user' \
     '\$ @@mount -r 10.0.2.10:564 /mnt/a' \
     '\$ @@cbig' \
     '\$ @@'
# expected: "cbig: 4661 bytes over the remote mount match the local copy
# (4661 > one chunk)", 0 fault lines both nodes.
```

Its control is the session's whole point: in `session_rmount`, close the
session after every verb (`if let Some(sess) = sessions[slot].take() {
client_close(mac, &sess); }` before the refcount block) so it is not held; cbig
then FAILS (the second verb reaches a clunked fid), measured 2/2. Against
`main` a C `open()` on a remote mount answers `FS_ERR_NO_SUCH_VERB` (a fid verb
on a one-shot connection), the pre-session behaviour. **cbig is subject to the
same ~1/6 socket-link flake as any remote op here** (`cbig: remote … read
failed`, no fault): re-run rather than reading one failure as a regression.

**The remote-write witness** (step 7 of `roadmap-fid-verbs.md`, `NP_PWRITE`,
2026-09-12). `/bin/CWRITE` (`libc/cwrite.c`) writes an 800-byte pattern (two
`NP_REMOTE_CHUNK`s, so `write()` loops over one held session) through a remote
fid onto A's disk, reopens it, and reads it back, comparing against the
pattern. Run node B as **root**, which may write A's root-owned directory:

```sh
python3 scripts/drive-2vm.py build/espext2-a.img build/espext2-b.img \
  --a 'login:@@root' 'assword:@@root' '# @@ls /man' \
  --b 'login:@@root' 'assword:@@root' \
     '# @@mount -r 10.0.2.10:564 /mnt/a' \
     '# @@cwrite' \
     '# @@'
# expected: "cwrite: 800 bytes written through a remote fid and read back
# identical (800 > one chunk)", 0 fault lines.
```

**The permission control, and it dictates the rig.** Run node B as **user**
instead (`login:@@user` / `\$ @@` prompt): the `O_CREAT` open in A's
root-owned directory is refused, `cwrite: open (write) refused: permission
denied`, measured 2/2 where root's succeeds. This is the `w`-permission control
the plan names, and it is real ONLY on the ext2 pair. FAT32 records no mode, so
`fsd` has nothing to enforce and the write would go through for anyone. **Shown
failing:** feed the export's write bridge no data (in `build_9p_reply`'s
`NP_PWRITE` arm, pass `&[]` to `fsd_pwrite`); `cwrite` then fails every attempt
rather than reporting success. Against the pre-step-7 tree a remote `write()`
returned `-1`. cwrite is subject to the same ~1/6 socket flake as any remote op
(a first-op `capability` denial, no fault); re-run.

**A remote op fails spuriously now and then — know which message is which.**
Measured 2026-08-31 on this rig: roughly one remote read in six fails on the
shared socket link, on `main` as much as on any branch (3 scripted runs each:
2/6 failed ops on `main`, 1/6 on the branch under test). It is the same
intermittent the Phase 2 notes recorded as "intermittent first-ls on two-VM",
which the 4-try SYN retransmit reduced but did not eliminate. It is *not* a
permission result, and the two are told apart by the message:

| message | meaning |
|---|---|
| `cat: failed` | transport flake — **retry the step** |
| `cat: permission denied` | the far side enforced a mode; this is a real result |

Read the specific message, never just "the command failed" — a permission test
whose refusal you cannot distinguish from a dropped packet proves nothing. When
in doubt, repeat the step: the flake does not repeat, a refusal does.

**A `cpu` command's errors print on the machine that ran it.** Only the child's
*stdout* streams back over the cluster, so a denied `cpu A cat /etc/shadow` looks
empty on B and prints `cat: permission denied` on **A's** console. Read both
transcripts before concluding a step did nothing.

**How the IPs work.** `netd` derives each guest's IPv4 from its NIC's MAC (last
octet): the two-VM targets set MAC `…:0a` → **10.0.2.10** (machine A) and `…:0b` →
**10.0.2.11** (machine B). (The default QEMU MAC `…:56` maps back to `.15`, so the
single-VM SLIRP runs are unchanged.) Read a machine's own address any time with
`mount -n /net ; cat /net/ip`.

**What to do once both are up** — see [`manual.md`](../manual.md)'s cluster section
for the full command set. The essentials, typed in **machine B's** shell:

```
mount -r 10.0.2.10:564 /mnt/a       # mount machine A's disk
ls /mnt/a                           #   ...and read it
cat /mnt/a/proc/2/state             # A's filesystem-server state (its /proc)
cat /mnt/a/net/ip                   # A's address (its /net)
write /mnt/a/dev/cons hello         # prints "hello" on A's screen
cpu 10.0.2.10:564 ls /              # run `ls` ON A, output back here
cpu 10.0.2.10:564 cat /host/x       # run cat on A, reading THIS machine's /x
```

Watch the wire with `tcpdump -nr build/net-a.pcap` (or `-b`) to confirm it's real
cross-machine traffic. Zero exception-trace aborts is the health bar — see §7.

---

**The re-entrant session witness** (the stack-depth limitation on
`roadmap-fid-verbs.md`'s ledger, closed 2026-09-20). A remote-mount request
from a *local* client can reach `netd` while `netd` is inside a `cpu` run: the
shell spawns a pipeline's program stages before it runs a builtin source, so
`cpu <A> ping <unreachable> | <client>` has the client's `NETOP_RMOUNT` arrive
at `tcp_run`'s re-entrant drain, where a relay would sit on `handle_run`'s and
`tcp_run`'s frames as well as its own. The unreachable ping's ARP wait is what
keeps the run open long enough; a reachable command returns before the client
asks, and the request lands at the top level instead (measured: the file just
prints, the pcap shows the two connections 20 ms apart, not overlapping).
`netd` now refuses a non-child client's `NETOP_RMOUNT` from that drain, before
`handle_client`'s frame is built, with `FS_ERR_BUSY` - "out of room for this
right now"; the run and the cpu child's own remote-fs (Phase 4b) are
unaffected. The refusal is BEFORE the session/one-shot split, so both verb
kinds are refused at one point.

```sh
make test-reentrant-session   # builds both ext2 node images, then the two-node boots
# or, by hand (the graded one-shot recipe):
python3 scripts/drive-2vm.py build/espext2-a.img build/espext2-b.img \
  --a 'login:@@root' 'assword:@@root' '# @@ls /man' \
  --b 'login:@@root' 'assword:@@root' \
     '# @@mount -r 10.0.2.10:564 /mnt/a' \
     '# @@cpu 10.0.2.10:564 ping 10.0.2.99 | cat /mnt/a/man/grep' \
     '# @@'
# expected: "cat: that server is out of room for this right now (try again
# later)", no "server slot 4 restarted", 0 fault lines both nodes.
```

**Its negative control, measured before the refusal existed**: on node B,
`Ouroboros kernel: EL0 FAULT task=4 esr_el1=0x9200004f` (a data abort at the
guard page), `task 4 killed after fault`, `server slot 4 restarted (attempt
1/3)`, and the client reporting "that server is not running (it died with this
request in flight)". The one-shot path (`cat`) faulted as readily as the
session path (`cbig`), one call level shallower, so the ledger's "latent" was
wrong. **The graded check drives `cat`**, which reaches netd deterministically.
`cbig` (the session path, `libc`'s fid client) is run too but best-effort: a
pre-existing race delegates a spawned task's netd send right just after spawn,
so cbig's first request is sometimes refused by capability ("not allowed to
reach that server") before any remote request - unrelated to this witness, so
the script reports it and retries rather than failing. A fault on either path
is a regression and does fail.

## 6. USB and GIC variants

```sh
make run-usb-kbd     # + an xHCI controller & a USB keyboard (HMP monitor sendkey)
make run-usb-multi   # + a USB tablet and a storage stick (the 3-device xHCI rig)
make run-gicv3       # force GICv3 instead of QEMU's default GICv2
```

On the USB targets you can inject keystrokes through the monitor socket:
`printf 'sendkey u\n' | nc -U qemu-monitor.sock`. (USB on *real* hardware is a
Parallels story — see [`manual.md`](../manual.md).)

---

## 7. Scripted / headless testing, and the health bar

QEMU's `-nographic` routes the guest console to stdio, so you can drive the shell
by feeding it commands and reading its output. For anything beyond a quick manual
check — and for the two-VM cluster, where you're juggling two consoles — drive it
from a small script over a pty (Python's `pty.fork` + `select`), sending a line and
reading until the `$ ` prompt. The `scripts/np9p_*.py` peers and the two-VM targets
are meant to be driven this way.

**The health bar this project holds every change to:** boot with `-d int -D
<logfile>` (the `run-image-2vm-*` and several other targets already add it, writing
`build/int-*.log`) and confirm **zero `Data Abort` / `Prefetch Abort` / `SError`**
across the run:

```sh
grep -cE "Data Abort|Prefetch Abort|SError" build/int-*.log   # must be 0
```

`Taking exception … [SVC]` (syscalls), `[IRQ]` (the timer tick), and `[Hypervisor
Call]` (firmware PSCI at boot) are all expected and benign; an *abort* is a real
fault. A clean run is zero aborts.

**Real-hardware testing** (Parallels) is a separate path — `make test-parallels`;
see [`manual.md`](../manual.md)'s Parallels section.

---

## Quick reference — every run target

| Target | What it adds |
|---|---|
| `run` | fastest loop; vvfat FAT16 disk (no real FS) |
| `run-image` | real FAT32 disk (`build/esp.img`) — disk commands work; **+ virtio-rng** |
| `run-image-gpt` | FAT32 inside a bootable GPT (GPT discovery) |
| `run-image-exfat` | exFAT partition + FAT32 ESP (`fsd` mounts exFAT) |
| `run-image-ext2` | ext2 partition + FAT32 ESP (needs `e2fsprogs`); **+ virtio-rng** |
| `run-net` | virtio-net + SLIRP + `net.pcap` |
| `run-image-net` | real FAT32 **and** the NIC |
| `run-image-server` | + `hostfwd tcp::5555->:80` (host `curl` reaches netd) |
| `run-image-9p` | + `hostfwd tcp::5640->:564` (host reads guest's disk over 9P) |
| `run-image-9p-client` | NIC only; guest mounts a host-run 9P server |
| `run-image-2vm-a` | machine A of the two-node cluster on FAT32 (listen, IP `.10`, port 12340) |
| `run-image-2vm-b` | machine B of the FAT32 pair (connect, IP `.11`) |
| `images-2vm-ext2` | build both ext2 node images (they differ: per-machine keypairs) |
| `run-image-2vm-ext2-a` | machine A on **ext2** — the only rig that can test cluster *permissions* (port 12341) |
| `run-image-2vm-ext2-b` | machine B of the ext2 pair (connect, IP `.11`) |
| `run-usb-kbd` | xHCI + USB keyboard |
| `run-usb-multi` | xHCI + tablet + storage stick |
| `run-gicv3` | force GICv3 |
| `test-parallels` | scripted real-hardware smoke test (Parallels, not QEMU) |

**A note on `virtio-rng`.** **Every** target that attaches a disk also attaches
`-device virtio-rng-device`, so the `RANDOM` syscall works and `passwd`/`useradd`
produce real random password salts (the boot log says `virtio-rng ready, entropy
available to userland`).

It was briefly only two targets, on the theory that leaving it off elsewhere kept
the degradation path exercised. That was the wrong trade and a code review said
so: the default dev loop (`make run`) was then producing exactly the guessable
clock-derived salt this device exists to replace, which is a poor default to
ship and a poor one to develop against. The degradation path does not need a
QEMU target to stay honest - Parallels and the Pi have no virtio-mmio at all, so
it is the *permanent* state on every real machine, and `accounts::salt_from`
reports it out loud (`no hardware RNG - using a weaker clock-derived salt`).
