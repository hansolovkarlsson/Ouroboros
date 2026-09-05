#!/usr/bin/env python3
"""Check that the published website has not fallen behind the documents it
abridges.

`docs/` is served live by GitHub Pages (https://hansolovkarlsson.github.io/Ouroboros/,
branch `main`, path `/docs`), and `docs/site/*.html` is HAND-WRITTEN. It is not
generated from the markdown and could not easily be: the pages are curated
ABRIDGEMENTS, not renderings. `architecture-overview.html` is a sixth of
`architecture.md` and a different document with a different job;
`changelog.html` was half the words of `CHANGELOG.md` before it was deleted
(see the MANIFEST note below); the site carries 10 of the 29 postmortems;
`glossary.html` has no markdown source at all; and the slugs are not derivable
from the filenames (`capability-and-hardening-postmortem.md` ->
`postmortem-capability-hardening.html`).

So there is no build step to notice when a source moves on and its page does
not, and the failure is silent and outward-facing: the site keeps serving a
confident, stale answer to the public. It had already happened when this check
was written - the whole site froze on 2026-08-23 while seven sources kept
moving, the worst by 12 days, so two releases and a closed frontier item were
absent from the public site with nothing anywhere reporting it.

This does not fix drift and does not generate anything. It makes drift LOUD:
re-abridging a page by hand stays a human job, but forgetting to stops being
free.

WHY BLOB HASHES AND NOT DATES. The obvious check - "is the source's last commit
newer than the page's?" - is both too weak and too easily levelled. A repo-wide
mechanical edit touches sources and pages in the SAME commit and equalises every
date, so anchored on the current history the check would have been born green:
PR #92's roadmap.md -> ROADMAP.md rename rewrote all 23 pages and all their
sources at once. Dates are also too coarse to see a same-day edit: comparing
last-commit dates found 7 stale pages, and comparing blobs found 9 - the two it
missed (research-helix-os, research-minix-boot) had a matching date and 19-20
changed lines each, one of them a whole "this section's premise is now out of
date" block that never reached the public page.

The cost of blob-exactness is the opposite error: a mechanical edit applied to
BOTH sides still changes the source blob and reports a page that is really in
sync. That happened once here at exactly one page (microkernel-comparison, two
lines, both the rename), and it is cleared by re-stamping that page after
confirming it needs no edit. Over-reporting in that direction is the safe one.

TO CLEAR A FAILURE: re-read the source, update the page to match, then re-stamp
that page:

    python3 scripts/check-site-freshness.py --update site/manual.html

Naming pages is deliberate - a bare --update re-stamps everything reported,
which asserts a review that did not happen and is the one way this check can be
made to lie.
"""
import os
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)

# page (under docs/) -> (markdown source under docs/, source blob it was written
# against) or None for a page with no markdown source, which is hand-written
# site-only content and cannot go stale against anything.
#
# EVERY page under docs/site/ must appear here, and main() fails if one does
# not. That is deliberate: a new page added without a line here would otherwise
# be unchecked and look fine, which is the state this whole file exists to
# prevent.
#
# FOUR PAGES ARE ABSENT ON PURPOSE. changelog, roadmap, shell-commands and
# processes were deleted as pages on 2026-09-05 and docs.html now links to the
# markdown on GitHub instead. They were reference material - worth CURRENT
# rather than abridged - and roadmap.html was 11,050 words against a 14,286-word
# source, which is barely an abridgement and so drifted every time the roadmap
# was touched. Deleting a drift surface beats promising to re-abridge it
# forever. Do not "restore" them here without restoring the pages: the presence
# check above would fail immediately, which is the intended way round.
MANIFEST = {
    "index.html": None,
    "site/docs.html": None,
    "site/glossary.html": None,
    "site/architecture-overview.html": ("architecture.md", "15c87b76f698"),
    "site/manual.html": ("manual.md", "ea3f93e07175"),
    "site/microkernel-comparison.html": ("microkernel-comparison.md", "5a75d8e5a23a"),
    "site/tutorial.html": ("tutorial.md", "cd4430be90e9"),
    "site/research-directions.html": ("research-directions.md", "6a43dfc7acbd"),
    "site/research-helix-os.html": ("research-helix-os.md", "01c5568c3840"),
    "site/research-minix-boot.html": ("research-minix-boot.md", "da738d5c2676"),
    "site/postmortem-boot-bringup.html": ("boot-bringup-postmortem.md", "d43cf0144698"),
    "site/postmortem-capability-hardening.html": ("capability-and-hardening-postmortem.md", "1f65f27ce9d8"),
    "site/postmortem-console-server.html": ("console-server-postmortem.md", "1a65a5208cdf"),
    "site/postmortem-filesystems.html": ("filesystems-arc-postmortem.md", "0241af6d571b"),
    "site/postmortem-isolation-dataflow.html": ("isolation-and-dataflow-postmortem.md", "dc6669553308"),
    "site/postmortem-network-stack.html": ("network-stack-postmortem.md", "624f8bbc5254"),
    "site/postmortem-shell-filesystem.html": ("shell-and-filesystem-postmortem.md", "519b37f659c1"),
    "site/postmortem-usb-storage.html": ("usb-storage-postmortem.md", "337ef42ceec1"),
    "site/postmortem-userland-pipelines.html": ("userland-and-pipelines-postmortem.md", "065974c4e2cb"),
    "site/postmortem-xhci-keyboard.html": ("xhci-keyboard-postmortem.md", "9d9803488de4"),
}

