#!/bin/sh
# The async remote-mount checks (docs/roadmap/roadmap-async-rmount.md, steps 0
# and 1): a guest with SLIRP networking mounts a HOST-run 9P peer
# (scripts/np9p_server.py at 10.0.2.2) and its path-verb remote mounts are
# PARKED on netd's event loop instead of blocking it. Three checks, each graded
# on the lines it names, each with a negative control that was measured:
#
#   1. served     a remote read is answered from the parked slot, and the shell
#                 gets its prompt back. (Fails on any tree where the park does
#                 not deliver: the caller stays blocked in its MSG_CALL forever
#                 and no prompt ever returns.)
#   2. identity   a parked reply whose caller was KILLED, and whose slot a second
#                 caller now holds, reaches nobody: the second caller prints ITS
#                 file. The peer holds every reply for 3.5 s (--delay): the
#                 driver takes ~2.5 s to get from the first `cat` to the second
#                 (two prompts settled, two lines typed), so the first reply
#                 lands while the second caller is parked and before either
#                 deadline (5 s, REMOTE_DEADLINE_TICKS). A 1.5 s delay landed
#                 the reply before the second caller existed, and the check
#                 could not fail (measured on the slot-sending kernel). On the
#                 kernel sending the reply by SLOT (the mutation control), the
#                 second `cat` prints the FIRST file's bytes.
#   3. concurrent a request parked on a peer that never answers (--delay past
#                 the guest's deadline) fails NO_FS within that deadline, and a
#                 second client's read of a live peer is served WHILE the first
#                 is parked: its bytes appear BEFORE the first one's failure.
#                 (On the synchronous tree the event loop is blocked for the
#                 whole wait and the second read cannot be served first.)
#
#   make test-async-rmount        # rebuilds the image first, then boots
#   ./scripts/test-async-rmount.sh   # by hand, against build/esp.img
#
# Three boots, about a minute each. Not part of `make test` (host only,
# seconds); run it when netd's client paths or the kernel's send arm change.
set -u
cd "$(dirname "$0")/.."
IMG=build/esp.img
[ -f "$IMG" ] || { echo "test-async-rmount: $IMG missing - run make image"; exit 2; }
# A stale image would grade the previous netd or kernel.
for k in target/aarch64-unknown-none/release/netd.bin build/esp/EFI/BOOT/BOOTAA64.EFI; do
    [ -f "$k" ] && [ "$k" -nt "$IMG" ] && {
        echo "test-async-rmount: $IMG is older than $k - run make image"; exit 2; }
done
fail=0
n=0
PEER=5641      # the live peer
SLOW=5642      # the peer that holds its replies
pids=""
peer() { # port [--delay S]
    python3 scripts/np9p_server.py "$@" > "build/np9p-$1.log" 2>&1 &
    pids="$pids $!"
}
stop_peers() { for p in $pids; do kill "$p" 2>/dev/null; done; pids=""; }
trap stop_peers EXIT
# One boot with SLIRP. A boot whose transcript never shows the firmware's own
# "BdsDxe: starting" line is retried (the hang and its rate are in
# docs/testing/testing-qemu.md section 1b); nothing of ours has run by then.
boot() {
    tries=0
    while :; do
        tries=$((tries + 1))
        out=$(python3 scripts/drive-qemu.py "$IMG" --slirp 'login:@@root' 'assword@@root' "$@" 2>&1)
        case $out in *"BdsDxe: starting"*) break ;; esac
        [ $tries -lt 3 ] || break
        echo "     (firmware never reached its boot entry, retrying)" >&2
    done
}
after() { awk -v m="# $1" 'p || $0 == m { p = 1; print }'; }
grade() { # name, transcript, marker, must-regex, must-not-regex
    name=$1; out=$2; marker=$3; want=$4; forbid=$5
    n=$((n + 1)); log=build/async-$n.txt
    printf '%s\n' "$out" | tr -d '\r' > "$log"
    tail=$(after "$marker" < "$log")
    if printf '%s\n' "$tail" | grep -qE -- "$want" && ! printf '%s\n' "$tail" | grep -qE -- "$forbid" \
        && grep -q -- "--- qemu -d int: 0 fault lines" "$log"; then
        echo "ok   $name ($log)"
    else
        echo "FAIL $name ($log)"
        grep -n 'TIMEOUT\|FAULT\|restarted' "$log" | head -4
        printf '%s\n' "$tail" | head -n 14 | sed 's/^/    | /'
        fail=1
    fi
}

