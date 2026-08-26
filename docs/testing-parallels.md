# Running and testing Ouroboros on real Parallels hardware

The practical guide to booting Ouroboros on **real Parallels Desktop hardware**
(Apple Silicon) — the project's actual test target, not an emulator. Companion to
[`testing-qemu.md`](testing-qemu.md) (the fast QEMU dev loop, single machine *and*
two-node cluster) and [`manual.md`](manual.md) (using the OS once booted).

Read the caveat below **first**: it decides what you can and can't validate here.

## The one caveat that shapes everything: no networking on Parallels (yet)

On real Parallels hardware the kernel sets `virtio_mmio_probe_safe = false` (it
keys off ACPI/SPCR console discovery, which Parallels doesn't provide), so
`main.rs` skips **both** `init_storage()` (virtio-blk) **and** `init_net()`
(virtio-net). Storage still works — through the separate **USB mass-storage /
xHCI** path — but **networking has no working path at all**:

- `netd`'s NIC driver is **virtio-mmio**, which is skipped on Parallels; and
- Parallels' own NIC is **virtio-PCI**, a transport this project deliberately
  does not have yet (see [`roadmap.md`](roadmap.md)'s "Platform reality").

So on Parallels `NET_MAC` returns `NET_ERROR`, `netd` reports *"no NIC this
boot,"* and **every network/cluster feature is unreachable**: `ping`, `resolve`,
`fetch`, `mount -r`, `cpu`, `dial`, the 9P export, cluster authentication, and
anything two-node. Those are all validated on **two QEMU VMs** instead (see
[`testing-qemu.md`](testing-qemu.md) §5) until a virtio-PCI transport exists.

**What you *can* validate on real Parallels:** the single-machine surface — boot,
console, USB keyboard, USB storage + the disk/filesystem commands, the shell and
`/bin` — **and** that the networked code *degrades gracefully* with no NIC
(fails cleanly, never faults or wedges). That last part is not busywork: two
releases (v0.10.0 auth, v0.11.0 dial-out) changed `netd`'s **boot path**, and the
change runs on Parallels even with no NIC — see [Risk #1](#risk-1--netd-boot-race)
below.

| Capability | Real Parallels | Two QEMU VMs |
| --- | --- | --- |
| Boot, console, USB keyboard | ✅ | ✅ |
| USB storage + disk/FS commands | ✅ (USB-MSD) | ✅ (virtio-blk) |
| Shell, `/bin`, pipelines, env | ✅ | ✅ |
| Networking (`ping`/`resolve`/`fetch`) | ❌ no NIC transport | ✅ |
| Cluster (`mount -r`/`cpu`/`dial`/export/auth) | ❌ no NIC transport | ✅ |
| Graceful no-NIC degradation | ✅ (this is worth testing) | n/a |

## Prerequisites

- **Parallels Desktop** (Apple Silicon). Its bundled `prl_disk_tool` builds the
  native disk; its `prlctl` CLI drives the VM headlessly.
- A registered Parallels VM (default name `Ouroboros`; see `prlctl list -a`),
  configured to boot from the disk built below.
- For the USB-storage checks: a **FAT32-formatted USB stick** to pass through.

## Building the bootable disk

```sh
make parallels-hdd     # build/esp.img -> build/esp.hdd (Parallels-native)
```

`make parallels-hdd` wraps `build/esp.img` into `build/esp.hdd` via Parallels'
own `prl_disk_tool`. (For a release image, `scripts/release.sh build` also
produces a self-contained `ouroboros-<ver>-esp.dmg`; wrap it into a `.hdd` with
the one-liner in the release notes.)

## The automated path — `make test-parallels`

`scripts/test-parallels.sh` (via `make test-parallels`) rebuilds `esp.hdd`, boots
the registered VM **headlessly**, types a `;`-separated list of shell commands
through `prlctl send-key-event` (real decimal PS/2 Set-1 scancodes), and saves a
`prlctl capture` screenshot **after each command** — no human watching the VM
live, no physical typing.

```sh
make test-parallels CMDS="help;echo hi;uptime;ls;env"
# overridable: VM_NAME=Ouroboros  BOOT_WAIT=12  (seconds to wait for boot)
```

Review the saved screenshots for correct output. **Caveat:** `send-key-event`
drives Parallels' *synthetic* keyboard, not the physical USB keyboard from the
xHCI postmortem — a legitimate scripted-regression stand-in, but **not** a
substitute for a real-physical-keyboard pass (checks A5 below).

## Part A — the single-machine validation matrix

Everything here runs on one Parallels VM. A1–A2 are the **new coverage** these
releases need; A3–A5 re-confirm the boot/USB surface (including the fixed xHCI
keyboard↔storage contention, which has real-hardware-only failure modes).

| # | Check | How | Pass signal |
| --- | --- | --- | --- |
| **A1** | Clean boot; `netd` does **not** wedge | Boot the `.hdd`; read the console during startup | `netd: no NIC this boot`, then either `cluster auth enabled` **or** `export CLOSED … (fail-closed)` — and **no** `server slot 4 restarted/wedged/failed` loop |
| **A2** | No-NIC graceful degradation | `ping 10.0.2.2` ; `mount -n /net` then `cat /net/ip` ; `dial /net 1.1.1.1 80` | Each fails with a *clean* "no network" message — **no** `EL0 FAULT`, no hang |
| **A3** | Shell + core commands | `make test-parallels CMDS="help;echo hi;uptime;ls;env;pwd"` | Screenshots show correct output |
| **A4** | USB disk + filesystem | With a FAT32 stick passed through: `ls`, `cat FILE`, `mkdir D`, `write F text`, `writeat F 0 x`, then reboot and re-`cat` | Files read/write/persist across reboot; no mid-transfer stall |
| **A5** | Physical keyboard | Type A1–A4 **by hand** on the real USB keyboard (not `send-key-event`) | Interactive typing works; no raw-HID-report flood, no keyboard death during disk I/O |

### <a name="risk-1--netd-boot-race"></a>Risk #1 — the `netd` boot race (the reason A1 exists)

`netd`'s `serve()` calls `load_auth()` **before** entering its event loop, and
`load_auth` does a *blocking* `read_file_chunk` to `fsd` for `\CLUSTER.KEY`, with
a retry loop on `NO_FS` (up to 50 tries × `NET_WAIT(40)` ≈ 2 s) for the case where
the disk isn't mounted yet.

On QEMU this is a non-event: virtio-blk auto-mounts before `netd` ever asks, so
`load_auth` returns on the first call. **On Parallels the disk is USB-MSD** —
slower to enumerate, and absent entirely if no stick is attached — so `load_auth`
can spend the full ~2 s in its retry loop. If the supervisor's health-ping fires
during that window (netd isn't draining its mailbox yet), `netd` could be
restarted mid-`load_auth`, potentially into the per-boot give-up cap — a boot
loop that **cannot reproduce on QEMU**. This is precisely the emulator-hides-a-bug
class the project keeps hitting (see the filesystems-arc and USB-storage
postmortems).

**If A1 fails** (netd restart/wedge loop at boot on Parallels): the fix is to stop
`load_auth` from blocking startup — e.g. make it lazy (read the key on first
export use instead of at boot) or bound the retry far tighter and treat "not yet"
as "closed for now, re-check later." That's a small, isolated patch — a `0.11.1`
candidate.

### Staging note: `CLUSTER.KEY` lives on the USB stick here

On Parallels, `fsd`'s mounted disk (tree 0) is the **USB stick**, not the boot
ESP — so `load_auth` reads `\CLUSTER.KEY` from the stick. Functionally it changes
nothing (there's no NIC, so the export is inert regardless), but to exercise the
*found-key* branch of `load_auth` rather than the retry-then-closed branch, put a
`CLUSTER.KEY` file at the root of the pass-through stick. Its absence is a valid
test too — you should see `export CLOSED … (fail-closed)`, not a crash.

## Part B — the cluster (blocked; the real path forward)

Genuine **two-node real-hardware** cluster testing is gated, in order, on:

1. **A virtio-PCI transport for the NIC** — the named sub-project in
   [`roadmap.md`](roadmap.md) ("Platform reality: the storage story again").
   Same shape as storage: virtio-mmio on QEMU, a separate PCI path for Parallels.
   Until it lands, `netd` has no NIC on Parallels, so **no cluster feature can
   run there at all.** This is independently the thing blocking *all* Parallels
   networking for the project, which makes it the highest-leverage networking
   arc regardless of cluster testing.
2. Then: two Parallels VMs (or a Parallels VM ↔ another machine) on a shared
   network, running the two-node matrix — `mount -r`, remote read/write, `cpu`,
   `dial`, matching-vs-mismatched `CLUSTER.KEY`, and the SIGKILL-a-node
   clean-disconnect check.

Every item in that matrix is **already validated on two QEMU VMs**
([`testing-qemu.md`](testing-qemu.md) §5) — the local `esp.img`-per-machine
socket-link setup, the auth cross-check via `scripts/np9p_client.py`, and the
dial-out foreign-observer test. That remains the cluster's validation path until
step 1 exists.

## Recommended sequence

1. **Run Part A now.** It's fast (`make test-parallels` for A3, a hand pass for
   A1/A2/A4/A5) and it's the responsible check after two releases that changed
   `netd`'s boot path. A1 is the one that could surface a real,
   QEMU-invisible bug.
2. **Treat virtio-PCI as the gate** for any real-hardware *cluster* validation —
   and, because it also unblocks all Parallels networking, as a strong candidate
   for the next arc in its own right.

## Related

- [`testing-qemu.md`](testing-qemu.md) — the QEMU dev loop and the two-node
  cluster testing that covers everything Parallels currently can't.
- [`roadmap.md`](roadmap.md) — the virtio-PCI transport sub-project (Part B step 1).
- [`xhci-keyboard-postmortem.md`](xhci-keyboard-postmortem.md),
  [`usb-storage-postmortem.md`](usb-storage-postmortem.md),
  [`filesystems-arc-postmortem.md`](filesystems-arc-postmortem.md) — the
  real-hardware bug classes A4/A5 guard against.
