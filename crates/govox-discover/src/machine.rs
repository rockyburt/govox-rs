//! The vocabulary of the machine itself: its name, its hosts, its units.
//!
//! Every path is a field rather than a constant, so a test can point a
//! provider at a directory it owns and no test ever depends on the machine it
//! runs on.

use std::path::PathBuf;

use govox_core::discovery::{
    Candidates, DiscoverySpec, ProviderName, TermProvider, WatchSet, normalize_candidate,
    parse_ssh_config_hosts,
};

/// Where the system records its own name.
pub const ETC_HOSTNAME: &str = "/etc/hostname";

/// This machine's name.
pub struct HostnameProvider {
    pub path: PathBuf,
    /// What the environment says, used when the file cannot be read.
    pub fallback: Option<String>,
}

impl TermProvider for HostnameProvider {
    fn name(&self) -> ProviderName {
        ProviderName::Hostname
    }

    fn candidates(&self, _spec: &DiscoverySpec) -> Candidates {
        let raw = std::fs::read_to_string(&self.path)
            .ok()
            .map(|text| text.trim().to_string())
            .filter(|name| !name.is_empty())
            .or_else(|| self.fallback.clone())
            .unwrap_or_default();

        let mut terms = Vec::new();
        if let Some(term) = normalize_candidate(&raw) {
            terms.push(term);
        }
        // An FQDN is said by its first label. `rockyburt-desktop.local` is
        // "rockyburt desktop" out loud, and the suffix is never spoken.
        if let Some((label, _)) = raw.split_once('.')
            && let Some(term) = normalize_candidate(label)
            && !terms.contains(&term)
        {
            terms.push(term);
        }
        Candidates::new(self.name(), terms)
    }
}

/// The hosts named in an ssh config.
pub struct SshHostsProvider {
    pub path: PathBuf,
}

impl TermProvider for SshHostsProvider {
    fn name(&self) -> ProviderName {
        ProviderName::SshHosts
    }

    fn candidates(&self, _spec: &DiscoverySpec) -> Candidates {
        let text = std::fs::read_to_string(&self.path).unwrap_or_else(|error| {
            tracing::debug!(path = %self.path.display(), %error, "no ssh hosts");
            String::new()
        });
        Candidates::new(self.name(), parse_ssh_config_hosts(&text))
    }

    fn watch_paths(&self, _spec: &DiscoverySpec) -> WatchSet {
        WatchSet {
            files: vec![self.path.clone()],
            dirs: Vec::new(),
        }
    }
}

/// Unit file suffixes worth reading. A unit is named by what it is.
const UNIT_SUFFIXES: [&str; 3] = [".service", ".timer", ".socket"];

/// The user's own systemd units.
pub struct SystemdUnitsProvider {
    pub dir: PathBuf,
}

impl TermProvider for SystemdUnitsProvider {
    fn name(&self) -> ProviderName {
        ProviderName::SystemdUnits
    }

    fn candidates(&self, _spec: &DiscoverySpec) -> Candidates {
        // A directory read, not `systemctl`: the unit names are the filenames,
        // and forking a binary to be told so would be the only subprocess in
        // the whole discovery path.
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            tracing::debug!(dir = %self.dir.display(), "no systemd units");
            return Candidates::new(self.name(), Vec::new());
        };

        let mut names: Vec<String> = entries
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .collect();
        names.sort();

        let mut terms = Vec::new();
        for file in names {
            let Some(stem) = UNIT_SUFFIXES
                .iter()
                .find_map(|suffix| file.strip_suffix(suffix))
            else {
                continue;
            };
            // `getty@.service` is a template; the `@` belongs to systemd's
            // grammar rather than to the name anyone says.
            let stem = stem.trim_end_matches('@');
            if let Some(term) = normalize_candidate(stem)
                && !terms.contains(&term)
            {
                terms.push(term);
            }
        }
        Candidates::new(self.name(), terms)
    }

    fn watch_paths(&self, _spec: &DiscoverySpec) -> WatchSet {
        WatchSet {
            files: Vec::new(),
            dirs: vec![self.dir.clone()],
        }
    }
}
