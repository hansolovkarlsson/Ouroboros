#!/bin/sh
# The pinned Raspberry Pi 4 UEFI firmware's zip, fetched once into
# CACHE_DIR (default build/cache), checked against its SHA-256 on every use,
# and its path printed on stdout. The one fetch: scripts/sdcard.sh (a card)
# and scripts/bootstick.py (a one-disk Pi stick, PI=1) both run this, so the
# download, the cache's naming and the check cannot drift apart. The pin
# itself is scripts/pftf.env.
#
#   CACHE_DIR=build/cache ./scripts/pftf.sh     prints build/cache/RPi4_UEFI_Firmware_<v>.zip
set -eu

. "$(dirname "$0")/pftf.env"
CACHE_DIR="${CACHE_DIR:-build/cache}"
FW_ZIP="RPi4_UEFI_Firmware_${FW_VERSION}.zip"
FW_URL="https://github.com/pftf/RPi4/releases/download/${FW_VERSION}/${FW_ZIP}"

mkdir -p "$CACHE_DIR"
if [ ! -f "$CACHE_DIR/$FW_ZIP" ]; then
    echo "pftf: fetching $FW_URL" >&2
    curl -fsSL -o "$CACHE_DIR/$FW_ZIP.part" "$FW_URL" || {
        rm -f "$CACHE_DIR/$FW_ZIP.part"
        echo "pftf: could not fetch $FW_URL (offline, or the release is missing?)" >&2; exit 1; }
    mv "$CACHE_DIR/$FW_ZIP.part" "$CACHE_DIR/$FW_ZIP"
fi
GOT=$(shasum -a 256 "$CACHE_DIR/$FW_ZIP" | cut -d ' ' -f 1)
if [ "$GOT" != "$FW_SHA256" ]; then
    echo "pftf: $CACHE_DIR/$FW_ZIP has SHA-256 $GOT, expected $FW_SHA256; delete it and retry, or check the release" >&2
    exit 1
fi
echo "$CACHE_DIR/$FW_ZIP"
