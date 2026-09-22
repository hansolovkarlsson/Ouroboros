#!/bin/sh
# The concurrent-client check: a request from a LOCAL client arriving at netd
# while netd is carrying a `cpu` run. Run on the two-node ext2 rig
# (docs/testing/testing-qemu.md section 5), graded on the lines it names.
#
# HOW THE CONDITION IS MADE. The shell spawns a pipeline's program stages
# before it runs a builtin source, so `cpu <A> ping <unreachable> | <client>`
# has <client>'s request reach netd while netd is carrying the run. The
# unreachable ping's ARP wait holds the run open.
#
# WHAT IT EXPECTS, AND WHY THIS FILE INVERTED. Until step 3 of
# docs/roadmap/roadmap-async-rmount.md a run BLOCKED netd's event loop, so
# every identified client's request was REFUSED `FS_ERR_BUSY` from `tcp_run`'s
# re-entrant drain - refusing was the honest answer, because every handler a
# client would reach was too deep for that stack and overflowed the guard page.
# Step 3 parks the run on the event-loop engine, so nothing blocks and there is
# no re-entrant drain left. Every recipe here is now SERVED, and the refusal
# text is what the check FORBIDS.
#
# WHAT THIS CHECK CAN AND CANNOT TELL APART, stated plainly because the
# inversion cost it something. Before step 3, "refused" and "succeeded" were
# different strings, so the harness could see whether the condition had been
# created and retry when the run had returned first. Now a client served DURING
# the run and a client served AFTER the run returned produce the SAME output,
# and nothing in the transcript separates them. What the check still does is
# fail: on the pre-step-3 tree every recipe here produces the refusal line (the
# measured control - `git stash` this change, rebuild the images, and all three
# report "out of room"/"network server busy"), and on any tree that faults netd
# it reports the fault. It is a regression check against the refusal and the
# overflow, not a proof of overlap; the async rig's ordering checks
# (scripts/test-async-rmount.sh, 3 and 6) are where overlap is proven.
#
# The negative control for the OVERFLOW, measured 2026-09-20 before the refusal
# existed: an EL0 data abort in netd at the guard page (esr_el1=0x9200004f),
# "server slot 4 restarted", and the client told its server had died
# mid-request - for `cat`, for `resolve`, and (deeper) for `cbig`.
#
#   make test-reentrant-session       # builds both node images first
#   ./scripts/test-reentrant-session.sh   # by hand, against the images in build/
#
# A few minutes. Not part of `make test` (host only, seconds); run it when
# netd's client paths or its stack use change.
set -u
cd "$(dirname "$0")/.."
A=build/espext2-a.img
B=build/espext2-b.img
for img in "$A" "$B"; do
    [ -f "$img" ] || { echo "test-reentrant-session: $img missing - run make images-2vm-ext2"; exit 2; }
done
# A stale image grades the previous netd; refuse if either image predates the
# server binary.
[ -f target/aarch64-unknown-none/release/netd.bin ] && \
    [ "target/aarch64-unknown-none/release/netd.bin" -nt "$B" ] && {
        echo "test-reentrant-session: $B is older than netd.bin - run make images-2vm-ext2"; exit 2; }
fail=0
n=0

# One two-node run. A boot whose transcript never shows the firmware's own
# "BdsDxe: starting" line for BOTH nodes is retried (the hang and its rate are
# in the guide, section 1b); nothing of ours has run by then.
run() { # B step (the piped command), after the mount
    tries=0
    while :; do
        tries=$((tries + 1))
        out=$(python3 scripts/drive-2vm.py "$A" "$B" \
            --a 'login:@@root' 'assword:@@root' '# @@ls /man' \
            --b 'login:@@root' 'assword:@@root' '# @@mount -r 10.0.2.10:564 /mnt/a' "$@" '# @@' 2>&1)
        case $(printf '%s\n' "$out" | grep -c "BdsDxe: starting") in [2-9]*) break ;; esac
        [ $tries -lt 3 ] || break
        echo "     (firmware never reached its boot entry on a node, retrying)" >&2
    done
}
after() { awk -v m="# $1" 'p || $0 == m { p = 1; print }'; }
keep() { n=$((n + 1)); log=build/reentrant-$n.txt; printf '%s\n' "$1" | tr -d '\r' > "$log"; }
no_faults() { grep -q -- '--- A: 0 fault lines' "$log" && grep -q -- '--- B: 0 fault lines' "$log"; }
show_fail() {
    echo "FAIL $1 ($log)"
    grep -n 'TIMEOUT\|FAULT\|restarted' "$log" | head -4
    tail -n 12 "$log" | sed 's/^/    | /'
    fail=1
}

