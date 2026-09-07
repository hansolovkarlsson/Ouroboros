# Work journal

*Why, in the order it happened. One file per working day, named for the date,
and a day gets a new file rather than an append to a single page. Inside a
day the entries run in the order they were written, oldest first, so a day's
closing summary is at the bottom. This is where the reasoning goes that the
other records have no room for: what was tried, what the first framing got
wrong, why a decision went the way it did. Wrong turns are recorded as wrong
turns, because a retreat is only informed if the map shows the dead end.
Prose, not bullets, written for a stranger who has only this to go on.*

The other records answer narrower questions and are kept separately: what is
left ([`ROADMAP.md`](../ROADMAP.md)), how each finished arc was sequenced
([`roadmap-completed.md`](../roadmap-completed.md)), what a mistake taught (the
postmortems, indexed in [`README.md`](../README.md)), and when something
shipped ([`CHANGELOG.md`](../CHANGELOG.md)). If a paragraph would fit in one of
those, it belongs there and not here.

Until 2026-09-07 this was one file, `docs/journal.md`, newest first, 3489
lines by the end. It was split into the files below on that date, one per day,
with each day's entries reversed into chronological order and nothing else
changed. A link that names `journal.md` in text older than that is a link to
what is now this directory.

| Day | Entries | What it covers |
|---|---|---|
| [2026-08-22](2026-08-22.md) | 1 | the userland day: /bin, pipelines, and the filesystem arc begins |
| [2026-08-23](2026-08-23.md) | 1 | the filesystems day: exFAT read+write, a cleanup, then ext2 |
| [2026-08-24](2026-08-24.md) | 1 | real-hardware xHCI, the north star, and the first releases |
| [2026-08-25](2026-08-25.md) | 7 | the cluster day: Phase 0 done (v0.5.0), Phase 1 begun · Phase 1c: a machine reads another's disk over TCP · Phase 1d: two Ouroboros machines, one reads the other's disk · Phase 2: a machine writes another's disk (shared-disk cluster) · Phase 3 begins: /proc, and reading another machine's processes · Phase 3, step 2: /dev/cons, writing another machine's screen · Phase 3, step 3: /net, and Phase 3 complete |
| [2026-08-26](2026-08-26.md) | 10 | the namespace-aware export: paying down the three prefix hacks · Phase 4a: running a program on another machine · Phase 4b: the full cpu model, importing the caller's namespace · v0.9.0, and admitting the syscalls aren't POSIX · locking the cluster door: export authentication · dialing out of another machine's NIC (`/net/tcp`) · dialing *in* through another machine (`/net/tcp` accept) · reply authentication (auth tier 2, the cheap half) · `cpu` output past one message (closing out the cluster) · four more `/bin` filters (`tail`/`nl`/`rev`/`uniq`) |
| [2026-08-27](2026-08-27.md) | 21 | `ps` shows process names · a path-command bug: `bin/echo` from `/` · `tree` joins `/bin` · `shutdown` and `halt` · `ls` grows up: columns, sort, and `-l` · a pager: `more` (and `less`) · moving `pwd` and `write` out to `/bin` · the keyboard arc: interactive programs can be `/bin` now · shrinking the shell: more/less/send/recv/selftest to /bin · `-?` usage help everywhere · man pages · shell wildcards and tab completion · keeping the roadmap forward-looking · reading the neighbours: Redox OS and the Pi-4 tutorials · FAT32 long-filename *write* · GPT CRC validation + backup fallback · `a \| b > file`: pipelines compose with redirection · grep flags, and a YIELD syscall for prompt early-exit · a builtin anywhere in a pipeline · `sort`, the filter that can't stream · exporting the environment to child programs |
| [2026-08-28](2026-08-28.md) | 3 | the users/permissions arc: from stored metadata to an enforced login · the libc arc, ending at a real C library (picolibc) · closing the security arc: account management |
| [2026-08-29](2026-08-29.md) | 1 | finishing the security tier, and learning to distrust green |
| [2026-08-30](2026-08-30.md) | 1 | the question a server is actually asking |
| [2026-08-31](2026-08-31.md) | 2 | a remote request now says who is asking · the shared cluster key is gone |
| [2026-09-01](2026-09-01.md) | 1 | reviewing the review's repairs, and shipping v0.16.0 |
| [2026-09-02](2026-09-02.md) | 1 | emptying the small-gaps parking lot, and four blind instruments |
| [2026-09-03](2026-09-03.md) | 7 | shrinking `CLAUDE.md`, and what the move exposed · the `ls`-of-a-remote-mount bug, and a review that inverted my account of it · `ls` stops claiming every failure is a missing file · a status code nobody compared, and the `mv` it was hiding · widening the parser instead of describing the gap · the twenty-ninth postmortem: true when written · the supervisor was killing netd mid-read |
| [2026-09-04](2026-09-04.md) | 1 | housekeeping, and measuring before restructuring |
| [2026-09-05](2026-09-05.md) | 2 | clearing the site by deleting four of it · the fid verbs, two gates, and three reviews of twenty lines |
| [2026-09-06](2026-09-06.md) | 7 | the remote-read bug was never the wedge timer, and the citation was the problem · a second review, and the reason beside a right decision was wrong · the wrong slot, and a bug you have to build a window to reach · a grant that was only ever made once · a slot is a position, and the kernel now names the occupant · rights flow one step down, and a nested shell can finally pipe · the day as a whole, written at its close |
| [2026-09-07](2026-09-07.md) | 1 | an audit of the segment, and four moves that emptied the top of `docs/` |
