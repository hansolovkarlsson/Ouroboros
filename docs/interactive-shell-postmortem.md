# Interactive programs, and the second axis of "where a command belongs"

*A design retrospective — the eighteenth in this project's series, and a direct
continuation of [userland maturation](userland-and-pipelines-postmortem.md)
(the ninth), which asked **where does a command belong** — shell builtin, `/bin`
program, or nowhere yet. That arc externalized the commands that take a path and
do I/O and kept job control in the shell. This one is the branch that finished
the job: a bigger `/bin` (a pager, `tree`, `man`, `pwd`/`write`/`send`/`recv`),
richer commands (`ls -l`, `ps` names + exit codes, `-?` help everywhere), and
shell ergonomics (wildcards, tab completion, `shutdown`/`halt`). The interesting
part is that finishing it required a **second axis** the ninth postmortem didn't
have: not just "is this job control?" but **"who owns the keyboard?"** — and
answering that turned "interactive program" from a shell-only privilege into a
`/bin` capability. Plus two old traps that refused to stay retired.*

## The starting point

After the pipelines arc, the split looked settled: filesystem and network
commands were `/bin` programs over `ulib`; job control (`ps`/`kill`/`fg`/`wait`)
and shell state (`cd`/`env`) stayed builtin. But three things were still
builtins that *looked* like they wanted to be programs — `more`/`less` (a
pager), `pwd`, `write` — and one whole class of program was flatly impossible:
anything that reads the keyboard *while running*. The shell's line editor owned
the keyboard. A spawned task couldn't read a key, because task 0 (the shell) was
the only task the keyboard input path ever delivered to. So "an editor" or "a
BASIC interpreter" or even a pager seemed to imply "build it into the shell."

Hans asked the question directly: *does an interactive program have to be built
into the shell?* Answering "no" is this postmortem's spine.

## Lesson 1: interactivity is keyboard ownership, not a shell property

The instinct — "interactive means built into the shell" — is wrong, and it's
wrong for a reason the rest of this kernel already knew. Every *other* scarce
hardware resource in Ouroboros is single-owner and handed to exactly one task:
the block device answers only `fsd`, the NIC only `netd`, the framebuffer only
`cond`. The keyboard is no different — it's a single-owner resource that happened
to be permanently owned by the shell. "Interactive" isn't a property of *being*
the shell; it's a property of *holding the keyboard*.

So the fix wasn't to move interactive programs into the shell — it was to make
keyboard ownership **transferable**. A single `INPUT_OWNER` names the task the
keyboard byte path delivers to. The shell, when it foregrounds a command
(`FG`), hands ownership to the child; when the child exits, ownership returns.
A `/bin` program reads a key through `ulib::read_char` exactly the way the shell
does — it just has to be the foreground task to get one. Ctrl+C is routed the
same way: the interrupt-key check sets a `PENDING_KILL`, and `on_tick` kills the
foreground child, so the shell gets its keyboard back.

The proof was `/bin/readkey` (an echo-the-keystroke diagnostic) reading keys as
an ordinary spawned program, and then the real consumer: **the pager became a
`/bin` program.** `more`/`less` reads its input *and* the keyboard while
running — the thing "nothing here does yet" a day earlier — and now it's not
special at all. It's just a program that happens to be foreground.

**The lesson:** when a capability seems to require being a specific privileged
task, check whether the real requirement is a *resource that task holds*. If it
is, make the resource transferable and the privilege dissolves. "Interactive
programs must be builtins" was a resource-ownership assumption wearing an
architecture costume.

## Lesson 2: a new lever on the task table inherits every old guard — or it's a hole

`FG` was new: "make task N the foreground keyboard owner," and Ctrl+C kills the
foreground owner. That's a new way to *act on a task by number* — and the
project already had a hard rule that acting on a task by number must refuse the
protected slots (0 = shell, 1 = idle, 2–4 = the `fsd`/`cond`/`netd` servers).
`kill` had that guard. `FG` was written without it.

The `/ultrareview` pass caught it (finding bug_001): `fg 2` followed by Ctrl+C
would foreground the **filesystem server** and then kill it. The supervisor
would restart it, so it wasn't fatal — but it's a userland command reaching
across the protected boundary the whole isolation arc exists to defend. The fix
was two-sided: `FG` rejects slots 1–4 outright, and the `on_tick` kill is
independently guarded to `(FIRST_SPAWNABLE..NUM_TASKS)` so *no* path — not just
the one I was thinking about — can turn Ctrl+C into a dead server.

**The lesson:** every new verb that takes a task number is a new place the
protected-slot invariant can be violated, and the invariant is only as strong as
its *least*-guarded caller. Adding a control path means re-auditing the guard on
every path, not just copying the happy case of an existing one. (And: a foreign
reviewer finds the lever you didn't think to guard precisely because they don't
share your mental model of "which tasks `fg` would obviously be used on.")

