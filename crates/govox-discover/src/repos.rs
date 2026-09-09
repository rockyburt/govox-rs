//! What the repositories on this machine are called, and what is checked out.
//!
//! Everything here reads `.git` directly. No `git` subprocess: it is faster,
//! it does not fork on the reload path, it is testable without the binary, and
//! a machine with no `git` installed degrades to "no repository terms" rather
//! than to a daemon that will not start.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use govox_core::discovery::{
    Candidates, DiscoverySpec, ProviderName, TermProvider, WatchSet, branch_terms,
    normalize_candidate, parse_git_config_origin, parse_head, parse_packed_refs,
};

/// One checkout found under a configured root.
#[derive(Debug, Clone)]
pub struct Repo {
    /// The directory name, which is what people actually say out loud.
    pub name: String,
    /// Where the refs live — not always `<repo>/.git`, for a worktree.
    pub git_dir: PathBuf,
    /// When `HEAD` last moved, which is the best available "was I working
    /// here" signal without opening the object store.
    pub touched: SystemTime,
}

/// Every repository under `roots`, most recently touched first.
///
/// The ordering is the budget policy. When there is not room for every repo's
/// vocabulary, the ones checked out today should win over the one cloned two
/// years ago and never opened since.
#[must_use]
pub fn repositories(roots: &[PathBuf], max_repos: usize) -> Vec<Repo> {
    let mut repos: Vec<Repo> = Vec::new();
    for root in roots {
        let Ok(entries) = std::fs::read_dir(root) else {
            tracing::debug!(dir = %root.display(), "cannot list; no repositories from this root");
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let Some(git_dir) = resolve_git_dir(&path) else {
                continue;
            };
            let Some(name) = path
                .file_name()
                .and_then(|name| normalize_candidate(&name.to_string_lossy()))
            else {
                continue;
            };
            let touched = std::fs::metadata(git_dir.join("HEAD"))
                .and_then(|meta| meta.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            repos.push(Repo {
                name,
                git_dir,
                touched,
            });
        }
    }

    repos.sort_by(|left, right| {
        right
            .touched
            .cmp(&left.touched)
            .then_with(|| left.name.cmp(&right.name))
    });
    repos.truncate(max_repos);
    repos
}

/// Where a checkout keeps its refs.
///
/// Usually `<repo>/.git`, but a worktree's `.git` is a *file* holding
/// `gitdir: <path>` — and a worktree is exactly the case where the branch name
/// is worth biasing, since it is the branch someone made a whole directory for.
fn resolve_git_dir(repo: &Path) -> Option<PathBuf> {
    let dot_git = repo.join(".git");
    if dot_git.is_dir() {
        return Some(dot_git);
    }
    let pointer = std::fs::read_to_string(&dot_git).ok()?;
    let target = pointer.trim().strip_prefix("gitdir:")?.trim();
    let target = Path::new(target);
    let resolved = if target.is_absolute() {
        target.to_path_buf()
    } else {
        repo.join(target)
    };
    resolved.is_dir().then_some(resolved)
}

/// Repository names, and the org and name their `origin` is written with.
pub struct RepoProvider {
    pub roots: Vec<PathBuf>,
}

impl TermProvider for RepoProvider {
    fn name(&self) -> ProviderName {
        ProviderName::Repos
    }

    fn candidates(&self, spec: &DiscoverySpec) -> Candidates {
        let mut terms: Vec<String> = Vec::new();
        // Repo by repo rather than all the names then all the orgs: if the
        // budget runs out mid-list, what survives should be whole projects.
        for repo in repositories(&self.roots, spec.max_repos) {
            let mut push = |term: String| {
                if !terms.contains(&term) {
                    terms.push(term);
                }
            };
            push(repo.name);
            let config = std::fs::read_to_string(repo.git_dir.join("config")).unwrap_or_default();
            if let Some(remote) = parse_git_config_origin(&config) {
                push(remote.repo);
                if let Some(org) = remote.org {
                    push(org);
                }
            }
        }
        Candidates::new(self.name(), terms)
    }

    fn watch_paths(&self, _spec: &DiscoverySpec) -> WatchSet {
        // The roots themselves: a repository cloned or deleted changes the
        // answer, and nothing else in these directories does.
        WatchSet {
            files: Vec::new(),
            dirs: self.roots.clone(),
        }
    }
}

/// The words inside the branches of those repositories.
pub struct BranchProvider {
    pub roots: Vec<PathBuf>,
}

impl TermProvider for BranchProvider {
    fn name(&self) -> ProviderName {
        ProviderName::Branches
    }

    fn candidates(&self, spec: &DiscoverySpec) -> Candidates {
        let mut terms: Vec<String> = Vec::new();
        for repo in repositories(&self.roots, spec.max_repos) {
            for branch in branches(&repo.git_dir, spec.max_branches_per_repo) {
                for term in branch_terms(&branch) {
                    if !terms.contains(&term) {
                        terms.push(term);
                    }
                }
            }
        }
        Candidates::new(self.name(), terms)
    }

    fn watch_paths(&self, spec: &DiscoverySpec) -> WatchSet {
        // `HEAD` moves on checkout, `packed-refs` on gc. Naming the two files
        // rather than watching `refs/heads` keeps `.git/index` churn — every
        // `git status`, every editor's background fetch — out of the reload
        // path. The gap that buys: a branch created but never checked out
        // waits for the next reload, and that is a branch nobody is talking
        // about yet.
        let files = repositories(&self.roots, spec.max_repos)
            .into_iter()
            .flat_map(|repo| [repo.git_dir.join("HEAD"), repo.git_dir.join("packed-refs")])
            .collect();
        WatchSet {
            files,
            dirs: Vec::new(),
        }
    }
}

/// Local branches of one checkout, most recently touched first.
///
/// `HEAD` leads whatever else is found: the branch you are standing on is the
/// one you are about to say out loud.
#[must_use]
pub fn branches(git_dir: &Path, max: usize) -> Vec<String> {
    let mut ordered: Vec<String> = Vec::new();
    let mut push = |branch: String| {
        if !branch.is_empty() && !ordered.contains(&branch) {
            ordered.push(branch);
        }
    };

    if let Ok(head) = std::fs::read_to_string(git_dir.join("HEAD"))
        && let Some(branch) = parse_head(&head)
    {
        push(branch);
    }

    let mut loose = loose_heads(&git_dir.join("refs").join("heads"), Path::new(""));
    loose.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    for (branch, _) in loose {
        push(branch);
    }

    if let Ok(packed) = std::fs::read_to_string(git_dir.join("packed-refs")) {
        for branch in parse_packed_refs(&packed) {
            push(branch);
        }
    }

    ordered.truncate(max);
    ordered
}

/// Walk `refs/heads`, which nests: `feature/x` is a directory and a file.
fn loose_heads(dir: &Path, prefix: &Path) -> Vec<(String, SystemTime)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let path = entry.path();
        if path.is_dir() {
            found.extend(loose_heads(&path, &prefix.join(&name)));
        } else {
            let branch = prefix.join(&name).to_string_lossy().to_string();
            let touched = entry
                .metadata()
                .and_then(|meta| meta.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            found.push((branch, touched));
        }
    }
    found
}
