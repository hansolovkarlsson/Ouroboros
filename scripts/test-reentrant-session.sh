#!/bin/sh
# The re-entrant client check: a request from a LOCAL client arriving at netd
# while netd is inside a `cpu` run. Run on the two-node ext2 rig
# (docs/testing/testing-qemu.md section 5, "The re-entrant client witness"),
# graded on the lines it names.
#
# HOW THE CONDITION IS MADE. The shell spawns a pipeline's program stages
# before it runs a builtin source, so `cpu <A> ping <unreachable> | <client>`
# has <client>'s request reach netd while netd is blocked in the run. The
# unreachable ping's ARP wait holds the run open; a reachable command returns
# before the client asks and the request lands at the top level, proving
# nothing (measured: the client just succeeds).
#
# WHAT IT REFUSES, AND WHY TWO CLIENTS. netd refuses an IDENTIFIED client's
# request from `tcp_run`'s re-entrant drain, before `handle_client`'s frame is
# built - every handler a client would reach (the remote-mount relay, an ICMP
# ping, a DNS/TCP lookup) is too deep for that stack. Only the supervisor's
# health ping (no identity) is serviced from there. So the refusal is by
# SENDER, not by op: `cat` (a remote-mount NETOP_RMOUNT) and `resolve` (a
# NETOP_RESOLVE, a different, deeper handler that needs no prior mount) must
# BOTH be refused. The first cut refused only NETOP_RMOUNT and `resolve` still
# faulted netd - hence both recipes here.
#
# The negative control, measured 2026-09-20 on the tree before the refusal: an
# EL0 data abort in netd at the guard page (esr_el1=0x9200004f), "server slot 4
# restarted", and the client told its server had died mid-request - for `cat`,
# for `resolve`, and (deeper) for `cbig`.
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

# One recipe: run `# @@<marker>`, and require the client's refusal line AND no
# netd fault/restart. A `condition_ok` grep that DID create the condition but
# without the refusal (e.g. the run ended first) is retried, not passed.
check() { # name, marker, want-regex, condition-not-created-regex [, also-want-regex, forbid-regex]
    name=$1; marker=$2; want=$3; notyet=$4; also=${5:-}; forbid=${6:-}
    attempt=0
    while :; do
        attempt=$((attempt + 1))
        run "# @@$marker"; keep "$out"
        tail=$(sed -n '/^NODE B/,$p' "$log" | after "$marker")
        if printf '%s\n' "$tail" | grep -q 'EL0 FAULT\|server slot 4 restarted'; then
            show_fail "$name: netd faulted instead of refusing"; return
        fi
        if [ -n "$forbid" ] && printf '%s\n' "$tail" | grep -qE -- "$forbid"; then
            show_fail "$name: the old answer came back ($forbid)"; return
        fi
        if printf '%s\n' "$tail" | grep -qE -- "$want" \
            && { [ -z "$also" ] || printf '%s\n' "$tail" | grep -qE -- "$also"; }; then
            if no_faults; then echo "ok   $name ($log)"
            else show_fail "$name: answered, but a node reported faults"; fi
            return
        fi
        # The client SUCCEEDED: the run returned before it asked, so the
        # re-entrant condition was never created. Expected sometimes (timing);
        # retried, and named distinctly from a link flake so a persistent one
        # points at the unreachable-ping timing, not at the kernel.
        if printf '%s\n' "$tail" | grep -qE -- "$notyet"; then
            reason="the client kept succeeding - the run returned before it asked (ping timing)"
            note="condition not created (client succeeded)"
        else
            reason="no refusal, no fault, no success - the shared QEMU socket link flaked"
            note="no usable output"
        fi
        if [ $attempt -ge 4 ]; then
            show_fail "$name: $reason in 4 tries"; return
        fi
        echo "     ($name: $note - retrying, $log)" >&2
    done
}

# The remote-mount path (NETOP_RMOUNT), the originally-reported case. SERVED
# since the async remote mount (docs/roadmap/roadmap-async-rmount.md step 1):
# a path verb is parked from the re-entrant drain without the deep frame, and
# netd says so on the console, which is the witness that the request really
# arrived mid-run (a served read looks the same either way). Graded on BOTH
# lines: the file's text and the park line. On the tree before the park this
# recipe answered "out of room", which the forbid pattern catches.
check "one-shot rmount SERVED inside a run" \
    'cpu 10.0.2.10:564 ping 10.0.2.99 | cat /mnt/a/man/grep' \
    'netd: remote mount parked inside a run' \
    'grep - keep lines' \
    'grep - keep lines' \
    'out of room for this right now'
# A different, deeper handler that needs no mount (NETOP_RESOLVE): the case the
# first, RMOUNT-only cut missed.
check "resolve refused inside a run" \
    'cpu 10.0.2.10:564 ping 10.0.2.99 | resolve example.com' \
    'resolve: network server busy' \
    'resolves to|NXDOMAIN|no such host'

exit $fail