# One recipe: run `# @@<marker>`, and require the client's own SUCCESS line, no
# netd fault/restart, and NOT the refusal the pre-step-3 tree answered with.
check() { # name, marker, want-regex, forbidden-refusal-regex
    name=$1; marker=$2; want=$3; forbid=$4
    attempt=0
    while :; do
        attempt=$((attempt + 1))
        run "# @@$marker"; keep "$out"
        tail=$(sed -n '/^NODE B/,$p' "$log" | after "$marker")
        if printf '%s\n' "$tail" | grep -q 'EL0 FAULT\|server slot 4 restarted'; then
            show_fail "$name: netd faulted instead of serving"; return
        fi
        # The pre-step-3 answer. Not retried: a refusal is a real regression
        # now, not a timing miss, so seeing it once is a failure.
        if printf '%s\n' "$tail" | grep -qE -- "$forbid"; then
            show_fail "$name: REFUSED - the client was turned away ($forbid)"; return
        fi
        if printf '%s\n' "$tail" | grep -qE -- "$want"; then
            if no_faults; then echo "ok   $name ($log)"
            else show_fail "$name: served, but a node reported faults"; fi
            return
        fi
        # Neither served nor refused nor faulted: the shared QEMU socket link
        # flaked (about 1 in 6 for any remote op on this rig).
        reason="no output, no refusal, no fault - the shared QEMU socket link flaked"
        note="no usable output" 
        if [ $attempt -ge 4 ]; then
            show_fail "$name: $reason in 4 tries"; return
        fi
        echo "     ($name: $note - retrying, $log)" >&2
    done
}

# The remote-mount path (NETOP_RMOUNT), the originally-reported case. SERVED
# since step 3: the run no longer blocks the loop, so the mount parks and is
# answered like any other. Before step 3 this reported "out of room".
check "a path verb is served during a run" \
    'cpu 10.0.2.10:564 ping 10.0.2.99 | cat /mnt/a/man/grep' \
    'grep - keep lines' \
    'out of room for this right now|that server is not running'
# A different, deeper handler that needs no mount (NETOP_RESOLVE): the case the
# first, RMOUNT-only cut of the refusal missed, and so the one that proves the
# fix is not op-specific either. "no response" IS the served outcome on THIS
# rig: the two nodes share a bare QEMU socket link with no SLIRP and so no DNS
# server, so netd runs the resolver and nothing answers it. What matters is
# that netd ran it at all - the pre-step-3 tree turned the request away with
# "network server busy" without ever reaching the handler, which is what the
# forbid pattern below still catches. (Measured: asserting a successful
# lookup here failed 4 of 4, because this rig cannot resolve anything.)
check "resolve is served during a run" \
    'cpu 10.0.2.10:564 ping 10.0.2.99 | resolve example.com' \
    'resolves to|NXDOMAIN|no such host|no response for' \
    'network server busy|that server is not running'
# The SESSION path (a C program's fid verbs). The by-sender refusal made this a
# flaky driver, so it was never graded here; with nothing refused by sender it
# is a recipe like the others, and it is the deepest of the three.
check "a fid verb sequence is served during a run" \
    'cpu 10.0.2.10:564 ping 10.0.2.99 | cbig' \
    'match the local copy' \
    'out of room for this right now|that server is not running'

exit $fail
