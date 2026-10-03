//! The kernel's build identity, baked in as `OUROBOROS_BUILD`: the commit
//! (12 hex digits), `+dirty` when tracked files differ from it, and the
//! profile, e.g. `626f402af5ad+dirty debug`. The boot logs it in its first
//! line and again once its own console is live, so a capture says which
//! build ran instead of leaving it to be inferred (a card staged from the
//! wrong tree on 2026-10-01 cost a round trip; on 2026-10-03 a boot's build
//! was read off its image size).
//!
//! **Reruns on every build**, by naming a path that never exists: cargo
//! reruns a build script whose `rerun-if-changed` file is missing. The usual
//! `rerun-if-changed=build.rs` was shown on 2026-10-03 to leave the old
//! commit in the image after a commit that changes no source (a new HEAD,
//! the same files), and cargo did not even recompile: the one case the line
//! is for. Cargo recompiles the kernel only when the value changes.
//!
//! Outside a git checkout (a source tarball) it says `unknown`, rather than
//! failing the build.

use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn main() {
    println!("cargo:rerun-if-changed=.build-identity-always-rerun");
    let commit = git(&["rev-parse", "--short=12", "HEAD"]).unwrap_or_else(|| "unknown".into());
    let dirty = match git(&["status", "--porcelain", "--untracked-files=no"]) {
        Some(s) if !s.is_empty() => "+dirty",
        _ => "",
    };
    let profile = std::env::var("PROFILE").unwrap_or_else(|_| "unknown".into());
    println!("cargo:rustc-env=OUROBOROS_BUILD={commit}{dirty} {profile}");
}
