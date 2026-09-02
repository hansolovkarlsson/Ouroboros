# Archive — contemporaneous build logs

Raw, day-by-day notes kept while the work was happening, moved here from the
assistant's working memory on 2026-09-01 so they live somewhere versioned
instead of somewhere that is not.

**These are not the reference.** Everything in them is recorded properly
elsewhere, and those places are the ones to read:

| For | Read |
| --- | --- |
| What was built, and when | [`../CHANGELOG.md`](../CHANGELOG.md) |
| The narrative of each day | [`../journal.md`](../journal.md) |
| How an arc was sequenced | [`../roadmap-completed.md`](../roadmap-completed.md) |
| What went wrong and why | the postmortems in [`../`](../) |
| The load-bearing boot/MMU/syscall guidance | [`../../CLAUDE.md`](../../CLAUDE.md) |

They are kept for one purpose: **archaeology.** A contemporaneous note
occasionally records *why a thing looked right at the time*, which a tidied-up
retrospective written afterwards tends to lose. If you are trying to work out
what someone knew on a particular afternoon, this is the only place that says.

- [`cluster-build-log-2026-08.md`](cluster-build-log-2026-08.md) — the cluster
  arc as it happened: Phases 0–4, authentication, dial-out and dial-in,
  reply-auth, `cpu` output streaming. Releases v0.5.0 through v0.14.0.
- [`early-milestones-log-2026-08.md`](early-milestones-log-2026-08.md) — the
  first milestones: boot bring-up, the shell, filesystems, USB, the network
  stack.
- [`memory-notes-2026-09.md`](memory-notes-2026-09.md) — the assistant's memory
  directory as it stood on 2026-09-02, immediately before it was consolidated,
  in two passes the same day: 26 notes to 20 (the notes that duplicated this
  repository), then 20 to 15 (the notes that duplicated each other). Twelve
  notes were retired or merged, and the verbatim text of each survives only
  here.

The `[[double-bracket]]` links in these files point at assistant memory notes,
not at files in this repository; they will not resolve here. Left as written
rather than rewritten, because editing a contemporaneous record defeats the only
reason to keep one.
