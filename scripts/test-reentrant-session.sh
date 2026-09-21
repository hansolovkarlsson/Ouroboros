#!/bin/sh
# The re-entrant session check: a remote-mount request from a LOCAL client
# arriving at netd while netd is inside a `cpu` run. Run on the two-node ext2
# rig (docs/testing/testing-qemu.md section 5, "The re-entrant session
# witness"), and graded on the lines it names.
#
# HOW THE CONDITION IS MADE. The shell spawns a pipeline's program stages
# before it runs a builtin source, so `cpu <A> ping <unreachable> | <client>`
# has <client>'s remote-mount request reach netd while netd is blocked in the
# run. The unreachable ping's ARP wait is what holds the run open long enough;
# a reachable command returns before the client asks and the request lands at
# the top level instead, proving nothing (measured: the file just prints).
#
# WHAT IT REFUSES, AND WHERE. netd refuses a non-child client's NETOP_RMOUNT
# from `tcp_run`'s re-entrant drain, before `handle_client`'s frame is
# allocated - the relay does not fit on the stack from there. The refusal is
# BEFORE the session/one-shot split, so both kinds of verb are refused at the
# same point; the graded recipe drives the one-shot path (`cat`), which reaches
# netd deterministically. The negative control, measured 2026-09-20 on the tree
# before the refusal: an EL0 data abort in netd at the guard page
# (esr_el1=0x9200004f), "server slot 4 restarted", and the client told its
# server had died mid-request. The session path (`cbig`) faulted the same way,
# one call level DEEPER; the ledger had called the one-shot case "latent" and
# it was not.
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
# server binary or a C program a recipe drives.
for k in target/aarch64-unknown-none/release/netd.bin build/cbig.bin; do
    [ -f "$k" ] && [ "$k" -nt "$B" ] && {
        echo "test-reentrant-session: $B is older than $k - run make images-2vm-ext2"; exit 2; }
done
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
# The B transcript, CR stripped, from the marker command on. A string
# comparison, not a regex, so a marker may contain / . * or |.
after() { awk -v m="# $1" 'p || $0 == m { p = 1; print }'; }
keep() { n=$((n + 1)); log=build/reentrant-$n.txt; printf '%s\n' "$1" | tr -d '\r' > "$log"; }
no_faults() { grep -q -- '--- A: 0 fault lines' "$log" && grep -q -- '--- B: 0 fault lines' "$log"; }
show_fail() {
    echo "FAIL $1 ($log)"
    grep -n 'TIMEOUT\|FAULT\|restarted' "$log" | head -4
    tail -n 12 "$log" | sed 's/^/    | /'
    fail=1
}

# GRADED: the one-shot path, refused from inside a run, netd stays up. `cat`
# reaches netd deterministically (unlike cbig below), so this is the check that
# must pass. A `cat` that PRINTS the file means the run ended before cat asked
# (condition not created) and is retried, not passed.
marker='cpu 10.0.2.10:564 ping 10.0.2.99 | cat /mnt/a/man/grep'
attempt=0
while :; do
    attempt=$((attempt + 1))
    run "# @@$marker"; keep "$out"
    tail=$(sed -n '/^NODE B/,$p' "$log" | after "$marker")
    if printf '%s\n' "$tail" | grep -q 'EL0 FAULT\|server slot 4 restarted'; then
        show_fail "one-shot verb inside a run: netd faulted instead of refusing"; break
    fi
    if printf '%s\n' "$tail" | grep -q 'cat: .*out of room for this right now'; then
        if no_faults; then echo "ok   one-shot verb inside a run is refused, netd stays up ($log)"
        else show_fail "one-shot verb inside a run: refused, but a node reported faults"; fi
        break
    fi
    if [ $attempt -ge 4 ]; then
        show_fail "one-shot verb inside a run: the condition was not created in 4 tries (no refusal, no fault)"; break
    fi
    echo "     (condition not created or the link flaked - retrying, $log)" >&2
done

# BEST-EFFORT: the session (fid) path, same refusal one level deeper. cbig
# reaches netd only intermittently - a pre-existing race delegates its netd
# send right just after it is spawned, so its first request is sometimes
# refused by capability ("not allowed to reach that server") before any remote
# request, unrelated to this check. So: a FAULT here fails (a regression on the
# deeper path), "out of room" is the win, and a persistent capability refusal
# is reported as an observation, never a failure - a check that turned red on a
# separate known race would be the blind instrument the postmortems warn of.
marker='cpu 10.0.2.10:564 ping 10.0.2.99 | cbig'
attempt=0
while :; do
    attempt=$((attempt + 1))
    run "# @@$marker"; keep "$out"
    tail=$(sed -n '/^NODE B/,$p' "$log" | after "$marker")
    if printf '%s\n' "$tail" | grep -q 'EL0 FAULT\|server slot 4 restarted'; then
        show_fail "session verb inside a run: netd faulted instead of refusing"; break
    fi
    if printf '%s\n' "$tail" | grep -q 'cbig: .*out of room for this right now'; then
        if no_faults; then echo "ok   session verb inside a run is refused, netd stays up ($log)"
        else show_fail "session verb inside a run: refused, but a node reported faults"; fi
        break
    fi
    if [ $attempt -ge 5 ]; then
        echo "note session verb inside a run: cbig never reached netd in 5 tries (the capability"
        echo "     race, not this fix); the one-shot check above covers the shared refusal ($log)"
        break
    fi
    echo "     (cbig capability-refused or the link flaked - retrying, $log)" >&2
done

exit $fail
