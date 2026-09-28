#!/bin/sh
# Stage the Raspberry Pi 4 / Pi 400 boot card: the pftf UEFI firmware at the
# root of a FAT volume, and Ouroboros's ESP tree (build/esp) on top of it.
# docs/testing/testing-pi4.md section 4 is the guide; this is its procedure as
# one command.
#
# Usage (normally via `make sdcard`, which stages build/esp first):
#   SDCARD=/Volumes/OUROBOROS ./scripts/sdcard.sh
#   SDCARD=/Volumes/OUROBOROS KEEP_ETC=1 ./scripts/sdcard.sh   # keep the card's /etc
#   SDCARD=/Volumes/OUROBOROS FIRMWARE=1 ./scripts/sdcard.sh   # reinstall the firmware
#   SDCARD=/Volumes/OUROBOROS EJECT=1 ./scripts/sdcard.sh      # eject when done
#
# It NEVER formats. Erasing a disk takes a /dev/diskN, and a wrong N erases
# some other disk; that step stays by hand (testing-pi4.md section 4). What
# this writes to must already be a mounted FAT volume on removable or external
# media, and must be empty or already a Pi card (RPI_EFI.fd at its root).
#
# The firmware is installed only when the card has none (or FIRMWARE=1). The
# pftf firmware has no NVRAM: its settings (the 3 GB limit, ACPI/devicetree
# mode, testing-pi4.md section 5) and every UEFI variable, the boot counter's
# included, are stored INSIDE RPI_EFI.fd on the card (edk2-platforms'
# Platform/RaspberryPi/RPi4/Readme.md, "NVRAM"; the pftf zip's own readme does
# not say). Overwriting it on every update would silently reset them. A
# reinstall also keeps an existing config.txt, since that is where Risk 3's
# UART overlay is set by hand.
#
# What Ouroboros owns on the card is computed from build/esp, not listed here:
# every top-level entry except EFI, plus every entry under EFI. Those are
# replaced whole, so a program dropped from the tree also leaves the card.
# Two are state, not build output:
#   etc    re-staged by default (the dev passwd, shadow, group and cluster
#          files, like every other image target), which resets accounts and
#          passwords made on the Pi. KEEP_ETC=1 keeps the card's files and only
#          adds the ones a newer tree introduced.
#   Users  the home directories: only the ones missing are created, never
#          replaced, since they hold what was made on the Pi.
# Anything else on the card (the firmware's files, a user's own) is untouched.
set -eu

# The pinned firmware release. Bump both together; the SHA-256 is the one
# GitHub reports for the release asset (`gh release view --repo pftf/RPi4`).
FW_VERSION=v1.53
FW_SHA256=ca9973e2a7a546b3df871cfb7382e656829114b6dfa424f40dc67cc90a217d88
FW_ZIP="RPi4_UEFI_Firmware_${FW_VERSION}.zip"
FW_URL="https://github.com/pftf/RPi4/releases/download/${FW_VERSION}/${FW_ZIP}"

ESP_DIR="${ESP_DIR:-build/esp}"
CACHE_DIR="${CACHE_DIR:-build/cache}"
SDCARD="${SDCARD:-}"
KEEP_ETC="${KEEP_ETC:-0}"
FIRMWARE="${FIRMWARE:-0}"
EJECT="${EJECT:-0}"

die() { echo "sdcard: $*" >&2; exit 1; }

# --- The card: refuse anything that does not look like one. -----------------

[ -n "$SDCARD" ] || die "set SDCARD to the card's mounted volume, e.g. make sdcard SDCARD=/Volumes/OUROBOROS"
[ -d "$SDCARD" ] || die "$SDCARD is not a directory (is the card mounted?)"
CARD=$(cd "$SDCARD" && pwd -P)
case "$CARD" in
    /Volumes/*/*|/Volumes/) die "$CARD is not a volume's mount point (expected /Volumes/<name>)" ;;
    /Volumes/*) ;;
    *) die "$CARD is not under /Volumes; refusing" ;;
esac

INFO=$(diskutil info "$CARD" 2>/dev/null) || die "diskutil knows no volume at $CARD"
field() { printf '%s\n' "$INFO" | sed -n "s/^ *$1: *//p" | head -n 1; }
MOUNT=$(field 'Mount Point')
[ "$MOUNT" = "$CARD" ] || die "$CARD is not a mount point (diskutil reports '$MOUNT')"
FS=$(field 'File System Personality')
case "$FS" in
    MS-DOS*) ;;
    *) die "$CARD is '$FS', not FAT; the Pi's boot ROM reads FAT only" ;;
esac
REMOVABLE=$(field 'Removable Media')
LOCATION=$(field 'Device Location')
[ "$REMOVABLE" = "Removable" ] || [ "$LOCATION" = "External" ] || \
    die "$CARD is on fixed internal media (Removable Media: $REMOVABLE, Device Location: $LOCATION); refusing"

# Empty, or already a Pi card. The names macOS itself leaves on a fresh FAT
# volume do not count as contents.
if [ ! -e "$CARD/RPI_EFI.fd" ]; then
    OTHER=$(ls -A "$CARD" | grep -v -x -E '\._.*|\.DS_Store|\.Spotlight-V100|\.fseventsd|\.Trashes|\.TemporaryItems|\.VolumeIcon\.icns|\.metadata_never_index|\.apdisk|System Volume Information' || true)
    [ -z "$OTHER" ] || die "$CARD is neither empty nor a Pi card (no RPI_EFI.fd), and holds: $(echo $OTHER); refusing"
