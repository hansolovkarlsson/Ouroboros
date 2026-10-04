//! The kernel's build identity, baked in as `OUROBOROS_BUILD`: the commit
//! (12 hex digits), `+dirty` when the tree differs from it (a tracked file
//! changed, or an untracked one that is not ignored, since the build may
//! have used it), and the profile, e.g. `626f402af5ad+dirty debug`. The
//! boot logs it in its first line and again once its own console is live
//! after the exit, whichever console that is, so a capture says which
//! build ran instead of leaving it to be inferred (a card staged from the
//! wrong tree on 2026-10-01 cost a round trip; on 2026-10-03 a boot's build
//! was read off its image size).
//!
//! **Reruns on every build**, by naming a path that never exists (inside
//! `OUT_DIR`, which only cargo writes): cargo reruns a build script whose
//! `rerun-if-changed` file is missing. The usual
//! `rerun-if-changed=build.rs` was shown on 2026-10-03 to leave the old
//! commit in the image after a commit that changes no source (a new HEAD,
//! the same files), and cargo did not even recompile: the one case the line
//! is for. Watching git's HEAD, refs and index instead would miss an
//! unstaged edit outside `kernel/`, so `+dirty` would go stale the same way.
//! The cost: a rerun makes cargo recompile the kernel crate on every build,
//! about 0.9 s debug and 1.3 s release on a no-change build (measured
//! 2026-10-03), paid for a line that cannot lie.
//!
//! `git status` runs with `--no-optional-locks`, so a build never takes
//! `.git/index.lock` under a commit or a rebase in progress.
//!
//! Outside a git checkout (a source tarball) it says `unknown`, rather than
//! failing the build.

use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn main() {
    let out_dir = std::env::var("OUT_DIR").unwrap_or_default();
    println!("cargo:rerun-if-changed={out_dir}/never-created-so-always-rerun");
    let commit = git(&["rev-parse", "--short=12", "HEAD"]).unwrap_or_else(|| "unknown".into());
    let dirty = match git(&["--no-optional-locks", "status", "--porcelain"]) {
        Some(s) if !s.is_empty() => "+dirty",
        _ => "",
    };
    let profile = std::env::var("PROFILE").unwrap_or_else(|_| "unknown".into());
    println!("cargo:rustc-env=OUROBOROS_BUILD={commit}{dirty} {profile}");
}
