//! Bakes the *build's* version in, not just the manifest's.
//!
//! `CARGO_PKG_VERSION` is `0.1.0` for every build made since the 0.1.0 tag,
//! which at the time of writing is twelve commits and six user-visible changes
//! ago. The About menu exists to answer "what am I actually running?", and a
//! string that cannot distinguish the release from a `develop` build twelve
//! commits later fails at exactly that.
//!
//! `git describe` answers it precisely: `v0.1.0` on the tag, `v0.1.0-12-g38dbed7`
//! past it, with `-dirty` appended when the tree has uncommitted changes.
//!
//! Falls back to `CARGO_PKG_VERSION` when git is unavailable or there is no
//! repository — which is the normal case for a source tarball, not an error.

use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    // Resolve HEAD through git itself rather than assuming `../../.git/HEAD`.
    // In a linked worktree `.git` is a *file* pointing elsewhere, so the naive
    // path does not exist and the rebuild trigger would silently never fire.
    if let Some(head) = git(&["rev-parse", "--git-path", "HEAD"]) {
        println!("cargo:rerun-if-changed={head}");
    }

    let version = git(&["describe", "--tags", "--always", "--dirty"])
        .unwrap_or_else(|| std::env::var("CARGO_PKG_VERSION").unwrap_or_default());
    println!("cargo:rustc-env=GOVOX_BUILD_VERSION={version}");
}

/// Run git and return trimmed stdout, or `None` if it failed for any reason.
fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?.trim().to_owned();
    if text.is_empty() { None } else { Some(text) }
}
