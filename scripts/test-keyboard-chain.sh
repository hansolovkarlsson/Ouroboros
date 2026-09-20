#!/bin/sh
# The keyboard-chain check: the recipes from docs/testing/testing-qemu.md
# section 1b ("The nested shell keeps the keyboard"), run against
# build/esp.img and graded on the lines they name. Each one is a check that
# can fail: its negative control was measured on the kernel before the fix it
# guards (the boot shell answering, or no prompt ever coming back), and the
# grep below is what distinguishes the two.
#
#   make test-keyboard-chain          # rebuilds the image first, then boots
#   PROFILE=release ./scripts/test-keyboard-chain.sh   # by hand, naming the profile the image was staged from
#
# Four boots, about a minute each. Not part of `make test` (host-only, seconds)
# for that reason; run it when tasks.rs's keyboard ownership changes.
#
# This script only boots. The firmware hang it retries on is described
# once, in docs/testing/testing-qemu.md section 1b.
set -u
cd "$(dirname "$0")/.."
IMG=build/esp.img
[ -f "$IMG" ] || { echo "test-keyboard-chain: $IMG missing - run make image"; exit 2; }
# A stale image would grade the previous kernel and print ok for a regression
# its own controls would catch; `make test-keyboard-chain` depends on `image`,
# and this refuses anyway if the image predates the kernel binary of the
# profile it was staged from (PROFILE, as the Makefile spells it; debug by
# default) or the staged copy in the ESP tree.
for k in "target/aarch64-unknown-uefi/${PROFILE:-debug}/BOOTAA64.efi" build/esp/EFI/BOOT/BOOTAA64.EFI; do
    [ -f "$k" ] && [ "$k" -nt "$IMG" ] && {
        echo "test-keyboard-chain: $IMG is older than $k - run make image"; exit 2; }
done
CTRLC=$(printf '\003')
fail=0

# One boot. A boot whose transcript never shows the firmware's own
# "BdsDxe: starting" line is retried, up to three times, and says so (the
# hang and its measured rate are in the guide, section 1b); nothing of ours
# has run by then. A boot that reaches that line is never retried: from
# there on a hang is the kernel's.
boot() {
    tries=0
    while :; do
        tries=$((tries + 1))
        out=$(python3 scripts/drive-qemu.py "$IMG" 'login:@@root' 'assword@@root' "$@" 2>&1)
        case $out in *"BdsDxe: starting"*) break ;; esac
        [ $tries -lt 3 ] || break
        echo "     (firmware never reached its boot entry, retrying)" >&2
    done
}

# The transcript is CRLF; strip the CR before grepping. Every expectation is
# checked on the lines AFTER the marker command, so a prompt or an answer
# from earlier in the run cannot satisfy it.
# A string comparison, not a regex, so a marker may contain / . * or [.
after() { awk -v m="# $1" 'p || $0 == m { p = 1; print }'; }
# Every transcript is kept (build/chain-N.txt), and a failure prints where
# the driver gave up and the transcript's tail, so a firmware hang or a
# changed prompt is distinguishable from the kernel misrouting the keyboard.
n=0
grade() { # name, transcript, marker, expected-regex...
    name=$1; out=$2; marker=$3; shift 3
    n=$((n + 1)); log=build/chain-$n.txt
    printf '%s\n' "$out" | tr -d '\r' > "$log"
    for want in "$@"; do
        if ! after "$marker" < "$log" | grep -qE -- "$want"; then
            echo "FAIL $name: after '$marker' expected /$want/ ($log)"
            grep -n 'TIMEOUT' "$log" | head -3
            tail -n 12 "$log" | sed 's/^/    | /'
            fail=1; return
        fi
    done
    echo "ok   $name ($log)"
}

# 1. The nested shell keeps the keyboard across the commands it runs.
boot '# @@exec /EFI/ORBS/SH.BIN' 'login:@@fg 6' '@@root' 'assword@@root' \
  '# @@cd /EFI' '# @@echo hi' '# @@pwd' '# @@ps' '# @@'
grade "nested shell keeps the keyboard" "$out" "pwd" '^/EFI$' '^task 6: runnable' '^task 0: blocked'

# 2. Three deep, the middle link killed from below: the keyboard skips it.
boot '# @@/EFI/ORBS/SH.BIN' 'login:@@root' 'assword@@root' '# @@cd /EFI' \
  '# @@/EFI/ORBS/SH.BIN' 'login:@@root' 'assword@@root' \
  '# @@exec /EFI/ORBS/SH.BIN' 'login:@@fg 8' '@@root' 'assword@@root' \
  '# @@kill 7' "# @@$CTRLC" '# @@pwd' '# @@ps' '# @@'
grade "dead link spliced out" "$out" "kill 7" '^/EFI$' '^task 6: runnable' '^task 0: blocked'

# 3. fg back to a task in the chain pops it: the boot shell gets it back last.
boot '# @@exec /EFI/ORBS/SH.BIN' 'login:@@fg 6' '@@root' 'assword@@root' \
  '# @@cd /EFI' '# @@exec /EFI/ORBS/SH.BIN' 'login:@@fg 7' '@@root' 'assword@@root' \
  '# @@fg 6' '# @@kill 7' "# @@$CTRLC" '# @@pwd' '# @@'
grade "fg into the chain pops" "$out" "kill 7" '^/$'

# 4. Four deep, fg back one level then a kill and a Ctrl+C: the live shell
#    two levels up must answer, not the boot shell blocked in its wait.
boot '# @@/EFI/ORBS/SH.BIN' 'login:@@root' 'assword@@root' '# @@cd /EFI' \
  '# @@/EFI/ORBS/SH.BIN' 'login:@@root' 'assword@@root' \
  '# @@exec /EFI/ORBS/SH.BIN' 'login:@@fg 8' '@@root' 'assword@@root' \
  '# @@fg 7' '# @@kill 8' "# @@$CTRLC" '# @@pwd' '# @@ps' '# @@'
grade "pop then kill, the live shell answers" "$out" "kill 8" '^/EFI$' '^task 6: runnable' '^task 0: blocked'

exit $fail
