# Releasing Ouroboros

How releases are cut, and the versioning scheme they follow. This is the
process; the forward-looking work itself lives in
[`roadmap.md`](roadmap.md) / [`roadmap-cluster.md`](roadmap-cluster.md),
and the per-milestone record in [`CHANGELOG.md`](CHANGELOG.md).

## Versioning scheme

Semver-shaped, pre-1.0 — `0.MINOR.PATCH`, where the leading `0.` is a
deliberate, honest signal: nothing here is API- or on-disk-format-stable
yet, and a release may change either.

- **Minor bump (`0.N.0`)** — one **completed arc**. An arc is the unit the
  CHANGELOG and the postmortems already think in: a coherent capability
  landing end to end (the network stack; the filesystems arc; disk
  management; a cluster phase). This is the normal release trigger.
- **Patch bump (`0.N.P`)** — an **isolated fix** on top of a released
  minor, with no new arc: a hardware bug, a regression, a security fix.
  (The xHCI keyboard↔storage contention fix is the archetype — had it
  landed after `0.4.0` shipped, it would have been `0.4.1`.)
- **`1.0.0`** is not near. It means committing to a stable syscall ABI and
  on-disk formats. Explicitly out of scope until the cluster direction
  (`roadmap-cluster.md`) has shaken out what those interfaces even are.

The single source of truth for the current number is the top-level
[`VERSION`](../VERSION) file. `kernel/Cargo.toml`'s `version` is kept in
step with it.

### Version history

Real, artifact-bearing releases start at **`0.4.0`** — the first cut, which
bundles *everything built to date* (four major arcs already in the tree:
boot/shell/FAT32, USB keyboard+storage, the network stack, and the
filesystems arc). Earlier arcs were never packaged and shipped, so there
are deliberately **no** fabricated `0.1`–`0.3` GitHub releases; their
history is the CHANGELOG. The number `0.4.0` (rather than `0.1.0`) reflects
that accumulated maturity and sets the cadence going forward: the next arc
is `0.5.0`.

| Version | Date | Arc |
| --- | --- | --- |
| `0.4.0` | 2026-08-24 | everything to date: boot/shell/FAT32, USB, network, filesystems |
| `0.4.1` | 2026-08-24 | patch — the `fsd` large-read restart |
| `0.5.0` | 2026-08-25 | cluster Phase 0: one verb set, per-task namespaces, multi-mount |
| `0.6.0` | 2026-08-25 | Phase 1 — 9P-over-TCP: export, remote-mount, two nodes |
| `0.7.0` | 2026-08-25 | Phase 2 — two-node read **and write** disk sharing |
| `0.8.0` | 2026-08-25 | Phase 3 — resources as files: `/proc`, `/dev/cons`, `/net` |
| `0.9.0` | 2026-08-26 | Phase 4 — remote execution, the full Plan 9 `cpu` model |
| `0.10.0` | 2026-08-26 | export hardening: shared-cluster-key HMAC auth |
| `0.11.0` | 2026-08-26 | `/net/tcp` dial-out — use another machine's NIC |
| `0.12.0` | 2026-08-26 | dial-in — accept inbound on another machine's NIC |
| `0.13.0` | 2026-08-26 | reply authentication (mutual auth) |
| `0.14.0` | 2026-08-27 | `cpu` chunked output delivery + four more `/bin` filters |
| `0.15.0` | 2026-08-31 | per-**user** cluster identity (wire flag day: `AUTHNP01`→`02`) |
| `0.16.0` | 2026-09-01 | per-**machine** Ed25519 keypairs (wire flag day: `AUTHNP02`→`03`) |
| `0.17.0` | 2026-09-02 | small gaps: `mv` replaces a file (`-f` required, on `cp` too), POSIX classes in `grep` |
| `0.18.0` | 2026-09-03 | correctness: cross-mount `mv` refused (was a silent local rename), `ls` reports the real error, host peer serves `NP_STAT` |
| `0.18.1` | 2026-09-03 | patch — the remote-read flake closed (supervisor no longer restarts `netd` mid-read; delegation race), `ls` exits non-zero on failure |

## Four things that have bitten, and how to avoid them

Learned the hard way, in the order they hurt.

**1. Bump `VERSION`, `kernel/Cargo.toml` and `Cargo.lock` in ONE commit, before
building.** `make image` / `release.sh build` regenerates `Cargo.lock` when the
kernel version changes, so a release commit that omits it goes *dirty mid-build*
and `publish` then refuses ("working tree is dirty"). Run `cargo metadata` after
editing `Cargo.toml` to regenerate the lock, and commit all three together. This
cost a v0.14.0 amend-and-diverge that took a `reset --soft` to untangle.