## Lesson 3: two independent gates decide if a command can leave the shell — and one is a chicken-and-egg

With the keyboard gate solved, Hans asked the natural follow-up: if `more`/`less`
can be externalized, can `partition` and `format` too? They're big, they're not
job control — why are they builtins?

The answer is the sharpest scoping point of the branch, and it's a second,
*independent* reason a command can't be a `/bin` program. `format`, `partition`,
and `erase` run **when nothing is mounted** — that is precisely their job:
prepare a blank or unmounted disk. But a `/bin` program is *loaded from the
mounted disk*. A `format` that lived in `/bin` could not be read into memory at
the exact moment you need it, because the filesystem it would be read through is
the one that doesn't exist yet. **The disk-management commands cannot live on the
disk they manage.** It's a bootstrap dependency, not a preference.

So "can this command be externalized?" turns out to have (at least) three
independent gates, and a command stays builtin if it fails *any*:

1. **Job control?** (`ps`/`kill`/`fg`/`wait`) — belongs to the shell's view of
   the task table; externalizing it makes it race itself (the ninth
   postmortem's ps/kill/wait revert).
2. **Needs the keyboard while running?** — *used* to be a gate; the
   keyboard-ownership arc **removed it**, which is why the pager could move.
3. **Runs when the disk is unavailable?** (`format`/`partition`/`erase`, and —
   for a different reason, power control with no disk — `shutdown`/`halt`) —
   can't be loaded from a disk that's unmounted or being formatted.

The minimal-shell principle isn't "externalize everything." It's "externalize
everything that *can* be, and be able to say *exactly why* each holdout can't."
When I laid these three gates out, the "move the clean ones" decision made
itself: `more`/`less`/`pwd`/`write`/`send`/`recv`/`selftest` cleared all three
and left; `format`/`partition`/`shutdown`/`halt`/the job-control set did not and
stayed. And `help` got a matching correction — as commands left, a builtin
`help` that still listed them was describing a shell that no longer owned them,
so `help` now lists only the builtins and points at `/bin` for the rest.
Documentation follows architecture: the command list *is* the boundary.

## Lesson 4: the old traps don't retire (two "same limit, new caller" bugs)

Two bugs this branch had nothing to do with, that it hit anyway because it wrote
new code over old limits.

**The `&str`-slice PIE trap, again.** Wildcards and tab completion suddenly
wouldn't link: `R_AARCH64_ABS64 ... referenced by core`. This project has known
since the first shell that `core::fmt` can't be PIE-linked, but I hadn't tripped
it in a while and it took a real bisection to re-find. The culprit wasn't the
filesystem calls — it was **slicing a `&str` by a runtime index**. That emits
`str`'s char-boundary panic path, which *formats* the offending string to build
its message, which drags in the unlinkable `core::fmt`. Byte slicing (`&[u8]`)
doesn't. The glob matcher and the completion code were rewritten to work
entirely in bytes, calling `from_utf8` only where a `&str` was genuinely needed.
(Written up as a standalone memory so the next runtime-index slice gets caught
at the keyboard, not the linker.)

**The pager's piped-read overflow.** `more` reads its stdin into a 256 KB heap
buffer — plenty. But when it was the consumer end of a pipe (`ls | more`), it
called `MSG_RECV` with a slice sized to *the heap buffer*, and `MSG_RECV`
rejects any length over `MSG_MAX_LEN` (768 bytes). The read failed. The buffer
size and the message size are **two different limits**, and the reader conflated
them — a large destination buffer doesn't mean you can ask for a large message.
Fixed by reading in bounded `MSG_MAX_LEN` chunks into the big buffer.

**The lesson (both):** a mature kernel accumulates hard limits (`core::fmt` is
unlinkable; an IPC message is ≤768 bytes), and *new* code violates them just as
easily as old code did — the limit lives in the platform, not in the module that
first respected it. The traps don't retire; only the habit of checking for them
does, and it lapses.

## What didn't need a postmortem

Most of the branch. `tree`, `man` + its plain-text pages, `-?` usage help,
`ls -l`/`-a` with the new `NP_STAT` verb, `ps` names + exit codes
(`TASK_NAME`/`TASK_EXIT_CODE`), the relative-path command fix, `shutdown`/`halt`
over a PSCI `SYSTEM_OFF` — all went in cleanly, each a small, turnkey addition
over machinery the earlier arcs had already built (the `/bin`+PATH pattern, the
`ninep-abi` verb set, `ulib`). That's the payoff of the ninth postmortem's
externalization work: once the pattern is turnkey, most new commands are
genuinely just "a new crate under `programs/<category>/` over `ulib`." The
lessons here cluster exactly where the branch touched something *structural* —
keyboard ownership, the task-table guard, the externalization boundary — and
nowhere else. The interesting bugs are always at the joints.
