//! Bakes the *build's* version in, not just the manifest's.
//!
//! `CARGO_PKG_VERSION` is the same string for every build made since the last
//! release tag, however many commits and user-visible changes ago that was. The
//! About menu and `--version` exist to answer "what am I actually running?",
//! and a string that cannot distinguish the release from a `develop` build
//! fourteen commits later fails at exactly that.
//!
//! The examples below use `0.1.0` as the tagged version; the mechanism does not
//! depend on which release it is.
//!
//! The answer is the manifest version with the commit attached as **semver
//! build metadata**:
//!
//! | Where | Reported |
//! |---|---|
//! | On the release tag | `0.1.0` |
//! | Fourteen commits past it | `0.1.0+14.a18ad6e` |
//! | No tags reachable (shallow clone, CI) | `0.1.0+a18ad6e` |
//! | No git at all (source tarball) | `0.1.0` |
//!
//! ## Why build metadata rather than `git describe`'s own shape
//!
//! `git describe` yields `v0.1.0-14-ga18ad6e`, which *parses* as semver but
//! ranks **below** `0.1.0`: everything after the first `-` is a prerelease
//! identifier, and a prerelease always sorts under its release. So a build
//! fourteen commits newer than 0.1.0 would compare as older than it. Anything
//! that ever parses this — packaging, an update check, a support script — would
//! get it exactly backwards.
//!
//! Build metadata after `+` is ignored for precedence, so `0.1.0+14.a18ad6e`
//! ranks equal to `0.1.0` rather than below it. Equal is not perfect, but it is
//! the honest answer while the manifest still says the tagged version, and it
//! is not wrong in the way the alternative is.
//!
//! ## Composed, not parsed
//!
//! The pieces are asked for individually rather than scraped out of
//! `describe`'s output, so nothing depends on that format staying stable, and a
//! repository with no tags degrades to "commit only" instead of to a surprise.
//!
//! ## No dirty marker
//!
//! `git describe --dirty` was tried and removed. Keeping it truthful means
//! rebuilding whenever any tracked file changes, which a build script cannot
//! ask for; its accuracy instead depended on whether the repository happened to
//! keep its refs packed, so the flag was right on one machine and stale on
//! another. A marker that is only sometimes correct is worse than no marker.

use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    // Every path is resolved through git rather than assumed. In a linked
    // worktree `.git` is a *file* pointing elsewhere, so `../../.git/HEAD` does
    // not exist and the trigger would silently never fire.
    //
    // Watching HEAD alone is not enough, and quietly so: HEAD holds
    // `ref: refs/heads/<branch>` and only changes when you *switch* branches. A
    // commit moves the branch ref, leaving HEAD untouched — so the version
    // string stayed at the previous commit until something else forced a
    // rebuild. Caught by a fresh build reporting a commit behind its own tree.
    for path in ["HEAD", "packed-refs"] {
        if let Some(resolved) = git(&["rev-parse", "--git-path", path]) {
            println!("cargo:rerun-if-changed={resolved}");
        }
    }
    // The branch ref itself, which is what a commit actually writes. Absent on
    // a detached HEAD, where HEAD above already carries the commit id.
    if let Some(reference) = git(&["symbolic-ref", "--quiet", "HEAD"])
        && let Some(resolved) = git(&["rev-parse", "--git-path", &reference])
    {
        println!("cargo:rerun-if-changed={resolved}");
    }

    let base = std::env::var("CARGO_PKG_VERSION").unwrap_or_default();
    let version = match build_metadata() {
        Some(metadata) => format!("{base}+{metadata}"),
        None => base,
    };
    println!("cargo:rustc-env=GOVOX_BUILD_VERSION={version}");
}

/// The `+…` part: how far past the last release this is, and which commit.
///
/// `None` means there is nothing to add — either this *is* the release, or
/// there is no repository to ask.
fn build_metadata() -> Option<String> {
    let commit = git(&["rev-parse", "--short=7", "HEAD"])?;

    // Distance from the most recent tag. A shallow clone and a repository with
    // no releases yet both have none, and the commit alone still identifies the
    // build, so this degrades rather than failing.
    let distance = git(&["describe", "--tags", "--abbrev=0"])
        .and_then(|tag| git(&["rev-list", &format!("{tag}..HEAD"), "--count"]));

    match distance.as_deref() {
        // Standing exactly on the tag: this is the release, and a release wears
        // its bare version number.
        Some("0") => None,
        Some(count) => Some(format!("{count}.{commit}")),
        None => Some(commit),
    }
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