**2. Ship the `.dmg`, never a zipped `.hdd`.** `prl_disk_tool create --dmg`
writes an `.hdd` *bundle* whose `DiskDescriptor.xml` references the disk data by
**absolute path** to the `.dmg` — the bundle's own data file is 0 bytes. A zipped
`.hdd` is therefore useless on any machine but the one that built it. The `.dmg`
is self-contained; the notes carry the one-line `prl_disk_tool create` recipe for
wrapping it locally.

**3. `publish` must push the branch before it tags.** It did not, for the first
fourteen releases: the tag and the GitHub Release were pushed while the commit
they name stayed local, so the release pointed at something not on `origin/main`
and the repository still advertised the *previous* `VERSION`. It escaped notice
because the next merged PR carried the release commit up afterwards — the state
repaired itself, just never before someone looked. Found at v0.17.0 by checking
`git rev-parse HEAD origin/main` after publishing instead of trusting the "published"
line. `release.sh` now pushes the branch first (and skips that on a detached
HEAD, which the two-arcs recipe below uses deliberately).

**4. Smoke-test the built image before publishing.** The artifacts are what
people download, and `release.sh build` is the last point at which a bad one is
free to fix. Boot `build/esp.img` and check the console reaches a login prompt
with no faults — `python3 scripts/drive-qemu.py --slirp build/esp.img
'login:@@root' 'assword@@root' '# @@uptime'` does it unattended and prints the
abort count.

**When two arcs share one branch** and each needs its own minor tag: ff-merge the
whole branch, then tag *each* version at *its* commit, checking out the earlier
one detached to build its artifacts (its notes file does not exist at that
commit, so `-F` a copy made beforehand). Done for v0.10.0 + v0.11.0.

## What a release contains

Every release is three things:

1. **An annotated git tag** `vX.Y.Z` (message = the release notes).
2. **A GitHub Release** page with human-readable notes derived from the
   CHANGELOG arc(s) since the previous tag.
3. **Bootable, downloadable artifacts**, built from the **release** profile:
   - `ouroboros-X.Y.Z-esp.img.zip` — the raw MBR+FAT32 disk image; boot it
     under QEMU (`qemu-system-aarch64 … -drive`) or any UEFI-AArch64 VM.
   - `ouroboros-X.Y.Z-esp.dmg` — a self-contained, UDZO-compressed disk
     image for Parallels on Apple Silicon. A downloader wraps it into a
     Parallels-native `.hdd` locally with one command (in the release
     notes): `prl_disk_tool create --hdd esp.hdd --dmg <path>/…-esp.dmg`.
   - `SHA256SUMS` for both.

   **Why the `.dmg`, not a `.hdd`:** `prl_disk_tool create --dmg` writes a
   `.hdd` bundle whose `DiskDescriptor.xml` references the disk data by
   *absolute path* to the `.dmg` (the bundle's own data file is empty), so
   a zipped `.hdd` is useless off the machine that built it. The `.dmg`
   embeds the data and is the portable form.

## Cutting a release

Two phases, split on purpose so the outward-facing half is never an
accident of the local half (`scripts/release.sh`):

```sh
# 0. Set the number.
echo 0.5.0 > VERSION
#    keep kernel/Cargo.toml's version in step, then commit both.

# 1. Write the notes. (See the previous tag's file for the shape.)
$EDITOR docs/release-notes/v0.5.0.md

# 2. Build + package the artifacts (local, safe, repeatable).
scripts/release.sh build          # == make release

# 3. Publish (OUTWARD-FACING: tag, push, GitHub Release). Only when you
#    actually mean to ship. Refuses on a dirty tree or an existing tag.
scripts/release.sh publish
```

`make release` is an alias for `scripts/release.sh build`. There is no
`make` alias for `publish` — publishing is deliberately a conscious,
typed-out step.

### Preconditions the script enforces

- `publish` refuses a **dirty working tree** and an **already-existing
  tag**, and warns if you're not on `main`.
- `publish` requires the packaged artifacts (run `build` first) and a
  `docs/release-notes/vX.Y.Z.md` notes file.
- The artifacts are always built `PROFILE=release`, never debug.

### Requirements

The build phase needs the normal toolchain plus `qemu-img`/`hdiutil`/`zip`,
and `parallels-hdd` needs Parallels Desktop's bundled `prl_disk_tool`
(so releases are cut on the macOS dev machine, same box as `test-parallels`).
The publish phase needs `gh` authenticated to the repo.
