//! The two providers that read what the user pointed at, rather than what the
//! machine happens to contain.
//!
//! `dirs` names plain directories, for project folders that were never
//! checkouts. `files` reads term lists the user maintains — the hand-written
//! `bias` list's standing, kept somewhere else.

use std::path::PathBuf;
use std::time::SystemTime;

use govox_core::discovery::{
    Candidates, DiscoverySpec, ProviderName, TermProvider, WatchSet, normalize_candidate,
    parse_term_file,
};

/// Directory names under the configured roots.
///
/// Deliberately not `repos`: that provider requires a `.git` and reads config,
/// refs and HEAD out of it, so a plain folder contributes nothing there. This
/// one asks only that the directory exist, which is what makes it useful for a
/// project that was never a checkout — and also what makes it need a cap, since
/// nothing about a directory proves anyone works in it.
pub struct DirProvider {
    pub roots: Vec<PathBuf>,
}

impl TermProvider for DirProvider {
    fn name(&self) -> ProviderName {
        ProviderName::Dirs
    }

    fn candidates(&self, spec: &DiscoverySpec) -> Candidates {
        let mut found: Vec<(String, SystemTime)> = Vec::new();
        for root in &self.roots {
            let Ok(entries) = std::fs::read_dir(root) else {
                tracing::debug!(dir = %root.display(), "cannot list; no directory names");
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }
                let raw = entry.file_name().to_string_lossy().to_string();
                // A dotfile directory is machinery, not a project someone
                // talks about.
                if raw.starts_with('.') {
                    continue;
                }
                let Some(name) = normalize_candidate(&raw) else {
                    continue;
                };
                let touched = entry
                    .metadata()
                    .and_then(|meta| meta.modified())
                    .unwrap_or(SystemTime::UNIX_EPOCH);
                found.push((name, touched));
            }
        }

        // Recency first, exactly as repositories are ordered, so the cap keeps
        // whatever has been touched rather than whatever sorts early.
        found.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
        let mut terms: Vec<String> = Vec::new();
        for (name, _) in found {
            if !terms.contains(&name) {
                terms.push(name);
            }
            if terms.len() >= spec.max_dirs {
                break;
            }
        }
        Candidates::new(self.name(), terms)
    }

    fn watch_paths(&self, _spec: &DiscoverySpec) -> WatchSet {
        WatchSet {
            files: Vec::new(),
            dirs: self.roots.clone(),
        }
    }
}

/// Terms listed in files the user names.
///
/// The escape hatch for vocabulary no provider can infer: client names, people,
/// jargon. It is the hand-written `bias` list's standing kept in another file,
/// which is why it outranks every other provider when the budget runs short.
pub struct FileProvider {
    pub paths: Vec<PathBuf>,
}

impl TermProvider for FileProvider {
    fn name(&self) -> ProviderName {
        ProviderName::Files
    }

    fn candidates(&self, _spec: &DiscoverySpec) -> Candidates {
        let mut terms: Vec<String> = Vec::new();
        for path in &self.paths {
            let text = match std::fs::read_to_string(path) {
                Ok(text) => text,
                Err(error) => {
                    tracing::debug!(path = %path.display(), %error, "no terms from this file");
                    continue;
                }
            };
            for term in parse_term_file(&text) {
                if !terms.contains(&term) {
                    terms.push(term);
                }
            }
        }
        Candidates::new(self.name(), terms)
    }

    fn watch_paths(&self, _spec: &DiscoverySpec) -> WatchSet {
        // Named exactly, so editing a term list reloads it the way editing the
        // dictionary does.
        WatchSet {
            files: self.paths.clone(),
            dirs: Vec::new(),
        }
    }
}
