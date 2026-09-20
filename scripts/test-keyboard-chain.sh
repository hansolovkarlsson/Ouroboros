#!/bin/sh
# The keyboard-chain check: the recipes from docs/testing/testing-qemu.md
# section 1b ("The nested shell keeps the keyboard"), run against
# build/esp.img and graded on the lines they name. Each one is a check that
# can fail: its negative control was measured on the kernel before the fix it
# guards (the boot shell answering, or no prompt ever coming back), and the
# grep below is what distinguishes the two.
#
#   make test-keyboard-chain          # builds nothing: run `make image` first
#
# Four boots, about a minute each. Not part of `make test` (host-only, seconds)
# for that reason; run it when tasks.rs's keyboard ownership changes.
#
# Boot after `make image` in the SAME command has hung in the firmware
# (docs/testing/testing-qemu.md, section 1b); this script only boots.
set -u
cd "$(dirname "$0")/.."
IMG=build/esp.img
[ -f "$IMG" ] || { echo "test-keyboard-chain: $IMG missing - run make image"; exit 2; }
CTRLC=$(printf '\003')
DRIVE="python3 scripts/drive-qemu.py $IMG login:@@root assword@@root"
fail=0

# The transcript is CRLF; strip the CR before grepping. Every expectation is
# checked on the lines AFTER the marker command, so a prompt or an answer
# from earlier in the run cannot satisfy it.
after() { tr -d '\r' | sed -n "/^# $1\$/,\$p"; }
grade() { # name, transcript, marker, expected-regex...
    name=$1; out=$2; marker=$3; shift 3
    for want in "$@"; do
        if ! printf '%s\n' "$out" | after "$marker" | grep -qE -- "$want"; then
            echo "FAIL $name: after '$marker' expected /$want/"; fail=1; return
        fi
    done
    echo "ok   $name"
}

# 1. The nested shell keeps the keyboard across the commands it runs.
out=$($DRIVE '# @@exec /EFI/ORBS/SH.BIN' 'login:@@fg 6' '@@root' 'assword@@root' \
  '# @@cd /EFI' '# @@echo hi' '# @@pwd' '# @@ps' '# @@' 2>&1)
grade "nested shell keeps the keyboard" "$out" "pwd" '^/EFI$' '^task 6: runnable' '^task 0: blocked'

# 2. Three deep, the middle link killed from below: the keyboard skips it.
out=$($DRIVE '# @@/EFI/ORBS/SH.BIN' 'login:@@root' 'assword@@root' '# @@cd /EFI' \
  '# @@/EFI/ORBS/SH.BIN' 'login:@@root' 'assword@@root' \
  '# @@exec /EFI/ORBS/SH.BIN' 'login:@@fg 8' '@@root' 'assword@@root' \
  '# @@kill 7' "# @@$CTRLC" '# @@pwd' '# @@ps' '# @@' 2>&1)
grade "dead link spliced out" "$out" "kill 7" '^/EFI$' '^task 6: runnable' '^task 0: blocked'

# 3. fg back to a task in the chain pops it: the boot shell gets it back last.
out=$($DRIVE '# @@exec /EFI/ORBS/SH.BIN' 'login:@@fg 6' '@@root' 'assword@@root' \
  '# @@cd /EFI' '# @@exec /EFI/ORBS/SH.BIN' 'login:@@fg 7' '@@root' 'assword@@root' \
  '# @@fg 6' '# @@kill 7' "# @@$CTRLC" '# @@pwd' '# @@' 2>&1)
grade "fg into the chain pops" "$out" "kill 7" '^/$'

# 4. Four deep, fg back one level then a kill and a Ctrl+C: the live shell
#    two levels up must answer, not the boot shell blocked in its wait.
out=$($DRIVE '# @@/EFI/ORBS/SH.BIN' 'login:@@root' 'assword@@root' '# @@cd /EFI' \
  '# @@/EFI/ORBS/SH.BIN' 'login:@@root' 'assword@@root' \
  '# @@exec /EFI/ORBS/SH.BIN' 'login:@@fg 8' '@@root' 'assword@@root' \
  '# @@fg 7' '# @@kill 8' "# @@$CTRLC" '# @@pwd' '# @@ps' '# @@' 2>&1)
grade "pop then kill, the live shell answers" "$out" "kill 8" '^/EFI$' '^task 6: runnable' '^task 0: blocked'

exit $fail
