#!/usr/bin/env bash
#
# Boot a guest, run a HOST-SIDE client against it, and kill the guest after.
#
# WHY THIS EXISTS, AND WHY THE LIVENESS LINE IS LOAD-BEARING.
#
# `drive-qemu.py` drives the guest SHELL: it types its steps, lingers a few
# seconds and then kills QEMU. That is right for a shell test and wrong for a
# host-side client that runs for minutes, because the guest dies mid-run and
# every socket error the client then sees reads as a REFUSAL BY THE GUEST. The
# session gate was measured that way on 2026-09-07: its late checks (the idle
# reap, the slots coming back) were passing and failing against a guest that had
# already been killed, and the "3/3 reaped" line - the strongest evidence in the
# run - was a dead process. Nothing in the transcript said so.
#
# So: this script boots the guest, waits for the export to announce itself, runs
# the command, and then ASSERTS THE GUEST IS STILL ALIVE before believing any of
# it - exiting 99 if not, because a run whose subject died proves nothing in
# either direction. That check is the whole point of the script; the rest is
# `make run-image-9p`'s command line.
#
# Usage:
#   scripts/run-guest.sh [--hostfwd=tcp::5640-:564 ...] -- <command> [args...]
# e.g.
#   scripts/run-guest.sh -- python3 scripts/np9p_client.py localhost 5640 session-gate 5
#   scripts/run-guest.sh --hostfwd=tcp::5555-:80 -- curl -s http://localhost:5555/ -o /tmp/x
set -u
cd "$(dirname "$0")/.."
OVMF="${OVMF:-$(brew --prefix qemu)/share/qemu/edk2-aarch64-code.fd}"
IMAGE="${IMAGE:-build/esp.img}"
# BUILD FIRST, as every `make run-*` target does. This script boots an image by
# PATH, so without a build a run silently measures whatever was last staged,
# which is how a "verified" claim becomes worthless. NO_BUILD=1 skips it, for
# the mutation runs that build a deliberately broken image by hand.
if [ "${NO_BUILD:-0}" != "1" ] && [ "$IMAGE" = "build/esp.img" ]; then
	make image >/dev/null || { echo "run-guest: make image failed" >&2; exit 96; }
fi
LOGDIR="${LOGDIR:-${TMPDIR:-/tmp}}"
# PER PROCESS: two runs share $LOGDIR (the docs show a gate run and a curl run),
# and fixed names let the second truncate the first's serial log while the first
# is still grepping it for readiness and for restart lines - so one run's boot
# output could satisfy the other's wait and `report_guest` score the wrong guest.
SERIAL="$LOGDIR/guest-serial.$$.log"
TRACE="$LOGDIR/guest-int.$$.log"
BOOT_WAIT="${BOOT_WAIT:-60}"

# What the guest says about its own health, and this script's verdict on it.
# The counts are not decoration: a run whose server was restarted, or whose
# guest faulted, proves nothing about the client's result - so they DECIDE the
# exit code rather than being printed beside it.
report_guest() {
	RESTARTS=$(grep -cE 'wedged|restarted|not restarting|giving up' "$SERIAL" 2>/dev/null)
	ABORTS=$(grep -cE 'Abort|SError' "$TRACE" 2>/dev/null)
	echo "run-guest: ${RESTARTS:-0} restart line(s), ${ABORTS:-0} abort line(s)"
	if [ "${RESTARTS:-0}" != "0" ] || [ "${ABORTS:-0}" != "0" ]; then
		echo "run-guest: the guest did not stay healthy - the result above is not evidence" >&2
		return 1
	fi
	return 0
}

FWD=()
while [ $# -gt 0 ]; do
	case "$1" in
		--hostfwd=*) FWD+=("${1#--hostfwd=}"); shift ;;
		--) shift; break ;;
		*) break ;;
	esac
done
[ $# -gt 0 ] || { echo "usage: $0 [--hostfwd=SPEC ...] -- <command> [args...]" >&2; exit 2; }
[ ${#FWD[@]} -gt 0 ] || FWD=("tcp::5640-:564")
NETDEV="user,id=net0"
for f in "${FWD[@]}"; do NETDEV="$NETDEV,hostfwd=$f"; done

qemu-system-aarch64 -machine virt -cpu cortex-a72 -m 512M -bios "$OVMF" \
	-drive "file=$IMAGE,format=raw,if=none,id=hd0" \
	-device virtio-blk-device,drive=hd0 -device virtio-rng-device \
	-netdev "$NETDEV" -device virtio-net-device,netdev=net0 \
	-global virtio-mmio.force-legacy=false -nographic \
	-d int -D "$TRACE" > "$SERIAL" 2>&1 &
QPID=$!
trap 'kill $QPID 2>/dev/null' EXIT

# The export announcing itself is the guest saying it is ready to be asked -
# a fixed sleep would sometimes start the client before netd was listening.
for _ in $(seq "$BOOT_WAIT"); do
	grep -q 'export open' "$SERIAL" 2>/dev/null && break
	kill -0 $QPID 2>/dev/null || break
	sleep 1
done
if ! grep -q 'export open' "$SERIAL" 2>/dev/null; then
	# Report what the guest DID say before leaving. This is the one path where
	# the guest is KNOWN to have failed, and it used to exit before printing the
	# restart and abort counts that say why. 97, not 2: the client's own exit
	# codes start at 1, and 2 is its usage error.
	echo "run-guest: the guest never opened its export (see $SERIAL)" >&2
	report_guest
	exit 97
fi

# BOUNDED. The recipe this replaces wrapped the client in `perl -e 'alarm 200'`;
# a bare "$@" lets a client that hangs - the gate blocked on a session netd
# never answers, which is one of the failure modes under test - hang the whole
# run with QEMU alive. The reap escalation can legitimately sleep ~235 s, so the
# bound is generous enough to tell slow from wedged. macOS has no `timeout`.
perl -e 'alarm shift; exec @ARGV' "${RUN_TIMEOUT:-600}" "$@"
RC=$?
if [ $RC -eq 142 ] || [ $RC -eq 14 ]; then
	echo "run-guest: the client hit the ${RUN_TIMEOUT:-600}s bound - wedged, not slow" >&2
fi

if kill -0 $QPID 2>/dev/null; then
	echo "run-guest: guest alive at the end - the run means what it says"
else
	echo "run-guest: THE GUEST DIED DURING THE RUN - every refusal above is suspect" >&2
	RC=99
fi
kill $QPID 2>/dev/null; wait $QPID 2>/dev/null
# Three verdicts, three codes a caller can tell apart: 97 the guest never came
# up, 98 it came up and did not stay healthy, 99 it died mid-run. Anything else
# is the client's own exit status, passed through untouched.
if ! report_guest && [ $RC -eq 0 ]; then
	RC=98
fi
exit $RC
