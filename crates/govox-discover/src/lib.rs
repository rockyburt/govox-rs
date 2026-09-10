//! Enumerating bias terms from the machine.
//!
//! This crate is the half of discovery that is allowed to touch the disk.
//! `govox-core` owns the [`TermProvider`] trait, the parsers, and every
//! decision about ordering and budget; everything here reads files and hands
//! back what it found.
//!
//! Nothing in here can fail. A root that does not exist, an unreadable ssh
//! config, a repository mid-rebase — each is an empty answer and a `debug!`
//! line, never an error. The rule is in `TermProvider::candidates`, which
//! returns no `Result`, so a quiet machine has no way to become a daemon that
//! will not start.

pub mod listed;
pub mod machine;
pub mod repos;
pub mod roots;

use std::path::{Path, PathBuf};

use govox_core::discovery::{Candidates, DiscoverySpec, ProviderName, TermProvider, WatchSet};

/// What the machine said, and what to watch so the answer stays true.
#[derive(Debug, Clone, Default)]
pub struct Discovered {
    pub candidates: Vec<Candidates>,
    pub watch: WatchSet,
}

impl Discovered {
    /// How many terms were found in total, before the budget is applied.
    #[must_use]
    pub fn len(&self) -> usize {
        self.candidates.iter().map(|found| found.terms.len()).sum()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Run every provider the spec asks for.
///
/// `extra` carries answers this crate cannot produce itself — capture device
/// names, which belong to whoever owns the audio backend. Pulling `cpal` in
/// here to ask would make the whole of discovery fail wherever `cpal` fails,
/// for the sake of two words.
#[must_use]
pub fn discover(spec: &DiscoverySpec, home: Option<&Path>, extra: &[Candidates]) -> Discovered {
    let roots = roots::expand_roots(&spec.repo_roots, home);
    if spec.repo_roots.is_empty() {
        tracing::debug!("no repo_roots configured; no repository terms");
    } else {
        tracing::debug!(roots = roots.len(), "expanded repository roots");
    }

    let providers = built_in(spec, home, roots);
    let mut found = Discovered::default();

    for provider in providers {
        if !spec.runs(provider.name()) {
            continue;
        }
        let candidates = provider.candidates(spec);
        tracing::info!(
            provider = provider.name().as_str(),
            terms = candidates.terms.len(),
            "discovered bias terms"
        );
        if !candidates.terms.is_empty() {
            found.candidates.push(candidates);
        }
        found.watch.absorb(provider.watch_paths(spec));
    }

    for candidates in extra {
        if candidates
            .provider
            .is_some_and(|provider| spec.runs(provider))
            && !candidates.terms.is_empty()
        {
            tracing::info!(
                provider = candidates.provider.map_or("?", ProviderName::as_str),
                terms = candidates.terms.len(),
                "discovered bias terms"
            );
            found.candidates.push(candidates.clone());
        }
    }

    found
}

/// The providers this crate can build, in priority order.
fn built_in(
    spec: &DiscoverySpec,
    home: Option<&Path>,
    roots: Vec<PathBuf>,
) -> Vec<Box<dyn TermProvider>> {
    let home = home.map(Path::to_path_buf);
    let providers: Vec<Box<dyn TermProvider>> = vec![
        Box::new(listed::FileProvider {
            paths: roots::expand_files(&spec.term_files, home.as_deref()),
        }),
        Box::new(repos::RepoProvider {
            roots: roots.clone(),
        }),
        Box::new(repos::BranchProvider { roots }),
        Box::new(listed::DirProvider {
            roots: roots::expand_roots(&spec.dir_roots, home.as_deref()),
        }),
        Box::new(machine::HostnameProvider {
            path: PathBuf::from(machine::ETC_HOSTNAME),
            fallback: std::env::var("HOSTNAME").ok(),
        }),
        Box::new(machine::SshHostsProvider {
            path: home
                .as_ref()
                .map(|home| home.join(".ssh").join("config"))
                .unwrap_or_default(),
        }),
        Box::new(machine::SystemdUnitsProvider {
            dir: home
                .as_ref()
                .map(|home| home.join(".config").join("systemd").join("user"))
                .unwrap_or_default(),
        }),
    ];
    providers
        .into_iter()
        .filter(|provider| spec.runs(provider.name()))
        .collect()
}
