#!/bin/zsh
# Check the PIE relocation contract for every userland binary.
#
# This project's loader applies R_AARCH64_RELATIVE and CANNOT process
# R_AARCH64_ABS64 (see docs/processes.md). A binary carrying an ABS64 is
# unloadable, and the failure is a fault at run time rather than a link error -
# which is why this is checked mechanically.
#
# THE CANARY IS THE POINT. This script was written after `llvm-readelf -r` was
# found to print NOTHING AT ALL for these binaries, so a `grep -c ABS64` against
# it returned 0 and was read as "no ABS64 relocations" for five consecutive
# steps. An empty output is not a zero count. So this uses llvm-readobj (which
# does report them) AND fails if the total across all binaries is zero, because
# that means the tool has stopped seeing relocations rather than that there are
# none.
set -u
cd "$(dirname "$0")/.."

READOBJ="$(find "${HOME}/.rustup" -name llvm-readobj -type f 2>/dev/null | head -1)"
if [ -z "$READOBJ" ]; then
  echo "check-relocs: no llvm-readobj (rustup component add llvm-tools)" >&2
  exit 2
fi

DIR="target/aarch64-unknown-none/release"
if [ ! -d "$DIR" ]; then
  echo "check-relocs: $DIR missing - build the userland first (make esp)" >&2
  exit 2
fi

# THE C PROGRAMS TOO. They link elsewhere - clang + LLD into build/*.elf, not
# cargo into target/ - so for as long as this scanned only $DIR it checked 56
# Rust binaries and ZERO C ones, while reporting a clean contract for "every
# userland binary".
#
# That blind spot sat exactly where the risk is highest. The C link is the one
# that pulls PREBUILT `core` out of a Rust staticlib (nsresolve), whose .rodata
# carries ABS64 - it needs --gc-sections to link at all, and a change that
# dropped that flag would have produced either a link error or, worse, a binary
# this check called clean without looking at it.
CDIR="build"

total_relative=0
bad=0
checked=0
cfiles=()
[ -d "$CDIR" ] && cfiles=("$CDIR"/*.elf(N.))
for f in "$DIR"/*(.) $cfiles; do
  # Only ELF executables, not rlibs or build artefacts.
  head -c 4 "$f" 2>/dev/null | grep -q $'\x7fELF' || continue
  case "$f" in *.bin|*.d) continue;; esac
  out="$("$READOBJ" -r "$f" 2>/dev/null)"
  abs=$(printf '%s' "$out" | grep -c R_AARCH64_ABS64)
  rel=$(printf '%s' "$out" | grep -c R_AARCH64_RELATIVE)
  checked=$((checked + 1))
  total_relative=$((total_relative + rel))
  if [ "$abs" -ne 0 ]; then
    echo "  FAIL $(basename "$f"): $abs R_AARCH64_ABS64 (unloadable)"
    bad=$((bad + 1))
  fi
done

if [ "$checked" -eq 0 ]; then
  echo "check-relocs: no binaries examined - the check proved nothing" >&2
  exit 1
fi
if [ "$total_relative" -eq 0 ]; then
  echo "check-relocs: CANARY FAILED - $checked binaries and not one RELATIVE" >&2
  echo "  the tool is not reporting relocations, so a clean ABS64 result is meaningless" >&2
  exit 1
fi
if [ "$bad" -ne 0 ]; then
  echo "check-relocs: $bad binary(ies) carry ABS64 relocations"
  exit 1
fi
echo "check-relocs: $checked binaries, 0 ABS64, $total_relative RELATIVE (tool confirmed working)"
