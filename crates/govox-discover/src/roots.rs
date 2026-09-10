//! Turning a configured `repo_roots` pattern into directories that exist.

use std::path::{Component, Path, PathBuf};

use govox_core::caret::glob_match;
use govox_core::domain::expand_user;

/// Expand every configured root, resolving `~` and any `*` or `?`.
///
/// Deduplicated and filtered to directories that actually exist, because a
/// pattern matching nothing is the ordinary case on a machine that has not
/// been set up yet — not an error worth reporting.
#[must_use]
pub fn expand_roots(roots: &[String], home: Option<&Path>) -> Vec<PathBuf> {
    expand_all(roots, home, |path| path.is_dir())
}

/// Expand every configured pattern to the *files* it names.
///
/// The same globbing as [`expand_roots`], so one list of terms or a directory
/// of them are written the same way.
#[must_use]
pub fn expand_files(patterns: &[String], home: Option<&Path>) -> Vec<PathBuf> {
    expand_all(patterns, home, |path| path.is_file())
}

fn expand_all(
    patterns: &[String],
    home: Option<&Path>,
    keep: impl Fn(&Path) -> bool + Copy,
) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = Vec::new();
    for pattern in patterns {
        for path in expand_one(pattern, home, keep) {
            if !found.contains(&path) {
                found.push(path);
            }
        }
    }
    found
}

fn expand_one(pattern: &str, home: Option<&Path>, keep: impl Fn(&Path) -> bool) -> Vec<PathBuf> {
    let expanded = expand_user(Path::new(pattern), home);

    let mut bases: Vec<PathBuf> = vec![PathBuf::new()];
    for component in expanded.components() {
        match component {
            Component::Normal(name) => {
                let name = name.to_string_lossy().to_string();
                bases = if name.contains('*') || name.contains('?') {
                    bases
                        .iter()
                        .flat_map(|base| children_matching(base, &name))
                        .collect()
                } else {
                    bases.iter().map(|base| base.join(&name)).collect()
                };
            }
            // A wildcard cannot appear in these, so they only ever extend.
            other => {
                let literal = other.as_os_str();
                bases = bases.iter().map(|base| base.join(literal)).collect();
            }
        }
        if bases.is_empty() {
            break;
        }
    }

    bases.retain(|path| keep(path));
    bases
}

/// Directory entries of `base` whose name matches `pattern`.
///
/// Sorted, so two runs over an unchanged disk produce the same order and the
/// budget spends itself the same way twice.
fn children_matching(base: &Path, pattern: &str) -> Vec<PathBuf> {
    let listing = base.join("");
    let Ok(entries) = std::fs::read_dir(if listing.as_os_str().is_empty() {
        Path::new(".")
    } else {
        base
    }) else {
        tracing::debug!(dir = %base.display(), "not listing; skipping this root");
        return Vec::new();
    };

    let glob: Vec<char> = pattern.chars().collect();
    let mut matched: Vec<PathBuf> = entries
        .flatten()
        .filter(|entry| {
            let name = entry.file_name();
            let name: Vec<char> = name.to_string_lossy().chars().collect();
            glob_match(&glob, &name)
        })
        .map(|entry| entry.path())
        .collect();
    matched.sort();
    matched
}
