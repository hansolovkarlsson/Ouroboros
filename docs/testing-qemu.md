# Running and testing Ouroboros on QEMU

The practical guide to booting Ouroboros under QEMU — the fast dev loop the whole
project relies on — from a single machine up to a **two-node cluster** on a shared
virtual network. Companion to [`manual.md`](manual.md) (which covers *using* the
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

```sh
python3 scripts/drive-qemu.py build/espext2.img \
    'login@@root' 'assword@@root' '# @@id' '# @@cat /etc/shadow'
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
(they must run when nothing is mounted — see [`manual.md`](manual.md)).

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

# The GUEST reads a file served by the HOST:
make run-image-9p-client                            # a NIC, no hostfwd needed
python3 scripts/np9p_server.py 5641                 # on the host; serves a small tree
#   ...then in the guest shell:
#   mount -r 10.0.2.2:5641 /mnt/a ; ls /mnt/a ; cat /mnt/a/HELLO.TXT
```

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
# a reply signed by the wrong machine (we check the key for the address we dialled)
python3 scripts/np9p_client.py localhost 5640 readdir / --peer=node-b     # -> REPLY NOT VERIFIED
```

The shared `\CLUSTER.KEY` (v0.10.0–v0.15.0) authenticates nothing now and is no
longer staged onto any image; `--legacy-mac` exists only so its refusal is
demonstrable rather than assumed.

This host↔guest round trip is the real cross-implementation check: the python
`hmac` and the guest's hand-rolled `netd`/`hmac.rs` must agree byte-for-byte, or
the correct key is rejected too (that's how a magic-byte transposition in the
python peers was caught — see `docs/cluster-auth-postmortem.md`). A machine with
a `\NOEXEC` flag file authenticates mounts but refuses `cpu` remote-exec.

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
served reply). See `docs/dial-out-postmortem.md` / `docs/dial-in-postmortem.md`.

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

**What to do once both are up** — see [`manual.md`](manual.md)'s cluster section
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

## 6. USB and GIC variants

```sh
make run-usb-kbd     # + an xHCI controller & a USB keyboard (HMP monitor sendkey)
make run-usb-multi   # + a USB tablet and a storage stick (the 3-device xHCI rig)
make run-gicv3       # force GICv3 instead of QEMU's default GICv2
```

On the USB targets you can inject keystrokes through the monitor socket:
`printf 'sendkey u\n' | nc -U qemu-monitor.sock`. (USB on *real* hardware is a
Parallels story — see [`manual.md`](manual.md).)

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
see [`manual.md`](manual.md)'s Parallels section.

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
