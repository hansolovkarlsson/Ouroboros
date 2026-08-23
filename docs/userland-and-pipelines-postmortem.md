# Userland maturation: /bin, pipelines, and where a command belongs

*A design retrospective — the ninth in this project's series, and a companion to
the [capability & hardening](capability-and-hardening-postmortem.md) and
[network stack](network-stack-postmortem.md) retrospectives whose mechanisms it
builds on. Not a bug hunt: one long day that turned the shell's compiled-in
builtins into a real `/bin` of standalone programs with genuine multi-stage
pipelines, then began abstracting the filesystem. The through-line is a question
that kept recurring: **where does this command actually belong** — in the shell,
in a `/bin` program, or nowhere yet? Almost every interesting decision was an
answer to it.*

## The starting point

Every command was a shell builtin: the shell parsed a line and called a `cmd_*`
function compiled into its own binary. `exec /path` could launch a program, but
passed it no arguments — a spawned task's registers were simply zeroed. The
capability model, per-task isolation, grant/safecopy IPC, and (from the network
arc) runtime capability *delegation* all already existed. What didn't exist was
any reason for a program other than the shell to run — no argv, no `/bin`, no
environment, no way to compose programs.

The plan was a staged arc: an argv ABI, a `/bin` + `PATH` lookup, a shell
environment, then externalize the commands over a shared `ulib` support crate.
It mostly went as planned. The lessons are in the places it didn't.

## Lesson 1: not every command wants to be a program (the ps/kill/wait revert)

The externalization was going smoothly — `echo`, `ls`, `cat`, `mkdir`, `cp`,
`mv`, and eventually `ping`/`resolve`/`fetch` all became clean `/bin` programs.
So the next batch seemed obvious: the task-management commands `ps`, `kill`,
`wait`. They were built, and they *mostly* worked. Then a test sequence exposed
the flaw:

```
$ ps                 # shows a long-lived task at slot 5
$ kill 5             # correctly kills it
$ wait 5             # "that task is protected"
```

`wait 5` was waiting on *itself*. An externalized command runs in a spawnable
slot, and slots are reused immediately, so by the time `wait` ran it had *become*
task 5 — the very number `ps` had reported for something else a moment earlier.
The whole point of these commands is to act on the shell's view of the task
table, and putting them in a spawned task both makes them appear in their own
output and makes any task number they're handed racy.

This is not a bug to fix; it's a category error. It's exactly why `kill`, `wait`,
`jobs`, `fg`, and `bg` are shell *builtins* in bash — they're job control, they
belong to the shell. Unix `ps` is external only because Unix PIDs are stable;
Ouroboros reuses slots the instant they free, so even an external `ps` misleads.
The work was reverted, and the finding recorded in the roadmap.

**The lesson:** "externalize the commands" is not a uniform goal. A command that
takes a path and does I/O (the filesystem and network commands) externalizes
cleanly; a command that manipulates the shell's own control state does not.
Testing, not review, drew the line — the code compiled and ran; only exercising
it revealed that the abstraction was wrong. Knowing when to *stop* an arc is part
of executing it.

## Lesson 2: a packed bitfield has a ceiling you can walk into (the caps collision)

Raising the spawnable-slot count (Stage 0) from two to five was meant to be a
mechanical `NUM_TASKS` bump. It nearly wasn't. The per-slot capabilities are
packed into one `u32`: the low `NUM_TASKS` bits are the IPC send-mask ("may I
initiate a send to slot *t*?"), and the resource caps (`CAP_BLOCK`, `CAP_CON`,
`CAP_NET`) sat at bits 8, 9, 10. At `NUM_TASKS = 7` the send-mask stopped at bit
6 — a comfortable gap. At `NUM_TASKS = 10` it reaches bit 9: a spawnable slot's
send-mask bit would have *aliased* `CAP_CON`, silently granting or denying a
device capability as a side effect of the slot count.