# 1. served
peer $PEER; sleep 3
boot '# @@mount -r 10.0.2.2:5641 /mnt/h' '# @@cat /mnt/h/SUB/NOTE.TXT' '# @@uptime' '# @@'
grade "a path verb is served from a parked slot" "$out" 'cat /mnt/h/SUB/NOTE.TXT' \
    'a nested file, read remotely' 'out of room|no filesystem|not running'
printf '%s\n' "$out" | tr -d '\r' | after 'uptime' | grep -q 'ticks since boot' || { echo "FAIL the shell never got its prompt back after the read"; fail=1; }
stop_peers

# 2. identity. Task 6 is the first spawnable slot: the killed cat's, and then
# the second cat's, which the transcript confirms.
peer $PEER --delay 3.5; sleep 3
boot '# @@mount -r 10.0.2.2:5641 /mnt/h' '# @@exec /bin/cat /mnt/h/HELLO.TXT' '# @@kill 6' \
     '# @@cat /mnt/h/SUB/NOTE.TXT' '# @@'
grade "a parked reply outlives its caller and reaches nobody else" "$out" 'cat /mnt/h/SUB/NOTE.TXT' \
    'a nested file, read remotely' 'hello from the host|not running'
stop_peers

# 3. concurrent
peer $PEER; peer $SLOW --delay 30; sleep 3
boot '# @@mount -r 10.0.2.2:5641 /mnt/h' '# @@mount -r 10.0.2.2:5642 /mnt/s' \
     '# @@exec /bin/cat /mnt/s/HELLO.TXT' '# @@cat /mnt/h/SUB/NOTE.TXT' '# @@uptime' '# @@'
n=$((n + 1)); log=build/async-$n.txt
printf '%s\n' "$out" | tr -d '\r' > "$log"
tail=$(after 'cat /mnt/h/SUB/NOTE.TXT' < "$log")
served=$(printf '%s\n' "$tail" | grep -n 'a nested file, read remotely' | head -1 | cut -d: -f1)
failed=$(printf '%s\n' "$tail" | grep -n 'no filesystem mounted' | head -1 | cut -d: -f1)
if [ -n "$served" ] && [ -n "$failed" ] && [ "$served" -lt "$failed" ] \
    && grep -q -- "--- qemu -d int: 0 fault lines" "$log"; then
    echo "ok   a live peer is served while a silent one is parked, which then fails NO_FS ($log)"
else
    echo "FAIL a live peer is served while a silent one is parked ($log): served at line ${served:-none}, NO_FS at line ${failed:-none}"
    printf '%s\n' "$tail" | head -n 14 | sed 's/^/    | /'
    fail=1
fi
stop_peers

# 4. connection refused. A mount to the AUTHORIZED host on a DEAD port: the
# request parks, the host RSTs the SYN, and the parked caller must be answered
# NO_FS with the prompt coming back - not left blocked in its MSG_CALL. This
# exercises the park -> dead-slot -> NO_FS reply-to-identity path end to end.
# It does NOT reproduce the pump-vs-service reap race the review of #149 found
# (that needs a peer answering ARP but DROPPING the SYN, so Connecting exhausts
# inside pump_dials; SLIRP RSTs a dead port instead, and an in-subnet down IP
# fails ARP synchronously). That race is fixed by the `parked.is_none()` guard
# in pump_dials (a parked slot is service_remotes's to free), correct by
# construction; a SYN-drop control needs a live node on an L2 link, noted in
# docs/roadmap/roadmap-async-rmount.md. 10.0.2.2 is authorized (the dev host
# peer), so the request reaches the park rather than failing auth first.
boot '# @@mount -r 10.0.2.2:5599 /mnt/d' '# @@cat /mnt/d/X' '# @@uptime' '# @@'
grade "a refused connection fails NO_FS, the caller is not hung" "$out" 'cat /mnt/d/X' \
    'no filesystem mounted this boot' 'out of room'
printf '%s\n' "$out" | tr -d '\r' | after 'uptime' | grep -q 'ticks since boot' || { echo "FAIL the shell never got its prompt back: the parked caller hung"; fail=1; }

exit $fail