fi

[ -f "$ESP_DIR/EFI/ORBS/INIT.CFG" ] || die "$ESP_DIR is not a staged Ouroboros ESP tree (run make esp)"

# --- Firmware: pinned, checksummed, cached, installed once. -----------------

if [ ! -e "$CARD/RPI_EFI.fd" ] || [ "$FIRMWARE" = 1 ]; then
    if [ -e "$CARD/RPI_EFI.fd" ]; then
        echo "sdcard: FIRMWARE=1: reinstalling; this resets the firmware's settings and UEFI variables"
    fi
    mkdir -p "$CACHE_DIR"
    if [ ! -f "$CACHE_DIR/$FW_ZIP" ]; then
        echo "sdcard: fetching $FW_URL"
        curl -fsSL -o "$CACHE_DIR/$FW_ZIP.part" "$FW_URL"
        mv "$CACHE_DIR/$FW_ZIP.part" "$CACHE_DIR/$FW_ZIP"
    fi
    GOT=$(shasum -a 256 "$CACHE_DIR/$FW_ZIP" | cut -d ' ' -f 1)
    [ "$GOT" = "$FW_SHA256" ] || die "$CACHE_DIR/$FW_ZIP has SHA-256 $GOT, expected $FW_SHA256; delete it and retry, or check the release"
    # Names exactly as shipped: the pftf readme says renaming them breaks boot.
    if [ -e "$CARD/config.txt" ]; then
        unzip -o -q "$CACHE_DIR/$FW_ZIP" -x config.txt -d "$CARD"
        echo "sdcard: config.txt kept (edit or delete it by hand to take the shipped one)"
    else
        unzip -o -q "$CACHE_DIR/$FW_ZIP" -d "$CARD"
    fi
    rm -f "$CARD/Readme.md"
    FW_NOTE="installed pftf $FW_VERSION"
else
    FW_NOTE="kept (RPI_EFI.fd present; FIRMWARE=1 reinstalls pftf $FW_VERSION)"
fi

# --- Ouroboros on top. ------------------------------------------------------

# One owned entry: $1 is its path relative to the ESP root.
place() {
    [ -n "$1" ] || die "internal: empty entry name"
    case "$1" in
        etc)
            if [ "$KEEP_ETC" = 1 ] && [ -e "$CARD/etc" ]; then
                # Only files the card lacks are added. Not `cp -n`: macOS's
                # exits 1 when it skips one, which `set -e` turns into a
                # silent abort halfway through the card.
                (cd "$ESP_DIR" && find etc -type f) | while IFS= read -r f; do
                    if [ ! -e "$CARD/$f" ]; then
                        mkdir -p "$CARD/${f%/*}"
                        cp -X "$ESP_DIR/$f" "$CARD/$f"
                    fi
                done
                echo "sdcard: etc kept (KEEP_ETC=1), missing files added"
                return
            fi
            [ -e "$CARD/etc" ] && echo "sdcard: etc re-staged: accounts and passwords made on the card are reset (KEEP_ETC=1 keeps them)" ;;
        Users)
            for home in "$ESP_DIR/Users"/*; do
                [ -e "$home" ] || continue
                if [ ! -e "$CARD/Users/${home##*/}" ]; then
                    mkdir -p "$CARD/Users"
                    cp -R -X "$home" "$CARD/Users/"
                fi
            done
            return ;;
    esac
    # Copy first, then swap, so a failed copy (a full card, a pulled reader)
    # leaves the old entry, EFI/BOOT's loader included, in place.
    rm -rf "${CARD:?}/$1.sdnew"
    cp -R -X "$ESP_DIR/$1" "$CARD/$1.sdnew"
    rm -rf "${CARD:?}/$1"
    mv "$CARD/$1.sdnew" "$CARD/$1"
}

mkdir -p "$CARD/EFI"
for entry in "$ESP_DIR"/* "$ESP_DIR"/EFI/*; do
    rel=${entry#"$ESP_DIR"/}
    [ "$rel" = EFI ] && continue
    place "$rel"
done

# FAT holds no xattrs, so macOS spills them into ._* sidecars; -X above avoids
# most, this catches the rest. The directories are macOS's own bookkeeping,
# which fsd would list at the root. The same set `make image` strips, but
# best-effort here: `make image` works on an image attached -nobrowse, and
# this is a live card that Spotlight may be indexing, which makes macOS refuse
# to remove .Spotlight-V100 ("Operation not permitted"). Leftovers are only
# clutter in `ls /` on the Pi, never a reason to stop before sync and eject.
find "$CARD" \( -name '._*' -o -name '.DS_Store' \) -delete 2>/dev/null || true
for d in .fseventsd .Trashes .Spotlight-V100; do
    rm -rf "${CARD:?}/$d" 2>/dev/null || echo "sdcard: $d left on the card (macOS holds it while mounted; harmless)"
done
sync

echo "sdcard: $CARD staged from $ESP_DIR"
echo "sdcard: firmware $FW_NOTE"
if [ "$EJECT" = 1 ]; then
    diskutil eject "$CARD"
else
    echo "sdcard: not ejected; run diskutil eject $CARD (or pass EJECT=1)"
fi