Caught by reading the layout before flipping the constant, not by a failure —
which is the point. A packed representation encodes an invariant (send-mask and
resource caps don't overlap) that nothing enforces; growing one field walked it
toward the other. The fix was to move the resource caps to bit 16 and up, clear
of the send-mask for any plausible slot count, and to say so in a comment so the
next person raising `NUM_TASKS` doesn't rediscover it.

## Lesson 3: build the mechanism for a consumer that exists (delegation, twice)

Runtime capability *delegation* — the shell handing a spawned child a
send-capability it doesn't statically hold — shipped during the network arc, and
the [capability postmortem](capability-and-hardening-postmortem.md) had flagged a
trap: don't build the *general* (transitive, multi-target) version until a
consumer needs it, or it's a mechanism without a user. This day produced two
consumers, and both turned out to need *less* than feared.

**The network commands** (`ping`/`resolve`/`fetch`) reach `netd`, which a
spawnable slot's static mask (`TO_SHELL | TO_FSD | TO_CON`) doesn't include. The
clean fix wasn't to widen the policy for every spawned program — it was for the
shell (which holds `TO_NET`) to `DELEGATE` it to the specific child at spawn. One
real subtlety: a tick can let the child run and call `netd` in the window before
the shell's delegation lands, so the child gets `MSG_ERR_DENIED`. Rather than
add synchronization, `ulib::net_call` retries briefly on that specific error —
the same bounded-wait pattern the pipe producer already used. The race resolves
itself.

**Multi-stage pipelines** (`a | b | c`) looked like the case that would finally
*require* general delegation — relay-free program-to-program chains were exactly
the example the roadmap gave for it. But a linear chain doesn't: task *a*
delegates to *b*, *b* to *c*, and each task has exactly **one** downstream
target — which the existing one-target-per-task delegation already models
perfectly. The general version still has no consumer. The lesson from the
capability postmortem held: the hard, general mechanism wasn't needed; the
specific one was enough, and building it first would have been waste.

## Lesson 4: a heuristic that doesn't generalize is technical debt (the pipeline rewrite)

The two-stage pipe had grown a patchwork: it split on the *first* `|` only, the
right side had to be a single token with no arguments and an explicit path (no
`$PATH`), and it decided "is the left side a program or a builtin?" by a
`/`-prefix heuristic. Each of those was a reasonable shortcut for a single
`builtin | program` pipe, and none of them survived contact with N stages,
arguments, or bare command names.

The fix was not to extend the heuristics but to replace them: split on every `|`,
resolve each stage the way any command is resolved (`$PATH` or path), tokenize
each into its own argv, and spawn the chain right-to-left, delegating each link.
The `/`-prefix trick became a proper "is this a builtin, a program, or unknown?"
decision. The rewrite was larger than an extension would have been, but it left
one code path instead of three, and the "programs take no arguments in a pipe"
and "one `|` per line" limitations simply evaporated rather than needing their
own future fixes.

**The lesson:** when a shortcut has to be special-cased to handle the next case,
that's the signal to generalize, not to add another special case. The patchwork
had reached that point.

## Lesson 5: to test a format, you have to be able to make one (GPT)

The filesystem arc opened with a pure refactor (extract `fsd`'s hardcoded FAT32
into a `Filesystem` enum, prove byte-identical) and then GPT + multi-partition
support. The code was straightforward; *testing* it was the problem. `fsd`'s GPT
parser only matters if it's fed a real GPT disk, and macOS ships no GPT tooling
(`sgdisk`/`gdisk` absent; `hdiutil` builds MBR). Worse, the disk has to be
*bootable* — the parser runs at runtime, after UEFI has loaded the kernel from
the disk — and UEFI validates a GPT's CRC32s before it will boot from it.

So testing the feature meant building the artifact by hand: `scripts/mkgpt.py`
wraps the existing FAT32 partition in a protective MBR, primary and backup GPT
headers with correct CRC32s, and an EFI-System-Partition entry. Only once UEFI
accepted the CRCs and booted from it could the actual thing under test — `fsd`
discovering the partition through the GPT path — run at all. And the test is
conclusive precisely because the GPT disk has *no* real MBR partition table (only
the protective 0xEE entry): `fsd` mounting it means the GPT parser found it,
nothing else could have.

**The lesson:** for a format or protocol, the test harness that *produces* valid
inputs is part of the feature, and sometimes the harder part. A parser you can't
feed a real, valid input isn't tested. The `mkgpt.py` builder is now committed
alongside the parser it verifies — and it's exactly what the next arc steps
(exFAT, ext2, each on GPT) will reuse.

## The shape of the day

Two arcs closed (standalone binaries, multi-stage pipelines) and a third begun
(filesystems), and the recurring discipline underneath was the same one the
earlier retrospectives keep finding: **scope it down before you build it, and let
testing tell you where the real boundaries are.** Three of the day's decisions
were subtractions — job control *stays* builtin, general delegation *isn't*
needed, the pipeline heuristics *go away* — and each made the system simpler, not
poorer. The additions that remained (argv, `/bin`, a filter set, a VFS enum, a
GPT parser) were the ones a concrete consumer actually wanted.

Everything here was QEMU-verified with zero exception-trace aborts. The one honest
gap: none of it has run on real Parallels hardware yet — a large body of change
resting on the emulator's confidence, and a real-hardware regression pass is the
outstanding debt this day leaves behind.
