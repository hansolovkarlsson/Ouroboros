# Testing exFAT (read-write) yourself

How to build the exFAT test disk, drive the whole read-write surface from the
shell, and confirm the result against a real exFAT driver. This is the
QEMU-based test rig used to bring up `fsd/src/exfat.rs` (the more-filesystems
arc, steps 2–3). For *what* exFAT support does and how it works, see
[`CHANGELOG.md`](CHANGELOG.md)'s "More filesystems, steps 2/3" and the module
doc in `fsd/src/exfat.rs`.

## Why there's a special disk for this

The catch that shapes the whole setup: **UEFI can only boot from a FAT
filesystem**, and `fsd` mounts the *first* partition it can. So to make `fsd`
mount exFAT while the disk still boots, the test disk is a two-partition MBR:

- **partition 1 — exFAT** (`fsd` discovers it first; the FAT32 probe fails, the
  exFAT probe succeeds — the real `Filesystem`-enum fallthrough), carrying
  `/bin` (so the shell runs its commands off exFAT) plus a few test files;
- **partition 2 — the FAT32 ESP** (UEFI ignores the exFAT partition it can't
  read and boots `BOOTAA64` from here).

One block device, no slot-ordering ambiguity. The exFAT filesystem is built with
macOS's `newfs_exfat` (`hdiutil` can't make exFAT) and assembled into the
combined disk by `scripts/mkexfat.py`.

## Prerequisites

- On the `exfat-readonly` branch (it carries the read + write code) — or `main`
  once it's merged there.
- QEMU: `brew install qemu` (also provides the aarch64 OVMF firmware the run
  targets point at).
- macOS's `newfs_exfat`/`diskutil`/`fsck_exfat` — already present on macOS, used
  by the build and the verification step.

## 1. Build and boot

```sh
make run-image-exfat
```

This builds the ESP (kernel + servers + `/bin`), builds and populates the exFAT
partition, assembles `build/espexfat.img`, and boots QEMU on it. When it's up you'll
see:

```
fsd: exFAT mounted, disk commands available
```

Quit QEMU any time with **`Ctrl-A`** then **`X`**.

> **Persistence note.** Your writes are saved into `build/espexfat.img` and survive a
> reboot (that persistence is part of what's being tested). To start from a
> clean, freshly-populated disk, rebuild it first:
>
> ```sh
> make image-exfat        # rebuild build/espexfat.img without booting
> make run-image-exfat    # then boot
> ```

## 2. Drive the read-write surface

At the `$` prompt:

```sh
ls /                              # bin/ SUB/ README.TXT a-long-exfat-name.txt ...
cat /README.TXT
cat /a-long-exfat-name.txt        # a long UTF-16 name (spans several name entries)

# create
touch /NEW.TXT
write /W.TXT hello exfat write
cat /W.TXT

# copy (streaming; second one is a 16 KB, four-cluster binary)
cp /README.TXT /COPY.TXT
cp /bin/CAT /CATCOPY

# append (write_at past EOF)
echo more >> /W.TXT
cat /W.TXT

# directories
mkdir /D
write /D/INSIDE.TXT nested
ls /D
cat /D/INSIDE.TXT

# rename / move
mv /README.TXT /RENAMED.TXT
mv /RENAMED.TXT /D/MOVED.TXT      # cross-directory move
mv /SUB /SUB2                     # move a whole directory (contents intact)

# delete
rm /NEW.TXT
rmdir /D                          # refused until emptied — that's correct
rm /D/MOVED.TXT /D/INSIDE.TXT
rmdir /D

# a pipeline reading from exFAT
cat /COPY.TXT | grep line | wc
```

Things worth confirming as you go:

- `rmdir` on a non-empty directory is refused (`directory not empty`).
- `mv` onto a name that already exists is refused (`already exists`).
- A moved directory keeps its contents (`ls /SUB2` still shows `NESTED.TXT`).
- Everything you created/renamed is still there after quitting and re-running
  `make run-image-exfat` (persistence).

## 3. Verify against a real driver

The convincing check: does macOS's own exFAT driver accept what `fsd` wrote?
After quitting QEMU:

```sh
# attach without mounting, find the exFAT partition
DEV=$(hdiutil attach -nomount build/espexfat.img | awk '/Windows_NTFS|Microsoft/{print $1}')

# integrity check — bitmap + directory hierarchy
fsck_exfat -n "/dev/r$(basename "$DEV")"       # expect: "appears to be OK"

# mount and inspect the files fsd created
diskutil mount "$DEV"
ls -la /Volumes/OUROEXFAT
cat /Volumes/OUROEXFAT/W.TXT
cmp /Volumes/OUROEXFAT/CATCOPY build/esp/bin/CAT && echo "copy is byte-identical"

# clean up
diskutil unmount "$DEV"
hdiutil detach "$DEV"
```

If `fsck_exfat` passes and `cmp` reports identical, the on-disk structures — the
allocation bitmap, the FAT chains, and both the `SetChecksum` and `NameHash`
each entry carries — are all spec-correct.

## What about real Parallels hardware?

Not a one-command path, and not covered here. Storage on Parallels/Apple Silicon
is USB-mass-storage only (no emulated disk controller on the PCI bus — see
[`roadmap.md`](roadmap.md)'s parking lot), so testing exFAT there needs a
physical exFAT-formatted USB stick passed through to the VM. QEMU
(`run-image-exfat`) is the honest test surface for this feature, the same as the
rest of the disk work.

## Where the pieces live

| Piece | File |
|---|---|
| The exFAT driver | `fsd/src/exfat.rs` |
| Filesystem dispatch (FAT32 / exFAT) | `fsd/src/vfs.rs` |
| Combined-disk builder | `scripts/mkexfat.py` |
| Build + run targets | `Makefile` (`build/exfatpart.img`, `image-exfat`, `run-image-exfat`) |