# Length of the abbreviated blob hashes stored above.
STAMP_LEN = 12


def blob_hash(relpath):
    """Git's blob hash for a working-tree file, or None if it is missing.

    `git hash-object` hashes what is ON DISK, not what is committed, so an
    uncommitted edit to a source counts as drift immediately rather than at the
    next commit - which is the moment the page's author can still act on it.
    """
    path = os.path.join(ROOT, "docs", relpath)
    if not os.path.exists(path):
        return None
    out = subprocess.run(
        ["git", "hash-object", path], capture_output=True, text=True, cwd=ROOT
    )
    if out.returncode != 0:
        return None
    return out.stdout.strip()[:STAMP_LEN]


def listed_pages():
    """Every HTML page actually present under docs/ that the site serves."""
    pages = ["index.html"]
    site = os.path.join(ROOT, "docs", "site")
    if os.path.isdir(site):
        pages += sorted(f"site/{f}" for f in os.listdir(site) if f.endswith(".html"))
    return pages


def update(stale, only):
    """Re-stamp stale pages, rewriting this file's MANIFEST in place.

    `only` restricts it to the pages named on the command line; empty means
    every stale page, which the caller warns about first.
    """
    if only:
        unknown = [p for p in only if p not in {s[0] for s in stale}]
        if unknown:
            for p in unknown:
                print(f"  ! {p} is not currently reported stale - nothing to re-stamp")
            return 1
        stale = [s for s in stale if s[0] in only]

    path = os.path.abspath(__file__)
    src = open(path, encoding="utf-8").read()
    for page, source, _, now in stale:
        old = f'"{page}": ("{source}", "{MANIFEST[page][1]}"),'
        new = f'"{page}": ("{source}", "{now}"),'
        if old not in src:
            print(f"  ! could not re-stamp {page} (manifest line not found verbatim)")
            return 1
        src = src.replace(old, new)
    open(path, "w", encoding="utf-8").write(src)
    print(f"re-stamped {len(stale)} page(s). Commit this file with the page edits.")
    return 0


def main():
    problems, stale, checked, unsourced = [], [], 0, 0

    present = set(listed_pages())
    for page in sorted(present - set(MANIFEST)):
        problems.append(
            f"{page}: served by the site but absent from MANIFEST - add a line "
            "giving its markdown source, or None if it is hand-written"
        )
    for page in sorted(set(MANIFEST) - present):
        problems.append(f"{page}: in MANIFEST but no such file - was it renamed or removed?")

    for page, entry in sorted(MANIFEST.items()):
        if entry is None:
            unsourced += 1
            continue
        source, stamp = entry
        now = blob_hash(source)
        if now is None:
            problems.append(f"{page}: source docs/{source} is missing")
            continue
        checked += 1
        if now != stamp:
            stale.append((page, source, stamp, now))

    if problems:
        print("check-site-freshness: MANIFEST is wrong")
        for p in problems:
            print(f"  - {p}")
        return 1

    if stale:
        if "--update" in sys.argv:
            named = [a for a in sys.argv[1:] if a != "--update"]
            if not named:
                print(
                    f"re-stamping ALL {len(stale)} reported page(s). This asserts that every\n"
                    "one of them has been re-read and updated. If that is not true, name the\n"
                    "pages instead: --update site/manual.html\n"
                )
            return update(stale, named)
        print(f"check-site-freshness: {len(stale)} page(s) BEHIND their source")
        for page, source, stamp, now in stale:
            print(f"  - docs/{page}")
            print(f"      docs/{source} changed since the page was written ({stamp} -> {now})")
        print(
            "\nThe site is live, so these are public pages serving a stale answer.\n"
            "Fix by re-reading the source and updating the page, then re-stamp it:\n"
            "    python3 scripts/check-site-freshness.py --update <page> [<page>...]\n"
            "Re-stamping WITHOUT updating the page asserts a review that did not\n"
            "happen - that is the one way to make this check lie."
        )
        return 1

    print(
        f"check-site-freshness: {checked} page(s) match the source they abridge "
        f"({unsourced} hand-written page(s) have no source)"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
